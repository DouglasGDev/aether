use crate::connectors::{Connector, McpEntry, Registry, RegistryError};
use crate::idiomas::tr;
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct McpConfig {
    #[serde(default)]
    pub servers: Vec<Connector>,
}

impl McpConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let caminho = path.display().to_string();
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)
                .with_context(|| tr("erros.configInvalida", &[("caminho", &caminho)])),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| tr("erros.falhaLerConfig", &[("caminho", &caminho)])),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let caminho = path.display().to_string();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                tr("erros.falhaCriarPasta", &[("caminho", &parent.display().to_string())])
            })?;
        }
        let text = serde_json::to_string_pretty(self)
            .context(tr("erros.falhaSalvarConfig", &[]))?;
        let temp = path.with_extension("json.tmp");
        std::fs::write(&temp, text).with_context(|| {
            tr("erros.falhaEscreverConfig", &[("caminho", &temp.display().to_string())])
        })?;
        std::fs::rename(&temp, path)
            .with_context(|| tr("erros.falhaSubstituirConfig", &[("caminho", &caminho)]))?;
        Ok(())
    }

    /// O conector embutido faz parte do cadastro mesmo sem estar no arquivo: é
    /// o próprio aplicativo, não algo que o usuário instalou. Se ele já está no
    /// arquivo, a versão de lá vale — é onde a allowlist dele fica guardada.
    pub fn registry(&self) -> Result<Registry, RegistryError> {
        let mut servidores = self.servers.clone();
        if !servidores.iter().any(|c| c.id == crate::local::CONECTOR) {
            servidores.push(crate::local::conector());
        }
        // Um servidor do catálogo pertence ao aplicativo: o arquivo guarda o id
        // e o transporte, e o texto legível vem sempre dos idiomas. Sem isto,
        // trocar de idioma deixaria o servidor instalado na língua antiga. O
        // servidor que o próprio Aether publica é o mesmo caso.
        for conector in servidores.iter_mut() {
            if let Some(pronto) = crate::mcp::instalador::pronto(&conector.id) {
                conector.name = crate::mcp::instalador::nome(pronto);
                conector.description = crate::mcp::instalador::descricao(pronto);
            } else if conector.id == crate::mcp::servidor::CONECTOR {
                crate::mcp::servidor::relocalizar(conector);
            }
        }
        Registry::new(servidores)
    }

    pub fn upsert(&mut self, connector: Connector) {
        match self.servers.iter_mut().find(|c| c.id == connector.id) {
            Some(existing) => *existing = connector,
            None => self.servers.push(connector),
        }
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.servers.len();
        self.servers.retain(|c| c.id != id);
        self.servers.len() != before
    }
}

fn substitute_with(text: &str, lookup: &dyn Fn(&str) -> Option<String>) -> Result<String> {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            bail!(tr("erros.placeholderAberto", &[("texto", text)]));
        };
        let name = &after[..end];
        let value = lookup(name)
            .ok_or_else(|| anyhow!(tr("erros.variavelAusente", &[("nome", name)])))?;
        out.push_str(&value);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

fn resolve_entry_with(entry: &McpEntry, lookup: &dyn Fn(&str) -> Option<String>) -> Result<McpEntry> {
    let sub = |text: &str| substitute_with(text, lookup);
    match entry {
        McpEntry::Stdio { command, args, env } => {
            let mut resolved_env = HashMap::new();
            for (key, value) in env {
                resolved_env.insert(key.clone(), sub(value)?);
            }
            Ok(McpEntry::Stdio {
                command: sub(command)?,
                args: args.iter().map(|a| sub(a)).collect::<Result<Vec<_>>>()?,
                env: resolved_env,
            })
        }
        McpEntry::Http { url, headers } => {
            let mut resolved_headers = HashMap::new();
            for (key, value) in headers {
                resolved_headers.insert(key.clone(), sub(value)?);
            }
            Ok(McpEntry::Http {
                url: sub(url)?,
                headers: resolved_headers,
            })
        }
    }
}

// Placeholders are resolved into a throwaway copy at connect time; the stored
// config keeps ${VAR} so a secret is never written to disk in clear text.
pub fn resolve_connector(connector: &Connector, segredos: &Segredos) -> Result<Connector> {
    resolve_connector_with(connector, &|name| {
        segredos.obter(name).or_else(|| std::env::var(name).ok())
    })
}

/// Um segredo que só existe enquanto o aplicativo está de pé. O token do
/// servidor que o próprio Aether publica é sorteado a cada execução e o
/// `mcp.json` guarda só `${AETHER_MCP_TOKEN}`: sem ele no ambiente — e pôr um
/// segredo no ambiente do processo seria expô-lo a todo filho — a conexão só se
/// resolve aqui dentro, onde ele nasceu.
#[derive(Debug, Default)]
pub struct Segredos {
    valores: Mutex<HashMap<String, String>>,
}

impl Segredos {
    pub fn guardar(&self, chave: &str, valor: &str) {
        self.valores
            .lock()
            .expect("segredos envenenados")
            .insert(chave.to_string(), valor.to_string());
    }

    pub fn esquecer(&self, chave: &str) {
        self.valores
            .lock()
            .expect("segredos envenenados")
            .remove(chave);
    }

    pub fn obter(&self, chave: &str) -> Option<String> {
        self.valores
            .lock()
            .expect("segredos envenenados")
            .get(chave)
            .cloned()
    }
}

fn resolve_connector_with(
    connector: &Connector,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<Connector> {
    let mut resolved = connector.clone();
    resolved.entry = connector
        .entry
        .as_ref()
        .map(|e| resolve_entry_with(e, lookup))
        .transpose()?;
    let mut local = HashMap::new();
    for (os, entry) in &connector.local {
        local.insert(*os, resolve_entry_with(entry, lookup)?);
    }
    resolved.local = local;
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::{ConnectorCategory, ConnectorKind, DesktopOs, McpAuth};

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map = env(pairs);
        move |name: &str| map.get(name).cloned()
    }

    fn connector(entry: McpEntry) -> Connector {
        Connector {
            id: "srv".into(),
            name: "Server".into(),
            description: String::new(),
            kind: ConnectorKind::Mcp,
            category: ConnectorCategory::Development,
            docs_url: String::new(),
            routes: vec![],
            auth: McpAuth::Token,
            entry: Some(entry),
            local: HashMap::new(),
            fields: vec![],
            env: vec![],
            tool_allowlist: None,
        }
    }

    #[test]
    fn text_without_placeholders_is_unchanged() {
        let f = lookup(&[]);
        assert_eq!(substitute_with("plain", &f).unwrap(), "plain");
    }

    #[test]
    fn placeholder_is_substituted() {
        let f = lookup(&[("TOKEN", "abc123")]);
        assert_eq!(
            substitute_with("Bearer ${TOKEN}", &f).unwrap(),
            "Bearer abc123"
        );
    }

    #[test]
    fn multiple_placeholders_are_substituted() {
        let f = lookup(&[("A", "1"), ("B", "2")]);
        assert_eq!(substitute_with("${A}-${B}", &f).unwrap(), "1-2");
    }

    #[test]
    fn missing_variable_is_an_error_not_a_literal() {
        let f = lookup(&[]);
        assert!(substitute_with("${MISSING}", &f).is_err());
    }

    #[test]
    fn unterminated_placeholder_is_an_error() {
        let f = lookup(&[("A", "1")]);
        assert!(substitute_with("${A", &f).is_err());
    }

    #[test]
    fn http_headers_are_resolved_without_touching_the_original() {
        let original = connector(McpEntry::Http {
            url: "http://localhost:8000/mcp".into(),
            headers: env(&[("Authorization", "Bearer ${TOKEN}")]),
        });
        let resolved =
            resolve_connector_with(&original, &lookup(&[("TOKEN", "real-secret")])).unwrap();
        assert!(matches!(
            original.entry.as_ref().unwrap(),
            McpEntry::Http { headers, .. } if headers["Authorization"] == "Bearer ${TOKEN}"
        ));
        assert!(matches!(
            resolved.entry.as_ref().unwrap(),
            McpEntry::Http { headers, .. } if headers["Authorization"] == "Bearer real-secret"
        ));
    }

    #[test]
    fn per_os_entries_are_resolved_too() {
        let mut c = connector(McpEntry::Stdio {
            command: "npx".into(),
            args: vec![],
            env: HashMap::new(),
        });
        c.local.insert(
            DesktopOs::Windows,
            McpEntry::Stdio {
                command: "${SERVER_BIN}".into(),
                args: vec!["--token".into(), "${TOKEN}".into()],
                env: HashMap::new(),
            },
        );
        let resolved =
            resolve_connector_with(&c, &lookup(&[("SERVER_BIN", "srv.exe"), ("TOKEN", "t")]))
                .unwrap();
        assert!(matches!(
            resolved.local.get(&DesktopOs::Windows).unwrap(),
            McpEntry::Stdio { command, args, .. }
                if command == "srv.exe" && args[1] == "t"
        ));
    }

    #[test]
    fn a_missing_secret_aborts_resolution_instead_of_leaking_a_placeholder() {
        let c = connector(McpEntry::Http {
            url: "http://localhost:8000/mcp".into(),
            headers: env(&[("Authorization", "Bearer ${TOKEN}")]),
        });
        assert!(resolve_connector_with(&c, &lookup(&[])).is_err());
    }

    #[test]
    fn config_roundtrips_through_disk() {
        let dir = std::env::temp_dir().join(format!("aether-config-test-{}", std::process::id()));
        let path = dir.join("mcp.json");
        let mut config = McpConfig::default();
        config.upsert(connector(McpEntry::Stdio {
            command: "npx".into(),
            args: vec![],
            env: HashMap::new(),
        }));
        config.save(&path).unwrap();

        let mut loaded = McpConfig::load(&path).unwrap();
        assert_eq!(loaded.servers.len(), 1);
        assert!(loaded.registry().is_ok());

        assert!(loaded.remove("srv"));
        assert!(!loaded.remove("srv"));
        assert!(loaded.servers.is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_config_file_is_an_empty_state() {
        let path = std::env::temp_dir().join("aether-does-not-exist-9f3b2a.json");
        let config = McpConfig::load(&path).unwrap();
        assert!(config.servers.is_empty());
    }

    #[test]
    fn upsert_replaces_by_id() {
        let mut config = McpConfig::default();
        config.upsert(connector(McpEntry::Stdio {
            command: "old".into(),
            args: vec![],
            env: HashMap::new(),
        }));
        config.upsert(connector(McpEntry::Stdio {
            command: "new".into(),
            args: vec![],
            env: HashMap::new(),
        }));
        assert_eq!(config.servers.len(), 1);
        assert!(matches!(
            config.servers[0].entry.as_ref().unwrap(),
            McpEntry::Stdio { command, .. } if command == "new"
        ));
    }
}