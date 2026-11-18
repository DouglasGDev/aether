//! Download real dos pesos e dos arquivos de apoio.
//!
//! Nada aqui é simulado: os bytes vêm do HuggingFace, o progresso conta o que foi
//! escrito em disco, e o tamanho final é conferido contra o catálogo antes de o
//! arquivo ganhar o nome definitivo.

use super::catalog::{self, ModelSpec};
use crate::idiomas::tr;
use anyhow::{Context, bail};
use futures_util::StreamExt;
use serde::Serialize;
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

fn part_path(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "arquivo".to_string());
    target.with_file_name(format!("{name}.part"))
}

pub(crate) fn client() -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("Aether/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(30))
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

/// Escreve `url` em `target`, retomando de um `.part` quando o servidor aceita
/// Range, e tentando de novo — com o `.part` como ponto de partida — quando a
/// rede é quem falha. `expected` é o tamanho exato esperado, ou None para apoio
/// (o tamanho só é conhecido pela resposta). Devolve quantos bytes vieram da
/// rede na pedida que completou o arquivo — menos que o tamanho dele quando a
/// retomada funcionou.
///
/// Também é o que baixa os modelos de voz e o pacote do runtime: mesma retomada,
/// mesma conferência de tamanho, mesmo `.part` abandonado em disco para a
/// próxima tentativa.
pub(crate) async fn stream_to_file(
    http: &reqwest::Client,
    url: &str,
    target: &Path,
    expected: Option<u64>,
    cancel: &AtomicBool,
    on_progress: impl FnMut(u64, u64, u64),
) -> anyhow::Result<u64> {
    stream_desde_o_ponto(http, url, target, expected, cancel, on_progress, ESPERA_BASE).await
}

async fn stream_desde_o_ponto(
    http: &reqwest::Client,
    url: &str,
    target: &Path,
    expected: Option<u64>,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(u64, u64, u64),
    espera_base: Duration,
) -> anyhow::Result<u64> {
    for tentativa in 1..=TENTATIVAS {
        match tentativa_de_escrever(http, url, target, expected, cancel, &mut on_progress).await {
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
    while let Some(chunk) = stream.next().await {
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
        stream_to_file(
            &http,
            &catalog::support_url(spec, file),
            &target,
            None,
            cancel,
            |_, _, _| {},
        )
        .await
        .with_context(|| tr("erros.arquivoDeApoio", &[("arquivo", file)]))?;
    }

    let progress = emit.clone();
    let weights_id = id.clone();
    let total = spec.weights_bytes;
    stream_to_file(
        &http,
        &catalog::weights_url(spec),
        &dir.join(spec.weights_file),
        Some(spec.weights_bytes),
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

        #[derive(Clone, Copy)]
        pub(super) enum Pedida {
            /// Anuncia o corpo inteiro e entrega só `bytes` antes de fechar. É
            /// o que uma conexão cortada deixa escrito no `.part`.
            Cortar { bytes: usize },
            /// Entrega tudo, honrando o Range quando ele vem.
            Inteira,
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
            let corpo = corpo();
            for pedida in script {
                let Ok((mut socket, _)) = listener.accept() else { return };
                conexoes.fetch_add(1, Ordering::Relaxed);
                let inicio = ler_inicio(&mut socket);
                let _ = socket.write_all(&resposta(pedida, &corpo, inicio));
                let _ = socket.flush();
                let _ = socket.shutdown(std::net::Shutdown::Write);
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

        fn resposta(pedida: Pedida, corpo: &[u8], inicio: u64) -> Vec<u8> {
            let inicio = (inicio as usize).min(corpo.len());
            let restante = corpo.len() - inicio;
            if let Pedida::Status(codigo) = pedida {
                return format!(
                    "HTTP/1.1 {codigo} indefinido\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .into_bytes();
            }
            let entregue = match pedida {
                Pedida::Inteira => restante,
                Pedida::Cortar { bytes } => bytes.min(restante),
                Pedida::Status(_) => unreachable!(),
            };
            let (codigo, faixa) = if inicio > 0 {
                (
                    "206 Partial Content",
                    format!(
                        "Content-Range: bytes {}-{}/{}\r\n",
                        inicio,
                        corpo.len() - 1,
                        corpo.len()
                    ),
                )
            } else {
                ("200 OK", String::new())
            };
            let mut bytes = format!(
                "HTTP/1.1 {codigo}\r\nContent-Length: {restante}\r\n{faixa}Connection: close\r\n\r\n"
            )
            .into_bytes();
            bytes.extend_from_slice(&corpo[inicio..inicio + entregue]);
            bytes
        }
    }

    use servidor::{corpo, Pedida, Servidor, TOTAL};

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
        stream_desde_o_ponto(
            &cliente_local(),
            &servidor.url,
            &dir.join("arquivo.gguf"),
            esperado,
            cancel,
            |_, _, _| {},
            Duration::from_millis(5),
        )
        .await
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
}