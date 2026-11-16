//! Motor de inferência sobre a ggml/llama.cpp vendorizada.
//!
//! É o caminho GPU. A regra que este módulo implementa é a do produto: **uma**
//! placa por vez — a escolhida de propósito, nunca um modelo espalhado entre
//! várias — e o motor de CPU só entra quando não há placa nenhuma. O que não
//! cabe na placa não é motivo para sair dela: o despacho vem do veredito medido em
//! `alojamento`, então um modelo maior que a VRAM sobe com o topo na placa e o
//! resto na memória do sistema, que é mais lento que tudo-na-placa e muito mais
//! rápido que o motor de CPU. Como a ggml fala com NVIDIA, AMD e Intel pelo mesmo
//! backend Vulkan, a escolha não depende do fabricante da placa; depende do tipo
//! e da memória dela.
//!
//! O prompt chega pronto, em ids, porque quem renderiza o template e tokeniza é
//! o mesmo caminho de antes (`chat.rs` + `tokenizers`): aqui só muda *onde* a
//! conta acontece. Isso também mantém uma única implementação de amostragem, e
//! amostragem duplicada entre backends é como duas respostas divergem.

pub mod ffi;

use crate::idiomas::tr;
use anyhow::{Result, bail};
use ffi::{Ggml, InfoDevice, Token};
use std::ffi::c_int;
use std::ffi::c_void;
use std::path::{Path, PathBuf};

use ffi::{TIPO_CPU, TIPO_GPU, TIPO_IGPU};

/// Uma placa que a ggml registrou, reduzida ao que a escolha precisa.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placa {
    pub indice: usize,
    pub nome: String,
    /// `true` para `GGML_BACKEND_DEVICE_TYPE_GPU` (VRAM própria); `false` para
    /// integrada, que usa a memória do sistema. As duas rodam o modelo; a
    /// dedicada só ganha a preferência.
    pub dedicada: bool,
    pub total: usize,
}

impl Placa {
    fn de(info: &InfoDevice) -> Self {
        Self {
            indice: info.indice,
            nome: info.nome.clone(),
            dedicada: info.tipo == TIPO_GPU,
            total: info.total,
        }
    }
}

/// Candidatas a rodar o modelo, na ordem em que devem ser preferidas: placa
/// dedicada antes de integrada, e dentro de cada classe a de maior memória.
/// Empate vai para o menor índice de registro — sem desempate fixo a escolha
/// mudaria entre execuções e nenhuma medição seria comparável.
pub fn candidatas(dispositivos: &[InfoDevice]) -> Vec<Placa> {
    let mut placas: Vec<Placa> = dispositivos
        .iter()
        .filter(|d| d.total > 0 && matches!(d.tipo, TIPO_GPU | TIPO_IGPU))
        .map(Placa::de)
        .collect();
    placas.sort_by(|a, b| {
        b.dedicada
            .cmp(&a.dedicada)
            .then(b.total.cmp(&a.total))
            .then(a.indice.cmp(&b.indice))
    });
    placas
}

/// A placa a usar, se houver alguma. `None` é a resposta honesta para "nesta
/// máquina só existe CPU": é o único caso em que o motor de CPU entra.
pub fn escolher(dispositivos: &[InfoDevice]) -> Option<Placa> {
    candidatas(dispositivos).into_iter().next()
}

/// O registro inteiro, na ordem da biblioteca: é o que a medição confere, porque
/// uma lista já filtrada não prova que o filtro acertou.
pub fn registro(dlls: &[PathBuf]) -> Result<Vec<InfoDevice>> {
    let (ggml, _pasta) = Ggml::abrir(dlls).map_err(anyhow::Error::msg)?;
    Ok(ggml.devices())
}

/// O motor: pesos, contexto e o lote de ids reaproveitado a cada passo.
pub struct Motor {
    ggml: Ggml,
    modelo: *mut c_void,
    contexto: *mut c_void,
    /// Quantos floats há num vetor de logits — a largura real do buffer que a
    /// biblioteca entrega, lida dela mesma em vez de suposta.
    n_vocab: usize,
    /// Máximo de tokens num único `llama_decode`: acima disso a própria
    /// biblioteca rejeita o lote, então o prefill é fatiado por aqui.
    n_batch: usize,
    /// Janela que a biblioteca aceitou, que pode ser menor que a pedida.
    pub janela: usize,
    /// Estado que a interface mostra: qual placa está servindo.
    pub dispositivo: String,
    /// Os adaptadores que este motor tem em memória, com o handle que a
    /// biblioteca deu para o **modelo** dele. Um adaptador não é um arquivo
    /// aberto: é RAM da placa derivada dos pesos do base, e por isso o dono é o
    /// motor, nunca uma tabela global.
    adaptadores: Vec<(PathBuf, *mut c_void)>,
    buf: Vec<Token>,
}

/// O que o motor devolve de cada adaptador pedido: se ele está de fato sobre os
/// pesos agora, e o motivo quando não está. É a resposta inteira, e o chamador
/// não tem de adivinhar nada para desenhar a tela.
#[derive(Debug, Clone)]
pub struct Aplicado {
    pub caminho: PathBuf,
    pub aplicada: bool,
    pub erro: Option<String>,
}

/// Os índices de `carregados` que nenhum pedido quer mais. Função separada
/// porque é dela que depende soltar RAM da placa: errar aqui é vazar memória ou
/// derrubar um adaptador que ainda está em uso.
fn obsoletos(carregados: &[&PathBuf], pedidos: &[(PathBuf, f32)]) -> Vec<usize> {
    (0..carregados.len())
        .filter(|i| !pedidos.iter().any(|(p, _)| p == carregados[*i]))
        .collect()
}

/// `modelo` e `contexto` só são usados pela thread que detém o `Engine`, e o
/// `Engine` vive atrás do Mutex de turno do runtime: um motor por vez, nunca
/// dois donos. É essa invariante que `Send` está afirmando.
unsafe impl Send for Motor {}

impl Motor {
    /// Carrega `pesos` na única placa escolhida, despachando `camadas_gpu` para
    /// ela: `-1` é o inteiro na placa, um número é o topo em cima e o resto em
    /// RAM, `0` é nada na placa. Erro se não houver placa — o chamador decide se
    /// cai para a CPU.
    pub fn novo(dlls: &[PathBuf], pesos: &Path, janela: usize, camadas_gpu: c_int) -> Result<Motor> {
        let (ggml, _pasta) = Ggml::abrir(dlls).map_err(anyhow::Error::msg)?;
        let dispositivos = ggml.devices();
        let Some(placa) = escolher(&dispositivos) else {
            bail!(tr("erros.motorSemPlaca", &[]));
        };
        let bruto = dispositivos[placa.indice].bruto;

        // Lista NULL-terminated, lida durante a carga: o array local é o dono, e
        // um único elemento é o que mantém o modelo inteiro numa só placa.
        let mut selecionados = [bruto, std::ptr::null_mut()];
        let modelo =
            unsafe { ggml.carregar(pesos, ggml.params_modelo(camadas_gpu), &mut selecionados) }
                .map_err(anyhow::Error::msg)?;

        let n_vocab = ggml.vocabulario(modelo);
        if n_vocab == 0 {
            ggml.liberar_modelo(modelo);
            bail!(tr("erros.motorSemVocabulario", &[]));
        }

        let params = ggml.params_contexto(janela as u32);
        let contexto = match unsafe { ggml.contexto(modelo, params) } {
            Ok(c) => c,
            Err(e) => {
                ggml.liberar_modelo(modelo);
                bail!(e);
            }
        };
        // Tudo que precisa do `ggml` é lido antes de ele entrar no Motor: depois
        // de movido, só o Motor o empresta.
        let janela_real = ggml.tamanho_contexto(contexto);
        let n_batch = ggml.tamanho_lote(contexto).max(1);

        Ok(Motor {
            dispositivo: placa.nome.clone(),
            ggml,
            modelo,
            contexto,
            n_vocab,
            n_batch,
            janela: janela_real,
            adaptadores: Vec::new(),
            buf: Vec::new(),
        })
    }

    /// Pré-processa `tokens` e devolve os logits da última posição. Um prompt
    /// mais longo que `n_batch` é fatiado; só o último bloco produz os logits
    /// que voltam, e os anteriores apenas alimentam o KV cache.
    pub fn passo(&mut self, tokens: &[u32]) -> Result<Vec<f32>> {
        if tokens.is_empty() {
            return Ok(Vec::new());
        }
        // O lote é sempre só `tokens`: o `buf` é reuso de capacidade, não memória
        // da conversa. Sem este clear cada passo de decode reenviaria o prompt
        // inteiro, a placa encheria o KV cache com cópias e recusaria o lote.
        let mut buf = std::mem::take(&mut self.buf);
        buf.clear();
        buf.extend(tokens.iter().map(|t| *t as Token));
        let (ctx, n_vocab, n_batch) = (self.contexto, self.n_vocab, self.n_batch);
        let ggml = &self.ggml;

        let mut logits = Vec::new();
        let mut falha = None;
        for inicio in (0..buf.len()).step_by(n_batch) {
            let fim = (inicio + n_batch).min(buf.len());
            // O ponteiro é da ggml, que só lê durante a chamada, e `buf` não é
            // tocado entre um bloco e outro: o intervalo continua válido.
            let bruto = unsafe { ggml.passo(ctx, buf[inicio..fim].as_mut_ptr(), fim - inicio, n_vocab) };
            match bruto {
                Ok(l) => logits = l,
                Err(e) => {
                    falha = Some(e);
                    break;
                }
            }
        }
        self.buf = buf;
        match falha {
            Some(e) => bail!(e),
            None => Ok(logits),
        }
    }

    /// Esquece a conversa: posições e KV cache voltam ao estado de modelo novo.
    pub fn limpar(&mut self) {
        self.ggml.limpar_cache(self.contexto);
    }

    /// Põe `pedidos` sobre os pesos do modelo em memória, com a força de cada
    /// um, e devolve item por item o que realmente aconteceu.
    ///
    /// Três decisões que não são cosméticas:
    /// - um adaptador que já está carregado **não é relido**: arrastar a força
    ///   de um LoRA de 100 MB até ontem custaria 100 MB de disco e de placa a
    ///   cada soltura do cursor, e é por isso que o handle fica guardado aqui;
    /// - um adaptador que não abriu não derruba os outros: o `erro` dele volta
    ///   na linha dele, e a placa continua com o que serviu;
    /// - o cache é descartado quando os pesos mudaram de verdade. Ali dentro há
    ///   logits do prompt calculados com o adaptador anterior; conservá-los é
    ///   a conversa mais cara que existe para um resultado errado.
    pub fn usar_adaptadores(&mut self, pedidos: &[(PathBuf, f32)]) -> Vec<Aplicado> {
        let mut carregados = std::mem::take(&mut self.adaptadores);
        let ggml = &self.ggml;
        let (modelo, contexto) = (self.modelo, self.contexto);

        let mut aplicados: Vec<(*mut c_void, f32)> = Vec::with_capacity(pedidos.len());
        let mut saídas: Vec<Aplicado> = Vec::with_capacity(pedidos.len());
        for (caminho, forca) in pedidos {
            let existente = carregados.iter().find(|(p, _)| p == caminho).map(|(_, h)| *h);
            let handle = match existente {
                Some(handle) => Ok(handle),
                None => unsafe { ggml.carregar_adapter(modelo, caminho) },
            };
            match handle {
                Ok(handle) => {
                    if existente.is_none() {
                        carregados.push((caminho.clone(), handle));
                    }
                    aplicados.push((handle, *forca));
                    saídas.push(Aplicado {
                        caminho: caminho.clone(),
                        aplicada: true,
                        erro: None,
                    });
                }
                Err(erro) => saídas.push(Aplicado {
                    caminho: caminho.clone(),
                    aplicada: false,
                    erro: Some(erro),
                }),
            }
        }

        // Soltar o que ninguém pediu mais é o que devolve a VRAM ao resto do
        // turno: um adaptador carregado e não aplicado continua ocupando placa.
        // Os índices são materializados antes de mexer na lista, e a ordem é
        // decrescente porque cada `remove` reindexia o que vem depois.
        let soltar = {
            let caminhos: Vec<&PathBuf> = carregados.iter().map(|(p, _)| p).collect();
            obsoletos(&caminhos, pedidos)
        };
        for indice in soltar.into_iter().rev() {
            let (_, handle) = carregados.remove(indice);
            ggml.liberar_adapter(handle);
        }

        let falha = unsafe { ggml.aplicar_adaptadores(contexto, &aplicados) }.err();
        match falha {
            Some(erro) => {
                // A biblioteca valida antes de mexer no contexto: se a chamada
                // falhou, nenhum pedido entrou — inclusive os que abriram, que
                // ficaram carregados mas sem efeito sobre os pesos.
                for saida in saídas.iter_mut() {
                    saida.aplicada = false;
                    saida.erro = Some(erro.clone());
                }
            }
            None => self.limpar(),
        }
        self.adaptadores = carregados;
        saídas
    }

    pub fn vocabulario(&self) -> usize {
        self.n_vocab
    }

    pub fn versao(&self) -> String {
        self.ggml.versao()
    }

    /// As placas visíveis nesta máquina, para a medição e para o diagnóstico.
    pub fn placas(dlls: &[PathBuf]) -> Result<Vec<Placa>> {
        Ok(candidatas(&registro(dlls)?))
    }
}

impl Drop for Motor {
    fn drop(&mut self) {
        // Ordem importa duas vezes: o adaptador é derivado do modelo, então ele
        // morre antes do contexto que o aplica, e o contexto morre antes do
        // modelo que ele lê.
        for (_, adaptador) in self.adaptadores.drain(..) {
            self.ggml.liberar_adapter(adaptador);
        }
        self.ggml.liberar_contexto(self.contexto);
        self.ggml.liberar_modelo(self.modelo);
    }
}

/// O device onde o vetor sai: a placa, quando existe uma; a CPU quando não
/// existe nenhuma. A ordem é a mesma do modelo de conversa — `escolher` já é a
/// regra do produto — e o que muda é só o último degrau.
fn device_de_embutir(dispositivos: &[InfoDevice]) -> Option<(usize, String, bool)> {
    if let Some(placa) = escolher(dispositivos) {
        return Some((placa.indice, placa.nome, true));
    }
    // O device de CPU vem do mesmo registro; não é uma placa inventada.
    dispositivos
        .iter()
        .find(|d| d.tipo == TIPO_CPU)
        .map(|d| (d.indice, d.nome.clone(), false))
}

/// O agrupamento como a interface o mostra. O número é o que a biblioteca
/// respondeu; esta palavra é só para o diagnóstico caber numa linha.
pub fn nome_do_agrupamento(codigo: c_int) -> &'static str {
    match codigo {
        ffi::AGRUPAMENTO_MEDIA => "média",
        ffi::AGRUPAMENTO_CLS => "cls",
        ffi::AGRUPAMENTO_ULTIMO => "último",
        ffi::AGRUPAMENTO_NENHUM => "último",
        _ => "indefinido",
    }
}

/// O motor das embeddings: um modelo pequeno, carregado uma vez, que lê uma
/// peça de texto por vez e devolve um vetor.
///
/// É o único lugar do aplicativo em que a CPU aceita a conta quando não há
/// placa, e o motivo não vale para o modelo de conversa: aqui não há segundo
/// caminho. O motor de CPU do Aether implementa a arquitetura granite de
/// geração; um codificador de embeddings não existe nele, e recusar a CPU
/// deixaria uma máquina sem placa sem memória nenhuma — enquanto embutir
/// trezentos tokens num modelo de seiscentos milhões custa centenas de
/// milissegundos nessa mesma CPU.
pub struct Embutidor {
    ggml: Ggml,
    modelo: *mut c_void,
    contexto: *mut c_void,
    /// `true` quando o modelo tem codificador e nenhuma decodificador: é o
    /// caminho (`llama_encode`) que a biblioteca exige para um BERT.
    pelo_codificador: bool,
    /// Largura do vetor, lida do modelo. Um índice gravado com outra largura é
    /// outro modelo, e é por aqui que se descobre.
    dimensao: usize,
    /// Tokens que atravessam o modelo num lote só. Uma peça maior que isto não é
    /// embutida em duas partes: a segunda metade daria um vetor do texto
    /// inteiro, e o índice mentiria sobre o que representa.
    limite: usize,
    /// Janela que a biblioteca aceitou, que é o teto real do lote.
    pub janela: usize,
    pub dispositivo: String,
    /// `true` quando foi a placa que rodou — é o que a interface mostra.
    pub na_placa: bool,
    pub agrupamento: &'static str,
    buf: Vec<Token>,
}

/// `modelo` e `contexto` vivem enquanto o dono os emprestar, e o dono é um só:
/// o `Embutidor` mora atrás do lock da memória, que segura a peça inteira
/// durante o cálculo. É essa invariante que `Send` está afirmando.
unsafe impl Send for Embutidor {}

impl Embutidor {
    /// Carrega `pesos` para embutir peças de até `janela` tokens.
    pub fn novo(dlls: &[PathBuf], pesos: &Path, janela: u32) -> Result<Embutidor> {
        let (ggml, _pasta) = Ggml::abrir(dlls).map_err(anyhow::Error::msg)?;
        let dispositivos = ggml.devices();
        let Some((indice, nome, na_placa)) = device_de_embutir(&dispositivos) else {
            // Sem placa e sem device de CPU no registro não há onde contar. Isto
            // não acontece numa máquina que rode o aplicativo, e se acontecer a
            // resposta honesta é um erro, não um vetor zerado.
            bail!(tr("erros.motorSemDispositivo", &[]));
        };
        let mut selecionados = [dispositivos[indice].bruto, std::ptr::null_mut()];
        // O embutidor vai inteiro para a placa: são dezenas de megabytes, e a
        // conta de dividir é do LLM, não dele.
        let modelo = unsafe {
            ggml.carregar(pesos, ggml.params_modelo(ffi::CAMADAS_NA_GPU), &mut selecionados)
        }
        .map_err(anyhow::Error::msg)?;

        let dimensao = ggml.dimensao_do_vetor(modelo);
        if dimensao == 0 {
            ggml.liberar_modelo(modelo);
            bail!(tr("erros.motorSemDimensao", &[]));
        }

        let params = ggml.params_embutir(janela);
        let contexto = match unsafe { ggml.contexto(modelo, params) } {
            Ok(c) => c,
            Err(e) => {
                ggml.liberar_modelo(modelo);
                bail!(e);
            }
        };
        let (pelo_codificador, janela_real, n_batch, agrupamento) = (
            ggml.usar_codificador(modelo),
            ggml.tamanho_contexto(contexto),
            ggml.tamanho_lote(contexto),
            ggml.agrupamento(contexto),
        );

        Ok(Embutidor {
            pelo_codificador,
            dimensao,
            limite: janela_real.min(n_batch).max(1),
            janela: janela_real,
            dispositivo: nome,
            na_placa,
            agrupamento: nome_do_agrupamento(agrupamento),
            ggml,
            modelo,
            contexto,
            buf: Vec::new(),
        })
    }

    pub fn dimensao(&self) -> usize {
        self.dimensao
    }

    /// Tokens máximos de uma peça. Vem do lote que a biblioteca aceitou, não do
    /// que o modelo declara como treino.
    pub fn limite(&self) -> usize {
        self.limite
    }

    /// O vetor do texto tokenizado em `ids`. Vazio na entrada devolve vazio, e
    /// um texto maior que o lote é erro de quem fatiou: aqui não se corta.
    pub fn embutir(&mut self, ids: &[u32]) -> Result<Vec<f32>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        if ids.len() > self.limite {
            bail!(tr(
                "erros.motorPecaGrande",
                &[("tokens", &ids.len().to_string()), ("limite", &self.limite.to_string())]
            ));
        }
        // Cada peça é uma sequência própria: as posições recomeçam em zero e o
        // cache da anterior não pode participar da atenção desta.
        self.ggml.limpar_cache(self.contexto);
        let mut buf = std::mem::take(&mut self.buf);
        buf.clear();
        buf.extend(ids.iter().map(|t| *t as Token));
        let saida = unsafe {
            self.ggml.embutir(self.contexto, self.pelo_codificador, &buf, self.dimensao)
        };
        self.buf = buf;
        // A ponte devolve o texto traduzido do lado C; aqui ele vira erro de
        // verdade, com cadeia, em vez de uma `String` sem tipo.
        Ok(saida.map_err(anyhow::Error::msg)?)
    }
}

impl Drop for Embutidor {
    fn drop(&mut self) {
        self.ggml.liberar_contexto(self.contexto);
        self.ggml.liberar_modelo(self.modelo);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::ffi::{TIPO_ACCEL, TIPO_CPU};

    const GIB: usize = 1024 * 1024 * 1024;

    fn device(indice: usize, nome: &str, tipo: i32, total: usize) -> InfoDevice {
        InfoDevice {
            indice,
            bruto: std::ptr::null_mut(),
            nome: nome.to_string(),
            tipo,
            livre: total / 2,
            total,
        }
    }

    #[test]
    fn escolhe_a_dedicada_mesmo_contra_integrada_de_mais_memoria() {
        // Placa compartilhada reporta a memória do sistema e pode aparecer com
        // total maior; ainda assim a dedicada é a que roda o modelo.
        let devs = [
            device(0, "Intel UHD", TIPO_IGPU, 32 * GIB),
            device(1, "RTX 4060", TIPO_GPU, 8 * GIB),
        ];
        assert_eq!(escolher(&devs).unwrap().nome, "RTX 4060");
    }

    #[test]
    fn entre_dedicadas_ganha_a_maior_memoria() {
        // A máquina de referência desta fase: RX 9060 XT de 16 GB e RTX 4060 de
        // 8 GB na mesma placa-mãe.
        let devs = [
            device(0, "NVIDIA GeForce RTX 4060", TIPO_GPU, 8 * GIB),
            device(1, "AMD Radeon RX 9060 XT", TIPO_GPU, 16 * GIB),
        ];
        assert_eq!(escolher(&devs).unwrap().nome, "AMD Radeon RX 9060 XT");
        // A ordem do registro não muda a escolha.
        let invertido = [devs[1].clone(), devs[0].clone()];
        assert_eq!(escolher(&invertido).unwrap().nome, "AMD Radeon RX 9060 XT");
    }

    #[test]
    fn sem_dedicada_a_integrada_serve() {
        // Num notebook Intel-only a integrada é a única GPU: o motor de GPU
        // ainda tem de funcionar, porque é ela, não a CPU.
        let devs = [device(0, "Intel Iris Xe", TIPO_IGPU, 16 * GIB)];
        let escolhida = escolher(&devs).unwrap();
        assert_eq!(escolhida.nome, "Intel Iris Xe");
        assert!(!escolhida.dedicada);
    }

    #[test]
    fn cpu_e_acelerador_nao_sao_placas() {
        let devs = [
            device(0, "CPU", TIPO_CPU, 64 * GIB),
            device(1, "AMX", TIPO_ACCEL, 8 * GIB),
        ];
        assert!(escolher(&devs).is_none());
        assert!(candidatas(&devs).is_empty());
    }

    #[test]
    fn placa_sem_memoria_declarada_e_descartada() {
        // Driver básico reporta zero: não é placa para o modelo, é ruído.
        let devs = [
            device(0, "Microsoft Basic Render", TIPO_GPU, 0),
            device(1, "AMD Radeon RX 9060 XT", TIPO_GPU, 16 * GIB),
        ];
        assert_eq!(escolher(&devs).unwrap().indice, 1);
    }

    #[test]
    fn empate_se_resolve_pelo_indice() {
        let devs = [
            device(0, "placa A", TIPO_GPU, 8 * GIB),
            device(1, "placa B", TIPO_GPU, 8 * GIB),
        ];
        assert_eq!(candidatas(&devs).first().unwrap().indice, 0);
    }

    /// A decisão de o que sai da memória é a única parte do `usar_adaptadores`
    /// que existe sem placa: o resto é chamada de biblioteca. Errar aqui custa
    /// VRAM parada ou um adaptador derrubado enquanto ainda está em uso, e os
    /// dois erros são silenciosos na resposta do modelo.
    #[test]
    fn so_sai_da_memoria_o_que_nao_foi_pedido() {
        let a = PathBuf::from("a.gguf");
        let b = PathBuf::from("b.gguf");
        let c = PathBuf::from("c.gguf");
        let carregados = [&a, &b];
        let pedidos = vec![(b.clone(), 1.0f32), (c.clone(), 0.5)];
        assert_eq!(obsoletos(&carregados, &pedidos), vec![0]);

        // Nada pedido: tudo que está carregado sobra.
        assert_eq!(obsoletos(&carregados, &[]), vec![0, 1]);
        // Tudo pedido: nada sai, e o handle continua pronto para a próxima
        // mudança de força sem reler o disco.
        let mesmos = vec![(a.clone(), 0.8f32), (b.clone(), 1.0)];
        assert!(obsoletos(&carregados, &mesmos).is_empty());
    }
}
