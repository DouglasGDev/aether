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

/// Abaixo de Q4_K_M a perda de qualidade é visível: o usuário pode baixar, mas o
/// roteador não escolhe sozinho.
macro_rules! variant {
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

pub const CATALOG: [ModelSpec; 30] = [
    variant!("3", Tier::Leve, 3.0, "Q2_K", 1_455_736_352, false),
    variant!("3", Tier::Leve, 3.0, "Q3_K_S", 1_677_207_072, false),
    variant!("3", Tier::Leve, 3.0, "Q3_K_M", 1_835_968_032, false),
    variant!("3", Tier::Leve, 3.0, "Q3_K_L", 1_974_576_672, false),
    variant!("3", Tier::Leve, 3.0, "Q4_0", 2_129_118_752, false),
    variant!("3", Tier::Leve, 3.0, "Q4_K_S", 2_142_881_312, false),
    variant!("3", Tier::Leve, 3.0, "Q4_K_M", 2_244_011_552, true),
    variant!("3", Tier::Leve, 3.0, "Q4_1", 2_341_783_072, false),
    variant!("3", Tier::Leve, 3.0, "Q5_0", 2_554_447_392, false),
    variant!("3", Tier::Leve, 3.0, "Q5_K_S", 2_554_447_392, false),
    variant!("3", Tier::Leve, 3.0, "Q5_K_M", 2_613_634_592, true),
    variant!("3", Tier::Leve, 3.0, "Q5_1", 2_767_111_712, false),
    variant!("3", Tier::Leve, 3.0, "Q6_K", 3_006_359_072, true),
    variant!("3", Tier::Leve, 3.0, "Q8_0", 3_892_651_552, true),
    variant!("3", Tier::Leve, 3.0, "bf16", 7_323_461_152, true),
    variant!("8", Tier::Pesado, 8.0, "Q2_K", 3_412_312_192, false),
    variant!("8", Tier::Pesado, 8.0, "Q3_K_S", 3_942_957_184, false),
    variant!("8", Tier::Pesado, 8.0, "Q3_K_M", 4_347_052_160, false),
    variant!("8", Tier::Pesado, 8.0, "Q3_K_L", 4_699_897_984, false),
    variant!("8", Tier::Pesado, 8.0, "Q4_0", 5_055_955_072, false),
    variant!("8", Tier::Pesado, 8.0, "Q4_K_S", 5_090_820_224, false),
    variant!("8", Tier::Pesado, 8.0, "Q4_K_M", 5_347_917_952, true),
    variant!("8", Tier::Pesado, 8.0, "Q4_1", 5_579_718_784, false),
    variant!("8", Tier::Pesado, 8.0, "Q5_0", 6_103_482_496, false),
    variant!("8", Tier::Pesado, 8.0, "Q5_K_S", 6_103_482_496, false),
    variant!("8", Tier::Pesado, 8.0, "Q5_K_M", 6_253_887_616, true),
    variant!("8", Tier::Pesado, 8.0, "Q5_1", 6_627_246_208, false),
    variant!("8", Tier::Pesado, 8.0, "Q6_K", 7_216_480_384, true),
    variant!("8", Tier::Pesado, 8.0, "Q8_0", 9_345_613_952, true),
    variant!("8", Tier::Pesado, 8.0, "bf16", 17_587_421_312, true),
];

/// Os modelos que viram vetor, não texto. É a memória do Aether: as pastas
/// concedidas são lidas por semelhança de sentido, e para isso precisa de um
/// codificador. Escolhido por três razões medidas no próprio repositório: é
/// multilíngue (o usuário fala português), é Apache-2.0 como o resto do
/// aplicativo, e os 600 milhões de parâmetros cabem na placa sem tirar o modelo
/// de conversa de lá.
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

    #[test]
    fn every_entry_is_an_architecture_the_engine_runs() {
        for spec in CATALOG {
            assert_eq!(spec.family, "granite");
        }
    }

    /// candle-core implementa `from_data` para cada um destes em CPU; uma quant
    /// fora da lista seria um download que o motor não abre.
    #[test]
    fn quants_are_all_implemented_by_candle_on_cpu() {
        const IMPLEMENTED: [&str; 15] = [
            "Q2_K", "Q3_K_S", "Q3_K_M", "Q3_K_L", "Q4_0", "Q4_K_S", "Q4_K_M", "Q4_1", "Q5_0",
            "Q5_K_S", "Q5_K_M", "Q5_1", "Q6_K", "Q8_0", "bf16",
        ];
        for spec in CATALOG {
            assert!(
                IMPLEMENTED.contains(&spec.quant),
                "{} não é executável pelo motor",
                spec.quant
            );
        }
    }

    #[test]
    fn every_size_has_a_routable_baseline() {
        for size in ["3", "8"] {
            let id = format!("granite-4.2-{size}b-Q4_K_M");
            assert!(find(&id).unwrap().routable);
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