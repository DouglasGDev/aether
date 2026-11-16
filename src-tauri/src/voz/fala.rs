//! A fala: o Kokoro sintetiza e o cpal toca.
//!
//! As duas metades correm em ritmos diferentes: gerar uma sentença leva mais
//! tempo que ouvi-la (medido: 8,9 s para 8,1 s de áudio), então esperar o texto
//! inteiro custaria segundos de silêncio antes da primeira sílaba. Em vez disso
//! o sherpa entrega as amostras sentença por sentença num callback, que escreve
//! num buffer sem trava lido pela thread de áudio no ritmo do alto-falante. A
//! voz começa a sair enquanto o resto ainda está sendo gerado.
//!
//! O callback do Windows não pode bloquear nem tomar lock, e o `Stream` do cpal
//! não é `Send`: por isso uma única thread do aplicativo é dona do stream e do
//! modelo, e o que atravessa para ela é só o pedido.

use super::ffi::{ConfigGeracao, ConfigTts, Opaco, Sherpa};
use crate::idiomas::tr;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, Host, Stream, StreamConfig};
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};
use std::ffi::{c_float, c_void, CString};
use std::path::{Path, PathBuf};
use std::ptr::null;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// Threads de inferência do sherpa. É a configuração com que a porta de fala
/// pt-BR foi medida: 8,9 s para sintetizar 8,1 s de áudio.
const FILAMENTOS: i32 = 4;

/// Pausa entre sentenças, no valor padrão do próprio sherpa.
const SILÊNCIO: f32 = 0.2;

/// Quantos segundos de áudio cabem no buffer. A geração é mais lenta que a
/// reprodução, então ele raramente enche; isto é a folga para um pico.
const SEGUNDOS_NO_BUFFER: u32 = 4;

/// O que o sherpa aceita de multiplicador de velocidade sem virar ruído. A
/// tela de voz e o `voz.json` usam a mesma faixa: pedir fora dela é recusado
/// na entrada, não corrigido em segredo.
pub const VELOCIDADE_MÍNIMA: f32 = 0.5;
pub const VELOCIDADE_MÁXIMA: f32 = 2.0;

struct Pedido {
    /// Identificador monotônico do que o callback de áudio pode tocar.
    serial: i64,
    dir: PathBuf,
    texto: String,
    sid: i32,
    velocidade: f32,
    /// O mesmo áudio que vai para o alto-falante, gravado para conferência.
    gravar: Option<PathBuf>,
    /// Pedido de soltar o modelo desta pasta, com o cano do acuse. Uma fala tem
    /// `texto` e nenhum acuse; um `largar` é o contrário. Sem ele, apagar um
    /// modelo poderia reescrever o disco enquanto o sherpa ainda o tem aberto.
    ack: Option<Sender<()>>,
}

/// O que o callback de entrega escreve enquanto o sherpa gera. Vive na pilha da
/// thread do sherpa durante a chamada — exatamente o tempo em que o ponteiro
/// que o sherpa recebe vale.
struct Entregador<'a> {
    prod: &'a mut HeapProd<f32>,
    estado: &'a Estado,
    gravado: Option<&'a mut Vec<f32>>,
}

extern "C" fn entregar(
    amostras: *const c_float,
    n: i32,
    _progresso: f32,
    usuario: *mut c_void,
) -> i32 {
    if usuario.is_null() {
        return 0;
    }
    let e = unsafe { &mut *(usuario as *mut Entregador) };
    if e.estado.abortar.load(Ordering::Relaxed) {
        return 0;
    }
    if amostras.is_null() || n <= 0 {
        return 1;
    }
    // O ponteiro do sherpa só é válido durante esta chamada: tudo é copiado.
    let dados = unsafe { std::slice::from_raw_parts(amostras, n as usize) };
    if let Some(gravado) = &mut e.gravado {
        gravado.extend_from_slice(dados);
    }
    let mut escrito = 0usize;
    while escrito < dados.len() {
        if e.estado.abortar.load(Ordering::Relaxed) {
            return 0;
        }
        escrito += e.prod.push_slice(&dados[escrito..]);
        if escrito < dados.len() {
            // O alto-falante ainda não consumiu: em vez de jogar áudio fora,
            // espera. A geração desacelera junto com a reprodução.
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    1
}

/// Estado vivo da fala, lido pela interface e escrito pelas duas threads.
#[derive(Default)]
struct Estado {
    /// Há áudio saindo: gerado agora ou ainda no buffer.
    falando: AtomicBool,
    /// O sherpa está produzindo neste instante.
    gerando: AtomicBool,
    /// Peça de interrupção: vale para a geração em curso e para o buffer.
    abortar: AtomicBool,
    /// O stream morreu (dispositivo trocado) e precisa ser reaberto.
    recriar: AtomicBool,
    /// Serial do áudio que o callback de áudio tem permissão de tocar.
    tocando: AtomicI64,
    /// Serial máximo derrubado por um `parar`: pedidos atrás dele não falam.
    corte: AtomicI64,
    prox: AtomicI64,
    aviso: Mutex<Option<String>>,
}

impl Estado {
    fn novo() -> Estado {
        Estado {
            prox: AtomicI64::new(1),
            ..Estado::default()
        }
    }

    /// Para de falar agora e esquece o que estava na fila.
    fn parar(&self) {
        self.abortar.store(true, Ordering::Relaxed);
        self.falando.store(false, Ordering::Relaxed);
        self.corte.store(self.prox.load(Ordering::Relaxed), Ordering::Relaxed);
    }

    fn avisar(&self, texto: Option<String>) {
        *self.aviso.lock().expect("aviso de fala envenenado") = texto;
    }
}

/// O stream e a ponta escritora do buffer. O consumidor vai embora dentro do
/// closure: só a thread de áudio pode tocá-lo. O stream nunca é lido — ele
/// existe para viver tanto quanto o contexto, e cair junto com ele.
struct Contexto {
    _stream: Stream,
    prod: HeapProd<f32>,
    taxa: u32,
}

/// A boca do Aether: uma thread só, dona do modelo e do alto-falante.
pub struct Parlante {
    pedidos: Mutex<Sender<Pedido>>,
    estado: Arc<Estado>,
    _thread: JoinHandle<()>,
}

impl Parlante {
    pub fn novo(sherpa: Arc<Sherpa>) -> Result<Parlante, String> {
        let (tx, rx) = std::sync::mpsc::channel::<Pedido>();
        let estado = Arc::new(Estado::novo());
        let thread = std::thread::Builder::new()
            .name("aether-fala".to_string())
            .spawn({
                let estado = Arc::clone(&estado);
                move || administrar(sherpa, rx, estado)
            })
            .map_err(|e| tr("erros.vozSemThread", &[("detalhe", &e.to_string())]))?;
        Ok(Parlante {
            pedidos: Mutex::new(tx),
            estado,
            _thread: thread,
        })
    }

    /// Fala `texto` com a voz `sid` do modelo em `dir`. Devolve assim que o
    /// pedido entra na fila: a fala em si é assíncrona por natureza.
    pub fn dizer(&self, dir: &Path, texto: &str, sid: i32, velocidade: f32) -> Result<(), String> {
        self.enfileirar(dir, texto, sid, velocidade, None)
    }

    /// Como `dizer`, mas grava no WAV de `destino` exatamente o áudio que passou
    /// pelo buffer. É a prova de que a fala do aplicativo existiu.
    pub fn dizer_e_gravar(
        &self,
        dir: &Path,
        texto: &str,
        sid: i32,
        velocidade: f32,
        destino: &Path,
    ) -> Result<(), String> {
        self.enfileirar(dir, texto, sid, velocidade, Some(destino.to_path_buf()))
    }

    fn enfileirar(
        &self,
        dir: &Path,
        texto: &str,
        sid: i32,
        velocidade: f32,
        gravar: Option<PathBuf>,
    ) -> Result<(), String> {
        let texto = limpar_texto(texto);
        if texto.is_empty() {
            return Err(tr("erros.vozSemTexto", &[]));
        }
        let serial = self.estado.prox.fetch_add(1, Ordering::Relaxed);
        self.mandar(Pedido {
            serial,
            dir: dir.to_path_buf(),
            texto,
            sid: sid.max(0),
            velocidade: velocidade.clamp(VELOCIDADE_MÍNIMA, VELOCIDADE_MÁXIMA),
            gravar,
            ack: None,
        })
    }

    /// Solta o modelo desta pasta e espera o acuse da thread que o abriu. É o
    /// que permite apagar o arquivo no Windows: enquanto o sherpa segura a
    /// instância, o disco está ocupado.
    ///
    /// Devolve falso se a thread não respondeu: aí quem chama não apaga nada,
    /// em vez de trocar um arquivo que ainda está em uso.
    pub fn largar(&self, dir: &Path) -> bool {
        self.estado.parar();
        let (tx, rx) = std::sync::mpsc::channel();
        let serial = self.estado.prox.fetch_add(1, Ordering::Relaxed);
        let pedido = Pedido {
            serial,
            dir: dir.to_path_buf(),
            texto: String::new(),
            sid: 0,
            velocidade: 1.0,
            gravar: None,
            ack: Some(tx),
        };
        if self.mandar(pedido).is_err() {
            return false;
        }
        // Trinta segundos é folga para a frase mais longa que o texto de uma
        // resposta costuma formar; o acuse chega assim que ela termina.
        rx.recv_timeout(Duration::from_secs(30)).is_ok()
    }

    fn mandar(&self, pedido: Pedido) -> Result<(), String> {
        self.pedidos
            .lock()
            .expect("fila de fala envenenada")
            .send(pedido)
            .map_err(|_| tr("erros.vozFilaFechada", &[]))
    }

    pub fn parar(&self) {
        self.estado.parar();
    }

    pub fn falando(&self) -> bool {
        self.estado.falando.load(Ordering::Relaxed)
    }

    /// O erro da última tentativa, ou None depois de uma que deu certo. Falar é
    /// assíncrono: um alto-falante ausente só se revela aqui.
    pub fn erro(&self) -> Option<String> {
        self.estado.aviso.lock().expect("aviso de fala envenenado").clone()
    }
}

/// Abre o alto-falante padrão na taxa que o modelo entrega e monta o buffer.
/// Devolve o stream junto com a ponta escritora; a leitora fica no closure.
fn abrir_stream(host: &Host, estado: &Arc<Estado>, taxa: u32) -> Result<Contexto, String> {
    let device = host
        .default_output_device()
        .ok_or_else(|| tr("erros.vozSemSaida", &[]))?;
    let rb = HeapRb::<f32>::new((taxa * SEGUNDOS_NO_BUFFER) as usize);
    let (prod, mut cons): (HeapProd<f32>, HeapCons<f32>) = rb.split();
    let config = StreamConfig {
        channels: 1,
        sample_rate: taxa,
        buffer_size: BufferSize::Default,
    };

    // Serial do áudio que este closure já adotou. Fica fora do closure de
    // propósito: é memória da thread de áudio, e só ela a lê.
    let mut visto = 0i64;
    let escrita = Arc::clone(estado);
    let stream = device
        .build_output_stream(
            config,
            move |dados: &mut [f32], _info: &cpal::OutputCallbackInfo| {
                if escrita.abortar.load(Ordering::Relaxed) {
                    cons.clear();
                    escrita.falando.store(false, Ordering::Relaxed);
                    dados.fill(0.0);
                    return;
                }
                let tocando = escrita.tocando.load(Ordering::Relaxed);
                if tocando != visto {
                    // Começou outra fala: o que sobrou da anterior é descartado.
                    cons.clear();
                    visto = tocando;
                }
                let ouvidos = cons.pop_slice(dados);
                dados[ouvidos..].fill(0.0);
                if ouvidos == 0 && !escrita.gerando.load(Ordering::Relaxed) {
                    // Buffer vazio e geração terminada: acabou de falar.
                    escrita.falando.store(false, Ordering::Relaxed);
                }
            },
            {
                let aviso = Arc::clone(estado);
                move |err: cpal::Error| {
                    // O Windows corta o stream quando o dispositivo padrão muda.
                    // A thread reabre antes da próxima fala; o motivo fica à
                    // mostra para o usuário entender por que a voz parou.
                    aviso.recriar.store(true, Ordering::Relaxed);
                    aviso.falando.store(false, Ordering::Relaxed);
                    aviso.avisar(Some(tr(
                        "erros.vozStreamInterrompido",
                        &[("detalhe", &err.to_string())],
                    )));
                }
            },
            None,
        )
        .map_err(|e| {
            tr(
                "erros.vozNaoAbrirSaida",
                &[("detalhe", &e.to_string()), ("taxa", &taxa.to_string())],
            )
        })?;
    stream
        .play()
        .map_err(|e| tr("erros.vozNaoTocar", &[("detalhe", &e.to_string())]))?;
    Ok(Contexto {
        _stream: stream,
        prod,
        taxa,
    })
}

/// Monta a configuração do Kokoro a partir dos arquivos do modelo. Os CStrings
/// vivem enquanto o sherpa copia os caminhos, ou seja, só aqui dentro.
fn montar_tts(sherpa: &Sherpa, dir: &Path) -> Result<Opaco, String> {
    for nome in [
        "model.int8.onnx",
        "voices.bin",
        "tokens.txt",
        "espeak-ng-data/pt_dict",
    ] {
        if !dir.join(nome).is_file() {
            return Err(tr(
                "erros.vozModeloIncompleto",
                &[("arquivo", nome), ("pasta", &dir.display().to_string())],
            ));
        }
    }
    let caminho = |nome: &str| -> CString {
        CString::new(dir.join(nome).to_string_lossy().replace('\0', ""))
            .expect("um caminho sem nulo interno")
    };
    let modelo = caminho("model.int8.onnx");
    let vozes = caminho("voices.bin");
    let tokens = caminho("tokens.txt");
    let dados = caminho("espeak-ng-data");
    let idioma = CString::new("pt-br").expect("constante sem nulo");
    let provedor = CString::new("cpu").expect("constante sem nulo");

    // Kokoro é um dos muitos backends deste config: o resto precisa chegar
    // zerado para o sherpa entender qual modelo é o usado.
    let mut config: ConfigTts = unsafe { std::mem::zeroed() };
    config.model.num_threads = FILAMENTOS;
    config.model.provider = provedor.as_ptr();
    config.model.kokoro.model = modelo.as_ptr();
    config.model.kokoro.voices = vozes.as_ptr();
    config.model.kokoro.tokens = tokens.as_ptr();
    config.model.kokoro.data_dir = dados.as_ptr();
    config.model.kokoro.lang = idioma.as_ptr();
    config.model.kokoro.length_scale = 1.0;
    config.max_num_sentences = 2;
    config.silence_scale = SILÊNCIO;

    sherpa
        .novo_tts(&config)
        .ok_or_else(|| tr("erros.vozNaoCriou", &[("pasta", &dir.display().to_string())]))
}

fn administrar(sherpa: Arc<Sherpa>, rx: Receiver<Pedido>, estado: Arc<Estado>) {
    let host = cpal::default_host();
    let mut contexto: Option<Contexto> = None;
    // A instância do modelo fica aberta entre as falas: abrí-la leva ~1 s e só
    // troca quando o modelo em disco é outro.
    let mut instancia: Option<Opaco> = None;
    let mut pasta_aberta = PathBuf::new();
    let mut taxa_aberta = 0u32;

    for pedido in rx.iter() {
        // Um `largar` não é fala: ele passa antes do corte, porque soltar o
        // modelo depois de um "parar" continua sendo necessário.
        if let Some(ack) = pedido.ack {
            if pasta_aberta == pedido.dir {
                if let Some(velho) = instancia.take() {
                    sherpa.destruir_tts(velho);
                }
                pasta_aberta = PathBuf::new();
                taxa_aberta = 0;
            }
            let _ = ack.send(());
            continue;
        }
        if pedido.serial <= estado.corte.load(Ordering::Relaxed) {
            continue;
        }

        if instancia.is_none() || pasta_aberta != pedido.dir {
            if let Some(velho) = instancia.take() {
                sherpa.destruir_tts(velho);
            }
            match montar_tts(&sherpa, &pedido.dir) {
                Ok(nova) => {
                    taxa_aberta = sherpa.taxa_de_amostragem(nova) as u32;
                    pasta_aberta = pedido.dir.clone();
                    instancia = Some(nova);
                }
                Err(e) => {
                    estado.avisar(Some(e));
                    continue;
                }
            }
        }
        let Some(opaco) = instancia else { continue };
        let taxa = taxa_aberta;

        let reabrir = contexto
            .as_ref()
            .is_none_or(|c| c.taxa != taxa || estado.recriar.load(Ordering::Relaxed));
        if reabrir {
            // Soltar o stream antigo antes de abrir o novo: o alto-falante é um
            // recurso exclusivo por vez.
            contexto = None;
            match abrir_stream(&host, &estado, taxa) {
                Ok(novo) => contexto = Some(novo),
                Err(e) => {
                    estado.avisar(Some(e));
                    continue;
                }
            }
            estado.recriar.store(false, Ordering::Relaxed);
        }
        let Some(contexto) = contexto.as_mut() else { continue };

        let texto = CString::new(pedido.texto.replace('\0', ""))
            .expect("o texto limpo não tem nulo interno");
        let geracao = ConfigGeracao {
            silence_scale: SILÊNCIO,
            speed: pedido.velocidade,
            sid: pedido.sid,
            reference_audio: null(),
            reference_audio_len: 0,
            reference_sample_rate: 0,
            reference_text: null(),
            num_steps: 0,
            extra: null(),
        };

        let mut gravado: Option<Vec<f32>> = pedido.gravar.as_ref().map(|_| Vec::new());
        // A ordem importa: o serial muda antes de `abortar` cair, para que o
        // callback de áudio limpe o buffer velho assim que puder voltar.
        estado.tocando.store(pedido.serial, Ordering::Relaxed);
        estado.abortar.store(false, Ordering::Relaxed);
        estado.gerando.store(true, Ordering::Relaxed);
        estado.falando.store(true, Ordering::Relaxed);
        estado.avisar(None);

        // O bloco delimita o empréstimo: `envio` só existe enquanto o sherpa
        // pode chamá-lo, e o produtor volta para o contexto quando ele sai.
        let saiu = {
            let mut envio = Entregador {
                prod: &mut contexto.prod,
                estado: &estado,
                gravado: gravado.as_mut(),
            };
            unsafe {
                sherpa.falar(
                    opaco,
                    &texto,
                    &geracao,
                    Some(entregar),
                    &mut envio as *mut Entregador as *mut c_void,
                )
            }
        };
        estado.gerando.store(false, Ordering::Relaxed);

        if !saiu {
            estado.falando.store(false, Ordering::Relaxed);
            let trecho: String = pedido.texto.chars().take(80).collect();
            estado.avisar(Some(tr("erros.vozNaoSintetizou", &[("texto", &trecho)])));
        } else if let (Some(destino), Some(amostras)) = (&pedido.gravar, &gravado) {
            if sherpa.escrever_wav(amostras, taxa as i32, destino) == 0 {
                estado.avisar(Some(tr(
                    "erros.vozFalhaWav",
                    &[("caminho", &destino.display().to_string())],
                )));
            }
        }
    }

    if let Some(velho) = instancia.take() {
        sherpa.destruir_tts(velho);
    }
    // O contexto cai aqui, nesta thread: o stream não pode ser solto de fora.
    drop(contexto);
    estado.falando.store(false, Ordering::Relaxed);
    estado.gerando.store(false, Ordering::Relaxed);
}

/// O texto que o modelo lê. Uma resposta do assistente vem em markdown: cercas
/// de código, ênfase e links não são fala, e ler "asterisco asterisco" seria o
/// oposto de uma voz natural.
pub fn limpar_texto(texto: &str) -> String {
    // Blocos alternados: os de índice par estão fora das cercas de código.
    let sem_cercas: String = texto
        .split("```")
        .enumerate()
        .filter(|(i, _)| i % 2 == 0)
        .map(|(_, parte)| parte)
        .collect();
    let sem_marcas: String = sem_cercas
        .chars()
        .filter(|c| !matches!(c, '`' | '*' | '_' | '#' | '|'))
        .collect();
    let limpo = remover_urls(&sem_marcas)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if limpo.is_empty() {
        return String::new();
    }
    // Sem pontuação final o espeak emenda a frase na próxima e corta a
    // entonação da última palavra.
    if matches!(
        limpo.chars().next_back(),
        Some('.' | '!' | '?' | ':' | ';' | '…')
    ) {
        limpo
    } else {
        format!("{limpo}.")
    }
}

/// `[rótulo](http://url)` vira `rótulo`; o que não tem a forma de um link fica
/// como está.
fn remover_urls(texto: &str) -> String {
    let mut saida = String::with_capacity(texto.len());
    let mut resto = texto.chars().peekable();
    while let Some(c) = resto.next() {
        if c != '[' {
            saida.push(c);
            continue;
        }
        let mut rotulo = String::new();
        let mut fechou = false;
        for d in resto.by_ref() {
            if d == ']' {
                fechou = true;
                break;
            }
            rotulo.push(d);
        }
        if !fechou {
            // Colchetes sem fechamento não são um link: devolve como veio.
            saida.push('[');
            saida.push_str(&rotulo);
            continue;
        }
        if resto.peek() == Some(&'(') {
            resto.next();
            let mut abertos = 1usize;
            for d in resto.by_ref() {
                match d {
                    '(' => abertos += 1,
                    ')' => {
                        abertos -= 1;
                        if abertos == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
            }
        }
        saida.push_str(&rotulo);
    }
    saida
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn cercas_de_codigo_nao_sao_faladas() {
        let limpo = limpar_texto("Rodo o teste:\n```rust\nfn main() {}\n```\nPronto.");
        assert_eq!(limpo, "Rodo o teste: Pronto.");
    }

    #[test]
    fn o_rotulo_de_um_link_fica_e_a_url_sai() {
        assert_eq!(
            limpar_texto("Veja [a documentação](https://example.com/aether)."),
            "Veja a documentação."
        );
    }

    #[test]
    fn enfase_e_tabela_desaparecem() {
        assert_eq!(limpar_texto("**oi** _tudo_ | bem |"), "oi tudo bem.");
    }

    #[test]
    fn a_prosa_acabada_em_ponto_nao_ganha_nada() {
        assert_eq!(limpar_texto("Já terminei."), "Já terminei.");
        assert_eq!(limpar_texto("Só uma pergunta?"), "Só uma pergunta?");
    }

    #[test]
    fn texto_que_nao_e_fala_devolve_vazio() {
        assert!(limpar_texto("   \n  ").is_empty());
        assert!(limpar_texto("```\ncódigo\n```").is_empty());
    }

    #[test]
    fn colchetes_sem_fechamento_continam_no_texto() {
        assert_eq!(limpar_texto("item [opcional"), "item [opcional.");
    }
}
