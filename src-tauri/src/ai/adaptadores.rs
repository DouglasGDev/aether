//! O ajuste fino do Aether: adaptadores LoRA aplicados ao modelo em memória.
//!
//! Nada aqui treina pesos — treinar custa dias de placa e este aplicativo não
//! faz isso. O que ele faz é a outra metade do conceito, a que dá para usar numa
//! máquina só: ler os deltas que um GGUF de adaptador declara, aplicá-los sobre
//! os pesos que já estão na placa e dar ao dono a força de cada um. É o mesmo
//! caminho que llama.cpp, LM Studio e os adaptadores publicados no HuggingFace
//! usam, e é por isso que os arquivos de lá funcionam aqui sem conversão.
//!
//! Duas regras do produto explicam o módulo existir separado do motor:
//! - nada é gerenciado por terceiro. Os adaptadores vivem numa pasta do
//!   aplicativo; um arquivo fora dela só entra se o dono apontar o caminho, e
//!   então ele passa a ser cópia administrada aqui;
//! - nenhum número é chutado. Nome, tipo, base e contagem de tensores saem do
//!   cabeçalho GGUF do próprio arquivo, lido pelo parser que o projeto já usa
//!   para os pesos, e o tamanho é o que o disco responde.

use crate::ai::ggml::Aplicado;
use crate::idiomas::tr;
use anyhow::{Context, Result, bail};
use candle::quantized::gguf_file;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Nome da pasta administrada dentro de `models`. Separada do LLM de conversa e
/// da memória de propósito: apagar um modelo não leva os adaptadores junto.
pub const PASTA: &str = "adaptadores";

const EXTENSAO: &str = "gguf";
/// A faixa que a biblioteca aceita e que a tela oferece. Negativo é o adaptador
/// invertido — existe quem treina assim, e o número é do dono da máquina.
const FORCA_MIN: f32 = -2.0;
const FORCA_MAX: f32 = 2.0;

/// Raiz dos adaptadores a partir da raiz de modelos.
pub fn raiz(models_root: &Path) -> PathBuf {
    models_root.join(PASTA)
}

/// O que o arquivo declara, tudo lido do cabeçalho GGUF dele.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Ficha {
    /// Caminho como o aplicativo o administra. É identidade de arquivo, não
    /// explicação: dois adaptadores chamados "roleplay" se distinguem por ele.
    pub caminho: String,
    /// O nome que o adaptador se deu (`adapter.name`), ou o nome do arquivo
    /// quando ele não se deu nenhum.
    pub nome: String,
    /// O que o adaptador é (`adapter.type`): `lora` é a imensa maioria, e o
    /// valor vem escrito de quem converteu, não de uma lista nossa.
    pub tipo: String,
    /// A base que o arquivo diz ter (`general.name`), vazia quando ele não diz.
    pub base: String,
    /// A arquitetura que o arquivo declara (`general.architecture`).
    pub arquitetura: String,
    /// Quantas deltas o arquivo traz. Um adaptador de zero tensores não muda
    /// nada, e é por este número que isso aparece em vez de virar mistério.
    pub tensores: usize,
    /// `true` quando o arquivo traz os vetores de magnitude do DoRA junto com
    /// os deltas: um tensor cujo nome termina no componente `mag`. É o que o
    /// arquivo declara, medido no cabeçalho dele. A biblioteca desta máquina não
    /// tem o que ler disso — os três literais com que uma implementação de DoRA
    /// nomeia esse tensor não ocorrem no `llama.dll` vendorizado, enquanto os
    /// símbolos dos pares de deltas estão (`crate::ai::ggml::LE_MAGNITUDE`, e o
    /// teste que lê o binário). Então o adaptador não falha: ele monta as deltas
    /// e deixa a normalização por magnitude de fora. O que este campo marca é o
    /// que vai ficar de fora, não uma capacidade desta máquina, e o caminho de
    /// carga o lê — é dele que nasce [`Item::magnitude_fora`].
    pub dora: bool,
    /// `true` quando o arquivo declara tokens de invocação
    /// (`adapter.alora.invocation_tokens`) — a mesma chave que a biblioteca lê
    /// na carga. Um adaptador desses só age nos trechos do prompt em que o token
    /// dele aparece: especialista localizado, em vez de carimbar a conversa toda.
    pub invocado: bool,
    /// Bytes do arquivo, medidos no disco.
    pub bytes: u64,
}

/// Uma escolha em `adaptadores.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pino {
    pub caminho: String,
    pub forca: f32,
    /// Desligado é diferente de removido: a força escolhida continua lá quando
    /// o dono ligar de novo.
    pub ativo: bool,
}

/// O que a tela edita, em `adaptadores.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Ajustes {
    pub pinos: Vec<Pino>,
}

impl Ajustes {
    pub fn de(caminho: &Path) -> Self {
        std::fs::read_to_string(caminho)
            .ok()
            .and_then(|texto| serde_json::from_str(&texto).ok())
            .map(|a: Self| a.aprovar())
            .unwrap_or_default()
    }

    /// Escrita atômica, como `geracao.json` e `rag.json`: ou o JSON inteiro está
    /// no disco, ou fica o anterior.
    pub fn salvar(&self, caminho: &Path) -> Result<()> {
        if let Some(pai) = caminho.parent() {
            std::fs::create_dir_all(pai).with_context(|| {
                tr("erros.falhaCriarPasta", &[("caminho", &pai.display().to_string())])
            })?;
        }
        let texto =
            serde_json::to_string_pretty(self).context(tr("erros.falhaSalvarConfig", &[]))?;
        let temporario = caminho.with_extension("json.tmp");
        std::fs::write(&temporario, &texto).with_context(|| {
            tr("erros.falhaEscreverConfig", &[("caminho", &temporario.display().to_string())])
        })?;
        std::fs::rename(&temporario, caminho)
            .with_context(|| tr("erros.falhaSubstituirConfig", &[("caminho", &caminho.display().to_string())]))?;
        Ok(())
    }

    /// O que vale depois da faixa. Sem caminho não há adaptador, e um arquivo
    /// pedido duas vezes dobraria o efeito dele sobre os pesos — o segundo é
    /// descartado, porque a última linha escrita é a última decisão do dono.
    pub fn aprovar(self) -> Self {
        let mut pinos: Vec<Pino> = Vec::with_capacity(self.pinos.len());
        for pino in self.pinos {
            let caminho = pino.caminho.trim();
            if caminho.is_empty() {
                continue;
            }
            let forca = if pino.forca.is_finite() {
                pino.forca.clamp(FORCA_MIN, FORCA_MAX)
            } else {
                1.0
            };
            let novo = Pino { caminho: caminho.to_string(), forca, ativo: pino.ativo };
            match pinos.iter_mut().find(|p| p.caminho == novo.caminho) {
                Some(velho) => *velho = novo,
                None => pinos.push(novo),
            }
        }
        pinos.sort_by(|a, b| a.caminho.cmp(&b.caminho));
        Self { pinos }
    }

    /// O que o motor deve aplicar agora: ligados, na ordem escolhida.
    pub fn pedidos(&self) -> Vec<(PathBuf, f32)> {
        self.pinos
            .iter()
            .filter(|p| p.ativo)
            .map(|p| (PathBuf::from(&p.caminho), p.forca))
            .collect()
    }

    /// O pin de um caminho, se houver.
    pub fn pino(&self, caminho: &Path) -> Option<&Pino> {
        let texto = caminho.display().to_string();
        self.pinos.iter().find(|p| p.caminho == texto)
    }

    /// Tira o pin de um caminho. Remover o arquivo sem tirar o pin deixaria o
    /// próximo turno pedindo um adaptador que não existe mais.
    pub fn sem_pino(&self, caminho: &Path) -> Self {
        let texto = caminho.display().to_string();
        Self { pinos: self.pinos.iter().filter(|p| p.caminho != texto).cloned().collect() }
    }
}

fn texto(md: &HashMap<String, gguf_file::Value>, chave: &str) -> String {
    match md.get(chave) {
        Some(gguf_file::Value::String(valor)) => valor.trim().to_string(),
        _ => String::new(),
    }
}

/// Lê o cabeçalho de um GGUF de adaptador. Os tensores não são lidos: um
/// adaptador de duzentos megabytes vira uma ficha do mesmo jeito que um de dez,
/// e é o cabeçalho que diz o que ele é.
pub fn ler_ficha(caminho: &Path) -> Result<Ficha> {
    let aberto = std::fs::File::open(caminho).map_err(|e| {
        anyhow::anyhow!(tr(
            "erros.falhaLer",
            &[("caminho", &caminho.display().to_string()), ("detalhe", &e.to_string())]
        ))
    })?;
    let mut leitor = std::io::BufReader::new(aberto);
    let cabecalho = gguf_file::Content::read(&mut leitor).map_err(|e| {
        anyhow::anyhow!(tr(
            "erros.adaptadorGgufInvalido",
            &[("caminho", &caminho.display().to_string()), ("detalhe", &e.to_string())]
        ))
    })?;
    // `adapter.type` é a chave que o contêiner GGUF reserva para dizer "isto é
    // um adaptador, e deste tipo". Sem ela o arquivo é um modelo inteiro, e
    // importá-lo como ajuste fino seria uma mentira sobre o que está rodando.
    let tipo = texto(&cabecalho.metadata, "adapter.type");
    if tipo.is_empty() {
        bail!(tr(
            "erros.adaptadorSemTipo",
            &[("caminho", &caminho.display().to_string())]
        ));
    }
    let nome_arquivo = caminho
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let declarado = texto(&cabecalho.metadata, "adapter.name");
    Ok(Ficha {
        caminho: caminho.display().to_string(),
        nome: if declarado.is_empty() {
            caminho
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| nome_arquivo.clone())
        } else {
            declarado
        },
        tipo,
        base: texto(&cabecalho.metadata, "general.name"),
        arquitetura: texto(&cabecalho.metadata, "general.architecture"),
        tensores: cabecalho.tensor_infos.len(),
        dora: cabecalho.tensor_infos.keys().any(|nome| magnitude_dora(nome)),
        invocado: cabecalho.metadata.contains_key("adapter.alora.invocation_tokens"),
        bytes: std::fs::metadata(caminho).map(|m| m.len()).unwrap_or(0),
    })
}

/// Se um nome de tensor é a magnitude de um adaptador DoRA. Não existe chave no
/// cabeçalho para dizer isso — o que declara o DoRA é o componente `mag` no nome
/// do tensor, escrito por quem converteu o arquivo.
fn magnitude_dora(nome: &str) -> bool {
    nome.split('.').next_back().is_some_and(|fim| fim.eq_ignore_ascii_case("mag"))
}

/// Os adaptadores da pasta, na ordem da tela. Um arquivo que não abre como
/// adaptador não aparece: o que está ali é o que o aplicativo sabe aplicar.
pub fn descobrir(pasta: &Path) -> Vec<Ficha> {
    let mut fichas: Vec<Ficha> = match std::fs::read_dir(pasta) {
        Ok(entradas) => entradas
            .flatten()
            .filter_map(|entrada| {
                let caminho = entrada.path();
                if !caminho.is_file()
                    || caminho.extension().and_then(|e| e.to_str()).map(|e| e.to_lowercase())
                        != Some(EXTENSAO.to_string())
                {
                    return None;
                }
                ler_ficha(&caminho).ok()
            })
            .collect(),
        Err(_) => return Vec::new(),
    };
    fichas.sort_by(|a, b| a.nome.cmp(&b.nome).then(a.caminho.cmp(&b.caminho)));
    fichas
}

/// Um nome que não colide com o que já está na pasta.
fn livre(pasta: &Path, stem: &str) -> PathBuf {
    let mut candidato = pasta.join(format!("{stem}.{EXTENSAO}"));
    for sufixo in 2.. {
        if !candidato.exists() {
            return candidato;
        }
        candidato = pasta.join(format!("{stem}-{sufixo}.{EXTENSAO}"));
    }
    unreachable!("um nome livre sempre existe");
}

/// Copia um adaptador para a pasta do aplicativo e devolve a ficha do arquivo
/// que ficou aqui. Importar é a única forma de um caminho de fora participar:
/// assim nada depende de uma pasta que o dono pode mover amanhã, e "apagar" tem
/// sempre um arquivo nosso para remover.
pub fn importar(pasta: &Path, origem: &Path) -> Result<Ficha> {
    if !origem.is_file() {
        bail!(tr("erros.adaptadorNaoExiste", &[("caminho", &origem.display().to_string())]));
    }
    // Ler a ficha de origem é o teste, não um passo sobrando: o que não é um
    // adaptador GGUF recusa aqui, antes de copiar duzentos megabytes para a
    // pasta do aplicativo.
    ler_ficha(origem)?;
    std::fs::create_dir_all(pasta).with_context(|| {
        tr("erros.falhaCriarPasta", &[("caminho", &pasta.display().to_string())])
    })?;
    let stem = origem
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "adaptador".to_string());
    let candidato = pasta.join(format!("{stem}.{EXTENSAO}"));
    // O mesmo arquivo importado duas vezes é a mesma coisa: mesmo nome, mesmo
    // tamanho. Duplicar duzentos megabytes por um clique repetido não é
    // importar, é encher o disco.
    if candidato.is_file() {
        let mesmo = std::fs::metadata(&candidato)
            .map(|m| m.len())
            .ok()
            .zip(std::fs::metadata(origem).map(|m| m.len()).ok())
            .map(|(a, b)| a == b)
            .unwrap_or(false);
        if mesmo {
            return ler_ficha(&candidato);
        }
    }
    let destino = livre(pasta, &stem);
    std::fs::copy(origem, &destino).with_context(|| {
        tr("erros.adaptadorNaoCopiou", &[("caminho", &destino.display().to_string())])
    })?;
    Ok(ler_ficha(&destino)
        .with_context(|| tr("erros.adaptadorNaoCopiou", &[("caminho", &destino.display().to_string())]))?)
}

/// Se um caminho é da pasta administrada. Fora dela o aplicativo não apaga
/// nada: o arquivo é do dono, não nosso.
pub fn dentro_da_pasta(pasta: &Path, caminho: &Path) -> bool {
    match (std::path::absolute(pasta), std::path::absolute(caminho)) {
        (Ok(a), Ok(b)) => b.starts_with(&a),
        _ => false,
    }
}

/// Apaga o arquivo de um adaptador da pasta do aplicativo.
pub fn apagar(pasta: &Path, caminho: &Path) -> Result<()> {
    if !dentro_da_pasta(pasta, caminho) {
        bail!(tr("erros.adaptadorForaDaPasta", &[("caminho", &caminho.display().to_string())]));
    }
    if !caminho.is_file() {
        return Ok(());
    }
    std::fs::remove_file(caminho).with_context(|| {
        tr("erros.adaptadorNaoApagou", &[("caminho", &caminho.display().to_string())])
    })?;
    Ok(())
}

/// A linha da tela: o arquivo, a escolha do dono e o que o motor fez com elas.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub ficha: Ficha,
    pub ativo: bool,
    pub forca: f32,
    /// `true` só quando os deltas daquele arquivo estão sobre os pesos agora.
    pub aplicada: bool,
    /// `true` quando aquilo que está sobre os pesos **não é o arquivo inteiro**:
    /// um DoRA aplicado cujas magnitudes ficaram de fora. Não é um erro do
    /// adaptador — as deltas abriram, entraram e funcionam — é a meia montagem
    /// que a biblioteca desta máquina sabe fazer. Ver [`magnitude_fora`].
    pub magnitude_fora: bool,
    /// O motivo dito pelo motor, quando ele não aplicou. Nunca um texto nosso:
    /// um adaptador que não abriu tem uma razão, e ela é a resposta.
    pub erro: Option<String>,
}

/// O que fica de fora da montagem de um adaptador que está sobre os pesos.
///
/// A decisão mora aqui, e não no motor, porque o motor não tem do que decidir:
/// a `llama.dll` vendorizada lê os pares de deltas e não lê tensor de magnitude
/// nenhum (`crate::ai::ggml::LE_MAGNITUDE`, conferida byte a byte no binário pelo
/// teste da frente ggml). Somar as deltas de um DoRA e calar a normalização é o
/// que acontece — antes deste campo, acontecia em silêncio, e a linha "Na placa"
/// era lida como "o modelo que o arquivo descreve está rodando".
///
/// Um adaptador desligado ou que não abriu não tem nada montado, então não tem
/// nada que tenha ficado de fora: `aplicada == false` responde `false`.
fn magnitude_fora(ficha: &Ficha, aplicada: bool) -> bool {
    aplicada && ficha.dora && !crate::ai::ggml::LE_MAGNITUDE
}

/// Junta disco, escolha e realidade. Os três são coisas diferentes: um
/// adaptador pode existir no disco, estar ligado na escolha e não estar
/// aplicado porque o modelo em memória é outro — e a tela tem de mostrar isso.
pub fn itens(pasta: &Path, ajustes: &Ajustes, aplicados: &[Aplicado]) -> Vec<Item> {
    let descobertos = descobrir(pasta);
    let aplicado_de = |caminho: &Path| aplicados.iter().find(|a| a.caminho == caminho);

    let mut itens: Vec<Item> = descobertos
        .iter()
        .map(|ficha| {
            let caminho = PathBuf::from(&ficha.caminho);
            let pino = ajustes.pino(&caminho);
            let aplicado = aplicado_de(&caminho);
            let aplicada = aplicado.map(|a| a.aplicada).unwrap_or(false);
            Item {
                ficha: ficha.clone(),
                ativo: pino.map(|p| p.ativo).unwrap_or(false),
                forca: pino.map(|p| p.forca).unwrap_or(1.0),
                aplicada,
                magnitude_fora: magnitude_fora(ficha, aplicada),
                erro: aplicado.and_then(|a| a.erro.clone()),
            }
        })
        .collect();

    // Um pin apontando para um arquivo que não está mais ali continua na tela,
    // com o motivo dito por quem tentou usá-lo: é o dono quem decide se religa
    // ou se esquece, e esconder a linha esconderia o problema.
    for pino in &ajustes.pinos {
        let caminho = PathBuf::from(&pino.caminho);
        if descobertos.iter().any(|f| f.caminho == pino.caminho) {
            continue;
        }
        let aplicado = aplicado_de(&caminho);
        let aplicada = aplicado.map(|a| a.aplicada).unwrap_or(false);
        let erro = match aplicado {
            Some(a) => a.erro.clone(),
            // Sem resposta do motor é porque ele nem chegou a pedir o arquivo:
            // a verdade disponível é a de que ele não está onde foi escolhido.
            None if pino.ativo => {
                Some(tr("erros.adaptadorNaoExiste", &[("caminho", &pino.caminho)]))
            }
            None => None,
        };
        // A ficha possível de um arquivo que não está mais ali: sem cabeçalho
        // lido não há declaração de magnitude, e `magnitude_fora` responde
        // `false` pelos dois motivos — nada montado e nada declarado.
        let ficha = Ficha {
            caminho: pino.caminho.clone(),
            nome: caminho
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| pino.caminho.clone()),
            tipo: String::new(),
            base: String::new(),
            arquitetura: String::new(),
            tensores: 0,
            dora: false,
            invocado: false,
            bytes: 0,
        };
        itens.push(Item {
            magnitude_fora: magnitude_fora(&ficha, aplicada),
            aplicada,
            ficha,
            ativo: pino.ativo,
            forca: pino.forca,
            erro,
        });
    }
    itens.sort_by(|a, b| a.ficha.nome.cmp(&b.ficha.nome).then(a.ficha.caminho.cmp(&b.ficha.caminho)));
    itens
}

/// O que a tela de ajuste fino pergunta de uma vez.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Estado {
    /// A pasta administrada — identidade, não parágrafo.
    pub pasta: String,
    /// O modelo que está em memória agora, e é sobre ele que os adaptadores
    /// agem. Sem modelo, escolha nenhuma tem efeito ainda.
    pub modelo: Option<String>,
    /// `true` quando o motor em memória é o da placa, o único que aplica.
    pub na_placa: bool,
    pub itens: Vec<Item>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pino(caminho: &str, forca: f32, ativo: bool) -> Pino {
        Pino { caminho: caminho.to_string(), forca, ativo }
    }

    /// Um cabeçalho GGUF v3 de verdade, escrito byte a byte: mágica, versão,
    /// contagem de tensores, contagem de chaves, os pares chave/valor e as
    /// fichas dos tensores. O parser confere as contagens contra o tamanho do
    /// arquivo, então um cabeçalho que mente sobre si mesmo não é um teste.
    fn gguf(chaves: &[(&str, &str)], tensores: u64) -> Vec<u8> {
        let nomes: Vec<String> =
            (0..tensores).map(|indice| format!("tensores.{indice}.delta")).collect();
        gguf_com_nomes(chaves, &nomes)
    }

    /// O mesmo cabeçalho, com os nomes dos tensores à escolha: é o nome que
    /// declara o DoRA, e um teste que não pode escrevê-lo não prova nada.
    fn gguf_com_nomes(chaves: &[(&str, &str)], nomes: &[String]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"GGUF");
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&(nomes.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&(chaves.len() as u64).to_le_bytes());
        for (chave, valor) in chaves {
            bytes.extend_from_slice(&(chave.len() as u64).to_le_bytes());
            bytes.extend_from_slice(chave.as_bytes());
            bytes.extend_from_slice(&8u32.to_le_bytes()); // ValueType::String
            bytes.extend_from_slice(&(valor.len() as u64).to_le_bytes());
            bytes.extend_from_slice(valor.as_bytes());
        }
        for (indice, nome) in nomes.iter().enumerate() {
            bytes.extend_from_slice(&(nome.len() as u64).to_le_bytes());
            bytes.extend_from_slice(nome.as_bytes());
            bytes.extend_from_slice(&2u32.to_le_bytes()); // duas dimensões
            bytes.extend_from_slice(&32u64.to_le_bytes());
            bytes.extend_from_slice(&32u64.to_le_bytes());
            bytes.extend_from_slice(&0u32.to_le_bytes()); // GgmlDType::F32
            bytes.extend_from_slice(&(indice * 4096).to_le_bytes());
        }
        bytes
    }

    fn escrever(pasta: &Path, nome: &str, bytes: &[u8]) -> PathBuf {
        let caminho = pasta.join(nome);
        std::fs::write(&caminho, bytes).unwrap();
        caminho
    }

    fn pasta_de_teste(rotulo: &str) -> PathBuf {
        let pasta = std::env::temp_dir().join(format!("aether-adaptadores-{rotulo}"));
        std::fs::create_dir_all(&pasta).unwrap();
        pasta
    }

    #[test]
    fn aprovar_limita_forca_descarta_vazios_e_duplicados() {
        let aceito = Ajustes {
            pinos: vec![
                pino("  a.gguf ", 9.0, true),
                pino("", 1.0, true),
                pino("b.gguf", f32::NAN, true),
                pino("a.gguf", 0.25, false),
            ],
        }
        .aprovar();
        // O último "a" vence, e o caminho chegou aparado.
        assert_eq!(aceito.pinos.len(), 2);
        assert_eq!(aceito.pinos[0].caminho, "a.gguf");
        assert_eq!(aceito.pinos[0].forca, 0.25);
        assert!(!aceito.pinos[0].ativo);
        // NAN não é força: vira a escolha neutra, não um número sem sentido.
        assert_eq!(aceito.pinos[1].caminho, "b.gguf");
        assert_eq!(aceito.pinos[1].forca, 1.0);
        assert_eq!(aceito.pedidos(), vec![(PathBuf::from("b.gguf"), 1.0)]);
    }

    #[test]
    fn sem_forca_finita_a_faixa_e_a_regra_que_cheia_o_cursor() {
        let aceito = Ajustes { pinos: vec![pino("a.gguf", -7.0, true)] }.aprovar();
        assert_eq!(aceito.pinos[0].forca, FORCA_MIN);
    }

    #[test]
    fn ficha_le_o_que_o_arquivo_declara() {
        let pasta = pasta_de_teste("ficha");
        let caminho = escrever(
            &pasta,
            "papel.gguf",
            &gguf(
                &[
                    ("adapter.type", "lora"),
                    ("adapter.name", "Roleplay forte"),
                    ("general.name", "granite-4.2-3b"),
                    ("general.architecture", "llama"),
                ],
                128,
            ),
        );
        let ficha = ler_ficha(&caminho).unwrap();
        assert_eq!(ficha.nome, "Roleplay forte");
        assert_eq!(ficha.tipo, "lora");
        assert_eq!(ficha.base, "granite-4.2-3b");
        assert_eq!(ficha.arquitetura, "llama");
        assert_eq!(ficha.tensores, 128);
        assert_eq!(ficha.bytes, std::fs::metadata(&caminho).unwrap().len());

        // Sem nome declarado, o arquivo é o nome — não uma string inventada.
        let anonimo = escrever(&pasta, "anônimo.gguf", &gguf(&[("adapter.type", "lora")], 0));
        let ficha = ler_ficha(&anonimo).unwrap();
        assert_eq!(ficha.nome, "anônimo");
        assert_eq!(ficha.tensores, 0);
        // Um LoRA comum não é DoRA nem especialista: as duas etiquetas só vem
        // do que o arquivo escreve, e aqui ele não escreveu nada.
        assert!(!ficha.dora);
        assert!(!ficha.invocado);
        std::fs::remove_dir_all(&pasta).ok();
    }

    #[test]
    fn a_ficha_ve_magnitude_e_invocacao_onde_o_arquivo_escreve_elas() {
        let pasta = pasta_de_teste("dora-alora");
        let caminho = escrever(
            &pasta,
            "especialista.gguf",
            &gguf_com_nomes(
                &[("adapter.type", "lora"), ("adapter.alora.invocation_tokens", "1,2,3")],
                &[
                    "blk.0.attn_q.weight.a".to_string(),
                    "blk.0.attn_q.weight.b".to_string(),
                    "blk.0.attn_q.weight.mag".to_string(),
                ],
            ),
        );
        let ficha = ler_ficha(&caminho).unwrap();
        assert!(ficha.dora, "{ficha:?}");
        assert!(ficha.invocado, "{ficha:?}");
        // As magnitudes são tensores do arquivo como os outros: entram na
        // contagem que a tela mostra, não somem da ficha.
        assert_eq!(ficha.tensores, 3);
        std::fs::remove_dir_all(&pasta).ok();
    }

    #[test]
    fn magnitude_e_componente_do_nome_e_nao_o_final_da_string() {
        // O que declara o DoRA é o componente do nome, não o final da string:
        // um tensor chamado "imagem" termina em "mag" e não é magnitude de nada.
        assert!(magnitude_dora("blk.0.attn_q.mag"));
        assert!(magnitude_dora("token_embd.mag"));
        assert!(!magnitude_dora("blk.0.attn_q.imagem"));
        assert!(!magnitude_dora("adapter.mag_down"));
    }

    #[test]
    fn um_gguf_qualquer_nao_e_um_adaptador() {
        let pasta = pasta_de_teste("sem-tipo");
        let caminho = escrever(
            &pasta,
            "modelo.gguf",
            &gguf(&[("general.architecture", "llama"), ("general.name", "Granite")], 4),
        );
        let erro = ler_ficha(&caminho).unwrap_err().to_string();
        assert!(erro.contains(&caminho.display().to_string()), "{erro}");
        // Descobrir não mostra o que não sabe aplicar.
        assert!(descobrir(&pasta).is_empty());
        std::fs::remove_dir_all(&pasta).ok();
    }

    #[test]
    fn descobrir_so_ve_gguf_e_ordena_pelo_nome() {
        let pasta = pasta_de_teste("descobrir");
        escrever(&pasta, "zzz.gguf", &gguf(&[("adapter.type", "lora"), ("adapter.name", "Zeta")], 1));
        escrever(&pasta, "aaa.gguf", &gguf(&[("adapter.type", "lora"), ("adapter.name", "Alpha")], 1));
        escrever(&pasta, "notas.txt", b"nada");
        escrever(&pasta, "sem-extensao", b"nada");
        let nomes: Vec<String> = descobrir(&pasta).into_iter().map(|f| f.nome).collect();
        assert_eq!(nomes, vec!["Alpha".to_string(), "Zeta".to_string()]);
        std::fs::remove_dir_all(&pasta).ok();
    }

    #[test]
    fn importar_copia_para_a_pasta_e_repetir_o_mesmo_arquivo_nao_duplica() {
        let origem_pasta = pasta_de_teste("importar-origem");
        let pasta = pasta_de_teste("importar-destino");
        let bytes = gguf(&[("adapter.type", "lora"), ("adapter.name", "Importado")], 16);
        let origem = escrever(&origem_pasta, "importado.gguf", &bytes);

        let ficha = importar(&pasta, &origem).unwrap();
        assert!(dentro_da_pasta(&pasta, &PathBuf::from(&ficha.caminho)));
        assert_eq!(ficha.nome, "Importado");
        assert_eq!(ficha.tensores, 16);

        let de_novo = importar(&pasta, &origem).unwrap();
        assert_eq!(de_novo.caminho, ficha.caminho);
        assert_eq!(descobrir(&pasta).len(), 1);

        // Mesmo nome, conteúdo diferente: ganha sufixo, os dois ficam.
        let caminho = {
            let mut b = bytes.clone();
            b.extend_from_slice(b"excedente");
            escrever(&origem_pasta, "importado.gguf", &b)
        };
        let terceiro = importar(&pasta, &caminho).unwrap();
        assert_ne!(terceiro.caminho, ficha.caminho);
        assert!(terceiro.caminho.ends_with("importado-2.gguf"), "{}", terceiro.caminho);

        // Nome diferente: entra como arquivo próprio, sem sufixo nenhum.
        let outro = escrever(
            &origem_pasta,
            "outro.gguf",
            &gguf(&[("adapter.type", "lora"), ("adapter.name", "Outro")], 32),
        );
        let quarto = importar(&pasta, &outro).unwrap();
        assert!(quarto.caminho.ends_with("outro.gguf"), "{}", quarto.caminho);
        assert_eq!(descobrir(&pasta).len(), 3);
        std::fs::remove_dir_all(&pasta).ok();
        std::fs::remove_dir_all(&origem_pasta).ok();
    }

    #[test]
    fn importar_recusa_o_que_nao_e_adaptador_e_apagar_recusa_o_que_nao_e_nosso() {
        let pasta = pasta_de_teste("recusas");
        let destino = pasta.join("x.gguf");
        std::fs::write(&destino, gguf(&[("adapter.type", "lora")], 1)).unwrap();
        let fora = std::env::temp_dir().join("aether-adaptadores-fora.gguf");
        std::fs::write(&fora, gguf(&[("adapter.type", "lora")], 1)).unwrap();

        assert!(importar(&pasta, &pasta.join("nao-existe.gguf")).is_err());
        let nao_adaptador = pasta_de_teste("recusas-origem");
        let lixo = escrever(&nao_adaptador, "lixo.gguf", &gguf(&[("general.architecture", "llama")], 1));
        assert!(importar(&pasta, &lixo).is_err());

        let erro = apagar(&pasta, &fora).unwrap_err().to_string();
        assert!(erro.contains(&fora.display().to_string()), "{erro}");
        assert!(fora.is_file());
        // O que é da pasta sai.
        apagar(&pasta, &destino).unwrap();
        assert!(!destino.exists());
        std::fs::remove_file(&fora).ok();
        std::fs::remove_dir_all(&pasta).ok();
        std::fs::remove_dir_all(&nao_adaptador).ok();
    }

    #[test]
    fn item_diz_arquivo_escolha_e_realidade_separadas() {
        let pasta = pasta_de_teste("itens");
        let caminho = escrever(
            &pasta,
            "papel.gguf",
            &gguf(&[("adapter.type", "lora"), ("adapter.name", "Papel")], 8),
        );
        let texto = caminho.display().to_string();
        let ajustes =
            Ajustes { pinos: vec![pino(&texto, 0.5, true), pino("somente-pin.gguf", 1.0, true)] }
                .aprovar();
        let aplicados = vec![
            Aplicado { caminho: caminho.clone(), aplicada: true, erro: None },
            Aplicado {
                caminho: PathBuf::from("somente-pin.gguf"),
                aplicada: false,
                erro: Some("não abriu".to_string()),
            },
        ];
        let itens = itens(&pasta, &ajustes, &aplicados);
        assert_eq!(itens.len(), 2, "{:?}", itens.iter().map(|i| &i.ficha.caminho).collect::<Vec<_>>());
        let papel = itens.iter().find(|i| i.ficha.nome == "Papel").unwrap();
        assert!(papel.ativo);
        assert_eq!(papel.forca, 0.5);
        assert!(papel.aplicada);
        assert_eq!(papel.erro, None);
        // O pin sem arquivo não some da tela, e o motor é quem responde por ele.
        let orfao = itens.iter().find(|i| i.ficha.caminho == "somente-pin.gguf").unwrap();
        assert!(!orfao.aplicada);
        assert!(!orfao.magnitude_fora, "sem arquivo lido não há magnitude declarada");
        assert_eq!(orfao.erro.as_deref(), Some("não abriu"));
        std::fs::remove_dir_all(&pasta).ok();
    }

    /// A linha da frente #34. Antes deste caso, um DoRA montado era indistinguível
    /// de um LoRA comum no estado: as deltas entravam, a normalização por magnitude
    /// não tinha quem a fizesse, e a tela dizia "Na placa" para um modelo que não é
    /// o que o arquivo descreve.
    #[test]
    fn um_dora_aplicado_registra_que_a_magnitude_ficou_fora() {
        let pasta = pasta_de_teste("dora-montagem");
        let nomes = |sufixo: &str| {
            ["blk.0.attn_q.weight", "blk.0.attn_v.weight"]
                .iter()
                .map(|n| format!("{n}.{sufixo}"))
                .collect::<Vec<String>>()
        };
        let mut do_dora = nomes("lora_a");
        do_dora.extend(nomes("lora_b"));
        do_dora.extend(nomes("mag"));
        let dora = escrever(
            &pasta,
            "dora.gguf",
            &gguf_com_nomes(&[("adapter.type", "lora"), ("adapter.name", "DoRA")], &do_dora),
        );
        let lora = escrever(
            &pasta,
            "lora.gguf",
            &gguf_com_nomes(
                &[("adapter.type", "lora"), ("adapter.name", "LoRA")],
                &[nomes("lora_a"), nomes("lora_b")].concat(),
            ),
        );
        // Os dois arquivos têm de ser o que dizem ser, ou o resto do teste mede
        // uma montagem que não existe.
        assert!(ler_ficha(&dora).unwrap().dora);
        assert!(!ler_ficha(&lora).unwrap().dora);

        let ajustes = Ajustes {
            pinos: vec![
                pino(&dora.display().to_string(), 1.0, true),
                pino(&lora.display().to_string(), 1.0, true),
            ],
        }
        .aprovar();
        let aplicados = vec![
            Aplicado { caminho: dora.clone(), aplicada: true, erro: None },
            Aplicado { caminho: lora.clone(), aplicada: true, erro: None },
        ];
        let linhas = itens(&pasta, &ajustes, &aplicados);
        let montado = |nome: &str| linhas.iter().find(|i| i.ficha.nome == nome).unwrap();
        assert!(montado("DoRA").magnitude_fora, "as deltas entraram sem a normalização");
        assert!(!montado("LoRA").magnitude_fora, "um LoRA comum não tem o que ficar fora");

        // O nome no JSON é o contrato com a tela, e um `camelCase` que escapa
        // aqui escapa invisível: o chip simplesmente some sem ninguém reclamar.
        let linha = serde_json::to_value(montado("DoRA")).unwrap();
        assert!(
            linha.get("magnitudeFora").is_some_and(|v| *v == serde_json::Value::Bool(true)),
            "o campo saiu torto no JSON: {linha}"
        );

        // Desligado não é "montado pela metade": é nada montado. A mesma ficha
        // que dava `true` acima devolve `false` quando o motor não aplicou.
        for sem_motor in [
            itens(&pasta, &ajustes, &[]),
            itens(
                &pasta,
                &ajustes,
                &[Aplicado {
                    caminho: dora.clone(),
                    aplicada: false,
                    erro: Some("o arquivo não abriu".to_string()),
                }],
            ),
        ] {
            let desligado = sem_motor.iter().find(|i| i.ficha.nome == "DoRA").unwrap();
            assert!(
                !desligado.magnitude_fora,
                "nada sobre os pesos, nada fora da montagem: {desligado:?}"
            );
        }
        std::fs::remove_dir_all(&pasta).ok();
    }

    #[test]
    fn config_sobrevive_ao_disco_e_um_arquivo_ruim_e_estado_vazio() {
        let pasta = pasta_de_teste("config");
        let caminho = pasta.join("adaptadores.json");
        let ajustes = Ajustes { pinos: vec![pino("a.gguf", 1.5, true)] };
        ajustes.salvar(&caminho).unwrap();
        assert_eq!(Ajustes::de(&caminho), ajustes);

        std::fs::write(&caminho, "{ isso não é json").unwrap();
        assert_eq!(Ajustes::de(&caminho), Ajustes::default());
        assert!(!caminho.with_extension("json.tmp").exists());
        std::fs::remove_dir_all(&pasta).ok();
    }

    #[test]
    fn sem_pino_e_o_caminho_que_sai() {
        let ajustes = Ajustes { pinos: vec![pino("a.gguf", 1.0, true), pino("b.gguf", 1.0, true)] };
        let limpo = ajustes.sem_pino(&PathBuf::from("a.gguf"));
        assert_eq!(limpo.pinos.len(), 1);
        assert_eq!(limpo.pinos[0].caminho, "b.gguf");
    }
}
