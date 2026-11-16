//! Onde o modelo vai morar, medido antes de ele ser carregado.
//!
//! A pergunta que este módulo responde é a única que decide a velocidade de uma
//! conversa: os pesos cabem na placa? A resposta sai de três medidas somadas —
//! os bytes de tensor do próprio arquivo, o KV cache da janela pedida e a folga
//! que a biblioteca reserva para os buffers de computação — contra a VRAM livre
//! que a ggml respondeu agora.
//!
//! Nada aqui é chutado, e as duas contas que parecem fórmula têm prova:
//!
//! - **KV cache**: `2 (K e V) * camadas * cabeças_kv * dimensao_cabeca * 2
//!   bytes`. No granite-4.2-3b Q4_K_M com janela de 4096 isso dá 335.544.320
//!   bytes, e a biblioteca escreveu `llama_kv_cache: size = 320.00 MiB (4096
//!   cells, 40 layers, 1/1 seqs), K (f16): 160.00 MiB, V (f16): 160.00 MiB` —
//!   320,00 MiB é exatamente 335.544.320 / 1048576. Os 2 bytes são o f16 que o
//!   flash attention ligado (`ffi.rs`, `ATENCAO_RAPIDA`) pede para K e V.
//! - **bytes de tensor**: `elementos / bloco * tamanho_do_bloco` com o dtype e a
//!   forma que estão no cabeçalho, lidos pelo parser que o projeto já usa para
//!   carregar os pesos. Somando os tensores do mesmo arquivo dá 2136,65 MiB, e a
//!   carga respondeu `Vulkan0 model buffer size = 1998.84 MiB` mais
//!   `CPU_Mapped model buffer size = 137.81 MiB` — os mesmos 2136,65 MiB.

use super::ggml;
use crate::idiomas::tr;
use anyhow::{Result, bail};
use candle::quantized::gguf_file;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// O que a biblioteca reserva além dos pesos e do KV: os buffers de computação
/// do grafo, por onde passam os ativos. Medido na granite-4.2-3b em janela de
/// 4096 com flash attention: `sched_reserve: Vulkan0 compute buffer size =
/// 220.02 MiB`. É o único número daqui que não vem do arquivo do modelo — ele
/// depende do grafo, então vale como folga, não como medida daquele modelo.
const FOLGA_DE_COMPUTACAO: u64 = 220 * 1024 * 1024;

/// A placa que a ggml escolheria agora, com o que ela tem de livre neste
/// instante. `None` é a resposta honesta de "esta máquina não tem placa".
#[derive(Debug, Clone)]
pub struct Capacidade {
    pub placa: String,
    pub livre: u64,
    pub total: u64,
}

/// Lê o registro de dispositivos e aplica a mesma preferência do motor: a
/// dedicada de maior memória. É a mesma função que a carga usa, então o que a
/// tela anuncia e o que roda são a mesma placa por construção, não por sorte.
pub fn capacidade(dlls: &[PathBuf]) -> Result<Option<Capacidade>> {
    let dispositivos = ggml::registro(dlls).map_err(anyhow::Error::msg)?;
    let Some(escolhida) = ggml::escolher(&dispositivos) else {
        return Ok(None);
    };
    let bruto = &dispositivos[escolhida.indice];
    Ok(Some(Capacidade {
        placa: escolhida.nome.clone(),
        livre: bruto.livre as u64,
        total: bruto.total as u64,
    }))
}

/// O que o cabeçalho de um GGUF diz sobre o tamanho das coisas. Os tensores não
/// são lidos: só as fichas deles, que é onde estão a forma e o dtype.
#[derive(Debug, Clone)]
pub struct Ficha {
    pub arquivo: String,
    pub arquitetura: String,
    pub camadas: usize,
    /// Bytes de todos os tensores do arquivo, somados do dtype e da forma.
    pub pesos_total: u64,
    /// Bytes de cada `blk.N.*`, por índice de camada. Denso e MoE têm a mesma
    /// entrada aqui: quem difere entre camadas difere na lista, não na conta.
    pub camada: Vec<u64>,
    /// O que não pertence a nenhuma camada: tabela de tokens, cabeça de saída,
    /// normalizações. Fica do lado de fora da conta de camadas porque é isso que
    /// a biblioteca faz quando a placa aperta.
    pub fixo: u64,
    /// Bytes de KV cache por token, no dtype que o flash attention usa.
    pub kv_por_token: u64,
    /// A janela que o arquivo declara — o teto do que o modelo aceita.
    pub janela_maxima: usize,
    /// O template de conversa que o próprio arquivo publica. É o que a biblioteca
    /// usaria ao renderizar o prompt; o aplicativo usa o mesmo, porque um
    /// template de outro lugar — o repositório de pesos, por exemplo — pode ser
    /// outra versão, e aí a gramática de chamada de ferramenta que o modelo
    /// aprendeu não é a que ele está sendo convidado a falar.
    pub template: Option<String>,
    /// Os ids que encerram uma resposta, lidos do arquivo: `eos_token_id` e,
    /// quando o modelo separa os dois, `eot_token_id`.
    pub eos: Vec<u32>,
    /// Os mesmos ids escritos como tokens, que é a forma que a geração observa
    /// como sequência de parada. Vêm do vocabulário do próprio arquivo, não de
    /// uma lista de marcadores supostos.
    pub paradas: Vec<String>,
}

fn inteiro(metadata: &HashMap<String, gguf_file::Value>, chave: &str) -> Option<u32> {
    match metadata.get(chave) {
        Some(gguf_file::Value::U32(v)) => Some(*v),
        Some(gguf_file::Value::U64(v)) => u32::try_from(*v).ok(),
        Some(gguf_file::Value::I32(v)) if *v >= 0 => Some(*v as u32),
        _ => None,
    }
}

fn lista_de_textos<'a>(
    metadata: &'a HashMap<String, gguf_file::Value>,
    chave: &str,
) -> Option<&'a [gguf_file::Value]> {
    match metadata.get(chave) {
        Some(gguf_file::Value::Array(itens)) => Some(itens),
        _ => None,
    }
}

/// O que o arquivo diz sobre como falar com o modelo: template, fins de
/// resposta e os tokens que os nomeiam. Um arquivo sem vocabulário escrito nele
/// devolve as partes que tem — quem chama decide o que fazer com a ausência.
fn vocabulario(
    metadata: &HashMap<String, gguf_file::Value>,
) -> (Option<String>, Vec<u32>, Vec<String>) {
    let template = match metadata.get("tokenizer.chat_template") {
        Some(gguf_file::Value::String(t)) if !t.is_empty() => Some(t.clone()),
        _ => None,
    };
    let mut eos: Vec<u32> = Vec::new();
    for chave in ["tokenizer.ggml.eos_token_id", "tokenizer.ggml.eot_token_id"] {
        if let Some(id) = inteiro(metadata, chave) {
            if !eos.contains(&id) {
                eos.push(id);
            }
        }
    }
    let tokens = lista_de_textos(metadata, "tokenizer.ggml.tokens");
    let paradas: Vec<String> = eos
        .iter()
        .filter_map(|id| tokens?.get(*id as usize))
        .filter_map(|v| match v {
            gguf_file::Value::String(t) if !t.is_empty() => Some(t.clone()),
            _ => None,
        })
        .collect();
    (template, eos, paradas)
}

fn numero(metadata: &HashMap<String, gguf_file::Value>, chave: &str) -> Result<u64> {
    match metadata.get(chave) {
        Some(gguf_file::Value::U32(v)) => Ok(*v as u64),
        Some(gguf_file::Value::U64(v)) => Ok(*v),
        Some(gguf_file::Value::I32(v)) if *v >= 0 => Ok(*v as u64),
        _ => bail!(tr("erros.metadadoAusente", &[("chave", chave)])),
    }
}

fn texto(metadata: &HashMap<String, gguf_file::Value>, chave: &str) -> String {
    match metadata.get(chave) {
        Some(gguf_file::Value::String(v)) => v.clone(),
        _ => String::new(),
    }
}

/// Quantos bytes um tensor ocupa no arquivo: o dtype diz o tamanho do bloco e a
/// forma diz quantos elementos há. `elementos / bloco * tamanho_do_bloco` é a
/// conta do próprio ggml, e vale para f32/f16 também, cujo bloco é 1.
fn bytes_do_tensor(info: &gguf_file::TensorInfo) -> u64 {
    let elementos: u64 = info.shape.dims().iter().map(|d| *d as u64).product();
    let bloco = info.ggml_dtype.block_size() as u64;
    if bloco == 0 {
        return 0;
    }
    // O ggml arredonda para cima: um tensor cujo número de elementos não é
    // múltiplo do bloco ainda paga o bloco inteiro.
    elementos.div_ceil(bloco) * info.ggml_dtype.type_size() as u64
}

/// Os bytes de `blk.N.*` somados por camada. A camada de um tensor se lê do
/// nome, que é o que o contêiner GGUF usa para dizer a quem o tensor pertence.
fn por_camada(tensor_infos: &HashMap<String, gguf_file::TensorInfo>, camadas: usize) -> (Vec<u64>, u64) {
    let mut soma = vec![0u64; camadas];
    let mut fixo = 0u64;
    for (nome, info) in tensor_infos {
        let bytes = bytes_do_tensor(info);
        match camada_do_nome(nome, camadas) {
            Some(indice) => soma[indice] += bytes,
            None => fixo += bytes,
        }
    }
    (soma, fixo)
}

fn camada_do_nome(nome: &str, camadas: usize) -> Option<usize> {
    let resto = nome.strip_prefix("blk.")?;
    let fim = resto.find('.')?;
    match resto[..fim].parse::<usize>() {
        Ok(indice) if indice < camadas => Some(indice),
        _ => None,
    }
}

/// Abre o GGUF e tira dele o que a decisão precisa. Um arquivo que não é GGUF,
/// ou a que faltam as chaves de geometria, é recusado aqui e não na tela.
pub fn fichar(caminho: &Path) -> Result<Ficha> {
    let aberto = std::fs::File::open(caminho).map_err(|e| {
        anyhow::anyhow!(tr(
            "erros.falhaLer",
            &[("caminho", &caminho.display().to_string()), ("detalhe", &e.to_string())]
        ))
    })?;
    let mut leitor = std::io::BufReader::new(aberto);
    let cabecalho = gguf_file::Content::read(&mut leitor).map_err(|e| {
        anyhow::anyhow!(tr(
            "erros.alojamentoGguf",
            &[("caminho", &caminho.display().to_string()), ("detalhe", &e.to_string())]
        ))
    })?;
    let md = &cabecalho.metadata;
    let arquitetura = texto(md, "general.architecture");
    if arquitetura.is_empty() {
        bail!(tr("erros.alojamentoSemArquitetura", &[]));
    }
    let prefixo = format!("{arquitetura}.");
    let chave = |s: &str| format!("{prefixo}{s}");
    let camadas = numero(md, &chave("block_count"))? as usize;
    if camadas == 0 {
        bail!(tr("erros.alojamentoSemCamadas", &[]));
    }
    let cabecas = numero(md, &chave("attention.head_count"))? as u64;
    let cabecas_kv = numero(md, &chave("attention.head_count_kv"))?;
    let comprimento = numero(md, &chave("embedding_length"))?;
    // A dimensão da cabeça vem escrita quando o autor do arquivo a escreveu
    // (`attention.key_length`, que é como o llama.cpp grava a que não é
    // comprimento/cabeças); sem ela, a divisão é a própria definição.
    let dimensao = numero(md, &chave("attention.key_length"))
        .ok()
        .filter(|v| *v > 0)
        .unwrap_or_else(|| comprimento.checked_div(cabecas).unwrap_or(comprimento));
    let (camada, fixo) = por_camada(&cabecalho.tensor_infos, camadas);
    let pesos_total = camada.iter().sum::<u64>() + fixo;
    let (template, eos, paradas) = vocabulario(md);
    Ok(Ficha {
        arquivo: caminho.display().to_string(),
        arquitetura,
        camadas,
        pesos_total,
        camada,
        fixo,
        // 2 = K e V; 2 bytes = o f16 do flash attention.
        kv_por_token: 2 * camadas as u64 * cabecas_kv * dimensao * 2,
        janela_maxima: numero(md, &chave("context_length")).unwrap_or(0) as usize,
        template,
        eos,
        paradas,
    })
}

/// Onde os bytes vão ficar. A conta é a da biblioteca: ela põe na placa o que
/// cabe, começando das camadas de cima, e o que sobra fica do lado do sistema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "tipo"
)]
pub enum Alojamento {
    /// Cabe inteiro: pesos, KV da janela e folga dentro da VRAM livre medida.
    Inteira {
        /// Bytes de peso que vão para a placa.
        pesos: u64,
        kv: u64,
        livre: u64,
    },
    /// Não cabe: `camadas` de `total` vão para a placa e o resto fica em RAM. É
    /// o caso dos modelos grandes, e não é uma falha — é a conta da placa.
    Dividida {
        camadas: usize,
        total: usize,
        na_placa: u64,
        em_ram: u64,
        livre: u64,
    },
    /// Sem placa nesta máquina: tudo fica do lado do sistema.
    SoSistema { pesos: u64, kv: u64 },
}

/// O veredito para uma janela pedida. `janela` é o que o aplicativo vai pedir,
/// não o máximo do modelo: o KV é proporcional a ela, e é por isso que a mesma
/// placa pode caber um modelo em 4096 e não caber em 32768.
pub fn alojar(ficha: &Ficha, capacidade: Option<&Capacidade>, janela: usize) -> Alojamento {
    let Some(cap) = capacidade else {
        return Alojamento::SoSistema {
            pesos: ficha.pesos_total,
            kv: ficha.kv_por_token * janela as u64,
        };
    };
    let kv = ficha.kv_por_token * janela as u64;
    if ficha.pesos_total + kv + FOLGA_DE_COMPUTACAO <= cap.livre {
        return Alojamento::Inteira { pesos: ficha.pesos_total, kv, livre: cap.livre };
    }
    // O orçamento da placa, tirado o que ela é obrigada a guardar para si: o KV
    // da janela e a folga do grafo. As camadas entram de cima para baixo, que é
    // a ordem em que a biblioteca empilha quando falta memória.
    let orcamento = cap.livre.saturating_sub(kv + FOLGA_DE_COMPUTACAO);
    let mut usados = 0u64;
    let mut postas = 0usize;
    for bytes in ficha.camada.iter().rev() {
        if usados + bytes > orcamento {
            break;
        }
        usados += bytes;
        postas += 1;
    }
    Alojamento::Dividida {
        camadas: postas,
        total: ficha.camadas,
        na_placa: usados,
        em_ram: ficha.pesos_total - usados,
        livre: cap.livre,
    }
}

/// O veredito traduzido na moeda da carga: quantas camadas vão para a placa.
/// Negativo é o sentinela do llama.cpp para "todas", zero é "nenhuma". Esta
/// função existe porque prever sem obedecer é só uma tela bonita — o número que
/// ela devolve é o que a carga recebe, e é por ele que um modelo maior que a
/// placa roda com o topo na placa em vez de cair inteiro na RAM do sistema.
pub fn despacho(alojado: &Alojamento) -> i32 {
    match alojado {
        Alojamento::Inteira { .. } => -1,
        Alojamento::Dividida { camadas, .. } => *camadas as i32,
        Alojamento::SoSistema { .. } => 0,
    }
}

/// Bytes por camada, em média — o número que a tela usa para dizer de quanto em
/// quanto o modelo cresce quando a janela ou a quant mudam.
pub fn media_da_camada(ficha: &Ficha) -> u64 {
    if ficha.camadas == 0 {
        return 0;
    }
    ficha.camada.iter().sum::<u64>() / ficha.camadas as u64
}

#[cfg(test)]
mod testes {
    use super::*;

    /// Uma ficha montada à mão com a geometria do granite-4.2-3b Q4_K_M medida
    /// no arquivo real: 40 camadas, 8 cabeças de KV, 64 por cabeça, 2 GiB de
    /// pesos e 20 MiB por camada. Os testes não lêem disco — o que se verifica
    /// aqui é a decisão; a sonda `engine-probe alojar:` é o que confere a ficha
    /// contra a carga real.
    fn ficha_granite() -> Ficha {
        Ficha {
            arquivo: "granite-4.2-3b-Q4_K_M.gguf".to_string(),
            arquitetura: "granite".to_string(),
            camadas: 40,
            pesos_total: 2136 * 1024 * 1024,
            camada: vec![20 * 1024 * 1024; 40],
            fixo: 2136 * 1024 * 1024 - 40 * 20 * 1024 * 1024,
            kv_por_token: 2 * 40 * 8 * 64 * 2,
            janela_maxima: 131072,
            template: None,
            eos: vec![100257],
            paradas: vec!["<|im_end|>".to_string()],
        }
    }

    fn placa(livre_mib: u64) -> Option<Capacidade> {
        Some(Capacidade {
            placa: "AMD Radeon RX 9060 XT".to_string(),
            livre: livre_mib * 1024 * 1024,
            total: 16 * 1024 * 1024 * 1024,
        })
    }

    #[test]
    fn kv_de_quatro_mil_tokens_e_o_que_a_biblioteca_escreveu() {
        // A fórmula é a única parte que parece chute, então o pino é o número
        // que o llama.cpp imprime: `size = 320.00 MiB (4096 cells, 40 layers)`.
        let ficha = ficha_granite();
        let kv = ficha.kv_por_token * 4096;
        assert_eq!(kv, 335_544_320);
        assert_eq!(kv as f64 / 1048576.0, 320.0);
    }

    #[test]
    fn com_vram_de_sobra_a_resposta_e_inteira() {
        let ficha = ficha_granite();
        // 15 GiB livres: pesos + KV de 4096 + folga cabem com margem.
        let veredito = alojar(&ficha, placa(15 * 1024).as_ref(), 4096);
        match veredito {
            Alojamento::Inteira { pesos, kv, .. } => {
                assert_eq!(pesos, ficha.pesos_total);
                assert_eq!(kv, ficha.kv_por_token * 4096);
            }
            other => panic!("esperava inteira, veio {other:?}"),
        }
    }

    #[test]
    fn na_placa_apertada_a_janela_decide_sozinha() {
        // O modelo é o mesmo, a placa é a mesma, só a janela muda: ela é o que
        // come o orçamento antes das camadas. Uma placa de 15 GiB leva este 3B
        // inteira em qualquer janela que o arquivo declara — e é isso que a
        // conta diz, não o contrário.
        let ficha = ficha_granite();
        let apertada = placa(4 * 1024);
        assert!(matches!(
            alojar(&ficha, apertada.as_ref(), 4096),
            Alojamento::Inteira { .. }
        ));
        match alojar(&ficha, apertada.as_ref(), 65536) {
            Alojamento::Dividida { camadas, total, na_placa, em_ram, .. } => {
                assert_eq!(total, 40);
                // O KV de 65536 já passou da VRAM livre antes de qualquer camada.
                assert_eq!(camadas, 0);
                assert_eq!(na_placa, 0);
                assert_eq!(em_ram, ficha.pesos_total);
            }
            other => panic!("esperava dividida, veio {other:?}"),
        }
    }

    #[test]
    fn a_mesma_janela_muda_de_veredito_conforme_a_placa() {
        // É esta a pergunta do usuário: onde o modelo fica, dado o hardware de
        // agora. A placa grande leva, a pequena divide, e a janela é idêntica.
        let ficha = ficha_granite();
        let longa = 65536;
        assert!(matches!(
            alojar(&ficha, placa(16 * 1024).as_ref(), longa),
            Alojamento::Inteira { .. }
        ));
        match alojar(&ficha, placa(6 * 1024).as_ref(), longa) {
            Alojamento::Dividida { na_placa, em_ram, .. } => {
                assert!(na_placa > 0 && em_ram > 0);
                assert_eq!(na_placa + em_ram, ficha.pesos_total);
            }
            other => panic!("esperava dividida, veio {other:?}"),
        }
    }

    #[test]
    fn dividida_soma_os_dois_lados_e_poe_as_camadas_de_cima() {
        let ficha = ficha_granite();
        // Orçamento que comporta exatamente dez camadas depois do KV e da folga.
        let livre = ficha.kv_por_token * 4096
            + FOLGA_DE_COMPUTACAO
            + 10 * 20 * 1024 * 1024;
        match alojar(&ficha, Some(&Capacidade {
            placa: "AMD Radeon RX 9060 XT".to_string(),
            livre,
            total: 16 * 1024 * 1024 * 1024,
        }), 4096) {
            Alojamento::Dividida { camadas, na_placa, em_ram, total, .. } => {
                assert_eq!(camadas, 10);
                assert_eq!(total, 40);
                assert_eq!(na_placa, 10 * 20 * 1024 * 1024);
                assert_eq!(em_ram, ficha.pesos_total - na_placa);
            }
            other => panic!("esperava dividida, veio {other:?}"),
        }
    }

    #[test]
    fn o_veredito_e_a_ordem_de_carga() {
        // O que a conta diz e o que a placa recebe têm de ser o mesmo número:
        // pedir `-1` para um modelo que não cabe é o caminho que joga a conversa
        // inteira na RAM do sistema.
        let ficha = ficha_granite();
        assert_eq!(despacho(&alojar(&ficha, placa(15 * 1024).as_ref(), 4096)), -1);
        assert_eq!(despacho(&alojar(&ficha, None, 4096)), 0);
        // A placa de 4 GiB com janela de 65536 não leva camada nenhuma: o zero
        // aqui é instrução, não derrota — a biblioteca não vai tentar.
        assert_eq!(despacho(&alojar(&ficha, placa(4 * 1024).as_ref(), 65536)), 0);
        // E no meio da faixa, o despacho é exatamente a contagem do veredito.
        let dividida = alojar(&ficha, Some(&Capacidade {
            placa: "AMD Radeon RX 9060 XT".to_string(),
            livre: ficha.kv_por_token * 4096 + FOLGA_DE_COMPUTACAO + 10 * 20 * 1024 * 1024,
            total: 16 * 1024 * 1024 * 1024,
        }), 4096);
        assert_eq!(despacho(&dividida), 10);
    }

    #[test]
    fn sem_placa_a_resposta_e_sistema_e_sem_chute_de_vram() {
        let ficha = ficha_granite();
        match alojar(&ficha, None, 4096) {
            Alojamento::SoSistema { pesos, kv } => {
                assert_eq!(pesos, ficha.pesos_total);
                assert_eq!(kv, ficha.kv_por_token * 4096);
            }
            other => panic!("sem placa só existe o caso de sistema, veio {other:?}"),
        }
    }

    #[test]
    fn camada_se_le_do_nome_do_tensor() {
        assert_eq!(camada_do_nome("blk.7.ffn_up.weight", 40), Some(7));
        assert_eq!(camada_do_nome("blk.39.attn_q.weight", 40), Some(39));
        // Fora da geometria não é camada: é um nome que o parser do arquivo já
        // viu, e contá-lo como camada empurraria bytes para o lugar errado.
        assert_eq!(camada_do_nome("blk.40.attn_q.weight", 40), None);
        assert_eq!(camada_do_nome("token_embd.weight", 40), None);
        assert_eq!(camada_do_nome("output.weight", 40), None);
    }

    #[test]
    fn bytes_de_tensor_usam_o_bloco_do_dtype_e_arredondam_para_cima() {
        use candle::quantized::GgmlDType;
        let mk = |dtype: GgmlDType, dims: &[usize]| gguf_file::TensorInfo {
            ggml_dtype: dtype,
            shape: candle::Shape::from_dims(dims),
            offset: 0,
        };
        // Q4_K_M: 256 elementos por bloco de 144 bytes.
        let q4 = mk(GgmlDType::Q4K, &[4096usize, 4096]);
        assert_eq!(bytes_do_tensor(&q4), 4096 * 4096 / 256 * 144);
        // f32 é bloco de um: a conta degenera em elementos vezes o tamanho.
        let f32 = mk(GgmlDType::F32, &[10usize, 10]);
        assert_eq!(bytes_do_tensor(&f32), 400);
        // Um tensor que não fecha o bloco inteiro paga o bloco mesmo assim.
        let torto = mk(GgmlDType::Q4K, &[300usize, 1]);
        assert_eq!(bytes_do_tensor(&torto), 2 * 144);
    }

    #[test]
    fn por_camada_separa_o_que_e_da_placa_do_que_fica_fora() {
        use candle::quantized::GgmlDType;
        let mk = |dims: &[usize]| gguf_file::TensorInfo {
            ggml_dtype: GgmlDType::F32,
            shape: candle::Shape::from_dims(dims),
            offset: 0,
        };
        let mut tensores = HashMap::new();
        tensores.insert("blk.0.attn_q.weight".to_string(), mk(&[10usize, 10]));
        tensores.insert("blk.1.attn_q.weight".to_string(), mk(&[10usize, 10]));
        tensores.insert("token_embd.weight".to_string(), mk(&[4usize, 4]));
        let (camada, fixo) = por_camada(&tensores, 2);
        assert_eq!(camada, vec![400, 400]);
        assert_eq!(fixo, 64);
    }

    #[test]
    fn media_da_camada_e_o_passo_de_crescimento() {
        let ficha = ficha_granite();
        assert_eq!(media_da_camada(&ficha), 20 * 1024 * 1024);
        let vazio = Ficha { camada: vec![], camadas: 0, ..ficha };
        assert_eq!(media_da_camada(&vazio), 0);
    }

    /// O fio é o contrato com `types.ts`, e ele já foi quebrado uma vez: num
    /// enum, `rename_all` só troca os nomes das variantes — os campos pedem
    /// `rename_all_fields`. As três formas ficam pinadas aqui, byte a byte.
    #[test]
    fn as_tres_formas_do_veredito_sao_camelcase_no_wire() {
        let json = |a: &Alojamento| serde_json::to_string(a).unwrap();
        assert_eq!(
            json(&Alojamento::Inteira { pesos: 2240, kv: 335, livre: 15_360 }),
            r#"{"tipo":"inteira","pesos":2240,"kv":335,"livre":15360}"#
        );
        assert_eq!(
            json(&Alojamento::Dividida {
                camadas: 27,
                total: 40,
                na_placa: 3132,
                em_ram: 1967,
                livre: 4096,
            }),
            r#"{"tipo":"dividida","camadas":27,"total":40,"naPlaca":3132,"emRam":1967,"livre":4096}"#
        );
        assert_eq!(
            json(&Alojamento::SoSistema { pesos: 2240, kv: 335 }),
            r#"{"tipo":"soSistema","pesos":2240,"kv":335}"#
        );
    }

    /// Um GGUF mínimo escrito byte a byte, com os metadados que a ficha lê. O
    /// parser é o do candle, então o que o teste exercita é a nossa leitura de
    /// vocabulário — template, fins de resposta e os tokens que os nomeiam.
    fn gguf_de_vocabulario(caminho: &Path, com_eot: bool) {
        const STR: u32 = 8;
        const U32: u32 = 4;
        const ARR: u32 = 9;

        fn texto(s: &str) -> Vec<u8> {
            let mut v = (s.len() as u64).to_le_bytes().to_vec();
            v.extend_from_slice(s.as_bytes());
            v
        }
        fn par(chave: &str, tipo: u32, valor: Vec<u8>) -> Vec<u8> {
            let mut v = texto(chave);
            v.extend_from_slice(&tipo.to_le_bytes());
            v.extend_from_slice(&valor);
            v
        }
        fn u32_bytes(v: u32) -> Vec<u8> {
            v.to_le_bytes().to_vec()
        }
        fn array_de_textos(itens: &[&str]) -> Vec<u8> {
            let mut v = STR.to_le_bytes().to_vec();
            v.extend_from_slice(&(itens.len() as u64).to_le_bytes());
            for item in itens {
                v.extend_from_slice(&texto(item));
            }
            v
        }

        let mut corpo: Vec<u8> = Vec::new();
        for bytes in [
            par("general.architecture", STR, texto("qwen35")),
            par("qwen35.block_count", U32, u32_bytes(2)),
            par("qwen35.embedding_length", U32, u32_bytes(64)),
            par("qwen35.attention.head_count", U32, u32_bytes(4)),
            par("qwen35.attention.head_count_kv", U32, u32_bytes(1)),
            par("qwen35.attention.key_length", U32, u32_bytes(16)),
            par("qwen35.context_length", U32, u32_bytes(262_144)),
            par("tokenizer.chat_template", STR, texto("{{ messages }}")),
            par("tokenizer.ggml.tokens", ARR, array_de_textos(&["<unk>", "<bos>", "<|im_end|>", "<|im_start|>"])),
            par("tokenizer.ggml.eos_token_id", U32, u32_bytes(2)),
        ] {
            corpo.extend_from_slice(&bytes);
        }
        let pares = 10 + usize::from(com_eot);
        if com_eot {
            corpo.extend_from_slice(&par(
                "tokenizer.ggml.eot_token_id",
                U32,
                u32_bytes(3),
            ));
        }

        let mut arquivo: Vec<u8> = b"GGUF".to_vec();
        arquivo.extend_from_slice(&3u32.to_le_bytes());
        arquivo.extend_from_slice(&0u64.to_le_bytes());
        arquivo.extend_from_slice(&(pares as u64).to_le_bytes());
        arquivo.extend_from_slice(&corpo);
        std::fs::write(caminho, arquivo).unwrap();
    }

    #[test]
    fn a_ficha_le_template_eos_e_paradas_do_gguf() {
        let dir = std::env::temp_dir().join(format!("aether-aloj-vocab-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let caminho = dir.join("mini.gguf");

        gguf_de_vocabulario(&caminho, true);
        let ficha = fichar(&caminho).unwrap();
        assert_eq!(ficha.arquitetura, "qwen35");
        assert_eq!(ficha.janela_maxima, 262_144);
        assert_eq!(ficha.template.as_deref(), Some("{{ messages }}"));
        assert_eq!(ficha.eos, vec![2, 3]);
        // O que a geração observa como parada é o token escrito, lido do
        // vocabulário do arquivo — não um marcador copiado de outro modelo.
        assert_eq!(ficha.paradas, vec!["<|im_end|>", "<|im_start|>"]);

        // Sem o eot declarado fica só o eos: nada é inventado.
        gguf_de_vocabulario(&caminho, false);
        let ficha = fichar(&caminho).unwrap();
        assert_eq!(ficha.eos, vec![2]);
        assert_eq!(ficha.paradas, vec!["<|im_end|>"]);

        // Um arquivo sem template publicado não faz a ficha mentir sobre ele: o
        // chamador recua para o chat_template.jinja baixado.
        let vazio = dir.join("sem-template.gguf");
        std::fs::write(&vazio, b"GGUF").unwrap();
        assert!(fichar(&vazio).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
