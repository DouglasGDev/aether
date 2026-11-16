//! Carregamento e geração, com streaming token a token.
//!
//! Dois backends, uma amostragem: a conta vai para a placa quando existe uma
//! (ggml/Vulkan, de qualquer fabricante) e para a CPU quando não existe nenhuma
//! (candle). Os dois entregam logits na mesma forma, e é um único caminho de
//! amostragem que os consome — sampler duplicado é como duas respostas passam a
//! divergir entre si.
//!
//! A inferência é bloqueante e pesada: quem chama precisa rodar isto fora do
//! runtime async (spawn_blocking ou thread dedicada).

use super::ggml;
use super::granite::{self, Config};
use crate::idiomas::tr;
use anyhow::{Context, Result, bail};
use candle::quantized::gguf_file;
use candle::{Device, Tensor};
use candle_transformers::generation::{LogitsProcessor, Sampling};
use candle_transformers::utils::apply_repeat_penalty;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use tokenizers::Tokenizer;

#[derive(Debug, Clone)]
pub struct SamplingParams {
    pub temperature: f64,
    pub top_k: Option<usize>,
    pub top_p: Option<f64>,
    pub repeat_penalty: f32,
    pub repeat_last_n: usize,
    pub seed: u64,
    pub max_tokens: usize,
}

impl Default for SamplingParams {
    fn default() -> Self {
        Self {
            temperature: 0.7,
            top_k: Some(40),
            top_p: Some(0.9),
            repeat_penalty: 1.1,
            repeat_last_n: 128,
            seed: 0x4145_5448_4552,
            max_tokens: 512,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StopReason {
    Eos,
    StopSequence,
    MaxTokens,
    Cancelled,
    ContextFull,
}

#[derive(Debug, Clone)]
pub struct Generation {
    pub text: String,
    pub prompt_tokens: usize,
    pub generated_tokens: usize,
    pub prompt_ms: u64,
    pub decode_ms: u64,
    pub stop: StopReason,
}

impl Generation {
    pub fn decode_tokens_per_second(&self) -> f64 {
        if self.decode_ms == 0 {
            return 0.0;
        }
        self.generated_tokens as f64 * 1000.0 / self.decode_ms as f64
    }

    pub fn prefill_tokens_per_second(&self) -> f64 {
        if self.prompt_ms == 0 {
            return 0.0;
        }
        self.prompt_tokens as f64 * 1000.0 / self.prompt_ms as f64
    }
}

/// O que o motor entrega a quem escuta a geração.
#[derive(Debug, Clone)]
pub enum Emission {
    /// Texto novo, para anexar ao que já foi mostrado.
    Delta(String),
    /// Uma sequência de parada foi transmitida pela metade: o texto acumulado
    /// passa a ser exatamente este, sem o trecho retractado.
    Replace(String),
}

/// Decodificação incremental que nunca emite um caractere UTF-8 pela metade.
/// Só a cauda com caractere de substituição fica pendurada; o resto sai inteiro.
/// Segurar espaço e pontuação e depois emitir apenas o delta seguinte descartava
/// o texto pendurado — foi assim que "arquitetura como" virou "arquiteturacomo".
struct TextStream {
    tokenizer: Tokenizer,
    tokens: Vec<u32>,
    committed: usize,
}

impl TextStream {
    fn new(tokenizer: Tokenizer) -> Self {
        Self { tokenizer, tokens: Vec::new(), committed: 0 }
    }

    fn decode(&self, tokens: &[u32]) -> Result<String> {
        self.tokenizer
            .decode(tokens, true)
            .map_err(|e| anyhow::anyhow!(tr("erros.falhaDecodificar", &[("detalhe", &e.to_string())])))
    }

    fn push(&mut self, token: u32) -> Result<Option<String>> {
        self.tokens.push(token);
        let text = self.decode(&self.tokens[self.committed..])?;
        if text.ends_with('\u{fffd}') {
            return Ok(None);
        }
        self.committed = self.tokens.len();
        if text.is_empty() { Ok(None) } else { Ok(Some(text)) }
    }

    fn flush(&mut self) -> Result<Option<String>> {
        let before = self.decode(&self.tokens[self.committed..])?;
        if before.is_empty() {
            return Ok(None);
        }
        self.committed = self.tokens.len();
        Ok(Some(before))
    }
}

/// Onde a conta acontece.
enum Motor {
    /// ggml/llama.cpp numa placa. Posições e KV cache são da biblioteca: ela
    /// mesma sabe onde termina a sequência.
    Gpu { motor: ggml::Motor },
    /// candle na CPU. As posições são contadas aqui, e é exatamente o que
    /// `granite::Layer::rope` espera: `narrow(index_pos..index_pos + n)`.
    Cpu { model: granite::Model, config: Config, pos: usize },
}

impl Motor {
    /// Logits da última posição de `tokens`, 1-D. Prefill e passo de decode
    /// entram pelo mesmo método porque o que muda entre eles é só o tamanho do
    /// lote.
    fn passo(&mut self, tokens: &[u32]) -> Result<Tensor> {
        match self {
            Motor::Gpu { motor } => {
                let logits = motor.passo(tokens)?;
                let n = logits.len();
                Ok(Tensor::from_vec(logits, (n,), &Device::Cpu)?)
            }
            Motor::Cpu { model, pos, .. } => {
                let input = Tensor::new(tokens, &Device::Cpu)?.reshape((1, tokens.len()))?;
                let logits = model.forward(&input, *pos)?.squeeze(0)?;
                *pos += tokens.len();
                Ok(logits)
            }
        }
    }

    /// Esquece a conversa: cache e posições voltam ao estado de modelo novo.
    fn limpar(&mut self) {
        match self {
            Motor::Gpu { motor } => motor.limpar(),
            Motor::Cpu { model, pos, .. } => {
                model.clear_kv_cache();
                *pos = 0;
            }
        }
    }

    /// A janela que o backend aceitou, que pode ser menor que a pedida.
    fn janela(&self) -> usize {
        match self {
            Motor::Gpu { motor } => motor.janela,
            Motor::Cpu { model, .. } => model.max_seq_len(),
        }
    }

    fn vocabulario(&self) -> usize {
        match self {
            Motor::Gpu { motor } => motor.vocabulario(),
            Motor::Cpu { config, .. } => config.vocab_size,
        }
    }
}

/// O que a interface mostra sobre onde o modelo está rodando. Nunca um vazio:
/// se a conta foi para a CPU, o motivo de a placa não ter servido vem junto.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Acelerador {
    /// `"gpu"` ou `"cpu"` — o que foi usado, não o que foi pedido.
    pub tipo: &'static str,
    /// Nome que a placa se deu no registro Vulkan; vazio na CPU.
    pub dispositivo: String,
    /// Por que a placa não serviu, quando ela não serviu.
    pub motivo: Option<String>,
    /// Onde os bytes ficaram, na placa que serviu. `None` só na CPU, onde não há
    /// placa para alojar nada.
    pub alojado: Option<Alojado>,
}

/// A previsão feita com a VRAM livre de antes da carga, e o que a carga de fato
/// tirou da placa. Os dois lados juntos são a única forma de a tela dizer "está
/// inteiro na placa" sem estar pedindo confiança.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Alojado {
    /// Veredito calculado a partir do cabeçalho do GGUF e da VRAM livre medida
    /// antes de `llama_load_model_from_file`.
    pub previsto: crate::ai::alojamento::Alojamento,
    /// Bytes de VRAM que sumiram do livre entre antes e depois da carga.
    pub cedido: u64,
}

pub struct Engine {
    pub model_id: String,
    motor: Motor,
    tokenizer: Tokenizer,
    eos_tokens: Vec<u32>,
    acelerador: Acelerador,
}

impl Engine {
    /// `dir` contém `tokenizer.json`, `generation_config.json` e o GGUF nomeado
    /// por `weights_file`. Os ids de EOS vêm do generation_config do próprio
    /// modelo — nunca de uma lista de nomes chutada.
    ///
    /// `dlls` são as pastas onde o aplicativo publica a ggml vendorizada. Sem
    /// elas não há caminho de GPU, e a regra do produto (placa primeiro) viraria
    /// "CPU porque não havia opção" — por isso o motivo da recusa da placa é
    /// guardado e devolvido, nunca descartado.
    pub fn load(
        model_id: &str,
        dir: &Path,
        weights_file: &str,
        context: usize,
        dlls: &[PathBuf],
    ) -> Result<Self> {
        let tokenizer_path = dir.join("tokenizer.json");
        let tokenizer = Tokenizer::from_file(&tokenizer_path).map_err(|e| {
            anyhow::anyhow!(tr(
                "erros.falhaAbrirTokenizador",
                &[("caminho", &tokenizer_path.display().to_string()), ("detalhe", &e.to_string())]
            ))
        })?;
        let eos_tokens = eos_from_generation_config(&dir.join("generation_config.json"))?;
        if eos_tokens.is_empty() {
            bail!(tr("erros.semEosDeclarado", &[]));
        }

        let weights = dir.join(weights_file);
        // A VRAM é lida antes da carga. Depois dela o livre já descontaria o
        // próprio modelo, e a conta deixaria de ser previsão para ser retrospecto.
        let capacidade = super::alojamento::capacidade(dlls).ok().flatten();
        let ficha = super::alojamento::fichar(&weights).ok();
        let livre_antes = capacidade.as_ref().map(|c| c.livre).unwrap_or(0);
        // O veredito antes da carga é o que manda a placa e é o que a tela mostra:
        // o mesmo número decide quantas camadas sobem e conta a história no chip.
        // Recalculá-lo depois com a janela que a biblioteca aceitou poderia dizer
        // "inteiro" para uma carga que foi comandada dividida.
        let previsto = ficha
            .as_ref()
            .zip(capacidade.as_ref())
            .map(|(ficha, capacidade)| {
                super::alojamento::alojar(ficha, Some(capacidade), context)
            });
        let despacho = previsto
            .as_ref()
            .map(super::alojamento::despacho)
            .unwrap_or(super::ggml::ffi::CAMADAS_NA_GPU);
        match ggml::Motor::novo(dlls, &weights, context, despacho) {
            Ok(motor) => {
                let alojado = previsto.map(|previsto| {
                    let livre_depois = super::alojamento::capacidade(dlls)
                        .ok()
                        .flatten()
                        .map(|c| c.livre)
                        .unwrap_or(livre_antes);
                    Alojado { previsto, cedido: livre_antes.saturating_sub(livre_depois) }
                });
                let acelerador = Acelerador {
                    tipo: "gpu",
                    dispositivo: motor.dispositivo.clone(),
                    motivo: None,
                    alojado,
                };
                Ok(Self {
                    model_id: model_id.to_string(),
                    tokenizer,
                    eos_tokens,
                    motor: Motor::Gpu { motor },
                    acelerador,
                })
            }
            Err(falha_gpu) => {
                // Fallback legítimo: nenhuma placa serviu. O que a placa respondeu
                // continua visível, junto com o erro da CPU se ela também falhar.
                let (model, config) = carregar_na_cpu(&weights, context)
                    .map_err(|falha_cpu| {
                        anyhow::anyhow!("{falha_gpu}\n{falha_cpu}")
                    })?;
                Ok(Self {
                    model_id: model_id.to_string(),
                    tokenizer,
                    eos_tokens,
                    motor: Motor::Cpu { model, config, pos: 0 },
                    acelerador: Acelerador {
                        tipo: "cpu",
                        dispositivo: String::new(),
                        motivo: Some(falha_gpu.to_string()),
                        alojado: None,
                    },
                })
            }
        }
    }

    pub fn tokenizer(&self) -> &Tokenizer {
        &self.tokenizer
    }

    pub fn acelerador(&self) -> &Acelerador {
        &self.acelerador
    }

    /// Põe `pedidos` sobre os pesos que estão em memória agora, sem recarregar
    /// nada, e devolve linha por linha o que aconteceu.
    ///
    /// O caminho de CPU recusa à vista. Não é preguiça de implementação: os
    /// pesos granite são lidos pelo candle e não há adaptador nele, e fingir que
    /// aplicou — ou pior, calar o motivo — deixaria a tela mostrar um ajuste
    /// fino ligado que não muda uma palavra da resposta.
    pub fn usar_adaptadores(&mut self, pedidos: &[(PathBuf, f32)]) -> Vec<ggml::Aplicado> {
        match &mut self.motor {
            Motor::Gpu { motor } => motor.usar_adaptadores(pedidos),
            Motor::Cpu { .. } => pedidos
                .iter()
                .map(|(caminho, _)| ggml::Aplicado {
                    caminho: caminho.clone(),
                    aplicada: false,
                    erro: Some(tr("erros.adaptadorSemCpu", &[])),
                })
                .collect(),
        }
    }

    /// A metadata granite lida pelo candle. `None` no caminho de GPU, onde quem
    /// lê o GGUF é a llama.cpp e estes escalares nunca passam por nós.
    pub fn config(&self) -> Option<&Config> {
        match &self.motor {
            Motor::Gpu { .. } => None,
            Motor::Cpu { config, .. } => Some(config),
        }
    }

    /// Quantos ids cabem na conversa, ditos pelo backend — não os que pedimos.
    pub fn context(&self) -> usize {
        self.motor.janela()
    }

    pub fn vocabulario(&self) -> usize {
        self.motor.vocabulario()
    }

    pub fn eos_tokens(&self) -> &[u32] {
        &self.eos_tokens
    }

    pub fn count_tokens(&self, text: &str) -> Result<usize> {
        Ok(self
            .tokenizer
            .encode(text, false)
            .map_err(|e| anyhow::anyhow!(tr("erros.tokenizar", &[("detalhe", &e.to_string())])))?
            .len())
    }

    pub fn reset(&mut self) {
        self.motor.limpar();
    }

    /// Gera a continuação de `prompt_tokens`, emitindo texto por `emit`.
    pub fn generate(
        &mut self,
        prompt_tokens: &[u32],
        params: &SamplingParams,
        stop_sequences: &[String],
        cancel: &AtomicBool,
        mut emit: impl FnMut(Emission),
    ) -> Result<Generation> {
        if prompt_tokens.is_empty() {
            bail!(tr("erros.nadaParaInferir", &[]));
        }
        let janela = self.context();
        if prompt_tokens.len() >= janela {
            bail!(tr(
                "erros.promptForaDaJanela",
                &[
                    ("tokens", &prompt_tokens.len().to_string()),
                    ("janela", &janela.to_string())
                ]
            ));
        }
        self.motor.limpar();

        let sampling = match (params.top_k, params.top_p) {
            (Some(k), Some(p)) => Sampling::TopKThenTopP { k, p, temperature: params.temperature },
            (Some(k), None) => Sampling::TopK { k, temperature: params.temperature },
            (None, Some(p)) => Sampling::TopP { p, temperature: params.temperature },
            (None, None) => Sampling::All { temperature: params.temperature },
        };
        let sampling = if params.temperature < 1e-7 { Sampling::ArgMax } else { sampling };
        let mut logits_processor = LogitsProcessor::from_sampling(params.seed, sampling);

        let mut all_tokens = prompt_tokens.to_vec();
        let mut stream = TextStream::new(self.tokenizer.clone());
        let mut text = String::new();

        let prefill_start = Instant::now();
        let logits = self.motor.passo(prompt_tokens)?;
        let prompt_ms = prefill_start.elapsed().as_millis() as u64;

        let mut next = logits_processor.sample(&logits)?;
        let decode_start = Instant::now();
        let mut generated = 0usize;
        let budget = params.max_tokens.min(janela - prompt_tokens.len());

        let stop = loop {
            if cancel.load(Ordering::Relaxed) {
                break StopReason::Cancelled;
            }
            if self.eos_tokens.contains(&next) {
                break StopReason::Eos;
            }
            if generated >= budget {
                break if generated >= params.max_tokens {
                    StopReason::MaxTokens
                } else {
                    StopReason::ContextFull
                };
            }

            all_tokens.push(next);
            generated += 1;
            if let Some(piece) = stream.push(next)? {
                text.push_str(&piece);
                emit(Emission::Delta(piece));
            }
            if let Some(hit) = stop_sequences
                .iter()
                .find(|s| !s.is_empty() && text.contains(s.as_str()))
            {
                let cut = text.find(hit.as_str()).expect("parada recém-encontrada");
                text.truncate(cut);
                emit(Emission::Replace(text.clone()));
                break StopReason::StopSequence;
            }

            let logits = self.motor.passo(&[next])?;
            let logits = if params.repeat_penalty == 1.0 {
                logits
            } else {
                let start = all_tokens.len().saturating_sub(params.repeat_last_n);
                apply_repeat_penalty(&logits, params.repeat_penalty, &all_tokens[start..])?
            };
            next = logits_processor.sample(&logits)?;
        };

        if stop != StopReason::StopSequence {
            if let Some(rest) = stream.flush()? {
                text.push_str(&rest);
                emit(Emission::Delta(rest));
            }
        }
        let decode_ms = decode_start.elapsed().as_millis() as u64;

        Ok(Generation {
            text,
            prompt_tokens: prompt_tokens.len(),
            generated_tokens: generated,
            prompt_ms,
            decode_ms,
            stop,
        })
    }
}

/// O caminho de CPU: candle sobre o granite nativo, para máquinas sem placa
/// nenhuma. É também onde a arquitetura continua restrita a granite, porque é
/// o que este motor implementa.
fn carregar_na_cpu(weights: &Path, context: usize) -> Result<(granite::Model, Config)> {
    let mut file = std::fs::File::open(weights).map_err(|e| {
        anyhow::anyhow!(tr(
            "erros.falhaAbrir",
            &[("caminho", &weights.display().to_string()), ("detalhe", &e.to_string())]
        ))
    })?;
    let content = gguf_file::Content::read(&mut file).with_context(|| {
        tr("erros.ggufInvalido", &[("caminho", &weights.display().to_string())])
    })?;
    let arch = content
        .metadata
        .get("general.architecture")
        .and_then(|v| v.to_string().ok())
        .cloned()
        .unwrap_or_default();
    if arch != "granite" {
        bail!(tr("erros.arquiteturaNaoGranite", &[("arquitetura", &arch)]));
    }
    let model = granite::Model::from_gguf(content, &mut file, &Device::Cpu, context)
        .context(tr("erros.falhaCarregarTensores", &[]))?;
    let config = model.config.clone();
    Ok((model, config))
}

/// EOS declarados no generation_config.json do modelo. Aceita id único ou lista,
/// como o Transformers faz. Sem chute: arquivo ausente ou sem o campo é erro.
fn eos_from_generation_config(path: &Path) -> Result<Vec<u32>> {
    let detalhe = path.display().to_string();
    let raw = std::fs::read_to_string(path).map_err(|e| {
        anyhow::anyhow!(tr("erros.falhaLer", &[("caminho", &detalhe), ("detalhe", &e.to_string())]))
    })?;
    let json: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| {
            anyhow::anyhow!(tr(
                "erros.jsonInvalidoEm",
                &[("caminho", &detalhe), ("detalhe", &e.to_string())]
            ))
        })?;
    let field = json
        .get("eos_token_id")
        .with_context(|| tr("erros.semCampoEos", &[("caminho", &detalhe)]))?;
    let mut ids = Vec::new();
    match field {
        serde_json::Value::Number(n) => {
            if let Some(id) = n.as_u64() {
                ids.push(id as u32);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                if let Some(id) = item.as_u64() {
                    ids.push(id as u32);
                }
            }
        }
        other => bail!(tr(
            "erros.eosFormatoInesperado",
            &[("valor", &other.to_string())]
        )),
    }
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WordLevel + Fuse concatena os pedaços sem separador, como o BPE do
    /// Granite, sem depender do arquivo do modelo.
    fn stream_tokenizer() -> Tokenizer {
        let json = r#"{
            "version": "1.0",
            "truncation": null,
            "padding": null,
            "added_tokens": [],
            "normalizer": null,
            "pre_tokenizer": null,
            "post_processor": null,
            "decoder": { "type": "Fuse" },
            "model": {
                "type": "WordLevel",
                "vocab": {
                    "capital": 0, " ": 1, "Aether": 2, ".": 3,
                    "é": 4, "\ufffd": 5, "x": 6
                },
                "unk_token": "[UNK]"
            }
        }"#;
        Tokenizer::from_bytes(json.as_bytes()).expect("tokenizer de teste")
    }

    #[test]
    fn stream_keeps_spaces_and_punctuation() {
        let mut stream = TextStream::new(stream_tokenizer());
        let mut out = String::new();
        for token in [0u32, 1, 2, 3, 1, 4] {
            if let Some(piece) = stream.push(token).unwrap() {
                out.push_str(&piece);
            }
        }
        if let Some(rest) = stream.flush().unwrap() {
            out.push_str(&rest);
        }
        assert_eq!(out, "capital Aether. é");
    }

    #[test]
    fn stream_holds_back_incomplete_character() {
        let mut stream = TextStream::new(stream_tokenizer());
        assert!(
            stream.push(5).unwrap().is_none(),
            "caractere parcial não pode sair"
        );
        let mut out = stream.push(6).unwrap().unwrap_or_default();
        if let Some(rest) = stream.flush().unwrap() {
            out.push_str(&rest);
        }
        assert_eq!(out, "\u{fffd}x");
    }

    #[test]
    fn default_sampling_is_bounded() {
        let p = SamplingParams::default();
        assert!(p.temperature > 0.0);
        assert!(p.max_tokens > 0);
        assert!(p.repeat_last_n > 0);
    }

    #[test]
    fn throughput_is_zero_without_measurement() {
        let g = Generation {
            text: String::new(),
            prompt_tokens: 10,
            generated_tokens: 0,
            prompt_ms: 0,
            decode_ms: 0,
            stop: StopReason::Cancelled,
        };
        assert_eq!(g.decode_tokens_per_second(), 0.0);
        assert_eq!(g.prefill_tokens_per_second(), 0.0);
    }

    #[test]
    fn throughput_uses_measured_milliseconds() {
        let g = Generation {
            text: "x".into(),
            prompt_tokens: 100,
            generated_tokens: 50,
            prompt_ms: 500,
            decode_ms: 2000,
            stop: StopReason::Eos,
        };
        assert_eq!(g.decode_tokens_per_second(), 25.0);
        assert_eq!(g.prefill_tokens_per_second(), 200.0);
    }

    #[test]
    fn cpu_so_existe_quando_a_placa_recusou() {
        // O motivo da recusa é o que a interface mostra: sem ele, "CPU" vira um
        // fato sem explicação e ninguém descobre que a placa estava lá.
        let a = Acelerador {
            tipo: "cpu",
            dispositivo: String::new(),
            motivo: Some("nenhuma GPU".to_string()),
            alojado: None,
        };
        let json = serde_json::to_string(&a).unwrap();
        assert!(json.contains("\"tipo\":\"cpu\""));
        assert!(json.contains("nenhuma GPU"));
        assert!(json.contains("\"dispositivo\":\"\""));
    }

    #[test]
    fn gpu_mostra_a_placa_e_nenhum_motivo() {
        let a = Acelerador {
            tipo: "gpu",
            dispositivo: "AMD Radeon RX 9060 XT".to_string(),
            motivo: None,
            alojado: None,
        };
        let json = serde_json::to_string(&a).unwrap();
        assert!(json.contains("RX 9060 XT"));
        // A chave vem sempre no despacho — `types.ts` declara `motivo: string |
        // null`. Na placa ela é `null`, e é isso que a interface lê para não
        // desenhar explicação onde não houve recusa.
        assert!(json.contains("\"motivo\":null"));
        assert!(json.contains("\"alojado\":null"));
    }

    #[test]
    fn alojado_dividido_diz_as_camadas_e_os_dois_lados() {
        // O card da placa lê exatamente estas chaves. Se o enum mudasse de forma
        // sem a interface mudar junto, o turno continuaria rodando e a tela
        // mostraria "na placa" para um modelo metade em RAM.
        use super::super::alojamento::Alojamento;
        let a = Acelerador {
            tipo: "gpu",
            dispositivo: "AMD Radeon RX 9060 XT".to_string(),
            motivo: None,
            alojado: Some(Alojado {
                previsto: Alojamento::Dividida {
                    camadas: 21,
                    total: 40,
                    na_placa: 1000,
                    em_ram: 1136,
                    livre: 6000,
                },
                cedido: 1900,
            }),
        };
        let json = serde_json::to_string(&a).unwrap();
        assert!(json.contains("\"tipo\":\"gpu\""), "{json}");
        assert!(json.contains("\"alojado\":{"), "{json}");
        assert!(json.contains("\"cedido\":1900"), "{json}");
        assert!(json.contains("\"tipo\":\"dividida\""), "{json}");
        assert!(json.contains("\"camadas\":21"), "{json}");
        assert!(json.contains("\"emRam\":1136"), "{json}");
    }
}
