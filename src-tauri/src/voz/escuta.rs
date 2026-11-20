//! A escuta: o microfone grava e o Whisper transcreve, na mesma máquina.
//!
//! Quem aperta o botão liga o microfone e quem o solta pede o texto. Entre os
//! dois, uma thread coleta o que o dispositivo de entrada entrega num buffer sem
//! trava; nada é transcrito durante a captura, porque o Whisper trabalha com o
//! áudio inteiro e uma frase cortada em pedaços perde o contexto.
//!
//! A captura abre o microfone no formato que o sistema lhe dá — na maioria dos
//! aparelhos, estéreo a 48 kHz — e reduz a mono aqui mesmo. Pedir 16 kHz mono
//! direto ao WASAPI é o que faz a abertura falhar com "formato não suportado" em
//! vários endpoints: medido nesta máquina, o cabo virtual não abre de jeito
//! nenhum a 16 kHz, enquanto entrega som verdadeiro na taxa nativa. O Whisper
//! recebe a taxa com que o áudio foi gravado e reamostra ele mesmo.

use super::ffi::{ConfigReconhecimento, Opaco, Sherpa};
use super::modelos::EspecVoz;
use crate::idiomas::tr;
use crate::state::trava_estado;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, Host, StreamConfig};
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Taxa em que os coeficientes mel do modelo são calculados. É o que o sherpa
/// espera no `feat_config`; o áudio chega na taxa do aparelho e é ele quem
/// reamostra.
const TAXA_DO_MODELO: u32 = 16_000;

/// Coeficientes mel que o modelo de fala consome.
const DIMENSÃO_DA_FEATURE: i32 = 80;

/// Teto de uma escuta só, para o botão ligado sem querer não crescer a memória
/// da aplicação até ela tombar. Um minuto de float são ~3,8 MB a 16 kHz e ~11 MB
/// a 48 kHz, a taxa em que a maioria dos microfones de desktop fala.
const LIMITE_DE_SEGUNDOS: u32 = 60;

/// Menos que isso não é fala, é um estalo do teclado.
const MENOS_QUE_UM_SILABO: f32 = 0.2;

/// Pico abaixo do qual a gravação é o nada. Um microfone mudo no sistema
/// entrega zeros, e o Whisper não devolve vazio para silêncio: ele inventa
/// "[Música]", que na tela seria uma frase que ninguém falou.
const SEM_SINAL: f32 = 0.002;

/// O fundo do medidor, em dBFS. É a mesma régua dos medidores do Windows: uma
/// fala normal picoa entre 0,05 e 0,3 e, linear, o vidro ficaria quase cheio.
const FUNDO_DO_MEDIDOR: f32 = -50.0;

struct Captura {
    parar: Arc<AtomicBool>,
    /// O áudio mono e a taxa com que foi gravado.
    handle: JoinHandle<(Vec<f32>, u32)>,
}

#[derive(Default)]
struct Estado {
    ouvindo: AtomicBool,
    /// Altura do medidor no último bloco lido, de 0 a 1, como bits de f32. É o
    /// que prova que o microfone está entregando algo.
    nivel: AtomicU32,
    aviso: Mutex<Option<String>>,
}

impl Estado {
    /// Dado puro: um `Option<String>` trocado por atribuição inteira. Um panic
    /// segurando esta trava deixa o aviso de antes ou o de depois, nunca um aviso
    /// partido — e `erro()` e `transcrever()` leem daqui na thread da janela,
    /// onde `expect` fecharia a aplicação em vez de mostrar o motivo.
    fn avisar(&self, texto: Option<String>) {
        *trava_estado(&self.aviso) = texto;
    }
}

/// A altura do medidor para um pico de 0 a 1.
pub(crate) fn nivel_de(pico: f32) -> f32 {
    if pico <= 0.0 {
        0.0
    } else {
        let db = 20.0 * pico.log10();
        ((db - FUNDO_DO_MEDIDOR) / -FUNDO_DO_MEDIDOR).clamp(0.0, 1.0)
    }
}

/// O ouvido do Aether. O reconhecedor fica aberto entre as frases; a captura é
/// de um botão só.
pub struct Escuta {
    sherpa: Arc<Sherpa>,
    estado: Arc<Estado>,
    /// A escuta em curso, sempre escrita inteira: um `Captura` nasce do literal
    /// e sai por `take`. Sob um panic isto é `None` ou o par completo, e as duas
    /// leituras são verdadeiras — por isso a trava recupera com `into_inner`.
    captura: Mutex<Option<Captura>>,
    /// (pasta do modelo, instância)
    ///
    /// Guarda um ponteiro cru do sherpa, e mesmo assim é dado puro: os dois
    /// caminhos que o tocam (`na_cache`, `largar`) primeiro tiram o `Some` com
    /// `take` e só depois chamam `destruir_reconhecedor`. Não existe instrução
    /// entre a trava e o valor que deixe um ponteiro morto dentro do cache, então
    /// o que um panic devolve é `None` ou uma instância viva — nunca uma
    /// instância já destruída, que é o que uma decodificação seguiria usando.
    recon: Mutex<Option<(PathBuf, Opaco)>>,
}

impl Escuta {
    pub fn novo(sherpa: Arc<Sherpa>) -> Escuta {
        Escuta {
            sherpa,
            estado: Arc::new(Estado::default()),
            captura: Mutex::new(None),
            recon: Mutex::new(None),
        }
    }

    /// Liga o microfone. Devolve na hora: o que se ouve fica na thread até
    /// `transcrever` pedir. `entrada` é o nome escolhido na tela; vazio é o
    /// microfone padrão do sistema.
    pub fn ouvir(&self, entrada: String) -> Result<(), String> {
        let mut em_curso = trava_estado(&self.captura);
        if em_curso.is_some() {
            return Err(tr("erros.vozEscutaEmCurso", &[]));
        }
        let estado = Arc::clone(&self.estado);
        let parar = Arc::new(AtomicBool::new(false));
        let do_thread = Arc::clone(&parar);
        let handle = std::thread::Builder::new()
            .name("aether-escuta".to_string())
            .spawn(move || capturar(do_thread, estado, entrada))
            .map_err(|e| tr("erros.vozSemThread", &[("detalhe", &e.to_string())]))?;
        *em_curso = Some(Captura { parar, handle });
        self.estado.avisar(None);
        self.estado.ouvindo.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Desliga o microfone e transcreve o que foi ouvido na modelo de `dir`.
    /// Demora menos que a própria fala: medido, o fator em tempo real do
    /// Whisper base int8 é 0,117.
    pub fn transcrever(&self, dir: &Path, espec: &EspecVoz) -> Result<String, String> {
        let captura = trava_estado(&self.captura)
            .take()
            .ok_or_else(|| tr("erros.vozNaoOuvindo", &[]))?;
        captura.parar.store(true, Ordering::Relaxed);
        let (amostras, taxa) = captura
            .handle
            .join()
            .map_err(|_| tr("erros.vozCapturaNaoTerminou", &[]))?;
        self.estado.ouvindo.store(false, Ordering::Relaxed);
        self.estado.nivel.store(0, Ordering::Relaxed);

        if amostras.len() < (taxa as f32 * MENOS_QUE_UM_SILABO) as usize {
            // O motivo de haver pouco áudio é outro quando o microfone nem
            // abriu: devolve o aviso da thread, não uma desculpa genérica.
            let guard = trava_estado(&self.estado.aviso);
            return Err(guard.clone().unwrap_or_else(|| tr("erros.vozSemFala", &[])));
        }
        if pico(&amostras) < SEM_SINAL {
            // Áudio houve, silêncio foi o que tinha nele.
            return Err(tr("erros.vozSemSinal", &[]));
        }

        self.decodificar(dir, espec, &amostras, taxa)
    }

    /// Lê `amostras`, gravadas a `taxa`, no modelo de escuta de `dir`. É o que a
    /// captura do microfone chama no fim, e é também o caminho de uma prova de
    /// ponta a ponta: o áudio que a fala escreveu em WAV pode ser lido de volta
    /// sem depender de ninguém estar ouvindo.
    pub(crate) fn decodificar(
        &self,
        dir: &Path,
        espec: &EspecVoz,
        amostras: &[f32],
        taxa: u32,
    ) -> Result<String, String> {
        let texto = {
            // A decodificação acontece com o lock da instância tomado: é a mesma
            // que um `largar` destrói, e as duas nunca podem se cruzar.
            let mut cache = trava_estado(&self.recon);
            let opaco = self.na_cache(&mut cache, dir, espec)?;
            // A instância só é destruída sob este mesmo lock, então não há
            // decodificação com ponteiro solto.
            unsafe { self.sherpa.transcrever(opaco, taxa as i32, amostras) }
        };
        texto
            .map(|texto| texto.trim().to_string())
            .ok_or_else(|| tr("erros.vozNaoTranscreveu", &[]))
    }

    /// Solta o reconhecedor desta pasta para que o modelo seja apagado. Se
    /// houver uma decodificação em curso, ela segura o mesmo lock: esperada.
    pub fn largar(&self, dir: &Path) {
        self.descartar();
        let mut cache = trava_estado(&self.recon);
        if cache.as_ref().is_some_and(|(aberta, _)| aberta == dir) {
            if let Some((_, velho)) = cache.take() {
                self.sherpa.destruir_reconhecedor(velho);
            }
        }
    }

    /// Desliga o microfone sem ler nada: é o botão solto em cancelamento. O
    /// áudio vai fora, mas a thread é esperada, para o próximo clique abrir com
    /// o buffer vazio e o estado coerente.
    pub fn descartar(&self) {
        let Some(captura) = trava_estado(&self.captura).take() else {
            return;
        };
        captura.parar.store(true, Ordering::Relaxed);
        let _ = captura.handle.join();
        self.estado.ouvindo.store(false, Ordering::Relaxed);
        self.estado.nivel.store(0, Ordering::Relaxed);
    }

    /// Pega o reconhecedor da pasta, abrindo-o na primeira vez ou quando o
    /// modelo trocou. Recebe o lock já tomado por quem vai decodificar: a
    /// instância morre somente quando ninguém está usando.
    fn na_cache(
        &self,
        cache: &mut Option<(PathBuf, Opaco)>,
        dir: &Path,
        espec: &EspecVoz,
    ) -> Result<Opaco, String> {
        if cache.as_ref().map(|(d, _)| d != dir).unwrap_or(true) {
            if let Some((_, velho)) = cache.take() {
                self.sherpa.destruir_reconhecedor(velho);
            }
            match montar_asr(&self.sherpa, dir, espec) {
                Ok(novo) => *cache = Some((dir.to_path_buf(), novo)),
                Err(e) => return Err(e),
            }
        }
        let Some((_, opaco)) = cache.as_ref() else {
            return Err(tr("erros.vozNaoCriouReconhecedor", &[]));
        };
        Ok(*opaco)
    }

    pub fn ouvindo(&self) -> bool {
        self.estado.ouvindo.load(Ordering::Relaxed)
    }

    pub fn nivel(&self) -> f32 {
        f32::from_bits(self.estado.nivel.load(Ordering::Relaxed))
    }

    pub fn erro(&self) -> Option<String> {
        trava_estado(&self.estado.aviso).clone()
    }
}

/// Drena o microfone até o botão soltar ou o teto de tempo chegar, e devolve o
/// que ouviu junto com a taxa em que ouviu. Roda na thread do botão: nada aqui
/// toca o callback de áudio.
fn capturar(parar: Arc<AtomicBool>, estado: Arc<Estado>, entrada: String) -> (Vec<f32>, u32) {
    let host = cpal::default_host();
    let device = match escolher_entrada(&host, &entrada) {
        Ok(device) => device,
        Err(motivo) => {
            estado.avisar(Some(motivo));
            return (Vec::new(), TAXA_DO_MODELO);
        }
    };
    // O formato que o aparelho entrega, não o que o modelo gostaria de receber:
    // ver o topo deste arquivo.
    let nativo = match device.default_input_config() {
        Ok(nativo) => nativo,
        Err(e) => {
            estado.avisar(Some(tr(
                "erros.vozNaoAbrirEntrada",
                &[("detalhe", &e.to_string())],
            )));
            return (Vec::new(), TAXA_DO_MODELO);
        }
    };
    let canais = nativo.channels().max(1) as usize;
    let taxa = nativo.sample_rate();
    let silencioso = (Vec::new(), taxa);
    let config = StreamConfig {
        channels: canais as u16,
        sample_rate: taxa,
        buffer_size: BufferSize::Default,
    };
    let rb = HeapRb::<f32>::new((taxa as usize).saturating_mul(LIMITE_DE_SEGUNDOS as usize + 2));
    // O produtor desce com o closure: só a thread de áudio escreve nele.
    let (mut prod, mut cons): (HeapProd<f32>, HeapCons<f32>) = rb.split();

    let nivel_estado = Arc::clone(&estado);
    let erro_estado = Arc::clone(&estado);
    let stream = match device.build_input_stream(
        config,
        move |dados: &[f32], _info: &cpal::InputCallbackInfo| {
            // Os quadros interleaved viram uma voz só: o modelo lê um canal.
            let mut pico = 0f32;
            for quadro in dados.chunks_exact(canais) {
                let mono = quadro.iter().sum::<f32>() / canais as f32;
                pico = pico.max(mono.abs());
                let _ = prod.try_push(mono);
            }
            nivel_estado
                .nivel
                .store(nivel_de(pico).to_bits(), Ordering::Relaxed);
        },
        move |err: cpal::Error| {
            // Um microfone arrancado no meio da frase não é um texto vazio: o
            // aviso explica por que a transcrição não chegou.
            erro_estado.avisar(Some(tr(
                "erros.vozStreamInterrompido",
                &[("detalhe", &err.to_string())],
            )));
        },
        None,
    ) {
        Ok(stream) => stream,
        Err(e) => {
            estado.avisar(Some(tr(
                "erros.vozNaoAbrirEntrada",
                &[("detalhe", &e.to_string())],
            )));
            return silencioso;
        }
    };
    if let Err(e) = stream.play() {
        estado.avisar(Some(tr(
            "erros.vozNaoOuvir",
            &[("detalhe", &e.to_string())],
        )));
        return silencioso;
    }

    let inicio = Instant::now();
    let teto = (taxa as usize).saturating_mul(LIMITE_DE_SEGUNDOS as usize);
    let mut coletado: Vec<f32> = Vec::with_capacity(teto.min(1 << 20));
    let mut bloco = vec![0f32; 4096];
    while inicio.elapsed().as_secs() < LIMITE_DE_SEGUNDOS as u64 {
        let lidos = cons.pop_slice(&mut bloco);
        coletado.extend_from_slice(&bloco[..lidos]);
        if parar.load(Ordering::Relaxed) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // O que o callback entregou no último instante ainda está no buffer.
    loop {
        let lidos = cons.pop_slice(&mut bloco);
        if lidos == 0 {
            break;
        }
        coletado.extend_from_slice(&bloco[..lidos]);
    }
    coletado.truncate(teto);
    drop(stream);
    // O microfone fechou aqui, seja pelo *Parar* seja pelo teto de 60 s: o que
    // o medidor mostrou no último bloco não é o nível de agora, é o último que
    // ele viu. Deixar esse valor na tela é o app continuar "gravando" com a
    // barra congelada no meio de uma sílaba.
    estado.nivel.store(0, Ordering::Relaxed);
    (coletado, taxa)
}

/// O microfone escolhido na tela, ou o que o sistema usa quando ninguém escolheu.
/// Uma escolha que sumiu — o aparelho foi desligado no meio do caminho — é dita,
/// não trocada em silêncio por outra que esteja ali.
fn escolher_entrada(host: &Host, entrada: &str) -> Result<cpal::Device, String> {
    if entrada.is_empty() {
        return host
            .default_input_device()
            .ok_or_else(|| tr("erros.vozSemEntrada", &[]));
    }
    let devices = host
        .input_devices()
        .map_err(|e| tr("erros.vozNaoAbrirEntrada", &[("detalhe", &e.to_string())]))?;
    for device in devices {
        let nome = device
            .description()
            .ok()
            .map(|d| d.name().to_owned())
            .unwrap_or_default();
        if nome == entrada {
            return Ok(device);
        }
    }
    Err(tr("erros.vozEntradaSumiu", &[("nome", entrada)]))
}

/// O maior módulo de uma gravação: o que separa fala de silêncio.
fn pico(amostras: &[f32]) -> f32 {
    amostras.iter().fold(0f32, |m, x| m.max(x.abs()))
}

/// Os três arquivos do Whisper, na ordem em que o catálogo os declara: encoder,
/// decoder e tokens. É a ordem do próprio release do sherpa-onnx.
fn trio(espec: &EspecVoz) -> Option<(&'static str, &'static str, &'static str)> {
    match espec.obrigatorios {
        [(encoder, _), (decoder, _), (tokens, _), ..] => Some((*encoder, *decoder, *tokens)),
        _ => None,
    }
}

/// Abre o reconhecedor. Os CStrings vivem enquanto o sherpa copia os caminhos.
fn montar_asr(sherpa: &Sherpa, dir: &Path, espec: &EspecVoz) -> Result<Opaco, String> {
    let Some((encoder, decoder, tokens)) = trio(espec) else {
        return Err(tr(
            "erros.vozFormatoInesperado",
            &[("modelo", espec.id), ("pasta", &dir.display().to_string())],
        ));
    };
    for nome in [encoder, decoder, tokens] {
        if !dir.join(nome).is_file() {
            return Err(tr(
                "erros.vozModeloIncompleto",
                &[("arquivo", nome), ("pasta", &dir.display().to_string())],
            ));
        }
    }
    let caminho = |nome: &str| -> CString {
        CString::new(dir.join(nome).to_string_lossy().replace('\0', ""))
            .expect("um caminho não tem nulo interno")
    };
    let encoder = caminho(encoder);
    let decoder = caminho(decoder);
    let tokens = caminho(tokens);
    // "pt" em vez de deixar o Whisper adivinhar: com poucos segundos de fala a
    // detecção automática erra o idioma e a transcrição vem em outra língua.
    let idioma = CString::new("pt").expect("constante sem nulo");
    let tarefa = CString::new("transcribe").expect("constante sem nulo");
    let metodo = CString::new("greedy_search").expect("constante sem nulo");
    let provedor = CString::new("cpu").expect("constante sem nulo");

    // Zero em todo o resto é o "não usado" dos outros backends deste config.
    let mut config: ConfigReconhecimento = unsafe { std::mem::zeroed() };
    config.feat_config.sample_rate = TAXA_DO_MODELO as i32;
    config.feat_config.feature_dim = DIMENSÃO_DA_FEATURE;
    config.model_config.num_threads = 4;
    config.model_config.provider = provedor.as_ptr();
    config.model_config.tokens = tokens.as_ptr();
    config.model_config.whisper.encoder = encoder.as_ptr();
    config.model_config.whisper.decoder = decoder.as_ptr();
    config.model_config.whisper.language = idioma.as_ptr();
    config.model_config.whisper.task = tarefa.as_ptr();
    // -1 é o cálculo próprio do sherpa para o reboque da cauda do áudio.
    config.model_config.whisper.tail_paddings = -1;
    config.decoding_method = metodo.as_ptr();
    config.max_active_paths = 4;

    sherpa
        .novo_reconhecedor(&config)
        .ok_or_else(|| tr("erros.vozEscutaNaoAbriu", &[("pasta", &dir.display().to_string())]))
}

#[cfg(test)]
mod testes {
    use super::{nivel_de, pico, SEM_SINAL};

    #[test]
    fn o_medidor_mostra_a_fala_na_meia_altura() {
        // Linear, um microfone funcionando quase não mexeria no vidro: medido na
        // captura desta máquina, a fala picoa em 0,09 e é metade da régua.
        assert!((nivel_de(0.09) - 0.58).abs() < 0.01);
        assert_eq!(nivel_de(0.0), 0.0);
        assert_eq!(nivel_de(1.0), 1.0);
        // O que passa do teto do medidor não passa do fim da régua.
        assert_eq!(nivel_de(2.0), 1.0);
        assert_eq!(nivel_de(-1.0), 0.0);
    }

    #[test]
    fn silencio_nao_e_uma_frase_vazia() {
        let zeros = vec![0f32; 48_000];
        assert_eq!(pico(&zeros), 0.0);
        assert!(pico(&zeros) < SEM_SINAL);
        // O fundo de um microfone aberto também não é sinal; uma fala baixa é.
        assert!(pico(&vec![0.0005f32; 1_000]) < SEM_SINAL);
        assert!(pico(&vec![0.02f32; 1_000]) > SEM_SINAL);
    }
}
