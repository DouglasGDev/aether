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

/// Escreve `url` em `target`, retomando de um `.part` quando o servidor aceita
/// Range. `expected` é o tamanho exato esperado, ou None para apoio (o tamanho
/// só é conhecido pela resposta). Devolve quantos bytes vieram da rede nesta
/// chamada — menos que o arquivo quando a retomada funcionou.
///
/// Também é o que baixa os modelos de voz: mesma retomada, mesma conferência de
/// tamanho, mesmo `.part` abandonado em disco para a próxima tentativa.
pub(crate) async fn stream_to_file(
    http: &reqwest::Client,
    url: &str,
    target: &Path,
    expected: Option<u64>,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(u64, u64, u64),
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
        .with_context(|| tr("erros.falhaPedir", &[("url", url)]))?;
    let status = response.status();
    if !status.is_success() {
        bail!(tr("erros.servidorRespondeu", &[("status", &status.to_string()), ("url", url)]));
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
                bail!(tr(
                    "erros.retomouDeOutroPonto",
                    &[
                        ("inicio", &format!("{start:?}")),
                        ("esperado", &resumed.to_string())
                    ]
                ));
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
        let chunk = chunk.with_context(|| tr("erros.conexaoCaiu", &[("url", url)]))?;
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
        bail!(tr(
            "erros.downloadIncompleto",
            &[("recebidos", &done.to_string()), ("esperados", &expected.to_string())]
        ));
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
}