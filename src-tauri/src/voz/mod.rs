//! A voz do Aether: fala, escuta e os modelos que cada uma precisa.
//!
//! Tudo aqui é local. O runtime de síntese e reconhecimento é o sherpa-onnx
//! vendorizado no repositório, carregado em tempo de execução; os modelos vêm do
//! release do sherpa-onnx e do HuggingFace para o disco do aplicativo, e a
//! decisão de que foi dito acontece na máquina, nunca num serviço de terceiro.
//!
//! Se a DLL não estiver ao lado do executável, nada neste módulo funciona — e
//! nada disto impede o resto do aplicativo de abrir. A voz é um recurso, não um
//! pré-requisito do processo.

pub mod baixar;
pub mod escuta;
pub mod fala;
pub mod ffi;
pub mod modelos;

use crate::idiomas::tr;
use escuta::Escuta;
use fala::Parlante;
use ffi::Sherpa;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub use baixar::VozEvento;
pub use modelos::{CATALOGO_VOZ, EspecVoz, Servico, StatusVoz, VOZES_KOKORO, Voz};

/// O que o usuário escolheu na tela de voz, guardado em `voz.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Ajustes {
    /// Ler a resposta em voz alta quando ela terminar.
    pub auto_falar: bool,
    /// Id do modelo de fala no catálogo.
    pub modelo_fala: String,
    /// Alto-falante do modelo: 42 é a Dora, 43 o Alex, 44 o Santa.
    pub sid: i32,
    pub velocidade: f32,
    /// Id do modelo de escuta; vazio desliga o microfone.
    pub modelo_escuta: String,
    /// Nome do microfone escolhido na tela; vazio é o que o sistema usa.
    pub entrada: String,
}

impl Default for Ajustes {
    fn default() -> Self {
        Ajustes {
            auto_falar: false,
            modelo_fala: "kokoro-ptbr".to_string(),
            // A primeira voz da lista é feminina: é a que mais gente espera
            // ouvir ao ligar a fala pela primeira vez.
            sid: 42,
            velocidade: 1.0,
            modelo_escuta: "whisper-base".to_string(),
            // Ninguém é gravado por um microfone que o dono não escolheu: sem
            // escolha, vale o padrão do Windows.
            entrada: String::new(),
        }
    }
}

impl Ajustes {
    /// Um ajuste lido do disco só vale se o modelo existir no catálogo e for do
    /// serviço: um `voz.json` escrito à mão não pode apontar a fala para um
    /// Whisper.
    fn valido(&self) -> Result<(), String> {
        modelo_de(&self.modelo_fala, Servico::Fala)?;
        if self.modelo_escuta.is_empty() {
            return Ok(());
        }
        modelo_de(&self.modelo_escuta, Servico::Escuta).map(|_| ())
    }

    /// As escolhas guardadas. Um arquivo que não existe ainda é o padrão; um que
    /// existe e não bate é um defeito que a tela precisa ver, não engolir.
    fn carregar(caminho: &Path) -> Result<Ajustes, String> {
        let texto = match std::fs::read_to_string(caminho) {
            Ok(texto) => texto,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Ajustes::default()),
            Err(e) => {
                return Err(tr(
                    "erros.vozFalhaLerAjustes",
                    &[
                        ("caminho", &caminho.display().to_string()),
                        ("detalhe", &e.to_string()),
                    ],
                ));
            }
        };
        let ajustes: Ajustes = serde_json::from_str(&texto).map_err(|e| {
            tr(
                "erros.vozAjustesInvalidos",
                &[
                    ("caminho", &caminho.display().to_string()),
                    ("detalhe", &e.to_string()),
                ],
            )
        })?;
        ajustes.valido()?;
        Ok(Ajustes {
            velocidade: na_faixa(ajustes.velocidade),
            ..ajustes
        })
    }

    /// Escrita atômica: ou o JSON inteiro está no disco, ou fica o anterior. Um
    /// meio arquivo lido na próxima abertura seria um ajuste perdido em silêncio.
    fn salvar(&self, caminho: &Path) -> Result<(), String> {
        if let Some(pai) = caminho.parent() {
            std::fs::create_dir_all(pai).map_err(|e| {
                tr(
                    "erros.vozFalhaCriarPasta",
                    &[("pasta", &pai.display().to_string()), ("detalhe", &e.to_string())],
                )
            })?;
        }
        let texto = serde_json::to_string_pretty(self)
            .map_err(|e| tr("erros.vozFalhaSerializar", &[("detalhe", &e.to_string())]))?;
        let temporario = caminho.with_extension("json.tmp");
        std::fs::write(&temporario, &texto).map_err(|e| {
            tr(
                "erros.vozFalhaEscrever",
                &[("caminho", &temporario.display().to_string()), ("detalhe", &e.to_string())],
            )
        })?;
        std::fs::rename(&temporario, caminho).map_err(|e| {
            tr(
                "erros.vozFalhaSubstituir",
                &[("caminho", &caminho.display().to_string()), ("detalhe", &e.to_string())],
            )
        })?;
        Ok(())
    }
}

/// A mesma faixa que o falante impõe ao pedido: guardar uma velocidade que ele
/// recusaria é prometer um ritmo que nunca toca.
fn na_faixa(velocidade: f32) -> f32 {
    velocidade.clamp(fala::VELOCIDADE_MÍNIMA, fala::VELOCIDADE_MÁXIMA)
}

/// O pulso que a tela lê a cada quadro: três atômicos do motor, nenhum acesso a
/// disco. É o que mantém o medidor do microfone e o botão de parar a fala vivos
/// sem reler o catálogo de modelos.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Medidor {
    pub falando: bool,
    pub ouvindo: bool,
    pub nivel: f32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Dispositivos {
    /// Nome do alto-falante padrão do sistema agora.
    pub saida: Option<String>,
    /// O microfone que a captura vai abrir: o escolhido, ou o padrão do sistema.
    pub entrada: Option<String>,
    /// Todo microfone que o sistema mostra, na ordem dele.
    pub entradas: Vec<String>,
}

/// O que a tela de voz lê: se a voz existe, o que está escolhido, o que está
/// instalado e o que o microfone está fazendo agora.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EstadoVoz {
    pub disponivel: bool,
    /// Por que não está disponível, quando não está.
    pub motivo: Option<String>,
    pub ajustes: Ajustes,
    pub modelos: Vec<StatusVoz>,
    pub vozes: &'static [Voz],
    pub falando: bool,
    pub ouvindo: bool,
    /// Pico do microfone neste instante, de 0 a 1.
    pub nivel: f32,
    /// O que está errado agora: a fala ou a escuta que falhou, ou o `voz.json`
    /// que não pôde ser lido.
    pub erro: Option<String>,
    pub dispositivos: Dispositivos,
    /// Ids dos modelos de voz com download em curso.
    pub baixando: Vec<String>,
}

/// A voz inteira do aplicativo: o runtime carregado, a boca, o ouvido e as
/// escolhas guardadas.
pub struct MotorVoz {
    sherpa: Option<Arc<Sherpa>>,
    motivo: Option<String>,
    parlante: Option<Parlante>,
    escuta: Option<Escuta>,
    ajustes: Mutex<Ajustes>,
    /// Um `voz.json` ilegível: a voz segue com o padrão, mas a tela fica sabendo.
    aviso: Option<String>,
    /// `models/voz`, onde os modelos moram.
    raiz: PathBuf,
    ajustes_path: PathBuf,
    /// Um cancelador por download em curso: a UI precisa poder parar sem
    /// esperar a barra chegar ao fim.
    downloads: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

impl MotorVoz {
    /// Abre o sherpa-onnx e lê as escolhas guardadas. Nada aqui baixa modelo
    /// nenhum nem abre peso: só carrega a DLL e o JSON.
    pub fn novo(pastas_dll: &[PathBuf], raiz: PathBuf, ajustes_path: PathBuf) -> MotorVoz {
        let (sherpa, motivo) = match Sherpa::abrir(pastas_dll) {
            Ok((sherpa, _caminho)) => (Some(Arc::new(sherpa)), None),
            Err(motivo) => (None, Some(motivo)),
        };
        let (parlante, escuta) = match sherpa.as_ref() {
            Some(sherpa) => (
                Parlante::novo(Arc::clone(sherpa)).ok(),
                Some(Escuta::novo(Arc::clone(sherpa))),
            ),
            None => (None, None),
        };
        let (ajustes, aviso) = match Ajustes::carregar(&ajustes_path) {
            Ok(ajustes) => (ajustes, None),
            Err(aviso) => (Ajustes::default(), Some(aviso)),
        };
        MotorVoz {
            sherpa,
            motivo,
            parlante,
            escuta,
            aviso,
            ajustes: Mutex::new(ajustes),
            raiz,
            ajustes_path,
            downloads: Mutex::new(HashMap::new()),
        }
    }

    pub fn disponivel(&self) -> bool {
        self.sherpa.is_some()
    }

    pub fn ajustes(&self) -> Ajustes {
        self.ajustes.lock().expect("ajustes de voz envenenados").clone()
    }

    /// Grava as escolhas e as devolve. Cada modelo é conferido contra o
    /// catálogo: um id que não existe é recusado em vez de trocado por outro.
    pub fn ajustar(&self, novos: Ajustes) -> Result<Ajustes, String> {
        novos.valido()?;
        let guardados = Ajustes {
            velocidade: na_faixa(novos.velocidade),
            ..novos
        };
        guardados.salvar(&self.ajustes_path)?;
        *self.ajustes.lock().expect("ajustes de voz envenenados") = guardados.clone();
        Ok(guardados)
    }

    /// Fala com a voz e a velocidade escolhidas. É o caminho da resposta do
    /// assistente: o texto vem como está e a limpeza é a do falante.
    pub fn dizer(&self, texto: &str) -> Result<(), String> {
        let ajustes = self.ajustes();
        self.falar_com(texto, &ajustes.modelo_fala, ajustes.sid, ajustes.velocidade)
    }

    /// Amostra uma voz da lista sem mudar o que está guardado.
    pub fn prever(
        &self,
        texto: &str,
        id_voz: &str,
        sid: i32,
        velocidade: f32,
    ) -> Result<(), String> {
        self.falar_com(texto, id_voz, sid, velocidade)
    }

    fn falar_com(
        &self,
        texto: &str,
        id_voz: &str,
        sid: i32,
        velocidade: f32,
    ) -> Result<(), String> {
        let parlante = self.parlante.as_ref().ok_or_else(|| self.indisponivel())?;
        let espec = modelo_de(id_voz, Servico::Fala)?;
        self.baixado(espec)?;
        parlante.dizer(&modelos::modelo_dir(&self.raiz, espec), texto, sid, velocidade)
    }

    /// Grava em WAV o áudio exato que passou pelo alto-falante. É a prova de fim
    /// a fim da fala: sem ela, só a orelha de quem ouve confere se saiu som.
    pub fn gravar(&self, texto: &str, destino: &Path) -> Result<(), String> {
        let ajustes = self.ajustes();
        let parlante = self.parlante.as_ref().ok_or_else(|| self.indisponivel())?;
        let espec = modelo_de(&ajustes.modelo_fala, Servico::Fala)?;
        self.baixado(espec)?;
        parlante.dizer_e_gravar(
            &modelos::modelo_dir(&self.raiz, espec),
            texto,
            ajustes.sid,
            ajustes.velocidade,
            destino,
        )
    }

    pub fn parar_fala(&self) {
        if let Some(parlante) = self.parlante.as_ref() {
            parlante.parar();
        }
    }

    pub fn falando(&self) -> bool {
        self.parlante.as_ref().is_some_and(|p| p.falando())
    }

    /// Liga o microfone com o modelo de escuta escolhido.
    pub fn ouvir(&self) -> Result<(), String> {
        let ajustes = self.ajustes();
        let escuta = self.escuta.as_ref().ok_or_else(|| self.indisponivel())?;
        if ajustes.modelo_escuta.is_empty() {
            return Err(tr("erros.vozSemModeloDeEscuta", &[]));
        }
        let espec = modelo_de(&ajustes.modelo_escuta, Servico::Escuta)?;
        self.baixado(espec)?;
        escuta.ouvir(ajustes.entrada)
    }

    /// Desliga o microfone e devolve o texto que o Whisper leu.
    pub fn transcrever(&self) -> Result<String, String> {
        let ajustes = self.ajustes();
        let escuta = self.escuta.as_ref().ok_or_else(|| self.indisponivel())?;
        let espec = modelo_de(&ajustes.modelo_escuta, Servico::Escuta)?;
        escuta.transcrever(&modelos::modelo_dir(&self.raiz, espec), espec)
    }

    /// Interrompe a captura sem transcrever: soltar o botão para cancelar não é
    /// pedir texto de um áudio que ninguém quis gravar.
    pub fn descartar_escuta(&self) {
        if let Some(escuta) = self.escuta.as_ref() {
            escuta.descartar();
        }
    }

    /// Solta um modelo do disco para que ele possa ser apagado. No Windows um
    /// `.onnx` aberto pela onnxruntime não é removível, então apagar sem isto
    /// deixaria um erro de sistema no meio de uma ação do usuário.
    ///
    /// A fala espera o acuse da thread que abriu o modelo; sem acuse, devolve
    /// erro e nada é apagado.
    pub fn liberar(&self, espec: &EspecVoz) -> Result<(), String> {
        let dir = modelos::modelo_dir(&self.raiz, espec);
        match espec.servico {
            Servico::Fala => match self.parlante.as_ref() {
                Some(parlante) if parlante.largar(&dir) => Ok(()),
                Some(_) => Err(tr(
                    "erros.vozFalaNaoLiberou",
                    &[("nome", espec.nome)],
                )),
                // Sem motor aberto não há instância nenhuma segurando o disco.
                None => Ok(()),
            },
            Servico::Escuta => {
                if let Some(escuta) = self.escuta.as_ref() {
                    escuta.largar(&dir);
                }
                Ok(())
            }
        }
    }

    pub fn ouvindo(&self) -> bool {
        self.escuta.as_ref().is_some_and(|e| e.ouvindo())
    }

    pub fn nivel(&self) -> f32 {
        self.escuta.as_ref().map(Escuta::nivel).unwrap_or(0.0)
    }

    /// O que a tela de voz mostra inteiro, lido do disco e das threads agora.
    pub fn estado(&self) -> EstadoVoz {
        let erro_fala = self.parlante.as_ref().and_then(|p| p.erro());
        let erro_escuta = self.escuta.as_ref().and_then(|e| e.erro());
        let erro = erro_fala.or(erro_escuta).or_else(|| self.aviso.clone());
        let ajustes = self.ajustes();
        EstadoVoz {
            disponivel: self.disponivel(),
            motivo: self.motivo.clone(),
            modelos: modelos::todos_status(&self.raiz),
            vozes: &VOZES_KOKORO,
            falando: self.falando(),
            ouvindo: self.ouvindo(),
            nivel: self.nivel(),
            erro,
            dispositivos: ler_dispositivos(&ajustes.entrada),
            baixando: self.baixando(),
            ajustes,
        }
    }

    /// Onde os modelos de voz ficam: serve para o gerenciador apagar o LLM sem
    /// tocar nesta pasta.
    pub fn raiz(&self) -> &Path {
        &self.raiz
    }

    fn indisponivel(&self) -> String {
        self.motivo
            .clone()
            .unwrap_or_else(|| tr("erros.vozNaoDisponivel", &[]))
    }

    fn baixado(&self, espec: &'static EspecVoz) -> Result<(), String> {
        if modelos::status(&self.raiz, espec).baixado {
            Ok(())
        } else {
            Err(tr("erros.vozModeloNaoBaixado", &[("nome", espec.nome)]))
        }
    }

    /// Registra um download. Devolve None se este modelo já está baixando: duas
    /// tarefas escreveriam no mesmo `.part`.
    pub fn iniciar_download(&self, id: &str) -> Option<Arc<AtomicBool>> {
        let mut cursos = self.downloads.lock().expect("downloads de voz envenenados");
        if cursos.contains_key(id) {
            return None;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        cursos.insert(id.to_string(), Arc::clone(&cancel));
        Some(cancel)
    }

    pub fn encerrar_download(&self, id: &str) {
        self.downloads
            .lock()
            .expect("downloads de voz envenenados")
            .remove(id);
    }

    pub fn cancelar_download(&self, id: &str) -> bool {
        match self
            .downloads
            .lock()
            .expect("downloads de voz envenenados")
            .get(id)
        {
            Some(cancel) => {
                cancel.store(true, Ordering::Relaxed);
                true
            }
            None => false,
        }
    }

    pub fn baixando(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .downloads
            .lock()
            .expect("downloads de voz envenenados")
            .keys()
            .cloned()
            .collect();
        ids.sort();
        ids
    }
}

/// Acha a entrada do catálogo e confere o serviço: não se fala com um Whisper
/// nem se escuta com um Kokoro.
pub fn modelo_de(id: &str, servico: Servico) -> Result<&'static EspecVoz, String> {
    let espec =
        modelos::encontrar(id).ok_or_else(|| tr("erros.vozModeloNaoCatalogado", &[("id", id)]))?;
    if espec.servico != servico {
        let pedido = nome_servico(servico);
        return Err(tr(
            "erros.vozServicoTrocado",
            &[("nome", espec.nome), ("servico", &pedido)],
        ));
    }
    Ok(espec)
}

fn nome_servico(servico: Servico) -> String {
    match servico {
        Servico::Fala => tr("voz.servicoFala", &[]),
        Servico::Escuta => tr("voz.servicoEscuta", &[]),
    }
}

/// Os aparelhos que a voz pode usar, lidos na hora: um fone desligado some da
/// lista entre duas aberturas da tela, e uma lista velha viraria um botão morto.
fn ler_dispositivos(escolhida: &str) -> Dispositivos {
    use cpal::traits::{DeviceTrait, HostTrait};
    let host = cpal::default_host();
    let nome = |device: &cpal::Device| {
        device
            .description()
            .ok()
            .map(|d| d.name().to_owned())
            .filter(|n| !n.is_empty())
    };
    let entradas = host
        .input_devices()
        .map(|todos| todos.filter_map(|d| nome(&d)).collect())
        .unwrap_or_default();
    let escolha = (!escolhida.is_empty()).then(|| escolhida.to_owned());
    Dispositivos {
        saida: host.default_output_device().as_ref().and_then(&nome),
        // O que a captura vai abrir: a escolha da tela, ou o do sistema.
        entrada: escolha.or_else(|| host.default_input_device().as_ref().and_then(&nome)),
        entradas,
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    /// Um motor aberto sem DLL nenhuma: a voz está desligada, mas nada disto
    /// quebra, e toda recusa chega com motivo.
    fn sem_dll() -> MotorVoz {
        let raiz =
            std::env::temp_dir().join(format!("aether-voz-sem-dll-{}", std::process::id()));
        MotorVoz::novo(&[], raiz.clone(), raiz.join("voz.json"))
    }

    #[test]
    fn o_padrao_fala_com_a_primeira_voz_do_kokoro() {
        let ajustes = Ajustes::default();
        assert_eq!(ajustes.modelo_fala, "kokoro-ptbr");
        assert_eq!(ajustes.sid, 42);
        assert_eq!(ajustes.velocidade, 1.0);
        assert!(ajustes.valido().is_ok());
        // Ninguém é falado por acaso na primeira vez: o auto-falar é escolha.
        assert!(!ajustes.auto_falar);
    }

    #[test]
    fn um_whisper_nao_fala_e_um_kokoro_nao_escuta() {
        assert!(modelo_de("whisper-base", Servico::Fala).is_err());
        assert!(modelo_de("kokoro-ptbr", Servico::Escuta).is_err());
        assert!(modelo_de("quem-nunca-existiu", Servico::Fala).is_err());
        assert_eq!(modelo_de("whisper-tiny", Servico::Escuta).unwrap().nome, "Whisper tiny");
    }

    #[test]
    fn um_ajuste_fora_do_catalogo_e_recusado() {
        let ruim = Ajustes {
            modelo_fala: "whisper-base".to_string(),
            ..Ajustes::default()
        };
        assert!(ruim.valido().is_err());
        // Desligar o microfone é legítimo: o campo vazio passa.
        let sem_microfone = Ajustes {
            modelo_escuta: String::new(),
            ..Ajustes::default()
        };
        assert!(sem_microfone.valido().is_ok());
    }

    #[test]
    fn o_arquivo_de_ajustes_sobrevive_a_uma_releitura() {
        let caminho =
            std::env::temp_dir().join(format!("aether-voz-ajustes-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&caminho);
        // Sem arquivo, o padrão: isso não é um erro, é a primeira vez.
        assert_eq!(Ajustes::carregar(&caminho).unwrap().sid, 42);

        let escolhidos = Ajustes {
            auto_falar: true,
            sid: 43,
            velocidade: 1.4,
            ..Ajustes::default()
        };
        escolhidos.salvar(&caminho).unwrap();
        let lidos = Ajustes::carregar(&caminho).unwrap();
        assert_eq!((lidos.sid, lidos.velocidade, lidos.auto_falar), (43, 1.4, true));
        // A escrita atômica não deixa o temporário para trás.
        assert!(!caminho.with_extension("json.tmp").exists());

        // Velocidade fora da faixa volta para o que o falante aceita.
        Ajustes {
            velocidade: 9.0,
            ..Ajustes::default()
        }
        .salvar(&caminho)
        .unwrap();
        assert_eq!(Ajustes::carregar(&caminho).unwrap().velocidade, 2.0);

        std::fs::write(&caminho, "{ isto não é json").unwrap();
        assert!(Ajustes::carregar(&caminho).is_err());
        let _ = std::fs::remove_file(&caminho);
    }

    #[test]
    fn sem_dll_a_voz_recusa_com_motivo_e_o_resto_vive() {
        let motor = sem_dll();
        assert!(!motor.disponivel());
        let estado = motor.estado();
        assert!(!estado.disponivel);
        assert!(estado.motivo.is_some());
        assert!(motor.ouvir().is_err());
        assert!(motor.dizer("oi").is_err());
        assert!(motor.transcrever().is_err());
        // Parar o que nunca começou é inocente, não um pânico.
        motor.parar_fala();
        motor.descartar_escuta();
        assert_eq!(motor.nivel(), 0.0);
        assert!(!motor.falando());
        assert!(!motor.ouvindo());
        // O catálogo continua legível: a tela mostra o que baixar mesmo sem voz.
        assert_eq!(motor.estado().modelos.len(), CATALOGO_VOZ.len());
    }

    #[test]
    fn um_unico_download_por_modelo() {
        let motor = sem_dll();
        assert!(motor.iniciar_download("whisper-tiny").is_some());
        assert_eq!(motor.baixando(), vec!["whisper-tiny".to_string()]);
        assert!(motor.iniciar_download("whisper-tiny").is_none());
        assert!(motor.iniciar_download("kokoro-ptbr").is_some());
        assert!(motor.cancelar_download("whisper-tiny"));
        assert!(!motor.cancelar_download("quem-nao-esta-baixando"));
        motor.encerrar_download("whisper-tiny");
        motor.encerrar_download("kokoro-ptbr");
        assert!(motor.baixando().is_empty());
    }

    /// As pastas onde o aplicativo procura a DLL. Num teste o executável está em
    /// `target/debug/deps`, um nível abaixo de onde o build a publica.
    fn pastas_da_dll() -> Vec<PathBuf> {
        let exe = std::env::current_exe().expect("o teste roda de um executável");
        let mut pastas = Vec::new();
        let mut atual = exe.parent().map(Path::to_path_buf);
        for _ in 0..3 {
            let Some(pasta) = atual else { break };
            if pasta.join("sherpa-onnx-c-api.dll").is_file() {
                pastas.push(pasta.clone());
            }
            atual = pasta.parent().map(Path::to_path_buf);
        }
        pastas
    }

    /// A mesma raiz que o aplicativo monta em `app_local_data_dir()/models/voz`.
    fn raiz_dos_modelos() -> PathBuf {
        let base = if cfg!(windows) {
            std::env::var_os("LOCALAPPDATA").expect("o Windows tem LOCALAPPDATA")
        } else {
            std::env::var_os("XDG_DATA_HOME").or_else(|| {
                std::env::var_os("HOME")
                    .map(|h| format!("{}/.local/share", h.to_string_lossy()).into())
            }).expect("uma pasta de dados do usuário")
        };
        PathBuf::from(base).join("ai.aether.core").join("models").join("voz")
    }

    /// O PCM16 mono que o sherpa escreveu, com a taxa que está no próprio
    /// cabeçalho: a prova não pode presumir os 24 kHz do Kokoro.
    fn ler_wav(caminho: &Path) -> (Vec<f32>, u32) {
        let bytes = std::fs::read(caminho).expect("o WAV foi escrito");
        let taxa = u32::from_le_bytes(bytes[24..28].try_into().expect("cabeçalho RIFF tem 28 bytes"));
        let inicio = bytes
            .windows(4)
            .position(|janela| janela == b"data")
            .expect("o WAV tem o chunk data");
        let quantidade =
            u32::from_le_bytes(bytes[inicio + 4..inicio + 8].try_into().expect("chunk data tem tamanho"))
                as usize;
        let corpo = &bytes[inicio + 8..];
        let amostras = corpo
            .chunks_exact(2)
            .take(quantidade / 2)
            .map(|par| i16::from_le_bytes([par[0], par[1]]) as f32 / 32768.0)
            .collect();
        (amostras, taxa)
    }

    /// A prova de ponta a ponta na máquina, com o runtime e os modelos reais: a
    /// frase que o Kokoro fala é a frase que o Whisper escreve. Roda só quando
    /// alguém pede, porque precisa da DLL vendorizada e dos ~280 MB de modelos
    /// baixados — e fala alto, no alto-falante que estiver padrão.
    #[test]
    #[ignore]
    fn o_que_o_kokoro_fala_e_o_que_o_whisper_ouve() {
        let pastas = pastas_da_dll();
        assert!(!pastas.is_empty(), "a DLL do sherpa não está ao lado do executável");
        let raiz = raiz_dos_modelos();
        let kokoro = modelos::encontrar("kokoro-ptbr").expect("kokoro-ptbr está no catálogo");
        let whisper = modelos::encontrar("whisper-base").expect("whisper-base está no catálogo");
        let dir_fala = modelos::modelo_dir(&raiz, kokoro);
        let dir_escuta = modelos::modelo_dir(&raiz, whisper);
        for pasta in [&dir_fala, &dir_escuta] {
            assert!(pasta.is_dir(), "baixe os modelos de voz antes: {}", pasta.display());
        }

        let (sherpa, _) = ffi::Sherpa::abrir(&pastas).expect("a DLL abre");
        let sherpa = Arc::new(sherpa);
        let parlante = fala::Parlante::novo(Arc::clone(&sherpa)).expect("o falante liga");
        let escuta = escuta::Escuta::novo(Arc::clone(&sherpa));

        let wav =
            std::env::temp_dir().join(format!("aether-voz-roda-{}.wav", std::process::id()));
        let _ = std::fs::remove_file(&wav);
        let frase = "Boa tarde. Eu escolho a minha voz e falo por aqui.";
        parlante
            .dizer_e_gravar(&dir_fala, frase, 42, 1.0, &wav)
            .expect("a fala entrou na fila");

        // Gravar é assíncrono: o arquivo aparece quando o sherpa termina a frase.
        let limite = std::time::Instant::now() + std::time::Duration::from_secs(180);
        while !wav.is_file() {
            assert!(
                std::time::Instant::now() < limite,
                "o WAV não chegou: {}",
                wav.display()
            );
            std::thread::sleep(std::time::Duration::from_millis(200));
        }

        let (amostras, taxa) = ler_wav(&wav);
        assert!(taxa > 8_000, "a taxa do WAV está errada: {taxa}");
        assert!(
            amostras.len() > (taxa as usize) / 2,
            "meio segundo de áudio não é uma frase: {} amostras a {taxa} Hz",
            amostras.len()
        );
        let pico = amostras.iter().fold(0.0f32, |maior, a| maior.max(a.abs()));
        assert!(pico > 0.05, "o WAV gravou silêncio: pico {pico}");

        let ouvido = escuta
            .decodificar(&dir_escuta, whisper, &amostras, taxa)
            .expect("o whisper respondeu");
        let _ = std::fs::remove_file(&wav);
        println!("dito:   {frase}\nouvido: {ouvido}");

        for palavra in ["escolho", "voz", "falo"] {
            assert!(
                ouvido.to_lowercase().contains(palavra),
                "a frase falada foi \"{frase}\" e o whisper escreveu \"{ouvido}\"; faltou \"{palavra}\""
            );
        }

        // A mesma frase na taxa em que um microfone de desktop entrega o áudio.
        // É o caminho da captura: se o decodificador não honrar a taxa que recebe,
        // a fala sai acelerada e o Whisper escreve outra coisa — medido, a 48 kHz
        // vem a mesma transcrição de 24 kHz.
        let em_48 = rematar(&amostras, taxa, 48_000);
        let ouvido_em_48 = escuta
            .decodificar(&dir_escuta, whisper, &em_48, 48_000)
            .expect("o whisper respondeu a 48 kHz");
        println!("ouvido a 48 kHz: {ouvido_em_48}");
        for palavra in ["escolho", "voz", "falo"] {
            assert!(
                ouvido_em_48.to_lowercase().contains(palavra),
                "a 48 kHz a frase \"{frase}\" virou \"{ouvido_em_48}\"; faltou \"{palavra}\""
            );
        }
    }

    /// Reamostragem linear, a mesma que um aparelho de áudio faz entre duas taxas.
    fn rematar(amostras: &[f32], de: u32, para: u32) -> Vec<f32> {
        if de == para || de == 0 {
            return amostras.to_vec();
        }
        let total = amostras.len() as u64 * para as u64 / de as u64;
        (0..total)
            .map(|i| {
                let x = i as f64 * de as f64 / para as f64;
                let base = (x as usize).min(amostras.len() - 1);
                let vizinho = (base + 1).min(amostras.len() - 1);
                let t = (x - base as f64) as f32;
                amostras[base] * (1.0 - t) + amostras[vizinho] * t
            })
            .collect()
    }
}
