//! A senha de conexão nunca toca o disco em claro.
//!
//! O `bancos.json` guarda o texto cifrado pelo DPAPI do Windows no escopo do
//! usuário: a cifra é do usuário logado na máquina, então nem um arquivo vazado
//! num backup abre a conexão em outro computador. O valor real só existe na
//! memória do aplicativo, no momento de abrir a conexão — a mesma regra do
//! token do servidor MCP publicado, que também nunca dorma em disco.

#[cfg(windows)]
mod interna {
    use anyhow::{Result, anyhow};
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD as BASE64;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CryptProtectData, CryptUnprotectData,
    };

    fn blob(bytes: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB { cbData: bytes.len() as u32, pbData: bytes.as_ptr() as *mut u8 }
    }

    /// Cifra com o DPAPI do usuário logado e devolve base64, pronto para o JSON.
    pub fn cifrar(plaintext: &str) -> Result<String> {
        unsafe {
            let entrada = blob(plaintext.as_bytes());
            let mut saida = CRYPT_INTEGER_BLOB::default();
            CryptProtectData(
                &entrada,
                PCWSTR::null(),
                None,
                None,
                None,
                0,
                &mut saida,
            )
            .map_err(|e| anyhow!(crate::idiomas::tr("erros.dpapiCifrar", &[("detalhe", &e.to_string())])))?;
            let cifrado =
                std::slice::from_raw_parts(saida.pbData, saida.cbData as usize).to_vec();
            let _ = LocalFree(Some(HLOCAL(saida.pbData as *mut _)));
            Ok(BASE64.encode(cifrado))
        }
    }

    /// Abre o texto cifrado do `bancos.json`. Só é chamado na hora de conectar.
    pub fn decifrar(cifrado: &str) -> Result<String> {
        let bytes = BASE64
            .decode(cifrado)
            .map_err(|e| anyhow!(crate::idiomas::tr("erros.dpapiBase64", &[("detalhe", &e.to_string())])))?;
        unsafe {
            let entrada = blob(&bytes);
            let mut saida = CRYPT_INTEGER_BLOB::default();
            CryptUnprotectData(
                &entrada,
                None,
                None,
                None,
                None,
                0,
                &mut saida,
            )
            .map_err(|e| anyhow!(crate::idiomas::tr("erros.dpapiDecifrar", &[("detalhe", &e.to_string())])))?;
            let texto =
                String::from_utf8(std::slice::from_raw_parts(saida.pbData, saida.cbData as usize).to_vec())
                    .map_err(|e| anyhow!(crate::idiomas::tr("erros.dpapiTexto", &[("detalhe", &e.to_string())])))?;
            let _ = LocalFree(Some(HLOCAL(saida.pbData as *mut _)));
            Ok(texto)
        }
    }
}

#[cfg(not(windows))]
mod interna {
    use anyhow::{Result, anyhow};

    pub fn cifrar(_plaintext: &str) -> Result<String> {
        anyhow!(crate::idiomas::tr("erros.dpapiSohWindows", &[]))
    }

    pub fn decifrar(_cifrado: &str) -> Result<String> {
        anyhow!(crate::idiomas::tr("erros.dpapiSohWindows", &[]))
    }
}

#[cfg(windows)]
pub use interna::{cifrar, decifrar};
