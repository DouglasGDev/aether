//! O que a máquina realmente é: sistema, processador, memória e disco medidos.
//!
//! Nada aqui é estimado. Quando uma medição não existe nesta plataforma, a
//! linha é omitida em vez de receber um número inventado.

use crate::ai::router;
use crate::idiomas::tr;
use crate::local::pastas::Pastas;
use crate::local::{Saida, Vista, raiz_de_trabalho};
use crate::local::vista::Par;
use anyhow::Result;
use std::path::{Path, PathBuf};

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

fn nome_do_sistema() -> String {
    match std::env::consts::OS {
        "windows" => "Windows".to_string(),
        "macos" => "macOS".to_string(),
        "linux" => "Linux".to_string(),
        outro => outro.to_string(),
    }
}

fn processador() -> Option<String> {
    std::env::var("PROCESSOR_IDENTIFIER")
        .ok()
        .map(|texto| texto.trim().to_string())
        .filter(|texto| !texto.is_empty())
}

fn usuario() -> Option<String> {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .ok()
        .filter(|texto| !texto.trim().is_empty())
}

/// O volume onde o trabalho acontece: a primeira pasta concedida, ou o diretório
/// atual quando ainda não há nenhuma.
fn volume(pastas: &Pastas) -> PathBuf {
    raiz_de_trabalho(pastas)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(windows)]
fn disco(caminho: &Path) -> Option<(u64, u64)> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    use windows::core::PCWSTR;

    // A API quer a raiz do volume ("C:\"), não uma pasta dentro dele.
    let raiz = match caminho.components().next() {
        Some(std::path::Component::Prefix(prefixo)) => {
            PathBuf::from(format!("{}\\", prefixo.as_os_str().to_string_lossy()))
        }
        _ => return None,
    };
    let largo: Vec<u16> = raiz.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut livre = 0u64;
    let mut total = 0u64;
    // SAFETY: os três ponteiros apontam para u64 vivos desta pilha, e a função
    // só escreve neles; `largo` é terminado em zero e vive até o fim da chamada.
    unsafe {
        GetDiskFreeSpaceExW(
            PCWSTR(largo.as_ptr()),
            Some(&mut livre),
            Some(&mut total),
            None,
        )
    }
    .ok()?;
    Some((total, livre))
}

#[cfg(not(windows))]
fn disco(_caminho: &Path) -> Option<(u64, u64)> {
    None
}

pub fn informacoes(pastas: &Pastas) -> Result<Saida> {
    let mut linhas: Vec<Par> = Vec::new();
    linhas.push(Par::texto(tr("sistema.sistemaOperacional", &[]), nome_do_sistema()));
    linhas.push(Par::texto(tr("sistema.arquitetura", &[]), std::env::consts::ARCH));

    if let Some(nome) = processador() {
        linhas.push(Par::texto(tr("sistema.processador", &[]), nome));
    }
    if let Ok(nucleos) = std::thread::available_parallelism() {
        linhas.push(Par::texto(tr("sistema.nucleos", &[]), nucleos.get().to_string()));
    }
    if let Ok(memoria) = router::measure_hardware() {
        linhas.push(Par::medida(
            tr("sistema.memoriaTotal", &[]),
            memoria.total_ram_bytes as f64 / GIB,
            "GiB",
        ));
        linhas.push(Par::medida(
            tr("sistema.memoriaLivre", &[]),
            memoria.available_ram_bytes as f64 / GIB,
            "GiB",
        ));
    }

    let trabalho = volume(pastas);
    if let Some((total, livre)) = disco(&trabalho) {
        linhas.push(Par::medida(tr("sistema.discoTotal", &[]), total as f64 / GIB, "GiB"));
        linhas.push(Par::medida(tr("sistema.discoLivre", &[]), livre as f64 / GIB, "GiB"));
    }
    if let Some(nome) = usuario() {
        linhas.push(Par::texto(tr("sistema.usuario", &[]), nome));
    }
    linhas.push(Par::texto(
        tr("sistema.pastasConcedidas", &[]),
        pastas.raizes.len().to_string(),
    ));
    linhas.push(Par::texto(tr("sistema.pastaTrabalho", &[]), trabalho.display().to_string()));

    let para_modelo = linhas
        .iter()
        .map(|linha| format!("{}: {}", linha.rotulo, linha.valor))
        .collect::<Vec<_>>()
        .join("\n");

    Ok(Saida::nova(para_modelo, Vista::Sistema { linhas }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_linhas_vem_de_medicoes_reais_e_nao_de_texto_fixo() {
        let saida = informacoes(&Pastas::default()).expect("medi");
        let Vista::Sistema { linhas } = &saida.vista else { panic!("vista errada") };
        assert!(linhas.len() >= 4, "{}", linhas.len());
        for linha in linhas {
            assert!(!linha.rotulo.is_empty());
            assert!(!linha.valor.is_empty());
            assert!(!linha.rotulo.starts_with("sistema."), "{}", linha.rotulo);
        }
        assert!(saida.texto.contains(&linhas[0].valor), "{}", saida.texto);
    }

    #[test]
    fn a_memoria_livre_nunca_passa_da_total() {
        let Ok(memoria) = router::measure_hardware() else { return };
        assert!(memoria.available_ram_bytes <= memoria.total_ram_bytes);
        assert!(memoria.total_ram_bytes > 0);
    }

    #[test]
    fn o_numero_cru_viaja_junto_do_texto_para_a_interface_formatar() {
        let saida = informacoes(&Pastas::default()).expect("medi");
        let Vista::Sistema { linhas } = &saida.vista else { panic!("vista errada") };
        let medidas: Vec<&Par> = linhas.iter().filter(|l| l.numero.is_some()).collect();
        if router::measure_hardware().is_ok() {
            assert!(!medidas.is_empty(), "nenhuma medida chegou");
            for linha in medidas {
                assert!(linha.numero.expect("número") > 0.0);
                assert_eq!(linha.unidade.as_deref(), Some("GiB"));
            }
        }
    }

    #[test]
    fn o_volume_e_a_raiz_da_pasta_concedida() {
        let mut pastas = Pastas::default();
        let base = std::env::temp_dir().join(format!("aether-sistema-{}", std::process::id()));
        std::fs::create_dir_all(&base).expect("criei");
        pastas.conceder(&base.display().to_string()).expect("concedi");
        let escolhido = volume(&pastas);
        assert!(escolhido.starts_with(&base) || base.starts_with(&escolhido));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    #[cfg(windows)]
    fn o_disco_medido_tem_total_maior_que_o_livre() {
        let (total, livre) = disco(Path::new("C:\\")).expect("medi o disco");
        assert!(total > 0);
        assert!(livre <= total, "{livre} > {total}");
    }
}