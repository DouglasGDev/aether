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
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

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

/// `LLAMA_FLASH_ATTN_TYPE_DISABLED`. É o degrau que a sonda de CPU compara com o
/// de cima: na placa o quadrado `n × n` é memória que falta, no processador ele é
/// aritmética a mais — e nem toda arquitetura tem kernel de atenção rápida em CPU.
const ATENCAO_NULA: c_int = 0;

/// Os números que a carga na CPU do sistema obedece. Vêm da sonda
/// (`engine-probe cpu:`), não de uma preferência: `lote` é o
/// `n_batch`/`n_ubatch` do contexto e `atencao_rapida` liga ou desliga o kernel
/// de flash attention.
///
/// As threads são duas porque a biblioteca são duas: `threads` é a
/// `n_threads`, que o llama.cpp usa no passo de um token só (a decodificação), e
/// `threads_lote` é a `n_threads_batch`, usada no passo de muitos tokens (o
/// prefill). Medido no 5700X3D (docs/melhorias/CPU.md), elas querem números
/// opostos: o prefill rende mais com os 16 fios lógicos (75,3 t/s contra 60,8 dos
/// 8), e a decodificação rende mais com os 8 núcleos físicos (10,16 t/s contra
/// 8,41). Um `Escala` só não pode dizer as duas coisas — por isso ele carrega as
/// duas. Zero em qualquer uma é "deixa com a biblioteca", que é o braço de
/// controle da medição, nunca o valor publicado.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Escala {
    pub threads: c_int,
    pub threads_lote: c_int,
    pub lote: u32,
    pub atencao_rapida: bool,
}

/// Quantos núcleos físicos a máquina tem. É o número que a decodificação obedece,
/// e não é o `available_parallelism`: no 5700X3D os dois são 16 contra 8, e o
/// passo de um token cai de 10,16 para 8,41 t/s quando os 16 fios trabalham nele
/// (docs/melhorias/CPU.md) — os fios de um mesmo núcleo disputam a unidade de
/// execução que já está parada no DRAM. Contar o núcleo em vez de dividir o
/// total por dois é o que faz a escala valer numa máquina que não é esta.
///
/// `None` quando a contagem falha: quem chama cai no total de fios lógicos, que é
/// o degrau onde a biblioteca já estava, e nunca um zero.
#[cfg(windows)]
pub fn nucleos_fisicos() -> Option<usize> {
    use windows::Win32::System::SystemInformation::{
        GetLogicalProcessorInformationEx, RelationProcessorCore,
    };
    let mut necessario: u32 = 0;
    // Primeira chamada sem buffer: ela recusa escrevendo o tamanho de que precisa.
    // SAFETY: o único destino escrito é o `necessario` vivo nesta pilha.
    let _ = unsafe {
        GetLogicalProcessorInformationEx(RelationProcessorCore, None, &mut necessario)
    };
    if necessario == 0 {
        return None;
    }
    // A sequência é de structs de 8 bytes de alinhamento, então o caixa é medido
    // em `u64`: um `Vec<u8>` não promete alinhamento nenhum, e a API escreve
    // structs C dentro dele.
    let mut caixa = vec![0u64; (necessario as usize).div_ceil(8)];
    // O mesmo parâmetro faz os dois lados da conversa: entra como o tamanho do
    // buffer que a API pode escrever e sai como a quantidade que ela escreveu.
    // Zerá-lo — como este arquivo fazia — é pedir a chamada com um buffer de zero
    // bytes: ela recusa com `ERROR_INSUFFICIENT_BUFFER`, devolve o tamanho certo no
    // mesmo lugar, e quem olha só o erro conclui que a máquina não respondeu.
    let mut obtido: u32 = necessario;
    // SAFETY: `caixa` cabe os bytes que a primeira chamada pediu, e a função só
    // escreve dentro dele.
    let ligada = unsafe {
        GetLogicalProcessorInformationEx(
            RelationProcessorCore,
            Some(caixa.as_mut_ptr().cast()),
            &mut obtido,
        )
    };
    if ligada.is_err() || obtido == 0 {
        return None;
    }
    // A conferência é do `obtido`, não da reserva: a segunda chamada devolve
    // quantos bytes ela realmente escreveu.
    // SAFETY: nenhum struct é lido por aqui — `u8` tem alinhamento 1, então toda
    // a sequência de `u64` é reinterpretável como bytes, e `caminhar` só olha
    // dentro do recorte que lhe é entregue.
    let (_, bytes, _) = unsafe { caixa.align_to::<u8>() };
    let logicos = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0);
    caminhar(&bytes[..(obtido as usize).min(bytes.len())], logicos).map(|(nucleos, _)| nucleos)
}

/// Caminha a sequência que a API devolve um registro por vez, pelo `Size` que o
/// próprio registro anuncia, e tira núcleos e fios das máscaras de afinidade —
/// não do número de registros lidos.
///
/// Os dois detalhes são o ponto. Avançar de `size_of::<struct>()` em
/// `size_of::<struct>()` só dá certo enquanto todo registro tem exatamente o
/// tamanho do struct, e `GroupMask` é um `ANYSIZE_ARRAY` declarado com uma casa:
/// numa máquina com mais de 64 processadores lógicos o registro do núcleo vem com
/// um `GROUP_AFFINITY` por grupo, ou seja, maior que o struct. Somar um núcleo por
/// item encontrado é a outra metade do mesmo erro — o número que a decodificação
/// obedece é o de núcleos, e o que diz se a leitura fez sentido é a soma dos fios.
///
/// `None` para sequência que não fecha no `obtido`, `Size` menor que o cabeçalho
/// (o laço não anda), núcleo em mais de um grupo (a struct tem espaço para a
/// primeira máscara, e contar meio núcleo é pior que não contar) ou soma de fios
/// diferente de `logicos`.
#[cfg(windows)]
fn caminhar(bytes: &[u8], logicos: usize) -> Option<(usize, usize)> {
    use windows::Win32::System::SystemInformation::{
        GROUP_AFFINITY, LOGICAL_PROCESSOR_RELATIONSHIP, PROCESSOR_RELATIONSHIP,
        RelationProcessorCore, SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
    };
    // Os deslocamentos saem das próprias structs, não de números escritos à mão:
    // as structs são o espelho do C, e um campo que mudar de lugar muda aqui junto.
    let d_tipo = std::mem::offset_of!(SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX, Relationship);
    let d_tamanho = std::mem::offset_of!(SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX, Size);
    let d_anonimo = std::mem::offset_of!(SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX, Anonymous);
    let d_grupos = d_anonimo + std::mem::offset_of!(PROCESSOR_RELATIONSHIP, GroupCount);
    let d_mascara = d_anonimo
        + std::mem::offset_of!(PROCESSOR_RELATIONSHIP, GroupMask)
        + std::mem::offset_of!(GROUP_AFFINITY, Mask);
    let bytes_mascara = std::mem::size_of::<usize>();

    let fatia = |de: usize, tamanho: usize| -> Option<&[u8]> { bytes.get(de..de + tamanho) };
    let mut nucleos = 0usize;
    let mut fios = 0usize;
    let mut de = 0usize;
    while de < bytes.len() {
        let tipo = i32::from_le_bytes(fatia(de + d_tipo, 4)?.try_into().ok()?);
        let tamanho = u32::from_le_bytes(fatia(de + d_tamanho, 4)?.try_into().ok()?) as usize;
        if tamanho <= d_anonimo || de + tamanho > bytes.len() {
            return None;
        }
        if LOGICAL_PROCESSOR_RELATIONSHIP(tipo) == RelationProcessorCore {
            if u16::from_le_bytes(fatia(de + d_grupos, 2)?.try_into().ok()?) != 1 {
                return None;
            }
            let mascara =
                usize::from_le_bytes(fatia(de + d_mascara, bytes_mascara)?.try_into().ok()?);
            let do_nucleo = mascara.count_ones() as usize;
            if do_nucleo == 0 {
                return None;
            }
            nucleos += 1;
            fios += do_nucleo;
        }
        de += tamanho;
    }
    if nucleos == 0 || (logicos > 0 && fios != logicos) {
        return None;
    }
    Some((nucleos, fios))
}

#[cfg(not(windows))]
pub fn nucleos_fisicos() -> Option<usize> {
    None
}

impl Escala {
    /// A escala lida na própria máquina: decodificação nos núcleos físicos,
    /// prefill em todos os fios lógicos, e o lote com que a dupla ganhou (medida
    /// em docs/melhorias/CPU.md). Onde a contagem de núcleos falha, os dois
    /// números coincidem — que é o degrau de antes, não uma adivinhação.
    pub fn da_maquina() -> Self {
        let logicos = std::thread::available_parallelism()
            .map(|n| n.get() as c_int)
            .unwrap_or(0);
        let fisicos = nucleos_fisicos().map(|n| n as c_int).unwrap_or(logicos);
        Self {
            threads: fisicos,
            threads_lote: logicos,
            lote: LOTE_CPU,
            atencao_rapida: ATENCAO_RAPIDA_LIGADA,
        }
    }
}

/// O `n_batch` com que a CPU andou mais rápido no lote grande. É o valor com que
/// a braço medido ganhou o prefill sem perder decodificação (docs/melhorias/CPU.md).
pub const LOTE_CPU: u32 = 1024;

/// Se a atenção rápida entra no contexto de conversa da CPU. Não é "ligada porque
/// existe": o kernel troca decodificação por prefill, e o laço de ferramentas do
/// Aether é pré-fil por dez tokens de saída para um — `bancada.rs:8` cita os
/// 29.777 tokens de prompt recalculados por rodada antes do cache de prefixo.
pub const ATENCAO_RAPIDA_LIGADA: bool = true;

/// O sentinela do llama.cpp para "todas as camadas": negativo não é "quantas
/// couberem" — a biblioteca manda todas e a conta de caber é de quem escolhe.
/// É por isso que os dois caminhos passam um número medido
/// (`alojamento::despacho`, no LLM; `super::despachar_codificador`, no
/// embutidor) e só usam esta constante quando o veredito diz `Inteira` — ou
/// quando não houve veredito possível, e aí o motivo vai junto, nunca calado.
/// O embutidor costuma ouvir `-1`: dezenas de megabytes contra a VRAM livre
/// medida costumam caber inteiros. O que não pode é isso ser uma prescrição do
/// arquivo em vez de uma conta lida nele.
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
    memoria_limpar: unsafe extern "C" fn(*mut c_void, bool),
    /// `llama_memory_seq_rm(mem, llama_seq_id, llama_pos p0, llama_pos p1)`. Os
    /// três inteiros são `int32_t` nesta build — medido no cabeçalho da release
    /// (`typedef int32_t llama_pos;`), e não um detalhe de gosto: declará-los
    /// como `int64_t` faria a biblioteca ler os bits errados da pilha.
    memoria_seq_rm: unsafe extern "C" fn(*mut c_void, c_int, c_int, c_int) -> bool,
    set_abort: unsafe extern "C" fn(
        *mut c_void,
        Option<extern "C" fn(*mut c_void) -> bool>,
        *mut c_void,
    ),
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

extern "C" fn abortar(data: *mut c_void) -> bool {
    !data.is_null() && unsafe { &*(data as *const AtomicBool) }.load(Ordering::Relaxed)
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

/// Mantém as três DLLs mapeadas pelo resto do processo.
///
/// A `ggml-base.dll` da release b12387 instala um `std::set_terminate` no
/// carregamento. Esse slot é do processo, não do módulo: quando o último
/// `FreeLibrary` a descarrega, o ponteiro fica apontando para o endereço
/// antigo. O próximo `LoadLibrary` — comum aqui, porque `capacidade` abre e
/// solta antes de `Motor::novo` abrir de novo — vê
/// `prev == ggml_uncaught_exception` e aborta com `GGML_ASSERT` /
/// `STATUS_STACK_BUFFER_OVERRUN` (`0xC0000409`). Ver
/// [ggml-org/llama.cpp#15752](https://github.com/ggml-org/llama.cpp/issues/15752).
///
/// Um handle a mais deixa o contador acima de zero quando o `Ggml` da chamada
/// cai, e o assert deixa de ser alcançável.
///
/// Devolve o erro do carregador em vez de entrar em pânico, e é escolha de
/// projeto, não pudor: o pino vivia dentro de `OnceLock::get_or_init` com um
/// `expect` no fechamento, e o que esse `expect` fazia estava medido — a thread
/// caía com a nossa frase (`llama.dll já abriu nesta chamada: LoadLibraryExW
/// { source: 126 }`) e o erro real do carregador nunca chegava à tela. Um
/// `expect` dentro de `get_or_init` também re-executa o fechamento a cada
/// chamada seguinte — conferido com `std::sync::OnceLock` nesta toolchain, que
/// ao contrário de `std::sync::Once` não envenena: o fechamento rodou duas vezes
/// e a terceira abertura funcionou. `set` é outra história: ou o pino entra, ou a
/// chamada devolve o `detalhe` que a Windows deu, e a próxima tentativa acontece
/// de novo.
fn segurar_no_processo(llama: &Path, registro: &Path, base: &Path) -> Result<(), String> {
    static PIN: OnceLock<Pino> = OnceLock::new();
    pinar(&PIN, llama, registro, base)
}

/// As três bibliotecas que o processo se recusa a descarregar.
struct Pino {
    _llama: Library,
    _registro: Library,
    _base: Library,
}

/// O corpo de `segurar_no_processo` com o destino como argumento. Não é
/// gosto: o destino real é um estático do processo, e um teste que escrevesse
/// nele dependeria da ordem em que os outros testes rodam — hoje
/// `abrir_soltar_abrir_nao_aborta` pinar com as DLLs reais e a recusa simulada
/// deste arquivo passaria a ver `Ok(())` por acaso. Com o `OnceLock` passado
/// por parâmetro a recusa do carregador é conferida numa instância própria,
/// e o que muda de comportamento é só o corpo abaixo.
fn pinar(
    destino: &OnceLock<Pino>,
    llama: &Path,
    registro: &Path,
    base: &Path,
) -> Result<(), String> {
    if destino.get().is_some() {
        // Já preso por uma abertura anterior deste processo: nada a fazer, e
        // nada a abrir — o contador já está guardado.
        return Ok(());
    }
    // Acabamos de abrir os mesmos caminhos nesta chamada: falhar aqui é o
    // carregador do Windows recusando um módulo que ele mesmo acabou de mapear.
    // Raro, e por isso mesmo merece o erro dele, não uma frase nossa.
    let presa = |c: &Path, rotulo: &str| -> Result<Library, String> {
        abrir_biblioteca(c).map_err(|e| {
            tr(
                "erros.motorDllNaoAbriu",
                &[
                    ("caminho", &c.display().to_string()),
                    ("detalhe", &format!("pino de {rotulo}: {e}")),
                ],
            )
        })
    };
    let pino = Pino {
        _llama: presa(llama, "llama.dll")?,
        _registro: presa(registro, "ggml.dll")?,
        _base: presa(base, "ggml-base.dll")?,
    };
    // Duas threads podem chegar aqui juntas; a que perde solta os três handles
    // extras dela, e a contagem continua acima de zero pelo pino que venceu.
    let _ = destino.set(pino);
    Ok(())
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
            // Antes de qualquer `Drop` de `Ggml`: sem isto, a próxima abertura
            // no mesmo processo aborta no assert do terminate handler.
            if let Err(motivo) = segurar_no_processo(&caminhos[0], &caminhos[1], &caminhos[2]) {
                // O pino falhou, mas estes três mapeamentos já aconteceram e o
                // `set_terminate` da `ggml-base.dll` já está no slot do
                // processo. Descarregá-los agora é exatamente o estado pendurado
                // que o pino existe para impedir: o próximo `LoadLibrary` aborta
                // com `0xC0000409`. Então os handles desta chamada viram o pino
                // — ficam no processo — e o erro real do carregador vai para a
                // tela em vez de virar um texto nosso.
                std::mem::forget((llama, registro, base));
                return Err(motivo);
            }
            let ggml = Ggml {
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
                memoria_limpar: simbolo(&llama, "llama_memory_clear")?,
                memoria_seq_rm: simbolo(&llama, "llama_memory_seq_rm")?,
                set_abort: simbolo(&llama, "llama_set_abort_callback")?,
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

    pub fn definir_aborte(&self, contexto: *mut c_void, cancel: Option<&AtomicBool>) {
        let (callback, data) = match cancel {
            Some(flag) => (Some(abortar as extern "C" fn(*mut c_void) -> bool), flag as *const AtomicBool as *mut c_void),
            None => (None, std::ptr::null_mut()),
        };
        unsafe { (self.set_abort)(contexto, callback, data) };
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

    /// O contexto de conversa quando a conta acontece na CPU do sistema. Aqui os
    /// números que importam num processador — quantas threads na decodificação,
    /// quantas no lote, o tamanho do lote e se a atenção rápida está ligada —
    /// deixam de ser o palpite da biblioteca e passam a ser o que a sonda mediu.
    /// O `n_ubatch` acompanha o `n_batch` porque, sem placa, o lote é o quanto de
    /// token cabe na cache de nível 3 de uma passada: fatiar por dentro do lote
    /// só multiplica as idas ao DRAM que é o gargalo.
    pub fn params_contexto_cpu(
        &self,
        janela: u32,
        escala: &Escala,
    ) -> ParamsContexto {
        let mut p = unsafe { (self.params_contexto)() };
        p.n_ctx = janela;
        p.n_batch = escala.lote;
        p.n_ubatch = escala.lote;
        p.n_threads = escala.threads;
        p.n_threads_batch = escala.threads_lote;
        p.flash_attn_type = if escala.atencao_rapida { ATENCAO_RAPIDA } else { ATENCAO_NULA };
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
        // `llama_memory_clear` é o pedido de "apaga tudo"; `seq_rm(-1, -1, -1)`
        // parece o mesmo e não é: o `-1` de "qualquer sequência" do cabeçalho é
        // rejeitado por esta build contra `n_seq_max = 1` — medido no log do app
        // (`seq_rm: invalid seq_id (-1) - larger than n_seq_max (1)`), e como a
        // função devolve `false` e ninguém olha, o cache do turno anterior
        // continuava ocupado e o lote seguinte morria em "failed to find a
        // memory slot". Os buffers em si não precisam de zeragem: o que decide
        // o aproveitamento de uma célula é o metadado, que aqui volta a vazio.
        unsafe { (self.memoria_limpar)(memoria, false) };
    }

    /// Apaga da posição `p0` em diante e guarda o prefixo `[0, p0)`. É o recorte
    /// que deixa o começo de um prompt ser reaproveitado: o que vem depois da
    /// divergência é de uma resposta que não continua nesta rodada.
    ///
    /// `false` é "este pedaço não pôde ser removido". Não é um acidente nem uma
    /// recusa a tentar: é o que a release documenta ("Returns false if a partial
    /// sequence cannot be removed") e o que foi medido nesta máquina com um
    /// modelo híbrido de estado recorrente (`qwen35.ssm.*`) — a metade
    /// recorrente da memória só desfaz o estado dentro dos instantâneos do
    /// último lote, e um recorte 47 células atrás está fora deles. Quem chama
    /// tem de esquecer tudo e prefilar do zero: seguir com um cache cujo
    /// conteúdo ninguém sabe é continuar a conversa errada.
    ///
    /// O `seq_id` é 0 e não -1 pelo mesmo motivo medido em `limpar_cache`: esta
    /// build tem `n_seq_max = 1` e rejeita o "qualquer sequência" do cabeçalho.
    /// `p1 = -1` é o "até o fim" que o cabeçalho documenta.
    ///
    /// # Segurança
    /// Invariante do chamador: `ctx` é um contexto vivo deste processo.
    pub fn apagar_de(&self, ctx: *mut c_void, p0: usize) -> bool {
        let memoria = unsafe { (self.memoria)(ctx) };
        if memoria.is_null() {
            return false;
        }
        unsafe { (self.memoria_seq_rm)(memoria, 0, p0 as c_int, -1) }
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

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn o_aborto_nativo_le_o_cancelador_vivo_do_turno() {
        let cancel = AtomicBool::new(false);
        let dados = &cancel as *const AtomicBool as *mut c_void;
        assert!(!abortar(dados));
        cancel.store(true, Ordering::Relaxed);
        assert!(abortar(dados));
        assert!(!abortar(std::ptr::null_mut()));
    }

    /// Um caminho que nenhuma DLL tem, e que o carregador do Windows recusa com
    /// o erro dele mesmo: não é um falso montado por este teste, é a resposta
    /// real do `LoadLibraryW` a um módulo que não existe.
    fn dll_inexistente(rotulo: &str) -> PathBuf {
        PathBuf::from(std::env::temp_dir()).join(format!("aether-ffi-sem-dll-{rotulo}.dll"))
    }

    /// A recusa do pino tem de voltar como erro do carregador, e o destino tem
    /// de continuar vazio para a próxima tentativa. O defeito original era o
    /// `expect` dentro de `get_or_init`, medido na experimentação: a thread caía
    /// com a nossa frase (`llama.dll já abriu nesta chamada: LoadLibraryExW {
    /// source: 126 }`) e o erro real nunca chegava à tela.
    ///
    /// O destino é um `OnceLock` deste teste, não o pino do processo: escrever no
    /// estático deixaria o resultado dependente da ordem dos testes, porque
    /// `abrir_soltar_abrir_nao_aborta` já o preencheu com as DLLs reais.
    ///
    /// O ramo do `Ok(())` para pino já vivo não tem teste aqui: ele só existe
    /// com três `Library` reais dentro do `Pino`, e é exatamente o que
    /// `reload::abrir_soltar_abrir_nao_aborta` faz — ele chama `Ggml::abrir`
    /// duas vezes no mesmo processo, e a segunda passagem é por esse ramo; se
    /// ele devolvesse `Err`, a segunda abertura falharia e aquele teste também.
    #[test]
    fn prender_dlls_recusadas_devolve_o_motivo_do_carregador_sem_panico() {
        let alvo = dll_inexistente("pino");
        let destino: OnceLock<Pino> = OnceLock::new();

        let primeira = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            pinar(&destino, &alvo, &alvo, &alvo)
        }))
        .unwrap_or_else(|_| panic!("o pino entrou em pânico em vez de devolver o erro"));
        let motivo = primeira.expect_err("um caminho sem DLL não pode pinar nada");
        assert!(
            motivo.contains(&alvo.display().to_string()),
            "o motivo não diz qual caminho o carregador recusou: {motivo}"
        );
        assert!(
            motivo.contains("llama.dll"),
            "o motivo não diz qual das três o carregador recusou primeiro: {motivo}"
        );
        assert!(
            destino.get().is_none(),
            "o pino recusou e mesmo assim entrou no destino: {motivo}"
        );

        // Segunda chamada: o carregador recusa de novo, e é a resposta dele que
        // tem de vir — não um pânico de inicialização.
        let segunda = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            pinar(&destino, &alvo, &alvo, &alvo)
        }))
        .unwrap_or_else(|_| panic!("a segunda chamada panicou em vez de devolver o erro"));
        assert!(
            segunda.is_err(),
            "a segunda chamada deveria devolver o erro do carregador de novo: {segunda:?}"
        );
    }

    /// Os números da escala têm de chegar ao `llama_context_params`, porque é por
    /// eles que a CPU faz o que a sonda mediu — e cada um vai a um campo
    /// diferente. Um `n_threads` que não fosse copiado deixaria a biblioteca
    /// escolhendo sozinha, que é exatamente o estado que a medição veio substituir.
    ///
    /// As duas contagens de thread vão a valores diferentes de propósito: a
    /// medição mostrou que o prefill e o decode querem números opostos no mesmo
    /// processador, e um `n_threads_batch = escala.threads` disfarçado de escala
    /// dividida passaria verde neste teste entregando uma escala única.
    ///
    /// Sem runtime ggml na máquina o teste se retira: ele confere a cópia dos
    /// campos, e a cópia só existe depois de a biblioteca abrir.
    #[test]
    fn a_escala_cpu_chega_ao_contexto() {
        let pastas = crate::ai::ggml::reload::pastas_teste();
        let Ok((ggml, _pasta)) = Ggml::abrir(&pastas) else {
            eprintln!("sem runtime ggml: nada a conferir");
            return;
        };
        let medida = ggml.params_contexto_cpu(
            4096,
            &Escala { threads: 6, threads_lote: 12, lote: 1024, atencao_rapida: true },
        );
        assert_eq!(medida.n_ctx, 4096);
        assert_eq!(medida.n_batch, 1024, "o lote pedido não chegou ao contexto");
        assert_eq!(medida.n_ubatch, 1024, "o micro-lote não acompanhou o lote");
        assert_eq!(medida.n_threads, 6, "as threads da decodificação não chegaram");
        assert_eq!(
            medida.n_threads_batch, 12,
            "as threads do lote chegaram amassadas nas da decodificação"
        );
        assert_eq!(medida.flash_attn_type, ATENCAO_RAPIDA);

        // O braço de controle: `threads = 0` é a biblioteca decidindo, e a
        // atenção desligada é o outro degrau da escala — os dois têm de passar
        // ilesos por esta mesma cópia, senão o zero virou um número nosso.
        let padrao = ggml.params_contexto_cpu(
            2048,
            &Escala { threads: 0, threads_lote: 0, lote: 512, atencao_rapida: false },
        );
        assert_eq!(padrao.n_threads, 0);
        assert_eq!(padrao.n_threads_batch, 0);
        assert_eq!(padrao.flash_attn_type, ATENCAO_NULA);
        assert_eq!(padrao.n_batch, 512);
    }

    /// A sequência que a API devolve é uma lista de registros de tamanhos
    /// diferentes, e era exatamente isso que a leitura antiga errava: ela avançava
    /// de `size_of::<struct>()` em `size_of::<struct>()` e somava um núcleo por
    /// item encontrado. Ninguém tem aqui uma máquina de 128 processadores lógicos
    /// para ver o estrago, então o estrago vem escrito à mão: um `Vec<u8>` com
    /// registros de 48 e de 64 bytes, um item de outro tipo no meio e máscaras de
    /// afinidade reais. Os deslocamentos saem das mesmas structs da produção, e é
    /// por isso que o fixture não precisa saber quantos bytes tem um registro.
    #[cfg(windows)]
    #[test]
    fn a_caminhada_le_a_sequencia_pelo_tamanho_de_cada_registro() {
        use windows::Win32::System::SystemInformation::{
            GROUP_AFFINITY, PROCESSOR_RELATIONSHIP, RelationCache,
            SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
        };
        let d_tipo = std::mem::offset_of!(SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX, Relationship);
        let d_tamanho = std::mem::offset_of!(SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX, Size);
        let d_anonimo = std::mem::offset_of!(SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX, Anonymous);
        let d_grupos = d_anonimo + std::mem::offset_of!(PROCESSOR_RELATIONSHIP, GroupCount);
        let d_mascara = d_anonimo
            + std::mem::offset_of!(PROCESSOR_RELATIONSHIP, GroupMask)
            + std::mem::offset_of!(GROUP_AFFINITY, Mask);
        let tamanho_mascara = std::mem::size_of::<usize>();
        let tamanho_grupo = std::mem::size_of::<GROUP_AFFINITY>();

        // Núcleo anunciado com `total` bytes: dois grupos caberiam num registro de
        // 64 e um num de 48, e é o `Size` escrito aqui que o laço tem de obedecer.
        let nucleo = |total: usize, grupos: u16, maskaras: &[usize]| -> Vec<u8> {
            let mut b = vec![0u8; total];
            b[d_tipo..d_tipo + 4].copy_from_slice(&0i32.to_le_bytes());
            b[d_tamanho..d_tamanho + 4].copy_from_slice(&(total as u32).to_le_bytes());
            b[d_grupos..d_grupos + 2].copy_from_slice(&grupos.to_le_bytes());
            for (casa, m) in maskaras.iter().enumerate() {
                let de = d_mascara + casa * tamanho_grupo;
                b[de..de + tamanho_mascara].copy_from_slice(&m.to_le_bytes());
            }
            b
        };
        // Registro de outro tipo, cheio de `0xff` onde um núcleo teria máscaras:
        // se a caminhada lesse todo item como processador, estes bytes virariam fios.
        let estrangeiro = |total: usize| -> Vec<u8> {
            let mut b = vec![0u8; total];
            b[d_tipo..d_tipo + 4].copy_from_slice(&RelationCache.0.to_le_bytes());
            b[d_tamanho..d_tamanho + 4].copy_from_slice(&(total as u32).to_le_bytes());
            for byte in b[d_anonimo..].iter_mut() {
                *byte = 0xff;
            }
            b
        };

        // O caso desta máquina: dois núcleos de dois fios, quatro fios no total.
        let dois = [nucleo(48, 1, &[0b0011]), nucleo(48, 1, &[0b1100])].concat();
        assert_eq!(caminhar(&dois, 4), Some((2, 4)));

        // Máquinas com mais de 64 processadores lógicos devolvem o registro maior.
        // Caminhar por `size_of` leria a segunda máscara como se fosse um registro.
        let largos = [nucleo(64, 1, &[0b0011]), nucleo(64, 1, &[0b1100])].concat();
        assert_eq!(caminhar(&largos, 4), Some((2, 4)));

        // O tipo filtra: o `0xff` do cache no meio não conta como fio de ninguém.
        let misto = [nucleo(48, 1, &[0b0011]), estrangeiro(40), nucleo(48, 1, &[0b1100])].concat();
        assert_eq!(caminhar(&misto, 4), Some((2, 4)));

        // Sequência truncada no meio do segundo registro: nada contado às pressas.
        assert_eq!(caminhar(&dois[..dois.len() - 8], 4), None);
        // Núcleo espalhado em dois grupos: a struct tem espaço para a primeira
        // máscara, e meio núcleo contado é pior que nenhum.
        assert_eq!(caminhar(&nucleo(64, 2, &[0b0011, 0b1100]), 4), None);
        // `Size` menor que o cabeçalho faria o laço andar zero bytes para sempre.
        // O registro é inteiro; só o tamanho que ele anuncia é falso.
        let curto = {
            let mut b = nucleo(48, 1, &[0b0011]);
            b[d_tamanho..d_tamanho + 4].copy_from_slice(&4u32.to_le_bytes());
            b
        };
        assert_eq!(caminhar(&curto, 4), None);
        // A soma dos fios tem de bater com a máquina; se não bate, a leitura não
        // foi entendida e o chamador cai no degrau de antes, não neste número.
        assert_eq!(caminhar(&dois, 8), None);
        // Um único núcleo com os 16 fios ligados é SMT fechado na BIOS — legítimo,
        // e é o caso em que `threads` e `threads_lote` voltam a coincidir.
        assert_eq!(caminhar(&nucleo(48, 1, &[0xffff]), 16), Some((1, 16)));
    }

    /// A caminhada acima é provada por bytes escritos à mão; este confere que a
    /// sequência real desta máquina fecha com ela. Não se pede um número — pede-se
    /// que a API responda, que o resultado divida a contagem de fios (núcleos
    /// iguais em um mesmo pacote) e que nunca seja maior que ela.
    #[cfg(windows)]
    #[test]
    fn a_contagem_real_fecha_com_a_sequencia_da_api() {
        let logicos = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0);
        let nucleos = nucleos_fisicos()
            .expect("a API de núcleos não respondeu nesta máquina Windows");
        assert!(nucleos >= 1 && nucleos <= logicos, "{nucleos} fora de 1..={logicos}");
        assert_eq!(
            logicos % nucleos,
            0,
            "{logicos} fios não se dividem em {nucleos} núcleos"
        );
        // E a escala publicada obedece a esta contagem: decodificação no núcleo,
        // prefill no fio lógico.
        let escala = Escala::da_maquina();
        assert_eq!(escala.threads, nucleos as c_int);
        assert_eq!(escala.threads_lote, logicos as c_int);
    }
}
