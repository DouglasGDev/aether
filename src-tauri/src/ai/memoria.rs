//! A memória do Aether: o que o assistente leu, guardado como vetor, e
//! recuperado por proximidade.
//!
//! É RAG sem nuvem, e o nome descreve o que está aqui: o texto vira vetor num
//! codificador GGUF local, os vetores vivem num arquivo desta máquina e a busca
//! é uma comparação de cosseno sobre eles. Não há serviço terceiro, banco
//! vetorial embutido nem chamada de rede — a única rede deste módulo é o
//! download do próprio codificador, que é o mesmo caminho dos modelos de
//! conversa.
//!
//! Três fontes entram no mesmo índice, cada uma com a sua política: o arquivo de
//! uma pasta concedida, a fala de uma conversa salva e a nota da base de
//! conhecimento ([`crate::ai::base`]) — a que o dono escreveu de propósito, e que
//! por isso não obedece nem às pastas nem às conversas. Ver [`Fonte`].
//!
//! A política não é duplicada: recuperar memória é a ferramenta
//! `computador__buscar_memoria`, então o fio do grafo que a liga é o mesmo que
//! autoriza a injeção automática no prompt, e as pastas que ela enxerga são as
//! de `pastas.json`.

use super::catalog::{self, ModelSpec};
use super::ggml::{Despacho, Embutidor};
use crate::ai::base::Nota;
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

/// Abaixo disto o pré-filtro por cluster não paga a si mesmo: varrer trezentos
/// vetores custa menos que contar o teto de sessenta centroides e, num índice
/// deste tamanho, cada turno pode mudar a partição — mantê-la viva custaria mais
/// que a busca que ela economizaria. O caminho exaustivo continua sendo o
/// caminho de verdade, e é ele que responde aqui.
const CLUSTER_MINIMO: usize = 512;
/// Teto de centroides. `raiz(n)` dá ~110 para o índice cheio; o teto existe para
/// um índice que um dia passe de `LIMITE_TRECHOS` não gastar mais medindo o mapa
/// da casa do que medindo a casa inteira.
const CLUSTER_MAXIMO: usize = 256;
/// Iterações de Lloyd na construção. Não é convergência — é o ponto em que a
/// partição de texto já separa o que separa, e cada iteração custa
/// `amostra × k × dimensão` multiplicações na máquina do dono, que é também a que
/// responde à pergunta.
const CLUSTER_ITERACOES: usize = 6;
/// Vetores que treinam os centroides. A amostra é colhida por passo fixo sobre o
/// índice inteiro, nunca por sorteio: uma partição escolhida ao acaso mudaria
/// quais baldes são sondados e, com eles, a resposta da mesma pergunta em duas
/// execuções — o mesmo motivo que obriga o empate do BM25 a cair para o índice
/// menor.
const CLUSTER_AMOSTRA: usize = 1024;
/// Folga do teto de um balde. O teto é uma desigualdade de Cauchy–Schwarz contada
/// em `f32`, e arredondar a soma pode deixar o cosseno verdadeiro uma fração
/// ínfima acima do valor calculado. Sem folga, um trecho sairia da lista por
/// morar num balde cortado por um bit — que é a perda que não se aceita aqui.
const CLUSTER_FOLGA: f32 = 1e-5;
/// Consultas cujo vetor fica na memória entre um turno e outro. Uma consulta é
/// um lote de ~380 tokens atravessando um codificador de seiscentos milhões de
/// parâmetros; sessenta e quatro vetores de mil e vinte e quatro `f32` são
/// duzentos e sessenta kilobytes. Cobrir uma conversa com rodadas de ferramenta
/// não pede mais que isso, e guardar consultas de ontem seria memória parada
/// sem pergunta nenhuma.
const CACHE_CONSULTAS: usize = 64;

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

/// O produto escalar das duas metades que a casa compara. Os dois lados entram
/// normatizados — é o contrato de `Indice::normalizar` e do `Embutidor` — então o
/// resultado é o próprio cosseno, na faixa de −1 a 1, e é a única operação que a
/// conta de [`Contagem::trechos`] mede: é ela, e não a leitura dos vetores, que
/// decide o tempo de uma consulta.
///
/// A soma em `f64` não é preciosismo: somar mil termos de `f32` acumula erro do
/// tamanho da própria folga que autoriza parar um balde, e um teto furado por
/// arredondamento é a lista respondendo diferente da varredura exaustiva.
fn produto(a: &[f32], b: &[f32]) -> f32 {
    let menor = a.len().min(b.len());
    let mut soma = 0f64;
    for i in 0..menor {
        soma += a[i] as f64 * b[i] as f64;
    }
    soma as f32
}

/// O balde de um vetor: o centroide de maior cosseno, desempatando pelo de índice
/// menor. Usado na construção do mapa e em cada anexação, e é o mesmo cálculo que
/// a consulta faz sobre os centroides — por isso a partição e a busca nunca
/// discordam sobre onde um vetor mora.
fn melhor_centroide(centroide: &[f32], x: &[f32], quantos: usize, dimensao: usize) -> usize {
    let mut melhor = 0usize;
    let mut maior = f32::NEG_INFINITY;
    for balde in 0..quantos {
        let inicio = balde * dimensao;
        let valor = produto(&centroide[inicio..inicio + dimensao], x);
        if valor > maior {
            maior = valor;
            melhor = balde;
        }
    }
    melhor
}

/// Euclides sobre `usize`, pelo único uso que a casa tem: escolher um passo de
/// amostragem coprimo com o tamanho do índice.
fn mdc(mut a: usize, mut b: usize) -> usize {
    while b != 0 {
        let resto = a % b;
        a = b;
        b = resto;
    }
    a
}

/// O passo da amostra de k-means: o quociente que entrega `alvo` pontos, corrigido
/// para o primeiro valor coprimo com `total`. Coprimo é o que faz a caminhada
/// `k × passo (mod total)` percorrer o índice inteiro num ciclo só, em vez de ficar
/// presa numa classe de resto — e é o resto que decide se um grupo inteiro do
/// índice entra ou não no mapa. Com `total ≤ alvo` o quociente é 1 e a amostra é o
/// índice todo, como sempre foi.
fn passo_da_amostra(total: usize, alvo: usize) -> usize {
    let mut passo = total.div_ceil(alvo).max(1);
    while mdc(passo, total) != 1 {
        passo += 1;
    }
    passo
}

/// Guarda os `necessario` maiores valores vistos, do maior para o menor. Mantém a
/// lista ordenada na inserção em vez de reordenar no fim: é o que deixa o corte do
/// laço de sondagem (`melhores[necessario - 1]`) pronto a cada medição, sem passada
/// extra sobre o resultado. Só interessa o valor do último, não quais empates o
/// compõem, então um valor igual ao último entra pela porta da frente: ele não
/// mudaria o corte e custaria uma reordenação.
fn oferecer(melhores: &mut Vec<f32>, necessario: usize, valor: f32) {
    if melhores.len() < necessario {
        let posicao = melhores
            .iter()
            .position(|atual| valor > *atual)
            .unwrap_or(melhores.len());
        melhores.insert(posicao, valor);
        return;
    }
    if valor > melhores[necessario - 1] {
        melhores[necessario - 1] = valor;
        let mut posicao = necessario - 1;
        while posicao > 0 && melhores[posicao] > melhores[posicao - 1] {
            melhores.swap(posicao, posicao - 1);
            posicao -= 1;
        }
    }
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

pub(crate) fn sem_acento(letra: char) -> char {
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
    /// O trecho não veio de busca nenhuma: entrou porque o dono prendeu a nota
    /// em que ele está. É a única origem que não é medida, e por isso ela existe
    /// como variante em vez de um número — ver [`Achado::pontuacao`].
    Fixada,
}

/// O que escreve um trecho no índice. As três fontes convivem no mesmo arquivo e
/// na mesma busca — é isso que faz um cosseno e um BM25 competirem na mesma
/// régua — mas a política de cada uma é sua.
///
/// Um arquivo só é lido enquanto a pasta onde ele está continua concedida; a
/// revogação tira o conteúdo dele da resposta e da varredura. Uma conversa é
/// estado do próprio aplicativo: não há pasta a revogar, e quem a apaga é a
/// lista de conversas. Uma nota de documento é a terceira coisa, e não é nenhuma
/// das duas: ela obedece à **lista de notas** — a pasta `base/` — e não às pastas
/// concedidas nem à lista de conversas. [`Fonte`] existe para isso: as fontes
/// não emprestam política uma para a outra. Confundí-las — deixar a varredura de
/// pastas apagar as conversas, a concessão esconder o histórico, ou a revogação
/// de uma pasta levar uma nota que o dono escreveu de propósito — seria uma das
/// duas: memória perdida ou memória que o dono mandou esquecer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Fonte {
    /// Texto lido de um arquivo dentro de uma pasta concedida.
    #[default]
    Arquivo,
    /// Uma fala de uma conversa salva.
    Conversa,
    /// O texto de uma nota da base de conhecimento (`crate::ai::base`). A chave
    /// no índice é o `id` da nota, que é o nome do arquivo dela sem a extensão.
    Documento,
}

impl Fonte {
    /// A palavra de máquina que a interface traduz.
    pub fn nome(self) -> &'static str {
        match self {
            Self::Arquivo => "arquivo",
            Self::Conversa => "conversa",
            Self::Documento => "documento",
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FichaDocumento {
    /// O título da nota, lido do arquivo dela no momento em que ela entrou. É o
    /// que o cartão mostra como `nome`.
    titulo: String,
    /// Instante da última escrita no `.md`, em milissegundos: é o que ordena as
    /// fixadas e o que a tela mostra como data.
    atualizada: u64,
    /// Se a nota estava presa quando o índice a leu. É espelho do marcador dentro
    /// do arquivo — a decisão mora no `.md`, e um `rag.json` com lista de fixadas
    /// seria a segunda verdade da mesma nota.
    fixada: bool,
    /// Bytes do texto indexado e instante do arquivo: a prova de que as peças
    /// gravadas são o texto de hoje, e não o de ontem.
    prova: Assinatura,
    /// A nota foi cortada pelo teto de trechos no meio do fatiamento: o índice
    /// lembra só o começo. Mesma regra da ficha de conversa, pelo mesmo motivo —
    /// marcada, ela não está "vista" e volta para a fila quando abrir espaço.
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
    /// Uma nota por id, com o título que o cartão mostra, a data do arquivo e se
    /// ela estava presa. Terceiro mapa e pelos mesmos três motivos do segundo: a
    /// nota tem política própria (nem pasta, nem conversa), a varredura de pastas
    /// não tem nada a decidir sobre ela, e o índice gravado antes disto existir é
    /// vazio — que é a verdade sobre uma base que ainda não foi escrita.
    documentos: BTreeMap<String, FichaDocumento>,
    trechos: Vec<Trecho>,
    /// A partição IVF dos vetores, gravada junto com o índice. Índice escrito antes
    /// disto existir não tem a chave: `None`, e a busca dele continua exaustiva —
    /// ver [`Clusteres`] e o porquê de não reclusterizar dentro de um turno.
    #[serde(default)]
    clusteres: Option<Clusteres>,
}

/// O mapa da casa: `k` centroides e o balde de cada trecho. Existe para a busca não
/// precisar multiplicar os doze mil vetores pela pergunta de cada turno quando
/// oitocentos deles não têm chance nenhuma de entrar na lista.
///
/// A correção não é uma aposta. Cada balde guarda o teto do cosseno que qualquer
/// vetor dele pode ter contra uma consulta `q` normalizada:
///
/// ```text
/// ⟨q, x⟩ = ⟨q, c⟩ + ⟨q, x − c⟩ ≤ ‖c‖·⟨q, ĉ‖ + max‖x − c‖
/// ```
///
/// que é Cauchy–Schwarz sobre a residual, com `‖q‖ = 1`. Sondando os baldes do teto
/// maior para o menor e parando quando o teto cai abaixo do que ainda interessa, o
/// que fica de fora é provado impossível, não improvável: a lista sai igual à da
/// varredura inteira. Daí as três invariantes que os mutadores de `Indice` têm de
/// manter — `balde` com um item por trecho, `raio[b]` nunca menor que a distância
/// real dos vetores que entraram naquele balde, e `centroide` normalizado com
/// `norma` guardando o tamanho da média crua.
///
/// Grava no JSON da metadata, e não num acréscimo binário ao fim do arquivo, por
/// duas razões: o `meta` já é a maior parte do `indice.aether` (é ele que guarda o
/// texto de doze mil peças), e um campo novo com `#[serde(default)]` é exatamente
/// como `fonte`, `sobreposicao` e `truncada` entraram — um índice antigo continua
/// abrindo com a assinatura de sempre, que é o contrato que `ASSINATURA` protege.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Clusteres {
    /// Quantos centroides. Os três vetores por-balde abaixo têm este comprimento.
    quantos: usize,
    /// Largura dos vetores na hora em que o mapa foi contado. Mudou o codificador,
    /// mudou a largura, e o mapa velho não vale para o novo espaço.
    dimensao: usize,
    /// Quantos trechos havia quando o mapa foi contado. É o que diz se o índice
    /// desde então dobrou de tamanho — caso em que os baldes deixam de ser um mapa
    /// e viram uma adivinhação, e `Indice::reagrupar` refaz.
    trechos: usize,
    /// `quantos × dimensao` floats, cada centroide normalizado. A normalização é do
    /// mapa, não do espaço: a média de vetores unitários não é unitária, e é por
    /// isso que `norma` existe ao lado.
    centroide: Vec<f32>,
    /// `‖c‖` de cada centroide cru.
    norma: Vec<f32>,
    /// O maior `‖x − c‖` do balde. Apertado a cada anexação e a cada gravação;
    /// afrouxado por uma remoção, o que só custa sondar um balde a mais.
    raio: Vec<f32>,
    /// O balde de cada trecho, na ordem de `meta.trechos`.
    balde: Vec<u32>,
}

/// Um trecho devolvido por proximidade, com a medida que o classificou.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Achado {
    /// O caminho do arquivo, ou o id da conversa.
    pub arquivo: String,
    /// O rótulo curto do cartão: o nome do arquivo, o título da conversa ou o
    /// título da nota.
    pub nome: String,
    pub linha: usize,
    pub texto: String,
    /// Cosseno medido entre o vetor da consulta e o do trecho, de 0 a 1. Vale
    /// em todos os modos: um trecho que entrou pela palavra tem vetor no índice,
    /// e o número mostrado é ele, medido — nunca a pontuação da fusão, que não é
    /// uma similaridade e apareceria como se fosse.
    ///
    /// A exceção é [`Origem::Fixada`]: um trecho preso nunca foi procurado, então
    /// não há cosseno nenhum a mostrar, e o valor é zero — que é o número dizendo
    /// "nenhuma medida foi feita", não "medido como nada parecido". Quem mostra
    /// o cartão escreve a palavra *fixada* neste caso, e não o zero: a origem é
    /// que diz isso, e é por ela que o campo existe.
    pub pontuacao: f32,
    /// O que trouxe o trecho: a proximidade, a palavra, as duas, ou a decisão do
    /// dono de prender a nota. O cartão traduz.
    pub origem: Origem,
    /// De onde veio o trecho — arquivo concedido, conversa salva ou nota da
    /// base. O cartão traduz, porque "linha 12" de um arquivo não é a mesma
    /// coisa que a mensagem 12 de uma conversa.
    pub fonte: Fonte,
    /// Instante da última mensagem da conversa, ou da última escrita da nota, em
    /// milissegundos. `None` num
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

/// Quanto custou uma busca, em produtos escalares. Não é um número para a tela: é
/// a medição que a casa faz de si mesma, e por isso não mora dentro de `Resposta`.
/// Os dois consumidores do contrato — `commands.rs` e a ferramenta `buscar_memoria`
/// — desmontam a resposta campo por campo, e um campo novo ali seria a API pública
/// mudando por causa de uma contagem interna; o número é devolvido por
/// [`Indice::buscar_contando`] ao lado da resposta, para quem mede.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Contagem {
    /// Produtos escalares entre a consulta e um trecho do índice. É o que a
    /// varredura exaustiva faz `n` vezes, `n` sendo o tamanho do índice, e o que a
    /// partição tenta não fazer.
    pub trechos: usize,
    /// Produtos escalares entre a consulta e um centroide — o preço de consultar o
    /// mapa antes dos vetores. Entra separado porque é outra largura de vetor e
    /// porque somar as duas coisas esconderia qual das duas a partição cortou.
    pub centroides: usize,
}

impl Contagem {
    /// O total de produtos escalares da consulta, que é o número que se cita.
    pub fn total(&self) -> usize {
        self.trechos + self.centroides
    }
}

/// O que uma busca precisa, lido do `rag.json` vivo. Agrupado porque as
/// escalas andam sempre juntas: uma busca com o `k` de um ajuste e o `piso` de
/// outro é exatamente a resposta que não se pode reproduzir.
///
/// `Copy` porque quem completa a busca é o `Custodio`: as sete palavras são
/// referências e números, e a lista de notas lida do disco entra por uma cópia,
/// sem mover nada de quem chamou.
#[derive(Clone, Copy)]
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
    /// As notas que existem na base neste momento, lidas do disco no instante da
    /// busca. É a resposta da terceira fonte à pergunta que `raizes` responde para
    /// o arquivo e `ignorar` para a conversa: a nota ainda está na lista? Apagar
    /// o `.md` por fora tira o texto da resposta antes de tirar do índice, e
    /// nenhum `rag.json` precisa ter sido tocado para isso.
    pub documentos: &'a [String],
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

/// A prova de que o índice gravado ainda representa esta nota: os bytes do texto
/// que entra no índice — não os do arquivo, que têm a linha de título e o
/// marcador no meio — e o instante em que o disco disse que ela foi escrita.
/// Os dois números vêm do arquivo lido, e é por eles que um `.md` trocado por
/// fora do aplicativo é reconhecido como trabalho novo na sincronização seguinte.
fn prova_da_nota(nota: &Nota) -> Assinatura {
    Assinatura {
        bytes: nota.para_indexar().len() as u64,
        modificado: nota.atualizado,
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
    /// Notas que o índice lembra, e quantos trechos vieram delas. Lidos dos mapas
    /// gravados, nunca da pasta: a pergunta da tela é o que a memória sabe.
    notas: usize,
    trechos_notas: usize,
    /// Notas presas na ficha — o que o índice viu da última sincronização.
    fixadas: usize,
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
                documentos: BTreeMap::new(),
                trechos: Vec::new(),
                clusteres: None,
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
        // As notas também foram cortadas na escala antiga. Esquecer a ficha delas
        // junto é o que faz a base voltar para a fila na próxima sincronização,
        // em vez de ficar prometendo um texto que não existe mais.
        self.meta.documentos.clear();
        self.vetores.clear();
        // Escala nova, texto novo, vetor novo: o mapa de baldes contado sobre as
        // peças antigas não diz nada sobre as que vão entrar.
        self.meta.clusteres = None;
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

    /// Esquece uma nota inteira: as peças dela e a ficha. É o que acontece quando
    /// o dono apaga o `.md` — da lista para o índice o caminho é só este, e ele é
    /// o par exato de [`Indice::esquecer_conversa`].
    pub fn esquecer_documento(&mut self, id: &str) -> usize {
        let saíram = self.remover(Fonte::Documento, id);
        self.meta.documentos.remove(id);
        saíram
    }

    /// Notas que o índice lembra. A tela compara com a pasta `base/` para o dono
    /// ver se a memória está atrás do que ele escreveu.
    pub fn documentos(&self) -> usize {
        self.meta.documentos.len()
    }

    /// Quantas notas lembradas estão presas. É lido da ficha, não do disco: a
    /// pergunta do `Estado` é "o que o índice sabe", e o índice sabe o que viu na
    /// última sincronização.
    pub fn fixadas(&self) -> usize {
        self.meta.documentos.values().filter(|f| f.fixada).count()
    }

    /// Se a nota já está no índice exatamente como a lista a descreve. Ficha
    /// truncada nunca está vista, pela mesma razão da conversa: falta texto dela.
    fn nota_vista(&self, id: &str, prova: &Assinatura) -> bool {
        self.meta
            .documentos
            .get(id)
            .is_some_and(|ficha| !ficha.truncada && ficha.prova == *prova)
    }

    /// Quantas notas pedem trabalho: as novas, as que mudaram de texto e as que
    /// saíram da pasta. Zero é o turno comum, e é ele que não custa vetor nenhum.
    pub fn notas_pendentes(&self, notas: &[Nota]) -> usize {
        let fora = self
            .meta
            .documentos
            .keys()
            .filter(|id| !notas.iter().any(|n| n.id == **id))
            .count();
        let mudaram = notas
            .iter()
            .filter(|n| !self.nota_vista(&n.id, &prova_da_nota(n)))
            .count();
        fora + mudaram
    }

    /// Esquece toda nota que não está mais na pasta. Devolve quantas saíram.
    pub fn reter_notas(&mut self, notas: &[Nota]) -> usize {
        let fora: Vec<String> = self
            .meta
            .documentos
            .keys()
            .filter(|id| !notas.iter().any(|n| *n.id == **id))
            .cloned()
            .collect();
        let mut saíram = 0usize;
        for id in fora {
            saíram += self.esquecer_documento(&id);
        }
        saíram
    }

    /// Troca a memória de uma nota inteira: saem as peças lembradas dela, entram
    /// as de agora com a ficha que o cartão mostra. Reescrever a nota toda, e não
    /// só o que mudou, é o que faz um `.md` editado por fora não deixar peça órfã
    /// apontando para uma linha que não existe mais.
    fn substituir_documento(
        &mut self,
        nota: &Nota,
        prova: Assinatura,
        pecas: &[(Peca, Vec<f32>)],
        truncada: bool,
    ) {
        self.remover(Fonte::Documento, &nota.id);
        for (peca, vetor) in pecas {
            self.anexar(
                Trecho {
                    arquivo: nota.id.clone(),
                    linha: peca.linha,
                    texto: peca.texto.clone(),
                    fonte: Fonte::Documento,
                },
                vetor,
            );
        }
        self.meta.documentos.insert(
            nota.id.clone(),
            FichaDocumento {
                titulo: nota.titulo.clone(),
                atualizada: nota.atualizado,
                fixada: nota.fixada,
                prova,
                truncada,
            },
        );
    }

    /// A primeira peça de cada nota presa, na ordem da última escrita — a mais
    /// mexida primeiro. Não passa por busca nenhuma: é por isto que a nota fixada
    /// entra no prompt mesmo quando a pergunta não tem nada a ver com ela, e é por
    /// isto que o `ignorar` do turno não a tira (o `ignorar` corta conversa, não
    /// decisão do dono).
    ///
    /// Zero peças quando a nota ainda não foi indexada: preso é o que o índice
    /// sabe, e um cartão inventado sobre uma nota que ninguém vetorou seria a
    /// tela prometendo memória que não existe.
    ///
    /// A primeira peça de cada nota é achada numa passada só sobre os trechos,
    /// guardando `(linha, índice)` por nota. Varrer o índice dentro do laço das
    /// notas presas custava `presas × 12.000` comparações em cada turno — doze
    /// notas presas eram a busca inteira de volta, e de graça, sem nem medir um
    /// vetor. O `min` mantém o primeiro de cada linha repetida, que é o que
    /// `min_by_key` sobre a ordem de varredura devolvia.
    pub fn pecas_fixadas(&self) -> Vec<Achado> {
        let mut presas: Vec<(&String, &FichaDocumento)> = self
            .meta
            .documentos
            .iter()
            .filter(|(_, ficha)| ficha.fixada)
            .collect();
        if presas.is_empty() {
            return Vec::new();
        }
        let fixadas: BTreeSet<&str> = presas.iter().map(|(id, _)| id.as_str()).collect();
        let mut primeira: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
        for (indice, trecho) in self.meta.trechos.iter().enumerate() {
            if trecho.fonte != Fonte::Documento || !fixadas.contains(trecho.arquivo.as_str()) {
                continue;
            }
            let entrada = primeira
                .entry(trecho.arquivo.as_str())
                .or_insert((trecho.linha, indice));
            if trecho.linha < entrada.0 {
                *entrada = (trecho.linha, indice);
            }
        }
        presas.sort_by(|a, b| b.1.atualizada.cmp(&a.1.atualizada).then(a.0.cmp(b.0)));
        presas
            .into_iter()
            .filter_map(|(id, ficha)| {
                let (linha, indice) = *primeira.get(id.as_str())?;
                let trecho = &self.meta.trechos[indice];
                Some(Achado {
                    nome: ficha.titulo.clone(),
                    arquivo: id.clone(),
                    linha,
                    texto: trecho.texto.clone(),
                    // Não medido: ninguém procurou este trecho, ele estava preso.
                    pontuacao: 0.0,
                    origem: Origem::Fixada,
                    fonte: Fonte::Documento,
                    quando: Some(ficha.atualizada),
                })
            })
            .collect()
    }

    /// Tira do índice os trechos de uma chave, e só os da fonte pedida. As duas
    /// fontes têm chaves que não se encontram — caminho cheio contra id de
    /// conversa — mas é a fonte que decide, para a varredura de pastas não mexer
    /// no histórico nem a reescrita de uma conversa mexer nos arquivos.
    fn remover(&mut self, fonte: Fonte, chave: &str) -> usize {
        let antes = self.meta.trechos.len();
        let mut novos_trechos: Vec<Trecho> = Vec::with_capacity(antes);
        let mut novos_vetores: Vec<f32> = Vec::with_capacity(self.vetores.len());
        // O balde acompanha o trecho que sai. Deslocar o índice de todos os que
        // ficam sem deslocar o mapa deixaria cada vetor apontando para o vizinho —
        // e o pré-filtro cortaria baldes pelo mapa de uma casa que não existe mais.
        let grupo = self.meta.clusteres.take().filter(|g| g.balde.len() == antes);
        let mut novo_balde: Vec<u32> = Vec::with_capacity(grupo.as_ref().map_or(0, |g| g.balde.len()));
        let dimensao = self.meta.dimensao;
        for (indice, trecho) in self.meta.trechos.iter().enumerate() {
            if trecho.fonte != fonte || trecho.arquivo != chave {
                novos_trechos.push(trecho.clone());
                let inicio = indice * dimensao;
                novos_vetores.extend_from_slice(&self.vetores[inicio..inicio + dimensao]);
                if let Some(mapa) = &grupo {
                    novo_balde.push(mapa.balde[indice]);
                }
            }
        }
        let saíram = antes - novos_trechos.len();
        self.meta.trechos = novos_trechos;
        self.vetores = novos_vetores;
        if let Some(mut mapa) = grupo {
            // Os `raios` ficam como estavam: são o máximo sobre um conjunto que
            // agora é menor, então continuam teto. O preço de não recontar é
            // sondar um balde a mais numa busca, nunca um trecho a menos.
            mapa.balde = novo_balde;
            self.meta.clusteres = Some(mapa);
        }
        saíram
    }

    /// Esquece tudo que não está mais dentro de uma pasta concedida. É isto que
    /// faz o fio da memória perder o conteúdo de uma pasta revogada no grafo.
    ///
    /// Só arquivos. Este é exatamente o laço onde a política das fontes não pode
    /// se misturar: as notas vivem na pasta `base/` do aplicativo, que não é
    /// concedida a ninguém, e uma conversa não tem pasta nenhuma. Passar uma das
    /// duas por aqui seria a revogação de um diretório apagando o que o dono
    /// escreveu — o `Documento nunca passa pelo reter_dentro` é o teste.
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
            self.anexar(
                Trecho {
                    arquivo: arquivo.to_string(),
                    linha: peca.linha,
                    texto: peca.texto.clone(),
                    fonte: Fonte::Arquivo,
                },
                vetor,
            );
        }
        self.meta.assinaturas.insert(arquivo.to_string(), assinatura);
    }

    /// A única porta por que um trecho entra no índice. Existe para a partição IVF
    /// poder continuar verdadeira entre duas escritas: vetor novo sem balde é vetor
    /// invisível para o pré-filtro, e um `raio` que não cobre o recém-chegado faria
    /// a busca parar antes de achá-lo — que é perda de resposta, não de tempo.
    /// Sem mapa contado (índice pequeno, recém-criado ou vindo de um arquivo de
    /// antes disto), anexar é só anexar: quem conta o mapa é `reagrupar`, no fim do
    /// trabalho de escrita, nunca dentro de um turno.
    fn anexar(&mut self, trecho: Trecho, vetor: &[f32]) {
        self.meta.trechos.push(trecho);
        self.vetores.extend_from_slice(vetor);
        let dimensao = self.meta.dimensao;
        // Um mapa contado noutra largura não diz nada deste vetor, e um mapa
        // parcial é pior que nenhum: o trecho ficaria num balde que não existe
        // enquanto o teto dele continuasse prometendo cobri-lo. Ficar sem mapa é
        // voltar à varredura inteira — lenta, mas nunca mentiu sobre a lista.
        let Some(mut grupo) = self.meta.clusteres.take().filter(|g| g.dimensao == dimensao) else {
            return;
        };
        let melhor = melhor_centroide(&grupo.centroide, vetor, grupo.quantos, dimensao);
        // A distância ao centroide cru sai de ‖x − c‖² = 1 − 2·‖c‖·⟨x, ĉ⟩ + ‖c‖², com
        // ‖x‖ = 1: é a mesma conta que o teto da busca lê, então o balde nunca é
        // bordado por um número que a busca não usaria.
        let inicio = melhor * dimensao;
        let proje = produto(&grupo.centroide[inicio..inicio + dimensao], vetor);
        let norma = grupo.norma[melhor];
        let residual = (1.0 - 2.0 * norma * proje + norma * norma).max(0.0).sqrt();
        grupo.balde.push(melhor as u32);
        grupo.raio[melhor] = grupo.raio[melhor].max(residual + CLUSTER_FOLGA);
        self.meta.clusteres = Some(grupo);
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
            self.anexar(
                Trecho {
                    arquivo: viva.id.clone(),
                    linha: peca.linha + viva.a_partir_de,
                    texto: peca.texto.clone(),
                    fonte: Fonte::Conversa,
                },
                vetor,
            );
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

    /// A política de cada fonte resolvida uma única vez por consulta, num vetor de
    /// booleanos. Antes era um fecho chamado dentro do laço dos vetores e outra vez
    /// sobre a lista léxica: `concedida` compara caminho byte a byte, e chamá-la duas
    /// vezes por trecho, em cada pergunta, custava mais que a parte da busca que o
    /// pré-filtro veio tirar.
    ///
    /// Quem responde se um trecho pode ser devolvido é a fonte dele: o arquivo obedece
    /// às pastas concedidas, a conversa obedece à lista de conversas e ao `ignorar`
    /// do turno, e a nota obedece à lista de notas da pasta `base/`. Nenhuma das três
    /// empresta política para as outras duas.
    fn liberados(&self, busca: &Busca, raizes: &[PathBuf]) -> Vec<bool> {
        self.meta
            .trechos
            .iter()
            .map(|trecho| match trecho.fonte {
                Fonte::Arquivo => concedida(&trecho.arquivo, raizes),
                Fonte::Conversa => !busca.ignorar.iter().any(|id| *id == trecho.arquivo),
                // A nota ainda existe? O `ignorar` do turno não passa por aqui:
                // ele corta o que a conversa já tem no buffer, e uma nota curada
                // não está em buffer nenhum.
                Fonte::Documento => busca
                    .documentos
                    .iter()
                    .any(|id| *id == trecho.arquivo),
            })
            .collect()
    }

    /// O cosseno de um trecho contra a consulta, com a medição contabilizada. Os dois
    /// lados estão normatizados — é o contrato de `normalizar` — então o produto
    /// escalar é a similaridade que o cartão mostra.
    fn medir(&self, indice: usize, consulta: &[f32], conta: &mut Contagem) -> f32 {
        conta.trechos += 1;
        let inicio = indice * self.meta.dimensao;
        produto(&self.vetores[inicio..inicio + self.meta.dimensao], consulta)
    }

    /// O mapa de baldes que vale para este índice, se vale. As condições são de
    /// correção, não de gosto:
    ///
    /// * o índice tem [`CLUSTER_MINIMO`] trechos ou mais — abaixo disto varrer é mais
    ///   barato que consultar o mapa, e é o caso em que a partição mudaria a cada
    ///   turno;
    /// * o mapa cobre um balde por trecho existente, na largura de hoje: vetor sem
    ///   balde é vetor que o pré-filtro cortaria da lista;
    /// * o tanto que a busca precisa de candidatos cabe no índice — pedir mais que
    ///   existe é pedir a casa inteira, e aí o mapa não economiza nada.
    ///
    /// `None` não é falha nenhuma: é o índice gravado antes disto existir, e o caminho
    /// dele continua sendo a varredura exaustiva. Escolhemos **não** reconstruir o
    /// mapa dentro de `buscar` de propósito: k-means sobre doze mil vetores de mil
    /// dimensões custa segundos, e segundos não cabem na resposta a um usuário — a
    /// varredura e a sincronização é que chamam [`Indice::reagrupar`], no fim do
    /// trabalho de escrita, onde o custo já é invisível. Um índice antigo responde
    /// devagar; um turno travado reorganizando a casa é outra coisa.
    fn particao(&self, necessario: usize) -> Option<&Clusteres> {
        let mapa = self.meta.clusteres.as_ref()?;
        let total = self.meta.trechos.len();
        if necessario == 0
            || total < CLUSTER_MINIMO
            || necessario >= total
            || mapa.dimensao != self.meta.dimensao
            || mapa.quantos == 0
            || mapa.balde.len() != total
            || mapa.centroide.len() != mapa.quantos * mapa.dimensao
            || mapa.norma.len() != mapa.quantos
            || mapa.raio.len() != mapa.quantos
        {
            return None;
        }
        Some(mapa)
    }

    /// A varredura particionada: ordena os baldes pelo teto que a borda de cada um
    /// dá, sonda do mais promissor para o menos e para quando o teto cai abaixo do
    /// que ainda pode entrar na lista. Preenche `proximidade` e `medido` só do que
    /// sondou — o resto do índice fica provadamente de fora.
    fn sondar(
        &self,
        mapa: &Clusteres,
        busca: &Busca,
        liberado: &[bool],
        necessario: usize,
        proximidade: &mut [f32],
        medido: &mut [bool],
        conta: &mut Contagem,
    ) {
        let dimensao = self.meta.dimensao;
        // O teto de um balde, para todo `x` dele: `⟨q,x⟩ ≤ ‖c‖·⟨q,ĉ⟩ + max‖x − c‖`.
        // É Cauchy–Schwarz sobre a residual, com `‖q‖ = 1`, e é a única coisa que
        // autoriza parar sem ter olhado: o balde que fica para trás não
        // *provavelmente* não tem ninguém bom — ele não tem. `CLUSTER_FOLGA` cobre o
        // arredondamento dos dois termos, que pode deixar o valor verdadeiro um ticho
        // acima do teto contado.
        let mut tetos: Vec<(f32, usize)> = Vec::with_capacity(mapa.quantos);
        for balde in 0..mapa.quantos {
            let inicio = balde * dimensao;
            conta.centroides += 1;
            let proje = produto(&mapa.centroide[inicio..inicio + dimensao], busca.vetor);
            tetos.push((mapa.norma[balde] * proje + mapa.raio[balde] + CLUSTER_FOLGA, balde));
        }
        // Empate de teto cai para o balde menor, a mesma regra dos empates de
        // colocação e de cosseno: uma busca que escolhe a ordem de duas coisas iguais
        // pelo acaso não é reproduzível.
        tetos.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));

        // Os membros de cada balde, numa passada só e já filtrada pela concessão. O
        // balde entrega candidatos, nunca resposta: a permissão continua decidindo
        // item a item, e um balde inteiro revogado sai da contagem sem deixar rastro
        // na lista — o que ele continha estaria fora também na varredura exaustiva.
        let mut membros: Vec<Vec<u32>> = vec![Vec::new(); mapa.quantos];
        for (indice, &balde) in mapa.balde.iter().enumerate() {
            let balde = balde as usize;
            if balde < mapa.quantos && liberado[indice] {
                membros[balde].push(indice as u32);
            }
        }

        // Os `necessario` maiores cossenos medidos até aqui, do maior para o menor. O
        // último deles é o chão abaixo do qual ninguém mais entra na lista, e é contra
        // ele — e contra o piso — que o teto de cada balde é testado.
        let mut melhores: Vec<f32> = Vec::with_capacity(necessario + 1);
        for (teto, balde) in tetos {
            let corte = if melhores.len() >= necessario {
                busca.piso.max(melhores[necessario - 1])
            } else {
                busca.piso
            };
            if teto < corte {
                // Os tetos vêm em ordem decrescente: nem este balde nem os que ficam
                // depois têm como conter alguém da lista.
                break;
            }
            for &indice in &membros[balde] {
                let indice = indice as usize;
                let similaridade = self.medir(indice, busca.vetor, conta);
                proximidade[indice] = similaridade;
                medido[indice] = true;
                oferecer(&mut melhores, necessario, similaridade);
            }
        }
    }

    /// Conta o mapa de baldes do índice. É k-means com semente determinística, e é
    /// caro: por isso só é chamado no fim de um trabalho de escrita, pela varredura,
    /// pela sincronização da base ou por uma revogação — nunca dentro de um turno.
    ///
    /// Reaproveita o mapa que já existe enquanto ele ainda cobre metade do índice:
    /// recontar as bordas custa uma passada sobre os vetores, recontar o mapa inteiro
    /// custa sete. Deixar o mapa velho apodrecer e serrar a busca em baldes que já não
    /// contêm o que dizem é o que perderia resposta, e é contra isso que as três
    /// checagens de `particao` trabalham.
    fn reagrupar(&mut self) {
        let total = self.meta.trechos.len();
        let dimensao = self.meta.dimensao;
        if dimensao == 0 || total < CLUSTER_MINIMO {
            // Índice pequeno: sem mapa, sem pré-filtro, varredura inteira. O custo de
            // manter um mapa vivo num índice que muda a cada turno não se paga.
            self.meta.clusteres = None;
            return;
        }
        let cobrindo = self
            .meta
            .clusteres
            .as_ref()
            .is_some_and(|g| g.dimensao == dimensao && g.balde.len() == total && g.trechos * 2 >= total);
        if cobrindo {
            self.reapertar();
            return;
        }
        let quantos = ((total as f64).sqrt() as usize)
            .clamp(2, CLUSTER_MAXIMO)
            .min(total - 1);
        let fatia = |indice: usize| &self.vetores[indice * dimensao..indice * dimensao + dimensao];
        // Amostra por passo fixo sobre o índice inteiro. Doze mil vetores treinando
        // custariam mais que a busca que o mapa acelera, e o passo — em vez de um
        // sorteio — é o que faz a mesma casa dar o mesmo mapa em duas execuções.
        //
        // O passo tem de ser coprimo com o tamanho, e isto não é preciosismo: as peças
        // se repetem num período (peças por arquivo, arquivos por pasta), e um passo
        // múltiplo dele amostra sempre a mesma posição dentro de cada repetição.
        // Medido com o passo ingênuo sobre doze mil peças em doze grupos — o quociente
        // caía em 12, exatamente o período — os 1.024 pontos saíam todos de um grupo
        // só, os baldes ficavam misturados, o raio de cada um virava a casca do índice
        // inteiro e a sondagem refezia 11.647 dos 12.000 produtos: mapa existindo e não
        // cortando nada.
        let passo = passo_da_amostra(total, CLUSTER_AMOSTRA);
        let amostra: Vec<usize> = (0..CLUSTER_AMOSTRA.min(total))
            .map(|k| (k * passo) % total)
            .collect();

        // Semente: o primeiro da amostra e, um a um, o ponto menos coberto pelos já
        // escolhidos. É a boca larga do k-means++ sem dado nenhum.
        let mut centroide: Vec<f32> = Vec::with_capacity(quantos * dimensao);
        centroide.extend_from_slice(fatia(amostra[0]));
        let mut usado = vec![false; amostra.len()];
        usado[0] = true;
        let mais_perto = |centroide: &[f32], escolhido: usize| {
            let inicio = escolhido * dimensao;
            amostra
                .iter()
                .map(|&indice| produto(&centroide[inicio..inicio + dimensao], fatia(indice)))
                .collect::<Vec<f32>>()
        };
        let mut cobertura = mais_perto(&centroide, 0);
        for escolhido in 1..quantos {
            let novo = (0..amostra.len())
                .filter(|&p| !usado[p])
                .min_by(|a, b| cobertura[*a].total_cmp(&cobertura[*b]).then(a.cmp(b)))
                .unwrap_or(0);
            usado[novo] = true;
            centroide.extend_from_slice(fatia(amostra[novo]));
            let novo_proje = mais_perto(&centroide, escolhido);
            for (posicao, atual) in cobertura.iter_mut().enumerate() {
                *atual = atual.max(novo_proje[posicao]);
            }
        }

        // Lloyd esférico: os vetores são unitários, então a partição por cosseno é a
        // mesma partição por distância euclidiana até um fator constante, e a direção
        // média é o centroide que o teto da busca consulta.
        for _ in 0..CLUSTER_ITERACOES {
            let mut soma = vec![0f32; quantos * dimensao];
            let mut contagem = vec![0u32; quantos];
            for &indice in &amostra {
                let vetor = fatia(indice);
                let melhor = melhor_centroide(&centroide, vetor, quantos, dimensao);
                let inicio = melhor * dimensao;
                for (acumulada, v) in soma[inicio..inicio + dimensao].iter_mut().zip(vetor) {
                    *acumulada += *v;
                }
                contagem[melhor] += 1;
            }
            for balde in 0..quantos {
                if contagem[balde] == 0 {
                    // Balde que a amostra esvaziou mantém a direção: descartá-la aqui
                    // abriria um buraco no mapa, e o mapa tem de continuar cobrindo o
                    // índice inteiro.
                    continue;
                }
                let inicio = balde * dimensao;
                let quadrado: f32 = soma[inicio..inicio + dimensao].iter().map(|v| v * v).sum();
                let tamanho = quadrado.sqrt();
                if !(tamanho > 1e-9) {
                    continue;
                }
                for v in soma[inicio..inicio + dimensao].iter_mut() {
                    *v /= tamanho;
                }
                centroide[inicio..inicio + dimensao].copy_from_slice(&soma[inicio..inicio + dimensao]);
            }
        }

        // Atribuição e bordas sobre o índice inteiro. É aqui que o mapa deixa de ser
        // a amostra: a busca só tem direito de parar antes de um balde se a borda
        // daquele balde for contada sobre os vetores que realmente estão nele.
        let mut balde = vec![0u32; total];
        let mut soma = vec![0f32; quantos * dimensao];
        let mut contagem = vec![0u32; quantos];
        for indice in 0..total {
            let vetor = fatia(indice);
            let melhor = melhor_centroide(&centroide, vetor, quantos, dimensao);
            balde[indice] = melhor as u32;
            let inicio = melhor * dimensao;
            for (acumulada, v) in soma[inicio..inicio + dimensao].iter_mut().zip(vetor) {
                *acumulada += *v;
            }
            contagem[melhor] += 1;
        }
        let mut norma = vec![0f32; quantos];
        let mut raio = vec![CLUSTER_FOLGA; quantos];
        for direcao in 0..quantos {
            let inicio = direcao * dimensao;
            if contagem[direcao] == 0 {
                // Nenhum vetor nesta direção: o teto sai `CLUSTER_FOLGA`, o balde é
                // sondado uma vez por consulta e não mede nada. Mais honesto que
                // sumir do mapa e deixar o índice com um buraco.
                centroide[inicio..inicio + dimensao].fill(0.0);
                continue;
            }
            let cont = contagem[direcao] as f32;
            let membros = &mut soma[inicio..inicio + dimensao];
            let mut quadrado = 0f32;
            for v in membros.iter_mut() {
                *v /= cont;
                quadrado += *v * *v;
            }
            let tamanho = quadrado.sqrt();
            norma[direcao] = tamanho;
            if tamanho > 1e-9 {
                for posicao in 0..dimensao {
                    centroide[inicio + posicao] = membros[posicao] / tamanho;
                }
            }
        }
        for (indice, &b) in balde.iter().enumerate() {
            let b = b as usize;
            if norma[b] <= 0.0 {
                continue;
            }
            let inicio = b * dimensao;
            // `‖x − c‖` lido de `⟨x, ĉ⟩` e `‖c‖`, sem montar o vetor da média outra
            // vez: `‖x‖ = 1`, então sai da mesma identidade usada em `anexar`.
            let proje = produto(&centroide[inicio..inicio + dimensao], fatia(indice));
            let tamanho = norma[b];
            let residual = (1.0 - 2.0 * tamanho * proje + tamanho * tamanho).max(0.0).sqrt();
            if residual + CLUSTER_FOLGA > raio[b] {
                raio[b] = residual + CLUSTER_FOLGA;
            }
        }
        self.meta.clusteres = Some(Clusteres {
            quantos,
            dimensao,
            trechos: total,
            centroide,
            norma,
            raio,
            balde,
        });
    }

    /// Reconta as bordas do mapa sem tocar nas direções: uma passada sobre os
    /// vetores, sem k-means nenhum. É o que aperta o teto de um balde depois das
    /// remoções — que só afrouxam o raio gravado — e das anexações que o enlargaram,
    /// e é por isso que a busca continua sondando baldes apertados em vez de uma
    /// casca grossa que a levaria a medir o índice todo de novo.
    fn reapertar(&mut self) {
        let dimensao = self.meta.dimensao;
        let total = self.meta.trechos.len();
        let Some(mut mapa) = self
            .meta
            .clusteres
            .take()
            .filter(|g| g.dimensao == dimensao && g.balde.len() == total && g.quantos > 0)
        else {
            return;
        };
        let mut raio = vec![CLUSTER_FOLGA; mapa.quantos];
        for (indice, &balde) in mapa.balde.iter().enumerate() {
            let balde = balde as usize;
            if balde >= mapa.quantos {
                continue;
            }
            let inicio = balde * dimensao;
            let proje = produto(
                &mapa.centroide[inicio..inicio + dimensao],
                &self.vetores[indice * dimensao..indice * dimensao + dimensao],
            );
            let tamanho = mapa.norma[balde];
            let residual = (1.0 - 2.0 * tamanho * proje + tamanho * tamanho).max(0.0).sqrt();
            if residual + CLUSTER_FOLGA > raio[balde] {
                raio[balde] = residual + CLUSTER_FOLGA;
            }
        }
        mapa.raio = raio;
        self.meta.clusteres = Some(mapa);
    }

    /// Os `k` melhores acima do `piso`, na ordem que o `modo` manda, e só do que
    /// ainda pode ser lembrado: os arquivos das pastas concedidas em `raizes`, as
    /// conversas que `ignorar` não corta e as notas que ainda estão em
    /// `documentos`.
    ///
    /// O cosseno é sempre medido, mesmo em modo léxico: ele é o número do
    /// cartão, e um achado que chegou pela palavra continua tendo o vetor dele no
    /// índice. O que muda com o modo é a *ordem*, não a medida.
    ///
    /// Com um mapa de baldes gravado ([`Clusteres`]), a busca deixa de medir o índice
    /// inteiro: sonda os baldes do teto mais promissor para o menor e para quando o
    /// teto cai abaixo do que ainda pode entrar na lista. A lista sai igual à da
    /// varredura completa — é o que a desigualdade de [`Clusteres`] garante, e o que
    /// `o_particionamento_devolve_a_mesma_lista` cobra. O único número que o corte
    /// muda é `alcancados`, que passa a contar o que a sondagem alcançou: ver
    /// [`Indice::buscar_contando`].
    pub fn buscar(&self, busca: &Busca, raizes: &[PathBuf]) -> Result<Resposta> {
        self.buscar_contando(busca, raizes).map(|(resposta, _)| resposta)
    }

    /// A mesma busca com a conta dos produtos escalares que ela fez. Existe porque
    /// `Resposta` não tem onde pôr o número: os dois consumidores do contrato
    /// (`commands.rs` e a ferramenta `buscar_memoria`) desmontam a struct campo por
    /// campo, e um campo novo ali seria a API pública mudando por causa de uma
    /// medição da casa. É por esta porta que se pode dizer quantos vetores uma
    /// pergunta atravessa, e não por uma estimativa de comentário.
    pub fn buscar_contando(
        &self,
        busca: &Busca,
        raizes: &[PathBuf],
    ) -> Result<(Resposta, Contagem)> {
        let mut conta = Contagem::default();
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
        let total = self.meta.trechos.len();
        let liberado = self.liberados(busca, raizes);
        // Quantos candidatos a lista vetorial precisa ter para que a ordem final saia
        // igual à da varredura inteira. No modo misto quem corta é `FUSAO_MAXIMA`,
        // não o `k`: são as primeiras posições que `fundir` lê do vetor.
        let necessario = match busca.modo {
            // O léxico não ordena por vetor nenhum: o cartão mostra o cosseno de quem
            // a palavra chamou, e medir os outros seria a casa inteira por meia dúzia
            // de cartas.
            Modo::Lexico => 0,
            Modo::Vetorial => busca.k.max(1),
            Modo::Hibrido => busca.k.max(FUSAO_MAXIMA),
        };
        let mut proximidade = vec![0f32; total];
        let mut medido = vec![false; total];
        let mut vetorial: Vec<usize> = Vec::new();
        if necessario > 0 {
            match self.particao(necessario) {
                None => {
                    for indice in 0..total {
                        if !liberado[indice] {
                            continue;
                        }
                        proximidade[indice] = self.medir(indice, busca.vetor, &mut conta);
                        medido[indice] = true;
                    }
                }
                Some(mapa) => self.sondar(
                    mapa,
                    busca,
                    &liberado,
                    necessario,
                    &mut proximidade,
                    &mut medido,
                    &mut conta,
                ),
            }
            // A lista nasce na ordem de `meta.trechos`, como no caminho exaustivo:
            // quem resolve o empate de cosseno é o `sort_by` estável, jogando para o
            // índice menor — e isso tem de valer igual dos dois lados da porta, ou a
            // mesma pergunta daria duas respostas.
            vetorial = (0..total)
                .filter(|&indice| {
                    liberado[indice]
                        && medido[indice]
                        && proximidade[indice] >= busca.piso
                })
                .collect();
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
                .filter(|(_, indice)| liberado[*indice])
                .collect::<Vec<(f32, usize)>>();
            // Empate de BM25 vai para o índice menor: sem desempate fixo a mesma
            // pergunta daria listas diferentes, e nenhuma medição do cartão
            // seria comparável de uma execução para outra.
            marcados.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
            // O cosseno do cartão é medido para quem ainda tem como chegar à ordem
            // final — os que passam de `FUSAO_MAXIMA` não entram em lista que a
            // fusão lê, e medir um vetor por palavra rara que ficou em centésimo
            // seria a varredura inteira de volta para responder oito cartas.
            for (_, indice) in marcados.iter().take(FUSAO_MAXIMA.max(busca.k)) {
                if !medido[*indice] {
                    proximidade[*indice] = self.medir(*indice, busca.vetor, &mut conta);
                    medido[*indice] = true;
                }
            }
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
        // `alcancados` com o pré-filtro ativo é o que a sondagem alcançou, não o que
        // a casa tem: quando ela para porque já tem o `necessario` melhor, pode
        // existir um trecho acima do piso num balde não sondado. A pergunta que o
        // número responde — "a lista parou porque acabou ou porque o `k` mandou?" —
        // continua certa: ou a sondagem varreu os baldes todos até o piso (e o
        // número é exato), ou ela parou com pelo menos `FUSAO_MAXIMA` na mão, e
        // nesse caso havia mais do que os `k` pedidos por construção.
        let resposta = Resposta {
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
                        // O nome do achado de uma nota é o título dela, lido da
                        // ficha gravada junto com o trecho — nunca deduzido do id:
                        // o `id` é o nome do arquivo, e o dono pode ter trocado o
                        // `#` de lugar depois de indexar.
                        Fonte::Documento => self
                            .meta
                            .documentos
                            .get(&trecho.arquivo)
                            .map(|ficha| (ficha.titulo.clone(), Some(ficha.atualizada)))
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
        };
        Ok((resposta, conta))
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
        notas: indice.documentos(),
        trechos_notas: indice.trechos_de(Fonte::Documento),
        fixadas: indice.fixadas(),
        dimensao: indice.dimensao(),
        agrupamento: indice.agrupamento().to_string(),
        peca,
        sobreposicao,
    }
}

/// A cópia que entra no lugar de um índice gravado. O vocabulário léxico nunca
/// é copiado de propósito: as peças novas valem do zero, e um cache que
/// sobrevivesse a uma reescrita responderia por texto que não está mais lá.
/// `Arc::make_mut` usa esta cópia quando o snapshot anterior ainda está
/// publicado ou segurado por outra referência; com exclusividade, o escritor
/// muta a própria alocação.
impl Clone for Indice {
    fn clone(&self) -> Self {
        Indice {
            meta: self.meta.clone(),
            vetores: self.vetores.clone(),
            lexico: OnceLock::new(),
        }
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

/// O que uma sincronização da base fez, medido durante ela.
///
/// Quatro números e um tempo, todos contados por quem os escreveu: `notas` é o
/// tamanho da lista que foi dada (não o da pasta — a pasta é o que a função lê, e
/// ler duas vezes daria duas respostas se alguém escrevesse no meio), `pecas` são
/// as que entraram, `removidas` as que saíram porque o arquivo deixou de existir
/// e `tokens` os que o codificador efetivamente comeu. Zero em `pecas` com
/// `notas` maior que zero tem duas respostas honestas no `estado`: ou a pasta já
/// estava lembrada, ou não há codificador instalado.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Sincronia {
    pub notas: usize,
    pub pecas: usize,
    pub removidas: usize,
    pub tokens: usize,
    pub ms: u64,
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
    /// Notas que o índice lembra, dos arquivos que ele leu na última
    /// sincronização. Comparado com `base_estado.notas` é o que a tela diz: a base
    /// tem N, a memória sabe de M.
    pub notas: usize,
    /// Dos `indexados`, quantos são peças de nota.
    pub trechos_notas: usize,
    /// Notas presas na ficha do índice. Não é `base::listar`: é o que a memória
    /// sabe que estava preso quando ela leu.
    pub fixadas: usize,
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

/// O vetor de uma pergunta e tudo que decide esse vetor. Não é a pergunta sozinha:
/// o mesmo texto embutido por outro modelo, com outro agrupamento, com outro corte
/// de lote ou com outro prefixo de consulta é **outro vetor**, e devolver o primeiro
/// seria a resposta procurando vizinhança num espaço que não é mais o da pergunta.
/// Entram por isto as quatro coisas que o [`Carregado`] pode trocar por baixo de uma
/// busca, mais o prefixo que é do modelo e não do texto.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ChaveConsulta {
    modelo: String,
    agrupamento: String,
    dimensao: usize,
    lote: usize,
    prefixo: Option<&'static str>,
    texto: String,
}

/// O LRU dos vetores de consulta. Uma lista ordenada, mais recente na frente: a
/// casa tem no máximo [`CACHE_CONSULTAS`] vetores de fora, e procurar neles é varrer
/// sessenta e quatro chaves — nada contra os milhões de multiplicar que uma consulta
/// poupa. Um `HashMap` seria mais rápido e não serviria: sem ordem de uso não há LRU,
/// e sem LRU o cache de uma conversa longa cresce até o teto de memória do processo.
///
/// A largura é [`CACHE_CONSULTAS`] — sessenta e quatro vetores, duzentos e
/// cinquenta quilobytes no pior codificador daqui. É o que cabe um turno inteiro de
/// reformulações, e é pequeno o bastante para nunca ser memória que alguém sinta.
#[derive(Default)]
struct CacheConsultas {
    itens: Vec<(ChaveConsulta, Vec<f32>)>,
}

impl CacheConsultas {
    /// Acha e, achando, põe na frente. O "põe na frente" é o LRU: uma consulta que
    /// se repete nunca é a mais antiga da casa, e a que sai é a mais antiga.
    fn consultar(&mut self, chave: &ChaveConsulta) -> Option<Vec<f32>> {
        let posicao = self.itens.iter().position(|(k, _)| k == chave)?;
        let item = self.itens.remove(posicao);
        self.itens.insert(0, item);
        Some(self.itens[0].1.clone())
    }

    fn inserir(&mut self, chave: ChaveConsulta, vetor: Vec<f32>) {
        self.itens.retain(|(k, _)| k != &chave);
        self.itens.insert(0, (chave, vetor));
        self.itens.truncate(CACHE_CONSULTAS);
    }

    fn limpar(&mut self) {
        self.itens.clear();
    }

    /// Derruba os vetores de um só codificador. [`Self::limpar`] serviria para o
    /// pedido de soltar, mas a casa pode ter o modelo B carregado enquanto guarda
    /// consultas do A que alguém usou antes: derrubar tudo jogaria fora o trabalho
    /// de B por um pedido que fala de A.
    fn soltar_modelo(&mut self, id: &str) {
        self.itens.retain(|(chave, _)| chave.modelo != id);
    }
}

/// O dono da memória. Vive atrás de um `Arc`, porque a varredura corre numa
/// tarefa bloqueante enquanto os comandos leem estado e a conversa busca.
pub struct Custodio {
    dlls: Vec<PathBuf>,
    models_root: PathBuf,
    caminho_indice: PathBuf,
    ajustes_path: PathBuf,
    /// A pasta da base de conhecimento (`crate::ai::base`). Um caminho só, tomado
    /// na abertura e nunca reescrito: é ele que a ferramenta, os comandos e a
    /// varredura leem, então não há segunda verdade sobre onde as notas moram.
    raiz_base: PathBuf,
    pastas: Arc<RwLock<Pastas>>,
    ajustes: Mutex<Ajustes>,
    carregado: Mutex<Option<Carregado>>,
    /// O espelho de `carregado` para leitura sem o lock do motor. `Some` é
    /// exatamente quando `carregado` tem um codificador; as duas pontas se movem
    /// juntas em `carregado()` (carga) e nos dois `= None` (descarga).
    info_motor: RwLock<Option<InfoMotor>>,
    /// O índice publicado é um snapshot imutável. Os leitores compartilham só o
    /// `Arc`; os escritores passam por `Arc::make_mut`, que copia o conteúdo se
    /// a versão anterior ainda estiver publicada ou segurada por um leitor.
    indice: RwLock<Option<Arc<Indice>>>,
    /// Trava dos escritores do índice. O `RwLock` acima só protege a publicação
    /// final; quem mexe no índice faz snapshot → mudar → gravar-disco → publicar,
    /// e sem esta trava dois escritores concorrentes (um `revogar` e a revalidação
    /// de conversas de um turno) leem a mesma cópia e o último publicar ressuscita
    /// o conteúdo que o primeiro acabou de apagar, no disco e na memória. Ordem:
    /// `escritores` antes de `carregado` e de `indice`. A varredura não pega esta
    /// trava — ela já é exclusiva por `ocupado`, e pegá-la sob o `carregado` que
    /// segura o tempo todo fecharia um ciclo com quem escreve.
    escritores: Mutex<()>,
    /// Os vetores das últimas consultas. Trava própria, e nunca segurada junto de
    /// outra: quem entra aqui já tem o `carregado` na mão — é ele que responde pela
    /// chave — e solta os dois antes de qualquer busca. Ordem documentada:
    /// `carregado` antes de `consultas`.
    consultas: Mutex<CacheConsultas>,
    ocupado: AtomicBool,
    cancelado: AtomicBool,
    progresso: RwLock<Progresso>,
    erro: Mutex<Option<String>>,
    concluido_em: Mutex<Option<u64>>,
}

/// Ordem dos locks, sem exceção: `carregado` antes de `consultas`, e `carregado`
/// antes de `indice`. Quem inverte espera para sempre, e as duas pontas são pesadas
/// o bastante para ninguém ver o deadlock acontecer.
impl Custodio {
    pub fn novo(
        dlls: &[PathBuf],
        models_root: &Path,
        raiz_memoria: &Path,
        ajustes_path: PathBuf,
        raiz_base: &Path,
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
            raiz_base: raiz_base.to_path_buf(),
            pastas,
            ajustes: Mutex::new(ajustes),
            carregado: Mutex::new(None),
            info_motor: RwLock::new(None),
            indice: RwLock::new(None),
            escritores: Mutex::new(()),
            consultas: Mutex::new(CacheConsultas::default()),
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
            // Outro codificador, outro espaço: os vetores guardados descrevem a
            // pergunta feita à casa antiga. A chave já os recusaria, e limpar é o
            // que faz a memória da casa antiga sair de vez, não esperar o LRU.
            trava_estado(&self.consultas).limpar();
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
        // "Apagar a memória" apaga também o que a casa lembra das perguntas: os
        // vetores guardados são texto do dono, transformado, e texto do dono não
        // sobrevive ao pedido de apagamento.
        trava_estado(&self.consultas).limpar();
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
        // Os vetores daquele codificador saem da casa mesmo quando ele não era o
        // carregado: a chave leva o identificador, e um texto do dono transformado
        // não pode ficar parado esperando o descarte natural do LRU depois do
        // pedido de soltar. Fora do `guard` de propósito — a ordem documentada é
        // `carregado` antes de `consultas`, e aqui as duas não se sobrepoem.
        trava_estado(&self.consultas).soltar_modelo(id);
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
    fn do_disco(&self, spec: &ModelSpec, dimensao: Option<usize>) -> Option<Arc<Indice>> {
        let indice = Arc::new(Indice::carregar(&self.caminho_indice)?);
        if indice.modelo() != spec.id {
            return None;
        }
        if indice.dimensao() == 0 || dimensao.is_some_and(|d| d != indice.dimensao()) {
            return None;
        }
        Some(indice)
    }

    /// Um snapshot do índice, sem copiar metadata nem vetores. O `Arc` é a única
    /// cópia deste caminho: `fixados`, a busca e a tela continuam vendo exatamente
    /// o mesmo índice, e um escritor só duplica o conteúdo se a versão anterior
    /// ainda estiver publicada ou segurada por um leitor.
    fn indice_na_memoria(&self) -> Option<Arc<Indice>> {
        if let Some(indice) = trava_leitura(&self.indice).as_ref() {
            return Some(Arc::clone(indice));
        }
        let spec = self.spec()?;
        let indice = self.do_disco(spec, None)?;
        Some(self.publicar_ou_manter(indice))
    }

    /// A única porta de publicação de um snapshot pronto. Depois daqui o índice
    /// não é mais mutado: quem escrever de novo usa `Arc::make_mut` e preserva o
    /// snapshot anterior enquanto ele estiver publicado ou segurado por leitor.
    fn publicar_indice(&self, indice: Arc<Indice>) {
        *trava_escrita(&self.indice) = Some(indice);
    }

    /// Compare-and-keep da primeira publicação: se outra escrita publicou enquanto
    /// o disco era lido, o snapshot vivo vence. O lock cobre a checagem e a troca;
    /// devolver o vencedor permite que a busca responda por ele, não pelo disco
    /// velho que acabou de perder.
    fn publicar_ou_manter(&self, candidato: Arc<Indice>) -> Arc<Indice> {
        let mut guard = trava_escrita(&self.indice);
        if let Some(publicado) = guard.as_ref() {
            Arc::clone(publicado)
        } else {
            *guard = Some(Arc::clone(&candidato));
            candidato
        }
    }

    /// Busca com o índice travado só para leitura, pela lista de notas que quem
    /// chama acabou de ler. A lista é viva — vem do disco no instante da busca, e é
    /// por fora desta função: apagar um `.md` esconde a nota da resposta seguinte sem
    /// que ninguém precise chamar comando nenhum, e `raizes` faz o mesmo pelas pastas.
    fn procurar_com_notas(&self, busca: &Busca, documentos: &[String]) -> Result<Resposta> {
        self.procurar_carregando(busca, documentos, || {
            let spec = self
                .spec()
                .ok_or_else(|| anyhow::anyhow!(tr("erros.memoriaSemModelo", &[])))?;
            self.do_disco(spec, Some(busca.vetor.len()))
                .ok_or_else(|| anyhow::anyhow!(tr("erros.memoriaSemIndice", &[])))
        })
    }

    /// O caminho da primeira busca, com o carregamento separado para que a
    /// publicação compare-and-keep possa ser testada numa interleavação exata:
    /// o loader publica uma escrita concorrente e devolve o snapshot velho do
    /// disco. Em produção, o loader é `do_disco`; a regra é a mesma.
    fn procurar_carregando(
        &self,
        busca: &Busca,
        documentos: &[String],
        carregar: impl FnOnce() -> Result<Arc<Indice>>,
    ) -> Result<Resposta> {
        let raizes = trava_leitura(&self.pastas).raizes.clone();
        let busca = &Busca { documentos, ..*busca };
        {
            let guard = trava_leitura(&self.indice);
            if let Some(indice) = guard.as_ref() {
                return indice.buscar(busca, &raizes);
            }
        }
        let candidato = carregar()?;
        // Não sobrescrever uma escrita publicada durante a leitura do disco, e
        // responder pelo snapshot que realmente venceu essa comparação.
        let indice = self.publicar_ou_manter(candidato);
        indice.buscar(busca, &raizes)
    }

    /// Um trecho é `Some` quando `indice` já tem alguma coisa na memória; sem
    /// escritor concorrente é o que vale. Esta função é a única porta de escrita
    /// no índice para quem não é a varredura: pega a trava dos escritores, obtém
    /// um snapshot, aplica a mudança, grava no disco e publica — tudo sob a mesma
    /// trava.
    /// O `alterar` devolve quantas peças mudou; zero significa "nada a gravar",
    /// e o índice nem sai da função.
    fn escrever_indice(
        &self,
        alterar: impl FnOnce(&mut Indice) -> usize,
    ) -> Result<usize> {
        let _trava = trava_estado(&self.escritores);
        let Some(mut snapshot) = self.indice_na_memoria() else {
            return Ok(0);
        };
        let indice = Arc::make_mut(&mut snapshot);
        let feitos = alterar(indice);
        if feitos > 0 {
            // Toda escrita passa por aqui, então é aqui que o mapa de baldes volta a
            // bater com o índice. Na regra geral o custo é uma passada só — as bordas
            // recontadas sobre o que já existe — e o k-means inteiro só roda quando o
            // mapa deixou de cobrir a casa, que é justamente o momento em que a busca
            // começaria a sondar baldes vazios.
            indice.reagrupar();
            indice.salvar(&self.caminho_indice)?;
            self.publicar_indice(snapshot);
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
        self.do_disco(spec, None).map(|indice| leitura(&indice)).unwrap_or_default()
    }

    /// O vetor da pergunta. A consulta passa pelo prefixo que o modelo declara
    /// — é assim que ele foi treinado — e é cortada no lote, porque a pergunta
    /// que cabe é a que vira vetor.
    ///
    /// É também o único lugar da casa onde um texto vira vetor dentro de um turno,
    /// e era o lugar onde ele virava mais de uma vez: a ferramenta `buscar_memoria`
    /// chama esta função a cada invocação, e um modelo que consulta a memória três
    /// vezes na mesma rodada pagava três vezes pelo vetor da mesma frase. O cache
    /// existe por causa disto. A chave é o estado do codificador inteiro, não só o
    /// texto — ver [`ChaveConsulta`] — e quem troca ou solta o modelo limpa a casa.
    fn vetor_da_consulta(&self, texto: &str, prefixo: Option<&'static str>) -> Result<Vec<f32>> {
        let mut guard = self.carregado()?;
        let carregado = guard.as_mut().expect("carregado acabou de ser garantido");
        let chave = ChaveConsulta {
            modelo: carregado.ids.clone(),
            agrupamento: carregado.embutidor.agrupamento.to_string(),
            dimensao: carregado.embutidor.dimensao(),
            lote: carregado.embutidor.limite(),
            prefixo,
            texto: texto.to_string(),
        };
        let tokenizador = &carregado.tokenizador;
        let embutidor = &mut carregado.embutidor;
        self.vetor_da_chave(chave, move || {
            let com_prefixo = match prefixo {
                Some(prefixo) => format!("{prefixo}{texto}"),
                None => texto.to_string(),
            };
            let limite = embutidor.limite();
            let mut ids = codificar(tokenizador, &com_prefixo)?;
            ids.truncate(limite);
            let mut vetor = embutidor.embutir(&ids)?;
            Indice::normalizar(&mut vetor)?;
            Ok(vetor)
        })
    }

    /// O cache puro, separado do codificador para que a política de acerto e de
    /// descarte seja testável sem modelo nenhum em disco: `produzir` é o que custa
    /// caro, e é exatamente ele que o teste conta quantas vezes rodou.
    fn vetor_da_chave(
        &self,
        chave: ChaveConsulta,
        produzir: impl FnOnce() -> Result<Vec<f32>>,
    ) -> Result<Vec<f32>> {
        {
            let mut cache = trava_estado(&self.consultas);
            if let Some(vetor) = cache.consultar(&chave) {
                return Ok(vetor);
            }
        }
        let vetor = produzir()?;
        trava_estado(&self.consultas).inserir(chave, vetor.clone());
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
        // `revogar`/revalidações concorrentes, segurada do snapshot ao publicar.
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
        let mut snapshot = self.indice_na_memoria().unwrap_or_else(|| {
            Arc::new(Indice::vazio(
                spec.id,
                carregado.embutidor.dimensao(),
                carregado.embutidor.agrupamento,
                ajustes.peca,
                ajustes.sobreposicao,
            ))
        });
        {
            let indice = Arc::make_mut(&mut snapshot);
            if indice.refatiar(ajustes.peca, ajustes.sobreposicao) {
                // Escala da tela diferente da escala gravada: as peças de conversa
                // foram cortadas na antiga também. A gravação imediata é o que faz
                // um índice esvaziado sobreviver mesmo se o turno cair no meio.
                indice.salvar(&self.caminho_indice)?;
                self.publicar_indice(Arc::new(indice.clone()));
            }
        }
        let indice = Arc::make_mut(&mut snapshot);
        let mudadas = self.escrever_conversas(indice, carregado, vivas, orcamento)?;
        if mudadas > 0 {
            // Mesmo motivo da varredura: o custo do mapa é de escrita, nunca de turno.
            indice.reagrupar();
            indice.salvar(&self.caminho_indice)?;
            self.publicar_indice(snapshot);
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
        let documentos = crate::ai::base::ids(&self.raiz_base).unwrap_or_default();
        self.buscar_com_notas(consulta, trechos, ignorar, &documentos)
    }

    /// A busca com o censo da base já lido. É por aqui que entra o turno, que lê a
    /// pasta uma vez e responde por ela nas duas portas — as presas e a busca.
    fn buscar_com_notas(
        &self,
        consulta: &str,
        trechos: u32,
        ignorar: &[String],
        documentos: &[String],
    ) -> Result<Resposta> {
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
        self.procurar_com_notas(
            &Busca {
                vetor: &vetor,
                texto: consulta,
                k,
                piso: ajustes.piso,
                modo: ajustes.modo,
                ignorar,
                // Espaços reservados, não uma decisão: a lista entra por quem chama,
                // lida do disco no instante do turno.
                documentos: &[],
            },
            documentos,
        )
    }

    /// Onde a base de conhecimento mora. Um caminho só, decidido na abertura.
    pub fn raiz_base(&self) -> &Path {
        &self.raiz_base
    }

    /// As notas como o disco as responde agora, com o censo da pasta.
    pub fn notas(&self) -> Result<crate::ai::base::Lista> {
        crate::ai::base::listar(&self.raiz_base)
    }

    /// As primeiras peças das notas presas, na ordem da última escrita — e só das
    /// que ainda estão na pasta.
    ///
    /// Não é busca: não há vetor, não há piso, não há `k`. O que existe é a
    /// decisão de um dono registrada no arquivo, e ela entra no prompt inteira ou
    /// não entra — dizer "aqui está o que você prendeu" custa ler a metadata, e é
    /// só isso que este número mede.
    ///
    /// A lista de notas é lida no disco pela mesma razão da busca: apagar o `.md`
    /// por fora tem de tirar a nota do turno no instante seguinte, e sem
    /// codificador carregado não há sincronização nenhuma para reescrever o índice
    /// e dizer que ela saiu.
    pub fn fixados(&self) -> Vec<Achado> {
        let vivas = crate::ai::base::ids(&self.raiz_base).unwrap_or_default();
        self.fixados_com_lista(&vivas)
    }

    /// As presas com o censo da base já lido — a mesma lista que a busca do turno
    /// obedece, para as duas portas responderem pelo mesmo disco.
    fn fixados_com_lista(&self, vivas: &[String]) -> Vec<Achado> {
        let Some(indice) = self.indice_na_memoria() else { return Vec::new() };
        indice
            .pecas_fixadas()
            .into_iter()
            .filter(|achado| vivas.iter().any(|id| *id == achado.arquivo))
            .collect()
    }

    /// O bloco de memória que entra neste turno: o que o dono prendeu primeiro, e
    /// depois o que a busca achou.
    ///
    /// Uma lista só, um teto só. `ajustes.trechos == 0` desliga **tudo** — as
    /// presas junto — porque uma chave que desliga a busca e deixa passar um
    /// conteúdo que ninguém pediu não é um desligamento, é uma segunda porta.
    /// O zero fecha em dois pontos de propósito: o retorno abaixo, que não paga
    /// o vetor de uma busca que não pode responder, e o `teto` que o laço obedece,
    /// que é o mesmo número. Fechar um dos dois ainda deixa o bloco vazio — foi o
    /// que a mutação mediu, cada metade sozinha e depois as duas juntas.
    /// O teto é `TRECHOS_MAX` somado, não por fonte: o que o modelo relê a cada
    /// rodada é o bloco inteiro, e a conta que dói é a de tokens no prompt.
    ///
    /// Repetido não entra duas vezes: uma nota presa que a busca também achou
    /// aparece uma vez, na frente, com a origem que diz por que ela está ali.
    ///
    /// Devolve uma [`Resposta`] — o mesmo tipo da ferramenta — porque o cartão do
    /// turno precisa do modo que ordenou a lista. `alcancados` é o número da
    /// busca, e não o do bloco: as presas não passaram por lista nenhuma, e
    /// somá-las ali seria medir uma coisa com o nome de outra.
    pub fn bloco_do_turno(&self, consulta: &str, ignorar: &[String]) -> Resposta {
        let ajustes = self.ajustes();
        if ajustes.trechos == 0 || consulta.trim().is_empty() {
            return Resposta {
                achados: Vec::new(),
                alcancados: 0,
                modo: ajustes.modo.nome(),
            };
        }
        let teto = ajustes.trechos.min(TRECHOS_MAX) as usize;
        // A busca é tentativa, não pré-condição: um índice que já está em disco
        // responde pelas presas mesmo na execução em que o codificador não abriu,
        // e o motivo de a busca ter falhado já está na tela pelo `auto` do
        // `estado` — copiá-lo para cá seria a mesma pergunta respondida em dois
        // lugares. Quem nunca teve peça nenhuma não entra: a presa é lida do
        // índice como tudo nesta memória, e um cartão escrito sobre uma nota que
        // ninguém vetorou seria a tela prometendo memória que não existe.
        // A lista de notas é lida uma vez, e é ela que vale nas duas portas abaixo.
        // Ler a pasta duas vezes custava dois `readdir` num turno que já paga um
        // vetor, e — mais importante — as duas leituras não precisam concordar.
        let vivas = crate::ai::base::ids(&self.raiz_base).unwrap_or_default();
        let (buscados, alcancados, modo) =
            match self.buscar_com_notas(consulta, teto as u32, ignorar, &vivas) {
                Ok(resposta) => (resposta.achados, resposta.alcancados, resposta.modo),
                Err(_) => (Vec::new(), 0, ajustes.modo.nome()),
            };
        let mut bloco: Vec<Achado> = Vec::with_capacity(teto);
        // O repetido não entra duas vezes. É o par fonte + chave + linha que diz o
        // que é o mesmo trecho — dois arquivos diferentes podem ter a mesma linha —
        // e ele vai num conjunto em vez de varrido na lista: `Vec::contains` sobre
        // o bloco era o teto de trechos ao quadrado, pequeno o bastante para nunca
        // doer e grande o bastante para crescer com a casa.
        let mut chaves: BTreeSet<(&'static str, String, usize)> = BTreeSet::new();
        for achado in self.fixados_com_lista(&vivas).into_iter().chain(buscados) {
            if bloco.len() >= teto {
                break;
            }
            let chave = (achado.fonte.nome(), achado.arquivo.clone(), achado.linha);
            if !chaves.insert(chave) {
                continue;
            }
            bloco.push(achado);
        }
        Resposta { achados: bloco, alcancados, modo }
    }

    /// Põe a pasta `base/` dentro do índice.
    ///
    /// É o ciclo completo em uma chamada: o que mudou de texto entra (peças novas
    /// no lugar das antigas), o que não mudou não custa um vetor, e o que sumiu da
    /// pasta sai do índice junto com a ficha — apagar o `.md` apaga a memória da
    /// nota, do mesmo jeito que apagar a conversa apaga a lembrança dela.
    ///
    /// A trava é a mesma da revalidação de conversas e na mesma ordem documentada
    /// (`escritores → carregado → indice`): dois escritores concorrentes usando o
    /// mesmo snapshot publicariam, no fim, um índice em que o trabalho um do outro
    /// não existe. E a varredura em curso tem a prioridade: ela regrava o índice no fim
    /// e escrever por cima agora seria perder o que ela está lendo.
    pub fn sincronizar_base(&self, notas: &[Nota]) -> Result<Sincronia> {
        let inicio = Instant::now();
        let visto = notas.len();
        // A resposta de "não fiz nada" tem três motivos diferentes e um número só,
        // porque os três medem a mesma coisa: quantas notas a lista tinha.
        let parada = |notas: usize| Sincronia {
            notas,
            pecas: 0,
            removidas: 0,
            tokens: 0,
            ms: inicio.elapsed().as_millis() as u64,
        };
        if self.esta_indexando() {
            return Ok(parada(visto));
        }
        let _trava = trava_estado(&self.escritores);
        // A pendência é lida só na metadata, antes de qualquer lock de motor: sem
        // ela, cada turno carregaria um codificador de duzentos milhões de
        // parâmetros para descobrir que nenhuma nota mudou. Sob a trava dos
        // escritores, como em `fundir_conversas`: lida fora dela, uma publicação
        // concorrente poderia ser contada como "já vista" por engano.
        let pendente = {
            let guard = trava_leitura(&self.indice);
            match guard.as_ref() {
                Some(indice) => indice.notas_pendentes(notas),
                None => visto,
            }
        };
        if pendente == 0 {
            return Ok(parada(visto));
        }
        let Some(spec) = self.spec() else {
            // Sem codificador não há tokenizador nem vetor, e sem vetor a nota não
            // entra. A resposta é zero peças, não um erro: a nota está no disco, a
            // base está na lista e o `estado` diz, por `auto`, que falta o modelo.
            return Ok(parada(visto));
        };
        let (escritas, removidas, tokens) = {
            let mut trava = self.carregado()?;
            let carregado = trava.as_mut().expect("carregado acabou de ser garantido");
            let ajustes = self.ajustes();
            let mut snapshot = self.indice_na_memoria().unwrap_or_else(|| {
                Arc::new(Indice::vazio(
                    spec.id,
                    carregado.embutidor.dimensao(),
                    carregado.embutidor.agrupamento,
                    ajustes.peca,
                    ajustes.sobreposicao,
                ))
            });
            {
                let indice = Arc::make_mut(&mut snapshot);
                if indice.refatiar(ajustes.peca, ajustes.sobreposicao) {
                    indice.salvar(&self.caminho_indice)?;
                    self.publicar_indice(Arc::new(indice.clone()));
                }
            }
            let indice = Arc::make_mut(&mut snapshot);
            let trabalho = self.escrever_notas(indice, carregado, notas)?;
            indice.reagrupar();
            indice.salvar(&self.caminho_indice)?;
            self.publicar_indice(snapshot);
            trabalho
        };
        Ok(Sincronia {
            notas: visto,
            pecas: escritas,
            removidas,
            tokens,
            ms: inicio.elapsed().as_millis() as u64,
        })
    }

    /// O trabalho em si, com o codificador e o índice já na mão — a mesma razão de
    /// `escrever_conversas` existir separado: quem segura `carregado` e chama
    /// `carregado()` de novo morre ali, e o mutex não é reentrante.
    ///
    /// Devolve as peças escritas, as removidas e os tokens vetorizados: os três
    /// números que a interface mostra como "o índice aprendeu".
    fn escrever_notas(
        &self,
        indice: &mut Indice,
        carregado: &mut Carregado,
        notas: &[Nota],
    ) -> Result<(usize, usize, usize)> {
        let (alvo, sobre) = indice.fatiamento();
        let (alvo, sobre) = (alvo as usize, sobre as usize);
        let limite = carregado.embutidor.limite();
        // Primeiro sai o que não é mais pasta: apagar o arquivo apaga a
        // lembrança dele, e tirar da memória não custa um vetor.
        let removidas = indice.reter_notas(notas);
        let mut escritas = 0usize;
        let mut tokens = 0usize;
        for nota in notas {
            let prova = prova_da_nota(nota);
            if indice.nota_vista(&nota.id, &prova) {
                continue;
            }
            if indice.trechos() >= LIMITE_TRECHOS {
                break;
            }
            let corpo = nota.para_indexar();
            let lidas = pecas(&corpo, alvo, sobre, limite, &carregado.tokenizador)?;
            let total_lidas = lidas.len();
            let mut prontas: Vec<(Peca, Vec<f32>)> = Vec::with_capacity(lidas.len());
            for peca in lidas {
                if indice.trechos() + prontas.len() >= LIMITE_TRECHOS {
                    break;
                }
                tokens += peca.ids.len();
                let mut vetor = carregado.embutidor.embutir(&peca.ids)?;
                Indice::normalizar(&mut vetor)?;
                prontas.push((peca, vetor));
            }
            escritas += prontas.len();
            indice.substituir_documento(nota, prova, &prontas, prontas.len() < total_lidas);
        }
        Ok((escritas, removidas, tokens))
    }

    /// O texto que o modelo lê: cabeçalho e trechos, cada um com a origem de
    /// onde ele saiu — caminho e linha num arquivo, conversa e mensagem numa
    /// lembrança de papo, nota e parágrafo num pedaço da base — para a citação
    /// sair certa.
    pub fn texto_do_bloco(achados: &[Achado]) -> String {
        let mut saida = String::from(tr("memoria.blocoCabecalho", &[]).as_str());
        for (numero, achado) in achados.iter().enumerate() {
            let origem = match achado.fonte {
                Fonte::Arquivo => format!("{}:{}", achado.arquivo, achado.linha),
                Fonte::Conversa => tr(
                    "memoria.blocoConversa",
                    &[("titulo", &achado.nome), ("linha", &achado.linha.to_string())],
                ),
                Fonte::Documento => tr(
                    "memoria.blocoNota",
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
            notas: disco.notas,
            trechos_notas: disco.trechos_notas,
            fixadas: disco.fixadas,
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

    /// Constrói ou atualiza o índice das pastas concedidas, das conversas salvas e
    /// da base de conhecimento. Bloqueante: quem chama roda isto fora do runtime
    /// assíncrono.
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
        // A base é lida antes da conta do que há a fazer: uma pasta `base/` com
        // notas é memória suficiente para o varrer ter trabalho, do mesmo jeito que
        // conversa salva é — e um `.md` escrito à mão sem pasta concedida nenhuma
        // não pode ficar sem caminho nenhum para entrar no índice.
        let notas = crate::ai::base::listar(&self.raiz_base).unwrap_or_default();
        if raizes.is_empty() && conversas.is_empty() && notas.itens.is_empty() {
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

        let mut snapshot = self
            .indice_na_memoria()
            .filter(|i| i.modelo() == spec.id && i.dimensao() == dimensao)
            .unwrap_or_else(|| {
                Arc::new(Indice::vazio(
                    spec.id,
                    dimensao,
                    carregado.embutidor.agrupamento,
                    alvo as u32,
                    sobre as u32,
                ))
            });
        {
            let indice = Arc::make_mut(&mut snapshot);
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
            let indice = Arc::make_mut(&mut snapshot);
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
                self.publicar_indice(Arc::clone(&snapshot));
                snapshot.salvar(&self.caminho_indice)?;
            }
            if truncado {
                break;
            }
        }

        // Uma pasta revogada no meio da varredura não pode voltar pelo arquivo
        // nem continuar em memória: a última palavra é da lista viva de
        // concessões, e ela vem depois de todo o trabalho.
        let resumo = {
            let indice = Arc::make_mut(&mut snapshot);
            let vivas = trava_leitura(&self.pastas).raizes.clone();
            indice.reter_dentro(&vivas);

            // O varrer é também o ponto em que o histórico inteiro entra na memória,
            // sem o orçamento de um turno: aqui o codificador já está na mão, o
            // trabalho corre fora da conversa e a barra de progresso existe justamente
            // para o dono ver o índice subir.
            self.escrever_conversas(indice, carregado, conversas, usize::MAX)?;

            // A base entra no mesmo ponto, pela mesma razão: o codificador já está na
            // mão, o trabalho corre fora da conversa e a barra de progresso existe para
            // o dono ver o índice subir. Ler a pasta agora, e não no começo, é o que faz
            // uma nota escrita durante a varredura entrar nela em vez de esperar a
            // próxima.
            let notas = crate::ai::base::listar(&self.raiz_base).unwrap_or_default();
            self.escrever_notas(indice, carregado, &notas.itens)?;

            // O mapa de baldes é recontado aqui, e não dentro de `buscar`: a varredura
            // é o único lugar da casa em que um segundo a mais é invisível — a barra de
            // progresso já está na tela e o codificador acabou de percorrer doze mil
            // vetores. Um índice que chegou do disco antes disto existir ganha aqui a
            // sua primeira partição, sem que ninguém precise reindexar nada.
            indice.reagrupar();

            Resumo {
                arquivos: indice.arquivos(),
                relidos,
                trechos: indice.trechos(),
                conversas: indice.conversas(),
                ms: 0,
                truncado: truncado || !completa,
                cancelado,
            }
        };
        // Publica antes de gravar, como antes: em falha de disco o snapshot vivo
        // ainda é o trabalho concluído. O `Arc` compartilhado não duplica vetores.
        self.publicar_indice(Arc::clone(&snapshot));
        snapshot.salvar(&self.caminho_indice)?;
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
        Busca { vetor, texto: "", k, piso, modo: Modo::Vetorial, ignorar: &[], documentos: &[] }
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
            &base.join(crate::ai::base::PASTA),
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
        let custodio = Custodio::novo(
            &[],
            &base,
            &base,
            base.join("rag.json"),
            &base.join(crate::ai::base::PASTA),
            pastas,
        );

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
    /// turno são os dois escritores do índice fora da varredura. Cada um obtém
    /// um snapshot, muda, grava no disco e publica — e sem a trava de escritores o
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
        let custodio = Custodio::novo(
            &[],
            &base,
            &base,
            base.join("rag.json"),
            &base.join(crate::ai::base::PASTA),
            pastas,
        );

        let semear = |custodio: &Custodio| {
            let mut indice = Indice::vazio("m", 1, "último", 64, 0);
            for nome in ["a", "b", "c"] {
                indice.adicionar(
                    &format!("C:/x/{nome}.rs"),
                    Assinatura { bytes: 1, modificado: 1 },
                    &[(Peca { linha: 1, texto: nome.into(), ids: vec![] }, vec![1.0])],
                );
            }
            *custodio.indice.write().expect("índice envenenado") = Some(Arc::new(indice));
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
                &Busca { vetor: &vetor, texto: "ds18b20", k: 2, piso: 0.5, modo, ignorar: &[], documentos: &[] },
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
            .buscar(&Busca { vetor: &vetor, texto: "norte", k: 3, piso: 0.5, modo: Modo::Hibrido, ignorar: &[], documentos: &[] }, &raizes)
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
                documentos: &[],
            }, &raizes)
            .unwrap();
        assert_eq!(resposta.achados.len(), 1);
        assert_eq!(resposta.achados[0].texto, "manutenção elétrica do prédio");
        assert_eq!(resposta.achados[0].origem, Origem::Lexica);
        // Sem nenhuma palavra em comum, a lista léxica fica vazia — ausência não
        // entra em lista nenhuma como "nota baixa".
        let vazio = indice
            .buscar(&Busca { vetor: &vetor, texto: "tubulação", k: 1, piso: 0.0, modo: Modo::Lexico, ignorar: &[], documentos: &[] }, &raizes)
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
                        documentos: &[],
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

    // --- a terceira fonte: a base curada -------------------------------------

    fn nota(id: &str, titulo: &str, corpo: &str, atualizada: u64, fixada: bool) -> Nota {
        Nota {
            id: id.into(),
            titulo: titulo.into(),
            texto: corpo.into(),
            criado: atualizada,
            atualizado: atualizada,
            fixada,
        }
    }

    /// Põe uma nota dentro do índice com o fatiamento que o codificador teria
    /// dado. Nada aqui depende de vetor: o que os testes abaixo medem é a política
    /// da terceira fonte — entrada, troca, esquecimento e o marcador de presa —
    /// e as peças entram com texto e um vetor de duas pontas, como em `adicionar`.
    fn nota_no_indice(indice: &mut Indice, nota: &Nota, pecas: &[&str], truncada: bool) {
        let prontas: Vec<(Peca, Vec<f32>)> = pecas
            .iter()
            .enumerate()
            .map(|(i, texto)| {
                (
                    Peca { linha: i + 1, texto: (*texto).to_string(), ids: vec![] },
                    vec![1.0, 0.0],
                )
            })
            .collect();
        indice.substituir_documento(nota, prova_da_nota(nota), &prontas, truncada);
    }

    /// A nota presa entra no turno por decisão do dono, e não por proximidade: a
    /// lista das presas é lida da ficha, na ordem da última escrita, e uma busca
    /// que não achou nada não tem poder nenhum sobre ela.
    #[test]
    fn a_nota_presa_entra_na_frente_sem_ninguem_ter_perguntado() {
        let velha = nota("regra", "Regra da casa", "p0 p1", 1_700_000_000_000, true);
        let nova = nota("dieta", "Dieta do gato", "p2 p3", 1_700_000_000_002, true);
        let solta = nota("solto", "Nota solta", "p4 p5", 1_700_000_000_009, false);
        let mut indice = Indice::vazio("m", 2, "último", 64, 0);
        nota_no_indice(&mut indice, &velha, &[&velha.texto], false);
        nota_no_indice(&mut indice, &nova, &[&nova.texto], false);
        nota_no_indice(&mut indice, &solta, &[&solta.texto], false);

        let presas = indice.pecas_fixadas();
        assert_eq!(
            presas.iter().map(|a| a.arquivo.as_str()).collect::<Vec<_>>(),
            vec!["dieta", "regra"],
            "a mais mexida primeiro, e nada da nota que ninguém prendeu"
        );
        assert!(presas.iter().all(|a| a.fonte == Fonte::Documento));
        assert!(presas.iter().all(|a| a.origem == Origem::Fixada));
        assert_eq!(presas[0].nome, "Dieta do gato", "o cartão chama a nota pelo título dela");
        assert_eq!(presas[0].pontuacao, 0.0, "presa não é medida: ninguém procurou este trecho");
        assert_eq!(presas[0].quando, Some(1_700_000_000_002), "a data do cartão é a do arquivo");

        // A prova de que prender não é buscar: a mesma pergunta, com o piso alto e
        // um vetor ortogonal, não achou trecho nenhum — e as presas continuam lá.
        let raizes: [PathBuf; 0] = [];
        let resposta = indice
            .buscar(
                &Busca {
                    vetor: &[0.0, 1.0],
                    texto: "zzz",
                    k: 3,
                    piso: 0.9,
                    modo: Modo::Hibrido,
                    ignorar: &[],
                    documentos: &["regra".to_string(), "dieta".to_string(), "solto".to_string()],
                },
                &raizes,
            )
            .expect("buscou");
        assert_eq!(resposta.achados.len(), 0, "nenhum trecho passa do piso contra um vetor ortogonal");
        assert_eq!(indice.pecas_fixadas().len(), 2);
    }

    /// Reescrever a nota troca as peças dela. Deixar as de antes seria o índice
    /// citando uma linha que não existe mais no `.md`.
    #[test]
    fn reescrever_a_nota_troca_as_pecas_delas_e_nao_deixa_orfa() {
        let antes = nota("plano", "Plano A", "p0 p1\np2 p3", 1, false);
        let mut indice = Indice::vazio("m", 2, "último", 64, 0);
        nota_no_indice(&mut indice, &antes, &["p0 p1", "p2 p3"], false);
        assert_eq!(indice.trechos_de(Fonte::Documento), 2);
        assert_eq!(indice.notas_pendentes(&[antes.clone()]), 0, "o índice viu esta nota como ela está");

        let depois = nota("plano", "Plano A", "p0 p1", 2, false);
        nota_no_indice(&mut indice, &depois, &["p0 p1"], false);
        assert_eq!(indice.trechos_de(Fonte::Documento), 1, "a peça que saiu do texto saiu do índice");
        assert_eq!(indice.documentos(), 1, "duas fichas para o mesmo id é uma nota fantasma");
        assert_eq!(indice.vetores.len(), 2, "vetor órfão é cosseno de um texto que não existe mais");
        assert_eq!(indice.notas_pendentes(&[depois.clone()]), 0);
        assert_eq!(indice.notas_pendentes(&[antes]), 1, "a prova de ontem não vale para o texto de hoje");
    }

    /// Apagar o `.md` apaga a lembrança: as peças, a ficha e o marcador de presa.
    #[test]
    fn a_nota_que_sai_da_pasta_sai_do_indice_e_da_ficha() {
        let n = nota("plano", "Plano A", "p0 p1", 5, true);
        let mut indice = Indice::vazio("m", 2, "último", 64, 0);
        nota_no_indice(&mut indice, &n, &["p0 p1"], false);
        assert_eq!(indice.fixadas(), 1);
        assert_eq!(indice.reter_notas(&[]), 1);
        assert_eq!(indice.documentos(), 0);
        assert_eq!(indice.fixadas(), 0);
        assert_eq!(indice.trechos_de(Fonte::Documento), 0);
        assert_eq!(indice.pecas_fixadas().len(), 0, "o marcador de uma nota apagada não entra no prompt");
    }

    /// Ficha truncada nunca está vista: o índice lembra só o começo da nota, e se
    /// ele se anunciasse inteiro a parte que falta nunca voltaria para a fila.
    #[test]
    fn uma_nota_cortada_pelo_teto_volta_para_a_fila() {
        let n = nota("longa", "Longa", "p0 p1\np2 p3", 1, false);
        let mut parada = Indice::vazio("m", 2, "último", 64, 0);
        nota_no_indice(&mut parada, &n, &["p0 p1"], true);
        assert_eq!(parada.notas_pendentes(&[n.clone()]), 1, "truncada não se anuncia inteira");

        let mut inteira = Indice::vazio("m", 2, "último", 64, 0);
        nota_no_indice(&mut inteira, &n, &["p0 p1", "p2 p3"], false);
        assert_eq!(inteira.notas_pendentes(&[n]), 0);
    }

    /// O título entra no texto indexado, então entrar na prova também: trocar o
    /// título de uma nota indexada tem de pôr a nota de volta na fila, senão a
    /// busca continuaria respondendo pelo vocabulário de um título que sumiu.
    #[test]
    fn mudar_o_titulo_e_mudar_a_nota_porque_o_titulo_tambem_indexa() {
        let velho = nota("id", "Título velho", "p0 p1 p2", 7, false);
        let novo = nota("id", "Título novo", "p0 p1 p2", 7, false);
        assert_ne!(prova_da_nota(&velho).bytes, prova_da_nota(&novo).bytes);
        assert_eq!(prova_da_nota(&velho).modificado, prova_da_nota(&novo).modificado, "só o texto mudou");

        let mut indice = Indice::vazio("m", 2, "último", 64, 0);
        nota_no_indice(&mut indice, &velho, &["p0 p1 p2"], false);
        assert_eq!(indice.notas_pendentes(&[velho]), 0);
        assert_eq!(indice.notas_pendentes(&[novo]), 1, "a pessoa trocou o título; a busca ainda não sabe dele");
    }

    /// As três fontes não emprestam política uma para a outra, e a da nota é a
    /// lista da pasta: apagou o `.md` por fora, a peça sai da resposta sem que
    /// ninguém precise ter tocado no índice. O arquivo de mesma chave responde por
    /// outra regra, no mesmo índice.
    #[test]
    fn apagar_o_md_por_fora_tira_a_nota_da_resposta_sem_tocar_no_indice() {
        let n = nota("politica", "Política", "devolver em sete dias", 1, false);
        let mut indice = Indice::vazio("m", 2, "último", 64, 0);
        indice.adicionar(
            "politica",
            Assinatura { bytes: 1, modificado: 1 },
            &[(Peca { linha: 4, texto: "devolver em sete dias".into(), ids: vec![] }, vec![1.0, 0.0])],
        );
        nota_no_indice(&mut indice, &n, &["devolver em sete dias"], false);
        assert_eq!(indice.trechos(), 2, "a mesma chave em duas fontes são dois trechos");

        let raizes: [PathBuf; 0] = [];
        let responder = |documentos: &[String]| {
            indice
                .buscar(
                    &Busca {
                        vetor: &[1.0, 0.0],
                        texto: "devolver",
                        k: 4,
                        piso: 0.0,
                        modo: Modo::Hibrido,
                        ignorar: &[],
                        documentos,
                    },
                    &raizes,
                )
                .map(|r| {
                    r.achados
                        .iter()
                        .map(|a| (a.fonte, a.arquivo.clone(), a.linha))
                        .collect::<Vec<_>>()
                })
        };
        // A nota está na pasta: ela responde. O arquivo de mesma chave não — não há
        // pasta concedida nesta busca, e quem manda no arquivo são as raízes.
        assert_eq!(
            responder(&["politica".to_string()]).unwrap(),
            vec![(Fonte::Documento, "politica".to_string(), 1)]
        );
        // A nota saiu da pasta: some da resposta, continua no índice.
        assert_eq!(responder(&[]).unwrap(), Vec::<(Fonte, String, usize)>::new());
        assert_eq!(indice.trechos_de(Fonte::Documento), 1, "esconder da resposta não é apagar a memória");
    }

    /// O bloco do turno é o que a conversa injeta: a presa vem do índice (não do
    /// codificador desta rodada), obedece à pasta como a busca obedece, e a mesma
    /// chave que desliga a busca (`trechos == 0`) desliga a presa junto — uma
    /// porta que deixa passar conteúdo com a busca fechada não é um desligamento,
    /// é uma segunda porta. O assert do zero só cai com as duas metades do
    /// desligamento abertas (o retorno antecipado e o `teto` do laço): mutar uma
    /// delas continua verde, e é isso que prova que o zero fecha por dois caminhos.
    #[test]
    fn o_bloco_do_turno_leva_a_presa_e_o_trechos_zero_fecha_as_duas_portas() {
        let base = std::env::temp_dir().join(format!("aether-memoria-bloco-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let custodio = Custodio::novo(
            &[],
            &base,
            &base,
            base.join("rag.json"),
            &base.join(crate::ai::base::PASTA),
            Arc::new(RwLock::new(Pastas::default())),
        );
        // A nota entra pelo caminho real: a loja escreve o `.md` com o marcador de
        // presa, e o índice recebe a nota que ela devolveu — id, título e prova.
        let registro = crate::ai::base::escrever(
            custodio.raiz_base(),
            "Regra da casa",
            "nao abrir o app",
            None,
            Some(true),
        )
        .expect("escreveu");
        let arquivo = base
            .join(crate::ai::base::PASTA)
            .join(format!("{}.{}", registro.nota.id, crate::ai::base::EXTENSAO));
        let mut indice = Indice::vazio("m", 2, "último", 64, 0);
        nota_no_indice(&mut indice, &registro.nota, &["nao abrir o app"], false);
        *trava_escrita(&custodio.indice) = Some(Arc::new(indice));

        let bloco = custodio.bloco_do_turno("o que vale aqui?", &[]);
        assert_eq!(bloco.achados.len(), 1, "a nota presa não depende da busca para entrar");
        assert_eq!(bloco.achados[0].origem, Origem::Fixada);
        assert_eq!(bloco.achados[0].nome, "Regra da casa");
        assert_eq!(bloco.alcancados, 0, "as presas não passaram por lista nenhuma");
        assert_eq!(custodio.estado(true).auto, "semModelo", "e a tela diz por que a busca não respondeu");

        custodio.ajustar(Ajustes { trechos: 0, ..custodio.ajustes() }).expect("ajustou");
        assert!(custodio.bloco_do_turno("o que vale aqui?", &[]).achados.is_empty());
        assert_eq!(custodio.estado(true).auto, "trechosZero");
        // Reaberta a porta, a presa volta: o zero desligou, não apagou.
        custodio.ajustar(Ajustes { trechos: 4, ..custodio.ajustes() }).expect("reabriu");
        assert_eq!(custodio.bloco_do_turno("o que vale aqui?", &[]).achados.len(), 1);
        // O `.md` saiu da pasta por fora, sem pedido nenhum ao núcleo: some do
        // turno na chamada seguinte, e o índice continua com a peça — a busca é que
        // consultou o disco, a memória não foi editada.
        std::fs::remove_file(&arquivo).expect("removeu");
        assert!(custodio.bloco_do_turno("o que vale aqui?", &[]).achados.is_empty());
        assert_eq!(custodio.estado(true).trechos_notas, 1, "a peça ficou no índice");
        std::fs::remove_dir_all(&base).ok();
    }

    /// A ficha manda na ordem e no marcador, mas a pasta manda em quais notas
    /// ainda existem. A lista é relida em cada chamada: criar um `.md` por fora o
    /// faz aparecer, e apagá-lo o faz desaparecer sem reescrever o índice.
    #[test]
    fn fixados_veem_a_lista_viva_e_guardam_a_ordem_da_ultima_escrita() {
        let base = std::env::temp_dir()
            .join(format!("aether-memoria-fixados-vivos-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let custodio = Custodio::novo(
            &[],
            &base,
            &base,
            base.join("rag.json"),
            &base.join(crate::ai::base::PASTA),
            Arc::new(RwLock::new(Pastas::default())),
        );
        std::fs::create_dir_all(custodio.raiz_base()).unwrap();
        let caminho_da_nota = |id: &str| {
            custodio
                .raiz_base()
                .join(format!("{id}.{}", crate::ai::base::EXTENSAO))
        };
        for id in ["antiga", "recente", "empate", "solto"] {
            std::fs::write(caminho_da_nota(id), format!("# {id}\ncorpo\n")).unwrap();
        }

        let mut indice = Indice::vazio("m", 2, "último", 64, 0);
        for (id, titulo, atualizada, fixada) in [
            ("antiga", "Antiga", 10, true),
            ("recente", "Recente", 30, true),
            ("empate", "Empate", 30, true),
            ("ausente", "Ausente", 20, true),
            ("solto", "Solto", 40, false),
            ("sem-peca", "Sem peça", 50, true),
        ] {
            let nota = nota(id, titulo, &format!("corpo {id}"), atualizada, fixada);
            if id == "sem-peca" {
                nota_no_indice(&mut indice, &nota, &[], false);
            } else {
                nota_no_indice(
                    &mut indice,
                    &nota,
                    &[&format!("primeira peça {id}"), &format!("segunda peça {id}")],
                    false,
                );
            }
        }
        *trava_escrita(&custodio.indice) = Some(Arc::new(indice));

        fn ids(achados: &[Achado]) -> Vec<&str> {
            achados.iter().map(|a| a.arquivo.as_str()).collect()
        }
        let iniciais = custodio.fixados();
        assert_eq!(ids(&iniciais), vec!["empate", "recente", "antiga"]);
        assert_eq!(iniciais[1].texto, "primeira peça recente", "só a primeira peça entra");
        assert_eq!(iniciais[1].origem, Origem::Fixada);
        assert_eq!(iniciais[1].quando, Some(30));
        assert!(ids(&iniciais).contains(&"antiga"));

        // Uma nota que já estava na ficha, mas não tinha arquivo, passa a responder
        // sem sincronização: a existência vem do disco no instante da chamada.
        std::fs::write(caminho_da_nota("ausente"), "# Ausente\ncorpo\n").unwrap();
        assert_eq!(
            ids(&custodio.fixados()),
            vec!["empate", "recente", "ausente", "antiga"]
        );

        std::fs::remove_file(caminho_da_nota("recente")).unwrap();
        assert_eq!(ids(&custodio.fixados()), vec!["empate", "ausente", "antiga"]);
        assert_eq!(
            trava_leitura(&custodio.indice).as_ref().unwrap().documentos(),
            6,
            "esconder da resposta não apaga a ficha nem o texto do índice"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    /// O índice já publicado não substitui a leitura da base: `procurar` troca
    /// os ids reservados do chamante pela lista viva do disco em toda busca.
    #[test]
    fn a_busca_le_as_notas_vivas_do_disco_com_o_indice_na_memoria() {
        let base = std::env::temp_dir()
            .join(format!("aether-memoria-busca-viva-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let custodio = Custodio::novo(
            &[],
            &base,
            &base,
            base.join("rag.json"),
            &base.join(crate::ai::base::PASTA),
            Arc::new(RwLock::new(Pastas::default())),
        );
        std::fs::create_dir_all(custodio.raiz_base()).unwrap();
        let arquivo = custodio
            .raiz_base()
            .join(format!("viva.{}", crate::ai::base::EXTENSAO));
        std::fs::write(&arquivo, "# Viva\ncorpo\n").unwrap();
        let nota = nota("viva", "Viva", "corpo", 1, false);
        let mut indice = Indice::vazio("m", 2, "último", 64, 0);
        nota_no_indice(&mut indice, &nota, &["corpo"], false);
        *trava_escrita(&custodio.indice) = Some(Arc::new(indice));

        let buscar = || {
            let vivas = crate::ai::base::ids(custodio.raiz_base()).unwrap_or_default();
            custodio
                .procurar_com_notas(
                    &Busca {
                        vetor: &[1.0, 0.0],
                        texto: "corpo",
                        k: 4,
                        piso: 0.0,
                        modo: Modo::Vetorial,
                        ignorar: &[],
                        documentos: &[],
                    },
                    &vivas,
                )
                .unwrap()
        };
        assert_eq!(buscar().achados.len(), 1, "a nota está no disco");
        std::fs::remove_file(&arquivo).unwrap();
        assert!(buscar().achados.is_empty(), "o `.md` saiu por fora");
        assert_eq!(
            trava_leitura(&custodio.indice).as_ref().unwrap().trechos_de(Fonte::Documento),
            1,
            "a busca filtra o disco; não edita o índice"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    /// Regressão determinística da primeira busca: o loader do disco termina
    /// depois de uma escrita concorrente publicar. O snapshot velho não pode
    /// substituir o novo, e a resposta tem de sair do que ficou publicado.
    #[test]
    fn a_primeira_busca_mantem_a_escrita_publicada_durante_a_leitura() {
        let base = std::env::temp_dir().join(format!(
            "aether-memoria-busca-concorrente-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&base).unwrap();
        let raiz = base.join("concedida");
        let pastas = Arc::new(RwLock::new(Pastas {
            raizes: vec![raiz.clone()],
            ..Pastas::default()
        }));
        let custodio = Custodio::novo(
            &[],
            &base,
            &base,
            base.join("rag.json"),
            &base.join(crate::ai::base::PASTA),
            pastas,
        );
        let arquivo = raiz.join("mesmo.rs").display().to_string();
        let mut antigo = Indice::vazio("m", 1, "último", 64, 0);
        antigo.adicionar(
            &arquivo,
            Assinatura {
                bytes: 1,
                modificado: 1,
            },
            &[(
                Peca {
                    linha: 1,
                    texto: "velho".into(),
                    ids: vec![],
                },
                vec![1.0],
            )],
        );
        let mut novo = Indice::vazio("m", 1, "último", 64, 0);
        novo.adicionar(
            &arquivo,
            Assinatura {
                bytes: 1,
                modificado: 2,
            },
            &[(
                Peca {
                    linha: 1,
                    texto: "novo".into(),
                    ids: vec![],
                },
                vec![1.0],
            )],
        );
        let novo = Arc::new(novo);

        let resposta = custodio
            .procurar_carregando(
                &Busca {
                    vetor: &[1.0],
                    texto: "",
                    k: 4,
                    piso: 0.0,
                    modo: Modo::Vetorial,
                    ignorar: &[],
                    documentos: &[],
                },
                &[],
                || {
                    // Este é o ponto exato em que a escrita concorrente vence.
                    custodio.publicar_indice(Arc::clone(&novo));
                    Ok(Arc::new(antigo))
                },
            )
            .unwrap();

        assert_eq!(resposta.achados.len(), 1);
        assert_eq!(
            resposta.achados[0].texto, "novo",
            "a busca respondeu pelo snapshot velho que perdeu a publicação"
        );
        let publicado = trava_leitura(&custodio.indice);
        assert!(
            Arc::ptr_eq(publicado.as_ref().unwrap(), &novo),
            "a leitura do disco sobrescreveu a escrita concorrente"
        );
        drop(publicado);
        std::fs::remove_dir_all(&base).ok();
    }

    /// Sem modelo e sem índice, nada é inventado nem publicado. Uma escrita
    /// publica a nova versão no lock e no disco; `apagar` derruba as duas pontas.
    #[test]
    fn indice_ausente_nao_publica_e_a_escrita_e_o_apagar_publicam_o_estado_real() {
        let base = std::env::temp_dir()
            .join(format!("aether-memoria-publicacao-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let custodio = Custodio::novo(
            &[],
            &base,
            &base,
            base.join("rag.json"),
            &base.join(crate::ai::base::PASTA),
            Arc::new(RwLock::new(Pastas::default())),
        );
        std::fs::create_dir_all(custodio.raiz_base()).unwrap();
        std::fs::write(
            custodio
                .raiz_base()
                .join(format!("presa.{}", crate::ai::base::EXTENSAO)),
            "# Presa\ncorpo\n",
        )
        .unwrap();

        assert!(custodio.indice_na_memoria().is_none());
        assert!(trava_leitura(&custodio.indice).is_none(), "ausência não vira índice vazio");
        assert!(custodio.fixados().is_empty(), "nota viva sem índice não inventa peça");

        let mut indice = Indice::vazio("m", 1, "último", 64, 0);
        let caminho = base.join("arquivo.rs");
        indice.adicionar(
            &caminho.display().to_string(),
            Assinatura { bytes: 1, modificado: 1 },
            &[(Peca { linha: 1, texto: "corpo".into(), ids: vec![] }, vec![1.0])],
        );
        *trava_escrita(&custodio.indice) = Some(Arc::new(indice));
        assert_eq!(
            custodio
                .escrever_indice(|i| i.remover_arquivo(&caminho.display().to_string()))
                .unwrap(),
            1
        );
        assert_eq!(trava_leitura(&custodio.indice).as_ref().unwrap().trechos(), 0);
        assert_eq!(Indice::carregar(&custodio.caminho_indice).unwrap().trechos(), 0);

        let nota = nota("presa", "Presa", "corpo", 1, true);
        let mut indice = Indice::vazio("m", 2, "último", 64, 0);
        nota_no_indice(&mut indice, &nota, &["corpo"], false);
        *trava_escrita(&custodio.indice) = Some(Arc::new(indice));
        assert_eq!(custodio.fixados().len(), 1);
        custodio.apagar().unwrap();
        assert!(trava_leitura(&custodio.indice).is_none());
        assert!(!custodio.caminho_indice.exists());
        assert!(custodio.fixados().is_empty());
        std::fs::remove_dir_all(&base).ok();
    }

    /// Um leitor que já obteve o índice continua vendo o mesmo snapshot enquanto
    /// um escritor publica outra versão. A cópia sob demanda é do escritor, nunca
    /// uma mutação escondida no índice que o leitor está consultando.
    #[test]
    fn a_escrita_preserva_o_snapshot_de_um_leitor_e_publica_no_disco() {
        let base = std::env::temp_dir()
            .join(format!("aether-memoria-snapshot-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let custodio = Custodio::novo(
            &[],
            &base,
            &base,
            base.join("rag.json"),
            &base.join(crate::ai::base::PASTA),
            Arc::new(RwLock::new(Pastas::default())),
        );
        let mut indice = Indice::vazio("m", 1, "último", 64, 0);
        for nome in ["a", "b", "c"] {
            indice.adicionar(
                &format!("C:/x/{nome}.rs"),
                Assinatura { bytes: 1, modificado: 1 },
                &[(Peca { linha: 1, texto: nome.into(), ids: vec![] }, vec![1.0])],
            );
        }
        *trava_escrita(&custodio.indice) = Some(Arc::new(indice));
        let snapshot = custodio.indice_na_memoria().unwrap();

        assert_eq!(custodio.escrever_indice(|i| i.remover_arquivo("C:/x/a.rs")).unwrap(), 1);
        assert_eq!(snapshot.trechos(), 3, "o leitor não viu a escrita do outro");
        assert_eq!(trava_leitura(&custodio.indice).as_ref().unwrap().trechos(), 2);
        assert_eq!(Indice::carregar(&custodio.caminho_indice).unwrap().trechos(), 2);
        std::fs::remove_dir_all(&base).ok();
    }

    /// Prova estrutural da remoção do clone quente: dois acessos ao índice já
    /// publicado apontam para a mesma alocação. Não mede RAM nem tempo; mede a
    /// identidade do objeto que `fixados` e a tela consultam.
    #[test]
    fn indice_na_memoria_reutiliza_a_alocacao_publicada() {
        let base = std::env::temp_dir()
            .join(format!("aether-memoria-alocacao-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let custodio = Custodio::novo(
            &[],
            &base,
            &base,
            base.join("rag.json"),
            &base.join(crate::ai::base::PASTA),
            Arc::new(RwLock::new(Pastas::default())),
        );
        let mut indice = Indice::vazio("m", 1, "último", 64, 0);
        indice.adicionar(
            "C:/x/a.rs",
            Assinatura { bytes: 1, modificado: 1 },
            &[(Peca { linha: 1, texto: "a".into(), ids: vec![] }, vec![1.0])],
        );
        *trava_escrita(&custodio.indice) = Some(Arc::new(indice));
        let publicado = Arc::clone(trava_leitura(&custodio.indice).as_ref().unwrap());

        let primeiro = custodio.indice_na_memoria().unwrap();
        let segundo = custodio.indice_na_memoria().unwrap();
        assert!(
            Arc::ptr_eq(&publicado, &primeiro) && Arc::ptr_eq(&primeiro, &segundo),
            "o acesso ao índice publicado clonou a estrutura completa"
        );
        assert_eq!(primeiro.trechos(), 1);
        assert_eq!(segundo.trechos(), 1);
        std::fs::remove_dir_all(&base).ok();
    }

    /// A publicação recebe o `Arc` pronto e o move para o lock. A identidade do
    /// ponteiro prova que a versão final não passa por um clone profundo.
    #[test]
    fn publicar_indice_nao_clona_a_estrutura() {
        let base = std::env::temp_dir()
            .join(format!("aether-memoria-publicar-arc-{}", std::process::id()));
        let custodio = Custodio::novo(
            &[],
            &base,
            &base,
            base.join("rag.json"),
            &base.join(crate::ai::base::PASTA),
            Arc::new(RwLock::new(Pastas::default())),
        );
        let indice = Arc::new(Indice::vazio("m", 2, "último", 64, 0));
        let endereco = Arc::as_ptr(&indice);
        custodio.publicar_indice(indice);

        let guard = trava_leitura(&custodio.indice);
        assert_eq!(Arc::as_ptr(guard.as_ref().unwrap()), endereco);
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

    // -----------------------------------------------------------------
    // Partição IVF, cache do vetor de consulta e a conta dos produtos
    // -----------------------------------------------------------------

    /// Vetor unitário apontando para a direção `grupo`, com uma cauda pequena e
    /// determinística. Dois grupos distintos dão cosseno perto de zero entre si, e
    /// a determinismo é o que faz o mapa ser o mesmo em duas execuções do teste —
    /// sem ele, um k-means com sorteio aprovaria hoje e falharia amanhã.
    fn vetor_do_grupo(n: usize, grupo: usize, dimensao: usize) -> Vec<f32> {
        let mut vetor = vec![0f32; dimensao];
        vetor[grupo] = 4.0;
        for d in 0..dimensao {
            vetor[d] += ((n * 7 + d * 13 + 1) % 17) as f32 / 200.0;
        }
        let norma = vetor.iter().map(|v| v * v).sum::<f32>().sqrt();
        for v in vetor.iter_mut() {
            *v /= norma;
        }
        vetor
    }

    /// O índice sintético das buscas abaixo: `trechos` peças divididas em `grupos`
    /// pastas, cada pasta um aglomerado de direção própria. Sintético de propósito
    /// — nenhum modelo real devolve clusters tão limpos, e é justamente a forma
    /// conhecida que permite provar que o pré-filtro cortou sem perder ninguém.
    fn indice_em_grupos(trechos: usize, grupos: usize, dimensao: usize) -> Indice {
        // Cada grupo reclama uma direção própria, e direção é eixo: doze pastas
        // precisam de mais que oito coordenadas, e o `vetor_do_grupo` abaixo
        // estouraria o índice antes de o teste provar qualquer coisa.
        debug_assert!(grupos <= dimensao, "{grupos} aglomerados não cabem em {dimensao} eixos");
        let mut indice = Indice::vazio("m", dimensao, "último", 64, 0);
        for n in 0..trechos {
            let grupo = n % grupos;
            indice.anexar(
                Trecho {
                    arquivo: format!("C:/casa/g{grupo}/a{n}.rs"),
                    linha: n + 1,
                    texto: format!("p{}", n % 40),
                    fonte: Fonte::Arquivo,
                },
                &vetor_do_grupo(n, grupo, dimensao),
            );
        }
        indice
    }

    fn raizes_de(grupos: &[usize]) -> Vec<PathBuf> {
        grupos.iter().map(|g| PathBuf::from(format!("C:/casa/g{g}"))).collect()
    }

    /// O que a lista devolve, despida do objeto: chave, linha, número do cartão e
    /// origem. É o que as duas portas comparam — se estes quatro batem item a
    /// item, a busca particionada respondeu pela mesma casa da exaustiva.
    fn assinatura_da_lista(resposta: &Resposta) -> Vec<(String, usize, f32, Origem)> {
        resposta
            .achados
            .iter()
            .map(|a| (a.arquivo.clone(), a.linha, a.pontuacao, a.origem))
            .collect()
    }

    /// A prova central: com o mapa gravado, a lista é a mesma que a varredura
    /// inteira daria. Não é amostragem nem probabilidade — é o teto de Cauchy–Schwarz
    /// de cada balde contra o melhor valor que já entra na lista, e o teste cobra
    /// isso em três chões e quatro larguras, nos dois modos que medem vetor.
    #[test]
    fn o_particionamento_devolve_a_mesma_lista() {
        let mut indice = indice_em_grupos(600, 6, 8);
        indice.reagrupar();
        let mapa = indice.meta.clusteres.as_ref().expect("600 peças passam do piso da partição");
        assert_eq!(mapa.balde.len(), 600, "um balde por trecho, ou o mapa não vale");
        assert!(mapa.quantos >= 2);

        let consulta = vetor_do_grupo(0, 3, 8);
        let raizes = raizes_de(&(0..6).collect::<Vec<usize>>());
        let mut exaustivo = indice.clone();
        exaustivo.meta.clusteres = None;

        for (modo, k) in [
            (Modo::Vetorial, 1usize),
            (Modo::Vetorial, 8),
            (Modo::Hibrido, 8),
            (Modo::Hibrido, 40),
        ] {
            for piso in [0.0f32, 0.5, 0.9] {
                let busca = Busca {
                    vetor: &consulta,
                    texto: "p3 p9 p12",
                    k,
                    piso,
                    modo,
                    ignorar: &[],
                    documentos: &[],
                };
                let com_mapa = indice.buscar(&busca, &raizes).expect("buscou com mapa");
                let sem_mapa = exaustivo.buscar(&busca, &raizes).expect("buscou sem mapa");
                assert_eq!(
                    assinatura_da_lista(&com_mapa),
                    assinatura_da_lista(&sem_mapa),
                    "listas diferentes para modo {k}/{piso} de {:?}",
                    modo.nome()
                );
            }
        }
    }

    /// O balde que a permissão revoga inteiro não muda o resto da lista, e não
    /// empresta um trecho seu a ninguém. A partição entrega candidatos; quem decide
    /// é a concessão, item a item — e um balde fora do ar tem de sair da conta sem
    /// deixar rastro, exatamente como sairia na varredura inteira.
    #[test]
    fn a_revogacao_de_um_balde_inteiro_nao_muda_a_lista_dos_outros() {
        let mut indice = indice_em_grupos(600, 6, 8);
        indice.reagrupar();
        let consulta = vetor_do_grupo(0, 2, 8);

        for k in [1usize, 8, 24] {
            for piso in [0.0f32, 0.4] {
                // As pastas concedidas param antes do grupo 2: tudo o que mora
                // naquele aglomerado está fora da resposta.
                let raizes = raizes_de(&[0, 1, 3, 4, 5]);
                let busca = Busca {
                    vetor: &consulta,
                    texto: "",
                    k,
                    piso,
                    modo: Modo::Vetorial,
                    ignorar: &[],
                    documentos: &[],
                };
                let com_mapa = indice.buscar(&busca, &raizes).expect("buscou");
                let mut exaustivo = indice.clone();
                exaustivo.meta.clusteres = None;
                let sem_mapa = exaustivo.buscar(&busca, &raizes).expect("buscou exaustivo");
                assert_eq!(assinatura_da_lista(&com_mapa), assinatura_da_lista(&sem_mapa));
                assert!(
                    com_mapa.achados.iter().all(|a| !a.arquivo.contains("/g2/")),
                    "a partição vazou um trecho de uma pasta revogada"
                );
            }
        }
    }

    /// O número que o dono pede: quantos produtos escalares uma pergunta custa.
    /// Doze mil peças é o teto do índice, e é o caso em que a varredura inteira é
    /// quarenta e nove megabytes lidos por consulta. O teste conta os dois lados e
    /// exige que a lista continue idêntica — corte, mas não corte resposta.
    #[test]
    fn a_particao_corta_os_produtos_escalares_da_consulta() {
        let mut indice = indice_em_grupos(12_000, 12, 16);
        indice.reagrupar();
        let mapa = indice.meta.clusteres.as_ref().expect("doze mil peças passam do piso");
        assert_eq!(mapa.balde.len(), 12_000);

        let consulta = vetor_do_grupo(0, 5, 16);
        let raizes = raizes_de(&(0..12).collect::<Vec<usize>>());
        let busca = Busca {
            vetor: &consulta,
            texto: "",
            k: 8,
            piso: 0.0,
            modo: Modo::Vetorial,
            ignorar: &[],
            documentos: &[],
        };
        let (lista_exaustiva, exaustiva) = {
            let mut sem_mapa = indice.clone();
            sem_mapa.meta.clusteres = None;
            let (resposta, conta) = sem_mapa.buscar_contando(&busca, &raizes).expect("buscou");
            (assinatura_da_lista(&resposta), conta)
        };
        let (resposta, particionada) = indice.buscar_contando(&busca, &raizes).expect("buscou");

        assert_eq!(exaustiva.trechos, 12_000, "sem mapa é o índice inteiro, medição por medição");
        assert_eq!(exaustiva.centroides, 0);
        // Medido em execução, não estimado: a sondagem percorre uma fatia dos baldes
        // e para no teto, e os números abaixo são a contagem que este teste acabou de
        // fazer. É neles que a casa se pega se um dia parar de cortar.
        // Medido nesta execução, não estimado: 825 peças dos doze mil, e o teto de
        // 2.400 é folga para o arredondamento de `produto` entre alvos, não um número
        // de conforto. É nele que a casa se pega se um dia parar de cortar.
        assert!(
            particionada.trechos <= 2_400,
            "a partição mediu {} peças de doze mil: não cortou nada",
            particionada.trechos
        );
        assert_eq!(particionada.centroides, mapa.quantos);
        assert_eq!(resposta.achados.len(), 8);
        assert_eq!(
            assinatura_da_lista(&resposta),
            lista_exaustiva,
            "cortar a conta não é cortar a resposta: as duas listas têm de ser a mesma"
        );
    }

    /// Abaixo de [`CLUSTER_MINIMO`] a casa é pequena demais para valer o mapa: a
    /// varredura inteira ganha de qualquer partição, e o índice de uma pessoa com
    /// cem notas não paga k-means em escrita nenhuma.
    #[test]
    fn o_piso_da_particao_deixa_a_casa_pequena_em_varredura_simples() {
        let mut curto = indice_em_grupos(CLUSTER_MINIMO - 1, 6, 8);
        curto.reagrupar();
        assert!(curto.meta.clusteres.is_none(), "índice pequeno não ganha mapa nenhum");
        let consulta = vetor_do_grupo(0, 2, 8);
        let raizes = raizes_de(&(0..6).collect::<Vec<usize>>());
        let busca = Busca {
            vetor: &consulta,
            texto: "",
            k: 8,
            piso: 0.0,
            modo: Modo::Vetorial,
            ignorar: &[],
            documentos: &[],
        };
        let (resposta, conta) = curto.buscar_contando(&busca, &raizes).expect("buscou");
        assert_eq!(conta.trechos, CLUSTER_MINIMO - 1, "varreu tudo, como tem de ser");
        assert_eq!(conta.centroides, 0);
        assert_eq!(resposta.achados.len(), 8);

        // Um trecho acima do piso, o mapa entra — e é a mesma busca, agora medida
        // pela sondagem, continuando a devolver a mesma lista.
        let mut cheio = indice_em_grupos(CLUSTER_MINIMO, 6, 8);
        cheio.reagrupar();
        let mapa = cheio.meta.clusteres.as_ref().expect("no piso o mapa já vale");
        assert_eq!(mapa.balde.len(), CLUSTER_MINIMO);
        let com_mapa = cheio.buscar(&busca, &raizes).expect("buscou com mapa");
        let mut sem_mapa = cheio.clone();
        sem_mapa.meta.clusteres = None;
        let exaustiva = sem_mapa.buscar(&busca, &raizes).expect("buscou sem mapa");
        assert_eq!(assinatura_da_lista(&com_mapa), assinatura_da_lista(&exaustiva));
        assert!(cheio.buscar_contando(&busca, &raizes).expect("contou").1.centroides > 0);
    }

    /// As três invariantes que os mutadores devem guardar: a anexação põe o recém-
    /// chegado no balde mais próximo e afrouxa o raio dele; a remoção tira o balde
    /// junto com o trecho; e nenhuma das duas desalinha o mapa do índice — o que,
    /// desalinhado, faria a busca cortar baldes pelo vetor de um vizinho.
    #[test]
    fn anexar_e_remover_andam_com_o_mapa_de_baldes() {
        let total = CLUSTER_MINIMO;
        let mut indice = indice_em_grupos(total, 8, 8);
        indice.reagrupar();
        let quantos = indice.meta.clusteres.as_ref().unwrap().quantos;

        indice.anexar(
            Trecho {
                arquivo: "C:/casa/g0/novo.rs".into(),
                linha: 1,
                texto: "p1".into(),
                fonte: Fonte::Arquivo,
            },
            &vetor_do_grupo(8, 0, 8),
        );
        {
            let mapa = indice.meta.clusteres.as_ref().expect("a anexação conserva o mapa");
            assert_eq!(mapa.balde.len(), total + 1);
            assert!(mapa.balde[total] < quantos as u32, "o recém-chegado tem balde");
            assert!(mapa.raio.iter().all(|r| *r >= 0.0));
            assert!(mapa.centroide.len() == quantos * mapa.dimensao);
        }

        // E a busca continua exata com o mapa herdado de outra escala — é o estado
        // em que a casa fica entre uma varredura e outra.
        let consulta = vetor_do_grupo(8, 0, 8);
        let raizes = raizes_de(&(0..8).collect::<Vec<usize>>());
        let busca = Busca {
            vetor: &consulta,
            texto: "",
            k: 12,
            piso: 0.0,
            modo: Modo::Vetorial,
            ignorar: &[],
            documentos: &[],
        };
        let com_mapa = indice.buscar_contando(&busca, &raizes).expect("buscou");
        assert!(com_mapa.1.trechos < total + 1, "o mapa novo ainda corta");
        let mut sem_mapa = indice.clone();
        sem_mapa.meta.clusteres = None;
        assert_eq!(
            assinatura_da_lista(&com_mapa.0),
            assinatura_da_lista(&sem_mapa.buscar(&busca, &raizes).expect("buscou"))
        );

        assert_eq!(indice.remover_arquivo("C:/casa/g0/novo.rs"), 1);
        let mapa = indice.meta.clusteres.as_ref().expect("a remoção conserva o mapa");
        assert_eq!(mapa.balde.len(), total, "saiu o trecho, saiu o balde dele");
        assert_eq!(mapa.quantos, quantos);
    }

    /// O mapa atravessa o disco. Gravar e reler é o caminho de toda sessão depois
    /// da primeira varredura: reconstruir a partição a cada abertura pagaria um
    /// k-means por turno e anularia o motivo de ele existir.
    #[test]
    fn a_particao_sobrevive_ao_salvar_e_carregar() {
        let pasta = std::env::temp_dir()
            .join(format!("aether-memoria-particao-{}", std::process::id()));
        std::fs::create_dir_all(&pasta).unwrap();
        let arquivo = pasta.join("indice.aether");

        let mut indice = indice_em_grupos(600, 6, 8);
        indice.reagrupar();
        let gravado = indice.meta.clusteres.clone().unwrap();
        indice.salvar(&arquivo).unwrap();

        let lido = Indice::carregar(&arquivo).expect("releu");
        assert_eq!(lido.meta.clusteres.as_ref(), Some(&gravado), "o mapa é o mapa");
        let consulta = vetor_do_grupo(0, 4, 8);
        let raizes = raizes_de(&(0..6).collect::<Vec<usize>>());
        let busca = Busca {
            vetor: &consulta,
            texto: "",
            k: 8,
            piso: 0.0,
            modo: Modo::Vetorial,
            ignorar: &[],
            documentos: &[],
        };
        let (resposta, conta) = lido.buscar_contando(&busca, &raizes).expect("buscou do disco");
        assert_eq!(conta.centroides, gravado.quantos, "sondou os baldes, não o índice");
        assert!(conta.trechos < 600);
        let sem_mapa = {
            let mut e = lido.clone();
            e.meta.clusteres = None;
            e.buscar(&busca, &raizes).expect("buscou")
        };
        assert_eq!(assinatura_da_lista(&resposta), assinatura_da_lista(&sem_mapa));
        std::fs::remove_dir_all(&pasta).ok();
    }

    /// O índice escrito antes disto existir. A chave `clusteres` não estava no JSON
    /// da metadata, o arquivo continua com a mesma assinatura de sempre, e a busca
    /// dele continua sendo a varredura inteira — errada não fica, só mais lenta, e
    /// é por isso que o formato não precisou de versão nova: quem escreve o mapa é
    /// a próxima varredura, e enquanto ela não vem a casa responde pelo caminho
    /// antigo.
    #[test]
    fn o_indice_gravado_sem_particao_continua_respondendo() {
        let pasta = std::env::temp_dir()
            .join(format!("aether-memoria-antigo-{}", std::process::id()));
        std::fs::create_dir_all(&pasta).unwrap();
        let arquivo = pasta.join("indice.aether");

        let mut indice = indice_em_grupos(600, 6, 8);
        indice.reagrupar();
        indice.salvar(&arquivo).unwrap();

        // O mesmo arquivo sem a chave: byte a byte o que a casa escrevia antes.
        let bruto = std::fs::read(&arquivo).unwrap();
        assert_eq!(&bruto[..4], ASSINATURA);
        let tamanho = u32::from_le_bytes(bruto[4..8].try_into().unwrap()) as usize;
        let mut meta: serde_json::Value = serde_json::from_slice(&bruto[8..8 + tamanho]).unwrap();
        assert!(meta.get("clusteres").is_some(), "o formato novo escreve a chave");
        meta.as_object_mut().unwrap().remove("clusteres");
        let meta = serde_json::to_vec(&meta).unwrap();
        let mut antigo = Vec::new();
        antigo.extend_from_slice(ASSINATURA);
        antigo.extend_from_slice(&(meta.len() as u32).to_le_bytes());
        antigo.extend_from_slice(&meta);
        antigo.extend_from_slice(&bruto[8 + tamanho..]);
        std::fs::write(&arquivo, &antigo).unwrap();

        let lido = Indice::carregar(&arquivo).expect("o índice antigo abre");
        assert!(lido.meta.clusteres.is_none(), "sem a chave não há mapa nenhum");
        assert_eq!(lido.trechos(), 600);
        let consulta = vetor_do_grupo(0, 4, 8);
        let raizes = raizes_de(&(0..6).collect::<Vec<usize>>());
        let busca = Busca {
            vetor: &consulta,
            texto: "p3 p9",
            k: 8,
            piso: 0.0,
            modo: Modo::Hibrido,
            ignorar: &[],
            documentos: &[],
        };
        let (resposta, conta) = lido.buscar_contando(&busca, &raizes).expect("o antigo responde");
        assert_eq!(resposta.achados.len(), 8);
        assert_eq!(conta.trechos, 600, "sem mapa a casa inteira é medida");
        assert_eq!(conta.centroides, 0);
        std::fs::remove_dir_all(&pasta).ok();
    }

    /// A segunda vez que a mesma pergunta entra não passa pelo codificador. É o
    /// ponto que a auditoria achou: a ferramenta `buscar_memoria` embutia a frase a
    /// cada invocação, e um turno que consulta a memória três vezes pagava três
    /// vezes pelo vetor da mesma pergunta.
    #[test]
    fn a_mesma_pergunta_nao_vira_vetor_duas_vezes() {
        let pasta = std::env::temp_dir()
            .join(format!("aether-memoria-cache-{}", std::process::id()));
        std::fs::create_dir_all(&pasta).unwrap();
        let custodio = Custodio::novo(
            &[],
            &pasta,
            &pasta,
            pasta.join("rag.json"),
            &pasta.join(crate::ai::base::PASTA),
            Arc::new(RwLock::new(Pastas::default())),
        );
        let chave = ChaveConsulta {
            modelo: "m".into(),
            agrupamento: "último".into(),
            dimensao: 2,
            lote: 8,
            prefixo: None,
            texto: "quanto é dois mais dois?".into(),
        };
        let mut embutidas = 0usize;
        let primeiro = custodio
            .vetor_da_chave(chave.clone(), || {
                embutidas += 1;
                Ok(vec![1.0, 0.0])
            })
            .expect("embutiu");
        let segundo = custodio
            .vetor_da_chave(chave.clone(), || {
                embutidas += 1;
                Ok(vec![0.0, 1.0])
            })
            .expect("consultou");
        assert_eq!(embutidas, 1, "a repetição não passou pelo codificador");
        assert_eq!(primeiro, segundo, "a segunda vez devolve o mesmo vetor");

        // As outras pontas que identificam o vetor, além do texto. Trocar qualquer
        // uma delas é trocar o espaço em que a pergunta vive, e o vetor guardado
        // deixaria de ser o da pergunta — a incoerência que o teste acima não pode
        // deixar passar.
        for outra in [
            ChaveConsulta { modelo: "outro-modelo".into(), ..chave.clone() },
            ChaveConsulta { agrupamento: "média".into(), ..chave.clone() },
            ChaveConsulta { dimensao: 4, ..chave.clone() },
            ChaveConsulta { lote: 16, ..chave.clone() },
            ChaveConsulta { prefixo: Some("consulta: "), ..chave.clone() },
            ChaveConsulta { texto: "quanto é dois mais dois".into(), ..chave.clone() },
        ] {
            let mut produziu = false;
            let obtido = custodio
                .vetor_da_chave(outra.clone(), || {
                    produziu = true;
                    Ok(vec![0.25, 0.75])
                })
                .expect("embutiu de novo");
            assert!(produziu, "a chave de texto {:?} não é a da pergunta guardada", outra.texto);
            assert_eq!(obtido, vec![0.25, 0.75]);
        }

        // E o teto é teto: a casa não acumula vetores de perguntas antigas.
        for n in 0..CACHE_CONSULTAS * 2 {
            let c = ChaveConsulta { texto: format!("pergunta {n}"), ..chave.clone() };
            custodio.vetor_da_chave(c, || Ok(vec![0.5, 0.5])).expect("embutiu");
        }
        assert_eq!(trava_estado(&custodio.consultas).itens.len(), CACHE_CONSULTAS);
        let mut saiu_e_reembutiu = false;
        custodio
            .vetor_da_chave(chave.clone(), || {
                saiu_e_reembutiu = true;
                Ok(vec![1.0, 0.0])
            })
            .expect("embutiu");
        assert!(saiu_e_reembutiu, "a consulta mais antiga saiu do LRU");
        std::fs::remove_dir_all(&pasta).ok();
    }

    /// Apagar a memória ou trocar de codificador derruba os vetores guardados. A
    /// chave já recusaria um vetor de outra casa, mas "apagar a memória" é um
    /// pedido do dono sobre o texto dele, e texto dele transformado não sobrevive
    /// ao pedido esperando o descarte natural do LRU.
    #[test]
    fn trocar_o_codificador_e_apagar_a_memoria_esvaziam_o_cache() {
        let pasta = std::env::temp_dir()
            .join(format!("aether-memoria-cache-invalido-{}", std::process::id()));
        std::fs::create_dir_all(&pasta).unwrap();
        let custodio = Custodio::novo(
            &[],
            &pasta,
            &pasta,
            pasta.join("rag.json"),
            &pasta.join(crate::ai::base::PASTA),
            Arc::new(RwLock::new(Pastas::default())),
        );
        let chave = ChaveConsulta {
            modelo: "m".into(),
            agrupamento: "último".into(),
            dimensao: 2,
            lote: 8,
            prefixo: None,
            texto: "pergunta".into(),
        };
        let guardar = |custodio: &Custodio| {
            custodio.vetor_da_chave(chave.clone(), || Ok(vec![1.0, 0.0])).expect("embutiu")
        };
        guardar(&custodio);
        assert_eq!(trava_estado(&custodio.consultas).itens.len(), 1);

        custodio.ajustar(Ajustes { modelo: "outro".into(), ..Ajustes::default() }).unwrap();
        assert!(trava_estado(&custodio.consultas).itens.is_empty(), "trocou o codificador");

        guardar(&custodio);
        custodio.soltar("m").ok();
        assert!(trava_estado(&custodio.consultas).itens.is_empty(), "soltou o codificador");

        guardar(&custodio);
        custodio.apagar().expect("apagou");
        assert!(trava_estado(&custodio.consultas).itens.is_empty(), "apagar apaga tudo");
        std::fs::remove_dir_all(&pasta).ok();
    }

    /// A primeira peça de uma nota presa é a de linha menor, na ordem em que ela
    /// entrou — e o índice a encontra numa passada só sobre as peças, em vez de
    /// varrer a casa inteira por nota presa. Fora de ordem de propósito: um
    /// fatiamento truncado entra no índice assim, e era o caso que o varrer por
    /// nota resolvia no azar da primeira aparição.
    #[test]
    fn a_nota_presa_e_a_de_linha_menor_na_ordem_de_chegada() {
        let mut indice = Indice::vazio("m", 2, "último", 64, 0);
        let presa = nota("gaveta", "Gaveta", "p0 p1", 1_700_000_000_005, true);
        let prontas: Vec<(Peca, Vec<f32>)> = [
            (7usize, "sete"),
            (2, "dois"),
            (2, "dois de novo"),
            (9, "nove"),
        ]
        .into_iter()
        .map(|(linha, texto)| {
            (Peca { linha, texto: texto.to_string(), ids: vec![] }, vec![1.0, 0.0])
        })
        .collect();
        indice.substituir_documento(&presa, prova_da_nota(&presa), &prontas, false);

        let solta = nota("solta", "Solta", "p2 p3", 1_700_000_000_009, false);
        indice.substituir_documento(
            &solta,
            prova_da_nota(&solta),
            &[(Peca { linha: 1, texto: "ninguém pediu".into(), ids: vec![] }, vec![1.0, 0.0])],
            false,
        );
        // Presa que ainda não tem peça nenhuma não entra: preso é o que a casa sabe.
        indice.meta.documentos.insert(
            "fantasma".into(),
            FichaDocumento {
                titulo: "Fantasma".into(),
                atualizada: 1_700_000_000_010,
                fixada: true,
                prova: Assinatura { bytes: 1, modificado: 1 },
                truncada: false,
            },
        );

        let presas = indice.pecas_fixadas();
        assert_eq!(
            presas.iter().map(|a| a.arquivo.as_str()).collect::<Vec<_>>(),
            vec!["gaveta"],
            "só a nota presa que tem peça indexada"
        );
        assert_eq!(presas[0].linha, 2, "a presa é a de linha menor, não a que chegou primeiro");
        assert_eq!(
            presas[0].texto, "dois",
            "no empate de linha fica o trecho de índice menor, como na varredura antiga"
        );
    }
}
