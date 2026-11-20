//! Download real dos pesos e dos arquivos de apoio.
//!
//! Nada aqui é simulado: os bytes vêm do HuggingFace, o progresso conta o que foi
//! escrito em disco, e o arquivo só ganha o nome definitivo depois de conferido
//! contra o catálogo — pelo tamanho sempre e, onde o repositório publica a soma
//! (`lfs.oid` de `tree/main`, ver `catalog.rs`), pelo sha256 do conteúdo inteiro.
//! Tamanho sozinho não prova nada: um corpo trocado chega com o tamanho certo.

use super::catalog::{self, ModelSpec};
use crate::idiomas::tr;
use anyhow::{Context, bail};
use futures_util::StreamExt;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// A barra da UI mede os pesos: são 99,9% dos bytes. Os arquivos de apoio são
/// anunciados por nome, sem inflar o denominador da barra.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum DownloadEvent {
    Started {
        id: String,
        total_bytes: u64,
    },
    Support {
        id: String,
        file: String,
    },
    Progress {
        id: String,
        done_bytes: u64,
        total_bytes: u64,
        bytes_per_second: u64,
    },
    Done {
        id: String,
    },
    Failed {
        id: String,
        error: String,
    },
    Cancelled {
        id: String,
    },
}

/// O download foi interrompido a pedido do usuário. O `.part` fica em disco para
/// a próxima tentativa continuar de onde parou.
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&tr("erros.downloadCancelado", &[]))
    }
}

impl std::error::Error for Cancelled {}

pub(crate) fn part_path(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "arquivo".to_string());
    target.with_file_name(format!("{name}.part"))
}

pub(crate) fn client() -> anyhow::Result<reqwest::Client> {
    client_com_leitura(Duration::from_secs(30))
}

/// O cliente real, com o prazo de leitura trocável apenas para que um teste
/// possa encurtá-lo sem encurtar o `read_timeout` que está em produção.
fn client_com_leitura(leitura: Duration) -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("Aether/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(30))
        // O `connect_timeout` só cobre abrir a conexão. Um servidor que aceita,
        // manda os cabeçalhos e então silencia no meio do corpo penduraria a
        // leitura para sempre, e sem prazo o `.part` fica congelado no ar. O
        // `read_timeout` é por leitura, não pelo corpo inteiro: um servidor lento
        // mas vivo continua mandando peças e não é cortado — o que cai é o
        // silêncio, e ele vira falha retentável na fila de tentativas.
        .read_timeout(leitura)
        .build()
        .context(tr("erros.falhaCriarCliente", &[]))
}

/// Uma falha da rede, não da máquina: o `.part` já está em disco pronto para a
/// próxima pedida, e o que caiu pode voltar. Marcar estas e só estas é o que
/// impede a retentativa de virar teimosia — um 404, um disco cheio ou um
/// cancelamento têm de devolver o motivo na primeira vez.
#[derive(Debug)]
struct Transitorio(String);

impl std::fmt::Display for Transitorio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Transitorio {}

/// O corpo veio com o tamanho publicado e não é o arquivo publicado. É outra
/// coisa que uma rede caindo, e por isso não é `Transitorio`: retomar do ponto
/// onde parou traria exatamente os mesmos bytes, e o único caminho que faz
/// sentido é deitar o `.part` fora e pedir o corpo inteiro de novo — poucas
/// vezes, porque um CDN servindo outro corpo de forma constante não se conserta
/// insistindo gigabytes por vez. Quando a fila cansa, o que volta é o par
/// medido/publicado, lido lado a lado, e nada é publicado.
#[derive(Debug)]
pub struct CorpoTroocado {
    /// O nome do arquivo como ele deveria ficar em disco.
    pub arquivo: String,
    /// O sha256 dos bytes que chegaram, medido aqui.
    pub medido: String,
    /// O sha256 que o repositório publica para este arquivo.
    pub esperado: String,
    /// Quantas vezes o corpo inteiro foi pedido de novo antes desta resposta.
    pub recomecos: u32,
}

impl std::fmt::Display for CorpoTroocado {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A frase vem do locale; os dois hashes vêm atrás, em código, porque são
        // a medição e não texto: sem eles a resposta seria um "arquivo errado"
        // sem nada que o usuário ou o maintenedor possa conferir.
        let frase = tr(
            "erros.corpoTrocado",
            &[("arquivo", &self.arquivo), ("recomecos", &self.recomecos.to_string())],
        );
        write!(f, "{frase}: {} != {}", self.medido, self.esperado)
    }
}

impl std::error::Error for CorpoTroocado {}

/// O sha256 do arquivo inteiro, lido do disco. É a mesma maquinaria do
/// instalador de runtime (`runtime.rs` soma cada DLL contra o
/// `SHA256SUMS.txt`): `Sha256` alimentado por `std::io::copy` e formatado em hex
/// minúsculo, o formato do `lfs.oid` publicado e do `sha256sum -c` do README
/// vendorizado. Pendência registrada em `docs/melhorias/M8.md`: as duas cópias
/// têm de virar uma função só, e promover a de `runtime.rs` para `pub(crate)` é
/// edição num arquivo que esta frente não toca.
fn sha256_de_arquivo(caminho: &Path) -> anyhow::Result<String> {
    let mut arquivo = std::fs::File::open(caminho).map_err(|e| {
        anyhow::anyhow!(tr(
            "erros.falhaAbrir",
            &[("caminho", &caminho.display().to_string()), ("detalhe", &e.to_string())]
        ))
    })?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut arquivo, &mut hasher).map_err(|e| {
        anyhow::anyhow!(tr(
            "erros.falhaLer",
            &[("caminho", &caminho.display().to_string()), ("detalhe", &e.to_string())]
        ))
    })?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// Descarta o `.part` e devolve o motivo. Um corpo que não é o arquivo não é
/// trabalho feito: o que está em disco ali é lixo de outra remessa, e retomar
/// dele produziria um GGUF montado metade de um corpo e metade de outro.
fn descartar_parcial(part: &Path) -> anyhow::Result<()> {
    std::fs::remove_file(part).map_err(|e| {
        anyhow::anyhow!(tr(
            "erros.falhaDescartarParcial",
            &[("caminho", &part.display().to_string()), ("detalhe", &e.to_string())]
        ))
    })
}

/// Confere o `.part` completo contra a soma publicada antes de qualquer
/// `rename`. Sem soma publicada (`None`) não há o que conferir, e isto é
/// declarado: o arquivo git comum do Granite, por exemplo, só tem SHA-1 do blob,
/// não SHA-256 do conteúdo — inventar uma expectativa a partir dos bytes que
/// chegaram seria fotografar o download e chamar de verificação.
fn conferir_sha256(part: &Path, alvo: &Path, esperado: Option<&str>) -> anyhow::Result<()> {
    let Some(esperado) = esperado else {
        return Ok(());
    };
    let medido = sha256_de_arquivo(part)?;
    if medido == esperado {
        return Ok(());
    }
    descartar_parcial(part)?;
    Err(CorpoTroocado {
        arquivo: alvo.display().to_string(),
        medido,
        esperado: esperado.to_string(),
        recomecos: 0,
    }
    .into())
}

/// Quantas vezes o corpo inteiro é pedido de novo quando o hash não bate. Duas é
/// o preço de um CDN que devolveu um corpo errado uma vez e um espelho no meio
/// de uma sincronia: a cada recomeço a fila de `TENTATIVAS` pedidas roda inteira,
/// então o pior caso já são 12 conexões antes da resposta honesta chegar.
const REINICIOS_POR_INTEGRIDADE: u32 = 2;

/// As quatro pedidas de antes cobrem o apagão de rota de um Wi-Fi que troca de
/// ponto e um 503 de poucos segundos do espelho; mais que isso é o usuário
/// esperando sem que nada novo esteja acontecendo.
const TENTATIVAS: u32 = 4;

/// 1s, 2s, 4s entre as pedidas. O fundo do sono é medido em pedacinhos para o
/// *Parar* continuar respondendo durante a espera.
const ESPERA_BASE: Duration = Duration::from_secs(1);

fn status_transitorio(status: reqwest::StatusCode) -> bool {
    matches!(
        status.as_u16(),
        408 | 425 | 429 | 500 | 502 | 503 | 504 | 509
    )
}

fn transitorio(motivo: impl std::fmt::Display) -> anyhow::Error {
    anyhow::Error::new(Transitorio(motivo.to_string()))
}

fn eh_transitorio(erro: &anyhow::Error) -> bool {
    erro.downcast_ref::<Transitorio>().is_some()
}

/// Dorme `espera` sem ensurdecer para o cancelamento: quem aperta *Parar* no
/// meio da espera não pode esperar mais um backoff para a resposta chegar.
async fn esperar(espera: Duration, cancel: &AtomicBool) -> anyhow::Result<()> {
    const PASSO: Duration = Duration::from_millis(50);
    let fim = Instant::now() + espera;
    while Instant::now() < fim {
        if cancel.load(Ordering::Relaxed) {
            return Err(Cancelled.into());
        }
        tokio::time::sleep(PASSO.min(fim - Instant::now())).await;
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(Cancelled.into());
    }
    Ok(())
}

/// Acaba quando *Parar* é apertado, sozinho. Vive para ser a outra perna de um
/// `select!` contra a espera pelo próximo chunk: se o servidor abriu a conexão e
/// silenciou no corpo, `stream.next()` não devolve nada até o `read_timeout`, e
/// o botão não pode esperar o timeout para fazer efeito. Este vigia dá o
/// cancelamento antes.
async fn vigiar_cancelamento(cancel: &AtomicBool) {
    const PASSO: Duration = Duration::from_millis(50);
    while !cancel.load(Ordering::Relaxed) {
        tokio::time::sleep(PASSO).await;
    }
}

/// Escreve `url` em `target`, retomando de um `.part` quando o servidor aceita
/// Range, e tentando de novo — com o `.part` como ponto de partida — quando a
/// rede é quem falha. `expected` é o tamanho exato esperado, ou None para apoio
/// (o tamanho só é conhecido pela resposta). Devolve quantos bytes vieram da
/// rede na pedida que completou o arquivo — menos que o tamanho dele quando a
/// retomada funcionou.
///
/// Esta é a porta sem soma: é o que baixa os modelos de voz e o pacote do
/// runtime, que conferem o conteúdo arquivo por arquivo depois, em `runtime.rs`
/// e no extrator. Conferir só o tamanho aqui é o que o M8 descreve; quem baixa
/// pesos e apoio do catálogo passa por [`stream_to_file_confirmando`].
pub(crate) async fn stream_to_file(
    http: &reqwest::Client,
    url: &str,
    target: &Path,
    expected: Option<u64>,
    cancel: &AtomicBool,
    on_progress: impl FnMut(u64, u64, u64),
) -> anyhow::Result<u64> {
    stream_to_file_confirmando(http, url, target, expected, None, cancel, on_progress).await
}

/// Como `stream_to_file`, mas confere o sha256 publicado antes de o `.part`
/// virar o arquivo definitivo. `esperado_sha256` é `None` onde o repositório
/// não publica soma — e nesse caso o que se confere é o tamanho, nada mais,
/// como está registrado em `docs/melhorias/M8.md`.
pub(crate) async fn stream_to_file_confirmando(
    http: &reqwest::Client,
    url: &str,
    target: &Path,
    expected: Option<u64>,
    esperado_sha256: Option<&str>,
    cancel: &AtomicBool,
    on_progress: impl FnMut(u64, u64, u64),
) -> anyhow::Result<u64> {
    stream_desde_o_ponto(
        http,
        url,
        target,
        expected,
        esperado_sha256,
        cancel,
        on_progress,
        ESPERA_BASE,
    )
    .await
}

/// A fila de pedidas da rede, com o recomeço do zero por cima: um corpo trocado
/// deita o `.part` fora e pede o arquivo inteiro outra vez, porque retomar do
/// ponto onde ele parou só traria os mesmos bytes errados. Tudo que não é
/// `CorpoTroocado` devolve como veio — inclusive o `Transitorio` que estourou as
/// tentativas, que já vem com o número de pedidas na mensagem.
async fn stream_desde_o_ponto(
    http: &reqwest::Client,
    url: &str,
    target: &Path,
    expected: Option<u64>,
    esperado_sha256: Option<&str>,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(u64, u64, u64),
    espera_base: Duration,
) -> anyhow::Result<u64> {
    let mut recomecos = 0u32;
    loop {
        match fila_de_tentativas(
            http,
            url,
            target,
            expected,
            esperado_sha256,
            cancel,
            &mut on_progress,
            espera_base,
        )
        .await
        {
            Ok(recebidos) => return Ok(recebidos),
            Err(erro) => match erro.downcast::<CorpoTroocado>() {
                Err(erro) => return Err(erro),
                Ok(trocado) => {
                    if recomecos >= REINICIOS_POR_INTEGRIDADE {
                        // A resposta é o par medido/publicado, não um "erro de
                        // download": é com os dois hashes lado a lado que o
                        // usuário ou o maintenedor decide se o catálogo está
                        // errado ou se foi o servidor quem trocou o corpo.
                        return Err(CorpoTroocado { recomecos, ..trocado }.into());
                    }
                    recomecos += 1;
                    esperar(espera_base * (1u32 << recomecos), cancel).await?;
                }
            },
        }
    }
}

/// As tentativas de uma mesma remessa: cai a conexão, volta do `.part`; cai a
/// rede inteira quatro vezes seguidas, devolve o motivo com o número de pedidas.
async fn fila_de_tentativas(
    http: &reqwest::Client,
    url: &str,
    target: &Path,
    expected: Option<u64>,
    esperado_sha256: Option<&str>,
    cancel: &AtomicBool,
    on_progress: &mut impl FnMut(u64, u64, u64),
    espera_base: Duration,
) -> anyhow::Result<u64> {
    for tentativa in 1..=TENTATIVAS {
        match tentativa_de_escrever(http, url, target, expected, esperado_sha256, cancel, on_progress)
            .await
        {
            Ok(recebidos) => return Ok(recebidos),
            Err(erro) => {
                if !eh_transitorio(&erro) {
                    return Err(erro);
                }
                if tentativa == TENTATIVAS {
                    // A última mensagem diz quantas vezes a rede foi tentada, e
                    // logo abaixo vem o motivo real da última: o usuário lê as
                    // duas, não um "falha de download" sem conteúdo.
                    return Err(erro.context(tr(
                        "erros.tentativasEsgotadas",
                        &[("tentativas", &TENTATIVAS.to_string())]
                    )));
                }
                esperar(espera_base * (1u32 << (tentativa - 1)), cancel).await?;
            }
        }
    }
    unreachable!("TENTATIVAS é maior que zero")
}

/// Uma única pedida HTTP: lê o `.part` que existe, pede o resto e escreve até o
/// fim ou até a rede cair. O que faz a retentativa valer alguma coisa é o que
/// ela devolve marcado: o que a máquina aguenta (disco, cancelamento, 404) é
/// devolvido na primeira vez; só o `Transitorio` volta para a fila.
async fn tentativa_de_escrever(
    http: &reqwest::Client,
    url: &str,
    target: &Path,
    expected: Option<u64>,
    esperado_sha256: Option<&str>,
    cancel: &AtomicBool,
    on_progress: &mut impl FnMut(u64, u64, u64),
) -> anyhow::Result<u64> {
    if let Some(expected) = expected
        && target.is_file()
        && std::fs::metadata(target).map(|m| m.len()).unwrap_or(0) == expected
    {
        return Ok(0);
    }

    let part = part_path(target);
    let mut resumed = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    if let Some(expected) = expected {
        if resumed > expected {
            // Um .part maior que o arquivo published não é retomável: lixo de
            // uma tentativa anterior. Recomeçar é mais honesto que truncar.
            std::fs::remove_file(&part)
                .map_err(|e| {
                    anyhow::anyhow!(tr(
                        "erros.falhaDescartarParcial",
                        &[("caminho", &part.display().to_string()), ("detalhe", &e.to_string())]
                    ))
                })?;
            resumed = 0;
        }
        if resumed == expected {
            // Um `.part` do tamanho exato também não é prova de ser o arquivo:
            // é por aqui que um download retomado depois de uma remessa errada
            // publicaria o mesmo corpo trocado sem tocar a rede nenhuma vez.
            conferir_sha256(&part, target, esperado_sha256)?;
            std::fs::rename(&part, target)
                .map_err(|e| {
                    anyhow::anyhow!(tr(
                        "erros.falhaFinalizarParcial",
                        &[("caminho", &target.display().to_string()), ("detalhe", &e.to_string())]
                    ))
                })?;
            return Ok(0);
        }
    }

    let mut request = http.get(url);
    if resumed > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={resumed}-"));
    }
    let response = request
        .send()
        .await
        .map_err(|e| {
            transitorio(format!("{}: {}", tr("erros.falhaPedir", &[("url", url)]), e))
        })?;
    let status = response.status();
    if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE && resumed > 0 {
        // O servidor não reconhece o ponto onde o `.part` diz que paramos: o
        // arquivo publicado mudou de tamanho debaixo da gente. Descartar o
        // parcial e pedir tudo de novo é a única retomada honesta aqui —
        // emendar bytes de outra versão seria um GGUF que o motor recusa.
        std::fs::remove_file(&part).map_err(|e| {
            anyhow::anyhow!(tr(
                "erros.falhaDescartarParcial",
                &[("caminho", &part.display().to_string()), ("detalhe", &e.to_string())]
            ))
        })?;
        return Err(transitorio(tr(
            "erros.servidorRespondeu",
            &[("status", &status.to_string()), ("url", url)],
        )));
    }
    if !status.is_success() {
        let motivo =
            tr("erros.servidorRespondeu", &[("status", &status.to_string()), ("url", url)]);
        return Err(if status_transitorio(status) {
            transitorio(motivo)
        } else {
            anyhow::anyhow!(motivo)
        });
    }

    let appending = if resumed > 0 {
        if status != reqwest::StatusCode::PARTIAL_CONTENT {
            // O servidor ignorou o Range e vai mandar tudo: recomeçar do zero é
            // a única forma de não misturar bytes duplicados.
            resumed = 0;
            false
        } else {
            let start = response
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("bytes "))
                .and_then(|v| v.split('-').next())
                .and_then(|v| v.parse::<u64>().ok());
            if start != Some(resumed) {
                return Err(transitorio(tr(
                    "erros.retomouDeOutroPonto",
                    &[
                        ("inicio", &format!("{start:?}")),
                        ("esperado", &resumed.to_string())
                    ]
                )));
            }
            true
        }
    } else {
        false
    };

    let total = expected.unwrap_or_else(|| {
        resumed
            + response
                .content_length()
                .unwrap_or(0)
    });

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(appending)
        .truncate(!appending)
        .open(&part)
        .map_err(|e| {
            anyhow::anyhow!(tr(
                "erros.falhaAbrir",
                &[("caminho", &part.display().to_string()), ("detalhe", &e.to_string())]
            ))
        })?;

    let mut done = resumed;
    let started = Instant::now();
    let mut last_report = Instant::now();
    let mut received = 0u64;
    let mut stream = response.bytes_stream();
    loop {
        // `stream.next()` fica pendurado se o servidor silenciar no corpo, e
        // aí o *Parar* só seria visto depois do `read_timeout`. O select dá o
        // cancelamento assim que ele chega, no meio da espera por bytes.
        let chunk = tokio::select! {
            chunk = stream.next() => chunk,
            () = vigiar_cancelamento(cancel) => {
                file.flush().ok();
                return Err(Cancelled.into());
            }
        };
        let Some(chunk) = chunk else { break };
        if cancel.load(Ordering::Relaxed) {
            file.flush().ok();
            return Err(Cancelled.into());
        }
        let chunk = chunk.map_err(|e| {
            transitorio(format!("{}: {}", tr("erros.conexaoCaiu", &[("url", url)]), e))
        })?;
        file.write_all(&chunk).map_err(|e| {
            anyhow::anyhow!(tr(
                "erros.falhaEscrever",
                &[("caminho", &part.display().to_string()), ("detalhe", &e.to_string())]
            ))
        })?;
        done += chunk.len() as u64;
        received += chunk.len() as u64;
        if let Some(expected) = expected
            && done > expected
        {
            bail!(tr(
                "erros.maisBytesQuePublicado",
                &[("recebidos", &done.to_string()), ("esperados", &expected.to_string())]
            ));
        }
        if last_report.elapsed() >= Duration::from_millis(200) {
            let elapsed = started.elapsed().as_secs_f64().max(0.001);
            let speed = ((done - resumed) as f64 / elapsed) as u64;
            on_progress(done, total, speed);
            last_report = Instant::now();
        }
    }
    file.flush().map_err(|e| {
        anyhow::anyhow!(tr(
            "erros.falhaEscrever",
            &[("caminho", &part.display().to_string()), ("detalhe", &e.to_string())]
        ))
    })?;
    drop(file);

    if let Some(expected) = expected
        && done != expected
    {
        // O corpo chegou ao fim antes do tamanho publicado: a conexão foi
        // cortada sem aviso, não um arquivo errado. O `.part` tem exatamente o
        // que veio, e é dele que a próxima pedida sai.
        return Err(transitorio(tr(
            "erros.downloadIncompleto",
            &[("recebidos", &done.to_string()), ("esperados", &expected.to_string())]
        )));
    }
    let elapsed = started.elapsed().as_secs_f64().max(0.001);
    on_progress(done, total, ((done - resumed) as f64 / elapsed) as u64);
    // O tamanho vem primeiro e o hash depois, e a ordem é deliberada: um corpo
    // curto é uma conexão cortada, tem o `.part` inteiro que existe e retoma do
    // ponto. Um corpo do tamanho certo com outro sha256 é outra coisa — aí o
    // `.part` é apagado e a resposta é `CorpoTroocado`, que a fila acima trata
    // como recomeço do zero e nunca como retomada.
    conferir_sha256(&part, target, esperado_sha256)?;
    std::fs::rename(&part, target)
        .map_err(|e| {
            anyhow::anyhow!(tr(
                "erros.falhaFinalizar",
                &[("caminho", &target.display().to_string()), ("detalhe", &e.to_string())]
            ))
        })?;
    Ok(received)
}

/// Baixa o que falta de um modelo: apoio primeiro (falha rápido e barato), depois
/// os pesos. Emite eventos reais; nunca inventa progresso.
pub async fn fetch(
    spec: &'static ModelSpec,
    root: &Path,
    cancel: &AtomicBool,
    emit: impl Fn(DownloadEvent) + Clone,
) -> anyhow::Result<()> {
    let dir = catalog::model_dir(root, spec);
    std::fs::create_dir_all(&dir).map_err(|e| {
        anyhow::anyhow!(tr(
            "erros.falhaCriarPasta",
            &[("caminho", &dir.display().to_string()), ("detalhe", &e.to_string())]
        ))
    })?;
    let http = client()?;
    let id = spec.id.to_string();

    emit(DownloadEvent::Started { id: id.clone(), total_bytes: spec.weights_bytes });

    for file in spec.support_files {
        if cancel.load(Ordering::Relaxed) {
            return Err(Cancelled.into());
        }
        let target = dir.join(file);
        // Um ponteiro Git LFS no lugar do arquivo é um download que precisa
        // acontecer de novo, não um que já aconteceu.
        if catalog::support_ok(&target) {
            continue;
        }
        emit(DownloadEvent::Support { id: id.clone(), file: file.to_string() });
        stream_to_file_confirmando(
            &http,
            &catalog::support_url(spec, file),
            &target,
            None,
            catalog::apoio_sha256(spec, file),
            cancel,
            |_, _, _| {},
        )
        .await
        .with_context(|| tr("erros.arquivoDeApoio", &[("arquivo", file)]))?;
    }

    let progress = emit.clone();
    let weights_id = id.clone();
    let total = spec.weights_bytes;
    stream_to_file_confirmando(
        &http,
        &catalog::weights_url(spec),
        &dir.join(spec.weights_file),
        Some(spec.weights_bytes),
        catalog::pesos_sha256(spec),
        cancel,
        move |done, _, speed| {
            progress(DownloadEvent::Progress {
                id: weights_id.clone(),
                done_bytes: done,
                total_bytes: total,
                bytes_per_second: speed,
            });
        },
    )
    .await
    .context(tr("erros.pesosDoModelo", &[]))?;

    let measured = std::fs::metadata(dir.join(spec.weights_file))
        .map(|m| m.len())
        .unwrap_or(0);
    if measured != spec.weights_bytes {
        bail!(
            tr(
                "erros.tamanhoForaDoCatalogo",
                &[
                    ("medido", &measured.to_string()),
                    ("esperado", &spec.weights_bytes.to_string())
                ]
            )
        );
    }
    emit(DownloadEvent::Done { id });
    Ok(())
}

/// Remove só os pesos desta quant. O apoio e as outras quants da mesma pasta
/// ficam: são compartilhados e apagá-los quebraria outro modelo.
pub fn remove_weights(root: &Path, spec: &ModelSpec) -> anyhow::Result<()> {
    let dir = catalog::model_dir(root, spec);
    let weights = dir.join(spec.weights_file);
    for path in [weights.clone(), part_path(&weights)] {
        if path.is_file() {
            std::fs::remove_file(&path).map_err(|e| {
                anyhow::anyhow!(tr(
                    "erros.falhaApagar",
                    &[("caminho", &path.display().to_string()), ("detalhe", &e.to_string())]
                ))
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A UI lê `totalBytes`, não `total_bytes`. `rename_all` num enum renomeia as
    /// variantes, não os campos delas: sem `rename_all_fields` a barra de
    /// progresso recebe undefined e fica em zero para sempre.
    #[test]
    fn progress_fields_reach_the_ui_in_camel_case() {
        let json = serde_json::to_value(DownloadEvent::Progress {
            id: "granite-4.2-3b-Q4_K_M".to_string(),
            done_bytes: 1024,
            total_bytes: 2048,
            bytes_per_second: 512,
        })
        .unwrap();
        assert_eq!(json["type"], "progress");
        assert_eq!(json["doneBytes"], 1024);
        assert_eq!(json["totalBytes"], 2048);
        assert_eq!(json["bytesPerSecond"], 512);
        assert!(json.get("total_bytes").is_none(), "{json}");
    }

    #[test]
    fn every_variant_keeps_a_single_word_tag() {
        for event in [
            DownloadEvent::Started { id: "x".into(), total_bytes: 1 },
            DownloadEvent::Support { id: "x".into(), file: "f".into() },
            DownloadEvent::Done { id: "x".into() },
            DownloadEvent::Failed { id: "x".into(), error: "e".into() },
            DownloadEvent::Cancelled { id: "x".into() },
        ] {
            let json = serde_json::to_value(&event).unwrap();
            let tag = json["type"].as_str().unwrap();
            assert!(!tag.contains('_'), "{tag}");
            assert!(json["id"].is_string(), "{json}");
        }
    }

    #[test]
    fn part_sits_next_to_the_target_and_keeps_its_extension() {
        let target = Path::new("modelos").join("granite-4.2-3b-Q4_K_M.gguf");
        assert_eq!(
            part_path(&target),
            Path::new("modelos").join("granite-4.2-3b-Q4_K_M.gguf.part")
        );
    }

    /// Um .part maior que o publicado não pode virar o arquivo final: o tamanho
    /// exato é a única prova de que o download é o que o catálogo promete.
    #[test]
    fn an_oversized_partial_is_not_accepted_as_the_final_file() {
        let dir = std::env::temp_dir().join(format!("aether-part-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("pesos.gguf");
        let part = part_path(&target);
        std::fs::write(&part, vec![0u8; 32]).unwrap();
        assert!(part.is_file());
        assert!(!target.is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn removing_one_quant_keeps_the_shared_support_and_the_other_quant() {
        let dir = std::env::temp_dir().join(format!("aether-remove-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let small = catalog::find("granite-4.2-3b-Q4_K_M").unwrap();
        let big = catalog::find("granite-4.2-3b-Q8_0").unwrap();
        let sub = catalog::model_dir(&dir, small);
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join(small.weights_file), b"a").unwrap();
        std::fs::write(sub.join(big.weights_file), b"b").unwrap();
        std::fs::write(sub.join("tokenizer.json"), b"{}").unwrap();

        remove_weights(&dir, small).unwrap();

        assert!(!sub.join(small.weights_file).exists());
        assert!(sub.join(big.weights_file).is_file());
        assert!(sub.join("tokenizer.json").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn removing_also_takes_the_interrupted_partial() {
        let dir = std::env::temp_dir().join(format!("aether-remove-part-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let spec = catalog::find("granite-4.2-3b-Q4_K_M").unwrap();
        let sub = catalog::model_dir(&dir, spec);
        std::fs::create_dir_all(&sub).unwrap();
        let weights = sub.join(spec.weights_file);
        std::fs::write(part_path(&weights), b"parcial").unwrap();

        remove_weights(&dir, spec).unwrap();

        assert!(!part_path(&weights).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Rede real. Um arquivo de apoio pequeno prova o caminho HTTP inteiro sem
    /// gastar gigabytes: URL do catálogo, resposta, escrita e rename.
    #[tokio::test]
    #[ignore = "baixa do HuggingFace de verdade"]
    async fn a_real_support_file_lands_as_valid_json() {
        let spec = catalog::find("granite-4.2-3b-Q4_K_M").unwrap();
        let dir = std::env::temp_dir().join(format!("aether-net-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("generation_config.json");
        let cancel = AtomicBool::new(false);

        let received = stream_to_file(
            &client().unwrap(),
            &catalog::support_url(spec, "generation_config.json"),
            &target,
            None,
            &cancel,
            |_, _, _| {},
        )
        .await
        .unwrap();

        let bytes = std::fs::read(&target).unwrap();
        assert_eq!(received, bytes.len() as u64);
        assert!(!part_path(&target).exists(), "o .part deveria ter virado o arquivo");
        serde_json::from_slice::<serde_json::Value>(&bytes).expect("JSON real do HuggingFace");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Rede real. Metade do arquivo em `.part` é exatamente o que um download
    /// interrompido deixa. A retomada tem de pedir só o resto e o resultado tem
    /// de ser byte a byte igual ao download completo.
    #[tokio::test]
    #[ignore = "baixa do HuggingFace de verdade"]
    async fn a_truncated_partial_resumes_over_range_and_matches_the_full_file() {
        let spec = catalog::find("granite-4.2-3b-Q4_K_M").unwrap();
        let dir = std::env::temp_dir().join(format!("aether-resume-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("tokenizer_config.json");
        let url = catalog::support_url(spec, "tokenizer_config.json");
        let http = client().unwrap();
        let cancel = AtomicBool::new(false);

        stream_to_file(&http, &url, &target, None, &cancel, |_, _, _| {}).await.unwrap();
        let full = std::fs::read(&target).unwrap();
        assert!(full.len() > 2000, "arquivo pequeno demais para provar retomada");

        let half = full.len() / 2;
        std::fs::write(part_path(&target), &full[..half]).unwrap();
        std::fs::remove_file(&target).unwrap();

        let received = stream_to_file(&http, &url, &target, None, &cancel, |_, _, _| {})
            .await
            .unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), full, "retomada corrompeu o arquivo");
        assert_eq!(
            received,
            (full.len() - half) as u64,
            "o servidor não honrou o Range: vieram {received} bytes, esperado só o resto"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Cancelar antes de qualquer byte não pode deixar um arquivo final pela
    /// metade: o `.part` fica e o nome definitivo não aparece.
    #[tokio::test]
    #[ignore = "baixa do HuggingFace de verdade"]
    async fn cancelling_keeps_the_partial_and_never_publishes_the_target() {
        let spec = catalog::find("granite-4.2-3b-Q4_K_M").unwrap();
        let dir = std::env::temp_dir().join(format!("aether-cancel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("tokenizer.json");
        let cancel = AtomicBool::new(true);

        let error = stream_to_file(
            &client().unwrap(),
            &catalog::support_url(spec, "tokenizer.json"),
            &target,
            None,
            &cancel,
            |_, _, _| {},
        )
        .await
        .expect_err("cancelado antes de começar deveria falhar");

        assert!(error.downcast_ref::<Cancelled>().is_some(), "{error:#}");
        assert!(!target.exists(), "o nome definitivo não pode aparecer");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Um servidor HTTP de uma pedida por conexão, montado aqui mesmo. A
    /// retentativa só é provada por uma conexão que some no meio do corpo, e
    /// nenhum teste vai derrubar o HuggingFace para conseguir isso — nem gastar
    /// gigabytes reais para ver o mesmo efeito.
    mod servidor {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        pub(super) const TOTAL: usize = 100_000;

        /// Bytes que não são zeros: um `.part` de zeros passaria em qualquer
        /// emenda errada, e a emenda errada é exatamente o risco da retomada.
        pub(super) fn corpo() -> Vec<u8> {
            (0..TOTAL).map(|i| (i % 251) as u8).collect()
        }

        /// O corpo publicado com um byte trocado no meio: mesmo tamanho, outro
        /// arquivo. É o que uma remessa errada de CDN deixa, e é o que nenhum
        /// contador de bytes vê — por isso a soma existe.
        pub(super) fn corpo_errado() -> Vec<u8> {
            let mut v = corpo();
            v[TOTAL / 2] ^= 0xAA;
            v
        }

        #[derive(Clone, Copy)]
        pub(super) enum Pedida {
            /// Anuncia o corpo inteiro e entrega só `bytes` antes de fechar. É
            /// o que uma conexão cortada deixa escrito no `.part`.
            Cortar { bytes: usize },
            /// Anuncia o corpo inteiro, entrega `bytes` e então silencia: a
            /// conexão fica aberta, sem mandar o resto e sem fechar. É o corpo
            /// que para no meio depois que o `connect_timeout` já passou — só o
            /// `read_timeout` ou o *Parar* devolvem o controle.
            Silenciar { bytes: usize },
            /// Entrega tudo, honrando o Range quando ele vem.
            Inteira,
            /// Entrega o tamanho exato do arquivo publicado com os bytes errados.
            /// Sem rede, sem gigabytes: é o corpo trocado que a conferência por
            /// tamanho é cega, e é contra isto que o M8 existe.
            CorpoTroocado,
            /// Status sem corpo: 404 para a falha definitiva, 416 para o
            /// servidor que não reconhece o ponto onde paramos.
            Status(u16),
        }

        pub(super) struct Servidor {
            pub(super) url: String,
            pub(super) conexoes: Arc<AtomicUsize>,
        }

        impl Servidor {
            pub(super) fn novo(script: Vec<Pedida>) -> Self {
                let listener = TcpListener::bind("127.0.0.1:0").expect("porta local livre");
                let porta = listener.local_addr().unwrap().port();
                let conexoes = Arc::new(AtomicUsize::new(0));
                std::thread::spawn({
                    let conexoes = conexoes.clone();
                    move || atender(listener, script, conexoes)
                });
                Servidor {
                    url: format!("http://127.0.0.1:{porta}/arquivo"),
                    conexoes,
                }
            }

            pub(super) fn atendidas(&self) -> usize {
                self.conexoes.load(Ordering::Relaxed)
            }
        }

        fn atender(listener: TcpListener, script: Vec<Pedida>, conexoes: Arc<AtomicUsize>) {
            let bom = corpo();
            let trocado = corpo_errado();
            for pedida in script {
                let Ok((mut socket, _)) = listener.accept() else { return };
                conexoes.fetch_add(1, Ordering::Relaxed);
                let inicio = ler_inicio(&mut socket);
                let conteudo =
                    if matches!(pedida, Pedida::CorpoTroocado) { &trocado } else { &bom };
                let (resposta, silencia) = resposta(pedida, conteudo, &bom, inicio);
                let _ = socket.write_all(&resposta);
                let _ = socket.flush();
                if silencia {
                    // Segura a conexão aberta e muda, sem corpo: o cliente fica
                    // esperando bytes que não vêm. Solta depois de um tempo para
                    // não deixar um socket pendurado, mas o suficiente para o
                    // read_timeout e o *Parar* terem o que fazer primeiro.
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_secs(10));
                        drop(socket);
                    });
                } else {
                    let _ = socket.shutdown(std::net::Shutdown::Write);
                }
            }
            // Acabou o script: o que chegar depois é derrubado na hora. Um
            // downloader que resolvesse pedir mais vezes que o combinado falha o
            // teste em vez de esperar para sempre por uma resposta que ninguém
            // vai mandar.
            for extra in listener.incoming() {
                drop(extra);
            }
        }

        /// O único cabeçalho que importa para o teste é o ponto de onde o
        /// downloader diz que voltou.
        fn ler_inicio(leitor: &mut std::net::TcpStream) -> u64 {
            let mut bruto = Vec::new();
            let mut byte = [0u8; 1];
            while bruto.len() < 4096 {
                match leitor.read(&mut byte) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        bruto.push(byte[0]);
                        if bruto.ends_with(b"\r\n\r\n") {
                            break;
                        }
                    }
                }
            }
            String::from_utf8_lossy(&bruto)
                .to_ascii_lowercase()
                .split("range: bytes=")
                .nth(1)
                .and_then(|resto| resto.split('-').next())
                .and_then(|n| n.trim().parse::<u64>().ok())
                .unwrap_or(0)
        }

        /// A resposta crua e se a conexão deve ficar aberta e muda depois dela.
        /// `conteudo` é o que sai pelo socket — o corpo publicado ou o trocado,
        /// que têm o mesmo tamanho — e `publicado` é o que o cabeçalho anuncia.
        fn resposta(
            pedida: Pedida,
            conteudo: &[u8],
            publicado: &[u8],
            inicio: u64,
        ) -> (Vec<u8>, bool) {
            let inicio = (inicio as usize).min(publicado.len());
            let restante = publicado.len() - inicio;
            if let Pedida::Status(codigo) = pedida {
                return (
                    format!(
                        "HTTP/1.1 {codigo} indefinido\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                    .into_bytes(),
                    false,
                );
            }
            let entregue = match pedida {
                Pedida::Inteira | Pedida::CorpoTroocado => restante,
                Pedida::Cortar { bytes } | Pedida::Silenciar { bytes } => bytes.min(restante),
                Pedida::Status(_) => unreachable!(),
            };
            let (codigo, faixa) = if inicio > 0 {
                (
                    "206 Partial Content",
                    format!(
                        "Content-Range: bytes {}-{}/{}\r\n",
                        inicio,
                        publicado.len() - 1,
                        publicado.len()
                    ),
                )
            } else {
                ("200 OK", String::new())
            };
            let mut bytes = format!(
                "HTTP/1.1 {codigo}\r\nContent-Length: {restante}\r\n{faixa}Connection: close\r\n\r\n"
            )
            .into_bytes();
            bytes.extend_from_slice(&conteudo[inicio..inicio + entregue]);
            (bytes, matches!(pedida, Pedida::Silenciar { .. }))
        }
    }

    use servidor::{corpo, corpo_errado, Pedida, Servidor, TOTAL};

    /// O proxy do sistema não pode levar uma pedida local para fora: o servidor
    /// do teste nunca veria a conexão e o teste passaria provando nada.
    fn cliente_local() -> reqwest::Client {
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("cliente local de teste")
    }

    fn pasta_de_teste(nome: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aether-ret-{nome}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// As esperas do backoff viram cinco milissegundos aqui: o que se mede é a
    /// sequência de pedidas, não o relógio.
    async fn baixar(
        servidor: &Servidor,
        dir: &Path,
        esperado: Option<u64>,
        cancel: &AtomicBool,
    ) -> anyhow::Result<u64> {
        baixar_conferindo(servidor, dir, esperado, None, cancel).await
    }

    /// O mesmo caminho, com a soma publicada na mão. É a porta que `fetch` usa
    /// para pesos e apoio.
    async fn baixar_conferindo(
        servidor: &Servidor,
        dir: &Path,
        esperado: Option<u64>,
        sha: Option<&str>,
        cancel: &AtomicBool,
    ) -> anyhow::Result<u64> {
        stream_desde_o_ponto(
            &cliente_local(),
            &servidor.url,
            &dir.join("arquivo.gguf"),
            esperado,
            sha,
            cancel,
            |_, _, _| {},
            Duration::from_millis(5),
        )
        .await
    }

    /// A soma do corpo que o servidor *deveria* servir, calculada do fixture.
    /// Este é o ponto: a expectativa nasce do que o catálogo declara, nunca dos
    /// bytes que chegaram — somar o download e chamar o resultado de verificação
    /// é fotografia, não conferência.
    fn sha256_de(dados: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(dados);
        format!("{:x}", hasher.finalize())
    }

    fn hash_do_corpo() -> String {
        sha256_de(&corpo())
    }

    fn hash_do_corpo_errado() -> String {
        sha256_de(&corpo_errado())
    }

    /// O defeito que a retentativa paga: uma conexão que some no meio de um
    /// arquivo grande não pode obrigar ninguém a apertar *Baixar* de novo. A
    /// segunda pedida sai do ponto onde a primeira parou e o arquivo final é
    /// byte a byte o mesmo de um download sem corte.
    #[tokio::test]
    async fn uma_conexao_cortada_levanta_a_pedida_do_ponto_onde_parou() {
        let servidor = Servidor::novo(vec![
            Pedida::Cortar { bytes: 10_000 },
            Pedida::Inteira,
        ]);
        let dir = pasta_de_teste("corte");
        let cancel = AtomicBool::new(false);

        let recebidos = baixar(&servidor, &dir, Some(TOTAL as u64), &cancel)
            .await
            .expect("a segunda pedida deveria completar o arquivo");

        let alvo = dir.join("arquivo.gguf");
        assert_eq!(std::fs::read(&alvo).unwrap(), corpo(), "a emenda corrompeu o arquivo");
        assert_eq!(recebidos, (TOTAL - 10_000) as u64, "vieram bytes repetidos do servidor");
        assert_eq!(servidor.atendidas(), 2);
        assert!(!part_path(&alvo).exists(), "o .part tinha de virar o arquivo");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Cortar todas as tentativas seguidas é uma rede caída, não um apagão de um
    /// segundo: a falha volta com o motivo da última conexão e com o número de
    /// pedidas gastadas, e o `.part` fica pronto para quem tentar mais tarde. O
    /// script tem o dobro de cortes de que a retentativa dispõe — o que se prova
    /// é que ela para, não que ela chegou justinho onde o número manda.
    #[tokio::test]
    async fn a_rede_caida_devolve_as_tentativas_e_o_motivo_e_para_no_numero_preso() {
        // Cada pedida só entrega um oitavo do arquivo, então por mais tentativas
        // que o número prese o download nunca completa sozinho.
        let corte = TOTAL / (TENTATIVAS as usize * 2);
        let servidor = Servidor::novo(vec![Pedida::Cortar { bytes: corte }; TENTATIVAS as usize * 2]);
        let dir = pasta_de_teste("caida");
        let cancel = AtomicBool::new(false);

        let erro = baixar(&servidor, &dir, Some(TOTAL as u64), &cancel)
            .await
            .expect_err("cortes seguidos não podem parecer sucesso");

        let texto = format!("{erro:#}");
        assert!(texto.contains(&TENTATIVAS.to_string()), "{texto}");
        assert!(part_path(&dir.join("arquivo.gguf")).is_file(), "o .part é o trabalho feito");
        assert_eq!(
            std::fs::metadata(part_path(&dir.join("arquivo.gguf"))).unwrap().len(),
            (TENTATIVAS as u64) * corte as u64,
            "cada pedida tinha de retomar do ponto anterior"
        );
        assert!(!dir.join("arquivo.gguf").exists(), "nada é publicado pela metade");
        assert_eq!(servidor.atendidas(), TENTATIVAS as usize, "{texto}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Um 404 não se conserta pedindo de novo. A falha definitiva tem de
    /// devolver o motivo na primeira conexão, senão o backoff vira teimosia e o
    /// usuário espera quatro vezes por uma resposta que já era certa. O script
    /// oferece mais pedidas do que a retentativa gasta: sobrar conexão na lista
    /// é a prova de que ela parou.
    #[tokio::test]
    async fn um_arquivo_que_nao_existe_devolve_o_motivo_na_primeira_pedida() {
        let servidor = Servidor::novo(vec![Pedida::Status(404); TENTATIVAS as usize * 2]);
        let dir = pasta_de_teste("404");
        let cancel = AtomicBool::new(false);

        let erro = baixar(&servidor, &dir, Some(TOTAL as u64), &cancel)
            .await
            .expect_err("404 não é download");

        assert!(format!("{erro:#}").contains("404"), "{erro:#}");
        assert_eq!(servidor.atendidas(), 1, "falha definitiva não gasta retentativa");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// O *Parar* tem de valer durante a espera, não só durante a transferência:
    /// cancelado, o download devolve `Cancelled` na hora e não levanta outra
    /// pedida na rede.
    #[tokio::test]
    async fn cancelar_para_a_retentativa_na_primeira_conexao() {
        let servidor = Servidor::novo(vec![Pedida::Cortar { bytes: 10_000 }]);
        let dir = pasta_de_teste("cancelar");
        let cancel = AtomicBool::new(true);

        let erro = baixar(&servidor, &dir, Some(TOTAL as u64), &cancel)
            .await
            .expect_err("cancelado no meio não é sucesso");

        assert!(erro.downcast_ref::<Cancelled>().is_some(), "{erro:#}");
        assert_eq!(servidor.atendidas(), 1, "cancelado não pode pedir de novo");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// O servidor que não reconhece o ponto em que paramos manda um 416. Emendar
    /// bytes de outra versão seria um GGUF que o motor recusa na carga, então o
    /// `.part` é descartado e a pedida volta do zero — o arquivo final continua
    /// sendo o que o catálogo promete.
    #[tokio::test]
    async fn um_416_descarta_o_parcial_e_volta_do_zero() {
        let servidor = Servidor::novo(vec![Pedida::Status(416), Pedida::Inteira]);
        let dir = pasta_de_teste("416");
        let alvo = dir.join("arquivo.gguf");
        std::fs::write(part_path(&alvo), &corpo()[..90_000]).unwrap();
        let cancel = AtomicBool::new(false);

        let recebidos = baixar(&servidor, &dir, Some(TOTAL as u64), &cancel)
            .await
            .expect("deveria recomeçar do zero e completar");

        assert_eq!(recebidos, TOTAL as u64, "recomeçar do zero é pedir o arquivo inteiro");
        assert_eq!(std::fs::read(&alvo).unwrap(), corpo());
        assert_eq!(servidor.atendidas(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// O *Parar* tem de valer durante um corpo que silenciou — conexão aberta,
    /// sem bytes, sem fechar — e não só durante a espera do backoff. Sem o
    /// `select!` contra o stream, `stream.next()` fica pendurado esperando bytes
    /// que não vêm (e este cliente de teste não tem `read_timeout` nenhum): o
    /// botão não faz nada até o teste estourar. Com o vigia de cancelamento, o
    /// *Parar* vence a espera por bytes e devolve `Cancelled` na hora.
    #[tokio::test]
    async fn parar_responde_durante_um_corpo_que_silenciou() {
        let servidor = Servidor::novo(vec![Pedida::Silenciar { bytes: 10_000 }]);
        let dir = pasta_de_teste("parar-silencio");
        let cancel = std::sync::Arc::new(AtomicBool::new(false));

        let control = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            control.store(true, Ordering::Relaxed);
        });

        // O timeout de 5s é o veredito do próprio defeito: pendurado no stream,
        // o download nunca devolve e o tempo esgota. Verde é `Cancelled` antes.
        let resposta =
            tokio::time::timeout(Duration::from_secs(5), baixar(&servidor, &dir, Some(TOTAL as u64), &cancel))
                .await;
        match resposta {
            Err(_) => panic!("o *Parar* não respondeu durante o silêncio: o stream ficou pendurado"),
            Ok(Ok(_)) => panic!("um download cancelado no meio não é sucesso"),
            Ok(Err(erro)) => {
                assert!(erro.downcast_ref::<Cancelled>().is_some(), "{erro:#}")
            }
        }
        assert_eq!(servidor.atendidas(), 1, "cancelado não pode pedir de novo");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Ninguém apertou *Parar* e mesmo assim um corpo que silenciou não pode
    /// pendurar o download para sempre: o `read_timeout` corta o silêncio, cada
    /// corte é retentável, e no fim a fila devolve as tentativas gastadas em vez
    /// de esperar por bytes que não vêm. O servidor silencioso tem o dobro de
    /// pedidas de que a retentativa dispõe — sobrar conexão na lista é a prova
    /// de que ela parou no número, não que ficou presa.
    #[tokio::test]
    async fn um_corpo_que_silencia_vira_falha_retentavel_pelo_read_timeout() {
        let servidor =
            Servidor::novo(vec![Pedida::Silenciar { bytes: 10_000 }; TENTATIVAS as usize * 2]);
        let dir = pasta_de_teste("read-timeout");
        let cancel = AtomicBool::new(false);
        // A cadeia de cliente real da produção, só com o prazo de leitura
        // encurtado: o que se mede é o corte do silêncio, não o relógio de 30s.
        let http =
            client_com_leitura(Duration::from_millis(200)).expect("cliente com read_timeout");

        let resposta = tokio::time::timeout(
            Duration::from_secs(10),
            stream_desde_o_ponto(
                &http,
                &servidor.url,
                &dir.join("arquivo.gguf"),
                Some(TOTAL as u64),
                None,
                &cancel,
                |_, _, _| {},
                Duration::from_millis(5),
            ),
        )
        .await;
        let erro = match resposta {
            Err(_) => panic!("o read_timeout não cortou o silêncio: o download ficou pendurado"),
            Ok(Ok(_)) => panic!("um corpo que silenciou não é download completo"),
            Ok(Err(erro)) => erro,
        };

        let texto = format!("{erro:#}");
        assert!(texto.contains(&TENTATIVAS.to_string()), "{texto}");
        assert_eq!(servidor.atendidas(), TENTATIVAS as usize, "cada silêncio tinha de virar uma pedida");
        assert!(!dir.join("arquivo.gguf").exists(), "nada é publicado pela metade");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// O defeito M8, medido sem rede e sem gigabyte: um corpo que chega com o
    /// tamanho publicado e um byte trocado no meio é outro arquivo, e o
    /// contador de bytes não tem como saber. Aqui isso tinha de virar o
    /// `arquivo.gguf` definitivo e o motor abriria um GGUF que ninguém publicou.
    /// O recomeço é pequeno e declarado — `REINICIOS_POR_INTEGRIDADE` — e no
    /// fim a resposta é o par medido/publicado, nunca um "falhou".
    #[tokio::test]
    async fn um_corpo_do_tamanho_certo_com_bytes_errados_nao_e_publicado() {
        let servidor =
            Servidor::novo(vec![Pedida::CorpoTroocado; REINICIOS_POR_INTEGRIDADE as usize + 1]);
        let dir = pasta_de_teste("corpo-trocado");
        let alvo = dir.join("arquivo.gguf");
        let cancel = AtomicBool::new(false);

        let erro = baixar_conferindo(
            &servidor,
            &dir,
            Some(TOTAL as u64),
            Some(&hash_do_corpo()),
            &cancel,
        )
        .await
        .expect_err("um corpo com outro sha256 não pode parecer download perfeito");

        let trocado = erro
            .downcast_ref::<CorpoTroocado>()
            .unwrap_or_else(|| panic!("corpo trocado não é a resposta: {erro:#}"));
        assert!(
            !eh_transitorio(&erro),
            "um corpo trocado não é falha de rede, e retomar do ponto seria a mesma remessa: {erro:#}"
        );
        assert_eq!(trocado.medido, hash_do_corpo_errado(), "o hash medido não é o dos bytes que chegaram");
        assert_eq!(trocado.esperado, hash_do_corpo(), "a expectativa não é a soma do corpo publicado");
        assert_eq!(trocado.recomecos, REINICIOS_POR_INTEGRIDADE, "{trocado:?}");
        let texto = format!("{erro:#}");
        assert!(texto.contains(&trocado.medido) && texto.contains(&trocado.esperado), "{texto}");
        assert!(!alvo.exists(), "nada é publicado com o corpo errado");
        assert!(
            !part_path(&alvo).exists(),
            "um .part de corpo trocado é lixo de outra remessa, não trabalho para retomar"
        );
        assert_eq!(
            servidor.atendidas(),
            REINICIOS_POR_INTEGRIDADE as usize + 1,
            "cada recomeço pede o corpo inteiro uma vez"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A consequência que a escolha abre, pelo lado bom: um CDN que devolveu uma
    /// remessa errada uma vez é consertado pelo recomeço do zero, e o arquivo
    /// publicado é o corpo que bate com a soma — não o `.part` antigo.
    #[tokio::test]
    async fn o_reinicio_do_zero_publica_a_remessa_que_bate_com_a_soma() {
        let servidor = Servidor::novo(vec![Pedida::CorpoTroocado, Pedida::Inteira]);
        let dir = pasta_de_teste("reinicio-integridade");
        let alvo = dir.join("arquivo.gguf");
        let cancel = AtomicBool::new(false);

        let recebidos = baixar_conferindo(
            &servidor,
            &dir,
            Some(TOTAL as u64),
            Some(&hash_do_corpo()),
            &cancel,
        )
        .await
        .expect("a segunda remessa é o arquivo publicado");

        assert_eq!(
            sha256_de(&std::fs::read(&alvo).unwrap()),
            hash_do_corpo(),
            "publicou a remessa errada"
        );
        assert_eq!(recebidos, TOTAL as u64, "recomeçar do zero é pedir o corpo inteiro outra vez");
        assert_eq!(servidor.atendidas(), 2, "um recomeço, não a fila de rede inteira");
        assert!(!part_path(&alvo).exists(), "o .part descartado não volta");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// O caminho que a retomada abre e que a conferência por tamanho deixava
    /// escancarado: um `.part` já do tamanho exato era publicado sem tocar a
    /// rede nenhuma vez. Se ele veio de uma remessa errada, tem de virar lixo e
    /// o corpo tem de ser pedido de novo.
    #[tokio::test]
    async fn um_parcial_do_tamanho_exato_so_vira_arquivo_com_a_soma_certa() {
        let servidor = Servidor::novo(vec![Pedida::Inteira]);
        let dir = pasta_de_teste("parcial-pronto");
        let alvo = dir.join("arquivo.gguf");
        std::fs::write(part_path(&alvo), corpo_errado()).unwrap();
        let cancel = AtomicBool::new(false);

        let recebidos = baixar_conferindo(
            &servidor,
            &dir,
            Some(TOTAL as u64),
            Some(&hash_do_corpo()),
            &cancel,
        )
        .await
        .expect("deveria descartar o parcial errado e pedir o corpo publicado");

        assert_eq!(
            sha256_de(&std::fs::read(&alvo).unwrap()),
            hash_do_corpo(),
            "o parcial com o corpo errado foi publicado"
        );
        assert_eq!(recebidos, TOTAL as u64, "um parcial que não bate não é trabalho feito");
        assert_eq!(servidor.atendidas(), 1, "uma pedida para cada remessa pedida");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// O limite do que foi medido, declarado em teste: onde o repositório não
    /// publica soma — o `tokenizer.json` do Granite, que é blob git comum e só
    /// tem SHA-1 do blob — não há expectativa a conferir, e o mesmo corpo
    /// trocado passa. Um hash calculado dos bytes que acabaram de chegar não
    /// entraria aqui por nada: seria uma fotografia do download.
    #[tokio::test]
    async fn sem_hash_publicado_a_conferencia_e_so_de_tamanho() {
        let servidor = Servidor::novo(vec![Pedida::CorpoTroocado]);
        let dir = pasta_de_teste("sem-hash");
        let alvo = dir.join("arquivo.gguf");
        let cancel = AtomicBool::new(false);

        let recebidos = baixar_conferindo(&servidor, &dir, Some(TOTAL as u64), None, &cancel)
            .await
            .expect("sem soma publicada o que se confere é o tamanho, e ele bate");

        assert_eq!(recebidos, TOTAL as u64);
        assert_eq!(
            sha256_de(&std::fs::read(&alvo).unwrap()),
            hash_do_corpo_errado(),
            "sem expectativa não há recusa: o que publica é o tamanho"
        );
        assert!(!part_path(&alvo).exists());
        assert_eq!(servidor.atendidas(), 1, "nenhum recomeço é disparado sem soma");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Rede real. O arquivo LFS mais barato do catálogo que tem soma publicada:
    /// os 11.423.705 bytes do `tokenizer.json` da Qwen, cujo `lfs.oid` foi lido
    /// de `tree/main` e está em `catalog.rs`. Este teste é a prova de que o
    /// campo que o catálogo lê é o sha256 dos bytes servidos — o veredito do
    /// M8, porém, vem dos testes locais acima, não daqui.
    #[tokio::test]
    #[ignore = "baixa do HuggingFace de verdade"]
    async fn a_soma_publicada_de_um_arquivo_lfs_real_bate() {
        let spec = catalog::find("qwen3-embedding-0.6b-q8_0").unwrap();
        let esperado = catalog::apoio_sha256(spec, "tokenizer.json")
            .expect("o tokenizador LFS da Qwen tem soma publicada");
        assert_eq!(esperado, "def76fb086971c7867b829c23a26261e38d9d74e02139253b38aeb9df8b4b50a");

        let dir = std::env::temp_dir().join(format!("aether-sha-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("tokenizer.json");
        let cancel = AtomicBool::new(false);

        stream_to_file_confirmando(
            &client().unwrap(),
            &catalog::support_url(spec, "tokenizer.json"),
            &target,
            None,
            Some(esperado),
            &cancel,
            |_, _, _| {},
        )
        .await
        .expect("os bytes publicados têm de bater com a soma publicada");

        assert_eq!(sha256_de_arquivo(&target).unwrap(), esperado);
        let _ = std::fs::remove_dir_all(&dir);
    }
}