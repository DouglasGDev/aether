//! FFI do motor llama.cpp (ggml) vendorizado.
//!
//! Cada `#[repr(C)]` e cada assinatura abaixo repetem, campo por campo e na
//! mesma ordem, os cabeçalhos de `vendor/llama-cpp/include/` da revisão
//! `b12387`. A ordem dos campos é a única fonte de verdade: um campo a mais ou
//! a menos desloca tudo o que vem depois, e o resultado é uma leitura de memória
//! alheia, não um erro de compilação.
//!
//! Os símbolos não estão todos na mesma biblioteca, e isso foi medido com
//! `GetProcAddress` sobre as DLLs vendorizadas, não por suposição:
//! - `llama.dll` exporta tudo o que é `llama_*`;
//! - `ggml.dll` exporta o registro (`ggml_backend_dev_count`, `_get`);
//! - `ggml-base.dll` exporta os consultores de device (`_name`, `_type`, `_memory`).
//!
//! Como em `voz/ffi.rs`, as bibliotecas são abertas em tempo de execução: se as
//! DLLs não estiverem ao lado do executável, o motor de GPU simplesmente não
//! existe e o aplicativo segue com o motor de CPU.

use crate::idiomas::tr;
use libloading::Library;
use std::ffi::{c_char, c_int, c_void};
use std::path::{Path, PathBuf};

pub type Token = c_int;
pub type DeviceBruto = *mut c_void;

/// `enum ggml_backend_dev_type`.
pub const TIPO_CPU: c_int = 0;
pub const TIPO_GPU: c_int = 1;
pub const TIPO_IGPU: c_int = 2;
pub const TIPO_ACCEL: c_int = 3;

/// `LLAMA_SPLIT_MODE_NONE`: um dispositivo só. As outras opções espalham o
/// modelo entre várias GPUs, e espalhar está fora de propósito — a placa é
/// escolhida de propósito, uma, e o que não couber nela fica na CPU por conta
/// da própria ggml.
const DIVISAO_NENHUMA: c_int = 0;

/// `LLAMA_FLASH_ATTN_TYPE_AUTO`: deixa com a biblioteca a decisão por
/// arquitetura. É o padrão do cabeçalho, escrito aqui para o contraste com o
/// caminho de conversa ficar evidente.
const ATENCAO_AUTO: c_int = -1;

/// `LLAMA_FLASH_ATTN_TYPE_ENABLED`. Sem ela a matriz de atenção `n × n` é
/// materializada na placa, e é esse quadrado que faz uma janela de 8192 custar
/// muito mais que a conta em si.
const ATENCAO_RAPIDA: c_int = 1;

/// O sentinela do llama.cpp para "todas as camadas": negativo não é "quantas
/// couberem" — a biblioteca manda todas e a conta de caber é de quem escolhe.
/// É por isso que o caminho do LLM passa um número medido (`alojamento::despacho`)
/// e só usa esta constante quando não há ficha lida — sem medida, a escolha é a
/// de sempre, não um número chutado. O embutidor, dezenas de megabytes, vai
/// inteiro.
pub(crate) const CAMADAS_NA_GPU: c_int = -1;

/// `enum llama_pooling_type`. `UNSPECIFIED` é o que o `llama_context_default_params`
/// devolve, e é dele que o agrupamento vem: o GGUF declara no próprio metadado se
/// o vetor do texto inteiro sai da média, do `[CLS]` ou do último token. Escolher
/// aqui por fora seria escolher contra o modelo.
pub const AGRUPAMENTO_INDEFINIDO: c_int = -1;
pub const AGRUPAMENTO_NENHUM: c_int = 0;
pub const AGRUPAMENTO_MEDIA: c_int = 1;
pub const AGRUPAMENTO_CLS: c_int = 2;
pub const AGRUPAMENTO_ULTIMO: c_int = 3;

/// `struct llama_batch`. Os ponteiros apontam memória de quem chama, por isso
/// este struct só vive dentro de uma chamada — nunca é guardado entre elas.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Lote {
    pub n_tokens: c_int,
    pub token: *const Token,
    pub embd: *const f32,
    pub pos: *const c_int,
    pub n_seq_id: *const c_int,
    pub seq_id: *const *const c_int,
    pub logits: *const i8,
}

/// `struct llama_model_params`.
#[repr(C)]
pub struct ParamsModelo {
    /// Lista terminada por NULL dos dispositivos a usar; um elemento é o que
    /// mantém o modelo inteiro numa única placa.
    pub dispositivos: *mut DeviceBruto,
    pub sobreposicoes_buft: *const c_void,
    pub camadas_gpu: c_int,
    pub divisao: c_int,
    pub modo_carga: c_int,
    pub modo_preguicoso: c_int,
    pub gpu_principal: c_int,
    pub divisao_tensor: *const f32,
    pub progresso: Option<extern "C" fn(f32, *mut c_void) -> bool>,
    pub progresso_usuario: *mut c_void,
    pub sobreposicoes_meta: *const c_void,
    pub so_vocabulario: bool,
    pub conferir_pesos: bool,
    pub bufts_extra: bool,
    pub sem_host: bool,
    pub sem_alocar: bool,
    pub carregar_mtp: bool,
}

/// `struct llama_context_params`.
#[repr(C)]
#[allow(non_snake_case)]
pub struct ParamsContexto {
    pub n_ctx: u32,
    pub n_batch: u32,
    pub n_ubatch: u32,
    pub n_seq_max: u32,
    pub n_rs_seq: u32,
    pub n_outputs_max: u32,
    pub n_outputs_max_per_seq: u32,
    pub n_threads: c_int,
    pub n_threads_batch: c_int,
    pub ctx_type: c_int,
    pub rope_scaling_type: c_int,
    pub pooling_type: c_int,
    pub attention_type: c_int,
    pub flash_attn_type: c_int,
    pub rope_freq_base: f32,
    pub rope_freq_scale: f32,
    pub yarn_ext_factor: f32,
    pub yarn_attn_factor: f32,
    pub yarn_beta_fast: f32,
    pub yarn_beta_slow: f32,
    pub yarn_orig_ctx: u32,
    pub cb_eval: Option<unsafe extern "C" fn(*mut c_void, bool, *mut c_void) -> bool>,
    pub cb_eval_usuario: *mut c_void,
    pub tipo_k: c_int,
    pub tipo_v: c_int,
    pub cache_moe: usize,
    pub cache_moe_cpu: f32,
    pub abort_callback: Option<extern "C" fn(*mut c_void) -> bool>,
    pub abort_callback_usuario: *mut c_void,
    pub embeddings: bool,
    pub offload_kqv: bool,
    pub no_perf: bool,
    pub op_offload: bool,
    pub swa_full: bool,
    pub kv_unified: bool,
    pub samplers: *mut c_void,
    pub n_samplers: usize,
    pub ctx_other: *mut c_void,
}

/// O que a ggml informou sobre um device, já como dados do aplicativo.
#[derive(Debug, Clone)]
pub struct InfoDevice {
    pub indice: usize,
    pub bruto: DeviceBruto,
    pub nome: String,
    pub tipo: c_int,
    pub livre: usize,
    pub total: usize,
}

/// As três bibliotecas abertas e os ponteiros que elas exportam.
pub struct Ggml {
    _llama: Library,
    _registro: Library,
    _base: Library,
    versao: unsafe extern "C" fn() -> *const c_char,
    init_backend: unsafe extern "C" fn(),
    params_modelo: unsafe extern "C" fn() -> ParamsModelo,
    params_contexto: unsafe extern "C" fn() -> ParamsContexto,
    carregar_modelo: unsafe extern "C" fn(*const c_char, ParamsModelo) -> *mut c_void,
    liberar_modelo: unsafe extern "C" fn(*mut c_void),
    criar_contexto: unsafe extern "C" fn(*mut c_void, ParamsContexto) -> *mut c_void,
    liberar_contexto: unsafe extern "C" fn(*mut c_void),
    ler_n_ctx: unsafe extern "C" fn(*const c_void) -> u32,
    ler_n_batch: unsafe extern "C" fn(*const c_void) -> u32,
    vocabulario_do_modelo: unsafe extern "C" fn(*const c_void) -> *const c_void,
    tokens_do_vocabulario: unsafe extern "C" fn(*const c_void) -> c_int,
    embd_saida: unsafe extern "C" fn(*const c_void) -> c_int,
    tem_codificador: unsafe extern "C" fn(*const c_void) -> bool,
    tem_decodificador: unsafe extern "C" fn(*const c_void) -> bool,
    ler_agrupamento: unsafe extern "C" fn(*const c_void) -> c_int,
    memoria: unsafe extern "C" fn(*const c_void) -> *mut c_void,
    memoria_remover: unsafe extern "C" fn(*mut c_void, c_int, c_int, c_int) -> bool,
    lote_de_um: unsafe extern "C" fn(*mut Token, c_int) -> Lote,
    decodificar: unsafe extern "C" fn(*mut c_void, Lote) -> c_int,
    codificar: unsafe extern "C" fn(*mut c_void, Lote) -> c_int,
    logits: unsafe extern "C" fn(*mut c_void) -> *mut f32,
    embeddings_da_sequencia: unsafe extern "C" fn(*mut c_void, c_int) -> *mut f32,
    embeddings_do_token: unsafe extern "C" fn(*mut c_void, c_int) -> *mut f32,
    sincronizar: unsafe extern "C" fn(*mut c_void),
    /// `llama_adapter_lora_init`: carrega um adaptador **contra o modelo vivo**.
    /// Não existe adaptador solto nesta biblioteca — o GGUF do LoRA só declara
    /// deltas das tensors do base, e é o base que diz o shape delas.
    adapter_init: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void,
    adapter_free: unsafe extern "C" fn(*mut c_void),
    /// `llama_set_adapters_lora`: aplica o conjunto inteiro de uma vez, com a
    /// força de cada um. Uma só chamada porque meio conjunto aplicado é um
    /// modelo que ninguém pediu.
    set_adapters: unsafe extern "C" fn(*mut c_void, *mut *mut c_void, usize, *mut f32) -> c_int,
    quantos_devices: unsafe extern "C" fn() -> usize,
    device: unsafe extern "C" fn(usize) -> DeviceBruto,
    nome_device: unsafe extern "C" fn(DeviceBruto) -> *const c_char,
    tipo_device: unsafe extern "C" fn(DeviceBruto) -> c_int,
    memoria_device: unsafe extern "C" fn(DeviceBruto, *mut usize, *mut usize),
}

// Os ponteiros de função e os handles de módulo valem enquanto o processo
// viver; nenhuma das funções abaixo depende de estado por thread.
unsafe impl Send for Ggml {}
unsafe impl Sync for Ggml {}

/// Texto UTF-8 vindo de um `const char *` da biblioteca; NULL vira texto vazio.
fn texto(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // A CStr é emprestada: o dono é a ggml, que mantém esses nomes vivos pelo
    // resto do processo.
    unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// Abre uma DLL vendorizada com a pasta dela no começo da busca. As dependências
/// (`llama.dll` → `ggml.dll` → `ggml-base.dll`, e os backends `ggml-cpu-*` e
/// `ggml-vulkan` que a `ggml.dll` carrega depois) só são encontradas se a pasta
/// delas participar da procura; a pasta padrão do sistema entra junto porque o
/// carregador do Windows exige isso quando se pede a busca alterada.
#[cfg(windows)]
fn abrir_biblioteca(caminho: &Path) -> Result<Library, libloading::Error> {
    use libloading::os::windows::{
        Library as Nativa, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS as CARGA_DO_SISTEMA,
        LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR as CARGA_DA_PASTA,
    };
    unsafe { Nativa::load_with_flags(caminho, CARGA_DA_PASTA | CARGA_DO_SISTEMA) }.map(Library::from)
}

#[cfg(not(windows))]
fn abrir_biblioteca(caminho: &Path) -> Result<Library, libloading::Error> {
    unsafe { Library::new(caminho) }
}

fn simbolo<T: Copy>(lib: &Library, nome: &str) -> Result<T, String> {
    unsafe { lib.get::<T>(nome) }
        .map(|f| *f)
        .map_err(|e| tr("erros.motorSemSimbolo", &[("simbolo", nome), ("detalhe", &e.to_string())]))
}

impl Ggml {
    /// Abre as três bibliotecas da primeira pasta em `pastas` que as tenha.
    /// Devolve também a pasta, porque é dela que a `ggml.dll` carrega os
    /// próprios backends.
    pub fn abrir(pastas: &[PathBuf]) -> Result<(Ggml, PathBuf), String> {
        let mut tentadas = Vec::new();
        for pasta in pastas {
            let nomes = ["llama.dll", "ggml.dll", "ggml-base.dll"];
            let caminhos: Vec<PathBuf> = nomes.iter().map(|n| pasta.join(n)).collect();
            if let Some(faltando) = caminhos.iter().find(|c| !c.is_file()) {
                tentadas.push(faltando.display().to_string());
                continue;
            }
            let abrir = |c: &Path| {
                abrir_biblioteca(c).map_err(|e| {
                    tr(
                        "erros.motorDllNaoAbriu",
                        &[("caminho", &c.display().to_string()), ("detalhe", &e.to_string())],
                    )
                })
            };
            let llama = abrir(&caminhos[0])?;
            let registro = abrir(&caminhos[1])?;
            let base = abrir(&caminhos[2])?;
            let ggml = Ggml {
                versao: simbolo(&llama, "llama_version")?,
                init_backend: simbolo(&llama, "llama_backend_init")?,
                params_modelo: simbolo(&llama, "llama_model_default_params")?,
                params_contexto: simbolo(&llama, "llama_context_default_params")?,
                carregar_modelo: simbolo(&llama, "llama_model_load_from_file")?,
                liberar_modelo: simbolo(&llama, "llama_model_free")?,
                criar_contexto: simbolo(&llama, "llama_init_from_model")?,
                liberar_contexto: simbolo(&llama, "llama_free")?,
                ler_n_ctx: simbolo(&llama, "llama_n_ctx")?,
                ler_n_batch: simbolo(&llama, "llama_n_batch")?,
                vocabulario_do_modelo: simbolo(&llama, "llama_model_get_vocab")?,
                tokens_do_vocabulario: simbolo(&llama, "llama_vocab_n_tokens")?,
                embd_saida: simbolo(&llama, "llama_model_n_embd_out")?,
                tem_codificador: simbolo(&llama, "llama_model_has_encoder")?,
                tem_decodificador: simbolo(&llama, "llama_model_has_decoder")?,
                ler_agrupamento: simbolo(&llama, "llama_pooling_type")?,
                memoria: simbolo(&llama, "llama_get_memory")?,
                memoria_remover: simbolo(&llama, "llama_memory_seq_rm")?,
                lote_de_um: simbolo(&llama, "llama_batch_get_one")?,
                decodificar: simbolo(&llama, "llama_decode")?,
                codificar: simbolo(&llama, "llama_encode")?,
                logits: simbolo(&llama, "llama_get_logits")?,
                embeddings_da_sequencia: simbolo(&llama, "llama_get_embeddings_seq")?,
                embeddings_do_token: simbolo(&llama, "llama_get_embeddings_ith")?,
                sincronizar: simbolo(&llama, "llama_synchronize")?,
                adapter_init: simbolo(&llama, "llama_adapter_lora_init")?,
                adapter_free: simbolo(&llama, "llama_adapter_lora_free")?,
                set_adapters: simbolo(&llama, "llama_set_adapters_lora")?,
                quantos_devices: simbolo(&registro, "ggml_backend_dev_count")?,
                device: simbolo(&registro, "ggml_backend_dev_get")?,
                nome_device: simbolo(&base, "ggml_backend_dev_name")?,
                tipo_device: simbolo(&base, "ggml_backend_dev_type")?,
                memoria_device: simbolo(&base, "ggml_backend_dev_memory")?,
                _llama: llama,
                _registro: registro,
                _base: base,
            };
            // É isto que faz a `ggml.dll` procurar e registrar os backends da
            // pasta dela. A biblioteca internaliza o "uma vez por processo",
            // então carregar dois modelos não registra a placa duas vezes.
            unsafe { (ggml.init_backend)() };
            return Ok((ggml, pasta.clone()));
        }
        Err(tr("erros.motorDllNaoEncontrada", &[("caminhos", &tentadas.join(", "))]))
    }

    pub fn versao(&self) -> String {
        texto(unsafe { (self.versao)() })
    }

    /// Todos os devices que a ggml registrou, na ordem do registro.
    pub fn devices(&self) -> Vec<InfoDevice> {
        let n = unsafe { (self.quantos_devices)() };
        (0..n)
            .map(|i| {
                let dev = unsafe { (self.device)(i) };
                let mut livre = 0usize;
                let mut total = 0usize;
                unsafe { (self.memoria_device)(dev, &mut livre, &mut total) };
                InfoDevice {
                    indice: i,
                    bruto: dev,
                    nome: texto(unsafe { (self.nome_device)(dev) }),
                    tipo: unsafe { (self.tipo_device)(dev) },
                    livre,
                    total,
                }
            })
            .collect()
    }

    /// Os padrões vêm da própria biblioteca; aqui só se fixa o despacho completo
    /// para a placa. O dispositivo em si é entregue por `carregar`, que é quem
    /// tem o array NULL-terminated vivo.
    ///
    /// `camadas_gpu` vem do veredito medido em `alojamento::despacho`, não de uma
    /// escolha nossa: `-1` manda todas, um número manda as últimas N. É por este
    /// argumento que um modelo maior que a placa é carregado dividido em vez de
    /// pedir tudo e não caber.
    pub fn params_modelo(&self, camadas_gpu: c_int) -> ParamsModelo {
        let mut p = unsafe { (self.params_modelo)() };
        p.camadas_gpu = camadas_gpu;
        p.divisao = DIVISAO_NENHUMA;
        p
    }

    pub fn params_contexto(&self, janela: u32) -> ParamsContexto {
        let mut p = unsafe { (self.params_contexto)() };
        p.n_ctx = janela;
        p.flash_attn_type = ATENCAO_RAPIDA;
        p
    }

    /// O contexto de um modelo de embeddings. Três diferenças contra o de
    /// conversa, cada uma por um motivo medido:
    /// - `embeddings = true`, sem o qual a biblioteca não devolve vetor nenhum;
    /// - `pooling_type` fica no padrão (`UNSPECIFIED`), que é a biblioteca
    ///   lendo o agrupamento declarado no GGUF — o Qwen3-Embedding diz "último
    ///   token", um BERT diz "[CLS]", e quem decide é quem foi treinado;
    /// - a janela é a do lote, não a do treino. Reservar os 32k que o modelo
    ///   aceita custaria gigabytes de KV cache para ler peças de trezentos
    ///   tokens, e o `n_batch` é igual à janela porque uma peça atravessa o
    ///   modelo inteira, num lote só — partida, ela daria um vetor de meia
    ///   peça.
    pub fn params_embutir(&self, janela: u32) -> ParamsContexto {
        let mut p = unsafe { (self.params_contexto)() };
        p.n_ctx = janela;
        p.n_batch = janela;
        p.n_ubatch = janela;
        p.n_seq_max = 1;
        p.embeddings = true;
        p.pooling_type = AGRUPAMENTO_INDEFINIDO;
        // AUTO, não ENABLED: a janela de uma peça é curta, e aqui o quadrado
        // `n × n` que a atenção rápida evita nunca chega a ser o custo. O que se
        // quer é o caminho que a biblioteca considera suportado para a
        // arquitetura do modelo, e é o AUTO que sabe disso.
        p.flash_attn_type = ATENCAO_AUTO;
        p
    }

    /// Largura real do vetor que a biblioteca entrega, lida do modelo.
    ///
    /// # Segurança
    /// Invariante do chamador: `modelo` veio de `carregar` e continua vivo.
    pub fn dimensao_do_vetor(&self, modelo: *const c_void) -> usize {
        unsafe { (self.embd_saida)(modelo).max(0) as usize }
    }

    /// O agrupamento que o contexto vai aplicar, já resolvido com o que o
    /// modelo declara.
    ///
    /// # Segurança
    /// Invariante do chamador: `ctx` é um contexto vivo deste processo.
    pub fn agrupamento(&self, ctx: *const c_void) -> c_int {
        unsafe { (self.ler_agrupamento)(ctx) }
    }

    /// Se este modelo tem um codificador — é o que decide entre `llama_encode`
    /// e `llama_decode`. Um BERT passa pelo codificador; um decoder de
    /// embeddings, pelo decodificador.
    ///
    /// # Segurança
    /// Invariante do chamador: `modelo` veio de `carregar` e continua vivo.
    pub fn usar_codificador(&self, modelo: *const c_void) -> bool {
        let tem_codificador = unsafe { (self.tem_codificador)(modelo) };
        // Um modelo que tem os dois (enc-dec) precisa de estados que a extração
        // de vetor não implementa: neste caminho ele é um decodificador, e é o
        // que a própria biblioteca usa para um decoder puro.
        tem_codificador && !unsafe { (self.tem_decodificador)(modelo) }
    }

    /// Roda `tokens` como uma sequência própria e devolve o vetor do texto
    /// inteiro: o pooling da biblioteca sobre a sequência 0 quando o modelo
    /// declara um, e o vetor do último token quando não declara — que é
    /// exatamente o que um modelo de embeddings de decoder espera.
    ///
    /// As posições são explícitas de caso pensado: deixar a biblioteca contar
    /// faria a segunda peça começar na posição em que a primeira acabou, porque
    /// é a mesma sequência 0, e o vetor sairia das duas juntas.
    ///
    /// # Segurança
    /// Invariante do chamador: `ctx` é um contexto vivo, com `embeddings` ligado
    /// e `n_seq_max = 1`, e `n_embd` é `dimensao_do_vetor(modelo)` do modelo
    /// daquele contexto. Este método é o único que escreve no contexto.
    pub unsafe fn embutir(
        &self,
        ctx: *mut c_void,
        pelo_codificador: bool,
        tokens: &[Token],
        n_embd: usize,
    ) -> Result<Vec<f32>, String> {
        let n = tokens.len();
        if n == 0 || n_embd == 0 {
            return Err(tr("erros.motorSemEmbedding", &[("n", &n.to_string())]));
        }
        let zero: c_int = 0;
        let ids: Vec<Token> = tokens.to_vec();
        let pos: Vec<c_int> = (0..n as c_int).collect();
        let por_token: Vec<c_int> = vec![1; n];
        let sequencias: Vec<*const c_int> = vec![std::ptr::addr_of!(zero); n];
        // `logits` é o que escolhe as saídas copiadas para a host: marcado só o
        // último token, o contexto devolve um vetor — não um por token, que
        // custaria `n × n_embd` de cópia, nem os logits de todo o vocabulário.
        let marcados: Vec<i8> = {
            let mut v = vec![0i8; n];
            v[n - 1] = 1;
            v
        };
        let lote = Lote {
            n_tokens: n as c_int,
            token: ids.as_ptr(),
            embd: std::ptr::null(),
            pos: pos.as_ptr(),
            n_seq_id: por_token.as_ptr(),
            seq_id: sequencias.as_ptr(),
            logits: marcados.as_ptr(),
        };
        let codigo = if pelo_codificador {
            unsafe { (self.codificar)(ctx, lote) }
        } else {
            unsafe { (self.decodificar)(ctx, lote) }
        };
        if codigo != 0 {
            return Err(tr("erros.motorEmbedding", &[("codigo", &codigo.to_string())]));
        }
        // Como em `passo`: em placa a conta termina depois do retorno da função.
        unsafe { (self.sincronizar)(ctx) };
        let agrupado = unsafe { (self.embeddings_da_sequencia)(ctx, 0) };
        // Sem agrupamento declarado no modelo, o vetor do texto é o do último
        // token — que é exatamente o que um decoder de embeddings espera.
        let base = if agrupado.is_null() {
            let ultimo = unsafe { (self.embeddings_do_token)(ctx, -1) };
            if ultimo.is_null() {
                return Err(tr("erros.motorSemEmbedding", &[("n", &n.to_string())]));
            }
            ultimo
        } else {
            agrupado
        };
        Ok(unsafe { std::slice::from_raw_parts(base, n_embd) }.to_vec())
    }

    /// Carrega o GGUF. `selecionados` precisa permanecer vivo durante a chamada:
    /// é a lista NULL-terminated que `params` aponta.
    ///
    /// # Segurança
    /// Invariante do chamador: as bibliotecas estão abertas (este `Ggml` é o
    /// dono) e `selecionados` é uma lista NULL-terminated de devices válidos.
    pub unsafe fn carregar(
        &self,
        caminho: &Path,
        mut params: ParamsModelo,
        selecionados: &mut [DeviceBruto],
    ) -> Result<*mut c_void, String> {
        params.dispositivos = selecionados.as_mut_ptr();
        let nome = std::ffi::CString::new(caminho.as_os_str().to_string_lossy().into_owned())
            .map_err(|_| tr("erros.caminhoInvalido", &[("caminho", &caminho.display().to_string())]))?;
        let modelo = unsafe { (self.carregar_modelo)(nome.as_ptr(), params) };
        if modelo.is_null() {
            return Err(tr(
                "erros.motorNaoCarregou",
                &[("caminho", &caminho.display().to_string())],
            ));
        }
        Ok(modelo)
    }

    /// # Segurança
    /// Invariante do chamador: `modelo` veio de `carregar` e continua vivo.
    pub unsafe fn contexto(&self, modelo: *mut c_void, params: ParamsContexto) -> Result<*mut c_void, String> {
        let ctx = unsafe { (self.criar_contexto)(modelo, params) };
        if ctx.is_null() {
            return Err(tr("erros.motorSemContexto", &[]));
        }
        Ok(ctx)
    }

    pub fn liberar_contexto(&self, ctx: *mut c_void) {
        unsafe { (self.liberar_contexto)(ctx) }
    }

    pub fn liberar_modelo(&self, modelo: *mut c_void) {
        unsafe { (self.liberar_modelo)(modelo) }
    }

    /// Janela que a biblioteca aceitou.
    pub fn tamanho_contexto(&self, ctx: *const c_void) -> usize {
        unsafe { (self.ler_n_ctx)(ctx) as usize }
    }

    /// Tokens máximos num único `llama_decode`, lidos da biblioteca: fatiar o
    /// prefill por outra conta daria lote rejeitado.
    pub fn tamanho_lote(&self, ctx: *const c_void) -> usize {
        unsafe { (self.ler_n_batch)(ctx) as usize }
    }

    /// Quantos floats há num vetor de logits de `modelo`. É o tamanho do vocabulário,
    /// lido do vocabulário do próprio modelo: `llama_get_logits` devolve exatamente
    /// essa largura, e a dimensão embutida do modelo não tem relação com ela.
    ///
    /// # Segurança
    /// Invariante do chamador: `modelo` veio de `carregar` e continua vivo.
    pub fn vocabulario(&self, modelo: *const c_void) -> usize {
        let vocab = unsafe { (self.vocabulario_do_modelo)(modelo) };
        if vocab.is_null() {
            return 0;
        }
        unsafe { (self.tokens_do_vocabulario)(vocab).max(0) as usize }
    }

    /// Esquece a conversa: posições e KV cache voltam ao estado de modelo novo.
    ///
    /// # Segurança
    /// Invariante do chamador: `ctx` é um contexto vivo deste processo.
    pub fn limpar_cache(&self, ctx: *mut c_void) {
        let memoria = unsafe { (self.memoria)(ctx) };
        if memoria.is_null() {
            return;
        }
        // `seq < 0` casa com qualquer sequência e `p0`/`p1` negativos casam com
        // a extensão inteira: é o pedido "apaga tudo".
        unsafe { (self.memoria_remover)(memoria, -1, -1, -1) };
    }

    /// Carrega o GGUF de um adaptador **contra o modelo** que já está em
    /// memória. O cabeçalho é explícito sobre a dependência: um adaptador vale
    /// enquanto o modelo que o recebeu viver — por isso o handle mora no
    /// `Motor`, que é o dono dos dois, e nunca num lugar separado.
    ///
    /// # Segurança
    /// Invariante do chamador: `modelo` veio de `carregar` e continua vivo.
    pub unsafe fn carregar_adapter(
        &self,
        modelo: *mut c_void,
        caminho: &Path,
    ) -> Result<*mut c_void, String> {
        let nome = std::ffi::CString::new(caminho.as_os_str().to_string_lossy().into_owned())
            .map_err(|_| tr("erros.caminhoInvalido", &[("caminho", &caminho.display().to_string())]))?;
        let adaptador = unsafe { (self.adapter_init)(modelo, nome.as_ptr()) };
        if adaptador.is_null() {
            return Err(tr(
                "erros.adaptadorNaoCarregou",
                &[("caminho", &caminho.display().to_string())],
            ));
        }
        Ok(adaptador)
    }

    /// Devolve a memória de um adaptador. Null é "não havia nada", e a
    /// biblioteca não tem um `free` tolerante a null.
    pub fn liberar_adapter(&self, adaptador: *mut c_void) {
        if adaptador.is_null() {
            return;
        }
        unsafe { (self.adapter_free)(adaptador) }
    }

    /// Aplica exatamente `pedidos` ao contexto, cada um com a força pedida. O
    /// conjunto é sempre o inteiro, nunca um por chamada: meio conjunto
    /// aplicado é um modelo que ninguém escolheu. Lista vazia é o pedido
    /// explícito de "pesos puros".
    ///
    /// Os dois arrays locais são o dono da memória durante a chamada — a
    /// biblioteca só lê durante a execução, mas os handles e as forças têm de
    /// estar em dois blocos contíguos separados, e não num `Vec` de tuplas.
    ///
    /// # Segurança
    /// Invariante do chamador: `ctx` é um contexto vivo deste processo e cada
    /// handle veio de `carregar_adapter` para o modelo desse contexto.
    pub unsafe fn aplicar_adaptadores(
        &self,
        ctx: *mut c_void,
        pedidos: &[(*mut c_void, f32)],
    ) -> Result<(), String> {
        let mut handles: Vec<*mut c_void> = pedidos.iter().map(|(h, _)| *h).collect();
        let mut forcas: Vec<f32> = pedidos.iter().map(|(_, f)| *f).collect();
        let codigo = unsafe {
            (self.set_adapters)(ctx, handles.as_mut_ptr(), pedidos.len(), forcas.as_mut_ptr())
        };
        if codigo != 0 {
            return Err(tr("erros.adaptadorAplicacao", &[("codigo", &codigo.to_string())]));
        }
        Ok(())
    }

    /// Roda os `n` ids apontados por `tokens` e copia os logits da última
    /// posição.
    ///
    /// # Segurança
    /// Invariante do chamador: `ctx` é um contexto vivo, `tokens` aponta `n` ids
    /// que não mudam durante a chamada, e `n_vocab` é `vocabulario(modelo)`.
    /// Este método é o único que escreve no contexto: um contexto por thread.
    pub unsafe fn passo(
        &self,
        ctx: *mut c_void,
        tokens: *mut Token,
        n: usize,
        n_vocab: usize,
    ) -> Result<Vec<f32>, String> {
        let lote = unsafe { (self.lote_de_um)(tokens, n as c_int) };
        let codigo = unsafe { (self.decodificar)(ctx, lote) };
        if codigo != 0 {
            return Err(tr("erros.motorDecodificacao", &[("codigo", &codigo.to_string())]));
        }
        // A decodificação só entrega o grafo para a fila da placa: sem esperar o
        // trabalho terminar, em GPU leríamos os logits do token anterior — e a
        // medição de tempo também deixaria de medir a placa.
        unsafe { (self.sincronizar)(ctx) };
        let base = unsafe { (self.logits)(ctx) };
        if base.is_null() {
            return Err(tr("erros.motorSemLogits", &[]));
        }
        // O buffer pertence ao contexto e é reescrito a cada decodificação: a
        // cópia é o que sai daqui.
        Ok(unsafe { std::slice::from_raw_parts(base, n_vocab) }.to_vec())
    }
}
