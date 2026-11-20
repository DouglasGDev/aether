//! Baixar os modelos de voz: o pacote do Kokoro e os arquivos do Whisper.
//!
//! As duas formas de chegada são diferentes porque os modelos são diferentes. O
//! Kokoro vem num único `.tar.bz2` do release do sherpa-onnx com trezentas e
//! setenta e oito arquivos dentro — pedi-los um a um pela rede seria ridículo.
//! O Whisper são três arquivos do HuggingFace, e cada um retoma do próprio
//! `.part`. O transporte é o mesmo dos pesos dos modelos de linguagem: mesma
//! conferência de tamanho, mesmo cancelamento, mesmo progresso contado em disco.

use super::modelos::{self, EspecVoz, Fonte};
use crate::ai::download::{client, part_path, stream_to_file};
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

/// Um arquivo que a descida ainda vai tocar, com o que o `.part` já guarda.
struct Remessa {
    url: String,
    destino: PathBuf,
    tamanho: u64,
    /// Bytes que já estão no `.part` quando a pedida começa. São do modelo, mas
    /// não vieram da rede nesta chamada, por isso ficam fora da barra.
    prefixo: u64,
}

impl Remessa {
    /// O que este arquivo ainda soma na barra.
    fn a_receber(&self) -> u64 {
        self.tamanho - self.prefixo
    }

    /// Numerado ao vivo: `em_disco` é o `.part` crescendo durante a pedida, e o
    /// que ele já tinha não é byte de rede. Um recomeço do zero (servidor que
    /// ignora o Range, `.part` maior que o publicado) faz `em_disco` voltar a
    /// caminhar do começo, e a barra atrasar um pouco — nunca adiantar.
    fn na_rede(&self, em_disco: u64) -> u64 {
        em_disco.saturating_sub(self.prefixo)
    }
}

/// O que falta baixar e quanto a barra promete. A barra é o que vem da rede,
/// não o tamanho teórico do modelo: um arquivo que já está em disco sai da
/// lista, e o `.part` pela metade entra só pelo que falta dele. Somar os
/// tamanhos inteiros, como era antes, começava a barra adiantada e o clamp no
/// denominador escondia isso empurrando a barra para os 100 % antes da hora.
fn remessas(espec: &'static EspecVoz, raiz: &Path) -> anyhow::Result<(Vec<Remessa>, u64)> {
    let mut lista = Vec::new();
    let mut total = 0u64;
    for (url, destino, esperado) in modelos::arquivos(espec, raiz) {
        if no_tamanho(&destino, esperado) {
            continue;
        }
        let tamanho = esperado.ok_or_else(|| {
            anyhow!(tr(
                "erros.vozSemTamanhoNoCatalogo",
                &[("arquivo", &destino.display().to_string())]
            ))
        })?;
        let bruto = part_path(&destino)
            .metadata()
            .map(|m| m.len())
            .unwrap_or(0);
        // Um `.part` maior que o arquivo publicado não é retomável: `download`
        // deita ele fora e pede tudo de novo, então aqui ele não adianta nada.
        let prefixo = if bruto > tamanho { 0 } else { bruto };
        let remessa = Remessa { url, destino, tamanho, prefixo };
        // A soma da barra é a mesma conta que o arquivo faz ao vivo, pelo mesmo
        // método: somar `tamanho - prefixo` aqui de novo é ter duas régua que
        // podem discordar.
        total += remessa.a_receber();
        lista.push(remessa);
    }
    Ok((lista, total))
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
    let (lista, total) = remessas(espec, raiz)?;
    emit(VozEvento::Iniciou {
        id: id.clone(),
        total_bytes: total,
    });

    if !lista.is_empty() {
        let http = client()?;
        let mut feitos = 0u64;
        for remessa in &lista {
            if cancel.load(Ordering::Relaxed) {
                bail!(tr("erros.downloadCancelado", &[]));
            }
            std::fs::create_dir_all(remessa.destino.parent().unwrap_or(raiz)).with_context(|| {
                tr("erros.vozFalhaPasta", &[("pasta", &raiz.display().to_string())])
            })?;
            let emissor = emit.clone();
            let progresso_id = id.clone();
            let base = feitos;
            let vindos = stream_to_file(
                &http,
                &remessa.url,
                &remessa.destino,
                Some(remessa.tamanho),
                cancel,
                |em_disco, _, speed| {
                    emissor(VozEvento::Progresso {
                        id: progresso_id.clone(),
                        done_bytes: base + remessa.na_rede(em_disco),
                        total_bytes: total,
                        bytes_por_segundo: speed,
                    });
                },
            )
            .await
            .with_context(|| {
                tr(
                    "erros.vozArquivoNaoVeio",
                    &[("arquivo", &remessa.destino.display().to_string())],
                )
            })?;
            // O que `stream_to_file` devolve é o que a pedida que completou o
            // arquivo trouxe da rede: menos que o tamanho quando a retomada
            // funcionou. É esse número, e não o tamanho, que alimenta a barra.
            feitos += vindos;
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

/// Descompacta um pacote na raiz pedida. O `tar` que o Windows traz lê os dois
/// formatos de que o aplicativo depende — o `.tar.bz2` do sherpa, que repõe os
/// trezentos e setenta e oito arquivos com os nomes que o modelo espera, e o
/// `.zip` do llama.cpp, de onde o runtime nativo tira as DLLs.
pub(crate) fn extrair(archive: &Path, raiz: &Path) -> anyhow::Result<()> {
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
        .with_context(|| tr("erros.semTar", &[("caminho", &tar.display().to_string())]))?;
    if !saida.status.success() {
        let motivo = String::from_utf8_lossy(&saida.stderr).trim().to_string();
        bail!(tr(
            "erros.extracaoFalhou",
            &[
                ("caminho", &archive.display().to_string()),
                ("detalhe", motivo.as_str())
            ]
        ));
    }
    Ok(())
}

pub(crate) fn caminho_do_tar() -> anyhow::Result<PathBuf> {
    let system_root =
        std::env::var_os("SystemRoot").ok_or_else(|| anyhow!(tr("erros.semSystemRoot", &[])))?;
    let tar = Path::new(&system_root).join("System32").join("tar.exe");
    if !tar.is_file() {
        bail!(tr("erros.semTar", &[("caminho", &tar.display().to_string())]));
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

    /// Whisper-tiny vazio numa pasta só deste processo: 12 937 772 + 89 855 401
    /// + 816 730 = 103 609 903 bytes quando estiver completo.
    fn cenario(nome: &str) -> (PathBuf, &'static EspecVoz) {
        let raiz = std::env::temp_dir().join(format!("aether-{nome}-{}", std::process::id()));
        let espec = modelos::encontrar("whisper-tiny").unwrap();
        let _ = std::fs::remove_dir_all(&raiz);
        std::fs::create_dir_all(modelos::modelo_dir(&raiz, espec)).unwrap();
        (raiz, espec)
    }

    /// O numerador e o denominador do primeiro progresso do laço de `baixar`
    /// para o estado do disco. O numerador é `na_rede(em_disco)` com o `.part`
    /// como ele está: a pedida recomeça do prefixo, então nenhum daqueles bytes
    /// veio da rede nesta chamada.
    fn primeiro_progresso(espec: &'static EspecVoz, raiz: &Path) -> (u64, u64) {
        let (lista, total) = remessas(espec, raiz).unwrap();
        let primeira = lista.first().expect("com arquivo faltando há remessa");
        (primeira.na_rede(primeira.prefixo), total)
    }

    #[test]
    fn um_modelo_inteiro_nao_tem_nada_a_receber() {
        let (raiz, espec) = cenario("inteiro");
        let dir = modelos::modelo_dir(&raiz, espec);
        for (nome, tamanho) in espec.obrigatorios {
            std::fs::write(dir.join(nome), vec![0u8; *tamanho as usize]).unwrap();
        }
        let (lista, total) = remessas(espec, &raiz).unwrap();
        assert!(lista.is_empty(), "modelo completo não tem remessa");
        assert_eq!(total, 0, "nada veio da rede para um modelo completo");
        let _ = std::fs::remove_dir_all(&raiz);
    }

    #[test]
    fn a_barra_soma_o_que_falta_nao_o_tamanho_do_modelo() {
        let (raiz, espec) = cenario("barra");
        let dir = modelos::modelo_dir(&raiz, espec);
        // Só o primeiro arquivo chegou: a barra mede os dois que faltam.
        std::fs::write(dir.join(espec.obrigatorios[0].0), vec![0u8; 12_937_772]).unwrap();
        let (lista, total) = remessas(espec, &raiz).unwrap();
        assert_eq!(lista.len(), 2);
        assert_eq!(total, 89_855_401 + 816_730);
        let _ = std::fs::remove_dir_all(&raiz);
    }

    #[test]
    fn um_arquivo_que_ja_esta_em_disco_nao_engorda_a_barra() {
        let (raiz, espec) = cenario("tick");
        let dir = modelos::modelo_dir(&raiz, espec);
        std::fs::write(dir.join(espec.obrigatorios[0].0), vec![0u8; 12_937_772]).unwrap();
        let (numerador, denominador) = primeiro_progresso(espec, &raiz);
        let _ = std::fs::remove_dir_all(&raiz);
        assert_eq!(
            numerador, 0,
            "sem byte da rede a barra começa em {numerador} de {denominador}"
        );
    }

    #[test]
    fn um_part_parcial_nao_conta_como_rede() {
        let (raiz, espec) = cenario("prefixo");
        let dir = modelos::modelo_dir(&raiz, espec);
        let (primeiro, tamanho) = espec.obrigatorios[0];
        // Um `.part` de 8 MB dentro de um arquivo de 12,9 MB: a pedida recomeça
        // dos 8 MB e só traz os 4,9 MB que faltam.
        std::fs::write(dir.join(format!("{primeiro}.part")), vec![0u8; 8_000_000]).unwrap();
        let (numerador, denominador) = primeiro_progresso(espec, &raiz);
        let total_do_modelo = espec.bytes_ocupados();
        let _ = std::fs::remove_dir_all(&raiz);
        assert!(8_000_000 < tamanho, "o prefixo tem de caber no arquivo");
        assert_eq!(
            numerador, 0,
            "o .part já tem 8 MB, mas nada veio da rede nesta chamada: a barra \
             começou em {numerador}"
        );
        assert_eq!(
            denominador,
            total_do_modelo - 8_000_000,
            "a barra promete {denominador} e a rede só tem {} para trazer",
            total_do_modelo - 8_000_000
        );
    }

    #[test]
    fn um_part_maior_que_o_arquivo_promete_o_arquivo_inteiro() {
        let (raiz, espec) = cenario("maior");
        let dir = modelos::modelo_dir(&raiz, espec);
        let (nome, tamanho) = espec.obrigatorios[2];
        std::fs::write(dir.join(format!("{nome}.part")), vec![0u8; tamanho as usize * 3]).unwrap();
        let (lista, _) = remessas(espec, &raiz).unwrap();
        let ultima = lista.last().expect("o terceiro arquivo falta");
        let _ = std::fs::remove_dir_all(&raiz);
        // `download` deita esse lixo fora e pede tudo de novo, então a barra não
        // pode descontar um prefixo que não vai ser retomado.
        assert_eq!(ultima.prefixo, 0);
        assert_eq!(ultima.a_receber(), tamanho);
    }

    #[test]
    fn a_barra_promete_o_que_a_rede_trara_de_um_disco_misto() {
        let (raiz, espec) = cenario("misto");
        let dir = modelos::modelo_dir(&raiz, espec);
        std::fs::write(dir.join(espec.obrigatorios[0].0), vec![0u8; 12_937_772]).unwrap();
        std::fs::write(
            dir.join(format!("{}.part", espec.obrigatorios[1].0)),
            vec![0u8; 40_000_000],
        )
        .unwrap();
        let (lista, total) = remessas(espec, &raiz).unwrap();
        let _ = std::fs::remove_dir_all(&raiz);
        assert_eq!(lista.len(), 2, "o arquivo pronto não entra na descida");
        assert_eq!(total, (89_855_401 - 40_000_000) + 816_730);
        assert_eq!(total, lista.iter().map(Remessa::a_receber).sum::<u64>());
    }

    #[test]
    fn o_tar_do_windows_existe_e_faz_o_pacote() {
        let tar = caminho_do_tar().unwrap();
        assert!(tar.is_file());
    }
}
