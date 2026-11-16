//! Baixar os modelos de voz: o pacote do Kokoro e os arquivos do Whisper.
//!
//! As duas formas de chegada são diferentes porque os modelos são diferentes. O
//! Kokoro vem num único `.tar.bz2` do release do sherpa-onnx com trezentas e
//! setenta e oito arquivos dentro — pedi-los um a um pela rede seria ridículo.
//! O Whisper são três arquivos do HuggingFace, e cada um retoma do próprio
//! `.part`. O transporte é o mesmo dos pesos dos modelos de linguagem: mesma
//! conferência de tamanho, mesmo cancelamento, mesmo progresso contado em disco.

use super::modelos::{self, EspecVoz, Fonte};
use crate::ai::download::{client, stream_to_file};
use crate::idiomas::tr;
use anyhow::{Context, anyhow, bail};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

/// O que a tela de voz escuta enquanto um modelo chega.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum VozEvento {
    Iniciou {
        id: String,
        total_bytes: u64,
    },
    Progresso {
        id: String,
        done_bytes: u64,
        total_bytes: u64,
        bytes_por_segundo: u64,
    },
    /// O pacote está sendo descompactado: aqui não há byte de rede a contar.
    Extraindo {
        id: String,
    },
    Concluiu {
        id: String,
    },
    Falhou {
        id: String,
        erro: String,
    },
    Cancelou {
        id: String,
    },
}

fn no_tamanho(destino: &Path, esperado: Option<u64>) -> bool {
    match esperado {
        Some(esperado) => std::fs::metadata(destino).map(|m| m.len()).ok() == Some(esperado),
        None => destino.is_file(),
    }
}

/// Os arquivos que ainda faltam e quantos bytes eles somam. A barra é o que
/// vem da rede, não o tamanho teórico do modelo: retomar um `.part` de meio
/// caminho não pode mostrar uma barra que anda mais devagar que a verdade.
fn a_receber(
    espec: &'static EspecVoz,
    raiz: &Path,
) -> (Vec<(String, PathBuf, Option<u64>)>, u64) {
    let lista = modelos::arquivos(espec, raiz);
    let total = lista
        .iter()
        .filter(|(_, destino, esperado)| !no_tamanho(destino, *esperado))
        .filter_map(|(_, _, esperado)| *esperado)
        .sum();
    (lista, total)
}

/// Busca o que falta e deixa o modelo pronto. `Concluiu` só é emitido depois de
/// conferir, arquivo por arquivo, o que o catálogo promete.
pub async fn baixar(
    espec: &'static EspecVoz,
    raiz: &Path,
    cancel: &AtomicBool,
    emit: impl Fn(VozEvento) + Clone,
) -> anyhow::Result<()> {
    let id = espec.id.to_string();
    let (lista, total) = a_receber(espec, raiz);
    emit(VozEvento::Iniciou {
        id: id.clone(),
        total_bytes: total,
    });

    if total > 0 {
        let http = client()?;
        let mut recebidos = 0u64;
        for (url, destino, esperado) in &lista {
            if cancel.load(Ordering::Relaxed) {
                bail!(tr("erros.downloadCancelado", &[]));
            }
            let tamanho = esperado.ok_or_else(|| {
                anyhow!(tr(
                    "erros.vozSemTamanhoNoCatalogo",
                    &[("arquivo", &destino.display().to_string())]
                ))
            })?;
            if no_tamanho(destino, *esperado) {
                recebidos += tamanho;
                continue;
            }
            std::fs::create_dir_all(destino.parent().unwrap_or(raiz)).with_context(|| {
                tr("erros.vozFalhaPasta", &[("pasta", &raiz.display().to_string())])
            })?;
            let emissor = emit.clone();
            let progresso_id = id.clone();
            let base = recebidos;
            let denominador = total;
            stream_to_file(&http, url, destino, *esperado, cancel, |done, _, speed| {
                emissor(VozEvento::Progresso {
                    id: progresso_id.clone(),
                    done_bytes: (base + done).min(denominador),
                    total_bytes: denominador,
                    bytes_por_segundo: speed,
                });
            })
            .await
            .with_context(|| {
                tr(
                    "erros.vozArquivoNaoVeio",
                    &[("arquivo", &destino.display().to_string())],
                )
            })?;
            recebidos += tamanho;
        }
    }

    // O pacote vem comprimido: os arquivos que o modelo lê nascem da
    // descompactação local, feita pelo `tar` que o próprio Windows traz.
    if let Fonte::Pacote { .. } = espec.fonte {
        let archive = modelos::pacote_caminho(raiz, espec);
        if archive.is_file() {
            emit(VozEvento::Extraindo { id: id.clone() });
            let raiz_extracao = raiz.to_path_buf();
            let archive_extracao = archive.clone();
            tauri::async_runtime::spawn_blocking(move || extrair(&archive_extracao, &raiz_extracao))
                .await
                .map_err(|e| {
                    anyhow!(tr(
                        "erros.vozExtracaoNaoTerminou",
                        &[("detalhe", &e.to_string())]
                    ))
                })??;
            if cancel.load(Ordering::Relaxed) {
                bail!(tr("erros.downloadCancelado", &[]));
            }
        }
    }

    let estado = modelos::status(raiz, espec);
    if !estado.baixado {
        bail!(tr(
            "erros.vozFaltouArquivo",
            &[("arquivos", &estado.faltando.join(", "))]
        ));
    }

    // Extraído e conferido, o cache de cento e trinta e dois megabytes perde a
    // razão de existir: é espaço que o modelo poderia estar usando.
    if let Fonte::Pacote { .. } = espec.fonte {
        let archive = modelos::pacote_caminho(raiz, espec);
        if archive.is_file() {
            std::fs::remove_file(&archive).with_context(|| {
                tr("erros.vozNaoApagouCache", &[("caminho", &archive.display().to_string())])
            })?;
        }
    }

    emit(VozEvento::Concluiu { id });
    Ok(())
}

/// Descompacta o pacote do sherpa na raiz dos modelos de voz. O `tar` do
/// Windows lê `.tar.bz2` e repõe os trezentos e setenta e oito arquivos com os
/// nomes que o modelo espera.
fn extrair(archive: &Path, raiz: &Path) -> anyhow::Result<()> {
    let tar = caminho_do_tar()?;
    let mut programa = Command::new(&tar);
    programa
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(raiz)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        programa.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: nada de console.
    }
    let saida = programa
        .output()
        .with_context(|| tr("erros.vozSemTar", &[("caminho", &tar.display().to_string())]))?;
    if !saida.status.success() {
        let motivo = String::from_utf8_lossy(&saida.stderr).trim().to_string();
        bail!(tr(
            "erros.vozExtracaoFalhou",
            &[
                ("caminho", &archive.display().to_string()),
                ("detalhe", motivo.as_str())
            ]
        ));
    }
    Ok(())
}

fn caminho_do_tar() -> anyhow::Result<PathBuf> {
    let system_root =
        std::env::var_os("SystemRoot").ok_or_else(|| anyhow!(tr("erros.vozSemSystemRoot", &[])))?;
    let tar = Path::new(&system_root).join("System32").join("tar.exe");
    if !tar.is_file() {
        bail!(tr("erros.vozSemTar", &[("caminho", &tar.display().to_string())]));
    }
    Ok(tar)
}

/// Apaga o modelo e o pacote que ficou em cache. Diferente dos pesos de um LLM,
/// aqui a pasta inteira é do modelo: nada é compartilhado entre as entradas do
/// catálogo de voz, então apagar não quebra outra voz.
pub fn apagar(espec: &'static EspecVoz, raiz: &Path) -> anyhow::Result<()> {
    let dir = modelos::modelo_dir(raiz, espec);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .with_context(|| tr("erros.vozNaoApagou", &[("pasta", &dir.display().to_string())]))?;
    }
    let archive = modelos::pacote_caminho(raiz, espec);
    if archive.is_file() {
        std::fs::remove_file(&archive).with_context(|| {
            tr("erros.vozNaoApagou", &[("pasta", &archive.display().to_string())])
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn um_modelo_inteiro_nao_tem_nada_a_receber() {
        let raiz = std::env::temp_dir().join(format!("aether-baixar-{}", std::process::id()));
        let espec = modelos::encontrar("whisper-tiny").unwrap();
        let dir = modelos::modelo_dir(&raiz, espec);
        let _ = std::fs::remove_dir_all(&raiz);
        std::fs::create_dir_all(&dir).unwrap();
        for (nome, tamanho) in espec.obrigatorios {
            std::fs::write(dir.join(nome), vec![0u8; *tamanho as usize]).unwrap();
        }
        let (lista, total) = a_receber(espec, &raiz);
        assert_eq!(lista.len(), 3);
        assert_eq!(total, 0, "nada veio da rede para um modelo completo");
        let _ = std::fs::remove_dir_all(&raiz);
    }

    #[test]
    fn a_barra_soma_o_que_falta_nao_o_tamanho_do_modelo() {
        let raiz = std::env::temp_dir().join(format!("aether-barra-{}", std::process::id()));
        let espec = modelos::encontrar("whisper-tiny").unwrap();
        let dir = modelos::modelo_dir(&raiz, espec);
        let _ = std::fs::remove_dir_all(&raiz);
        std::fs::create_dir_all(&dir).unwrap();
        // Só o primeiro arquivo chegou: a barra mede os dois que faltam.
        std::fs::write(dir.join(espec.obrigatorios[0].0), vec![0u8; 12_937_772]).unwrap();
        let (_, total) = a_receber(espec, &raiz);
        assert_eq!(total, 89_855_401 + 816_730);
        let _ = std::fs::remove_dir_all(&raiz);
    }

    #[test]
    fn o_tar_do_windows_existe_e_faz_o_pacote() {
        let tar = caminho_do_tar().unwrap();
        assert!(tar.is_file());
    }
}
