//! Leitor de cabeçalho GGUF do Aether — o que o arquivo pesa, dito pelo arquivo.
//!
//! O aplicativo tinha um jeito de abrir um GGUF: o parser do candle, que a carga
//! de pesos em CPU usa e que `alojamento::fichar` usava para contar bytes. Ele é
//! o parser do candle, e conhece só os tipos que o candle sabe desempacotar —
//! `f32, f16, bf16, q4_0, q4_1, q5_0, q5_1, q8_0, q8_1, q2_k, q3_k, q4_k, q5_k,
//! q6_k, q8_k` (`GgmlDType::from_u32`, candle-core 0.11.1, `quantized/mod.rs:304`).
//! Essa lista decidia o que o catálogo podia oferecer: num arquivo `IQ2_XXS`, o
//! `Content::read` devolve `unknown dtype for tensor 16` antes de qualquer conta,
//! e sem cabeçalho lido não há veredito de alojamento — foi por isso que onze
//! quants IQ/MXFP4 do `Qwen3.5-35B-A3B` ficaram de fora
//! (`docs/melhorias/CATALOGO.md`, item `#109`).
//!
//! Este módulo lê o mesmo cabeçalho sem depender daquela lista. Contar bytes não
//! exige saber desempacotar nada: exige por tipo dois números — elementos por
//! bloco e bytes por bloco — e eles estão medidos, de duas maneiras
//! independentes, sobre 74 cabeçalhos lidos tensor a tensor por
//! `Range: bytes=0-16777215` no `resolve/main` do HuggingFace em 2026-11-24:
//!
//! 1. **Espaçamento vizinho a vizinho**: o intervalo entre dois tensores
//!    consecutivos é o tamanho do primeiro, arredondado para cima em múltiplos de
//!    `general.alignment` (32 nos 74). São 34.829 dessas vizinhanças nos 69
//!    arquivos da conta e nenhuma contradiz a linha do seu tipo; somando os 11
//!    arquivos que o candle recusava, 8.063 vizinhanças e 0 violações.
//! 2. **Soma contra o tamanho publicado**: os tensores mais o começo da região de
//!    dados dão o `size` do `tree/main` byte por byte — `diff 0` nos 69, inclusive
//!    nos 11 sem conta no candle.
//!
//! As duas provas fixam o **bytes por elemento**. A granularidade do bloco não é
//! observável pelo espaçamento (`32×17` e `256×136` dão o mesmo intervalo), e nos
//! 8.063 tensores IQ/MXFP4 medidos toda contagem de elementos é múltipla de 256,
//! então o arredondamento de bloco nunca entra em campo nos arquivos conferidos.
//! Os nomes são a correspondência com os bits por elemento de cada quant; o
//! número é o que o arquivo escreve. Onde um tensor não fecha bloco — caso que
//! nenhum arquivo medido apresenta — a conta arredonda para cima, como o ggml.
//!
//! Os limites estruturais (teto de string e de array, quatro dimensões, 64
//! níveis de aninhamento, `Bool` só 0 ou 1, contagens que não cabem nos bytes que
//! restam) são os do parser que o aplicativo usava — `gguf_file.rs` do candle-core
//! 0.11.1, linhas 16-19 e 469-560 — e estão aqui porque a defesa é do leitor, não
//! da biblioteca de carga. Onde este módulo é mais estrito que o candle é na
//! versão: GGUF v1 escreve comprimentos em 4 bytes, nenhum dos 74 cabeçalhos
//! medidos é v1, e o leitor recusa em vez de adivinhar o layout que não conferiu.
//!
//! Trocar de leitor não muda o veredito de nenhum arquivo que já tinha: para os
//! 13 tipos que os dois conhecem, a conta nova é a conta antiga byte a byte, e é
//! o que confere `a_conta_nova_bate_com_a_do_parser_antigo`, contra o
//! `block_size` e o `type_size` do próprio candle. Os
//! outros dez pesos desta tabela — os IQ e o MXFP4 — são tipos que aquele parser
//! não abria, e é por isso que onze quants continuaram fora do catálogo até aqui.

use crate::idiomas::tr;
use anyhow::Result;
use std::collections::HashMap;
use std::io::{Read, Seek};
use std::path::Path;

/// O alinhamento que o GGUF assume quando o arquivo não declara
/// `general.alignment` — e é o que os 74 cabeçalhos medidos usam.
pub const ALINHAMENTO_PADRAO: u64 = 32;

/// Tetos e ordem dos tipos de valor, copiados do parser que o aplicativo usava:
/// string e array até 2³⁰, quatro dimensões por tensor, 64 níveis de array
/// aninhado.
const TETO_DE_STRING: u64 = 1 << 30;
const TETO_DE_ARRAY: u64 = 1 << 30;
const TETO_DE_DIMENSOES: u32 = 4;
const TETO_DE_PROFUNDIDADE: usize = 64;

/// Um valor de metadado GGUF, pelo tipo que o arquivo escreveu. O `Bool` só
/// existe como 0 ou 1: um cabeçalho que escreve outro byte é recusado na leitura,
/// não tolerado.
#[derive(Debug, Clone, PartialEq)]
pub enum Valor {
    U8(u8),
    I8(i8),
    U16(u16),
    I16(i16),
    U32(u32),
    I32(i32),
    U64(u64),
    I64(i64),
    F32(f32),
    F64(f64),
    Bool(bool),
    String(String),
    Array(Vec<Valor>),
}

/// A conta de bytes de um tipo de tensor: quantos elementos cabem num bloco e
/// quantos bytes o bloco ocupa. O que a conta de pesos usa é o quociente, e é ele
/// que as duas medidas provam.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bloco {
    /// Nome do tipo, escrito como o censo do catálogo o escreve.
    pub nome: &'static str,
    pub elementos: u64,
    pub bytes: u64,
}

/// Os tipos com conta de bloco medida, pelo número que o cabeçalho escreve.
/// Faltam 4 e 5 (os `q4_2`/`q4_3` que saíram do ggml), 9 e 15 (`q8_1` e `q8_k`,
/// que o candle nomeia mas nenhum arquivo dos 74 tem) e 24 a 44: sem tensor
/// daquele número em arquivo lido, não há medida, e sem medida o arquivo é
/// recusado em vez de pesado por suposição.
const BLOCOS: &[(u32, Bloco)] = &[
    (0, Bloco { nome: "f32", elementos: 1, bytes: 4 }),
    (1, Bloco { nome: "f16", elementos: 1, bytes: 2 }),
    (2, Bloco { nome: "q4_0", elementos: 32, bytes: 18 }),
    (3, Bloco { nome: "q4_1", elementos: 32, bytes: 20 }),
    (6, Bloco { nome: "q5_0", elementos: 32, bytes: 22 }),
    (7, Bloco { nome: "q5_1", elementos: 32, bytes: 24 }),
    (8, Bloco { nome: "q8_0", elementos: 32, bytes: 34 }),
    (10, Bloco { nome: "q2_k", elementos: 256, bytes: 84 }),
    (11, Bloco { nome: "q3_k", elementos: 256, bytes: 110 }),
    (12, Bloco { nome: "q4_k", elementos: 256, bytes: 144 }),
    (13, Bloco { nome: "q5_k", elementos: 256, bytes: 176 }),
    (14, Bloco { nome: "q6_k", elementos: 256, bytes: 210 }),
    (16, Bloco { nome: "iq2_xxs", elementos: 256, bytes: 66 }),
    (17, Bloco { nome: "iq2_xs", elementos: 256, bytes: 74 }),
    (18, Bloco { nome: "iq3_xxs", elementos: 256, bytes: 98 }),
    (19, Bloco { nome: "iq1_s", elementos: 256, bytes: 50 }),
    (20, Bloco { nome: "iq4_nl", elementos: 32, bytes: 18 }),
    (21, Bloco { nome: "iq3_s", elementos: 256, bytes: 110 }),
    (22, Bloco { nome: "iq2_s", elementos: 256, bytes: 82 }),
    (23, Bloco { nome: "iq4_xs", elementos: 256, bytes: 136 }),
    (29, Bloco { nome: "iq1_m", elementos: 256, bytes: 56 }),
    (30, Bloco { nome: "bf16", elementos: 1, bytes: 2 }),
    (39, Bloco { nome: "mxfp4", elementos: 32, bytes: 17 }),
];

/// A linha de bloco de um tipo, ou `None` para o tipo que não foi medido.
pub fn bloco_de(tipo: u32) -> Option<Bloco> {
    BLOCOS.iter().find(|(n, _)| *n == tipo).map(|(_, b)| *b)
}

/// O número de um tipo pelo nome do censo. É assim que o catálogo confere, sem
/// lista paralela escrita à mão, que todo tipo que ele diz que um arquivo carrega
/// é um tipo que este leitor sabe pesar.
pub fn numero_do_nome(nome: &str) -> Option<u32> {
    BLOCOS.iter().find(|(_, b)| b.nome == nome).map(|(n, _)| *n)
}

/// Os nomes que este leitor pesa, na ordem dos números.
pub fn nomes_pesados() -> Vec<&'static str> {
    BLOCOS.iter().map(|(_, b)| b.nome).collect()
}

/// Um tensor como o cabeçalho o descreve. O `tipo` fica como o número cru que o
/// arquivo escreve: quem quer os bytes passa pela tabela medida, e um número fora
/// dela é recusa, não um zero na soma.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tensor {
    pub nome: String,
    /// As dimensões na ordem do arquivo. A ordem não importa para a conta, que é
    /// o produto delas.
    pub dimensoes: Vec<u64>,
    pub tipo: u32,
    /// Deslocamento dentro da região de dados, que começa em `inicio_dos_dados`.
    pub deslocamento: u64,
}

impl Tensor {
    /// Elementos do tensor, com produto saturante: um cabeçalho fabricado que
    /// embrulhasse o `u64` pareceria minúsculo, e saturar deixa o peso do lado do
    /// "não cabe" — a única direção em que um número errado não vira promessa.
    pub fn elementos(&self) -> u64 {
        self.dimensoes
            .iter()
            .fold(1u64, |acum, d| acum.saturating_mul(*d))
    }

    /// Bytes deste tensor na região de dados: `elementos / bloco * tamanho`,
    /// arredondando para cima como o ggml faz — um tensor que não fecha um bloco
    /// inteiro paga o bloco todo. `None` é o tipo sem conta medida.
    pub fn bytes(&self) -> Option<u64> {
        let bloco = bloco_de(self.tipo)?;
        Some(self.elementos().div_ceil(bloco.elementos).saturating_mul(bloco.bytes))
    }
}

/// O cabeçalho lido: os metadados, as fichas dos tensores e onde começam os
/// dados. Nenhum peso passa por aqui.
#[derive(Debug)]
pub struct Cabecalho {
    pub versao: u32,
    pub alinhamento: u64,
    /// Primeiro byte da região de dados: o fim das fichas arredondado para cima
    /// em múltiplos de `alinhamento`. É o `0` de todo `deslocamento` de tensor.
    pub inicio_dos_dados: u64,
    /// Os pares chave → valor do cabeçalho. Uma chave escrita duas vezes fica
    /// com a última leitura, que é o que o parser anterior também fazia.
    pub kv: HashMap<String, Valor>,
    pub tensores: Vec<Tensor>,
}

impl Cabecalho {
    /// O que o arquivo declara ser, `""` quando não declara `general.architecture`.
    pub fn arquitetura(&self) -> &str {
        match self.kv.get("general.architecture") {
            Some(Valor::String(v)) => v,
            _ => "",
        }
    }

    /// O primeiro tensor cujo tipo não tem conta medida, com o número do tipo. É
    /// a resposta honesta a um arquivo que o aplicativo sabe abrir mas não sabe
    /// pesar: quem chama recusa o arquivo nomeando o tensor, em vez de somar o
    /// que conhece e chamar o resto de zero.
    pub fn tipo_sem_conta(&self) -> Option<(&Tensor, u32)> {
        self.tensores
            .iter()
            .find(|t| bloco_de(t.tipo).is_none())
            .map(|t| (t, t.tipo))
    }

    /// Bytes de cada tensor, na ordem das fichas. O arquivo em que algum tipo não
    /// tem conta medida é recusado aqui, com o nome do tensor e o número escrito
    /// no cabeçalho: meio arquivo pesado e meio arquivo chamado de zero é a
    /// forma como um veredito mente "cabe".
    pub fn bytes_por_tensor(&self) -> Result<Vec<u64>> {
        self.tensores
            .iter()
            .map(|t| match t.bytes() {
                Some(bytes) => Ok(bytes),
                None => Err(erro(
                    "cabecalhoTipoSemConta",
                    &[("nome", &t.nome), ("tipo", &t.tipo.to_string())],
                )),
            })
            .collect()
    }

    /// Bytes de todos os tensores, ou `None` se algum tipo não tem conta medida.
    pub fn pesos(&self) -> Option<u64> {
        Some(
            self.bytes_por_tensor()
                .ok()?
                .iter()
                .fold(0u64, |total, bytes| total.saturating_add(*bytes)),
        )
    }
}

/// Abre o arquivo e lê só o cabeçalho. Ler o cabeçalho não é carregar pesos: o
/// parser para nas fichas dos tensores, nenhuma camada se move e a placa não é
/// tocada — é por isso que esta função roda em qualquer máquina.
pub fn ler(caminho: &Path) -> Result<Cabecalho> {
    let aberto = std::fs::File::open(caminho).map_err(|e| {
        anyhow::anyhow!(tr(
            "erros.falhaLer",
            &[("caminho", &caminho.display().to_string()), ("detalhe", &e.to_string())]
        ))
    })?;
    let tamanho = aberto.metadata()?.len();
    let mut leitor = std::io::BufReader::new(aberto);
    let cabecalho = ler_de(&mut leitor, tamanho).map_err(|e| {
        anyhow::anyhow!(tr(
            "erros.alojamentoGguf",
            &[("caminho", &caminho.display().to_string()), ("detalhe", &format!("{e:#}"))]
        ))
    })?;
    Ok(cabecalho)
}

/// O parser em si, separado do disco: é o que permite conferir byte a byte um
/// cabeçalho montado num teste, ou um `Range` baixado na mão, com o mesmo código
/// que roda no aplicativo.
pub fn ler_de<R: Read + Seek>(leitor: &mut R, tamanho: u64) -> Result<Cabecalho> {
    let magia = u32_le(leitor)?;
    // "GGUF" e sua reversão: os dois aparecem em arquivo real, e o parser
    // anterior aceitava os dois.
    if magia != 0x4655_4747 && magia != 0x4747_5546 {
        return Err(erro("cabecalhoMagia", &[("magic", &format!("{magia:#010x}"))]));
    }
    let versao = u32_le(leitor)?;
    if versao != 2 && versao != 3 {
        return Err(erro("cabecalhoVersao", &[("versao", &versao.to_string())]));
    }
    let tensores_declarados = u64_le(leitor)?;
    let kvs_declarados = u64_le(leitor)?;
    if tensores_declarados > TETO_DE_ARRAY {
        return Err(limite("tensores", tensores_declarados, TETO_DE_ARRAY));
    }
    if kvs_declarados > TETO_DE_ARRAY {
        return Err(limite("metadados", kvs_declarados, TETO_DE_ARRAY));
    }
    // Uma entrada não custa menos que prefixo de comprimento + tipo + um byte de
    // valor (kv) nem que comprimento + n_dims + tipo + deslocamento (ficha de
    // tensor). Sem esta conta, um cabeçalho com `n_tensors = 2⁴⁰` faria o leitor
    // reservar fichas que a memória não tem antes de chegar ao byte que prova a
    // mentira. Os prefixos são de 8 bytes porque v1 é recusado acima.
    let minimo_kv = 8 + 4 + 1;
    let minimo_tensor = 8 + 4 + 4 + 8;
    let necessario = kvs_declarados
        .saturating_mul(minimo_kv)
        .saturating_add(tensores_declarados.saturating_mul(minimo_tensor));
    let restante = tamanho.saturating_sub(leitor.stream_position()?);
    if necessario > restante {
        return Err(truncado("o cabeçalho", necessario, restante));
    }

    let mut kv = HashMap::new();
    for _ in 0..kvs_declarados {
        let chave = string_le(leitor, tamanho)?;
        let tipo = u32_le(leitor)?;
        let valor = valor_le(leitor, tipo, tamanho, 0)?;
        kv.insert(chave, valor);
    }

    let mut tensores: Vec<Tensor> = Vec::new();
    for _ in 0..tensores_declarados {
        let nome = string_le(leitor, tamanho)?;
        let n_dims = u32_le(leitor)?;
        if n_dims > TETO_DE_DIMENSOES {
            return Err(erro(
                "cabecalhoDimensoes",
                &[("nome", &nome), ("n", &n_dims.to_string()), ("max", &TETO_DE_DIMENSOES.to_string())],
            ));
        }
        if n_dims == 0 {
            return Err(erro("cabecalhoSemDimensoes", &[("nome", &nome)]));
        }
        let mut dimensoes = Vec::with_capacity(n_dims as usize);
        for _ in 0..n_dims {
            dimensoes.push(u64_le(leitor)?);
        }
        let tipo = u32_le(leitor)?;
        let deslocamento = u64_le(leitor)?;
        tensores.push(Tensor { nome, dimensoes, tipo, deslocamento });
    }

    let posicao = leitor.stream_position()?;
    let alinhamento = match kv.get("general.alignment") {
        Some(Valor::U8(v)) => *v as u64,
        Some(Valor::U16(v)) => *v as u64,
        Some(Valor::U32(v)) => *v as u64,
        Some(Valor::I8(v)) if *v >= 0 => *v as u64,
        Some(Valor::I16(v)) if *v >= 0 => *v as u64,
        Some(Valor::I32(v)) if *v >= 0 => *v as u64,
        _ => ALINHAMENTO_PADRAO,
    };
    // Um alinhamento 0 dividiria por zero logo abaixo, e um arquivo que escreve 0
    // não está declarando um múltiplo: está quebrado.
    if alinhamento == 0 {
        return Err(erro("cabecalhoAlinhamento", &[("valor", "0")]));
    }
    let inicio_dos_dados = posicao.div_ceil(alinhamento) * alinhamento;
    Ok(Cabecalho { versao, alinhamento, inicio_dos_dados, kv, tensores })
}

fn erro(chave: &str, args: &[(&str, &str)]) -> anyhow::Error {
    anyhow::anyhow!(tr(&format!("erros.{chave}"), args))
}

fn limite(coisa: &str, valor: u64, teto: u64) -> anyhow::Error {
    erro(
        "cabecalhoLimite",
        &[("coisa", coisa), ("valor", &valor.to_string()), ("teto", &teto.to_string())],
    )
}

fn truncado(coisa: &str, necessario: u64, restante: u64) -> anyhow::Error {
    erro(
        "cabecalhoTruncado",
        &[
            ("coisa", coisa),
            ("necessario", &necessario.to_string()),
            ("restante", &restante.to_string()),
        ],
    )
}

/// O mínimo de bytes que um valor daquele tipo ocupa, usado para conferir um
/// array declarado antes de alocar: string custa o prefixo, array custa tipo +
/// comprimento, e o resto é o próprio inteiro ou float.
fn minimo_de_valor(tipo: u32) -> u64 {
    match tipo {
        0 | 1 | 7 => 1,
        2 | 3 => 2,
        4 | 5 | 6 => 4,
        10 | 11 | 12 => 8,
        8 => 8,
        9 => 12,
        _ => 1,
    }
}

fn u8_le<R: Read>(leitor: &mut R) -> Result<u8> {
    let mut b = [0u8; 1];
    leitor.read_exact(&mut b)?;
    Ok(b[0])
}

fn i8_le<R: Read>(leitor: &mut R) -> Result<i8> {
    Ok(u8_le(leitor)? as i8)
}

fn u16_le<R: Read>(leitor: &mut R) -> Result<u16> {
    let mut b = [0u8; 2];
    leitor.read_exact(&mut b)?;
    Ok(u16::from_le_bytes(b))
}

fn i16_le<R: Read>(leitor: &mut R) -> Result<i16> {
    Ok(u16_le(leitor)? as i16)
}

fn u32_le<R: Read>(leitor: &mut R) -> Result<u32> {
    let mut b = [0u8; 4];
    leitor.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn i32_le<R: Read>(leitor: &mut R) -> Result<i32> {
    Ok(u32_le(leitor)? as i32)
}

fn u64_le<R: Read>(leitor: &mut R) -> Result<u64> {
    let mut b = [0u8; 8];
    leitor.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

fn i64_le<R: Read>(leitor: &mut R) -> Result<i64> {
    Ok(u64_le(leitor)? as i64)
}

fn f32_le<R: Read>(leitor: &mut R) -> Result<f32> {
    Ok(f32::from_bits(u32_le(leitor)?))
}

fn f64_le<R: Read>(leitor: &mut R) -> Result<f64> {
    Ok(f64::from_bits(u64_le(leitor)?))
}

/// Uma string do cabeçalho: comprimento de 8 bytes e bytes. Terminar em NUL
/// acontece em arquivo real, e é por isso que o `pop` está aqui e não em quem
/// chama. Um comprimento acima do teto, ou maior que os bytes que restam, é
/// recusa antes de qualquer alocação.
fn string_le<R: Read + Seek>(leitor: &mut R, tamanho: u64) -> Result<String> {
    let len = u64_le(leitor)?;
    if len > TETO_DE_STRING {
        return Err(limite("string", len, TETO_DE_STRING));
    }
    let restante = tamanho.saturating_sub(leitor.stream_position()?);
    if len > restante {
        return Err(truncado("a string do cabeçalho", len, restante));
    }
    let mut bytes = vec![0u8; len as usize];
    leitor.read_exact(&mut bytes)?;
    while bytes.last() == Some(&0) {
        bytes.pop();
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Um valor de metadado pelo tipo que o arquivo escreveu: 0 U8, 1 I8, 2 U16,
/// 3 I16, 4 U32, 5 I32, 6 F32, 7 Bool, 8 String, 9 Array, 10 U64, 11 I64,
/// 12 F64.
fn valor_le<R: Read + Seek>(
    leitor: &mut R,
    tipo: u32,
    tamanho: u64,
    profundidade: usize,
) -> Result<Valor> {
    if profundidade > TETO_DE_PROFUNDIDADE {
        return Err(erro("cabecalhoProfundidade", &[("max", &TETO_DE_PROFUNDIDADE.to_string())]));
    }
    Ok(match tipo {
        0 => Valor::U8(u8_le(leitor)?),
        1 => Valor::I8(i8_le(leitor)?),
        2 => Valor::U16(u16_le(leitor)?),
        3 => Valor::I16(i16_le(leitor)?),
        4 => Valor::U32(u32_le(leitor)?),
        5 => Valor::I32(i32_le(leitor)?),
        6 => Valor::F32(f32_le(leitor)?),
        7 => match u8_le(leitor)? {
            0 => Valor::Bool(false),
            1 => Valor::Bool(true),
            outro => return Err(erro("cabecalhoBool", &[("valor", &outro.to_string())])),
        },
        8 => Valor::String(string_le(leitor, tamanho)?),
        9 => {
            let tipo_item = u32_le(leitor)?;
            let len = u64_le(leitor)?;
            if len > TETO_DE_ARRAY {
                return Err(limite("array", len, TETO_DE_ARRAY));
            }
            let restante = tamanho.saturating_sub(leitor.stream_position()?);
            let necessario = len.saturating_mul(minimo_de_valor(tipo_item));
            if necessario > restante {
                return Err(truncado("o array do cabeçalho", necessario, restante));
            }
            let mut itens = Vec::new();
            for _ in 0..len {
                itens.push(valor_le(leitor, tipo_item, tamanho, profundidade + 1)?);
            }
            Valor::Array(itens)
        }
        10 => Valor::U64(u64_le(leitor)?),
        11 => Valor::I64(i64_le(leitor)?),
        12 => Valor::F64(f64_le(leitor)?),
        outro => return Err(erro("cabecalhoTipoValor", &[("tipo", &outro.to_string())])),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Um cabeçalho GGUF v3 montado byte a byte, com os kv e as fichas que o
    /// teste pedir — é o que permite conferir o parser, e a conta de pesos, sem
    /// nenhum `.gguf` versionado no repositório.
    struct Montador {
        kv: Vec<(String, u32, Vec<u8>)>,
        fichas: Vec<(String, Vec<u64>, u32, u64)>,
    }

    impl Montador {
        fn novo() -> Self {
            Self { kv: Vec::new(), fichas: Vec::new() }
        }

        fn kv_str(&mut self, chave: &str, valor: &str) -> &mut Self {
            let mut corpo = (valor.len() as u64).to_le_bytes().to_vec();
            corpo.extend_from_slice(valor.as_bytes());
            self.kv.push((chave.into(), 8, corpo));
            self
        }

        fn kv_num(&mut self, chave: &str, valor: u32) -> &mut Self {
            self.kv.push((chave.into(), 4, valor.to_le_bytes().to_vec()));
            self
        }

        fn ficha(&mut self, nome: &str, dimensoes: &[u64], tipo: u32, deslocamento: u64) -> &mut Self {
            self.fichas.push((nome.into(), dimensoes.to_vec(), tipo, deslocamento));
            self
        }

        /// Escreve o cabeçalho. `bytes_de_dados` é o tamanho da região de dados
        /// que o arquivo fingir ter — o parser não lê nada dela, mas os cheques
        /// de bytes restantes usam o tamanho total do buf.
        fn montar(&mut self, bytes_de_dados: u64) -> Vec<u8> {
            let mut out: Vec<u8> = Vec::new();
            out.extend_from_slice(b"GGUF");
            out.extend_from_slice(&3u32.to_le_bytes());
            out.extend_from_slice(&(self.fichas.len() as u64).to_le_bytes());
            out.extend_from_slice(&(self.kv.len() as u64).to_le_bytes());
            for (chave, tipo, corpo) in &self.kv {
                out.extend_from_slice(&(chave.len() as u64).to_le_bytes());
                out.extend_from_slice(chave.as_bytes());
                out.extend_from_slice(&tipo.to_le_bytes());
                out.extend_from_slice(corpo);
            }
            for (nome, dims, tipo, deslocamento) in &self.fichas {
                out.extend_from_slice(&(nome.len() as u64).to_le_bytes());
                out.extend_from_slice(nome.as_bytes());
                out.extend_from_slice(&(dims.len() as u32).to_le_bytes());
                for d in dims {
                    out.extend_from_slice(&d.to_le_bytes());
                }
                out.extend_from_slice(&tipo.to_le_bytes());
                out.extend_from_slice(&deslocamento.to_le_bytes());
            }
            let fim_das_fichas = out.len() as u64;
            let alinhamento = match self.kv.iter().find(|(c, _, _)| c == "general.alignment") {
                Some((_, _, corpo)) if corpo.len() == 4 => {
                    u32::from_le_bytes(corpo[..4].try_into().unwrap()) as u64
                }
                _ => ALINHAMENTO_PADRAO,
            };
            // O montador não divide por zero: um alinhamento 0 é justamente o
            // caso que o leitor recusa, e aqui ele só deixa o fim das fichas onde
            // está, sem arredondar.
            let inicio = if alinhamento == 0 {
                fim_das_fichas
            } else {
                fim_das_fichas.div_ceil(alinhamento) * alinhamento
            };
            out.resize(inicio as usize, 0);
            out.resize(inicio.saturating_add(bytes_de_dados) as usize, 0);
            out
        }
    }

    fn ler_buf(bytes: &[u8]) -> Result<Cabecalho> {
        let tamanho = bytes.len() as u64;
        let mut cursor = std::io::Cursor::new(bytes.to_vec());
        super::ler_de(&mut cursor, tamanho)
    }

    fn cabecalho_minimo() -> Montador {
        let mut m = Montador::novo();
        m.kv_str("general.architecture", "qwen35moe");
        m
    }

    #[test]
    fn a_tabela_de_blocos_e_a_medida_e_nao_uma_lista_de_desejos() {
        assert_eq!(BLOCOS.len(), 23);
        let bpw = |tipo: u32| {
            let b = bloco_de(tipo).unwrap();
            b.bytes as f64 * 8.0 / b.elementos as f64
        };
        // Os quatro tipos que o censo do A3B mostra e o candle recusava, em bits
        // por elemento — cada um é o que o espaçamento dos arquivos obriga.
        assert!((bpw(16) - 2.0625).abs() < 1e-9, "iq2_xxs");
        assert!((bpw(18) - 3.0625).abs() < 1e-9, "iq3_xxs");
        assert!((bpw(23) - 4.25).abs() < 1e-9, "iq4_xs");
        assert!((bpw(39) - 4.25).abs() < 1e-9, "mxfp4");
        // As duas linhas que a segunda rodada da soma corrigiu: `iq1_s` estava em
        // 52 por cópia de tabela alheia, e `q6_k` em 208. Nenhuma das duas é a que
        // os arquivos pagam.
        assert_eq!(bloco_de(19).unwrap().bytes, 50, "iq1_s");
        assert_eq!(bloco_de(14).unwrap().bytes, 210, "q6_k");
    }

    #[test]
    fn tipo_sem_arquivo_lido_nao_tem_conta() {
        for tipo in [4u32, 5, 9, 15, 24, 25, 44, 133, 139, 145, 154] {
            assert_eq!(bloco_de(tipo), None, "tipo {tipo} não deveria ter conta");
        }
        assert_eq!(numero_do_nome("q8_1"), None);
        assert_eq!(numero_do_nome("q8_k"), None, "o candle nomeia esses tipos; nenhum arquivo os tem");
    }

    #[test]
    fn cada_linha_da_tabela_pesa_o_que_o_espacamento_obrigava() {
        // Uma só prova interna, mas do tipo que pega erro de transcrição: o
        // bytes-por-elemento de cada linha, multiplicado por um bloco cheio, tem
        // de dar exatamente `bytes`.
        for (numero, bloco) in BLOCOS {
            let t = Tensor {
                nome: "t".into(),
                dimensoes: vec![bloco.elementos],
                tipo: *numero,
                deslocamento: 0,
            };
            assert_eq!(t.bytes(), Some(bloco.bytes), "tipo {numero} ({})", bloco.nome);
            let dois = Tensor { dimensoes: vec![bloco.elementos * 2], ..t.clone() };
            assert_eq!(dois.bytes(), Some(bloco.bytes * 2));
        }
    }

    #[test]
    fn um_tensor_que_nao_fecha_bloco_paga_o_bloco_inteiro() {
        let t = Tensor { nome: "t".into(), dimensoes: vec![300], tipo: 8, deslocamento: 0 };
        assert_eq!(t.bytes().unwrap(), 340, "10 blocos de 34 bytes: os 4 elementos fora de bloco pagam o bloco todo");
        let sem_conta = Tensor { nome: "t".into(), dimensoes: vec![300], tipo: 9, deslocamento: 0 };
        assert_eq!(sem_conta.bytes(), None);
    }

    #[test]
    fn as_dimensoes_que_embrulham_o_u64_saturam_e_nao_viram_zero() {
        let t = Tensor {
            nome: "t".into(),
            dimensoes: vec![u64::MAX; 4],
            tipo: 0,
            deslocamento: 0,
        };
        assert_eq!(t.elementos(), u64::MAX);
        assert_eq!(t.bytes().unwrap(), u64::MAX);
    }

    #[test]
    fn le_um_cabecalho_minimo_e_o_que_ele_diz() {
        let mut m = cabecalho_minimo();
        m.kv_num("general.block_count", 40).kv_num("tokenizer.ggml.eos_token_id", 151_645);
        let bytes = m.montar(0);
        let c = ler_buf(&bytes).unwrap();
        assert_eq!(c.versao, 3);
        assert_eq!(c.alinhamento, ALINHAMENTO_PADRAO);
        assert_eq!(c.arquitetura(), "qwen35moe");
        assert_eq!(c.kv.get("general.block_count"), Some(&Valor::U32(40)));
        assert_eq!(c.kv.get("tokenizer.ggml.eos_token_id"), Some(&Valor::U32(151_645)));
        assert_eq!(c.tensores.len(), 0);
        assert_eq!(c.pesos(), Some(0));
        assert_eq!(c.inicio_dos_dados % ALINHAMENTO_PADRAO, 0);
        assert!(c.inicio_dos_dados >= 24);
    }

    #[test]
    fn um_tensor_de_tipo_iq_entra_na_conta_do_cabecalho() {
        // Exatamente o arquivo que o parser antigo não abria: um tensor de tipo
        // 16 faz `Content::read` devolver `unknown dtype for tensor 16`. Aqui ele
        // entra na soma, e o cabeçalho inteiro é lido.
        let mut m = cabecalho_minimo();
        m.ficha("blk.0.ffn_gate_exps.weight", &[2048, 512, 256], 16, 0);
        let bytes = m.montar(69_206_016);
        let c = ler_buf(&bytes).unwrap();
        assert_eq!(c.tensores.len(), 1);
        assert_eq!(c.tensores[0].tipo, 16);
        assert_eq!(c.tensores[0].elementos(), 268_435_456);
        assert_eq!(c.pesos(), Some(69_206_016));
        assert_eq!(c.tipo_sem_conta(), None);
    }

    #[test]
    fn um_tensor_de_tipo_nao_medido_e_recusado_nomeando_o_tensor() {
        let mut m = cabecalho_minimo();
        m.ficha("blk.0.attn_q.weight", &[2048, 2048], 139, 0);
        m.ficha("blk.1.attn_q.weight", &[2048, 2048], 12, 0);
        let bytes = m.montar(1_000_000);
        let c = ler_buf(&bytes).unwrap();
        assert_eq!(c.pesos(), None, "um tipo sem conta não pode pesar menos que o arquivo");
        let (tensor, tipo) = c.tipo_sem_conta().unwrap();
        assert_eq!(tensor.nome, "blk.0.attn_q.weight");
        assert_eq!(tipo, 139);
        // O caminho que o aplicativo usa para decidir é `bytes_por_tensor`, e a
        // recusa dele tem de dizer o mesmo que os dois acima.
        let erro = c.bytes_por_tensor().unwrap_err().to_string();
        assert!(erro.contains("blk.0.attn_q.weight"), "a recusa nomeia o tensor: {erro}");
        assert!(erro.contains("139"), "a recusa diz o número lido: {erro}");
        // O outro arquivo — o que o candle recusava por tipo IQ — passa aqui.
        let mut iq = cabecalho_minimo();
        iq.ficha("blk.0.attn_q.weight", &[2048, 2048], 16, 0);
        let c = ler_buf(&iq.montar(0)).unwrap();
        assert_eq!(c.bytes_por_tensor().unwrap(), vec![4_194_304 / 256 * 66]);
    }

    #[test]
    fn alinhamento_declarado_muda_o_comeco_dos_dados() {
        // Não é enfeite: `inicio_dos_dados` é o `0` de todo deslocamento de
        // tensor, e um arquivo que declara 64 desloca os pesos todos.
        let mut m = cabecalho_minimo();
        m.kv_num("general.alignment", 64);
        m.ficha("t", &[8], 0, 0);
        let bytes = m.montar(0);
        let c = ler_buf(&bytes).unwrap();
        assert_eq!(c.alinhamento, 64);
        assert_eq!(c.inicio_dos_dados % 64, 0);
    }

    #[test]
    fn alinhamento_zero_e_recusado() {
        let mut m = cabecalho_minimo();
        m.kv_num("general.alignment", 0);
        let bytes = m.montar(0);
        let erro = ler_buf(&bytes).unwrap_err().to_string();
        assert!(erro.contains('0'), "a recusa devia dizer o valor lido: {erro}");
    }

    #[test]
    fn v1_e_recusado_porque_nenhum_cabecalho_medido_e_v1() {
        let mut bytes = b"GGUF".to_vec();
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&[0u8; 16]);
        let erro = ler_buf(&bytes).unwrap_err().to_string();
        assert!(erro.contains('1'), "a recusa devia dizer a versão: {erro}");
    }

    #[test]
    fn magia_errada_e_recusada_antes_de_qualquer_alocacao() {
        let bytes = b"NADA1234".to_vec();
        let erro = ler_buf(&bytes).unwrap_err().to_string();
        assert!(erro.contains("4e414441") || erro.contains("4144414e"), "magia na recusa: {erro}");
    }

    #[test]
    fn contagens_que_nao_cabem_no_arquivo_sao_recusadas() {
        let mut bytes = b"GGUF".to_vec();
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&(1u64 << 40).to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&[0u8; 24]);
        assert!(ler_buf(&bytes).is_err(), "2⁴⁰ tensores num buf de 40 bytes");

        let mut acima_do_teto = b"GGUF".to_vec();
        acima_do_teto.extend_from_slice(&3u32.to_le_bytes());
        acima_do_teto.extend_from_slice(&(1u64 << 31).to_le_bytes());
        acima_do_teto.extend_from_slice(&0u64.to_le_bytes());
        acima_do_teto.extend_from_slice(&[0u8; 24]);
        assert!(ler_buf(&acima_do_teto).is_err(), "tensores acima do teto de 2³⁰");
    }

    #[test]
    fn bool_fora_de_zero_e_um_e_recusado() {
        let mut m = Montador::novo();
        m.kv.push(("tokenizer.ggml.add_bos_token".into(), 7, vec![2u8]));
        let bytes = m.montar(0);
        let erro = ler_buf(&bytes).unwrap_err().to_string();
        assert!(erro.contains('2'), "a recusa devia dizer o byte lido: {erro}");
    }

    #[test]
    fn bool_zero_e_um_sao_lidos() {
        let mut m = cabecalho_minimo();
        m.kv.push(("a".into(), 7, vec![0u8]));
        m.kv.push(("b".into(), 7, vec![1u8]));
        let c = ler_buf(&m.montar(0)).unwrap();
        assert_eq!(c.kv.get("a"), Some(&Valor::Bool(false)));
        assert_eq!(c.kv.get("b"), Some(&Valor::Bool(true)));
    }

    #[test]
    fn cada_tipo_de_valor_e_lido_pela_sua_largura() {
        let corpos: [(u32, Vec<u8>); 12] = [
            (0, vec![7u8]),
            (1, vec![(-7i8) as u8]),
            (2, vec![1u8, 2]),
            (3, vec![(-1i16).to_le_bytes()[0], (-1i16).to_le_bytes()[1]]),
            (4, 40u32.to_le_bytes().to_vec()),
            (5, (-40i32).to_le_bytes().to_vec()),
            (6, 1.5f32.to_bits().to_le_bytes().to_vec()),
            (7, vec![1u8]),
            (10, 4000u64.to_le_bytes().to_vec()),
            (11, (-4000i64).to_le_bytes().to_vec()),
            (12, 2.5f64.to_bits().to_le_bytes().to_vec()),
            (8, {
                let mut v = (3u64).to_le_bytes().to_vec();
                v.extend_from_slice(b"abc");
                v
            }),
        ];
        let tipos: [u32; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 10, 11, 12, 8];
        let mut m = Montador::novo();
        for (i, t) in tipos.iter().enumerate() {
            m.kv.push((format!("k{i}"), *t, corpos[i].1.clone()));
        }
        let c = ler_buf(&m.montar(0)).unwrap();
        assert_eq!(c.kv.get("k0"), Some(&Valor::U8(7)));
        assert_eq!(c.kv.get("k1"), Some(&Valor::I8(-7)));
        assert_eq!(c.kv.get("k2"), Some(&Valor::U16(513)));
        assert_eq!(c.kv.get("k3"), Some(&Valor::I16(-1)));
        assert_eq!(c.kv.get("k4"), Some(&Valor::U32(40)));
        assert_eq!(c.kv.get("k5"), Some(&Valor::I32(-40)));
        assert_eq!(c.kv.get("k6"), Some(&Valor::F32(1.5)));
        assert_eq!(c.kv.get("k7"), Some(&Valor::Bool(true)));
        assert_eq!(c.kv.get("k8"), Some(&Valor::U64(4000)));
        assert_eq!(c.kv.get("k9"), Some(&Valor::I64(-4000)));
        assert_eq!(c.kv.get("k10"), Some(&Valor::F64(2.5)));
        assert_eq!(c.kv.get("k11"), Some(&Valor::String("abc".into())));
    }

    #[test]
    fn tipo_de_valor_desconhecido_e_recusado() {
        let mut m = Montador::novo();
        m.kv.push(("x".into(), 99, vec![0u8; 4]));
        assert!(ler_buf(&m.montar(0)).is_err());
    }

    #[test]
    fn array_de_um_milhao_de_itens_truncado_e_recusado() {
        // Declara 1.000.000 de U32 num cabeçalho de 60 bytes: o teto de
        // profundidade não seguraria isso; o cheque de bytes restantes segura, e
        // antes de qualquer `Vec` crescer.
        let mut corpo = Vec::new();
        corpo.extend_from_slice(&4u32.to_le_bytes());
        corpo.extend_from_slice(&1_000_000u64.to_le_bytes());
        corpo.extend_from_slice(&[0u8; 4]);
        let mut m = Montador::novo();
        m.kv.push(("x".into(), 9, corpo));
        assert!(ler_buf(&m.montar(0)).is_err(), "array truncado não pode virar alocação");
    }

    #[test]
    fn array_de_bools_e_lido_entrada_a_entrada() {
        // É a forma que o `sliding_window_pattern` do Gemma 4 usa, e o que a
        // conta de cache lê: 48 Bools.
        let mut corpo = Vec::new();
        corpo.extend_from_slice(&7u32.to_le_bytes());
        corpo.extend_from_slice(&48u64.to_le_bytes());
        for il in 0..48u8 {
            corpo.push(if il % 4 == 3 { 1 } else { 0 });
        }
        let mut m = Montador::novo();
        m.kv.push(("gemma4.attention.sliding_window_pattern".into(), 9, corpo));
        let c = ler_buf(&m.montar(0)).unwrap();
        match c.kv.get("gemma4.attention.sliding_window_pattern") {
            Some(Valor::Array(itens)) => {
                assert_eq!(itens.len(), 48);
                assert_eq!(itens[3], Valor::Bool(true));
                assert_eq!(itens[0], Valor::Bool(false));
            }
            outra => panic!("o array virou {outra:?}"),
        }
    }

    #[test]
    fn n_dims_acima_de_quatro_e_recusado() {
        let mut bytes = b"GGUF".to_vec();
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&1u64.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&1u64.to_le_bytes());
        bytes.extend_from_slice(b"t");
        bytes.extend_from_slice(&5u32.to_le_bytes());
        bytes.extend_from_slice(&[0u8; 8 * 5 + 4 + 8 + 64]);
        assert!(ler_buf(&bytes).is_err());
    }

    #[test]
    fn n_dims_zero_e_recusado() {
        let mut bytes = b"GGUF".to_vec();
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&1u64.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&1u64.to_le_bytes());
        bytes.extend_from_slice(b"t");
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&[0u8; 4 + 8 + 32]);
        assert!(ler_buf(&bytes).is_err(), "tensor sem dimensão não tem o que pesar");
    }

    #[test]
    fn string_que_tronca_no_fim_do_arquivo_e_recusada() {
        let mut bytes = b"GGUF".to_vec();
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&1u64.to_le_bytes());
        bytes.extend_from_slice(&1_000u64.to_le_bytes());
        bytes.extend_from_slice(b"a");
        assert!(ler_buf(&bytes).is_err());
    }

    #[test]
    fn uma_chave_escrita_duas_vezes_fica_com_a_ultima_leitura() {
        // O mesmo que o parser anterior fazia, escrito aqui para a troca não
        // mudar nada em silêncio.
        let mut m = cabecalho_minimo();
        m.kv_num("general.block_count", 12);
        m.kv_num("general.block_count", 40);
        let c = ler_buf(&m.montar(0)).unwrap();
        assert_eq!(c.kv.get("general.block_count"), Some(&Valor::U32(40)));
    }

    #[test]
    fn a_mesma_conta_de_um_tensor_de_dimensoes_zero_da_zero() {
        // `elementos()` de um tensor com uma dimensão 0 é 0, e a conta dá 0
        // bytes — o mesmo que o ggml faz com um tensor vazio. O teste existe para
        // que a decisão de não inventar peso fique escrita.
        let t = Tensor { nome: "t".into(), dimensoes: vec![0, 2048], tipo: 12, deslocamento: 0 };
        assert_eq!(t.bytes(), Some(0));
    }

    #[test]
    fn o_censo_de_um_arquivo_real_pesado_no_rust_dá_o_tamanho_publicado() {
        // O cabeçalho do `Qwen3.5-35B-A3B-UD-IQ2_XXS.gguf` tem 733 fichas, e o
        // instrumento de medição (`docs/melhorias/CATALOGO.md`, `#110`) deixou a
        // conta por tipo: para cada número, quantos tensores e quantos elementos
        // cada um tem. Reproduzida aqui tensor a tensor, a soma dos pesos mais o
        // começo da região de dados dá o tamanho publicado byte por byte — e o
        // número de 733 confere com o cabeçalho. O arquivo tem tipos que o
        // parser anterior recusava, então este é o teste que mostra a porta
        // aberta.
        let linhas: &[(u32, u64, u64)] = &[
            (0, 32, 60),
            (0, 128, 30),
            (0, 256, 20),
            (0, 2_048, 121),
            (0, 32_768, 30),
            (0, 524_288, 40),
            (8, 65_536, 60),
            (12, 508_559_360, 2),
            (13, 1_048_576, 100),
            (13, 8_388_608, 40),
            (13, 16_777_216, 40),
            (14, 1_048_576, 40),
            (14, 8_388_608, 30),
            (16, 268_435_456, 80),
            (22, 268_435_456, 40),
        ];
        let tensores: Vec<Tensor> = linhas
            .iter()
            .flat_map(|(tipo, elementos, vezes)| {
                (0..*vezes).map(move |_| Tensor {
                    nome: "t".into(),
                    dimensoes: vec![*elementos],
                    tipo: *tipo,
                    deslocamento: 0,
                })
            })
            .collect();
        assert_eq!(tensores.len(), 733, "o censo do arquivo tem 733 fichas");
        let inicio = 10_989_696u64;
        let c = Cabecalho {
            versao: 3,
            alinhamento: 32,
            inicio_dos_dados: inicio,
            kv: HashMap::new(),
            tensores,
        };
        let pesos = c.pesos().expect("todo tipo do censo tem conta");
        assert_eq!(pesos + inicio, 10_656_955_008, "byte a byte do publicado no tree/main");
    }

    #[test]
    fn a_revisao_de_tipos_sobre_arquivos_reais_fica_no_rust() {
        // O mesmo arquivo, lido pelo `montar` real do parser: as 733 fichas do
        // IQ2_XXS cabem num cabeçalho de ~11 MB e é o caminho que o aplicativo
        // percorre. Este teste monta uma ficha por linha da tabela — as 23 — e
        // confere que o parser devolve a conta que o censo publicado diz, pelo
        // caminho de leitura de verdade.
        let tipos: Vec<(u32, u64)> = [
            (0u32, 4),
            (1, 2),
            (2, 18),
            (3, 20),
            (6, 22),
            (7, 24),
            (8, 34),
            (10, 84),
            (11, 110),
            (12, 144),
            (13, 176),
            (14, 210),
            (16, 66),
            (17, 74),
            (18, 98),
            (19, 50),
            (20, 18),
            (21, 110),
            (22, 82),
            (23, 136),
            (29, 56),
            (30, 2),
            (39, 17),
        ]
        .to_vec();
        let mut m = cabecalho_minimo();
        for (numero, bloco_bytes) in &tipos {
            let elementos = bloco_de(*numero).unwrap().elementos;
            m.ficha(&format!("t{numero}"), &[elementos], *numero, 0);
            let t = Tensor {
                nome: format!("t{numero}"),
                dimensoes: vec![elementos],
                tipo: *numero,
                deslocamento: 0,
            };
            assert_eq!(t.bytes(), Some(*bloco_bytes), "linha medida do tipo {numero}");
        }
        let c = ler_buf(&m.montar(0)).unwrap();
        assert_eq!(c.tensores.len(), tipos.len());
        let esperado: u64 = tipos.iter().map(|(_, b)| b).sum();
        assert_eq!(c.pesos(), Some(esperado), "o parser lê as 23 fichas e a soma é a tabela");
    }

    #[test]
    fn os_tipos_do_censo_do_catalogo_tem_todos_conta_de_bloco() {
        // A pergunta que o teste do catálogo passou a fazer a este leitor, em vez
        // de a uma lista escrita à mão: cada tipo que o censo mede de um arquivo
        // oferecido tem de ter linha aqui. Entra um arquivo com um tipo nunca
        // medido e este teste fica vermelho na hora.
        for (arquivo, tipos) in crate::ai::catalog::censo_para_conferencia() {
            for tipo in tipos {
                assert!(
                    numero_do_nome(tipo).is_some(),
                    "{arquivo} carrega {tipo}, que não tem conta de bloco medida"
                );
            }
        }
    }

    #[test]
    fn nenhum_nome_da_tabela_se_repete() {
        let nomes = nomes_pesados();
        let mut vistos = std::collections::HashSet::new();
        for nome in &nomes {
            assert!(vistos.insert(nome), "dois tipos com o mesmo nome: {nome}");
        }
        assert_eq!(nomes.len(), BLOCOS.len());
    }

    #[test]
    fn toda_recusa_deste_leitor_tem_texto_em_todo_idioma() {
        // As chaves saem de `format!("erros.{chave}")`, e o teste de paridade dos
        // idiomas compara os arquivos entre si: uma chave ausente nos dois passa
        // nele e a tela mostra o caminho da chave como palavra. `tem_texto` é o
        // cheque que olha o catálogo, não a cópia.
        for chave in [
            "cabecalhoMagia",
            "cabecalhoVersao",
            "cabecalhoLimite",
            "cabecalhoTruncado",
            "cabecalhoDimensoes",
            "cabecalhoSemDimensoes",
            "cabecalhoAlinhamento",
            "cabecalhoProfundidade",
            "cabecalhoBool",
            "cabecalhoTipoValor",
            "cabecalhoTipoSemConta",
        ] {
            assert!(
                crate::idiomas::tem_texto(&format!("erros.{chave}")),
                "erros.{chave} não tem texto publicado em todo idioma"
            );
        }
    }

    /// Os tipos que o parser que o aplicativo usava sabia nomear: a lista de
    /// `GgmlDType::from_u32`, em candle-core 0.11.1 `quantized/mod.rs:304`,
    /// transcrita com o número que o cabeçalho escreve para cada um.
    fn dtype_do_parser_antigo(numero: u32) -> Option<candle::quantized::GgmlDType> {
        use candle::quantized::GgmlDType as D;
        Some(match numero {
            0 => D::F32,
            1 => D::F16,
            2 => D::Q4_0,
            3 => D::Q4_1,
            6 => D::Q5_0,
            7 => D::Q5_1,
            8 => D::Q8_0,
            9 => D::Q8_1,
            10 => D::Q2K,
            11 => D::Q3K,
            12 => D::Q4K,
            13 => D::Q5K,
            14 => D::Q6K,
            15 => D::Q8K,
            30 => D::BF16,
            _ => return None,
        })
    }

    /// A conta que o alojamento fazia com o parser antigo, transcrita para servir
    /// de régua. Não é o que roda no aplicativo.
    fn peso_pelo_parser_antigo(elementos: u64, numero: u32) -> Option<u64> {
        let dtype = dtype_do_parser_antigo(numero)?;
        let bloco = dtype.block_size() as u64;
        if bloco == 0 {
            return Some(0);
        }
        Some(elementos.div_ceil(bloco).saturating_mul(dtype.type_size() as u64))
    }

    #[test]
    fn a_conta_nova_bate_com_a_do_parser_antigo() {
        // A troca de leitor não pode mudar o peso de um arquivo que já tinha
        // veredito: a mesma forma, no mesmo tipo, dá o mesmo número — inclusive
        // quando os elementos não fecham um bloco inteiro.
        let formas = [1u64, 31, 32, 33, 255, 256, 257, 1024, 4096 * 4096];
        let mut conferidos = 0usize;
        for (numero, _) in BLOCOS {
            let Some(dtype) = dtype_do_parser_antigo(*numero) else { continue };
            let bloco = bloco_de(*numero).unwrap();
            assert_eq!(bloco.elementos, dtype.block_size() as u64, "elementos por bloco do tipo {numero}");
            assert_eq!(bloco.bytes, dtype.type_size() as u64, "bytes por bloco do tipo {numero}");
            for elementos in formas.iter() {
                let t = Tensor {
                    nome: "t".to_string(),
                    dimensoes: vec![*elementos],
                    tipo: *numero,
                    deslocamento: 0,
                };
                let antigo = peso_pelo_parser_antigo(*elementos, *numero).unwrap();
                assert_eq!(t.bytes(), Some(antigo), "tipo {numero} com {elementos} elementos");
                conferidos += 1;
            }
        }
        assert_eq!(conferidos, 13 * formas.len(), "são 13 os tipos que os dois leitores conhecem");

        // Os dez que só este leitor pesa: com o parser antigo esses cabeçalhos
        // morriam em "unknown dtype" antes de qualquer veredito.
        let so_deste: Vec<u32> = BLOCOS
            .iter()
            .map(|(n, _)| *n)
            .filter(|n| dtype_do_parser_antigo(*n).is_none())
            .collect();
        assert_eq!(
            so_deste,
            vec![16, 17, 18, 19, 20, 21, 22, 23, 29, 39],
            "os tipos IQ e MXFP4 que entraram com este leitor"
        );

        // E os dois que o parser antigo nomeava sem que nenhum arquivo dos 74 os
        // carregue: aqui continuam recusa, porque pesar por tabela emprestada é
        // prometer um número que ninguém mediu.
        for numero in [9u32, 15] {
            assert!(dtype_do_parser_antigo(numero).is_some(), "o parser antigo nomeava {numero}");
            assert!(bloco_de(numero).is_none(), "{numero} não tem bloco medido em arquivo algum");
            assert!(
                peso_pelo_parser_antigo(256, numero).unwrap() > 0,
                "{numero} pesaria por tabela, não por medida"
            );
        }
    }

    /// A mesma identidade que a medida remota provou sobre 74 cabeçalhos, agora
    /// com este leitor abrindo o disco: `inicio_dos_dados + Σ fichas == tamanho
    /// do arquivo`. Um `.part` baixado pela metade não fecha por definição, e é
    /// por isso que só entram os arquivos que têm o tamanho publicado inteiro — o
    /// veredito que se confere é o de arquivo completo.
    #[test]
    fn o_peso_lido_no_disco_bate_com_o_tamanho_do_arquivo() {
        let Some(local) = std::env::var("LOCALAPPDATA").ok() else {
            eprintln!("sem LOCALAPPDATA: nenhum GGUF do catálogo para conferir");
            return;
        };
        let raiz = std::path::PathBuf::from(local).join("ai.aether.core").join("models");
        let specs = crate::ai::catalog::todos_de_conversa()
            .chain(crate::ai::catalog::todos_de_embeddings())
            .collect::<Vec<_>>();
        let mut conferidos = 0usize;
        for spec in &specs {
            let caminho = crate::ai::catalog::model_dir(&raiz, spec).join(spec.weights_file);
            let Ok(metadata) = std::fs::metadata(&caminho) else { continue };
            if metadata.len() != spec.weights_bytes {
                continue;
            }
            let c = ler(&caminho).unwrap_or_else(|e| panic!("{}: {e}", caminho.display()));
            // Pela conta que nomeia o tensor: se o catálogo oferecesse um tipo sem
            // medida, o pânico diria qual ficha, não só que a soma saiu `None`.
            let soma: u64 = c
                .bytes_por_tensor()
                .unwrap_or_else(|e| panic!("{}: {e}", caminho.display()))
                .iter()
                .fold(0u64, |total, bytes| total.saturating_add(*bytes));
            assert_eq!(c.pesos(), Some(soma), "{}: `pesos` é a dobra da conta ficha a ficha", caminho.display());
            assert_eq!(
                c.inicio_dos_dados.saturating_add(soma),
                metadata.len(),
                "{}: as {} fichas não fecham o tamanho do arquivo",
                caminho.display(),
                c.tensores.len()
            );
            conferidos += 1;
        }
        // Quantos foram conferidos, dito em voz alta: um "ok" com zero arquivos
        // na mão não é evidência de nada.
        eprintln!(
            "{conferidos} de {} specs do catálogo têm GGUF inteiro em disco e fecharam a conta",
            specs.len()
        );
    }
}
