//! FFI do sherpa-onnx vendorizado.
//!
//! Cada `#[repr(C)]` abaixo repete, campo por campo e na mesma ordem, o
//! `vendor/sherpa-onnx/include/sherpa-onnx/c-api/c-api.h` da versão 1.13.8. A
//! biblioteca é aberta em tempo de execução: se a DLL não estiver ao lado do
//! executável, a voz fica indisponível e o resto do aplicativo segue funcionando
//! — o processo não pode morrer por causa de um recurso de fala.

use crate::idiomas::tr;
use libloading::Library;
use std::ffi::{c_char, c_float, c_int, c_void};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// TTS
// ---------------------------------------------------------------------------

#[repr(C)]
pub struct Vits {
    pub model: *const c_char,
    pub lexicon: *const c_char,
    pub tokens: *const c_char,
    pub data_dir: *const c_char,
    pub noise_scale: c_float,
    pub noise_scale_w: c_float,
    pub length_scale: c_float,
    pub dict_dir: *const c_char,
}

#[repr(C)]
pub struct Matcha {
    pub acoustic_model: *const c_char,
    pub vocoder: *const c_char,
    pub lexicon: *const c_char,
    pub tokens: *const c_char,
    pub data_dir: *const c_char,
    pub noise_scale: c_float,
    pub length_scale: c_float,
    pub dict_dir: *const c_char,
}

#[repr(C)]
pub struct Kokoro {
    pub model: *const c_char,
    pub voices: *const c_char,
    pub tokens: *const c_char,
    pub data_dir: *const c_char,
    pub length_scale: c_float,
    pub dict_dir: *const c_char,
    pub lexicon: *const c_char,
    pub lang: *const c_char,
}

#[repr(C)]
pub struct Kitten {
    pub model: *const c_char,
    pub voices: *const c_char,
    pub tokens: *const c_char,
    pub data_dir: *const c_char,
    pub length_scale: c_float,
}

#[repr(C)]
pub struct Zipvoice {
    pub tokens: *const c_char,
    pub encoder: *const c_char,
    pub decoder: *const c_char,
    pub vocoder: *const c_char,
    pub data_dir: *const c_char,
    pub lexicon: *const c_char,
    pub feat_scale: c_float,
    pub t_shift: c_float,
    pub target_rms: c_float,
    pub guidance_scale: c_float,
}

#[repr(C)]
pub struct Pocket {
    pub lm_flow: *const c_char,
    pub lm_main: *const c_char,
    pub encoder: *const c_char,
    pub decoder: *const c_char,
    pub text_conditioner: *const c_char,
    pub vocab_json: *const c_char,
    pub token_scores_json: *const c_char,
    pub voice_embedding_cache_capacity: c_int,
}

#[repr(C)]
pub struct Supertonic {
    pub duration_predictor: *const c_char,
    pub text_encoder: *const c_char,
    pub vector_estimator: *const c_char,
    pub vocoder: *const c_char,
    pub tts_json: *const c_char,
    pub unicode_indexer: *const c_char,
    pub voice_style: *const c_char,
}

#[repr(C)]
pub struct ConfigModeloTts {
    pub vits: Vits,
    pub num_threads: c_int,
    pub debug: c_int,
    pub provider: *const c_char,
    pub matcha: Matcha,
    pub kokoro: Kokoro,
    pub kitten: Kitten,
    pub zipvoice: Zipvoice,
    pub pocket: Pocket,
    pub supertonic: Supertonic,
}

#[repr(C)]
pub struct ConfigTts {
    pub model: ConfigModeloTts,
    pub rule_fsts: *const c_char,
    pub max_num_sentences: c_int,
    pub rule_fars: *const c_char,
    pub silence_scale: c_float,
}

#[repr(C)]
pub struct ConfigGeracao {
    pub silence_scale: c_float,
    pub speed: c_float,
    pub sid: c_int,
    pub reference_audio: *const c_float,
    pub reference_audio_len: c_int,
    pub reference_sample_rate: c_int,
    pub reference_text: *const c_char,
    pub num_steps: c_int,
    pub extra: *const c_char,
}

#[repr(C)]
pub struct AudioGerado {
    pub samples: *const c_float,
    pub n: c_int,
    pub sample_rate: c_int,
}

/// Recebe os amostras de cada sentença conforme elas são sintetizadas. Devolve
/// zero para interromper a geração.
pub type CallbackAudio =
    extern "C" fn(samples: *const c_float, n: c_int, progress: c_float, user: *mut c_void) -> c_int;

// ---------------------------------------------------------------------------
// ASR (Whisper)
// ---------------------------------------------------------------------------

#[repr(C)]
pub struct ConfigFeature {
    pub sample_rate: c_int,
    pub feature_dim: c_int,
}

#[repr(C)]
pub struct Transducer {
    pub encoder: *const c_char,
    pub decoder: *const c_char,
    pub joiner: *const c_char,
}

#[repr(C)]
pub struct Paraformer {
    pub model: *const c_char,
}

#[repr(C)]
pub struct NemoCtc {
    pub model: *const c_char,
}

#[repr(C)]
pub struct Whisper {
    pub encoder: *const c_char,
    pub decoder: *const c_char,
    pub language: *const c_char,
    pub task: *const c_char,
    pub tail_paddings: c_int,
    pub enable_token_timestamps: c_int,
    pub enable_segment_timestamps: c_int,
}

#[repr(C)]
pub struct Tdnn {
    pub model: *const c_char,
}

#[repr(C)]
pub struct SenseVoice {
    pub model: *const c_char,
    pub language: *const c_char,
    pub use_itn: c_int,
}

#[repr(C)]
pub struct Moonshine {
    pub preprocessor: *const c_char,
    pub encoder: *const c_char,
    pub uncached_decoder: *const c_char,
    pub cached_decoder: *const c_char,
    pub merged_decoder: *const c_char,
}

#[repr(C)]
pub struct FireRedAsr {
    pub encoder: *const c_char,
    pub decoder: *const c_char,
}

#[repr(C)]
pub struct Dolphin {
    pub model: *const c_char,
}

#[repr(C)]
pub struct ZipformerCtc {
    pub model: *const c_char,
}

#[repr(C)]
pub struct Canary {
    pub encoder: *const c_char,
    pub decoder: *const c_char,
    pub src_lang: *const c_char,
    pub tgt_lang: *const c_char,
    pub use_pnc: c_int,
}

#[repr(C)]
pub struct WenetCtc {
    pub model: *const c_char,
}

#[repr(C)]
pub struct Omnilingual {
    pub model: *const c_char,
}

#[repr(C)]
pub struct MedAsr {
    pub model: *const c_char,
}

#[repr(C)]
pub struct FunAsrNano {
    pub encoder_adaptor: *const c_char,
    pub llm: *const c_char,
    pub embedding: *const c_char,
    pub tokenizer: *const c_char,
    pub system_prompt: *const c_char,
    pub user_prompt: *const c_char,
    pub max_new_tokens: c_int,
    pub temperature: c_float,
    pub top_p: c_float,
    pub seed: c_int,
    pub language: *const c_char,
    pub itn: c_int,
    pub hotwords: *const c_char,
}

#[repr(C)]
pub struct FireRedAsrCtc {
    pub model: *const c_char,
}

#[repr(C)]
pub struct Qwen3Asr {
    pub conv_frontend: *const c_char,
    pub encoder: *const c_char,
    pub decoder: *const c_char,
    pub tokenizer: *const c_char,
    pub max_total_len: c_int,
    pub max_new_tokens: c_int,
    pub temperature: c_float,
    pub top_p: c_float,
    pub seed: c_int,
    pub hotwords: *const c_char,
}

#[repr(C)]
pub struct CohereTranscribe {
    pub encoder: *const c_char,
    pub decoder: *const c_char,
    pub language: *const c_char,
    pub use_punct: c_int,
    pub use_itn: c_int,
}

#[repr(C)]
pub struct ConfigModeloAsr {
    pub transducer: Transducer,
    pub paraformer: Paraformer,
    pub nemo_ctc: NemoCtc,
    pub whisper: Whisper,
    pub tdnn: Tdnn,
    pub tokens: *const c_char,
    pub num_threads: c_int,
    pub debug: c_int,
    pub provider: *const c_char,
    pub model_type: *const c_char,
    pub modeling_unit: *const c_char,
    pub bpe_vocab: *const c_char,
    pub telespeech_ctc: *const c_char,
    pub sense_voice: SenseVoice,
    pub moonshine: Moonshine,
    pub fire_red_asr: FireRedAsr,
    pub dolphin: Dolphin,
    pub zipformer_ctc: ZipformerCtc,
    pub canary: Canary,
    pub wenet_ctc: WenetCtc,
    pub omnilingual: Omnilingual,
    pub medasr: MedAsr,
    pub funasr_nano: FunAsrNano,
    pub fire_red_asr_ctc: FireRedAsrCtc,
    pub qwen3_asr: Qwen3Asr,
    pub cohere_transcribe: CohereTranscribe,
}

#[repr(C)]
pub struct ConfigLm {
    pub model: *const c_char,
    pub scale: c_float,
}

#[repr(C)]
pub struct Homofonos {
    pub dict_dir: *const c_char,
    pub lexicon: *const c_char,
    pub rule_fsts: *const c_char,
}

#[repr(C)]
pub struct ConfigReconhecimento {
    pub feat_config: ConfigFeature,
    pub model_config: ConfigModeloAsr,
    pub lm_config: ConfigLm,
    pub decoding_method: *const c_char,
    pub max_active_paths: c_int,
    pub hotwords_file: *const c_char,
    pub hotwords_score: c_float,
    pub rule_fsts: *const c_char,
    pub rule_fars: *const c_char,
    pub blank_penalty: c_float,
    pub hr: Homofonos,
}

#[repr(C)]
pub struct ResultadoReconhecimento {
    pub text: *const c_char,
    pub timestamps: *mut c_float,
    pub count: c_int,
    pub tokens: *const c_char,
    pub tokens_arr: *const *const c_char,
    pub json: *const c_char,
    pub lang: *const c_char,
    pub emotion: *const c_char,
    pub event: *const c_char,
    pub durations: *mut c_float,
    pub ys_log_probs: *mut c_float,
    pub segment_timestamps: *const c_float,
    pub segment_durations: *const c_float,
    pub segment_texts: *const c_char,
    pub segment_texts_arr: *const *const c_char,
    pub segment_count: c_int,
}

// ---------------------------------------------------------------------------

/// Os ponteiros que a DLL exporta. Carregar é barato comparado a abrir um modelo.
pub struct Sherpa {
    _biblioteca: Library,
    criar_tts: unsafe extern "C" fn(*const ConfigTts) -> *const c_void,
    destruir_tts: unsafe extern "C" fn(*const c_void),
    taxa_tts: unsafe extern "C" fn(*const c_void) -> c_int,
    vozes_tts: unsafe extern "C" fn(*const c_void) -> c_int,
    gerar: unsafe extern "C" fn(
        *const c_void,
        *const c_char,
        *const ConfigGeracao,
        Option<CallbackAudio>,
        *mut c_void,
    ) -> *const AudioGerado,
    destruir_audio: unsafe extern "C" fn(*const AudioGerado),
    escrever_wav:
        unsafe extern "C" fn(*const c_float, c_int, c_int, *const c_char) -> c_int,
    criar_recon: unsafe extern "C" fn(*const ConfigReconhecimento) -> *const c_void,
    destruir_recon: unsafe extern "C" fn(*const c_void),
    criar_fluxo: unsafe extern "C" fn(*const c_void) -> *const c_void,
    destruir_fluxo: unsafe extern "C" fn(*const c_void),
    aceitar: unsafe extern "C" fn(*const c_void, c_int, *const c_float, c_int),
    decodificar: unsafe extern "C" fn(*const c_void, *const c_void),
    resultado: unsafe extern "C" fn(*const c_void) -> *const ResultadoReconhecimento,
    destruir_resultado: unsafe extern "C" fn(*const ResultadoReconhecimento),
}

/// Um ponteiro opaco do sherpa. Copiar não multiplica o dono: quem decide
/// quando destruir é a thread que abriu o modelo.
#[derive(Clone, Copy)]
pub struct Opaco(pub *const c_void);
unsafe impl Send for Opaco {}

/// Abre a DLL com a pasta dela no começo da busca de dependências. O
/// `sherpa-onnx-c-api` importa o `onnxruntime`; quando o runtime está nos
/// recursos do pacote, e não ao lado do executável, é a pasta da própria DLL
/// que tem de ser procurada — o caminho é sempre absoluto, vindo de `abrir`.
/// A pasta padrão do sistema entra junto: sem ela o `onnxruntime` não acha a
/// runtime C do Visual Studio, e a flag sozinha nem é aceita pelo carregador.
#[cfg(windows)]
fn abrir_biblioteca(caminho: &Path) -> Result<Library, libloading::Error> {
    use libloading::os::windows::{
        Library as Nativa, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS as CARGA_DO_SISTEMA,
        LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR as CARGA_DA_PASTA,
    };
    // Inseguro pelo mesmo motivo de `Library::new`: o código da DLL passa a
    // rodar aqui dentro; o caminho é um dos que o aplicativo publica.
    unsafe { Nativa::load_with_flags(caminho, CARGA_DA_PASTA | CARGA_DO_SISTEMA) }.map(Library::from)
}

#[cfg(not(windows))]
fn abrir_biblioteca(caminho: &Path) -> Result<Library, libloading::Error> {
    unsafe { Library::new(caminho) }
}

fn simbolo<T: Copy>(lib: &Library, nome: &str) -> Result<T, String> {
    // O ponteiro continua válido enquanto a Library viver, e ela vive enquanto
    // este struct viver.
    unsafe { lib.get::<T>(nome) }
        .map(|f| *f)
        .map_err(|e| {
            tr(
                "erros.vozSemSimbolo",
                &[("simbolo", nome), ("detalhe", &e.to_string())],
            )
        })
}

impl Sherpa {
    /// Abre a primeira DLL que existir em `pastas`, na ordem dada.
    pub fn abrir(pastas: &[PathBuf]) -> Result<(Sherpa, PathBuf), String> {
        let mut tentadas = Vec::new();
        for pasta in pastas {
            let caminho = pasta.join("sherpa-onnx-c-api.dll");
            if !caminho.is_file() {
                tentadas.push(caminho.display().to_string());
                continue;
            }
            // Carregar uma DLL é inseguro porque o código dela passa a rodar
            // dentro do processo: o caminho é sempre um dos que o próprio
            // aplicativo publica (ao lado do executável ou nos recursos do
            // pacote), nunca um texto que chegou da interface.
            let biblioteca = abrir_biblioteca(&caminho).map_err(|e| {
                tr(
                    "erros.vozDllNaoAbriu",
                    &[
                        ("caminho", &caminho.display().to_string()),
                        ("detalhe", &e.to_string()),
                    ],
                )
            })?;
            let sherpa = Sherpa {
                criar_tts: simbolo(&biblioteca, "SherpaOnnxCreateOfflineTts")?,
                destruir_tts: simbolo(&biblioteca, "SherpaOnnxDestroyOfflineTts")?,
                taxa_tts: simbolo(&biblioteca, "SherpaOnnxOfflineTtsSampleRate")?,
                vozes_tts: simbolo(&biblioteca, "SherpaOnnxOfflineTtsNumSpeakers")?,
                gerar: simbolo(&biblioteca, "SherpaOnnxOfflineTtsGenerateWithConfig")?,
                destruir_audio: simbolo(&biblioteca, "SherpaOnnxDestroyOfflineTtsGeneratedAudio")?,
                escrever_wav: simbolo(&biblioteca, "SherpaOnnxWriteWave")?,
                criar_recon: simbolo(&biblioteca, "SherpaOnnxCreateOfflineRecognizer")?,
                destruir_recon: simbolo(&biblioteca, "SherpaOnnxDestroyOfflineRecognizer")?,
                criar_fluxo: simbolo(&biblioteca, "SherpaOnnxCreateOfflineStream")?,
                destruir_fluxo: simbolo(&biblioteca, "SherpaOnnxDestroyOfflineStream")?,
                aceitar: simbolo(&biblioteca, "SherpaOnnxAcceptWaveformOffline")?,
                decodificar: simbolo(&biblioteca, "SherpaOnnxDecodeOfflineStream")?,
                resultado: simbolo(&biblioteca, "SherpaOnnxGetOfflineStreamResult")?,
                destruir_resultado: simbolo(&biblioteca, "SherpaOnnxDestroyOfflineRecognizerResult")?,
                _biblioteca: biblioteca,
            };
            return Ok((sherpa, caminho));
        }
        Err(tr(
            "erros.vozDllNaoEncontrada",
            &[("caminhos", &tentadas.join(", "))],
        ))
    }

    pub fn novo_tts(&self, config: &ConfigTts) -> Option<Opaco> {
        let tts = unsafe { (self.criar_tts)(config) };
        (!tts.is_null()).then_some(Opaco(tts))
    }

    pub fn destruir_tts(&self, tts: Opaco) {
        unsafe { (self.destruir_tts)(tts.0) }
    }

    pub fn taxa_de_amostragem(&self, tts: Opaco) -> i32 {
        unsafe { (self.taxa_tts)(tts.0) }
    }

    pub fn quantas_vozes(&self, tts: Opaco) -> i32 {
        unsafe { (self.vozes_tts)(tts.0) }
    }

    /// Sintetiza `texto` (UTF-8) entregando cada bloco ao callback. Devolve falso
    /// quando o sherpa não produziu nada.
    ///
    /// # Segurança
    /// Invariante do chamador: `texto` e `config` seguem vivos durante a chamada,
    /// e `usuario` só pode apontar para memória válida enquanto o sherpa puder
    /// chamar o callback — ou seja, até esta função retornar.
    pub unsafe fn falar(
        &self,
        tts: Opaco,
        texto: &std::ffi::CStr,
        config: &ConfigGeracao,
        callback: Option<CallbackAudio>,
        usuario: *mut c_void,
    ) -> bool {
        let audio =
            unsafe { (self.gerar)(tts.0, texto.as_ptr(), config, callback, usuario) };
        if audio.is_null() {
            return false;
        }
        unsafe { (self.destruir_audio)(audio) };
        true
    }

    /// Escreve as amostras como WAV; devolve zero quando falha.
    pub fn escrever_wav(&self, amostras: &[f32], taxa: i32, caminho: &Path) -> i32 {
        let nome = std::ffi::CString::new(caminho.as_os_str().to_string_lossy().into_owned())
            .unwrap_or_default();
        unsafe { (self.escrever_wav)(amostras.as_ptr(), amostras.len() as c_int, taxa, nome.as_ptr()) }
    }

    pub fn novo_reconhecedor(&self, config: &ConfigReconhecimento) -> Option<Opaco> {
        let r = unsafe { (self.criar_recon)(config) };
        (!r.is_null()).then_some(Opaco(r))
    }

    pub fn destruir_reconhecedor(&self, r: Opaco) {
        unsafe { (self.destruir_recon)(r.0) }
    }

    /// Alimente `taxa` com a taxa de amostragem real das amostras: o sherpa
    /// reamostra para o que o modelo espera.
    ///
    /// # Segurança
    /// `amostras` precisa apontar para `amostras.len()` floats válidos durante a
    /// chamada, e o fluxo precisa ser destruído pelo chamador.
    pub unsafe fn transcrever(
        &self,
        recon: Opaco,
        taxa: i32,
        amostras: &[f32],
    ) -> Option<String> {
        let fluxo = unsafe { (self.criar_fluxo)(recon.0) };
        if fluxo.is_null() {
            return None;
        }
        unsafe {
            (self.aceitar)(fluxo, taxa, amostras.as_ptr(), amostras.len() as c_int);
            (self.decodificar)(recon.0, fluxo);
        }
        let resultado = unsafe { (self.resultado)(fluxo) };
        let texto = if resultado.is_null() {
            None
        } else {
            let t = unsafe { (*resultado).text };
            (!t.is_null()).then(|| unsafe { std::ffi::CStr::from_ptr(t) }.to_string_lossy().into_owned())
        };
        if !resultado.is_null() {
            unsafe { (self.destruir_resultado)(resultado) };
        }
        unsafe { (self.destruir_fluxo)(fluxo) };
        texto
    }
}
