//! As pastas que o usuário concedeu. Fora delas o Aether não lê nem altera nada.
//!
//! A contenção é feita sobre o caminho real, não sobre o texto pedido: um link
//! simbólico que aponta para fora, ou um `..` no meio, é resolvido antes da
//! comparação, então não há como escapar por escrita criativa de caminho.

use crate::idiomas::tr;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Pastas {
    #[serde(default)]
    pub raizes: Vec<PathBuf>,
}

/// O Windows devolve `\\?\C:\...` de canonicalize; esse prefixo desliga a
/// normalização da API e não compare com um caminho comum. Tiramos dos dois
/// lados para que a comparação de contenção seja entre caminhos iguais.
fn sem_verbatim(caminho: PathBuf) -> PathBuf {
    let texto = caminho.to_string_lossy().into_owned();
    if let Some(resto) = texto.strip_prefix(r"\\?\") {
        if let Some(unc) = resto.strip_prefix(r"UNC\") {
            return PathBuf::from(format!(r"\\{unc}"));
        }
        return PathBuf::from(resto);
    }
    caminho
}

fn canonico(caminho: &Path) -> Result<PathBuf> {
    let real = std::fs::canonicalize(caminho)
        .with_context(|| tr("erros.caminhoNaoExiste", &[("caminho", &caminho.display().to_string())]))?;
    Ok(sem_verbatim(real))
}

/// Resolve `.` e `..` no texto, sem tocar no disco. Necessário porque a cauda
/// que ainda não existe não passa pelo canonicalize do sistema.
fn lexico(caminho: &Path) -> PathBuf {
    let mut saida = PathBuf::new();
    for componente in caminho.components() {
        match componente {
            Component::CurDir => {}
            Component::ParentDir => {
                // Na raiz não há para onde subir: `C:\..` continua `C:\`.
                saida.pop();
            }
            outro => saida.push(outro.as_os_str()),
        }
    }
    saida
}

impl Pastas {
    pub fn carregar(caminho: &Path) -> Result<Self> {
        let texto = match std::fs::read_to_string(caminho) {
            Ok(texto) => texto,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => {
                return Err(e).with_context(|| {
                    tr("erros.falhaLerPastas", &[("caminho", &caminho.display().to_string())])
                });
            }
        };
        let pastas: Self = serde_json::from_str(&texto).with_context(|| {
            tr("erros.pastasInvalidas", &[("caminho", &caminho.display().to_string())])
        })?;
        Ok(pastas)
    }

    pub fn salvar(&self, caminho: &Path) -> Result<()> {
        if let Some(pai) = caminho.parent() {
            std::fs::create_dir_all(pai).with_context(|| {
                tr("erros.falhaCriarPasta", &[("caminho", &pai.display().to_string())])
            })?;
        }
        let texto = serde_json::to_string_pretty(self)
            .map_err(|e| anyhow::anyhow!(tr("erros.falhaSalvarPastas", &[("detalhe", &e.to_string())])))?;
        let temporario = caminho.with_extension("json.tmp");
        std::fs::write(&temporario, texto).with_context(|| {
            tr("erros.falhaEscreverConfig", &[("caminho", &temporario.display().to_string())])
        })?;
        std::fs::rename(&temporario, caminho).with_context(|| {
            tr("erros.falhaSubstituirConfig", &[("caminho", &caminho.display().to_string())])
        })?;
        Ok(())
    }

    pub fn vazia(&self) -> bool {
        self.raizes.is_empty()
    }

    /// Concede uma pasta que existe de verdade. Devolve o caminho real guardado.
    pub fn conceder(&mut self, pedido: &str) -> Result<PathBuf> {
        let texto = pedido.trim();
        if texto.is_empty() {
            bail!(tr("pastas.erroVazio", &[]));
        }
        let pedido = Path::new(texto);
        if !pedido.exists() {
            bail!(tr("pastas.erroNaoExiste", &[]));
        }
        if !pedido.is_dir() {
            bail!(tr("pastas.erroNaoEPasta", &[]));
        }
        let real = canonico(pedido)?;
        if self.raizes.iter().any(|r| real.starts_with(r)) {
            bail!(tr("pastas.erroDentroDeOutra", &[]));
        }
        // Uma pasta que contém outra já concedida a substitui: manter as duas
        // daria a ilusão de duas permissões onde há uma só.
        self.raizes.retain(|r| !r.starts_with(&real));
        self.raizes.push(real.clone());
        self.raizes.sort();
        Ok(real)
    }

    pub fn revogar(&mut self, raiz: &str) -> bool {
        let antes = self.raizes.len();
        self.raizes.retain(|r| r.to_string_lossy() != raiz);
        self.raizes.len() != antes
    }

    /// Traduz um caminho pedido pelo modelo em um caminho real dentro de uma
    /// pasta concedida. Recusa em vez de devolver algo fora delas.
    pub fn resolver(&self, pedido: &str) -> Result<PathBuf> {
        if self.raizes.is_empty() {
            bail!(tr("erros.semPastas", &[]));
        }
        let texto = pedido.trim();
        if texto.is_empty() {
            bail!(tr("erros.caminhoVazio", &[]));
        }
        let bruto = Path::new(texto);
        let junto = if bruto.is_absolute() {
            bruto.to_path_buf()
        } else {
            // Relativo: a primeira raiz onde já existe; senão a primeira raiz,
            // que é onde um arquivo novo nasce.
            self.raizes
                .iter()
                .map(|r| r.join(bruto))
                .find(|p| p.exists())
                .unwrap_or_else(|| self.raizes[0].join(bruto))
        };
        let limpo = lexico(&junto);

        // Canonicalize só aceita caminho existente, então subimos até o
        // ancestral mais próximo que existe e remontamos a cauda depois.
        let mut ancestral = limpo.clone();
        let mut cauda: Vec<std::ffi::OsString> = Vec::new();
        while !ancestral.exists() {
            match ancestral.file_name() {
                Some(nome) => cauda.push(nome.to_os_string()),
                None => break,
            }
            if !ancestral.pop() {
                break;
            }
        }
        let mut alvo = if ancestral.exists() { canonico(&ancestral)? } else { ancestral };
        for parte in cauda.iter().rev() {
            alvo.push(parte);
        }

        if !self.raizes.iter().any(|raiz| alvo.starts_with(raiz)) {
            bail!(tr(
                "erros.foraDasPastas",
                &[("caminho", &alvo.display().to_string())]
            ));
        }
        Ok(alvo)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporaria(nome: &str) -> PathBuf {
        let pasta = std::env::temp_dir().join(format!("aether-pastas-{nome}-{}", std::process::id()));
        std::fs::create_dir_all(&pasta).expect("criei a pasta de teste");
        sem_verbatim(std::fs::canonicalize(&pasta).expect("canonicalizei"))
    }

    fn concedida(nome: &str) -> (PathBuf, Pastas) {
        let raiz = temporaria(nome);
        let mut pastas = Pastas::default();
        pastas.conceder(&raiz.to_string_lossy()).expect("concedi");
        (raiz, pastas)
    }

    #[test]
    fn sem_pasta_concedida_nenhum_caminho_e_aceito() {
        let pastas = Pastas::default();
        assert!(pastas.vazia());
        let erro = pastas.resolver("C:/Windows/system32").unwrap_err().to_string();
        assert_eq!(erro, tr("erros.semPastas", &[]));
    }

    #[test]
    fn caminho_dentro_da_raiz_e_aceito() {
        let (raiz, pastas) = concedida("dentro");
        std::fs::write(raiz.join("nota.txt"), "oi").unwrap();
        let alvo = pastas.resolver(&raiz.join("nota.txt").to_string_lossy()).unwrap();
        assert_eq!(alvo, raiz.join("nota.txt"));
        std::fs::remove_dir_all(&raiz).ok();
    }

    #[test]
    fn caminho_fora_da_raiz_e_recusado() {
        let (raiz, pastas) = concedida("fora");
        let fora = raiz.parent().unwrap().join("segredo.txt");
        let erro = pastas.resolver(&fora.to_string_lossy()).unwrap_err().to_string();
        assert!(erro.contains("fora das pastas"), "{erro}");
        std::fs::remove_dir_all(&raiz).ok();
    }

    #[test]
    fn ponto_ponto_no_meio_do_caminho_nao_escapa() {
        let (raiz, pastas) = concedida("ponto-ponto");
        let fuga = raiz.join("sub").join("..").join("..").join("segredo.txt");
        assert!(pastas.resolver(&fuga.to_string_lossy()).is_err());
        std::fs::remove_dir_all(&raiz).ok();
    }

    #[test]
    fn alvo_que_ainda_nao_existe_e_contido_do_mesmo_jeito() {
        let (raiz, pastas) = concedida("novo");
        let novo = pastas.resolver(&raiz.join("sub").join("novo.txt").to_string_lossy()).unwrap();
        assert_eq!(novo, raiz.join("sub").join("novo.txt"));
        let fuga = raiz.join("sub").join("..").join("..").join("novo.txt");
        assert!(pastas.resolver(&fuga.to_string_lossy()).is_err());
        std::fs::remove_dir_all(&raiz).ok();
    }

    #[test]
    fn caminho_relativo_cai_na_primeira_raiz() {
        let (raiz, pastas) = concedida("relativo");
        let alvo = pastas.resolver("nota.txt").unwrap();
        assert_eq!(alvo, raiz.join("nota.txt"));
        std::fs::remove_dir_all(&raiz).ok();
    }

    #[test]
    fn caminho_relativo_existente_em_outra_raiz_e_encontrado() {
        let (primeira, mut pastas) = concedida("relativo-a");
        let segunda = temporaria("relativo-b");
        std::fs::write(segunda.join("achado.txt"), "x").unwrap();
        pastas.conceder(&segunda.to_string_lossy()).unwrap();
        let alvo = pastas.resolver("achado.txt").unwrap();
        assert_eq!(alvo, segunda.join("achado.txt"));
        std::fs::remove_dir_all(&primeira).ok();
        std::fs::remove_dir_all(&segunda).ok();
    }

    #[test]
    fn conceder_exige_pasta_que_existe() {
        let mut pastas = Pastas::default();
        assert!(pastas.conceder("").is_err());
        let (raiz, _) = concedida("conceder");
        let arquivo = raiz.join("arquivo.txt");
        std::fs::write(&arquivo, "x").unwrap();
        assert!(pastas.conceder(&arquivo.to_string_lossy()).is_err());
        assert!(pastas.conceder(&raiz.join("nao-existe").to_string_lossy()).is_err());
        std::fs::remove_dir_all(&raiz).ok();
    }

    #[test]
    fn conceder_a_mesma_pasta_duas_vezes_e_recusado() {
        let (raiz, mut pastas) = concedida("duplicada");
        assert!(pastas.conceder(&raiz.to_string_lossy()).is_err());
        assert_eq!(pastas.raizes.len(), 1);
        std::fs::remove_dir_all(&raiz).ok();
    }

    #[test]
    fn conceder_a_pasta_mae_engole_a_filha() {
        let raiz = temporaria("mae");
        let filha = raiz.join("filha");
        std::fs::create_dir_all(&filha).unwrap();
        let mut pastas = Pastas::default();
        pastas.conceder(&filha.to_string_lossy()).unwrap();
        assert_eq!(pastas.raizes, vec![filha.clone()]);
        // A mãe cobre a filha: uma permissão, não duas.
        pastas.conceder(&raiz.to_string_lossy()).unwrap();
        assert_eq!(pastas.raizes, vec![raiz.clone()]);
        // E o caminho inverso é recusado: a filha já está coberta pela mãe.
        assert!(pastas.conceder(&filha.to_string_lossy()).is_err());
        assert_eq!(pastas.raizes, vec![raiz.clone()]);
        std::fs::remove_dir_all(&raiz).ok();
    }

    #[test]
    fn revogar_tira_so_aquela_raiz() {
        let (raiz, mut pastas) = concedida("revogar");
        assert!(pastas.revogar(&raiz.to_string_lossy()));
        assert!(!pastas.revogar(&raiz.to_string_lossy()));
        assert!(pastas.vazia());
        std::fs::remove_dir_all(&raiz).ok();
    }

    #[test]
    fn as_pastas_sobrevivem_ao_disco() {
        let (raiz, pastas) = concedida("disco");
        let arquivo = raiz.join("pastas.json");
        pastas.salvar(&arquivo).unwrap();
        let lidas = Pastas::carregar(&arquivo).unwrap();
        assert_eq!(lidas.raizes, pastas.raizes);
        assert!(Pastas::carregar(&raiz.join("nao-existe.json")).unwrap().vazia());
        std::fs::remove_dir_all(&raiz).ok();
    }

    #[test]
    fn lexico_nao_sobe_acima_da_raiz() {
        let caminho = lexico(Path::new("C:/a/../../b"));
        assert_eq!(caminho, PathBuf::from("C:/b"));
    }

    #[cfg(windows)]
    #[test]
    fn link_simbolico_para_fora_e_recusado() {
        let (raiz, pastas) = concedida("link");
        let fora = temporaria("link-fora");
        std::fs::write(fora.join("segredo.txt"), "x").unwrap();
        let link = raiz.join("atalho");
        // Sem privilégio de link simbólico o teste não prova nada: pulamos.
        if std::os::windows::fs::symlink_dir(&fora, &link).is_err() {
            std::fs::remove_dir_all(&raiz).ok();
            std::fs::remove_dir_all(&fora).ok();
            return;
        }
        assert!(pastas.resolver(&link.join("segredo.txt").to_string_lossy()).is_err());
        std::fs::remove_dir_all(&raiz).ok();
        std::fs::remove_dir_all(&fora).ok();
    }
}