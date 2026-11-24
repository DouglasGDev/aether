//! Catálogo de modelos que o Aether sabe baixar e executar.
//!
//! Só entra aqui arquitetura que o motor realmente executa. Cada tamanho foi
//! lido da árvore de arquivos do HuggingFace, não estimado.

use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Tier {
    /// Tarefas triviais e médias: o menor modelo capaz.
    Leve,
    /// Tarefas de alta complexidade: mais capacidade, mais memória.
    Pesado,
}

/// Para o que o modelo serve. É o que separa o catálogo de conversa do de
/// memória: um modelo de embeddings não responde nada e um de conversa não
/// produz vetor — deixar os dois na mesma lista faria o roteador escolher um
/// codificador para falar com o usuário.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Genero {
    Conversa,
    Embeddings,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSpec {
    pub id: &'static str,
    pub display: &'static str,
    /// Nome do modelo sem a quant: é o que a UI usa para agrupar as variantes.
    pub series: &'static str,
    pub family: &'static str,
    pub tier: Tier,
    pub genero: Genero,
    pub parameters_b: f32,
    pub license: &'static str,
    pub quant: &'static str,
    /// Pasta em disco. Variantes do mesmo tamanho compartilham a pasta e os
    /// arquivos de apoio, então baixar duas quants não duplica o tokenizer.
    pub dir: &'static str,
    pub weights_repo: &'static str,
    pub weights_file: &'static str,
    pub weights_bytes: u64,
    pub support_repo: &'static str,
    /// O que o motor exige além dos pesos. Um modelo de conversa precisa do
    /// template e do `generation_config`; um codificador não tem nenhum dos
    /// dois — pedir arquivo que não existe seria um download quebrando para
    /// sempre.
    pub support_files: &'static [&'static str],
    /// O texto que o modelo exige antes de uma *consulta* (não antes do
    /// documento). É declaração do próprio modelo: o Qwen3-Embedding foi treinado
    /// com uma instrução na pergunta e nenhuma no texto, e embutir os dois do
    /// mesmo jeito derruba a recuperação. `None` é o modelo que não pede nada.
    pub query_prefix: Option<&'static str>,
    pub context_limit: usize,
    /// Qualidade suficiente para o roteador escolher sem o usuário pedir.
    pub routable: bool,
}

/// Arquivos de apoio que um modelo de conversa exige além dos pesos.
pub const SUPPORT_FILES: [&str; 4] = [
    "tokenizer.json",
    "tokenizer_config.json",
    "generation_config.json",
    "chat_template.jinja",
];

/// Os de um codificador: só o tokenizador. Não há template de conversa nem EOS
/// de geração — o que sai dele é um vetor, não texto.
pub const SUPPORT_EMBEDDINGS: [&str; 2] = ["tokenizer.json", "tokenizer_config.json"];

/// O que um GGUF que traz o vocabulário e o template dentro de si precisa vir de
/// fora: o tokenizador rápido, que é o que o aplicativo usa para contar tokens e
/// montar o prompt. O template e o fim de resposta são lidos do próprio arquivo
/// que roda, então pedir os outros dois seria baixar o que não se usa — e
/// reclamar de um `generation_config.json` que o repositório não publica.
pub const SUPPORT_TOKENIZADOR: [&str; 2] = ["tokenizer.json", "tokenizer_config.json"];

/// Abaixo de Q4_K_M a perda de qualidade é visível: o usuário pode baixar,
/// mas o roteador não escolhe sozinho. Cada macro é uma série: os fatos que não
/// mudam entre quants ficam escritos uma vez, e cada linha do catálogo é
/// `(quant, bytes, routable)`. Os bytes são os do `tree/main` do HuggingFace,
/// lidos arquivo por arquivo — não é conta de parâmetro.
macro_rules! granite {
    ($size:literal, $tier:expr, $params:expr, $quant:literal, $bytes:expr, $routable:expr) => {
        ModelSpec {
            id: concat!("granite-4.2-", $size, "b-", $quant),
            display: concat!("Granite 4.2 ", $size, "B ", $quant),
            series: concat!("Granite 4.2 ", $size, "B"),
            family: "granite",
            tier: $tier,
            genero: Genero::Conversa,
            parameters_b: $params,
            license: "Apache-2.0",
            quant: $quant,
            dir: concat!("granite-4.2-", $size, "b"),
            weights_repo: concat!("ibm-granite/granite-4.2-", $size, "b-GGUF"),
            weights_file: concat!("granite-4.2-", $size, "b-", $quant, ".gguf"),
            weights_bytes: $bytes,
            support_repo: concat!("ibm-granite/granite-4.2-", $size, "b"),
            support_files: &SUPPORT_FILES,
            query_prefix: None,
            context_limit: 131_072,
            routable: $routable,
        }
    };
}

// O modelo de programação. A série Ornith 1.5 é a que os rankings de código de
// 2026 mostram no topo e a que o Granite não alcança: 9B densos, janela de 262
// mil, MIT, e — medido no template que o próprio GGUF publica — a mesma
// gramática XML de chamada de ferramenta que a sessão já sabe ler. A arquitetura
// `qwen35` é registrada pela llama.dll vendorizada, e é isso que o teste do
// catálogo confere contra o binário.
macro_rules! ornith {
    ($quant:literal, $bytes:expr, $routable:expr) => {
        ModelSpec {
            id: concat!("ornith-1.5-9b-", $quant),
            display: concat!("Ornith 1.5 9B ", $quant),
            series: "Ornith 1.5 9B",
            family: "qwen35",
            tier: Tier::Pesado,
            genero: Genero::Conversa,
            parameters_b: 9.0,
            license: "MIT",
            quant: $quant,
            dir: "ornith-1.5-9b",
            weights_repo: "ornith-ai/Ornith-1.5-9B-GGUF",
            weights_file: concat!("Ornith-1.5-9B-", $quant, ".gguf"),
            weights_bytes: $bytes,
            support_repo: "ornith-ai/Ornith-1.5-9B",
            support_files: &SUPPORT_TOKENIZADOR,
            query_prefix: None,
            context_limit: 262_144,
            routable: $routable,
        }
    };
}

/// O que cada arquivo destes GGUF declara sobre si, lido do próprio cabeçalho
/// com `Range: bytes=0-16777215` no `resolve/main` do HuggingFace em
/// 2026-11-20 — como a leitura foi feita e o que ela devolveu estão em
/// `docs/melhorias/CATALOGO.md`. Nada aqui veio do cartão do repositório:
/// `general.architecture`, `general.size_label`, `general.license` e
/// `*.context_length` são o que o arquivo que roda carrega. A lição da leitura:
/// no GGUF v3 o tipo de um par
/// chave → valor é `uint32`, não um byte, e um parser que assume o byte pede um
/// tokenizador de 100.663.296 bytes e falha em silêncio.
macro_rules! qwen35 {
    ($size:literal, $tier:expr, $params:expr, $quant:literal, $bytes:expr, $routable:expr) => {
        ModelSpec {
            id: concat!("qwen3.5-", $size, "b-", $quant),
            display: concat!("Qwen3.5 ", $size, "B ", $quant),
            series: concat!("Qwen3.5 ", $size, "B"),
            family: "qwen35",
            tier: $tier,
            genero: Genero::Conversa,
            parameters_b: $params,
            license: "Apache-2.0",
            quant: $quant,
            dir: concat!("qwen3.5-", $size, "b"),
            weights_repo: concat!("unsloth/Qwen3.5-", $size, "B-GGUF"),
            weights_file: concat!("Qwen3.5-", $size, "B-", $quant, ".gguf"),
            weights_bytes: $bytes,
            support_repo: concat!("Qwen/Qwen3.5-", $size, "B"),
            support_files: &SUPPORT_TOKENIZADOR,
            query_prefix: None,
            context_limit: 262_144,
            routable: $routable,
        }
    };
}

/// O Qwen3.5 35B A3B, lido do cabeçalho do próprio arquivo com
/// `Range: bytes=0-16777215` no `resolve/main` em 2026-11-24 (a leitura está em
/// `docs/melhorias/CATALOGO.md`):
/// arquitetura `qwen35moe`, `general.size_label` = `35B-A3B`, licença
/// `apache-2.0`, janela de 262.144, 40 camadas, 256 peritos com 8 usados por
/// token e `full_attention_interval` 4 — dez camadas de atenção cheia e trinta de
/// atenção linear, o híbrido cujo estado o alojamento já cobra como reserva fixa
/// em vez de KV. São os fatos de cada um dos catorze arquivos oferecidos, lidos
/// um por um, e não de um transferido para os outros: os onze IQ/MXFP4 entraram
/// com o leitor de cabeçalho próprio, que mede o bloco daqueles tipos. O template
/// que vem dentro do arquivo declara a chamada de
/// ferramenta no mesmo XML que o laço do Aether escreve e lê (`CALL_OPEN` e
/// `FN_OPEN` em `ai/chat.rs`), diferente aqui do Gemma 4, que usa marcadores
/// próprios. O que falta continua sendo a corrida com a placa, que nenhuma
/// família tem medida.
macro_rules! qwen35moe {
    ($quant:literal, $bytes:expr, $routable:expr) => {
        ModelSpec {
            id: concat!("qwen3.5-35b-a3b-", $quant),
            display: concat!("Qwen3.5 35B A3B ", $quant),
            series: "Qwen3.5 35B A3B",
            family: "qwen35moe",
            tier: Tier::Pesado,
            genero: Genero::Conversa,
            parameters_b: 35.0,
            license: "Apache-2.0",
            quant: $quant,
            dir: "qwen3.5-35b-a3b",
            weights_repo: "unsloth/Qwen3.5-35B-A3B-GGUF",
            weights_file: concat!("Qwen3.5-35B-A3B-", $quant, ".gguf"),
            weights_bytes: $bytes,
            support_repo: "Qwen/Qwen3.5-35B-A3B",
            support_files: &SUPPORT_TOKENIZADOR,
            query_prefix: None,
            context_limit: 262_144,
            routable: $routable,
        }
    };
}

/// O Gemma 4 12B pelo mesmo caminho: arquitetura `gemma4`, licença `apache-2.0`
/// e janela de 262.144, todas lidas do cabeçalho do arquivo. O template dele
/// declara chamada de ferramenta, mas com os marcadores próprios do modelo — o
/// laço do Aether escreve e lê o XML da família `qwen35`, então o que um Gemma 4
/// faz nesse laço ainda não foi medido com a placa, e isso está registrado como
/// pendência em `docs/melhorias/CATALOGO.md`, não como capacidade.
macro_rules! gemma4 {
    ($quant:literal, $bytes:expr, $routable:expr) => {
        ModelSpec {
            id: concat!("gemma-4-12b-it-", $quant),
            display: concat!("Gemma 4 12B It ", $quant),
            series: "Gemma 4 12B It",
            family: "gemma4",
            tier: Tier::Pesado,
            genero: Genero::Conversa,
            parameters_b: 12.0,
            license: "Apache-2.0",
            quant: $quant,
            dir: "gemma-4-12b-it",
            weights_repo: "unsloth/gemma-4-12b-it-GGUF",
            weights_file: concat!("gemma-4-12b-it-", $quant, ".gguf"),
            weights_bytes: $bytes,
            support_repo: "google/gemma-4-12B-it",
            support_files: &SUPPORT_TOKENIZADOR,
            query_prefix: None,
            context_limit: 262_144,
            routable: $routable,
        }
    };
}

/// Os pesos do Granite 4.2 3B com a soma que o HuggingFace publica. Cada linha
/// é `(arquivo, lfs.oid)` lido de
/// `curl -s https://huggingface.co/api/models/ibm-granite/granite-4.2-3b-GGUF/tree/main`
/// em 2026-11-20, arquivo por arquivo, no mesmo espírito dos `weights_bytes`:
/// medido, não estimado. O `lfs.oid` é o sha256 do *conteúdo*; o `oid` do git,
/// que vem na mesma resposta, é a soma SHA-1 do ponteiro LFS e não vale como
/// prova do arquivo. A tabela completa conferida está em `docs/melhorias/M8.md`.
const SHA256_GRANITE_3B_GGUF: [(&str, &str); 15] = [
    ("granite-4.2-3b-Q2_K.gguf", "a66f94672d05104517ae844664cac86086f627e29f8e6410bce4c42849afc71b"),
    ("granite-4.2-3b-Q3_K_S.gguf", "cd7dd8e5c38523514d70083e4cbd88a0cecb5c8f86ef57cd0b80c15b84aa369e"),
    ("granite-4.2-3b-Q3_K_M.gguf", "cf620d4d440f544ece5e6257ed86539d7729d75c3585c9e17dd74d2632a34322"),
    ("granite-4.2-3b-Q3_K_L.gguf", "99f8a87638f740420b415017901e1b23a3701e4f12b76cf42518bdaa01482e83"),
    ("granite-4.2-3b-Q4_0.gguf", "d34a4a7c0b230d3f544eca502beb7e04efc3a0ebd59e2f2df50058f23cc20711"),
    ("granite-4.2-3b-Q4_K_S.gguf", "2719546cd7202b70099aa6b9e03b4d4d69851bef12089bcf5c351a72b61a1de9"),
    ("granite-4.2-3b-Q4_K_M.gguf", "e0406663965846ae22a403456eb826ccce5f450840491f71952f18a7cb78e7d5"),
    ("granite-4.2-3b-Q4_1.gguf", "2fe0f96ec5c7aff6f0713e5970ec68a1066231b78d5a48783338106a90abfb7e"),
    ("granite-4.2-3b-Q5_0.gguf", "edf6a67910492f875646347fe9df5a1ca584831c1acde19923ba9a73b145d951"),
    ("granite-4.2-3b-Q5_K_S.gguf", "345c66d60ca7a7b43e2cde9b8876cf06bf47775ba9cc23253a3235afd0d72726"),
    ("granite-4.2-3b-Q5_K_M.gguf", "982e797413f842329266a00c2a85ce540bb5c1b65ac4646276bfb9d2886c5efe"),
    ("granite-4.2-3b-Q5_1.gguf", "42804ba675926aeacb4c5940d656caaa3869667fdaf42e5966244c1ab10c973d"),
    ("granite-4.2-3b-Q6_K.gguf", "2e08e2790489162105be79b7f26a81fde28ea30119fa6df12594acd59f12dd96"),
    ("granite-4.2-3b-Q8_0.gguf", "fbe986738041418e26de9e123ba740cb654931f85bf572a71bd01f9e6b85e53d"),
    ("granite-4.2-3b-bf16.gguf", "8aad15f38ab5796190c9701b1913fd8f0bef01968a42cd16ffe469831564ef47"),
];

/// O Granite 4.2 8B, mesma leitura (`tree/main` de 2026-11-20).
const SHA256_GRANITE_8B_GGUF: [(&str, &str); 15] = [
    ("granite-4.2-8b-Q2_K.gguf", "dbaf5bba1dff2e37fb4e9563ae322001f2228d37d2080e55796fef6c8b81ef1f"),
    ("granite-4.2-8b-Q3_K_S.gguf", "7b9849e1d501e7d85a6a7ef1c9acc23733c5bb1b63ecf0bac1c48bbdb6b5500c"),
    ("granite-4.2-8b-Q3_K_M.gguf", "921fd8b015a34420607300831cf70ed306581b9bf33742ae804e42b894a2ca9a"),
    ("granite-4.2-8b-Q3_K_L.gguf", "6230bceb89841b65d0455019f134a47fb6f4316dbc1f909747ec4329518220eb"),
    ("granite-4.2-8b-Q4_0.gguf", "82af51c80f8c2913430aa020f97bd3d4708c5fc49c5bfbff09ac012f3c2132d2"),
    ("granite-4.2-8b-Q4_K_S.gguf", "6b2438a9177be0883b27722dadd5271c8e18806e068a7fc30a42c481c000c092"),
    ("granite-4.2-8b-Q4_K_M.gguf", "16a9369d0805f80b7377d25d87f937a90c05dc04ad79173a52001e42c9aab311"),
    ("granite-4.2-8b-Q4_1.gguf", "02c58f69dcd7a5f75ce2dbb22da706872c859fe6fa4b321d33f22dab19180463"),
    ("granite-4.2-8b-Q5_0.gguf", "49efb3a7a5bd51c92d72f5d3f858ab5f2a00cbf05f853909d443da1e53725bf5"),
    ("granite-4.2-8b-Q5_K_S.gguf", "fd9866f3fdd5b3fd8a0a7942ede9f7e96f53a02e13b9ad77f295b8e08446b0dd"),
    ("granite-4.2-8b-Q5_K_M.gguf", "074ac7ee98b1703674657a65051caebe188d543656e0ad71eb1f6bb3ae063407"),
    ("granite-4.2-8b-Q5_1.gguf", "76f9e24373c2848c207f669ceac1755f8b851606c4de7d4f22c0dfac0a7a8d2b"),
    ("granite-4.2-8b-Q6_K.gguf", "5ad3d67f6fe09a448031029bead7f6a088697ca2787c0c5640bf964c59df1a21"),
    ("granite-4.2-8b-Q8_0.gguf", "fb66ad5750680c77c76b9dc095961375f14fc88ba15fd5fc084ef6e6701bad77"),
    ("granite-4.2-8b-bf16.gguf", "d82690e0dc827f2c43effeb3d489f572afbe2541fa8b4895b0d958ce473925a6"),
];

/// Os pesos do Ornith 1.5 9B, lidos do mesmo endpoint (`tree/main` de
/// `ornith-ai/Ornith-1.5-9B-GGUF`).
const SHA256_ORNITH_GGUF: [(&str, &str); 5] = [
    ("Ornith-1.5-9B-Q4_K_M.gguf", "70c112196e0b7023803c9762752e46d29e612a92c83f995bc3ba1ceb07e8fab6"),
    ("Ornith-1.5-9B-Q5_K_M.gguf", "e4d9634a3b6546a5c00a8680568fe1125f6c98c704ee51ae52ba07650fb4247d"),
    ("Ornith-1.5-9B-Q6_K.gguf", "b6f76e74f86245b3caee014b797c10dca931c4dfdaabfb134eab655f81e4154a"),
    ("Ornith-1.5-9B-Q8_0.gguf", "22086870b009dbe9815ee752c48a82de930118a7c5ce5599590892ae03b8b010"),
    ("Ornith-1.5-9B-BF16.gguf", "a72af9b7f34727a49e030627092e7ebd782e9c3459a0c95afa4b9a07a8650f50"),
];

/// Os pesos do Qwen3.5, lidos do `tree/main` de cada repositório da `unsloth` em
/// 2026-11-20. O arquivo entra com a soma do `lfs.oid`, como os outros: o que
/// confere o download é o sha256 publicado, não o tamanho.
const SHA256_QWEN35_4B_GGUF: [(&str, &str); 4] = [
    ("Qwen3.5-4B-Q4_K_M.gguf", "00fe7986ff5f6b463e62455821146049db6f9313603938a70800d1fb69ef11a4"),
    ("Qwen3.5-4B-Q5_K_M.gguf", "8814232b85594dcd46c50e5b8b29324a7efe9e746edbe8a3d1df3d3fce7aad39"),
    ("Qwen3.5-4B-Q6_K.gguf", "fdedd781c9ce676ab66b018ca247ff78e8a33c98098a822c1e2d5075e7718f66"),
    ("Qwen3.5-4B-Q8_0.gguf", "10cc391b403021dd11c614679d2fd92f611c3681d29e29651b717316965d61e1"),
];

const SHA256_QWEN35_9B_GGUF: [(&str, &str); 3] = [
    ("Qwen3.5-9B-Q4_K_M.gguf", "03b74727a860a56338e042c4420bb3f04b2fec5734175f4cb9fa853daf52b7e8"),
    ("Qwen3.5-9B-Q5_K_M.gguf", "dc2a39aef291f91a9116ad214058da0d86eb648743a124bd8c333787c4b9c91c"),
    ("Qwen3.5-9B-Q6_K.gguf", "91898433cf5ce0a8f45516a4cc3e9343b6e01d052d01f684309098c66a326c59"),
];

/// Os pesos do 35B A3B, pelo mesmo `tree/main` com `?blobs=true`: o `oid` do
/// bloco LFS é o sha256 de 64 hex do arquivo inteiro, lido em 2026-11-24. As
/// três quants que já estavam aqui e as onze IQ/MXFP4 que o parser do candle
/// recusava — oferecer um arquivo desses era prometer um veredito de alojamento
/// que `fichar` não conseguia produzir. Conseguiu com o leitor próprio: os
/// blocos daqueles tipos estão medidos em `cabecalho::BLOCOS`, e a ficha sai.
const SHA256_QWEN35_35B_A3B_GGUF: [(&str, &str); 14] = [
    ("Qwen3.5-35B-A3B-Q4_K_M.gguf", "3b46d1066bc91cc2d613e3bc22ce691dd77e6f0d33c9060690d24ce6de494375"),
    ("Qwen3.5-35B-A3B-Q5_K_M.gguf", "93198787ca7744a813f437317ff086b9abeb3f6fabbd5fa5702aa893fa315d89"),
    ("Qwen3.5-35B-A3B-Q6_K.gguf", "56ddab06cc049b66fee8cff294c0b0e5e848f3381ed0da1a3a16a7bd80e8280a"),
    ("Qwen3.5-35B-A3B-MXFP4_MOE.gguf", "0f135a59159030f4710477abc6f9922d2f13552c85bff736deaaef71023cd770"),
    ("Qwen3.5-35B-A3B-Q3_K_M.gguf", "5607c8fcc8b04ada7d1a1152b9a5b6c1e67e6768232c16f6b03d9719d5ab1b2d"),
    ("Qwen3.5-35B-A3B-Q3_K_S.gguf", "315cce210c0406ae05c4e482ceba154c9225fa0bb4c9e779465209dec08e625d"),
    ("Qwen3.5-35B-A3B-UD-IQ2_M.gguf", "187b5bbb81d7ec1dc228cb04120302cc0d486cb3c988501af87e5f02e15ace0e"),
    ("Qwen3.5-35B-A3B-UD-IQ2_XXS.gguf", "2a809de317cfd49ac9130b95619ee6ac039a855e467015b3e996eb61af84718b"),
    ("Qwen3.5-35B-A3B-UD-IQ3_S.gguf", "ca6e9542e9f9e34f28d59e1e335b3629825daf30a4c0b5c1ec9d2f898c061fb6"),
    ("Qwen3.5-35B-A3B-UD-IQ3_XXS.gguf", "9f7f59280fcac9373f9122de0ed7a19e014fc14120a68b343ea46ac5f5dd96cd"),
    ("Qwen3.5-35B-A3B-UD-IQ4_NL.gguf", "6aec31ed654fbf12f9950c98bab0a09faea433ffdc4b3acfeb55a1f050e3e444"),
    ("Qwen3.5-35B-A3B-UD-IQ4_XS.gguf", "745def087c9ec34d12a94eb9b31eb52dba958b36def510b7e8f6f2e6bf1be123"),
    ("Qwen3.5-35B-A3B-UD-Q2_K_XL.gguf", "855b8d11f4b52e7ea4b9eeea2fbe3555c4d96d5b7b74702b6144810db3e77270"),
    ("Qwen3.5-35B-A3B-UD-Q3_K_XL.gguf", "fef37f41877128b0c82195c5cd173ca0e02394f48c7667dbf96172fa300fc945"),
];

const SHA256_GEMMA4_12B_GGUF: [(&str, &str); 2] = [
    ("gemma-4-12b-it-Q4_K_M.gguf", "0a270ec9fe6b34f4a0d33992b6135117b484ebc4766ab76b51d4ae8c457e4c42"),
    ("gemma-4-12b-it-Q6_K.gguf", "6f394336500bb5409bd68c31aa9d75097b5e6104f6cf30c82a09a7b64df41faf"),
];

/// Os pesos do codificador.
const SHA256_QWEN_EMBEDS_GGUF: [(&str, &str); 2] = [
    ("Qwen3-Embedding-0.6B-Q8_0.gguf", "06507c7b42688469c4e7298b0a1e16deff06caf291cf0a5b278c308249c3e439"),
    ("Qwen3-Embedding-0.6B-f16.gguf", "421a27e58d165478cc7acb984a688c2aa41404968b0203e7cd743ece44c54340"),
];

/// Os arquivos de apoio que têm soma publicada: cada `tokenizer.json` abaixo é
/// Git LFS, e o `lfs.oid` é o sha256 do conteúdo — conferido baixando o arquivo
/// e somando os bytes servidos (ver `docs/melhorias/M8.md`).
///
/// O `5f9e4d49…` aparece quatro vezes de propósito: é o tokenizador comum do
/// Ornith 1.5 9B, do Qwen3.5 4B, do 9B e do 35B A3B — os mesmos 12.807.982 bytes
/// nos quatro repositórios, lidos arquivo por arquivo. Não é cópia confiada: é a
/// mesma soma medida quatro vezes, em caminhos diferentes.
///
/// O tokenizador do Granite **não** está aqui de propósito: com 7.153.414 bytes
/// ele é um blob git comum, então a API devolve só `oid` — o SHA-1 de
/// `blob <tamanho>\0<conteúdo>`, que conferi byte a byte contra os bytes
/// servidos. Não existe sha256 publicado desse arquivo, e calcular um a partir
/// do que se acabou de baixar é fotografia, não verificação. Fica sem conferência
/// de conteúdo até a soma sair do repositório (pendência no doc). O
/// `tokenizer_config.json` de todos eles está na mesma situação: blob git, sem
/// sha256 publicado.
const SHA256_APOIO: [(&str, &[(&str, &str)]); 6] = [
    ("ornith-ai/Ornith-1.5-9B", &[("tokenizer.json", "5f9e4d4901a92b997e463c1f46055088b6cca5ca61a6522d1b9f64c4bb81cb42")]),
    ("Qwen/Qwen3.5-4B", &[("tokenizer.json", "5f9e4d4901a92b997e463c1f46055088b6cca5ca61a6522d1b9f64c4bb81cb42")]),
    ("Qwen/Qwen3.5-9B", &[("tokenizer.json", "5f9e4d4901a92b997e463c1f46055088b6cca5ca61a6522d1b9f64c4bb81cb42")]),
    ("Qwen/Qwen3.5-35B-A3B", &[("tokenizer.json", "5f9e4d4901a92b997e463c1f46055088b6cca5ca61a6522d1b9f64c4bb81cb42")]),
    ("google/gemma-4-12B-it", &[("tokenizer.json", "9788bdb6a021b3170357cd3587b5d622b1177561ad3c5bf43d73728fd71aeacb")]),
    ("Qwen/Qwen3-Embedding-0.6B", &[("tokenizer.json", "def76fb086971c7867b829c23a26261e38d9d74e02139253b38aeb9df8b4b50a")]),
];

/// Onde cada tabela de pesos mora, pelo nome do repositório — a mesma chave que
/// a URL de `resolve/main` usa, então uma linha trocada não confere com nada.
const SHA256_PESOS: [(&str, &[(&str, &str)]); 8] = [
    ("ibm-granite/granite-4.2-3b-GGUF", &SHA256_GRANITE_3B_GGUF),
    ("ibm-granite/granite-4.2-8b-GGUF", &SHA256_GRANITE_8B_GGUF),
    ("ornith-ai/Ornith-1.5-9B-GGUF", &SHA256_ORNITH_GGUF),
    ("unsloth/Qwen3.5-4B-GGUF", &SHA256_QWEN35_4B_GGUF),
    ("unsloth/Qwen3.5-9B-GGUF", &SHA256_QWEN35_9B_GGUF),
    ("unsloth/Qwen3.5-35B-A3B-GGUF", &SHA256_QWEN35_35B_A3B_GGUF),
    ("unsloth/gemma-4-12b-it-GGUF", &SHA256_GEMMA4_12B_GGUF),
    ("Qwen/Qwen3-Embedding-0.6B-GGUF", &SHA256_QWEN_EMBEDS_GGUF),
];

/// O sha256 que o HuggingFace publica para `(repositório, arquivo)`. `None` é a
/// resposta honesta: aquele arquivo não tem soma publicada, e conferir por um
/// hash inventado seria pior que não conferir.
pub fn sha256_publicado(repo: &str, arquivo: &str) -> Option<&'static str> {
    SHA256_PESOS
        .iter()
        .chain(SHA256_APOIO.iter())
        .find(|(r, _)| *r == repo)
        .and_then(|(_, tabela)| tabela.iter().find(|(a, _)| *a == arquivo))
        .map(|(_, soma)| *soma)
}

/// Os pesos deste modelo, pela pasta e pelo arquivo que o catálogo declara.
pub fn pesos_sha256(spec: &ModelSpec) -> Option<&'static str> {
    sha256_publicado(spec.weights_repo, spec.weights_file)
}

/// O hash publicado de um arquivo de apoio, se o repositório publica um.
pub fn apoio_sha256(spec: &ModelSpec, arquivo: &str) -> Option<&'static str> {
    sha256_publicado(spec.support_repo, arquivo)
}

/// O censo de tipos ggml de cada arquivo que o catálogo oferece: que tipos os
/// tensores daquele arquivo **são**, lidos tensor a tensor do cabeçalho real
/// (`Range: bytes=0-16777215` no `resolve/main`, em 2026-11-20 para os 46
/// arquivos que já estavam aqui e em 2026-11-24 para os do 35B A3B; o
/// instrumento, a tabela de blocos provada e a saída completa estão em
/// `docs/melhorias/CATALOGO.md`).
///
/// A tabela existe porque a etiqueta do nome não é um tipo, e o
/// `general.file_type` que o arquivo declara tampouco resolve: o
/// `granite-4.2-3b-Q3_K_S.gguf` diz `11` e tem `q3_k×281`, e o
/// `Qwen3.5-35B-A3B-Q3_K_S.gguf` diz o mesmo `11` sem ter um tensor `q3_k`
/// nenhum — são `iq3_xxs×80`, `iq3_s×40` e `q6_k×2`. O `MXFP4_MOE` declara `15`,
/// o mesmo número do `Q4_K_M` da série. O que o motor abre é o censo, e o que o
/// Aether mostra antes de rodar é a ficha que `fichar` tira dele: todo nome
/// escrito aqui tem linha medida em `cabecalho::BLOCOS`, e um nome sem linha é
/// recusa que nomeia o tensor e o número lido — o catálogo continua sem oferecer
/// o que não pode medir.
///
/// A chave é o nome do arquivo porque ele carrega o modelo dentro:
/// `granite-4.2-3b-Q4_K_M.gguf` não é o `granite-4.2-8b-Q4_K_M.gguf`.
const CENSO: [(&str, &[&str]); 60] = [
    ("granite-4.2-3b-bf16.gguf", &["bf16", "f32"]),
    ("granite-4.2-3b-Q2_K.gguf", &["f32", "q2_k", "q3_k", "q4_k", "q6_k"]),
    ("granite-4.2-3b-Q3_K_L.gguf", &["f32", "q3_k", "q5_k", "q6_k"]),
    ("granite-4.2-3b-Q3_K_M.gguf", &["f32", "q3_k", "q4_k", "q5_k", "q6_k"]),
    ("granite-4.2-3b-Q3_K_S.gguf", &["f32", "q3_k", "q6_k"]),
    ("granite-4.2-3b-Q4_0.gguf", &["f32", "q4_0", "q6_k"]),
    ("granite-4.2-3b-Q4_1.gguf", &["f32", "q4_1", "q6_k"]),
    ("granite-4.2-3b-Q4_K_M.gguf", &["f32", "q4_k", "q6_k"]),
    ("granite-4.2-3b-Q4_K_S.gguf", &["f32", "q4_k", "q5_k", "q6_k"]),
    ("granite-4.2-3b-Q5_0.gguf", &["f32", "q5_0", "q6_k"]),
    ("granite-4.2-3b-Q5_1.gguf", &["f32", "q5_1", "q6_k"]),
    ("granite-4.2-3b-Q5_K_M.gguf", &["f32", "q5_k", "q6_k"]),
    ("granite-4.2-3b-Q5_K_S.gguf", &["f32", "q5_k", "q6_k"]),
    ("granite-4.2-3b-Q6_K.gguf", &["f32", "q6_k"]),
    ("granite-4.2-3b-Q8_0.gguf", &["f32", "q8_0"]),
    ("granite-4.2-8b-bf16.gguf", &["bf16", "f32"]),
    ("granite-4.2-8b-Q2_K.gguf", &["f32", "q2_k", "q3_k", "q4_k", "q6_k"]),
    ("granite-4.2-8b-Q3_K_L.gguf", &["f32", "q3_k", "q5_k", "q6_k"]),
    ("granite-4.2-8b-Q3_K_M.gguf", &["f32", "q3_k", "q4_k", "q5_k", "q6_k"]),
    ("granite-4.2-8b-Q3_K_S.gguf", &["f32", "q3_k", "q6_k"]),
    ("granite-4.2-8b-Q4_0.gguf", &["f32", "q4_0", "q6_k"]),
    ("granite-4.2-8b-Q4_1.gguf", &["f32", "q4_1", "q6_k"]),
    ("granite-4.2-8b-Q4_K_M.gguf", &["f32", "q4_k", "q6_k"]),
    ("granite-4.2-8b-Q4_K_S.gguf", &["f32", "q4_k", "q5_k", "q6_k"]),
    ("granite-4.2-8b-Q5_0.gguf", &["f32", "q5_0", "q6_k"]),
    ("granite-4.2-8b-Q5_1.gguf", &["f32", "q5_1", "q6_k"]),
    ("granite-4.2-8b-Q5_K_M.gguf", &["f32", "q5_k", "q6_k"]),
    ("granite-4.2-8b-Q5_K_S.gguf", &["f32", "q5_k", "q6_k"]),
    ("granite-4.2-8b-Q6_K.gguf", &["f32", "q6_k"]),
    ("granite-4.2-8b-Q8_0.gguf", &["f32", "q8_0"]),
    ("Ornith-1.5-9B-BF16.gguf", &["bf16", "f32"]),
    ("Ornith-1.5-9B-Q4_K_M.gguf", &["f32", "q4_k", "q6_k"]),
    ("Ornith-1.5-9B-Q5_K_M.gguf", &["f32", "q5_k", "q6_k"]),
    ("Ornith-1.5-9B-Q6_K.gguf", &["f32", "q6_k"]),
    ("Ornith-1.5-9B-Q8_0.gguf", &["f32", "q8_0"]),
    ("Qwen3.5-4B-Q4_K_M.gguf", &["f32", "q4_k", "q5_k", "q6_k", "q8_0"]),
    ("Qwen3.5-4B-Q5_K_M.gguf", &["f32", "q5_k", "q6_k", "q8_0"]),
    ("Qwen3.5-4B-Q6_K.gguf", &["f32", "q6_k", "q8_0"]),
    ("Qwen3.5-4B-Q8_0.gguf", &["f32", "q8_0"]),
    ("Qwen3.5-9B-Q4_K_M.gguf", &["f32", "q4_k", "q5_k", "q6_k", "q8_0"]),
    ("Qwen3.5-9B-Q5_K_M.gguf", &["f32", "q5_k", "q6_k", "q8_0"]),
    ("Qwen3.5-9B-Q6_K.gguf", &["f32", "q6_k", "q8_0"]),
    ("gemma-4-12b-it-Q4_K_M.gguf", &["f32", "q4_k", "q6_k"]),
    ("gemma-4-12b-it-Q6_K.gguf", &["f32", "q6_k"]),
    ("Qwen3.5-35B-A3B-Q4_K_M.gguf", &["f32", "q4_k", "q5_k", "q6_k", "q8_0"]),
    ("Qwen3.5-35B-A3B-Q5_K_M.gguf", &["f32", "q5_k", "q6_k", "q8_0"]),
    ("Qwen3.5-35B-A3B-Q6_K.gguf", &["f32", "q6_k", "q8_0"]),
    ("Qwen3.5-35B-A3B-MXFP4_MOE.gguf", &["f32", "mxfp4", "q5_k", "q6_k", "q8_0"]),
    ("Qwen3.5-35B-A3B-Q3_K_M.gguf", &["f32", "iq3_xxs", "iq4_xs", "q6_k", "q8_0"]),
    ("Qwen3.5-35B-A3B-Q3_K_S.gguf", &["f32", "iq3_s", "iq3_xxs", "q6_k", "q8_0"]),
    ("Qwen3.5-35B-A3B-UD-IQ2_M.gguf", &["f32", "iq2_xxs", "iq3_xxs", "q4_k", "q5_k", "q6_k", "q8_0"]),
    ("Qwen3.5-35B-A3B-UD-IQ2_XXS.gguf", &["f32", "iq2_s", "iq2_xxs", "q4_k", "q5_k", "q6_k", "q8_0"]),
    ("Qwen3.5-35B-A3B-UD-IQ3_S.gguf", &["f32", "iq2_s", "iq3_s", "q6_k", "q8_0"]),
    ("Qwen3.5-35B-A3B-UD-IQ3_XXS.gguf", &["f32", "iq2_s", "iq3_xxs", "q6_k", "q8_0"]),
    ("Qwen3.5-35B-A3B-UD-IQ4_NL.gguf", &["f32", "iq3_s", "iq4_nl", "q6_k", "q8_0"]),
    ("Qwen3.5-35B-A3B-UD-IQ4_XS.gguf", &["f32", "iq3_s", "iq4_xs", "q6_k", "q8_0"]),
    ("Qwen3.5-35B-A3B-UD-Q2_K_XL.gguf", &["f32", "iq2_xs", "iq3_xxs", "iq4_xs", "q4_k", "q5_k", "q6_k", "q8_0"]),
    ("Qwen3.5-35B-A3B-UD-Q3_K_XL.gguf", &["f32", "iq3_xxs", "iq4_xs", "q5_k", "q6_k", "q8_0"]),
    ("Qwen3-Embedding-0.6B-f16.gguf", &["f16", "f32"]),
    ("Qwen3-Embedding-0.6B-Q8_0.gguf", &["f32", "q8_0"]),
];

/// Os tipos medidos de um arquivo, ou `None` para o arquivo que nunca foi lido:
/// estado que o teste do catálogo não deixa chegar a uma entrada oferecida.
pub fn censo_do_arquivo(arquivo: &str) -> Option<&'static [&'static str]> {
    CENSO.iter().find(|(a, _)| *a == arquivo).map(|(_, tipos)| *tipos)
}

/// A tabela de censo toda, para quem confere tipo a tipo. É o que o leitor de
/// cabeçalho usa para provar que todo nome que o catálogo escreve tem linha na
/// sua tabela de blocos medida — conferida na tabela, não numa lista copiada
/// dela, porque lista copiada é justamente o que deixou onze arquivos de fora.
pub fn censo_para_conferencia() -> Vec<(&'static str, &'static [&'static str])> {
    CENSO.iter().map(|(arquivo, tipos)| (*arquivo, *tipos)).collect()
}

pub const CATALOG: [ModelSpec; 58] = [
    granite!("3", Tier::Leve, 3.0, "Q2_K", 1_455_736_352, false),
    granite!("3", Tier::Leve, 3.0, "Q3_K_S", 1_677_207_072, false),
    granite!("3", Tier::Leve, 3.0, "Q3_K_M", 1_835_968_032, false),
    granite!("3", Tier::Leve, 3.0, "Q3_K_L", 1_974_576_672, false),
    granite!("3", Tier::Leve, 3.0, "Q4_0", 2_129_118_752, false),
    granite!("3", Tier::Leve, 3.0, "Q4_K_S", 2_142_881_312, false),
    granite!("3", Tier::Leve, 3.0, "Q4_K_M", 2_244_011_552, true),
    granite!("3", Tier::Leve, 3.0, "Q4_1", 2_341_783_072, false),
    granite!("3", Tier::Leve, 3.0, "Q5_0", 2_554_447_392, false),
    granite!("3", Tier::Leve, 3.0, "Q5_K_S", 2_554_447_392, false),
    granite!("3", Tier::Leve, 3.0, "Q5_K_M", 2_613_634_592, true),
    granite!("3", Tier::Leve, 3.0, "Q5_1", 2_767_111_712, false),
    granite!("3", Tier::Leve, 3.0, "Q6_K", 3_006_359_072, true),
    granite!("3", Tier::Leve, 3.0, "Q8_0", 3_892_651_552, true),
    granite!("3", Tier::Leve, 3.0, "bf16", 7_323_461_152, true),
    granite!("8", Tier::Pesado, 8.0, "Q2_K", 3_412_312_192, false),
    granite!("8", Tier::Pesado, 8.0, "Q3_K_S", 3_942_957_184, false),
    granite!("8", Tier::Pesado, 8.0, "Q3_K_M", 4_347_052_160, false),
    granite!("8", Tier::Pesado, 8.0, "Q3_K_L", 4_699_897_984, false),
    granite!("8", Tier::Pesado, 8.0, "Q4_0", 5_055_955_072, false),
    granite!("8", Tier::Pesado, 8.0, "Q4_K_S", 5_090_820_224, false),
    granite!("8", Tier::Pesado, 8.0, "Q4_K_M", 5_347_917_952, true),
    granite!("8", Tier::Pesado, 8.0, "Q4_1", 5_579_718_784, false),
    granite!("8", Tier::Pesado, 8.0, "Q5_0", 6_103_482_496, false),
    granite!("8", Tier::Pesado, 8.0, "Q5_K_S", 6_103_482_496, false),
    granite!("8", Tier::Pesado, 8.0, "Q5_K_M", 6_253_887_616, true),
    granite!("8", Tier::Pesado, 8.0, "Q5_1", 6_627_246_208, false),
    granite!("8", Tier::Pesado, 8.0, "Q6_K", 7_216_480_384, true),
    granite!("8", Tier::Pesado, 8.0, "Q8_0", 9_345_613_952, true),
    granite!("8", Tier::Pesado, 8.0, "bf16", 17_587_421_312, true),
    ornith!("Q4_K_M", 5_780_090_816, true),
    ornith!("Q5_K_M", 6_642_544_576, true),
    ornith!("Q6_K", 7_558_901_696, true),
    ornith!("Q8_0", 9_786_060_384, true),
    // 18 GB: baixável numa máquina com mais placa, e o roteador não a escolhe.
    ornith!("BF16", 18_407_321_184, false),
    qwen35!("4", Tier::Leve, 4.0, "Q4_K_M", 2_740_937_888, true),
    qwen35!("4", Tier::Leve, 4.0, "Q5_K_M", 3_143_656_608, true),
    qwen35!("4", Tier::Leve, 4.0, "Q6_K", 3_525_956_768, true),
    qwen35!("4", Tier::Leve, 4.0, "Q8_0", 4_482_403_488, true),
    qwen35!("9", Tier::Pesado, 9.0, "Q4_K_M", 5_680_522_464, true),
    qwen35!("9", Tier::Pesado, 9.0, "Q5_K_M", 6_577_841_376, true),
    qwen35!("9", Tier::Pesado, 9.0, "Q6_K", 7_458_301_152, true),
    // As quants do 35B A3B com censo lido tensor a tensor: catorze, de
    // 10.656.955.008 bytes (UD-IQ2_XXS) a 28.852.861.568 (Q6_K). Nenhuma é
    // escolhida pelo roteador, e o motivo mudou — não é mais o cabeçalho, porque
    // com o leitor próprio os onze IQ/MXFP4 têm ficha e veredito como as três de
    // sempre, e sim que nenhum arquivo da série rodou com a placa medida. A
    // pessoa baixa por conta própria e vê na tela o veredito que a conta dela
    // permite. As outras nove do repositório não têm censo lido e continuam
    // fora. A leitura arquivo por arquivo está em `docs/melhorias/CATALOGO.md`.
    qwen35moe!("Q4_K_M", 22_016_023_168, false),
    qwen35moe!("Q5_K_M", 26_249_607_808, false),
    qwen35moe!("Q6_K", 28_852_861_568, false),
    qwen35moe!("MXFP4_MOE", 21_587_638_912, false),
    qwen35moe!("Q3_K_M", 16_356_375_168, false),
    qwen35moe!("Q3_K_S", 15_265_856_128, false),
    qwen35moe!("UD-IQ2_M", 11_391_613_568, false),
    qwen35moe!("UD-IQ2_XXS", 10_656_955_008, false),
    qwen35moe!("UD-IQ3_S", 13_583_383_168, false),
    qwen35moe!("UD-IQ3_XXS", 13_080_066_688, false),
    qwen35moe!("UD-IQ4_NL", 17_821_719_168, false),
    qwen35moe!("UD-IQ4_XS", 17_486_174_848, false),
    qwen35moe!("UD-Q2_K_XL", 12_156_754_560, false),
    qwen35moe!("UD-Q3_K_XL", 16_601_176_704, false),
    gemma4!("Q4_K_M", 7_121_861_440, true),
    gemma4!("Q6_K", 9_786_022_720, true),
];

pub const EMBEDS: [ModelSpec; 2] = [
    ModelSpec {
        id: "qwen3-embedding-0.6b-q8_0",
        display: "Qwen3 Embedding 0.6B Q8_0",
        series: "Qwen3 Embedding 0.6B",
        family: "qwen3",
        tier: Tier::Leve,
        genero: Genero::Embeddings,
        parameters_b: 0.6,
        license: "Apache-2.0",
        quant: "Q8_0",
        dir: "qwen3-embedding-0.6b",
        weights_repo: "Qwen/Qwen3-Embedding-0.6B-GGUF",
        weights_file: "Qwen3-Embedding-0.6B-Q8_0.gguf",
        weights_bytes: 639_150_592,
        support_repo: "Qwen/Qwen3-Embedding-0.6B",
        support_files: &SUPPORT_EMBEDDINGS,
        query_prefix: Some(
            "Instruct: Given a question, retrieve passages that answer it\nQuery:",
        ),
        context_limit: 32_768,
        routable: false,
    },
    ModelSpec {
        id: "qwen3-embedding-0.6b-f16",
        display: "Qwen3 Embedding 0.6B F16",
        series: "Qwen3 Embedding 0.6B",
        family: "qwen3",
        tier: Tier::Leve,
        genero: Genero::Embeddings,
        parameters_b: 0.6,
        license: "Apache-2.0",
        quant: "f16",
        dir: "qwen3-embedding-0.6b",
        weights_repo: "Qwen/Qwen3-Embedding-0.6B-GGUF",
        weights_file: "Qwen3-Embedding-0.6B-f16.gguf",
        weights_bytes: 1_197_629_632,
        support_repo: "Qwen/Qwen3-Embedding-0.6B",
        support_files: &SUPPORT_EMBEDDINGS,
        query_prefix: Some(
            "Instruct: Given a question, retrieve passages that answer it\nQuery:",
        ),
        context_limit: 32_768,
        routable: false,
    },
];

pub fn find(id: &str) -> Option<&'static ModelSpec> {
    CATALOG
        .iter()
        .chain(EMBEDS.iter())
        .find(|m| m.id == id)
}

/// Os modelos que o roteador e a tela de conversa conhecem: os que falam.
pub fn todos_de_conversa() -> impl Iterator<Item = &'static ModelSpec> {
    CATALOG.iter().filter(|m| m.genero == Genero::Conversa)
}

/// Os que viram vetor.
pub fn todos_de_embeddings() -> impl Iterator<Item = &'static ModelSpec> {
    EMBEDS.iter().filter(|m| m.genero == Genero::Embeddings)
}

/// Um modelo de conversa pelo id. Um codificador pedido aqui é recusado: ele
/// está no catálogo, tem pesos em disco e não diz uma palavra — carregar um no
/// motor de conversa daria um assistente mudo.
pub fn find_conversa(id: &str) -> Option<&'static ModelSpec> {
    find(id).filter(|m| m.genero == Genero::Conversa)
}

pub fn find_embeddings(id: &str) -> Option<&'static ModelSpec> {
    find(id).filter(|m| m.genero == Genero::Embeddings)
}

/// O primeiro codificador completo em disco, na ordem do catálogo: é o que a
/// memória usa quando o dono não escolheu um.
pub fn embeddings_instalados(root: &Path) -> Option<&'static ModelSpec> {
    EMBEDS.iter().find(|spec| status(root, spec).complete)
}

pub fn weights_url(spec: &ModelSpec) -> String {
    format!(
        "https://huggingface.co/{}/resolve/main/{}",
        spec.weights_repo, spec.weights_file
    )
}

pub fn support_url(spec: &ModelSpec, file: &str) -> String {
    format!(
        "https://huggingface.co/{}/resolve/main/{file}",
        spec.support_repo
    )
}

/// O `/raw/main/` de um arquivo rastreado por Git LFS devolve o ponteiro, não os
/// bytes: 133 caracteres começando em `version https://git-lfs.github.com/`. O
/// tokenizador chega assim, o codificador não o abre, e o cartão dizia "Em uso"
/// porque existir era confundido com ter. Aqui o ponteiro não vale.
pub fn support_ok(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut arquivo) = std::fs::File::open(path) else {
        return false;
    };
    let mut cabeca = [0u8; 64];
    let lido = match arquivo.read(&mut cabeca) {
        Ok(n) => n,
        Err(_) => return false,
    };
    !cabeca[..lido].starts_with(b"version https://git-lfs.github.com")
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelStatus {
    pub id: String,
    pub display: String,
    pub series: String,
    pub tier: Tier,
    pub genero: Genero,
    pub parameters_b: f32,
    pub license: String,
    pub quant: String,
    pub routable: bool,
    pub context_limit: usize,
    pub weights_bytes: u64,
    pub downloaded_bytes: u64,
    /// Bytes de um download interrompido. O `.part` fica em disco para a próxima
    /// tentativa continuar, e a biblioteca precisa mostrá-lo.
    pub partial_bytes: u64,
    pub complete: bool,
    pub missing_support: Vec<String>,
    pub dir: String,
}

pub fn model_dir(root: &Path, spec: &ModelSpec) -> PathBuf {
    root.join(spec.dir)
}

/// Estado medido em disco: bytes presentes e arquivos de apoio ausentes.
pub fn status(root: &Path, spec: &ModelSpec) -> ModelStatus {
    let dir = model_dir(root, spec);
    let weights = dir.join(spec.weights_file);
    let downloaded_bytes = std::fs::metadata(&weights).map(|m| m.len()).unwrap_or(0);
    let partial_bytes = std::fs::metadata(dir.join(format!("{}.part", spec.weights_file)))
        .map(|m| m.len())
        .unwrap_or(0);
    let missing_support: Vec<String> = spec
        .support_files
        .iter()
        .filter(|f| !support_ok(&dir.join(f)))
        .map(|f| f.to_string())
        .collect();
    ModelStatus {
        id: spec.id.to_string(),
        display: spec.display.to_string(),
        series: spec.series.to_string(),
        tier: spec.tier,
        genero: spec.genero,
        parameters_b: spec.parameters_b,
        license: spec.license.to_string(),
        quant: spec.quant.to_string(),
        routable: spec.routable,
        context_limit: spec.context_limit,
        weights_bytes: spec.weights_bytes,
        downloaded_bytes,
        partial_bytes,
        complete: downloaded_bytes == spec.weights_bytes && missing_support.is_empty(),
        missing_support,
        dir: dir.display().to_string(),
    }
}

pub fn all_status(root: &Path) -> Vec<ModelStatus> {
    todos_de_conversa().map(|spec| status(root, spec)).collect()
}

/// O que a tela de memória oferece: as quants do codificador, com o estado
/// medido em disco.
pub fn status_embeddings(root: &Path) -> Vec<ModelStatus> {
    todos_de_embeddings().map(|spec| status(root, spec)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_ids_are_unique_and_findable() {
        for spec in CATALOG {
            assert_eq!(find(spec.id).unwrap().id, spec.id);
        }
        assert!(find("nao-existe").is_none());
    }

    /// O contrato do catálogo é o motor vendorizado, não a minha memória: cada
    /// família declarada tem de estar registrada na `llama.dll` que o aplicativo
    /// carrega. A tabela de arquiteturas do llama.cpp é lida como texto do
    /// binário, então o que se verifica aqui é o arquivo real. Sem as DLLs
    /// (clone recém-feito) não há o que conferir, e o teste diz isso em vez de
    /// passar fingindo.
    #[test]
    fn every_entry_is_an_architecture_the_engine_runs() {
        let dll = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("vendor")
            .join("llama-cpp")
            .join("lib")
            .join("llama.dll");
        let Ok(binario) = std::fs::read(&dll) else {
            eprintln!("{} ausente: nada contra o que conferir", dll.display());
            return;
        };
        let mut familias: Vec<&str> = CATALOG.iter().map(|m| m.family).collect();
        familias.extend(EMBEDS.iter().map(|m| m.family));
        for familia in familias.iter().collect::<Vec<_>>() {
            let chave = format!("{familia}\0");
            assert!(
                binario.windows(chave.len()).any(|janela| janela == chave.as_bytes()),
                "a família {familia} não está registrada na llama.dll vendorizada"
            );
        }
    }

    /// O censo, não a etiqueta. Todo arquivo oferecido tem de ter linha medida na
    /// tabela `CENSO` — um nome errado ali não confere com a URL nem com a soma
    /// publicada, então o erro aparece aqui em vez de aparecer no download — e
    /// cada tipo da linha tem de ter conta de bloco no leitor de cabeçalho. A
    /// pergunta é `cabecalho::numero_do_nome`, e não uma lista copiada da tabela
    /// dele: foi uma lista copiada — os 15 tipos que o parser do candle conhecia
    /// — que deixou onze quants IQ e MXFP4 do `Qwen3.5-35B-A3B` de fora enquanto
    /// o leitor já media os blocos delas. Esta conta também mostrou que o
    /// `Qwen3.5-35B-A3B-Q3_K_S.gguf` não tem tensor `q3_k` nenhum.
    #[test]
    fn censo_medido_de_todo_arquivo_oferecido_e_aberto_pelo_leitor() {
        let arquivos: Vec<&str> = CATALOG
            .iter()
            .chain(EMBEDS.iter())
            .map(|spec| spec.weights_file)
            .collect();
        let mut ordenados = arquivos.clone();
        ordenados.sort_unstable();
        let antes = ordenados.len();
        ordenados.dedup();
        assert_eq!(
            antes - ordenados.len(),
            0,
            "dois arquivos oferecidos têm o mesmo nome: a chave do censo deixaria de ser única"
        );
        for arquivo in &arquivos {
            let tipos = censo_do_arquivo(arquivo)
                .unwrap_or_else(|| panic!("{arquivo} não tem censo medido: o catálogo oferece o que não leu"));
            for tipo in tipos {
                assert!(
                    crate::ai::cabecalho::numero_do_nome(tipo).is_some(),
                    "{arquivo} carrega {tipo}, que não tem conta de bloco medida — sem ficha não há veredito a mostrar"
                );
            }
        }
    }

    /// O caminho de CPU é o candle, e ele só implementa granite: para as outras
    /// famílias a placa é obrigatória. Aqui se cobra a etiqueta que o motor de
    /// CPU escolhe; o censo dos arquivos IBM está todo dentro da lista de tipos
    /// acima, e é por isso que os dois testes não se sobrepõem.
    #[test]
    fn granites_sao_executaveis_pelo_caminho_de_cpu() {
        const DO_CANDLE: [&str; 15] = [
            "Q2_K", "Q3_K_S", "Q3_K_M", "Q3_K_L", "Q4_0", "Q4_K_S", "Q4_K_M", "Q4_1", "Q5_0",
            "Q5_K_S", "Q5_K_M", "Q5_1", "Q6_K", "Q8_0", "bf16",
        ];
        for spec in CATALOG.iter().filter(|s| s.family == "granite") {
            assert!(
                DO_CANDLE.contains(&spec.quant),
                "granite na quant {} não é executável pelo candle",
                spec.quant
            );
        }
    }

    /// Séries oferecidas só para baixar, sem que o roteador as escolha sozinhas. O
    /// `routable` pede duas coisas — qualidade suficiente e carga que caiba na
    /// conta da placa — e cada linha aqui diz qual das duas falta. A lista é a
    /// exceção escrita à mão, e o teste abaixo confere os dois lados: uma série
    /// nova sem quant escolhível precisa entrar aqui com o motivo, e uma série
    /// aqui listada que passou a ter quant escolhível tem de sair.
    const SO_PARA_BAIXAR: [(&str, &str); 1] = [(
        "Qwen3.5 35B A3B",
        "nenhum arquivo da série rodou com a placa medida: o peso e o veredito de alojamento saem da conta de qualquer quant, mas a qualidade e o laço de ferramenta dessa arquitetura ainda não têm corrida",
    )];

    /// Toda série do catálogo tem de ter pelo menos uma quant escolhível pelo
    /// roteador: uma série em que nenhuma linha passa o critério é um modelo que
    /// a pessoa vê na tela e o assistente nunca usa sozinho. A lista de séries é
    /// lida do catálogo, não escrita à mão — entra série nova, o teste cobra
    /// série nova.
    #[test]
    fn every_size_has_a_routable_baseline() {
        for id in [
            "granite-4.2-3b-Q4_K_M",
            "granite-4.2-8b-Q4_K_M",
            "ornith-1.5-9b-Q4_K_M",
            "qwen3.5-4b-Q4_K_M",
            "qwen3.5-9b-Q4_K_M",
            "gemma-4-12b-it-Q4_K_M",
        ] {
            assert!(find(id).unwrap().routable, "{id} não é escolhível");
        }
        let series: Vec<&str> = {
            let mut v: Vec<&str> = CATALOG.iter().map(|s| s.series).collect();
            v.sort_unstable();
            v.dedup();
            v
        };
        for serie in &series {
            if let Some((_, motivo)) = SO_PARA_BAIXAR.iter().find(|(s, _)| s == serie) {
                assert!(
                    !CATALOG.iter().any(|s| s.series == *serie && s.routable),
                    "a série {serie} está em SO_PARA_BAIXAR mas passou a ter quant escolhível: {motivo}"
                );
                continue;
            }
            assert!(
                CATALOG.iter().any(|s| s.series == *serie && s.routable),
                "a série {serie} não tem nenhuma quant escolhível — entre em SO_PARA_BAIXAR com o motivo, ou dê-lhe uma"
            );
        }
        for (serie, _) in SO_PARA_BAIXAR {
            assert!(
                series.contains(&serie),
                "SO_PARA_BAIXAR cita {serie}, que não é mais uma série do catálogo"
            );
        }
    }

    #[test]
    fn low_quants_are_downloadable_but_not_auto_chosen() {
        assert!(!find("granite-4.2-3b-Q2_K").unwrap().routable);
        assert!(find("granite-4.2-3b-Q8_0").unwrap().routable);
    }

    #[test]
    fn variants_of_one_size_share_a_folder() {
        let a = find("granite-4.2-3b-Q4_K_M").unwrap();
        let b = find("granite-4.2-3b-Q8_0").unwrap();
        assert_eq!(a.dir, b.dir);
        assert_ne!(a.weights_file, b.weights_file);
        let root = Path::new("modelos");
        assert_eq!(model_dir(root, a), model_dir(root, b));
    }

    #[test]
    fn urls_point_at_the_declared_repos() {
        let spec = find("granite-4.2-3b-Q4_K_M").unwrap();
        assert_eq!(
            weights_url(spec),
            "https://huggingface.co/ibm-granite/granite-4.2-3b-GGUF/resolve/main/granite-4.2-3b-Q4_K_M.gguf"
        );
        assert!(support_url(spec, "tokenizer.json").ends_with("/resolve/main/tokenizer.json"));
    }

    /// O ponteiro Git LFS tem de ser lido como arquivo ausente: foi assim que o
    /// codificador chegou à tela com "Em uso" e um tokenizador de 133 bytes que
    /// nenhuma biblioteca abre.
    #[test]
    fn a_git_lfs_pointer_is_not_a_support_file() {
        let dir = std::env::temp_dir().join(format!("aether-catalog-lfs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let spec = find("qwen3-embedding-0.6b-q8_0").unwrap();
        let sub = model_dir(&dir, spec);
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(
            sub.join("tokenizer.json"),
            b"version https://git-lfs.github.com/spec/v1\noid sha256:def76fb086971c7867b829c23a26261e38d9d74e02139253b38aeb9df8b4b50a\nsize 11423705\n",
        )
        .unwrap();
        std::fs::write(sub.join("tokenizer_config.json"), b"{\"model_type\": \"qwen2\"}").unwrap();
        let st = status(&dir, spec);
        assert_eq!(st.missing_support, vec!["tokenizer.json".to_string()]);
        assert!(!st.complete);

        std::fs::write(sub.join("tokenizer.json"), b"{\"version\": \"1.0\"}").unwrap();
        let st = status(&dir, spec);
        assert!(st.missing_support.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Todo peso do catálogo tem de ter soma publicada. Uma quant nova que
    /// entra sem linha na tabela volta a ser conferida só pelo tamanho — que é
    /// exatamente o defeito que o M8 fecha — e o teste pega isso antes de
    /// qualquer download. O formato é o do `lfs.oid`: 64 hex minúsculas.
    #[test]
    fn todo_peso_do_catalogo_tem_sha256_publicado() {
        for spec in CATALOG.iter().chain(EMBEDS.iter()) {
            let soma = pesos_sha256(spec);
            let soma = soma.unwrap_or_else(|| panic!("{}: nenhum sha256 publicado", spec.id));
            assert_eq!(soma.len(), 64, "{}: {soma}", spec.id);
            assert!(
                soma.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
                "{}: {soma} não é hex minúsculo",
                spec.id
            );
        }
    }

    /// Tabela e catálogo têm de ser a mesma lista dos dois lados: uma linha de
    /// hash sem arquivo correspondente é dado morto que ninguém mais confere, e
    /// foi ela que fez o `weights_bytes` mentir uma vez em outro projeto.
    #[test]
    fn nenhuma_soma_publicada_sobra_fora_do_catalogo() {
        let (pesos, _): (Vec<_>, Vec<_>) =
            SHA256_PESOS.iter().flat_map(|(_, t)| t.iter().cloned()).unzip();
        let declarados: Vec<&str> = CATALOG
            .iter()
            .chain(EMBEDS.iter())
            .map(|s| s.weights_file)
            .collect();
        for arquivo in &pesos {
            assert!(declarados.contains(arquivo), "{arquivo} não é peso de nenhum modelo");
        }
        assert_eq!(
            pesos.len(),
            declarados.len(),
            "a tabela de pesos e o catálogo diferem: {} linhas contra {} modelos",
            pesos.len(),
            declarados.len()
        );
        for (repo, tabela) in &SHA256_APOIO {
            for (arquivo, _) in tabela.iter() {
                let pedido = CATALOG
                    .iter()
                    .chain(EMBEDS.iter())
                    .any(|s| s.support_repo == *repo && s.support_files.contains(arquivo));
                assert!(pedido, "{repo}/{arquivo} não é pedido por nenhum modelo");
            }
        }
    }

    /// Os arquivos de apoio que o Granite pede não têm soma publicada — e isto
    /// é registro do que a API devolve, não desculpa: se um dia o repositório
    /// passar a publicar LFS nesses arquivos, a tabela tem de crescer e este
    /// teste avisa que a premissa do doc M8 mudou.
    #[test]
    fn apoio_de_granite_nao_tem_hash_publicado_e_apoio_lfs_tem() {
        let granite = find("granite-4.2-3b-Q4_K_M").unwrap();
        for arquivo in granite.support_files {
            assert_eq!(
                apoio_sha256(granite, arquivo),
                None,
                "granite/{arquivo} passou a ter soma publicada: atualizar a tabela e o doc"
            );
        }
        let codificador = find("qwen3-embedding-0.6b-q8_0").unwrap();
        assert_eq!(
            apoio_sha256(codificador, "tokenizer.json"),
            Some("def76fb086971c7867b829c23a26261e38d9d74e02139253b38aeb9df8b4b50a"),
            "o tokenizador LFS da Qwen tem de chegar com a soma lida do `lfs.oid`"
        );
        assert_eq!(apoio_sha256(codificador, "tokenizer_config.json"), None);
    }

    #[test]
    fn absent_model_reports_zero_bytes_and_missing_support() {
        let dir = std::env::temp_dir().join(format!("aether-catalog-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let spec = find("granite-4.2-3b-Q4_K_M").unwrap();
        let st = status(&dir, spec);
        assert_eq!(st.downloaded_bytes, 0);
        assert!(!st.complete);
        assert_eq!(st.missing_support.len(), SUPPORT_FILES.len());
    }

    #[test]
    fn partial_download_is_not_complete() {
        let dir = std::env::temp_dir().join(format!("aether-catalog-partial-{}", std::process::id()));
        let spec = find("granite-4.2-3b-Q4_K_M").unwrap();
        let sub = model_dir(&dir, spec);
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join(spec.weights_file), b"12345").unwrap();
        let st = status(&dir, spec);
        assert_eq!(st.downloaded_bytes, 5);
        assert!(!st.complete);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Duas quants na mesma pasta são medidas separadamente: ter uma não faz a
    /// outra parecer presente.
    #[test]
    fn two_quants_of_one_size_are_measured_independently() {
        let dir =
            std::env::temp_dir().join(format!("aether-catalog-quants-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let small = find("granite-4.2-3b-Q4_K_M").unwrap();
        let big = find("granite-4.2-3b-Q8_0").unwrap();
        let sub = model_dir(&dir, small);
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join(small.weights_file), vec![0u8; 16]).unwrap();
        assert_eq!(status(&dir, small).downloaded_bytes, 16);
        assert_eq!(status(&dir, big).downloaded_bytes, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Um download interrompido tem de aparecer na biblioteca, senão o usuário
    /// não sabe que há gigabytes em disco nem como continuar.
    #[test]
    fn an_interrupted_download_is_reported_as_partial_bytes() {
        let dir =
            std::env::temp_dir().join(format!("aether-catalog-part-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let spec = find("granite-4.2-3b-Q4_K_M").unwrap();
        let sub = model_dir(&dir, spec);
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(
            sub.join(format!("{}.part", spec.weights_file)),
            vec![0u8; 4096],
        )
        .unwrap();

        let st = status(&dir, spec);
        assert_eq!(st.downloaded_bytes, 0);
        assert_eq!(st.partial_bytes, 4096);
        assert!(!st.complete);
        let _ = std::fs::remove_dir_all(&dir);
    }
}