//! A memória do Aether: o que o assistente leu das pastas concedidas, guardado
//! como vetor, e recuperado por proximidade.
//!
//! É RAG sem nuvem, e o nome descreve o que está aqui: o texto vira vetor num
//! codificador GGUF local, os vetores vivem num arquivo desta máquina e a busca
//! é uma comparação de cosseno sobre eles. Não há serviço terceiro, banco
//! vetorial embutido nem chamada de rede — a única rede deste módulo é o
//! download do próprio codificador, que é o mesmo caminho dos modelos de
//! conversa.
//!
//! A política não é duplicada: recuperar memória é a ferramenta
//! `computador__buscar_memoria`, então o fio do grafo que a liga é o mesmo que
//! autoriza a injeção automática no prompt, e as pastas que ela enxerga são as
//! de `pastas.json`.

use super::catalog::{self, ModelSpec};
use super::ggml::{Despacho, Embutidor};
use crate::idiomas::tr;
use crate::local::pastas::Pastas;
use crate::state::{trava_estado, trava_escrita, trava_leitura};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Instant;
use tokenizers::Tokenizer;

/// O identificador do formato. Mudou a estrutura gravada, mudou esta palavra:
/// um índice lido com a assinatura errada é descartado, nunca interpretado torto.
const ASSINATURA: &[u8; 4] = b"AEM1";

/// Escalas que a tela oferece. `trechos` 0 desliga a injeção automática; a
/// ferramenta continua existindo para o modelo chamar quando quiser.
pub const TRECHOS_MAX: u32 = 8;
/// Tokens de uma peça. Acima do lote que o motor aceitar, a peça seria cortada
/// de qualquer jeito — e uma peça cortada daria um vetor de meia peça.
pub const PECA_MIN: u32 = 64;
pub const PECA_MAX: u32 = 1024;
/// Tokens da peça anterior que repetem na seguinte. Sem sobreposição, uma
/// função cortada ao meio não aparece inteira em janela nenhuma, e o trecho que
/// o modelo recebe para de existir no meio de uma linha.
pub const SOBREPOSICAO_MIN: u32 = 0;
pub const SOBREPOSICAO_MAX: u32 = 256;
/// Constante da fusão por colocação recíproca. É a do artigo que descreve o
/// método: 60 achata a diferença entre as primeiras posições, de forma que um
/// trecho bem colocado nas duas listas passa um que venceu uma só.
const RRF: f32 = 60.0;
/// Quantas posições de cada lista entram na fusão. Abaixo disto um trecho não
/// alcança o topo somado, e varrer o índice inteiro nas duas listas custaria o
/// dobro da busca para não mudar nenhuma das primeiras oito respostas.
const FUSAO_MAXIMA: usize = 64;
/// Os dois números do BM25 de Robertson. `k1` satura a frequência de um termo
/// numa peça — a décima repetição não vale dez vezes a primeira — e `b` pesa o
/// comprimento, para uma página inteira da palavra não ganhar de um trecho curto
/// que só fala dela.
const K1: f32 = 1.2;
const B: f32 = 0.75;
/// Menos que isto uma palavra não é termo de busca: "de", "a", "os" estão em
/// todo arquivo e não distinguem nada.
const PALAVRA_MIN: usize = 2;

/// Limite da varredura. Não é preciosismo: o índice é lido inteiro para a
/// memória, e um repositório com um milhão de arquivos não caberia no turno.
const PROFUNDIDADE_MAXIMA: usize = 16;
const LIMITE_ARQUIVOS: usize = 4_000;
const LIMITE_BYTES_ARQUIVO: u64 = 512 * 1024;
const LIMITE_BYTES_TOTAIS: u64 = 96 * 1024 * 1024;
const LIMITE_TRECHOS: usize = 12_000;
/// Quantos arquivos entre cada gravação do índice. Embedding é caro e uma
/// janela fechada no meio não pode jogar o trabalho fora.
const SALVAR_A_CADA: usize = 24;

/// Quantas conversas a memória cobre, contadas da mais recente para trás. É a
/// janela: o histórico na tela guarda tudo, a memória lembra as últimas. Passou
/// da janela, o trecho sai do índice — e voltar depois é só o varrer de novo.
pub const CONVERSAS_MEMORIAIS: usize = 200;
/// O teto do texto lembrado de cada conversa, contado a partir do fim. Uma
/// resposta longa de ferramenta não pode custar o turno inteiro para virar
/// vetor, e o fim da conversa é o que responde "sobre que estávamos falando".
pub const CONVERSA_MAX_BYTES: usize = 32 * 1024;
/// Tokens de conversa que um turno pode transformar em vetor antes de responder.
/// É a unidade que mede o custo real — o vetor sai de tokens, não de peças — e o
/// que impede a primeira pergunta depois de ligar a memória de esperar o
/// histórico inteiro. O resto entra nos turnos seguintes e no varrer, que já
/// corre fora do turno com barra de progresso.
pub const TOKENS_POR_TURNO: usize = 8_192;

/// A regra que decide se a verba do turno acabou. A primeira conversa entra
/// sempre, maior que a verba ou não: sem essa exceção um histórico com uma
/// conversa só, grande, ficaria para sempre fora do índice — e o atraso que a
/// tela mostra nunca se pagaria.
fn acabou_a_verba(gastos: usize, tokens: usize, orcamento: usize) -> bool {
    gastos > 0 && gastos + tokens > orcamento
}

/// O que a varredura aceita como texto, por extensão conhecida. "Tenta ler e vê"
/// custaria abrir binários de centenas de megabytes; aqui o arquivo só é lido
/// quando já se sabe que é do tipo que se lê.
const EXTENSOES: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "json", "jsonc", "md", "mdx", "txt", "rst",
    "toml", "yaml", "yml", "ini", "cfg", "conf", "css", "scss", "less", "html", "htm", "xml",
    "sql", "sh", "bash", "zsh", "ps1", "bat", "cmd", "py", "pyi", "rb", "go", "c", "h", "cpp",
    "hpp", "cc", "cs", "java", "kt", "kts", "swift", "lua", "gd", "tscn", "php", "vue", "svelte",
    "csv", "tex", "r", "jl", "ex", "exs", "erl", "hs", "ml", "nim", "zig", "dart", "gradle",
    "properties", "lock",
];

/// Arquivos sem extensão que são texto e interessam.
const NOMES: &[&str] = &[
    "readme", "license", "licence", "changelog", "dockerfile", "makefile", "justfile",
    "contributors", "notes",
];

/// Nome de pasta que a varredura não atravessa: dependência, saída de build ou
/// cache. O conteúdo deles é cópia do que já está indexado em outro lugar.
const PASTAS_IGNORADAS: &[&str] = &[
    "node_modules", "target", "build", "dist", "out", "obj", "debug", "release", "__pycache__",
    "site-packages", "venv", "vendor", "coverage", "htmlcov", ".mypy_cache", ".pytest_cache",
];

/// Um arquivo com qualquer destes traços no nome nunca entra no índice, mesmo
/// numa pasta concedida: memória serve para lembrar trabalho, não credencial.
const MARCAS_DE_SEGREDO: &[&str] = &[
    "secret", "segredo", "senha", "password", "passwd", "credential", "credencial", "token",
    "apikey", "api_key", "access_key", "private_key",
];
const EXTENSOES_DE_SEGREDO: &[&str] = &[
    "env", "key", "pem", "p12", "pfx", "crt", "cer", "der", "jks", "keystore", "kdb", "kdbx",
    "netrc",
];

fn minusculo(texto: &str) -> String {
    texto.to_lowercase()
}

/// Letras que o português (e o vizinho imediato dele) escreve com acento e que
/// a busca léxica conhece sem ele. Não é capricho: um `configuração` no arquivo
/// e um `configuracao` no teclado do usuário são a mesma palavra, e quem tem de
/// decidir isso é a busca — não o codificador, que nunca viu a consulta escrita.
const SEM_ACENTO: &[(char, char)] = &[
    ('à', 'a'), ('á', 'a'), ('â', 'a'), ('ã', 'a'), ('ä', 'a'), ('å', 'a'), ('ā', 'a'),
    ('è', 'e'), ('é', 'e'), ('ê', 'e'), ('ë', 'e'), ('ē', 'e'), ('ę', 'e'),
    ('ì', 'i'), ('í', 'i'), ('î', 'i'), ('ï', 'i'), ('ī', 'i'),
    ('ò', 'o'), ('ó', 'o'), ('ô', 'o'), ('õ', 'o'), ('ö', 'o'), ('ø', 'o'), ('ō', 'o'),
    ('ù', 'u'), ('ú', 'u'), ('û', 'u'), ('ü', 'u'), ('ū', 'u'),
    ('ç', 'c'), ('ñ', 'n'), ('ý', 'y'), ('ÿ', 'y'), ('š', 's'), ('ž', 'z'),
];

fn sem_acento(letra: char) -> char {
    SEM_ACENTO
        .iter()
        .find(|(de, _)| *de == letra)
        .map_or(letra, |(_, simples)| *simples)
}

/// O texto reduzido às palavras que a busca léxica indexa: separadas por
/// qualquer coisa que não é letra nem dígito, em minúsculo e sem acento.
fn palavras(texto: &str) -> Vec<String> {
    let mut saida: Vec<String> = Vec::new();
    let mut atual = String::new();
    for letra in texto.chars() {
        if letra.is_alphanumeric() {
            for minuscula in letra.to_lowercase() {
                atual.push(sem_acento(minuscula));
            }
        } else if !atual.is_empty() {
            saida.push(std::mem::take(&mut atual));
        }
    }
    if !atual.is_empty() {
        saida.push(atual);
    }
    saida
}

/// Pastas ocultas e as listas acima. A raiz concedida nunca é testada: se o
/// usuário apontou para dentro de `node_modules`, é lá que ele quer memória.
fn pasta_ignorada(nome: &str) -> bool {
    let nome = minusculo(nome);
    nome.starts_with('.') || PASTAS_IGNORADAS.iter().any(|i| nome == *i)
}

fn arquivo_ignorado(nome: &str) -> bool {
    let nome = minusculo(nome);
    if nome.starts_with('.') {
        return true;
    }
    if nome.starts_with("id_rsa") || nome.starts_with("id_") {
        return true;
    }
    if MARCAS_DE_SEGREDO.iter().any(|marca| nome.contains(marca)) {
        return true;
    }
    // O nome já está em minúsculo, então a extensão que sai daqui também está:
    // as listas abaixo são todas em minúsculo por construção.
    let extensao = Path::new(&nome)
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_default();
    if EXTENSOES_DE_SEGREDO.contains(&extensao.as_str()) {
        return true;
    }
    if extensao.is_empty() {
        return !NOMES.contains(&nome.as_str());
    }
    !EXTENSOES.contains(&extensao.as_str())
}

/// Como uma busca ordena os trechos. São três maneiras de responder a mesma
/// pergunta, e cada uma acha o que as outras não veem: o vetor acha o que é
/// parecido mesmo sem a palavra escrita, a palavra acha o identificador exato
/// que nenhum codificador de 200 milhões de parâmetros aprendeu a existir.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Modo {
    /// Só proximidade entre vetores.
    Vetorial,
    /// As duas listas, combinadas por colocação recíproca.
    Hibrido,
    /// Só BM25 sobre o texto das peças.
    Lexico,
}

impl Modo {
    /// A palavra de máquina que a interface traduz.
    pub fn nome(self) -> &'static str {
        match self {
            Self::Vetorial => "vetorial",
            Self::Hibrido => "hibrido",
            Self::Lexico => "lexico",
        }
    }
}

/// De onde veio um achado. Na fusão, um trecho pode entrar pelas duas listas —
/// e é isso que o coloca acima de tudo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origem {
    Vetorial,
    Lexica,
    Ambas,
}

/// O que escreve um trecho no índice. As duas fontes convivem no mesmo arquivo e
/// na mesma busca — é isso que faz um cosseno e um BM25 competirem na mesma
/// régua — mas a política de cada uma é sua.
///
/// Um arquivo só é lido enquanto a pasta onde ele está continua concedida; a
/// revogação tira o conteúdo dele da resposta e da varredura. Uma conversa é
/// estado do próprio aplicativo: não há pasta a revogar, e quem a apaga é a
/// lista de conversas. Confundir as duas — deixar a varredura de pastas apagar as
/// conversas, ou a concessão de pastas esconder o histórico — seria uma das
/// duas: memória perdida ou memória que o dono mandou esquecer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Fonte {
    /// Texto lido de um arquivo dentro de uma pasta concedida.
    #[default]
    Arquivo,
    /// Uma fala de uma conversa salva.
    Conversa,
}

impl Fonte {
    /// A palavra de máquina que a interface traduz.
    pub fn nome(self) -> &'static str {
        match self {
            Self::Arquivo => "arquivo",
            Self::Conversa => "conversa",
        }
    }
}

/// O que a tela edita, em `rag.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Ajustes {
    /// Id do codificador. Vazio é o primeiro completo em disco.
    pub modelo: String,
    /// Quantos trechos entram no prompt a cada pedido. 0 desliga a injeção.
    pub trechos: u32,
    /// Tokens-alvo de uma peça do índice.
    pub peca: u32,
    /// Tokens da peça anterior que repetem na seguinte.
    pub sobreposicao: u32,
    /// Piso de similaridade de cosseno (0 a 1) abaixo do qual o trecho não entra.
    pub piso: f32,
    /// Como a busca ordena.
    pub modo: Modo,
}

impl Default for Ajustes {
    fn default() -> Self {
        Self {
            modelo: String::new(),
            // Poucos e bons: cada trecho é texto que o modelo relê a cada rodada.
            trechos: 4,
            // Medido num código real: peças de ~380 tokens cabem uma função ou
            // um método inteiro e ainda deixam o vetor representar uma coisa só.
            peca: 384,
            // Um sexto da peça: o suficiente para uma função cortada aparecer
            // inteira em uma das duas janelas, sem dobrar o custo do índice.
            sobreposicao: 64,
            // Baixo, mas não zero: abaixo disto o vizinho mais próximo é ruído.
            piso: 0.15,
            modo: Modo::Hibrido,
        }
    }
}

impl Ajustes {
    pub fn de(caminho: &Path) -> Self {
        std::fs::read_to_string(caminho)
            .ok()
            .and_then(|texto| serde_json::from_str(&texto).ok())
            .map(|a: Self| a.aprovar())
            .unwrap_or_default()
    }

    /// Escrita atômica, como `geracao.json`: ou o JSON inteiro está no disco, ou
    /// fica o anterior.
    pub fn salvar(&self, caminho: &Path) -> Result<()> {
        if let Some(pai) = caminho.parent() {
            std::fs::create_dir_all(pai).with_context(|| {
                tr("erros.falhaCriarPasta", &[("caminho", &pai.display().to_string())])
            })?;
        }
        let texto = serde_json::to_string_pretty(self)
            .context(tr("erros.falhaSalvarConfig", &[]))?;
        let temporario = caminho.with_extension("json.tmp");
        std::fs::write(&temporario, &texto).with_context(|| {
            tr("erros.falhaEscreverConfig", &[("caminho", &temporario.display().to_string())])
        })?;
        std::fs::rename(&temporario, caminho).with_context(|| {
            tr("erros.falhaSubstituirConfig", &[("caminho", &caminho.display().to_string())])
        })?;
        Ok(())
    }

    pub fn aprovar(self) -> Self {
        let peca = self.peca.clamp(PECA_MIN, PECA_MAX);
        Self {
            modelo: self.modelo.trim().to_string(),
            trechos: self.trechos.min(TRECHOS_MAX),
            peca,
            // Metade da peça é o teto: acima disso duas janelas vizinhas seriam
            // quase o mesmo texto, e o índice encheria de repetição.
            sobreposicao: self.sobreposicao.clamp(SOBREPOSICAO_MIN, SOBREPOSICAO_MAX).min(peca / 2),
            piso: self.piso.clamp(0.0, 0.99),
            modo: self.modo,
        }
    }

    /// Trocar de codificador é trocar de espaço vetorial: o índice antigo não é
    /// comparável com o novo. Trocar de fatiamento também não é — mas por outro
    /// motivo, e é `Indice::refatiar` quem cobra.
    pub fn trocou_codificador(&self, antes: &Ajustes) -> bool {
        self.modelo != antes.modelo
    }
}

/// A prova de que um arquivo já foi lido: dois números que o disco dá de graça.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Assinatura {
    bytes: u64,
    modificado: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Trecho {
    /// O caminho do arquivo, ou o id da conversa. É a chave que a escrita e a
    /// remoção usam dentro da fonte dele.
    pub arquivo: String,
    /// Primeira linha do trecho, contando a partir de 1: é para onde o cartão
    /// aponta, e o número vem da contagem real dos quebras-linha do arquivo.
    /// Numa conversa, a linha é a mensagem — o texto dela vem uma por linha.
    pub linha: usize,
    /// O texto exatamente como está no arquivo. É o que o vetor representa;
    /// mostrar outra coisa seria um cartão mentiroso.
    pub texto: String,
    /// Quem pôs este trecho aqui. Um índice gravado antes disto existir é todo de
    /// arquivo, que é como ele foi escrito de fato.
    #[serde(default)]
    pub fonte: Fonte,
}

/// A ficha de uma conversa indexada: o que o cartão mostra e a prova de que o
/// texto dela é o que está no índice. `modificado` é o instante da última
/// mensagem e `bytes` a contagem delas — os dois números que a lista de conversas
/// dá de graça, sem reler arquivo nenhum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FichaConversa {
    titulo: String,
    /// Momento da última mensagem, para o cartão dizer quando foi a conversa.
    atualizada: u64,
    prova: Assinatura,
    /// A conversa foi cortada pelo teto de trechos no meio do fatiamento: o
    /// índice lembra só o começo dela. Marcada, a ficha não prova o texto
    /// inteiro — `conversa_vista` a trata como pendente para o turno seguinte
    /// tentar de novo quando houver espaço, em vez de prometer para sempre um
    /// trecho que não existe. Índice gravado antes disto existir não tem a
    /// chave: vale `false`, que é a verdade sobre uma conversa que entrou inteira.
    #[serde(default)]
    truncada: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Meta {
    /// Id do codificador que produziu os vetores.
    modelo: String,
    dimensao: usize,
    /// O agrupamento que a biblioteca respondeu, por extenso: é o que prova que
    /// o vetor saiu do último token e não de uma média.
    agrupamento: String,
    peca: u32,
    /// A sobreposição com que as peças gravadas foram cortadas. Junto com `peca`
    /// é o que diz se o índice ainda representa o fatiamento que a tela mostra.
    /// Faltando — índice gravado antes disto existir — vale zero, que é como
    /// ele foi cortado de fato.
    sobreposicao: u32,
    /// Um arquivo por chave, mesmo os que não renderam trecho nenhum — é assim
    /// que a próxima varredura sabe que não precisa relê-los.
    assinaturas: BTreeMap<String, Assinatura>,
    /// Uma conversa por id, com o título que o cartão mostra e a prova de que o
    /// texto indexado é o do arquivo dela. Separado de `assinaturas` de propósito:
    /// as duas fontes têm políticas diferentes, e um mapa só obrigaria a varredura
    /// de pastas a decidir o que fazer com o histórico — que não é dela.
    conversas: BTreeMap<String, FichaConversa>,
    trechos: Vec<Trecho>,
}

/// Um trecho devolvido por proximidade, com a medida que o classificou.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Achado {
    /// O caminho do arquivo, ou o id da conversa.
    pub arquivo: String,
    /// O rótulo curto do cartão: o nome do arquivo ou o título da conversa.
    pub nome: String,
    pub linha: usize,
    pub texto: String,
    /// Cosseno medido entre o vetor da consulta e o do trecho, de 0 a 1. Vale
    /// em todos os modos: um trecho que entrou pela palavra tem vetor no índice,
    /// e o número mostrado é ele, medido — nunca a pontuação da fusão, que não é
    /// uma similaridade e apareceria como se fosse.
    pub pontuacao: f32,
    /// O que trouxe o trecho: a proximidade, a palavra, ou as duas.
    pub origem: Origem,
    /// De onde veio o trecho — arquivo concedido ou conversa salva. O cartão
    /// traduz, porque "linha 12" de um arquivo não é a mesma coisa que a mensagem
    /// 12 de uma conversa.
    pub fonte: Fonte,
    /// Instante da última mensagem da conversa, em milissegundos. `None` num
    /// arquivo: a data que importa ali é a do trecho, e ela não existe no índice.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quando: Option<u64>,
}

/// O que uma busca devolve. Os `k` melhores e quantos trechos entraram nas
/// listas que mandaram na ordem — no modo misto é a união das duas, não a soma.
/// O segundo número é o que permite ao cartão escrever "e mais N" em vez de
/// adivinhar se a lista parou porque acabou ou porque o `k` mandou.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Resposta {
    pub achados: Vec<Achado>,
    pub alcancados: usize,
    /// O modo que produziu esta lista, em palavra de máquina.
    pub modo: &'static str,
}

/// O que uma busca precisa, lido do `rag.json` vivo. Agrupado porque as
/// escalas andam sempre juntas: uma busca com o `k` de um ajuste e o `piso` de
/// outro é exatamente a resposta que não se pode reproduzir.
pub struct Busca<'a> {
    pub vetor: &'a [f32],
    pub texto: &'a str,
    pub k: usize,
    pub piso: f32,
    pub modo: Modo,
    /// Conversas que esta busca não responde. É o turno deixando de fora a
    /// conversa que já está no buffer do modelo: reler o que ele acabou de ler
    /// custa os tokens que faltariam para a conversa que ele realmente não sabe.
    pub ignorar: &'a [String],
}

/// Uma conversa como a memória a recebe: a ficha que o cartão mostra e o texto
/// do que foi dito, uma fala por linha. Quem lê o histórico é `conversas.rs`;
/// aqui o texto só é fatiado e virado vetor, e a linha da peça é a fala que o
/// cartão cita.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversaViva {
    pub id: String,
    pub titulo: String,
    /// Instante da última mensagem, em milissegundos. É a data que o cartão mostra.
    pub atualizada: u64,
    /// Quantas mensagens tem a conversa — com `atualizada`, a prova de que não há
    /// trabalho novo.
    pub mensagens: usize,
    /// O texto de uma fala por linha: usuário e assistente, sem as chamadas de
    /// ferramenta. Quando a conversa é longa, é o fim dela — as falas da frente
    /// saem pelo teto de `CONVERSA_MAX_BYTES`.
    pub texto: String,
    /// Quantas falas o `texto` deixou para trás. É o que devolve ao cartão o
    /// número verdadeiro da fala citada numa conversa cortada pela cauda.
    pub a_partir_de: usize,
}

impl ConversaViva {
    /// A prova de que o índice gravado ainda representa esta conversa.
    fn prova(&self) -> Assinatura {
        Assinatura { bytes: self.mensagens as u64, modificado: self.atualizada }
    }
}

/// O que a tela lê do índice sem precisar dos vetores: tamanho, origem, o
/// agrupamento que a biblioteca respondeu quando eles foram produzidos e a
/// escala com que as peças foram cortadas.
#[derive(Debug, Clone, Default)]
struct Leitura {
    trechos: usize,
    arquivos: usize,
    conversas: usize,
    trechos_conversas: usize,
    dimensao: usize,
    agrupamento: String,
    peca: u32,
    sobreposicao: u32,
}

/// O índice: os trechos e um blob de vetores normalizados, lado a lado.
pub struct Indice {
    meta: Meta,
    /// `dimensao` floats por trecho, na mesma ordem de `meta.trechos`.
    vetores: Vec<f32>,
    /// As palavras do índice, montadas na primeira busca léxica. Guardado porque
    /// refazer o vocabulário de doze mil peças a cada pergunta custaria o turno
    /// inteiro. Toda escrita no índice substitui o `Indice` inteiro — nunca
    /// mexe num que já tem cache — então ele nunca fica atrás das peças.
    lexico: OnceLock<Lexico>,
}

impl Indice {
    fn vazio(
        modelo: &str,
        dimensao: usize,
        agrupamento: &str,
        peca: u32,
        sobreposicao: u32,
    ) -> Self {
        Self {
            meta: Meta {
                modelo: modelo.to_string(),
                dimensao,
                agrupamento: agrupamento.to_string(),
                peca,
                sobreposicao,
                assinaturas: BTreeMap::new(),
                conversas: BTreeMap::new(),
                trechos: Vec::new(),
            },
            vetores: Vec::new(),
            lexico: OnceLock::new(),
        }
    }

    pub fn dimensao(&self) -> usize {
        self.meta.dimensao
    }

    pub fn modelo(&self) -> &str {
        &self.meta.modelo
    }

    pub fn agrupamento(&self) -> &str {
        &self.meta.agrupamento
    }

    /// A escala com que as peças gravadas foram cortadas. É o que a tela compara
    /// com o ajuste vivo para dizer se o índice ainda é o índice que mostra.
    pub fn fatiamento(&self) -> (u32, u32) {
        (self.meta.peca, self.meta.sobreposicao)
    }

    /// O índice foi cortado com outra escala, e peça de escala diferente é
    /// trecho de texto diferente: o vetor gravado não representa o que a tela
    /// mostra agora. A única resposta honesta é apagar as peças e as assinaturas
    /// e reler tudo na varredura que vem. Devolve `true` quando a escala mudou —
    /// vazio ou cheio, o índice passa a ser outro a partir daqui.
    pub fn refatiar(&mut self, peca: u32, sobreposicao: u32) -> bool {
        if self.meta.peca == peca && self.meta.sobreposicao == sobreposicao {
            return false;
        }
        self.meta.peca = peca;
        self.meta.sobreposicao = sobreposicao;
        self.meta.trechos.clear();
        self.meta.assinaturas.clear();
        // As peças de conversa foram cortadas na escala antiga também. Sem tirar
        // a ficha junto, o índice diria que a conversa está lembrada quando o
        // vetor que a representa não existe mais.
        self.meta.conversas.clear();
        self.vetores.clear();
        true
    }

    pub fn trechos(&self) -> usize {
        self.meta.trechos.len()
    }

    /// Conversas que o índice lembra. É o número que a tela compara com a lista
    /// de conversas para o dono ver se a memória está atrás do histórico.
    pub fn conversas(&self) -> usize {
        self.meta.conversas.len()
    }

    /// Quantos trechos vieram de uma fonte. É como a tela diz, do mesmo índice,
    /// quantos são de arquivo e quantos são de conversa.
    pub fn trechos_de(&self, fonte: Fonte) -> usize {
        self.meta.trechos.iter().filter(|t| t.fonte == fonte).count()
    }

    /// Arquivos que a varredura já viu, incluindo os que não renderam nada.
    pub fn arquivos(&self) -> usize {
        self.meta.assinaturas.len()
    }

    /// Se a varredura já viu exatamente este estado deste arquivo. Fica privado:
    /// `Assinatura` é o estado interno do índice, e uma porta pública sobre ela
    /// seria uma chave que ninguém de fora sabe ter.
    fn esta_visto(&self, arquivo: &str, assinatura: &Assinatura) -> bool {
        self.meta.assinaturas.get(arquivo) == Some(assinatura)
    }

    /// Se a conversa já está no índice exatamente como está na lista de conversas.
    /// É esta resposta, e não uma reescrita, que faz o turno sem conversa nova não
    /// pagar um vetor nenhum. Uma ficha truncada nunca está "vista": falta texto
    /// dela no índice, e dizê-lo é o que devolve os trechos que faltam quando o
    /// teto de trechos abrir espaço.
    fn conversa_vista(&self, id: &str, prova: &Assinatura) -> bool {
        self.meta
            .conversas
            .get(id)
            .is_some_and(|ficha| !ficha.truncada && ficha.prova == *prova)
    }

    /// Quantas conversas pedem trabalho: as novas, as que ganharam mensagem e as
    /// que saíram do histórico (apagadas ou velhas demais para a janela que a
    /// tela manda). Zero é o caso comum, e é ele que o turno paga para
    /// reconhecer — sem clonar um vetor sequer.
    pub fn conversas_pendentes(&self, vivas: &[ConversaViva]) -> usize {
        let fora = self
            .meta
            .conversas
            .keys()
            .filter(|id| !vivas.iter().any(|c| *c.id == **id))
            .count();
        let mudaram = vivas
            .iter()
            .filter(|c| !self.conversa_vista(&c.id, &c.prova()))
            .count();
        fora + mudaram
    }

    /// Esquece toda conversa que não está mais na lista: apagada do histórico ou
    /// saída da janela que a memória cobre. Devolve quantas saíram.
    pub fn reter_conversas(&mut self, vivas: &[ConversaViva]) -> usize {
        let fora: Vec<String> = self
            .meta
            .conversas
            .keys()
            .filter(|id| !vivas.iter().any(|c| *c.id == **id))
            .cloned()
            .collect();
        let mut saíram = 0usize;
        for id in fora {
            saíram += self.esquecer_conversa(&id);
        }
        saíram
    }

    /// Remove os trechos de um arquivo e o marca para reler. Devolve quantos
    /// trechos saíram, que é o número que a interface mostra.
    pub fn remover_arquivo(&mut self, arquivo: &str) -> usize {
        self.remover(Fonte::Arquivo, arquivo)
    }

    /// Esquece uma conversa inteira: o texto lembrado dela e a ficha. É o que
    /// acontece quando a pessoa apaga a conversa — apagar o histórico tem de
    /// apagar a memória, e não só o arquivo.
    pub fn esquecer_conversa(&mut self, id: &str) -> usize {
        let saíram = self.remover(Fonte::Conversa, id);
        self.meta.conversas.remove(id);
        saíram
    }

    /// Tira do índice os trechos de uma chave, e só os da fonte pedida. As duas
    /// fontes têm chaves que não se encontram — caminho cheio contra id de
    /// conversa — mas é a fonte que decide, para a varredura de pastas não mexer
    /// no histórico nem a reescrita de uma conversa mexer nos arquivos.
    fn remover(&mut self, fonte: Fonte, chave: &str) -> usize {
        let antes = self.meta.trechos.len();
        let mut novos_trechos: Vec<Trecho> = Vec::with_capacity(antes);
        let mut novos_vetores: Vec<f32> = Vec::with_capacity(self.vetores.len());
        let dimensao = self.meta.dimensao;
        for (indice, trecho) in self.meta.trechos.iter().enumerate() {
            if trecho.fonte != fonte || trecho.arquivo != chave {
                novos_trechos.push(trecho.clone());
                let inicio = indice * dimensao;
                novos_vetores.extend_from_slice(&self.vetores[inicio..inicio + dimensao]);
            }
        }
        let saíram = antes - novos_trechos.len();
        self.meta.trechos = novos_trechos;
        self.vetores = novos_vetores;
        saíram
    }

    /// Esquece tudo que não está mais dentro de uma pasta concedida. É isto que
    /// faz o fio da memória perder o conteúdo de uma pasta revogada no grafo.
    pub fn reter_dentro(&mut self, raizes: &[PathBuf]) -> usize {
        let fora: Vec<String> = self
            .meta
            .assinaturas
            .keys()
            .filter(|arquivo| !concedida(arquivo, raizes))
            .cloned()
            .collect();
        let mut removidos = 0usize;
        for arquivo in fora {
            removidos += self.remover_arquivo(&arquivo);
            self.meta.assinaturas.remove(&arquivo);
        }
        removidos
    }

    /// Anexa as peças de um arquivo com a assinatura que ele tinha no disco.
    /// Também privado: a mesma razão de `esta_visto`.
    fn adicionar(&mut self, arquivo: &str, assinatura: Assinatura, pecas: &[(Peca, Vec<f32>)]) {
        for (peca, vetor) in pecas {
            self.meta.trechos.push(Trecho {
                arquivo: arquivo.to_string(),
                linha: peca.linha,
                texto: peca.texto.clone(),
                fonte: Fonte::Arquivo,
            });
            self.vetores.extend_from_slice(vetor);
        }
        self.meta.assinaturas.insert(arquivo.to_string(), assinatura);
    }

    /// Troca a memória de uma conversa inteira: sai o texto lembrado, entra o de
    /// agora com a ficha que o cartão mostra. Reescrever a conversa toda, e não
    /// só as mensagens novas, é o que faz um histórico editado ou limpo não deixar
    /// trecho órfão apontando para uma mensagem que não existe mais.
    ///
    /// A linha de cada peça é contada no texto que foi fatiado; somar
    /// `a_partir_de` devolve o número da mensagem como a conversa o vê.
    ///
    /// `truncada` diz se o teto de trechos cortou a conversa no meio do
    /// fatiamento: a ficha guarda a prova completa mesmo assim (é o estado da
    /// conversa na lista), mas marcada não conta como vista — ver `conversa_vista`.
    fn substituir_conversa(
        &mut self,
        viva: &ConversaViva,
        prova: Assinatura,
        pecas: &[(Peca, Vec<f32>)],
        truncada: bool,
    ) {
        self.remover(Fonte::Conversa, &viva.id);
        for (peca, vetor) in pecas {
            self.meta.trechos.push(Trecho {
                arquivo: viva.id.clone(),
                linha: peca.linha + viva.a_partir_de,
                texto: peca.texto.clone(),
                fonte: Fonte::Conversa,
            });
            self.vetores.extend_from_slice(vetor);
        }
        self.meta.conversas.insert(
            viva.id.clone(),
            FichaConversa {
                titulo: viva.titulo.clone(),
                atualizada: viva.atualizada,
                prova,
                truncada,
            },
        );
    }

    /// Nada entra no índice sem passar por aqui: com os dois lados normatizados,
    /// o produto escalar *é* o cosseno, e a busca inteira vira uma soma de
    /// multiplicações.
    fn normalizar(vetor: &mut [f32]) -> Result<()> {
        let soma: f32 = vetor.iter().map(|v| v * v).sum();
        let norma = soma.sqrt();
        if !(norma > 1e-9) || !norma.is_finite() {
            // Um vetor de norma zero não é "longe de tudo", é um codificador que
            // não respondeu. Isso é falha, e falha se mostra.
            bail!(tr("erros.memoriaVetorZero", &[]));
        }
        for v in vetor.iter_mut() {
            *v /= norma;
        }
        Ok(())
    }

    /// Os `k` melhores acima do `piso`, na ordem que o `modo` manda, e só do que
    /// ainda pode ser lembrado: os arquivos das pastas concedidas em `raizes` e as
    /// conversas que `ignorar` não corta.
    ///
    /// O cosseno é sempre medido, mesmo em modo léxico: ele é o número do
    /// cartão, e um achado que chegou pela palavra continua tendo o vetor dele no
    /// índice. O que muda com o modo é a *ordem*, não a medida.
    pub fn buscar(&self, busca: &Busca, raizes: &[PathBuf]) -> Result<Resposta> {
        if busca.vetor.len() != self.meta.dimensao {
            // Só acontece com índice de outro codificador em memória, o que o
            // `Custodio` já recusa ao carregar. Preferimos o erro ao vizinho
            // errado calculado sobre duas larguras diferentes.
            bail!(tr(
                "erros.memoriaDimensao",
                &[
                    ("esperada", &self.meta.dimensao.to_string()),
                    ("recebida", &busca.vetor.len().to_string())
                ]
            ));
        }
        let lexico = matches!(busca.modo, Modo::Hibrido | Modo::Lexico);
        // Quem responde se um trecho pode ser devolvido é a fonte dele: o arquivo
        // obedece às pastas concedidas, a conversa obedece à lista de conversas e
        // ao `ignorar` do turno. Nenhuma das duas empresta política para a outra.
        let pode = |indice: usize| -> bool {
            let trecho = &self.meta.trechos[indice];
            match trecho.fonte {
                Fonte::Arquivo => concedida(&trecho.arquivo, raizes),
                Fonte::Conversa => {
                    !busca.ignorar.iter().any(|id| *id == trecho.arquivo)
                }
            }
        };
        let mut proximidade = vec![0f32; self.meta.trechos.len()];
        let mut vetorial: Vec<usize> = Vec::new();
        for indice in 0..self.meta.trechos.len() {
            if !pode(indice) {
                continue;
            }
            let inicio = indice * self.meta.dimensao;
            let vetor = &self.vetores[inicio..inicio + self.meta.dimensao];
            let similaridade: f32 = vetor.iter().zip(busca.vetor.iter()).map(|(a, b)| a * b).sum();
            proximidade[indice] = similaridade;
            if busca.modo != Modo::Lexico && similaridade >= busca.piso {
                vetorial.push(indice);
            }
        }
        // A posição na lista vetorial é o que a fusão lê, então ela é ordenada
        // em qualquer modo que use o vetor — não só no modo puramente vetorial.
        if busca.modo != Modo::Lexico {
            vetorial.sort_by(|a, b| proximidade[*b].total_cmp(&proximidade[*a]));
        }
        let por_palavra: Vec<usize> = if lexico {
            let mut marcados = self
                .lexico()
                .pontuar(busca.texto)
                .into_iter()
                .filter(|(_, indice)| pode(*indice))
                .collect::<Vec<(f32, usize)>>();
            // Empate de BM25 vai para o índice menor: sem desempate fixo a mesma
            // pergunta daria listas diferentes, e nenhuma medição do cartão
            // seria comparável de uma execução para outra.
            marcados.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
            marcados.into_iter().map(|(_, indice)| indice).collect()
        } else {
            Vec::new()
        };

        // A ordem final: uma lista vem do vetor, da palavra, ou as duas somadas
        // por colocação recíproca. Na fusão não é a pontuação que manda — é a
        // posição em cada lista, porque um cosseno de 0,91 e um BM25 de 14,3 não
        // são a mesma grandeza e somá-los seria escolher uma unidade no azar.
        let (ordenado, alcancados) = match busca.modo {
            Modo::Vetorial => (
                vetorial.iter().map(|indice| (*indice, Origem::Vetorial)).collect(),
                vetorial.len(),
            ),
            Modo::Lexico => (
                por_palavra
                    .iter()
                    .map(|indice| (*indice, Origem::Lexica))
                    .collect(),
                por_palavra.len(),
            ),
            Modo::Hibrido => (
                Self::fundir(
                    &vetorial.iter().copied().take(FUSAO_MAXIMA).collect::<Vec<usize>>(),
                    &por_palavra.iter().copied().take(FUSAO_MAXIMA).collect::<Vec<usize>>(),
                    &proximidade,
                ),
                // O "e mais N" da fusão conta quem entrou por qualquer caminho:
                // foi esse o conjunto que a ordenação teve à disposição.
                Self::uniao(&vetorial, &por_palavra),
            ),
        };
        Ok(Resposta {
            achados: ordenado
                .into_iter()
                .take(busca.k)
                .map(|(indice, origem)| {
                    let trecho = &self.meta.trechos[indice];
                    // O rótulo vem da ficha gravada junto com o trecho, nunca de
                    // uma suposição sobre o id: um título que a pessoa escreveu
                    // depois de indexar aparece na próxima reescrita da conversa,
                    // que é quando o índice passa a ser outro.
                    let (nome, quando) = match trecho.fonte {
                        Fonte::Arquivo => (nome_do_arquivo(&trecho.arquivo), None),
                        Fonte::Conversa => self
                            .meta
                            .conversas
                            .get(&trecho.arquivo)
                            .map(|ficha| {
                                (ficha.titulo.clone(), Some(ficha.atualizada))
                            })
                            .unwrap_or_else(|| (trecho.arquivo.clone(), None)),
                    };
                    Achado {
                        nome,
                        arquivo: trecho.arquivo.clone(),
                        linha: trecho.linha,
                        texto: trecho.texto.clone(),
                        pontuacao: proximidade[indice],
                        origem,
                        fonte: trecho.fonte,
                        quando,
                    }
                })
                .collect(),
            alcancados,
            modo: busca.modo.nome(),
        })
    }

    /// Quantos trechos aparecem em alguma das duas listas. É o denominador do
    /// "e mais N": na fusão, o que sobrou foi o que entrou por qualquer caminho,
    /// e contar por conjunto (não por `Vec::contains` sobre doze mil índices) é
    /// o que faz a busca continuar custando o tamanho do índice, não o quadrado
    /// dele.
    fn uniao(vetorial: &[usize], por_palavra: &[usize]) -> usize {
        let mut vistos: BTreeSet<usize> = vetorial.iter().copied().collect();
        vistos.extend(por_palavra.iter().copied());
        vistos.len()
    }

    /// Colocação recíproca: cada lista dá `1 / (RRF + posição)` ao trecho, e a
    /// soma decide. Um terceiro lugar nas duas listas passa um primeiro em uma
    /// só, que é o comportamento que faz as duas buscas concordarem em vez de
    /// uma delas mandar sozinha.
    fn fundir(
        vetorial: &[usize],
        por_palavra: &[usize],
        proximidade: &[f32],
    ) -> Vec<(usize, Origem)> {
        let mut marcados: BTreeMap<usize, (f32, bool, bool)> = BTreeMap::new();
        for (posicao, indice) in vetorial.iter().enumerate() {
            let entrada = marcados.entry(*indice).or_insert((0.0, false, false));
            entrada.0 += 1.0 / (RRF + posicao as f32);
            entrada.1 = true;
        }
        for (posicao, indice) in por_palavra.iter().enumerate() {
            let entrada = marcados.entry(*indice).or_insert((0.0, false, false));
            entrada.0 += 1.0 / (RRF + posicao as f32);
            entrada.2 = true;
        }
        let mut ordenado: Vec<(f32, usize, Origem)> = marcados
            .into_iter()
            .map(|(indice, (fusão, vetorial, lexica))| {
                (
                    fusão,
                    indice,
                    match (vetorial, lexica) {
                        (true, true) => Origem::Ambas,
                        (false, true) => Origem::Lexica,
                        _ => Origem::Vetorial,
                    },
                )
            })
            .collect();
        // Empate de colocação vai para o cosseno maior, e depois para o índice:
        // sem desempate fixo a mesma pergunta daria listas diferentes entre
        // execuções, e nenhuma medição do cartão seria comparável.
        ordenado.sort_by(|a, b| {
            b.0.total_cmp(&a.0)
                .then(proximidade[b.1].total_cmp(&proximidade[a.1]))
                .then(a.1.cmp(&b.1))
        });
        ordenado.into_iter().map(|(_, indice, origem)| (indice, origem)).collect()
    }

    /// O vocabulário do índice, montado uma vez por índice vivo e guardado nele
    /// até alguém substituir o `Indice` inteiro.
    fn lexico(&self) -> &Lexico {
        self.lexico.get_or_init(|| Lexico::construir(&self.meta.trechos))
    }

    /// Grava o índice. O formato é próprio de propósito: `magic` de quatro
    /// bytes, o JSON da metadata e depois os floats crus, porque um índice de
    /// milhões de números serializado como texto seria dez vezes maior e lento
    /// de ler.
    pub fn salvar(&self, caminho: &Path) -> Result<()> {
        if let Some(pai) = caminho.parent() {
            std::fs::create_dir_all(pai).with_context(|| {
                tr("erros.falhaCriarPasta", &[("caminho", &pai.display().to_string())])
            })?;
        }
        let meta = serde_json::to_vec(&self.meta)
            .context(tr("erros.falhaSalvarConfig", &[]))?;
        let temporario = caminho.with_extension("aether-tmp");
        let gravado = (|| -> Result<()> {
            let mut arquivo = std::fs::File::create(&temporario)
                .with_context(|| tr("erros.semPastaIndice", &[]))?;
            arquivo.write_all(ASSINATURA)?;
            arquivo.write_all(&(meta.len() as u32).to_le_bytes())?;
            arquivo.write_all(&meta)?;
            let mut blob = Vec::with_capacity(self.vetores.len() * 4);
            for v in &self.vetores {
                blob.extend_from_slice(&v.to_le_bytes());
            }
            arquivo.write_all(&blob)?;
            arquivo.flush()?;
            Ok(())
        })();
        if let Err(erro) = gravado {
            std::fs::remove_file(&temporario).ok();
            return Err(erro).context(tr("erros.semPastaIndice", &[]));
        }
        std::fs::rename(&temporario, caminho).map_err(|erro| {
            std::fs::remove_file(&temporario).ok();
            anyhow::anyhow!(tr(
                "erros.falhaEscrever",
                &[("caminho", &caminho.display().to_string()), ("detalhe", &erro.to_string())]
            ))
        })
    }

    /// Lê o arquivo. `None` é resposta honesta para "não há índice ainda" e para
    /// "o índice não bate consigo mesmo" — nas duas o caminho é indexar de novo.
    pub fn carregar(caminho: &Path) -> Option<Indice> {
        let mut bruto = Vec::new();
        std::fs::File::open(caminho).ok()?.read_to_end(&mut bruto).ok()?;
        if bruto.len() < 8 || &bruto[..4] != ASSINATURA {
            return None;
        }
        let meta_len = u32::from_le_bytes([bruto[4], bruto[5], bruto[6], bruto[7]]) as usize;
        let fim = 8usize.checked_add(meta_len)?;
        if bruto.len() < fim {
            return None;
        }
        let meta: Meta = serde_json::from_slice(&bruto[8..fim]).ok()?;
        let blob = &bruto[fim..];
        // A conta do tamanho esperado é checada: um `dimensao` danificado no
        // cabeçalho multiplicaria e embrulhari em release até casar com um blob
        // curto, o índice carregaria torto e a fatia `vetores[..inicio+dimensao]`
        // estouraria mais tarde. Overflow aqui é "não sei", e o caminho é indexar
        // de novo — igual a um arquivo que não bate consigo mesmo.
        let esperado = meta
            .trechos
            .len()
            .checked_mul(meta.dimensao)
            .and_then(|n| n.checked_mul(4));
        if meta.dimensao == 0 || esperado != Some(blob.len()) {
            return None;
        }
        let vetores = blob
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        Some(Indice { meta, vetores, lexico: OnceLock::new() })
    }
}

/// O outro lado da mesma memória: as palavras que aparecem nas peças. O vetor
/// acha o que é parecido mesmo sem a palavra escrita; a palavra acha o
/// identificador exato, o texto de um erro, o nome de uma função que nenhum
/// codificador de duzentos milhões de parâmetros aprendeu a existir.
#[derive(Debug, Default)]
struct Lexico {
    /// Termo → peças onde ele aparece, com quantas vezes. Ordem de peça é a de
    /// `meta.trechos`, a mesma do blob de vetores.
    termos: BTreeMap<String, Vec<(usize, u32)>>,
    /// Palavras de cada peça, na mesma ordem de `meta.trechos`.
    comprimentos: Vec<u32>,
    /// Média daquele vetor, o referencial do `b` do BM25.
    media: f32,
}

impl Lexico {
    /// Vocabulário do índice inteiro. Uma passada sobre o texto gravado, sem
    /// motor nenhum: é por isto que a busca léxica funciona com o codificador
    /// descarregado e responde pelo que está escrito, não pelo que foi aprendido.
    fn construir(trechos: &[Trecho]) -> Self {
        let mut termos: BTreeMap<String, Vec<(usize, u32)>> = BTreeMap::new();
        let mut comprimentos: Vec<u32> = Vec::with_capacity(trechos.len());
        for (indice, trecho) in trechos.iter().enumerate() {
            let lista = palavras(&trecho.texto);
            comprimentos.push(lista.len() as u32);
            for palavra in lista {
                if palavra.chars().count() < PALAVRA_MIN {
                    continue;
                }
                let posting = termos.entry(palavra).or_default();
                match posting.last_mut() {
                    Some((mesma, quantidade)) if *mesma == indice => *quantidade += 1,
                    _ => posting.push((indice, 1)),
                }
            }
        }
        let media = if comprimentos.is_empty() {
            0.0
        } else {
            comprimentos.iter().map(|c| *c as f64).sum::<f64>() / comprimentos.len() as f64
        };
        Self { termos, comprimentos, media: media as f32 }
    }

    /// BM25 da pergunta sobre o índice. Devolve só quem tem ao menos uma
    /// palavra em comum — o resto não é "nota baixa", é ausência, e ausência não
    /// entra em lista nenhuma.
    fn pontuar(&self, pergunta: &str) -> Vec<(f32, usize)> {
        if self.comprimentos.is_empty() || self.media <= 0.0 {
            return Vec::new();
        }
        let documentos = self.comprimentos.len() as f32;
        let mut somas: BTreeMap<usize, f32> = BTreeMap::new();
        // A mesma palavra repetida na pergunta não conta duas vezes: quem soma
        // o idf por ocorrência premiaria quem digitou o termo de novo.
        for palavra in Self::vocabulario(pergunta) {
            let Some(posting) = self.termos.get(&palavra) else { continue };
            // idf de Robertson & Sparck Jones com o +1 dentro do log: `df` nunca
            // passa de `N`, então o argumento é sempre ≥ 1 e a nota nunca é
            // negativa — uma palavra que está em todo arquivo quase não pontua.
            let idf = (1.0
                + (documentos - posting.len() as f32 + 0.5) / (posting.len() as f32 + 0.5))
                .ln();
            for (indice, quantidade) in posting.iter() {
                let comprimento = self.comprimentos[*indice] as f32;
                // O comprimento entra relativo à média do corpus, dentro do
                // denominador que satura a frequência: uma peça cinco vezes maior
                // que a média precisa de cinco vezes mais ocorrências do termo
                // para chegar à mesma nota.
                let normalizada = *quantidade as f32;
                let peso = 1.0 - B + B * comprimento / self.media;
                *somas.entry(*indice).or_insert(0.0) +=
                    idf * normalizada * (K1 + 1.0) / (normalizada + K1 * peso);
            }
        }
        somas
            .into_iter()
            .filter(|(_, soma)| *soma > 0.0)
            .map(|(indice, soma)| (soma, indice))
            .collect()
    }

    /// As palavras distintas de uma pergunta, já reduzidas como as do índice.
    fn vocabulario(texto: &str) -> BTreeSet<String> {
        palavras(texto)
            .into_iter()
            .filter(|p| p.chars().count() >= PALAVRA_MIN)
            .collect()
    }
}

/// Se o arquivo ainda está dentro de uma pasta concedida. É a política do
/// aplicativo, cobrada no momento da leitura: um trecho de uma pasta revogada não
/// é devolvido nem que o vetor dele continue no índice esperando a próxima
/// varredura apagar.
fn concedida(arquivo: &str, raizes: &[PathBuf]) -> bool {
    let caminho = Path::new(arquivo);
    raizes.iter().any(|raiz| caminho.starts_with(raiz))
}

fn nome_do_arquivo(arquivo: &str) -> String {
    Path::new(arquivo)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| arquivo.to_string())
}

/// O resumo do índice, lido da metadata sem tocar no blob de vetores.
fn leitura(indice: &Indice) -> Leitura {
    let (peca, sobreposicao) = indice.fatiamento();
    Leitura {
        trechos: indice.trechos(),
        arquivos: indice.arquivos(),
        conversas: indice.conversas(),
        trechos_conversas: indice.trechos_de(Fonte::Conversa),
        dimensao: indice.dimensao(),
        agrupamento: indice.agrupamento().to_string(),
        peca,
        sobreposicao,
    }
}

/// A cópia que entra no lugar de um índice gravado. O vocabulário léxico nunca
/// é copiado de propósito: as peças novas valem do zero, e um cache que
/// sobrevivesse a uma reescrita responderia por texto que não está mais lá.
fn copiar(indice: &Indice) -> Indice {
    Indice {
        meta: indice.meta.clone(),
        vetores: indice.vetores.clone(),
        lexico: OnceLock::new(),
    }
}

/// Uma peça do índice: o texto, a linha onde ele começa e os ids que o motor
/// consumiu. Os três andam juntos porque o vetor tem de representar exatamente
/// o texto que o cartão mostra.
#[derive(Debug, Clone, PartialEq)]
pub struct Peca {
    pub linha: usize,
    pub texto: String,
    pub ids: Vec<u32>,
}

fn codificar(tokenizador: &Tokenizer, texto: &str) -> Result<Vec<u32>> {
    tokenizador
        .encode(texto, false)
        .map(|e| e.get_ids().to_vec())
        .map_err(|e| anyhow::anyhow!(tr("erros.tokenizar", &[("detalhe", &e.to_string())])))
}

/// Uma linha dentro da peça em construção: onde ela começa no arquivo, o texto
/// dela e os ids que ela custou. Os três andam juntos porque a sobreposição tem
/// de recomeçar uma peça com linhas reais, não com um número aproximado.
#[derive(Debug, Clone)]
struct LinhaPeca {
    numero: usize,
    texto: String,
    ids: Vec<u32>,
}

/// Fecha a peça corrente e reabre a seguinte com a cauda pedida como
/// sobreposição. É isto que faz uma função cortada ao meio aparecer inteira em
/// uma das duas janelas vizinhas. Devolve quantos tokens a nova peça já tem.
fn fechar(
    saida: &mut Vec<Peca>,
    atuais: &mut Vec<LinhaPeca>,
    sobre: usize,
    tokenizador: &Tokenizer,
) -> Result<usize> {
    if atuais.is_empty() {
        return Ok(0);
    }
    let texto = atuais
        .iter()
        .map(|registro| registro.texto.as_str())
        .collect::<Vec<&str>>()
        .join("\n");
    let ids: Vec<u32> = atuais.iter().flat_map(|r| r.ids.iter().copied()).collect();
    saida.push(Peca { linha: atuais[0].numero, texto, ids });

    // A cauda entra por linhas inteiras: cortar uma linha ao meio, quando há
    // mais de uma, daria uma sobreposição que não é nem texto nem tokenização.
    let mut cauda: Vec<LinhaPeca> = Vec::new();
    let mut guardados = 0usize;
    for registro in atuais.iter().rev() {
        if guardados + registro.ids.len() > sobre {
            break;
        }
        guardados += registro.ids.len();
        cauda.push(registro.clone());
    }
    if cauda.is_empty() && sobre > 0 {
        // A última linha sozinha já passa da sobreposição: entra o final dela,
        // com o texto voltando do próprio tokenizador para que o par (texto,
        // ids) continue verdadeiro — o mesmo caminho de uma linha sem quebra.
        let registro = atuais.last().expect("peça com linhas");
        let n = sobre.min(registro.ids.len());
        if n > 0 {
            let ids = registro.ids[registro.ids.len() - n..].to_vec();
            let texto = tokenizador.decode(&ids, false).map_err(|e| {
                anyhow::anyhow!(tr("erros.tokenizar", &[("detalhe", &e.to_string())]))
            })?;
            cauda.push(LinhaPeca { numero: registro.numero, texto, ids });
        }
    }
    cauda.reverse();
    // A cauda pode abrir com linhas em branco: elas não custam token nenhum e
    // por isso passam por qualquer teto de `sobre`. A peça seguinte tem de
    // começar na linha do primeiro token dela — é essa linha que o cartão mostra
    // —, então a régua da linha em branco que não abre peça vale dos dois lados.
    let primeiro = cauda
        .iter()
        .position(|registro| !registro.ids.is_empty())
        .unwrap_or(cauda.len());
    cauda.drain(..primeiro);
    let tokens = cauda.iter().map(|r| r.ids.len()).sum();
    *atuais = cauda;
    Ok(tokens)
}

/// Uma linha maior que o lote inteiro, dividida por tokens. O texto de cada
/// fatia volta do próprio tokenizador, para o par (texto, ids) continuar
/// verdadeiro, e as fatias se repetem em `sobre` tokens na frente uma da outra.
fn fatias(
    numero: usize,
    ids: &[u32],
    limite: usize,
    sobre: usize,
    tokenizador: &Tokenizer,
) -> Result<Vec<Peca>> {
    let passo = (limite - sobre).max(1);
    let mut saida = Vec::new();
    for inicio in (0..ids.len()).step_by(passo) {
        let fim = (inicio + limite).min(ids.len());
        let fatia = &ids[inicio..fim];
        let texto = tokenizador.decode(fatia, false).map_err(|e| {
            anyhow::anyhow!(tr("erros.tokenizar", &[("detalhe", &e.to_string())]))
        })?;
        saida.push(Peca { linha: numero, texto, ids: fatia.to_vec() });
    }
    Ok(saida)
}

/// Fatia em peças de no máximo `alvo` tokens, quebrando só em fronteira de
/// linha, e uma linha maior que o lote é dividida por tokens.
///
/// A contagem é feita linha por linha e somada, o que é um teto do que a
/// tokenização junto custaria — uma junção a mais nunca aumenta o número de
/// tokens. É isso que garante que nenhuma peça passa do lote, sem precisar
/// tokenizar o buffer inteiro a cada linha acrescentada.
///
/// `sobre` tokens da peça fechada reabrem a seguinte. O teto real é `alvo / 2`:
/// mais que isso, duas janelas vizinhas seriam quase o mesmo texto e o índice
/// encheria de repetição em vez de memória.
pub fn pecas(
    texto: &str,
    alvo: usize,
    sobre: usize,
    limite: usize,
    tokenizador: &Tokenizer,
) -> Result<Vec<Peca>> {
    let alvo = alvo.clamp(1, limite);
    let sobre = sobre.min(alvo / 2);
    let mut saida: Vec<Peca> = Vec::new();
    let mut atuais: Vec<LinhaPeca> = Vec::new();
    let mut tokens = 0usize;

    for (indice, linha) in texto.lines().enumerate() {
        let numero = indice + 1;
        let ids_linha = codificar(tokenizador, linha)?;

        if ids_linha.len() > limite {
            if !atuais.is_empty() {
                // A cauda da peça aberta não tem lugar aqui: a fatia de uma
                // linha sem quebra já ocupa o lote inteiro sozinha.
                tokens = fechar(&mut saida, &mut atuais, 0, tokenizador)?;
            }
            saida.extend(fatias(numero, &ids_linha, limite, sobre, tokenizador)?);
            continue;
        }
        if tokens + ids_linha.len() > alvo && !atuais.is_empty() {
            tokens = fechar(&mut saida, &mut atuais, sobre, tokenizador)?;
            if tokens + ids_linha.len() > limite {
                // Nem a linha cabe depois da sobreposição. A cauda vira peça
                // sozinha, porque o teto do lote não é uma sugestão.
                tokens = fechar(&mut saida, &mut atuais, 0, tokenizador)?;
            }
        }
        if ids_linha.is_empty() && atuais.is_empty() {
            // Linha em branco antes de uma peça não abre peça: ela só engrossaria
            // o texto de uma peça que começa mais abaixo.
            continue;
        }
        tokens += ids_linha.len();
        atuais.push(LinhaPeca { numero, texto: linha.to_string(), ids: ids_linha });
        if tokens >= limite {
            tokens = fechar(&mut saida, &mut atuais, sobre, tokenizador)?;
        }
    }
    fechar(&mut saida, &mut atuais, 0, tokenizador)?;
    Ok(saida)
}

/// O que a varredura encontrou para ler, com a prova de como estava no disco.
/// `completa` é falsa quando um teto cortou a lista — e enquanto ela for falsa a
/// varredura não apaga nada: "não vi" não é "não existe".
struct Varredura {
    candidatos: Vec<(PathBuf, Assinatura)>,
    completa: bool,
}

/// A prova de como o arquivo estava no disco: tamanho e instante da última
/// escrita. O caminho não entra — ele é a chave, não o estado.
fn assinar(meta: &std::fs::Metadata) -> Assinatura {
    Assinatura {
        bytes: meta.len(),
        modificado: meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
    }
}

fn varrer(raizes: &[PathBuf]) -> Varredura {
    let mut candidatos: Vec<(PathBuf, Assinatura)> = Vec::new();
    let mut bytes = 0u64;
    let mut completa = true;
    for raiz in raizes {
        let Ok(meta) = std::fs::metadata(raiz) else { continue };
        if !meta.is_dir() {
            continue;
        }
        let mut pilha: Vec<(PathBuf, usize)> = vec![(raiz.clone(), 0)];
        while let Some((pasta, profundidade)) = pilha.pop() {
            let Ok(leitura) = std::fs::read_dir(&pasta) else { continue };
            let mut itens: Vec<_> = leitura.filter_map(|e| e.ok()).collect();
            // Ordem estável: o mesmo disco tem de produzir o mesmo índice, senão
            // nenhuma medição de busca é comparável entre execuções.
            itens.sort_by_key(|e| e.file_name());
            for item in itens {
                let caminho = item.path();
                let Ok(meta) = item.metadata() else { continue };
                if meta.is_dir() {
                    if profundidade + 1 < PROFUNDIDADE_MAXIMA
                        && !pasta_ignorada(&item.file_name().to_string_lossy())
                    {
                        pilha.push((caminho, profundidade + 1));
                    }
                    continue;
                }
                if !meta.is_file()
                    || arquivo_ignorado(&item.file_name().to_string_lossy())
                    || meta.len() > LIMITE_BYTES_ARQUIVO
                {
                    continue;
                }
                if candidatos.len() >= LIMITE_ARQUIVOS || bytes + meta.len() > LIMITE_BYTES_TOTAIS {
                    completa = false;
                    return Varredura { candidatos, completa };
                }
                bytes += meta.len();
                let assinatura = assinar(&meta);
                candidatos.push((caminho, assinatura));
            }
        }
    }
    candidatos.sort_by(|a, b| a.0.cmp(&b.0));
    Varredura { candidatos, completa }
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progresso {
    pub concluidos: usize,
    pub total: usize,
    pub arquivo: String,
    pub trechos: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Resumo {
    pub arquivos: usize,
    pub relidos: usize,
    pub trechos: usize,
    /// Conversas que hoje têm trechos neste índice.
    pub conversas: usize,
    pub ms: u64,
    /// A varredura bateu num teto seu; o índice está certo, mas incompleto.
    pub truncado: bool,
    pub cancelado: bool,
}

/// O que a interface mostra. É tudo número lido do motor ou do disco.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Estado {
    /// Codificador escolhido em `rag.json`; vazio é o automático.
    pub escolhido: String,
    /// O que está em uso agora, resolvido contra o disco. Vazio é nada instalado.
    pub modelo: String,
    pub instalado: bool,
    /// Os pesos do codificador estão carregados nesta execução.
    pub carregado: bool,
    pub dispositivo: String,
    pub na_placa: bool,
    pub agrupamento: String,
    pub dimensao: usize,
    /// Tokens que atravessam o motor numa peça só — o teto real do lote.
    pub lote: usize,
    pub peca: u32,
    pub sobreposicao: u32,
    pub trechos: u32,
    pub piso: f32,
    /// A palavra de máquina do modo de busca escolhido.
    pub modo: &'static str,
    pub indexados: usize,
    pub arquivos: usize,
    /// Conversas salvas que hoje têm trechos no índice.
    pub conversas: usize,
    /// Dos `indexados`, quantos vieram de conversa e não de arquivo. É o número
    /// que diz se a memória de conversa está realmente montada.
    pub trechos_conversas: usize,
    /// Quantas camadas do codificador subiram para a placa, com o veredito que as
    /// contou e, quando não houve conta, o motivo. `None` é "nenhum codificador
    /// carregado nesta execução" — a mesma distinção que `Acelerador` faz no
    /// caminho de conversa: número medido e tentativa sem medida não se parecem.
    pub despacho: Option<Despacho>,
    /// Bytes do arquivo de índice, medidos no disco.
    pub bytes: u64,
    pub indexando: bool,
    pub progresso: Progresso,
    pub erro: Option<String>,
    pub concluido_em: Option<u64>,
    /// Por que a injeção automática está ou não está valendo. Máquina, para a
    /// interface traduzir: é estado, não explicação.
    pub auto: &'static str,
}

/// Os dois objetos que precisam andar juntos: o motor que conta o vetor e o
/// tokenizador que conta as peças. Um `Carregado` é um codificador, e a chave
/// dele é o id do modelo.
struct Carregado {
    ids: String,
    embutidor: Embutidor,
    tokenizador: Tokenizer,
}

/// O resumo do codificador carregado, para a tela ler sem encostar no `carregado`.
/// É uma cópia pequena e estável: só muda quando o motor entra ou sai da memória,
/// nunca durante a varredura. A varredura segura `carregado` do primeiro ao último
/// arquivo, então quem pergunta o estado pelo lock dele esperaria a varredura
/// inteira — a mesma UI congelada do task #40, agora pelo lado da memória. A tela
/// lê este espelho, que a varredura não toca.
#[derive(Debug, Clone)]
struct InfoMotor {
    dispositivo: String,
    na_placa: bool,
    agrupamento: String,
    dimensao: usize,
    lote: usize,
    /// O despacho com que este codificador foi carregado. Copiado do próprio
    /// `Embutidor` no mesmo bloco que copia o dispositivo: é a prova de onde o
    /// `n_gpu_layers` dele saiu, e sem ela a tela mostra a placa sem dizer se a
    /// carga foi medida ou tentada.
    despacho: Despacho,
}

/// O dono da memória. Vive atrás de um `Arc`, porque a varredura corre numa
/// tarefa bloqueante enquanto os comandos leem estado e a conversa busca.
pub struct Custodio {
    dlls: Vec<PathBuf>,
    models_root: PathBuf,
    caminho_indice: PathBuf,
    ajustes_path: PathBuf,
    pastas: Arc<RwLock<Pastas>>,
    ajustes: Mutex<Ajustes>,
    carregado: Mutex<Option<Carregado>>,
    /// O espelho de `carregado` para leitura sem o lock do motor. `Some` é
    /// exatamente quando `carregado` tem um codificador; as duas pontas se movem
    /// juntas em `carregado()` (carga) e nos dois `= None` (descarga).
    info_motor: RwLock<Option<InfoMotor>>,
    indice: RwLock<Option<Indice>>,
    /// Trava dos escritores do índice. O `RwLock` acima só protege a publicação
    /// final; quem mexe no índice faz ler-cópia → mudar → gravar-disco → publicar,
    /// e sem esta trava dois escritores concorrentes (um `revogar` e a revalidação
    /// de conversas de um turno) leem a mesma cópia e o último publicar ressuscita
    /// o conteúdo que o primeiro acabou de apagar, no disco e na memória. Ordem:
    /// `escritores` antes de `carregado` e de `indice`. A varredura não pega esta
    /// trava — ela já é exclusiva por `ocupado`, e pegá-la sob o `carregado` que
    /// segura o tempo todo fecharia um ciclo com quem escreve.
    escritores: Mutex<()>,
    ocupado: AtomicBool,
    cancelado: AtomicBool,
    progresso: RwLock<Progresso>,
    erro: Mutex<Option<String>>,
    concluido_em: Mutex<Option<u64>>,
}

/// Ordem dos locks, sem exceção: `carregado` antes de `indice`. Quem inverte
/// espera para sempre, e as duas pontas são pesadas o bastante para ninguém ver
/// o deadlock acontecer.
impl Custodio {
    pub fn novo(
        dlls: &[PathBuf],
        models_root: &Path,
        raiz_memoria: &Path,
        ajustes_path: PathBuf,
        pastas: Arc<RwLock<Pastas>>,
    ) -> Self {
        // Lido antes de o caminho virar campo do custódio: é o mesmo arquivo, e
        // depois de movido não há mais como lê-lo sem clonar.
        let ajustes = Ajustes::de(&ajustes_path);
        Self {
            dlls: dlls.to_vec(),
            models_root: models_root.to_path_buf(),
            caminho_indice: raiz_memoria.join("indice.aether"),
            ajustes_path,
            pastas,
            ajustes: Mutex::new(ajustes),
            carregado: Mutex::new(None),
            info_motor: RwLock::new(None),
            indice: RwLock::new(None),
            escritores: Mutex::new(()),
            ocupado: AtomicBool::new(false),
            cancelado: AtomicBool::new(false),
            progresso: RwLock::new(Progresso::default()),
            erro: Mutex::new(None),
            concluido_em: Mutex::new(None),
        }
    }

    pub fn ajustes(&self) -> Ajustes {
        trava_estado(&self.ajustes).clone()
    }

    /// Grava a escolha e derruba o que depender dela. Trocar de codificador
    /// solta os pesos e invalida o índice em memória; o do disco é recusado na
    /// leitura seguinte, porque a metadata diz outro modelo.
    pub fn ajustar(&self, novos: Ajustes) -> Result<Ajustes> {
        let novos = novos.aprovar();
        let antes = self.ajustes();
        novos.salvar(&self.ajustes_path)?;
        *trava_estado(&self.ajustes) = novos.clone();
        if novos.trocou_codificador(&antes) {
            *trava_estado(&self.carregado) = None;
            *trava_escrita(&self.info_motor) = None;
            *trava_escrita(&self.indice) = None;
        }
        Ok(novos)
    }

    pub fn esta_indexando(&self) -> bool {
        self.ocupado.load(Ordering::Relaxed)
    }

    /// Interrompe a varredura em curso. Devolve `true` só se havia uma: cancelar
    /// nada é diferente de cancelar alguma.
    pub fn cancelar(&self) -> bool {
        if !self.esta_indexando() {
            return false;
        }
        self.cancelado.store(true, Ordering::Relaxed);
        true
    }

    /// O codificador que vale agora: a escolha do dono, se ele escolheu e o
    /// arquivo está completo, senão o primeiro do catálogo de embeddings que
    /// estiver instalado.
    pub fn spec(&self) -> Option<&'static ModelSpec> {
        let escolhido = self.ajustes().modelo;
        if !escolhido.is_empty() {
            let spec = catalog::find_embeddings(&escolhido)?;
            return catalog::status(&self.models_root, spec)
                .complete
                .then_some(spec);
        }
        catalog::embeddings_instalados(&self.models_root)
    }

    pub fn apagar(&self) -> Result<()> {
        if self.esta_indexando() {
            bail!(tr("erros.memoriaOcupada", &[]));
        }
        *trava_escrita(&self.indice) = None;
        *trava_escrita(&self.progresso) = Progresso::default();
        *trava_estado(&self.concluido_em) = None;
        match std::fs::remove_file(&self.caminho_indice) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(anyhow::anyhow!(tr(
                "erros.falhaApagar",
                &[("caminho", &self.caminho_indice.display().to_string()), ("detalhe", &e.to_string())]
            ))),
        }
    }

    /// Solta um codificador para que os pesos possam sair do disco: no Windows
    /// um GGUF mapeado em memória não é apagado, e o llama.cpp mapeia. O índice
    /// gravado fica onde está — apagar a escolha de um modelo não apaga o
    /// trabalho de ler as pastas.
    pub fn soltar(&self, id: &str) -> Result<bool> {
        let era = {
            let mut guard = trava_estado(&self.carregado);
            let era = guard.as_ref().map(|c| c.ids == id).unwrap_or(false);
            if era {
                if self.esta_indexando() {
                    // Os pesos estão sendo lidos pela varredura neste instante.
                    bail!(tr("erros.memoriaIndexando", &[]));
                }
                *guard = None;
                *trava_escrita(&self.info_motor) = None;
            }
            era
        };
        if era {
            *trava_escrita(&self.indice) = None;
        }
        Ok(era)
    }

    /// Carrega o codificador agora, para o "Usar" da tela de modelos não
    /// deixar a primeira busca esperar pelos pesos. O guard sai descartado: o
    /// que importava era o que ficou em `carregado`.
    pub fn aquecer(&self) -> Result<()> {
        // Solto de propósito: o que importava era o codificador ter entrado em
        // `carregado`, e segurá-lo até o fim da função só ocuparia o lock.
        drop(self.carregado()?);
        Ok(())
    }

    fn modelo_de(&self, spec: &'static ModelSpec) -> Result<Carregado> {
        let dir = catalog::model_dir(&self.models_root, spec);
        let caminho_tokenizador = dir.join("tokenizer.json");
        let tokenizador = Tokenizer::from_file(&caminho_tokenizador).map_err(|e| {
            anyhow::anyhow!(tr(
                "erros.falhaAbrirTokenizador",
                &[
                    ("caminho", &caminho_tokenizador.display().to_string()),
                    ("detalhe", &e.to_string())
                ]
            ))
        })?;
        // A janela pedida é o tamanho da peça: pedir os 32k que o modelo aceita
        // reservaria gigabytes de KV cache para ler trezentos tokens.
        let janela = self.ajustes().peca.min(spec.context_limit as u32);
        let embutidor = Embutidor::novo(&self.dlls, &dir.join(spec.weights_file), janela)?;
        Ok(Carregado { ids: spec.id.to_string(), embutidor, tokenizador })
    }

    /// O codificador em memória, carregado se preciso. Devolve o guard: quem o
    /// segura é o dono exclusivo dos pesos enquanto durar a peça.
    fn carregado(&self) -> Result<std::sync::MutexGuard<'_, Option<Carregado>>> {
        let spec = self.spec().ok_or_else(|| anyhow::anyhow!(tr("erros.memoriaSemModelo", &[])))?;
        let alvo = self.ajustes().peca as usize;
        let mut guard = trava_estado(&self.carregado);
        // A peça escolhida maior que a janela que o motor aceitou mudaria o teto
        // real do lote: recarrega, em vez de embutir peça cortada.
        let servindo = guard
            .as_ref()
            .map(|c| c.ids == spec.id && c.embutidor.janela >= alvo)
            .unwrap_or(false);
        if !servindo {
            *guard = Some(self.modelo_de(spec)?);
            // O espelho acompanha a carga: só aqui um codificador entra no motor,
            // então registrar o fato uma vez, sob o mesmo guard, basta. A ordem é
            // `carregado` antes de `info_motor`; `estado()` só lê `info_motor`, e
            // ninguém pega `info_motor` antes de `carregado`, então não há ciclo.
            if let Some(c) = guard.as_ref() {
                *trava_escrita(&self.info_motor) = Some(InfoMotor {
                    dispositivo: c.embutidor.dispositivo.clone(),
                    na_placa: c.embutidor.na_placa,
                    agrupamento: c.embutidor.agrupamento.to_string(),
                    dimensao: c.embutidor.dimensao(),
                    lote: c.embutidor.limite(),
                    despacho: c.embutidor.despacho.clone(),
                });
            }
        }
        Ok(guard)
    }

    /// O índice que está no disco, recusado quando é de outro codificador ou de
    /// outra largura: vetores de espaços diferentes não são comparáveis, e aceitar
    /// um seria calcular vizinhança sobre ruído.
    fn do_disco(&self, spec: &ModelSpec, dimensao: Option<usize>) -> Option<Indice> {
        let indice = Indice::carregar(&self.caminho_indice)?;
        if indice.modelo() != spec.id {
            return None;
        }
        if indice.dimensao() == 0 || dimensao.is_some_and(|d| d != indice.dimensao()) {
            return None;
        }
        Some(indice)
    }

    /// Uma cópia do índice, para quem vai escrever nele. Os caminhos quentes —
    /// busca e tela — não passam por aqui: clonar doze mil vetores de mil
    /// dimensões custa quarenta e nove megabytes por consulta.
    fn indice_na_memoria(&self) -> Option<Indice> {
        if let Some(indice) = trava_leitura(&self.indice).as_ref() {
            return Some(copiar(indice));
        }
        let spec = self.spec()?;
        let indice = self.do_disco(spec, None)?;
        let devolvido = copiar(&indice);
        *trava_escrita(&self.indice) = Some(indice);
        Some(devolvido)
    }

    /// Busca com o índice travado só para leitura. As pastas são lidas vivas,
    /// no momento da busca: revogar uma pasta tira o conteúdo dela da resposta
    /// antes de tirar do índice.
    fn procurar(&self, busca: &Busca) -> Result<Resposta> {
        let raizes = trava_leitura(&self.pastas).raizes.clone();
        {
            let guard = trava_leitura(&self.indice);
            if let Some(indice) = guard.as_ref() {
                return indice.buscar(busca, &raizes);
            }
        }
        let spec = self
            .spec()
            .ok_or_else(|| anyhow::anyhow!(tr("erros.memoriaSemModelo", &[])))?;
        let indice = self
            .do_disco(spec, Some(busca.vetor.len()))
            .ok_or_else(|| anyhow::anyhow!(tr("erros.memoriaSemIndice", &[])))?;
        let resposta = indice.buscar(busca, &raizes)?;
        *trava_escrita(&self.indice) = Some(indice);
        Ok(resposta)
    }

    /// Um trecho é `Some` quando `indice` já tem alguma coisa na memória; sem
    /// escritor concorrente é o que vale. Esta função é a única porta de escrita
    /// no índice para quem não é a varredura: pega a trava dos escritores, lê a
    /// cópia, aplica a mudança, grava no disco e publica — tudo sob a mesma trava.
    /// O `alterar` devolve quantas peças mudou; zero significa "nada a gravar",
    /// e o índice nem sai da função.
    fn escrever_indice(
        &self,
        alterar: impl FnOnce(&mut Indice) -> usize,
    ) -> Result<usize> {
        let _trava = trava_estado(&self.escritores);
        let Some(mut indice) = self.indice_na_memoria() else {
            return Ok(0);
        };
        let feitos = alterar(&mut indice);
        if feitos > 0 {
            indice.salvar(&self.caminho_indice)?;
            *trava_escrita(&self.indice) = Some(indice);
        }
        Ok(feitos)
    }

    /// Revogar uma pasta apaga o que ela continha do índice. Não é isto que
    /// protege o conteúdo — a busca já recusa o que está fora das pastas
    /// concedidas — é o espaço que volta. Com a varredura em curso ela mesma
    /// apaga, porque regrava o índice no fim.
    pub fn revogar(&self) -> Result<usize> {
        if self.esta_indexando() {
            return Ok(0);
        }
        let raizes = trava_leitura(&self.pastas).raizes.clone();
        self.escrever_indice(|indice| indice.reter_dentro(&raizes))
    }

    /// O que o índice diz sobre si, sem copiar um vetor sequer: a tela de
    /// memória lê isto num laço de desenho, e quarenta e nove megabytes por
    /// quadro não é estado — é vazamento.
    fn ler_indice(&self) -> Leitura {
        {
            let guard = trava_leitura(&self.indice);
            if let Some(indice) = guard.as_ref() {
                return leitura(indice);
            }
        }
        let Some(spec) = self.spec() else { return Leitura::default() };
        self.do_disco(spec, None).as_ref().map(leitura).unwrap_or_default()
    }

    /// O vetor da pergunta. A consulta passa pelo prefixo que o modelo declara
    /// — é assim que ele foi treinado — e é cortada no lote, porque a pergunta
    /// que cabe é a que vira vetor.
    fn vetor_da_consulta(&self, texto: &str, prefixo: Option<&'static str>) -> Result<Vec<f32>> {
        let mut guard = self.carregado()?;
        let carregado = guard.as_mut().expect("carregado acabou de ser garantido");
        let com_prefixo = match prefixo {
            Some(prefixo) => format!("{prefixo}{texto}"),
            None => texto.to_string(),
        };
        let limite = carregado.embutidor.limite();
        let mut ids = codificar(&carregado.tokenizador, &com_prefixo)?;
        ids.truncate(limite);
        let mut vetor = carregado.embutidor.embutir(&ids)?;
        Indice::normalizar(&mut vetor)?;
        Ok(vetor)
    }

    /// Põe as conversas salvas dentro do índice. É o que faz a memória chegar ao
    /// que foi dito em outra sessão sem que ninguém precise varrer pasta
    /// nenhuma: o histórico do próprio aplicativo entra no mesmo índice híbrido
    /// dos arquivos, com a mesma busca e o mesmo cartão.
    ///
    /// O turno comum não paga nada por isto: a pendência é lida só na metadata,
    /// e enquanto a prova de cada conversa — contagem de mensagens e instante da
    /// última — bater, nenhum vetor é recalculado. Quando há trabalho, ele cabe em
    /// `TOKENS_POR_TURNO`, e uma conversa só entra inteira: metade de um papo no
    /// índice é um lembrado pior que nenhum.
    pub fn revalidar_conversas(&self, vivas: &[ConversaViva]) -> Result<usize> {
        let resultado = self.fundir_conversas(vivas, TOKENS_POR_TURNO);
        if let Err(erro) = &resultado {
            // O turno continua sem a lembrança nova, e é por isso que o erro não
            // pode sumir: ele fica onde a tela de memória já mostra os outros, e
            // diz por que o índice está atrás do histórico.
            *trava_estado(&self.erro) = Some(format!("{erro:#}"));
        }
        resultado
    }

    fn fundir_conversas(&self, vivas: &[ConversaViva], orcamento: usize) -> Result<usize> {
        if self.esta_indexando() {
            // A varredura segura o índice e o regrava no fim. Escrever por cima
            // agora seria perder o trabalho dela.
            return Ok(0);
        }
        // Daqui para baixo há escrita no índice: a mesma trava que separa dois
        // `revogar`/revalidações concorrentes, segurada do ler-cópia ao publicar.
        // Ordem `escritores → carregado → indice`; a varredura não pega esta
        // trava, então não há como fechar um ciclo contra ela.
        let _trava = trava_estado(&self.escritores);
        let Some(spec) = self.spec() else {
            // Sem codificador não há vetor, e sem vetor a conversa não entra. A
            // tela já diz isto por `auto`; aqui basta não fazer nada.
            return Ok(0);
        };
        let pendente = {
            let guard = trava_leitura(&self.indice);
            match guard.as_ref() {
                Some(indice) => indice.conversas_pendentes(vivas),
                None => vivas.len(),
            }
        };
        if pendente == 0 {
            return Ok(0);
        }
        // Daqui para baixo há trabalho, e o `carregado` é o mesmo da varredura:
        // por isso a ordem dos locks — carregado antes de índice — vale também.
        let mut trava = self.carregado()?;
        let carregado = trava.as_mut().expect("carregado acabou de ser garantido");
        let ajustes = self.ajustes();
        let mut indice = self.indice_na_memoria().unwrap_or_else(|| {
            Indice::vazio(
                spec.id,
                carregado.embutidor.dimensao(),
                carregado.embutidor.agrupamento,
                ajustes.peca,
                ajustes.sobreposicao,
            )
        });
        if indice.refatiar(ajustes.peca, ajustes.sobreposicao) {
            // Escala da tela diferente da escala gravada: as peças de conversa
            // foram cortadas na antiga também. A gravação imediata é o que faz
            // um índice esvaziado sobreviver mesmo se o turno cair no meio.
            indice.salvar(&self.caminho_indice)?;
            *trava_escrita(&self.indice) = Some(copiar(&indice));
        }
        let mudadas = self.escrever_conversas(&mut indice, carregado, vivas, orcamento)?;
        if mudadas > 0 {
            indice.salvar(&self.caminho_indice)?;
            *trava_escrita(&self.indice) = Some(indice);
        }
        Ok(mudadas)
    }

    /// O trabalho em si, com o codificador e o índice já na mão. Existe separado
    /// de `fundir_conversas` porque a varredura já segura os dois: quem tem os
    /// dois travados e chama `carregado()?` de novo morre ali, e o mutex não é
    /// reentrante.
    ///
    /// Devolve quantas conversas mudaram de estado — as reescritas mais as que
    /// saíram porque o histórico as apagou ou a janela as deixou para trás.
    fn escrever_conversas(
        &self,
        indice: &mut Indice,
        carregado: &mut Carregado,
        vivas: &[ConversaViva],
        orcamento: usize,
    ) -> Result<usize> {
        let (alvo, sobre) = indice.fatiamento();
        let (alvo, sobre) = (alvo as usize, sobre as usize);
        let limite = carregado.embutidor.limite();
        // Primeiro sai o que não é mais lista: apagar um papo apaga a lembrança
        // dele, e tirar da memória não custa um vetor.
        let mut mudadas = indice.reter_conversas(vivas);
        let mut gastos = 0usize;
        for viva in vivas {
            let prova = viva.prova();
            if indice.conversa_vista(&viva.id, &prova) {
                continue;
            }
            if indice.trechos() >= LIMITE_TRECHOS {
                // O teto de trechos do índice vale para as duas fontes: com a
                // casa cheia, nem fatiar — o custo seria tokenizar para nada.
                break;
            }
            let lidas = pecas(&viva.texto, alvo, sobre, limite, &carregado.tokenizador)?;
            let tokens: usize = lidas.iter().map(|p| p.ids.len()).sum();
            if acabou_a_verba(gastos, tokens, orcamento) {
                // O orçamento do turno acabou. Sair sem gravar a ficha é o que
                // faz esta conversa ser a próxima, e não uma repetição da que já
                // entrou: nenhuma fica pela metade.
                break;
            }
            // Quantas peças o fatiamento produziu: se entrarem menos que isso, o
            // teto cortou a conversa no meio e a ficha precisa dizer a verdade.
            let total_lidas = lidas.len();
            let mut prontas: Vec<(Peca, Vec<f32>)> = Vec::with_capacity(lidas.len());
            for peca in lidas {
                if indice.trechos() + prontas.len() >= LIMITE_TRECHOS {
                    break;
                }
                let mut vetor = carregado.embutidor.embutir(&peca.ids)?;
                Indice::normalizar(&mut vetor)?;
                prontas.push((peca, vetor));
            }
            // A prova cobre só o que de fato foi vetorado. Se o teto de trechos
            // cortou a conversa no meio do fatiamento, a ficha fica marcada como
            // truncada: assim ela não é dada por inteira (o que a esconderia da
            // fila para sempre) e volta a ser tentada quando abrir espaço, em vez
            // de prometer um texto que não cabe.
            indice.substituir_conversa(viva, prova, &prontas, prontas.len() < total_lidas);
            gastos += tokens;
            mudadas += 1;
        }
        Ok(mudadas)
    }

    /// A busca em si: o chão do índice, o vetor da pergunta, os `k` vizinhos e as
    /// conversas que ela não pode responder.
    pub fn buscar(&self, consulta: &str, trechos: u32, ignorar: &[String]) -> Result<Resposta> {
        let consulta = consulta.trim();
        let ajustes = self.ajustes();
        if consulta.is_empty() {
            return Ok(Resposta {
                achados: Vec::new(),
                alcancados: 0,
                modo: ajustes.modo.nome(),
            });
        }
        if self.esta_indexando() {
            // Bloquear aqui seria a conversa inteira esperando a varredura. O
            // modelo recebe o estado real e pode dizer isso ao usuário.
            bail!(tr("erros.memoriaIndexando", &[]));
        }
        let spec = self
            .spec()
            .ok_or_else(|| anyhow::anyhow!(tr("erros.memoriaSemModelo", &[])))?;
        let k = if trechos == 0 { ajustes.trechos } else { trechos.min(TRECHOS_MAX) } as usize;
        let vetor = self.vetor_da_consulta(consulta, spec.query_prefix)?;
        self.procurar(&Busca {
            vetor: &vetor,
            texto: consulta,
            k,
            piso: ajustes.piso,
            modo: ajustes.modo,
            ignorar,
        })
    }

    /// O texto que o modelo lê: cabeçalho e trechos, cada um com a origem de
    /// onde ele saiu — caminho e linha num arquivo, conversa e mensagem numa
    /// lembrança de papo — para a citação sair certa.
    pub fn texto_do_bloco(achados: &[Achado]) -> String {
        let mut saida = String::from(tr("memoria.blocoCabecalho", &[]).as_str());
        for (numero, achado) in achados.iter().enumerate() {
            let origem = match achado.fonte {
                Fonte::Arquivo => format!("{}:{}", achado.arquivo, achado.linha),
                Fonte::Conversa => tr(
                    "memoria.blocoConversa",
                    &[("titulo", &achado.nome), ("linha", &achado.linha.to_string())],
                ),
            };
            saida.push_str(&format!(
                "\n[{}] {}\n{}",
                numero + 1,
                origem,
                achado.texto.trim_end()
            ));
        }
        saida
    }

    pub fn progresso(&self) -> Progresso {
        trava_leitura(&self.progresso).clone()
    }

    /// Tudo que a tela de memória mostra, lido agora.
    pub fn estado(&self, fio_ligado: bool) -> Estado {
        let ajustes = self.ajustes();
        let spec = self.spec();
        // Lemos o espelho, nunca o `carregado`: a varredura segura o lock do
        // motor do primeiro ao último arquivo, e esta função é chamada no loop da
        // tela. Pedir o lock aqui congelaria a UI enquanto a varredura corre.
        let vivo = trava_leitura(&self.info_motor).clone();
        let disco = self.ler_indice();
        let indexados = disco.trechos;
        Estado {
            escolhido: ajustes.modelo.clone(),
            modelo: spec.map(|s| s.id.to_string()).unwrap_or_default(),
            instalado: spec.is_some(),
            carregado: vivo.is_some(),
            dispositivo: vivo.as_ref().map(|c| c.dispositivo.clone()).unwrap_or_default(),
            na_placa: vivo.as_ref().map(|c| c.na_placa).unwrap_or(false),
            agrupamento: vivo.as_ref().map(|c| c.agrupamento.clone()).unwrap_or(disco.agrupamento),
            dimensao: vivo.as_ref().map(|c| c.dimensao).unwrap_or(disco.dimensao),
            lote: vivo.as_ref().map(|c| c.lote).unwrap_or(0),
            peca: ajustes.peca,
            sobreposicao: ajustes.sobreposicao,
            trechos: ajustes.trechos,
            piso: ajustes.piso,
            modo: ajustes.modo.nome(),
            indexados,
            arquivos: disco.arquivos,
            conversas: disco.conversas,
            trechos_conversas: disco.trechos_conversas,
            despacho: vivo.as_ref().map(|c| c.despacho.clone()),
            bytes: std::fs::metadata(&self.caminho_indice).map(|m| m.len()).unwrap_or(0),
            indexando: self.esta_indexando(),
            progresso: self.progresso(),
            erro: trava_estado(&self.erro).clone(),
            concluido_em: *trava_estado(&self.concluido_em),
            auto: if !fio_ligado {
                "semFio"
            } else if ajustes.trechos == 0 {
                "trechosZero"
            } else if spec.is_none() {
                "semModelo"
            } else if indexados == 0 {
                "indiceVazio"
            } else if disco.peca != ajustes.peca || disco.sobreposicao != ajustes.sobreposicao {
                // O índice gravado é de outra escala. A busca continua rodando
                // com o que está lá, mas o número na tela não é o fatiamento que
                // o dono escolheu — e dizer isso é o que falta para o "varrer"
                // ser uma consequência, não uma sugestão.
                "fatiamentoNovo"
            } else {
                "ligada"
            },
        }
    }

    /// Constrói ou atualiza o índice das pastas concedidas e das conversas
    /// salvas. Bloqueante: quem chama roda isto fora do runtime assíncrono.
    pub fn indexar(&self, conversas: &[ConversaViva], mut emitir: impl FnMut(Evento)) -> Result<Resumo> {
        if self.ocupado.swap(true, Ordering::SeqCst) {
            bail!(tr("erros.memoriaOcupada", &[]));
        }
        self.cancelado.store(false, Ordering::Relaxed);
        *trava_estado(&self.erro) = None;
        let inicio = Instant::now();
        *trava_escrita(&self.progresso) = Progresso {
            concluidos: 0,
            total: 0,
            arquivo: String::new(),
            trechos: 0,
        };
        let resultado = self.varrer_e_embutir(conversas, &mut emitir);
        self.ocupado.store(false, Ordering::SeqCst);
        match resultado {
            Ok(mut resumo) => {
                resumo.ms = inicio.elapsed().as_millis() as u64;
                *trava_estado(&self.concluido_em) = Some(agora_ms());
                let cancelado = resumo.cancelado;
                emitir(if cancelado {
                    Evento::Cancelado { resumo: resumo.clone() }
                } else {
                    Evento::Concluido { resumo: resumo.clone() }
                });
                Ok(resumo)
            }
            Err(erro) => {
                let texto = format!("{erro:#}");
                *trava_estado(&self.erro) = Some(texto.clone());
                emitir(Evento::Falhou { erro: texto });
                Err(erro)
            }
        }
    }

    fn varrer_e_embutir(
        &self,
        conversas: &[ConversaViva],
        emitir: &mut dyn FnMut(Evento),
    ) -> Result<Resumo> {
        let spec = self.spec().ok_or_else(|| anyhow::anyhow!(tr("erros.memoriaSemModelo", &[])))?;
        let raizes = trava_leitura(&self.pastas).raizes.clone();
        if raizes.is_empty() && conversas.is_empty() {
            // Nada para ler em lugar nenhum. Mas conversa salva já é memória:
            // sem pasta concedida o varrer ainda lembrado o histórico.
            bail!(tr("erros.semPastas", &[]));
        }
        let Varredura { candidatos, completa } = varrer(&raizes);
        let ajustes = self.ajustes();
        let mut guard = self.carregado()?;
        let carregado = guard.as_mut().expect("carregado acabou de ser garantido");
        let dimensao = carregado.embutidor.dimensao();
        let limite = carregado.embutidor.limite();
        let alvo = ajustes.peca as usize;
        let sobre = ajustes.sobreposicao as usize;

        let mut indice = self
            .indice_na_memoria()
            .filter(|i| i.modelo() == spec.id && i.dimensao() == dimensao)
            .unwrap_or_else(|| {
                Indice::vazio(
                    spec.id,
                    dimensao,
                    carregado.embutidor.agrupamento,
                    alvo as u32,
                    sobre as u32,
                )
            });
        if indice.refatiar(alvo as u32, sobre as u32) {
            // A escala da tela não é a escala gravada: cada vetor do índice
            // antigo representa outro pedaço de texto, e as assinaturas foram
            // apagadas junto para a varredura abaixo reler tudo. Um índice que
            // sobrevivesse a isto é exatamente o controle mentiroso que o
            // produto proíbe.
            indice.salvar(&self.caminho_indice)?;
        }
        // Uma pasta revogada no grafo tem de tirar o conteúdo dela da memória, e
        // isto vale mesmo com a varredura cortada por um teto.
        indice.reter_dentro(&raizes);

        let vistos: Vec<String> = candidatos
            .iter()
            .map(|(caminho, _)| caminho.display().to_string())
            .collect();
        if completa {
            // Arquivo que saiu da lista e continua no índice foi apagado ou
            // passou a ser ignorado. Só se tem certeza disso varrendo tudo.
            let fora: Vec<String> = indice
                .meta
                .assinaturas
                .keys()
                .filter(|arquivo| !vistos.iter().any(|vista| vista == *arquivo))
                .cloned()
                .collect();
            for arquivo in fora {
                indice.remover_arquivo(&arquivo);
                indice.meta.assinaturas.remove(&arquivo);
            }
        }

        let total = candidatos.len();
        let mut relidos = 0usize;
        let mut truncado = false;
        let mut cancelado = false;
        {
            let mut p = trava_escrita(&self.progresso);
            p.total = total;
        }

        for (numero, (caminho, assinatura)) in candidatos.into_iter().enumerate() {
            if self.cancelado.load(Ordering::Relaxed) {
                cancelado = true;
                break;
            }
            let chave = caminho.display().to_string();
            if indice.esta_visto(&chave, &assinatura) {
                let mut p = trava_escrita(&self.progresso);
                p.concluidos = numero + 1;
                p.arquivo = chave.clone();
                continue;
            }
            let texto = match std::fs::read_to_string(&caminho) {
                Ok(texto) => texto,
                // Arquivo que sumiu ou que não abriu: a próxima varredura tenta
                // de novo, porque nada aqui foi provado sobre ele.
                Err(_) => {
                    let mut p = trava_escrita(&self.progresso);
                    p.concluidos = numero + 1;
                    continue;
                }
            };
            indice.remover_arquivo(&chave);
            let lidas = pecas(&texto, alvo, sobre, limite, &carregado.tokenizador)?;
            let mut prontas: Vec<(Peca, Vec<f32>)> = Vec::with_capacity(lidas.len());
            for peca in lidas {
                if indice.trechos() + prontas.len() >= LIMITE_TRECHOS {
                    truncado = true;
                    break;
                }
                let mut vetor = carregado.embutidor.embutir(&peca.ids)?;
                Indice::normalizar(&mut vetor)?;
                prontas.push((peca, vetor));
            }
            indice.adicionar(&chave, assinatura, &prontas);
            relidos += 1;
            {
                let mut p = trava_escrita(&self.progresso);
                p.concluidos = numero + 1;
                p.arquivo = chave.clone();
                p.trechos = indice.trechos();
            }
            emitir(Evento::Progresso {
                arquivo: chave,
                concluidos: numero + 1,
                total,
                trechos: indice.trechos(),
            });
            if numero > 0 && numero % SALVAR_A_CADA == 0 {
                {
                    let mut guard = trava_escrita(&self.indice);
                    *guard = Some(copiar(&indice));
                }
                indice.salvar(&self.caminho_indice)?;
            }
            if truncado {
                break;
            }
        }

        // Uma pasta revogada no meio da varredura não pode voltar pelo arquivo
        // nem continuar em memória: a última palavra é da lista viva de
        // concessões, e ela vem depois de todo o trabalho.
        let vivas = trava_leitura(&self.pastas).raizes.clone();
        indice.reter_dentro(&vivas);

        // O varrer é também o ponto em que o histórico inteiro entra na memória,
        // sem o orçamento de um turno: aqui o codificador já está na mão, o
        // trabalho corre fora da conversa e a barra de progresso existe justamente
        // para o dono ver o índice subir.
        self.escrever_conversas(&mut indice, carregado, conversas, usize::MAX)?;

        let resumo = Resumo {
            arquivos: indice.arquivos(),
            relidos,
            trechos: indice.trechos(),
            conversas: indice.conversas(),
            ms: 0,
            truncado: truncado || !completa,
            cancelado,
        };
        {
            let mut guard = trava_escrita(&self.indice);
            *guard = Some(copiar(&indice));
        }
        indice.salvar(&self.caminho_indice)?;
        Ok(resumo)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Evento {
    Progresso { arquivo: String, concluidos: usize, total: usize, trechos: usize },
    Concluido { resumo: Resumo },
    Cancelado { resumo: Resumo },
    Falhou { erro: String },
}

fn agora_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod testes {
    use super::*;

    /// WordLevel com Whitespace: uma palavra é um token, e a contagem do teste
    /// não depende de arquivo de modelo nenhum.
    fn tokenizador() -> Tokenizer {
        // O valor é montado como JSON, não como texto cru: o prefixo do
        // WordPiece é `##`, e a sequência `"#` fecharia um literal cru no meio
        // da frase — cada chave virada de `{` para `{{` era um jeito de empurrar
        // o problema para outro lugar.
        let vocab: serde_json::Map<String, serde_json::Value> =
            (0..40).map(|i| (format!("p{i}"), serde_json::Value::from(i))).collect();
        let json = serde_json::json!({
            "version": "1.0",
            "truncation": null,
            "padding": null,
            "added_tokens": [],
            "normalizer": null,
            "pre_tokenizer": { "type": "Whitespace" },
            "post_processor": null,
            "decoder": { "type": "WordPiece", "prefix": "##", "cleanup": false },
            "model": { "type": "WordLevel", "vocab": vocab, "unk_token": "[UNK]" },
        });
        Tokenizer::from_bytes(json.to_string().as_bytes()).expect("tokenizer de teste")
    }

    fn linha(prefixo: &str, n: usize) -> String {
        (0..n).map(|i| format!("{prefixo}{i}")).collect::<Vec<_>>().join(" ")
    }

    /// A busca só pelo vetor, que é o que os testes de proximidade medem. O
    /// texto da pergunta fica em branco porque o que se verifica aqui é o cosseno
    /// e o piso, não a palavra.
    fn por_vetor(vetor: &[f32], k: usize, piso: f32) -> Busca<'_> {
        Busca { vetor, texto: "", k, piso, modo: Modo::Vetorial, ignorar: &[] }
    }

    #[test]
    fn nenhuma_peca_passa_do_lote() {
        let tok = tokenizador();
        let texto = linha("p", 30);
        let pecas = pecas(&texto, 8, 0, 8, &tok).unwrap();
        assert!(!pecas.is_empty());
        for peca in &pecas {
            assert!(peca.ids.len() <= 8, "{:?}", peca.ids.len());
        }
        // Sem quebra de linha nenhuma, 30 tokens em lotes de 8 fazem 4 peças.
        assert_eq!(pecas.len(), 4);
    }

    #[test]
    fn a_peca_quebra_na_fronteira_da_linha() {
        let tok = tokenizador();
        let texto = ["p0 p1 p2", "p3 p4 p5", "p6 p7 p8"].join("\n");
        // Três tokens por linha, alvo de quatro: duas linhas jamais caberiam
        // juntas, então cada peça é uma linha e nenhuma linha é cortada ao meio.
        let pecas = pecas(&texto, 4, 0, 8, &tok).unwrap();
        assert_eq!(pecas.len(), 3);
        assert_eq!(pecas[0].texto, "p0 p1 p2");
        assert_eq!(pecas[0].linha, 1);
        assert_eq!(pecas[1].texto, "p3 p4 p5");
        assert_eq!(pecas[1].linha, 2);
        assert_eq!(pecas[2].texto, "p6 p7 p8");
        assert_eq!(pecas[2].linha, 3);
    }

    #[test]
    fn a_sobreposicao_repete_a_cauda_na_peca_seguinte() {
        let tok = tokenizador();
        let texto = ["p0 p1 p2", "p3 p4 p5", "p6 p7 p8"].join("\n");
        // Alvo de oito, linhas de três: a terceira linha não cabe junto das duas
        // primeiras, então a primeira peça fecha com seis tokens. A sobreposição
        // pedida é de quatro, e a cauda entra por linha inteira — três tokens, a
        // linha que acabou de fechar, que é o que faz uma função cortada ao meio
        // aparecer inteira em uma das duas janelas.
        let pecas = pecas(&texto, 8, 4, 8, &tok).unwrap();
        assert_eq!(
            pecas.iter().map(|p| p.texto.as_str()).collect::<Vec<_>>(),
            vec!["p0 p1 p2\np3 p4 p5", "p3 p4 p5\np6 p7 p8"]
        );
        assert_eq!(pecas[0].linha, 1);
        // A peça repetida começa na linha repetida, não na linha seguinte.
        assert_eq!(pecas[1].linha, 2);
        // E o que se repetiu foi exatamente o mesmo par (texto, ids): a
        // sobreposição repete tokens, não inventa texto fora deles.
        assert_eq!(pecas[1].ids[..3], pecas[0].ids[3..]);
    }

    #[test]
    fn a_sobreposicao_respeita_o_teto_de_metade_da_peca() {
        let tok = tokenizador();
        let texto = ["p0 p1", "p2 p3", "p4 p5"].join("\n");
        // Sobreposição pedida maior que metade do alvo: o teto corta, senão a
        // última linha se repetiria para sempre e o índice encheria de cópia.
        let pecas = pecas(&texto, 4, 99, 4, &tok).unwrap();
        assert_eq!(
            pecas.iter().map(|p| p.texto.as_str()).collect::<Vec<_>>(),
            vec!["p0 p1\np2 p3", "p2 p3\np4 p5", "p4 p5"]
        );
        assert!(pecas.iter().all(|p| p.ids.len() <= 4));
    }

    #[test]
    fn a_peca_guarda_o_texto_exato_do_arquivo() {
        let tok = tokenizador();
        // Cabe no lote: uma peça só, e o texto dela é exatamente o que está no
        // arquivo — linhas em branco incluídas.
        let texto = "p0\n\np1 p2\np3";
        let uma = pecas(texto, 4, 0, 8, &tok).unwrap();
        assert_eq!(uma.len(), 1);
        assert_eq!(uma[0].texto, texto);
        assert_eq!(uma[0].linha, 1);
        // Lote de um token: as linhas em branco vão junto com a peça que já
        // abriu, e a peça seguinte começa na linha do token que ela contém.
        let de_um = pecas("p0\n\n\np1", 1, 0, 8, &tok).unwrap();
        assert_eq!(
            de_um.iter().map(|p| (p.texto.as_str(), p.linha)).collect::<Vec<_>>(),
            vec![("p0\n\n", 1), ("p1", 4)]
        );
        // Uma linha só, maior que o lote: cada fatia volta do tokenizador com o
        // texto dos próprios ids, nunca de um offset contado à mão.
        let fatias = pecas(&linha("p", 10), 8, 0, 4, &tok).unwrap();
        assert_eq!(fatias.iter().map(|p| p.texto.as_str()).collect::<Vec<_>>(),
                   vec!["p0 p1 p2 p3", "p4 p5 p6 p7", "p8 p9"]);
        assert!(fatias.iter().all(|p| p.linha == 1));
    }

    #[test]
    fn indice_grava_e_volta_com_os_mesmos_vetores() {
        let pasta = std::env::temp_dir().join(format!("aether-memoria-{}", std::process::id()));
        std::fs::create_dir_all(&pasta).unwrap();
        let arquivo = pasta.join("indice.aether");
        let mut indice = Indice::vazio("qwen3-embedding-0.6b-q8_0", 3, "último", 64, 32);
        let peca = Peca { linha: 1, texto: "p0 p1".into(), ids: vec![0, 1] };
        let mut vetor = vec![1.0f32, 2.0, 2.0];
        Indice::normalizar(&mut vetor).unwrap();
        indice.adicionar("C:/x/a.rs", Assinatura { bytes: 10, modificado: 20 }, &[(peca, vetor)]);
        indice.salvar(&arquivo).unwrap();

        let lido = Indice::carregar(&arquivo).expect("leu o índice");
        assert_eq!(lido.modelo(), "qwen3-embedding-0.6b-q8_0");
        assert_eq!(lido.agrupamento(), "último");
        assert_eq!(lido.trechos(), 1);
        assert_eq!(lido.meta.trechos[0].texto, "p0 p1");
        assert!(lido.esta_visto("C:/x/a.rs", &Assinatura { bytes: 10, modificado: 20 }));
        assert!(!lido.esta_visto("C:/x/a.rs", &Assinatura { bytes: 11, modificado: 20 }));
        assert_eq!(lido.vetores, indice.vetores);
        // A escala gravada junto: é ela que a varredura compara com o ajuste vivo
        // para saber se ainda vale o que está no disco.
        assert_eq!(lido.fatiamento(), (64, 32));
        std::fs::remove_dir_all(&pasta).ok();
    }

    #[test]
    fn um_arquivo_corrompido_nao_e_interpretado_torto() {
        let pasta = std::env::temp_dir().join(format!("aether-memoria-ruim-{}", std::process::id()));
        std::fs::create_dir_all(&pasta).unwrap();
        let arquivo = pasta.join("indice.aether");
        std::fs::write(&arquivo, b"OITI").unwrap();
        assert!(Indice::carregar(&arquivo).is_none());
        std::fs::remove_dir_all(&pasta).ok();
    }

    /// O M2 em forma de teste: um `dimensao` danificado no cabeçalho multiplicava
    /// `trechos * dimensao * 4` sem checagem. Em release o produto embrulha até
    /// casar com um blob curto e o índice carregava torto; em debug o próprio
    /// multiply entrava em pânico. Com aritmética checada é só "não sei": um
    /// cabeçalho que não cabe no blob volta a `None`, igual a um arquivo torto.
    #[test]
    fn uma_dimensao_que_estoura_a_conta_nao_carrega() {
        let pasta =
            std::env::temp_dir().join(format!("aether-memoria-estouro-{}", std::process::id()));
        std::fs::create_dir_all(&pasta).unwrap();
        let arquivo = pasta.join("indice.aether");
        let mut indice = Indice::vazio("m", 2, "último", 64, 0);
        for nome in ["a", "b", "c"] {
            indice.adicionar(
                &format!("C:/x/{nome}.txt"),
                Assinatura { bytes: 1, modificado: 1 },
                &[(Peca { linha: 1, texto: nome.into(), ids: vec![] }, vec![1.0, 0.0])],
            );
        }
        // Três peças caberiam numa dimensão 2 (blob de 24 bytes); o cabeçalho
        // mente uma dimensão gigante e o blob fica vazio. O produto embrulharia
        // para zero, igual ao tamanho do blob — é a colisão que a conta checada
        // agora recusa.
        indice.meta.dimensao = 1usize << 62;
        indice.vetores.clear();
        indice.salvar(&arquivo).unwrap();
        assert!(Indice::carregar(&arquivo).is_none(), "cabeçalho que estoura a conta não entra");
        std::fs::remove_dir_all(&pasta).ok();
    }

    /// O M1 em forma de teste: a varredura segura o lock do motor do primeiro ao
    /// último arquivo, e a tela chama `estado()` no loop dela. Se `estado()` pedir
    /// o lock do motor, a janela congela a varredura inteira (a classe do task
    /// #40). Com o espelho `info_motor`, `estado()` devolve mesmo com o motor
    /// ocupado. Sem o espelho, o `recv_timeout` abaixo estoura — vermelho antes
    /// da correção, verde depois.
    #[test]
    fn o_estado_nao_espera_o_motor_da_varredura() {
        let base = std::env::temp_dir().join(format!("aether-memoria-estado-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let pastas = Arc::new(RwLock::new(Pastas::default()));
        let custodio = Custodio::novo(
            &[],
            &base,
            &base,
            base.join("rag.json"),
            pastas,
        );

        let (tx, rx) = std::sync::mpsc::channel::<()>();
        std::thread::scope(|s| {
            // A varredura em andamento: o guard do motor fica tomado o tempo todo.
            let _guard = custodio.carregado.lock().expect("motor envenenado");
            s.spawn(|| {
                let _estado = custodio.estado(false);
                let _ = tx.send(());
            });
            let chegou = rx
                .recv_timeout(std::time::Duration::from_millis(500))
                .is_ok();
            assert!(chegou, "estado() deve ler o espelho, não o lock do motor");
        });
        std::fs::remove_dir_all(&base).ok();
    }

    /// O veredito do codificador chega à tela. O `Embutidor` carrega o
    /// `Despacho` desde #39, mas o espelho que `estado()` lê não tinha o campo:
    /// a chip dizia "na placa" sem distinguir as camadas contadas na VRAM livre
    /// das jogadas para cima por os pesos não terem sido lidos. Vermelho desta
    /// vez é compilação — o campo não existia, e não havia o que asserir.
    #[test]
    fn o_despacho_do_codificador_aparece_no_estado() {
        use crate::ai::alojamento;
        use crate::ai::ggml::ffi::CAMADAS_NA_GPU;

        let base =
            std::env::temp_dir().join(format!("aether-memoria-despacho-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let pastas = Arc::new(RwLock::new(Pastas::default()));
        let custodio = Custodio::novo(&[], &base, &base, base.join("rag.json"), pastas);

        // Sem codificador carregado não há veredito nenhum, e a tela não pode
        // inventar um: `None` é a resposta, não um zero ou um `-1`.
        assert!(
            custodio.estado(false).despacho.is_none(),
            "sem carga não há despacho para mostrar"
        );

        let espelho = |despacho: Despacho| InfoMotor {
            dispositivo: "AMD Radeon RX 9060 XT".to_string(),
            na_placa: true,
            agrupamento: "média".to_string(),
            dimensao: 768,
            lote: 512,
            despacho,
        };

        // A placa medida e a conta feita: o estado devolve o mesmo despacho, com
        // as camadas que a carga recebeu.
        let dividido = Despacho {
            camadas_gpu: 10,
            previsto: Some(alojamento::Alojamento::Dividida {
                camadas: 10,
                total: 24,
                na_placa: 4 << 30,
                em_ram: 2 << 30,
                estado: 0,
                livre: 5 << 30,
            }),
            motivo: None,
        };
        *custodio.info_motor.write().expect("espelho do motor envenenado") =
            Some(espelho(dividido.clone()));
        let lido = custodio
            .estado(false)
            .despacho
            .expect("o despacho do espelho chega ao estado");
        assert_eq!(lido, dividido, "o estado devolve o despacho carregado");

        // O nome que a frente lê: camelCase, com o `tipo` da variante. Pinado
        // aqui porque `types.ts` declara as três formas e o card escolhe a
        // palavra por elas.
        let json = serde_json::to_value(custodio.estado(false)).expect("estado serializa");
        assert_eq!(json["despacho"]["camadasGpu"], 10);
        assert_eq!(json["despacho"]["previsto"]["tipo"], "dividida");
        assert_eq!(json["despacho"]["previsto"]["emRam"], 2u64 << 30);

        // A conta que não pôde ser feita: o sentinela vai junto com o motivo
        // real. Perder o motivo é o que faria uma tentativa parecer medida.
        let sem_conta = Despacho {
            camadas_gpu: CAMADAS_NA_GPU,
            previsto: None,
            motivo: Some("o arquivo não declara block_count".to_string()),
        };
        *custodio.info_motor.write().expect("espelho do motor envenenado") =
            Some(espelho(sem_conta.clone()));
        let lido = custodio
            .estado(false)
            .despacho
            .expect("uma tentativa também é estado");
        assert_eq!(lido, sem_conta, "o motivo da conta impossível chega junto");
        std::fs::remove_dir_all(&base).ok();
    }

    /// O M5 em forma de teste: um `revogar` e a revalidação de conversas de um
    /// turno são os dois escritores do índice fora da varredura. Cada um lê uma
    /// cópia, muda, grava no disco e publica — e sem a trava de escritores o
    /// último publicar escreve por cima do primeiro e ressuscita o arquivo que o
    /// outro acabou de apagar. Com a trava o par é atômico: dos dois cortes nenhum
    /// some. Aqui um writer corta `a`, o outro corta `b`, e só `c` pode sobrar.
    #[test]
    fn dois_escritores_do_indice_nao_se_atropelam() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Barrier;

        let base = std::env::temp_dir()
            .join(format!("aether-memoria-escritores-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let pastas = Arc::new(RwLock::new(Pastas::default()));
        let custodio = Custodio::novo(&[], &base, &base, base.join("rag.json"), pastas);

        let semear = |custodio: &Custodio| {
            let mut indice = Indice::vazio("m", 1, "último", 64, 0);
            for nome in ["a", "b", "c"] {
                indice.adicionar(
                    &format!("C:/x/{nome}.rs"),
                    Assinatura { bytes: 1, modificado: 1 },
                    &[(Peca { linha: 1, texto: nome.into(), ids: vec![] }, vec![1.0])],
                );
            }
            *custodio.indice.write().expect("índice envenenado") = Some(indice);
        };

        let perdido = AtomicUsize::new(0);
        for _ in 0..150 {
            semear(&custodio);
            let conv = &custodio;
            let barreira = Barrier::new(3);
            std::thread::scope(|cena| {
                let b1 = &barreira;
                cena.spawn(move || {
                    b1.wait();
                    let _ = conv.escrever_indice(|i| i.remover_arquivo("C:/x/a.rs"));
                });
                let b2 = &barreira;
                cena.spawn(move || {
                    b2.wait();
                    let _ = conv.escrever_indice(|i| i.remover_arquivo("C:/x/b.rs"));
                });
                barreira.wait();
            });
            let guard = custodio.indice.read().expect("índice envenenado");
            let indice = guard.as_ref().expect("índice publicado");
            if indice.trechos() != 1 || indice.meta.trechos[0].arquivo != "C:/x/c.rs" {
                perdido.fetch_add(1, Ordering::Relaxed);
            }
        }

        assert_eq!(
            perdido.load(Ordering::Relaxed),
            0,
            "dois escritores do índice se atropelaram: um arquivo apagado voltou"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn a_busca_ordena_por_proximidade_e_aplica_o_piso() {
        let mut indice = Indice::vazio("m", 2, "último", 64, 0);
        for (nome, vetor) in [("norte", [1.0f32, 0.0]), ("leste", [0.0, 1.0])] {
            let peca = Peca { linha: 1, texto: nome.into(), ids: vec![] };
            let mut v = vetor.to_vec();
            Indice::normalizar(&mut v).unwrap();
            indice.adicionar(
                &format!("C:/concedida/{nome}.txt"),
                Assinatura { bytes: 1, modificado: 1 },
                &[(peca, v)],
            );
        }
        let concedida = [PathBuf::from("C:/concedida")];
        let consulta = [1.0f32, 0.0];
        let resposta = indice.buscar(&por_vetor(&consulta, 2, 0.0), &concedida).unwrap();
        assert_eq!(resposta.achados[0].texto, "norte");
        assert!((resposta.achados[0].pontuacao - 1.0).abs() < 1e-6);
        assert_eq!(resposta.achados[1].texto, "leste");
        assert_eq!(resposta.alcancados, 2);
        assert_eq!(resposta.modo, "vetorial");
        assert_eq!(resposta.achados[0].origem, Origem::Vetorial);
        // O piso corta o segundo antes do `k` chegar nele.
        let resposta = indice
            .buscar(&por_vetor(&consulta, 2, 0.5), &concedida)
            .unwrap();
        assert_eq!(resposta.achados.len(), 1);
        assert_eq!(resposta.alcancados, 1);
        // `k` menor que o número de aprovados: a lista corta, e o cartão precisa
        // poder dizer que cortou.
        let resposta = indice
            .buscar(&por_vetor(&consulta, 1, 0.0), &concedida)
            .unwrap();
        assert_eq!(resposta.achados.len(), 1);
        assert_eq!(resposta.alcancados, 2);
        assert_eq!(
            indice.buscar(&por_vetor(&consulta, 0, 0.0), &concedida).unwrap().achados.len(),
            0
        );
        // Uma pasta revogada no meio do caminho não devolve nada, nem com o
        // vetor dela ainda parado no índice: a política manda na leitura.
        let resposta = indice
            .buscar(&por_vetor(&consulta, 2, 0.0), &[PathBuf::from("C:/outra")])
            .unwrap();
        assert!(resposta.achados.is_empty());
        assert_eq!(resposta.alcancados, 0);
    }

    /// Índice com um trecho por linha de texto e o vetor pedido para cada um,
    /// todos dentro da mesma pasta concedida. Aqui a palavra e o vetor são
    /// medidas independentes de propósito: é assim que se vê qual das duas
    /// chamou cada achado.
    fn indice_com_palavras(trechos: &[(&str, [f32; 2])]) -> Indice {
        let mut indice = Indice::vazio("m", 2, "último", 64, 0);
        for (texto, vetor) in trechos {
            let peca = Peca { linha: 1, texto: texto.to_string(), ids: vec![] };
            let mut v = vetor.to_vec();
            Indice::normalizar(&mut v).unwrap();
            indice.adicionar(
                "C:/concedida/a.txt",
                Assinatura { bytes: 1, modificado: 1 },
                &[(peca, v)],
            );
        }
        indice
    }

    #[test]
    fn a_palavra_acha_o_que_o_vetor_nao_vi_e_o_cartao_continua_medindo_o_vetor() {
        let indice = indice_com_palavras(&[
            ("régua e medidor", [0.0, 1.0]),
            ("sensor ds18b20 calibra", [1.0, 0.0]),
        ]);
        let raizes = [PathBuf::from("C:/concedida")];
        // A consulta olha para o primeiro trecho; a palavra está no segundo.
        let vetor = [0.0f32, 1.0];
        let buscar = |modo: Modo| {
            indice.buscar(
                &Busca { vetor: &vetor, texto: "ds18b20", k: 2, piso: 0.5, modo, ignorar: &[] },
                &raizes,
            )
            .unwrap()
        };

        let so_vetor = buscar(Modo::Vetorial);
        assert_eq!(so_vetor.achados.len(), 1);
        assert_eq!(so_vetor.achados[0].texto, "régua e medidor");

        // Pela palavra, o piso do vetor não segura o achado: o identificador
        // literal é a resposta, e o número que o cartão mostra continua sendo o
        // cosseno medido — zero, neste caso — nunca a nota do BM25.
        let so_palavra = buscar(Modo::Lexico);
        assert_eq!(so_palavra.modo, "lexico");
        assert_eq!(so_palavra.achados.len(), 1);
        assert_eq!(so_palavra.achados[0].texto, "sensor ds18b20 calibra");
        assert_eq!(so_palavra.achados[0].origem, Origem::Lexica);
        assert_eq!(so_palavra.achados[0].pontuacao, 0.0);

        // Fundidas, as duas peças aparecem. Os dois caminhos colocam cada uma na
        // primeira posição da sua própria lista, então a fusão empata e o
        // desempate é o cosseno — o que o vetor viu vem primeiro.
        let junto = buscar(Modo::Hibrido);
        assert_eq!(junto.modo, "hibrido");
        assert_eq!(
            junto.achados.iter().map(|a| a.texto.as_str()).collect::<Vec<_>>(),
            vec!["régua e medidor", "sensor ds18b20 calibra"]
        );
        assert_eq!(junto.alcancados, 2);
    }

    #[test]
    fn cada_achado_diz_por_onde_entrou() {
        let indice = indice_com_palavras(&[
            ("norte", [1.0, 0.0]),
            ("leste e norte", [0.0, 1.0]),
            ("oeste", [0.7071067, 0.7071067]),
        ]);
        let raizes = [PathBuf::from("C:/concedida")];
        let vetor = [1.0f32, 0.0];
        let resposta = indice
            .buscar(&Busca { vetor: &vetor, texto: "norte", k: 3, piso: 0.5, modo: Modo::Hibrido, ignorar: &[] }, &raizes)
            .unwrap();
        assert_eq!(
            resposta
                .achados
                .iter()
                .map(|a| (a.texto.as_str(), a.origem))
                .collect::<Vec<_>>(),
            vec![
                ("norte", Origem::Ambas),
                ("oeste", Origem::Vetorial),
                ("leste e norte", Origem::Lexica),
            ]
        );
        // O "e mais N" do cartão conta o conjunto das duas listas, não o tamanho
        // de uma delas: foi isto que a ordenação teve à disposição.
        assert_eq!(resposta.alcancados, 3);
    }

    #[test]
    fn a_palavra_nao_precisa_ter_o_mesmo_acento() {
        let indice = indice_com_palavras(&[("manutenção elétrica do prédio", [1.0, 0.0])]);
        let raizes = [PathBuf::from("C:/concedida")];
        // Maiúsculo e sem acento, como alguém digita correndo. O índice também
        // foi escrito sem acento, nas duas pontas pela mesma função.
        let vetor = [0.0f32, 1.0];
        let resposta = indice
            .buscar(&Busca {
                vetor: &vetor,
                texto: "MANUTENCAO ELETRICA",
                k: 1,
                piso: 0.9,
                modo: Modo::Lexico,
                ignorar: &[],
            }, &raizes)
            .unwrap();
        assert_eq!(resposta.achados.len(), 1);
        assert_eq!(resposta.achados[0].texto, "manutenção elétrica do prédio");
        assert_eq!(resposta.achados[0].origem, Origem::Lexica);
        // Sem nenhuma palavra em comum, a lista léxica fica vazia — ausência não
        // entra em lista nenhuma como "nota baixa".
        let vazio = indice
            .buscar(&Busca { vetor: &vetor, texto: "tubulação", k: 1, piso: 0.0, modo: Modo::Lexico, ignorar: &[] }, &raizes)
            .unwrap();
        assert!(vazio.achados.is_empty());
    }

    #[test]
    fn refatiar_apaga_o_que_foi_cortado_de_outro_modo() {
        let mut indice = indice_com_palavras(&[("norte", [1.0, 0.0]), ("sul", [0.0, 1.0])]);
        assert_eq!(indice.fatiamento(), (64, 0));
        assert_eq!(indice.trechos(), 2);
        // A mesma escala não refatia: apagar memória boa porque o cursor não se
        // moveu é destruir índice à toa.
        assert!(!indice.refatiar(64, 0));
        assert_eq!(indice.trechos(), 2);
        assert!(indice.refatiar(256, 64));
        assert_eq!(indice.fatiamento(), (256, 64));
        // Peça de outra escala é trecho de outro texto: nem o vetor nem a
        // assinatura de leitura podem sobrar.
        assert_eq!(indice.trechos(), 0);
        assert_eq!(indice.arquivos(), 0);
        assert!(indice.vetores.is_empty());
    }

    #[test]
    fn vetor_de_norma_zero_e_falha_e_nao_um_vizinho_qualquer() {
        let mut zero = vec![0.0f32, 0.0];
        assert!(Indice::normalizar(&mut zero).is_err());
    }

    #[test]
    fn remover_tira_so_o_arquivo_pedido() {
        let mut indice = Indice::vazio("m", 1, "último", 64, 0);
        for arquivo in ["a", "b"] {
            // Dois trechos por arquivo: o que `remover_arquivo` tem de tirar é o
            // conjunto do arquivo, não só a peça que a cursor encontra.
            let pecas = vec![
                (Peca { linha: 1, texto: format!("{arquivo}-1"), ids: vec![] }, vec![1.0f32]),
                (Peca { linha: 2, texto: format!("{arquivo}-2"), ids: vec![] }, vec![1.0f32]),
            ];
            indice.adicionar(arquivo, Assinatura { bytes: 1, modificado: 1 }, &pecas);
        }
        assert_eq!(indice.trechos(), 4);
        assert_eq!(indice.remover_arquivo("a"), 2);
        assert_eq!(indice.trechos(), 2);
        assert_eq!(indice.vetores.len(), 2);
        assert!(indice.meta.trechos.iter().all(|t| t.arquivo == "b"));
    }

    #[test]
    fn revogar_a_pasta_tira_o_que_estava_dentro_dela() {
        let mut indice = Indice::vazio("m", 1, "último", 64, 0);
        let peca = Peca { linha: 1, texto: "x".into(), ids: vec![] };
        indice.adicionar(
            "C:/projetos/app/src/a.rs",
            Assinatura { bytes: 1, modificado: 1 },
            &[(peca.clone(), vec![1.0])],
        );
        indice.adicionar(
            "C:/outra/b.rs",
            Assinatura { bytes: 1, modificado: 1 },
            &[(peca, vec![1.0])],
        );
        indice.reter_dentro(&[PathBuf::from("C:/outra")]);
        assert_eq!(indice.trechos(), 1);
        assert_eq!(indice.arquivos(), 1);
        assert_eq!(indice.meta.trechos[0].arquivo, "C:/outra/b.rs");
    }

    /// Uma conversa como `conversas.rs` a entrega: `falas` falas, todas no texto
    /// porque aqui nenhuma delas estourou o teto de cauda.
    fn conversa(id: &str, titulo: &str, falas: usize, atualizada: u64) -> ConversaViva {
        ConversaViva {
            id: id.to_string(),
            titulo: titulo.to_string(),
            atualizada,
            mensagens: falas,
            texto: (1..=falas)
                .map(|n| format!("fala {n} sobre a troca da bomba"))
                .collect::<Vec<_>>()
                .join("\n"),
            a_partir_de: 0,
        }
    }

    /// O índice com as duas fontes ligadas no mesmo espaço vetorial: um trecho de
    /// arquivo concedido e um de conversa, ambos na direção da consulta. É assim
    /// que se vê qual das duas políticas corta qual.
    fn indice_duas_fontes() -> Indice {
        let mut indice = Indice::vazio("m", 2, "último", 64, 0);
        indice.adicionar(
            "C:/concedida/a.txt",
            Assinatura { bytes: 1, modificado: 1 },
            &[(Peca { linha: 3, texto: "a bomba d'água".into(), ids: vec![] }, vec![1.0, 0.0])],
        );
        let viva = conversa("c1", "Troca da bomba", 2, 1_700_000_000_000);
        indice.substituir_conversa(
            &viva,
            viva.prova(),
            &[(
                Peca { linha: 1, texto: "fala 1 sobre a troca da bomba".into(), ids: vec![] },
                vec![1.0, 0.0],
            )],
            false,
        );
        indice
    }

    /// A revogação de pasta é a política dos arquivos. O que a pessoa disse numa
    /// conversa não muda de dono quando ela mexe no grafo: perder isso seria
    /// apagar a memória por engano.
    #[test]
    fn revogar_a_pasta_nao_apaga_o_que_a_conversa_lembra() {
        let mut indice = indice_duas_fontes();
        assert_eq!(indice.trechos(), 2);
        // Nenhuma pasta concedida: o arquivo perde o direito de ser lembrado.
        assert_eq!(indice.reter_dentro(&[]), 1);
        assert_eq!(indice.trechos_de(Fonte::Arquivo), 0);
        assert_eq!(indice.trechos_de(Fonte::Conversa), 1);
        assert_eq!(indice.conversas(), 1);
        // E a busca, sem pasta nenhuma, ainda devolve a conversa.
        let resposta = indice.buscar(&por_vetor(&[1.0, 0.0], 2, 0.0), &[]).unwrap();
        assert_eq!(resposta.achados.len(), 1);
        assert_eq!(resposta.achados[0].fonte, Fonte::Conversa);
    }

    /// O contrário também vale, e aqui a chave é a mesma string de propósito: o
    /// id de uma conversa pode, por azar, ser igual ao caminho guardado no
    /// trecho de um arquivo. O que separa as duas casas é a fonte, nunca a chave.
    #[test]
    fn esquecer_a_conversa_tira_so_o_trecho_dela_mesmo_com_o_arquivo_de_mesma_chave() {
        let mut indice = Indice::vazio("m", 1, "último", 64, 0);
        indice.adicionar(
            "c1",
            Assinatura { bytes: 1, modificado: 1 },
            &[(Peca { linha: 1, texto: "o arquivo c1".into(), ids: vec![] }, vec![1.0])],
        );
        let viva = conversa("c1", "A conversa c1", 1, 7);
        indice.substituir_conversa(
            &viva,
            viva.prova(),
            &[(Peca { linha: 1, texto: "a fala da conversa".into(), ids: vec![] }, vec![1.0])],
            false,
        );
        assert_eq!(indice.trechos(), 2);
        assert_eq!(indice.esquecer_conversa("c1"), 1);
        assert_eq!(indice.trechos(), 1);
        assert_eq!(indice.meta.trechos[0].fonte, Fonte::Arquivo);
        assert_eq!(indice.meta.trechos[0].texto, "o arquivo c1");
        assert_eq!(indice.arquivos(), 1);
    }

    /// Apagar do histórico, ou simplesmente sair da janela que a memória cobre,
    /// tem de apagar a lembrança — e só ela.
    #[test]
    fn a_conversa_que_sai_da_lista_sai_do_indice() {
        let mut indice = indice_duas_fontes();
        let vivas = [conversa("c2", "Outro papo", 1, 200)];
        assert_eq!(indice.reter_conversas(&vivas), 1);
        assert_eq!(indice.trechos_de(Fonte::Conversa), 0);
        assert_eq!(indice.trechos_de(Fonte::Arquivo), 1);
        assert_eq!(indice.conversas(), 0);
        // O arquivo continua visto pela varredura: a conversa não tinha nada a
        // ver com as assinaturas dele.
        assert!(indice.esta_visto("C:/concedida/a.txt", &Assinatura { bytes: 1, modificado: 1 }));
    }

    /// Zero é o caso comum, e é ele que faz um turno ligar a memória de conversa
    /// sem custar um vetor nenhum.
    #[test]
    fn o_turno_sem_conversa_nova_nao_tem_trabalho_nenhum() {
        let mut indice = Indice::vazio("m", 1, "último", 64, 0);
        let viva = conversa("c1", "Bomba", 3, 100);
        assert_eq!(indice.conversas_pendentes(&[viva.clone()]), 1);
        indice.substituir_conversa(
            &viva,
            viva.prova(),
            &[(Peca { linha: 1, texto: "fala 1".into(), ids: vec![] }, vec![1.0])],
            false,
        );
        assert_eq!(indice.conversas_pendentes(&[viva.clone()]), 0);
        // Ganhou falas: voltou a ser trabalho.
        let crescida = ConversaViva { mensagens: 4, atualizada: 150, ..viva.clone() };
        assert_eq!(indice.conversas_pendentes(&[crescida]), 1);
        // Só mudou o título: o texto lembrado ainda é o mesmo, e reescrever por
        // causa de título seria pagar um vetor para trocar uma palavra do cartão.
        let renomeada = ConversaViva { titulo: "Título novo".into(), ..viva.clone() };
        assert_eq!(indice.conversas_pendentes(&[renomeada]), 0);
        // A que saiu da lista também é trabalho — trabalho de desfazer.
        assert_eq!(indice.conversas_pendentes(&[]), 1);
    }

    /// A prova de que a conversa está lembrada precisa sobreviver à reinicialização,
    /// ou o primeiro turno depois de abrir o app re vetorizaria o histórico todo.
    #[test]
    fn a_ficha_da_conversa_volta_do_disco_com_a_prova() {
        let pasta = std::env::temp_dir().join(format!("aether-memoria-conversa-{}", std::process::id()));
        std::fs::create_dir_all(&pasta).unwrap();
        let arquivo = pasta.join("indice.aether");
        let indice = indice_duas_fontes();
        let viva = conversa("c1", "Troca da bomba", 2, 1_700_000_000_000);
        indice.salvar(&arquivo).unwrap();
        let lido = Indice::carregar(&arquivo).expect("leu o índice");
        assert_eq!(lido.conversas(), 1);
        assert_eq!(lido.trechos_de(Fonte::Conversa), 1);
        assert!(lido.conversa_vista(&viva.id, &viva.prova()));
        assert_eq!(lido.conversas_pendentes(&[viva]), 0);
        // O trecho de arquivo, gravado antes de `fonte` existir, é arquivo: o
        // padrão do campo velho é a verdade sobre ele.
        assert_eq!(lido.meta.trechos[0].fonte, Fonte::Arquivo);
        std::fs::remove_dir_all(&pasta).ok();
    }

    /// O cartão cita o que o modelo pode reler: título e data da ficha gravada
    /// junto do trecho, não uma suposição sobre o id. E o `ignorar` do turno corta
    /// a conversa que já está na janela dele, sem tocar nos arquivos.
    #[test]
    fn o_achado_de_conversa_diz_o_titulo_a_data_e_sai_pelo_ignorar() {
        let indice = indice_duas_fontes();
        let raizes = [PathBuf::from("C:/concedida")];
        let buscar = |ignorar: &[String]| {
            indice
                .buscar(
                    &Busca {
                        vetor: &[1.0, 0.0],
                        texto: "bomba",
                        k: 2,
                        piso: 0.0,
                        modo: Modo::Hibrido,
                        ignorar,
                    },
                    &raizes,
                )
                .unwrap()
        };
        let resposta = buscar(&[]);
        let da_conversa = resposta
            .achados
            .iter()
            .find(|a| a.fonte == Fonte::Conversa)
            .expect("a conversa não entrou na lista");
        assert_eq!(da_conversa.nome, "Troca da bomba");
        assert_eq!(da_conversa.arquivo, "c1");
        assert_eq!(da_conversa.quando, Some(1_700_000_000_000));
        let do_arquivo = resposta
            .achados
            .iter()
            .find(|a| a.fonte == Fonte::Arquivo)
            .expect("o arquivo não entrou na lista");
        assert_eq!(do_arquivo.nome, "a.txt");
        assert_eq!(do_arquivo.quando, None);
        assert_eq!(do_arquivo.linha, 3);

        // O turno pergunta sobre o que ainda não sabe.
        let ignorada = ["c1".to_string()];
        let resposta = buscar(&ignorada);
        assert_eq!(resposta.achados.len(), 1);
        assert_eq!(resposta.achados[0].fonte, Fonte::Arquivo);
    }

    /// Numa conversa cortada pela cauda, a linha do trecho não é a posição dele no
    /// texto recebido: é o número da fala como a pessoa a vê no histórico.
    #[test]
    fn a_peca_da_cauda_cita_a_fala_verdadeira() {
        let mut indice = Indice::vazio("m", 1, "último", 64, 0);
        let viva = ConversaViva {
            id: "c1".into(),
            titulo: "Bomba".into(),
            atualizada: 9,
            mensagens: 50,
            texto: "fala 49\nfala 50".into(),
            a_partir_de: 48,
        };
        indice.substituir_conversa(
            &viva,
            viva.prova(),
            &[
                (Peca { linha: 1, texto: "fala 49".into(), ids: vec![] }, vec![1.0]),
                (Peca { linha: 2, texto: "fala 50".into(), ids: vec![] }, vec![1.0]),
            ],
            false,
        );
        assert_eq!(
            indice.meta.trechos.iter().map(|t| t.linha).collect::<Vec<_>>(),
            vec![49, 50]
        );
        // Reescrever a conversa não acumula: sai o texto lembrado, entra o novo.
        indice.substituir_conversa(
            &viva,
            viva.prova(),
            &[(Peca { linha: 1, texto: "fala 49 de novo".into(), ids: vec![] }, vec![1.0])],
            false,
        );
        assert_eq!(indice.trechos_de(Fonte::Conversa), 1);
        assert_eq!(indice.meta.trechos[0].linha, 49);
        assert_eq!(indice.meta.trechos[0].texto, "fala 49 de novo");
    }

    /// A verba do turno é um teto de trabalho, não uma fila de espera eterna: a
    /// primeira conversa sempre entra, mesmo maior que a verba inteira.
    #[test]
    fn a_verba_do_turno_nunca_deixa_a_primeira_conversa_de_fora() {
        // A primeira entra sempre, maior que a verba inteira.
        assert!(!acabou_a_verba(0, TOKENS_POR_TURNO * 6, TOKENS_POR_TURNO));
        // Da segunda em diante é a soma que manda, e ela corta no limite exato.
        assert!(!acabou_a_verba(TOKENS_POR_TURNO - 1, 1, TOKENS_POR_TURNO));
        assert!(acabou_a_verba(TOKENS_POR_TURNO, 1, TOKENS_POR_TURNO));
        assert!(acabou_a_verba(4_001, 4_001, 8_000));
    }

    /// Trocar a escala do fatiamento joga fora as peças inteiras — inclusive as de
    /// conversa, cuja ficha teria de dizer que lembra um texto que não existe mais.
    #[test]
    fn refatiar_tambem_esquece_as_conversas() {
        let mut indice = indice_duas_fontes();
        assert!(indice.refatiar(128, 32));
        assert_eq!(indice.trechos(), 0);
        assert_eq!(indice.conversas(), 0);
        assert_eq!(indice.conversas_pendentes(&[conversa("c1", "Troca da bomba", 2, 1_700_000_000_000)]), 1);
    }

    #[test]
    fn dependencia_binario_e_segredo_nao_entram_na_varredura() {
        assert!(pasta_ignorada("node_modules"));
        assert!(pasta_ignorada(".git"));
        assert!(pasta_ignorada("TARGET"));
        assert!(!pasta_ignorada("src"));

        assert!(arquivo_ignorado("a.dll"));
        assert!(arquivo_ignorado(".env"));
        assert!(arquivo_ignorado("id_ed25519"));
        assert!(arquivo_ignorado("server.pem"));
        assert!(arquivo_ignorado("app.key"));
        assert!(arquivo_ignorado("database_secret.json"));
        assert!(!arquivo_ignorado("main.rs"));
        assert!(!arquivo_ignorado("README"));
        assert!(!arquivo_ignorado("notes.txt"));
    }

    #[test]
    fn ajustes_fora_da_faixa_volta_para_dentro() {
        let a = Ajustes {
            trechos: 99,
            peca: 4,
            piso: 9.0,
            sobreposicao: 999,
            ..Ajustes::default()
        }
        .aprovar();
        assert_eq!(a.trechos, TRECHOS_MAX);
        assert_eq!(a.peca, PECA_MIN);
        assert!(a.piso <= 0.99);
        // Metade da peça é o teto real, abaixo do teto da escala: duas janelas
        // vizinhas quase iguais seriam cópia no índice, não memória.
        assert_eq!(a.sobreposicao, PECA_MIN / 2);
    }

    #[test]
    fn o_modo_e_a_sobreposicao_passam_pelo_arquivo() {
        let pasta = std::env::temp_dir().join(format!("aether-rag-modo-{}", std::process::id()));
        std::fs::create_dir_all(&pasta).unwrap();
        let arquivo = pasta.join("rag.json");
        let a = Ajustes {
            sobreposicao: 128,
            modo: Modo::Lexico,
            ..Ajustes::default()
        };
        a.salvar(&arquivo).unwrap();
        let lido = Ajustes::de(&arquivo);
        assert_eq!(lido.sobreposicao, 128);
        assert_eq!(lido.modo, Modo::Lexico);
        assert_eq!(lido.modo.nome(), "lexico");
        // O arquivo de antes deste ajuste não tem a chave: volta o padrão, não um
        // zero que fatiaria o arquivo inteiro numa peça só.
        std::fs::write(&arquivo, r#"{"modelo":"","trechos":4,"peca":384,"piso":0.15}"#).unwrap();
        let antes = Ajustes::de(&arquivo);
        assert_eq!(antes.sobreposicao, Ajustes::default().sobreposicao);
        assert_eq!(antes.modo, Modo::Hibrido);
        std::fs::remove_dir_all(&pasta).ok();
    }

    #[test]
    fn trocar_de_codificador_invalida_o_antes() {
        let antes = Ajustes::default();
        let depois = Ajustes { modelo: "qwen3-embedding-0.6b-f16".into(), ..Ajustes::default() };
        assert!(depois.trocou_codificador(&antes));
        assert!(!Ajustes { peca: 512, ..antes.clone() }.trocou_codificador(&antes));
    }

    #[test]
    fn o_arquivo_de_uma_escrita_só_vira_ajuste() {
        let pasta = std::env::temp_dir().join(format!("aether-rag-{}", std::process::id()));
        std::fs::create_dir_all(&pasta).unwrap();
        let arquivo = pasta.join("rag.json");
        assert_eq!(Ajustes::de(&arquivo), Ajustes::default());
        let a = Ajustes { trechos: 7, ..Ajustes::default() };
        a.salvar(&arquivo).unwrap();
        assert_eq!(Ajustes::de(&arquivo).trechos, 7);
        std::fs::write(&arquivo, "{ não é json").unwrap();
        assert_eq!(Ajustes::de(&arquivo), Ajustes::default());
        std::fs::remove_dir_all(&pasta).ok();
    }
}
