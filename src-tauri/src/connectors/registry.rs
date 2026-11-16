use super::meta;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConnectorKind {
    Builtin,
    Mcp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConnectorCategory {
    System,
    Files,
    Web,
    Development,
    Data,
    Productivity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum McpAuth {
    #[default]
    None,
    Token,
    Key,
    Oauth,
    OauthClient,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DesktopOs {
    Windows,
    Macos,
    Linux,
}

impl DesktopOs {
    pub fn current() -> Self {
        if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::Macos
        } else {
            Self::Linux
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "transport", rename_all = "lowercase")]
pub enum McpEntry {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
    },
    Http {
        url: String,
        #[serde(default)]
        headers: HashMap<String, String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorField {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub secret: bool,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConnectUi {
    None,
    Fields,
    BrowserHop,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Connector {
    pub id: String,
    pub name: String,
    pub description: String,
    pub kind: ConnectorKind,
    pub category: ConnectorCategory,
    #[serde(default)]
    pub docs_url: String,
    #[serde(default)]
    pub routes: Vec<String>,
    #[serde(default)]
    pub auth: McpAuth,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<McpEntry>,
    #[serde(default)]
    pub local: HashMap<DesktopOs, McpEntry>,
    #[serde(default)]
    pub fields: Vec<ConnectorField>,
    #[serde(default)]
    pub env: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_allowlist: Option<Vec<String>>,
}

impl Connector {
    pub fn entry_on(&self, os: DesktopOs) -> Option<&McpEntry> {
        self.local.get(&os).or(self.entry.as_ref())
    }

    pub fn connect_ui(&self) -> ConnectUi {
        match self.auth {
            McpAuth::None => ConnectUi::None,
            McpAuth::Oauth | McpAuth::OauthClient => ConnectUi::BrowserHop,
            McpAuth::Token | McpAuth::Key => ConnectUi::Fields,
        }
    }

    pub fn allows_tool(&self, tool: &str) -> bool {
        match &self.tool_allowlist {
            None => true,
            Some(allowed) => allowed.iter().any(|t| t == tool),
        }
    }

    pub fn has_entry(&self) -> bool {
        self.entry.is_some() || !self.local.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    DuplicateId(String),
    EmptyId,
    InvalidId(String),
    NoEntry(String),
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let texto = match self {
            Self::DuplicateId(id) => crate::idiomas::tr("erros.idDuplicado", &[("id", id)]),
            Self::EmptyId => crate::idiomas::tr("erros.idVazio", &[]),
            Self::InvalidId(id) => crate::idiomas::tr("erros.idInvalido", &[("id", id)]),
            Self::NoEntry(id) => crate::idiomas::tr("erros.semTransporte", &[("id", id)]),
        };
        f.write_str(&texto)
    }
}

impl std::error::Error for RegistryError {}

#[derive(Debug, Clone, PartialEq)]
pub enum Resolution<'a> {
    Match(&'a Connector),
    Ambiguous(Vec<&'a Connector>),
    NotFound,
}

#[derive(Debug, Clone, Default)]
pub struct Registry {
    connectors: Vec<Connector>,
    by_id: HashMap<String, usize>,
}

impl Registry {
    pub fn new(connectors: Vec<Connector>) -> Result<Self, RegistryError> {
        let mut by_id = HashMap::new();
        for (index, connector) in connectors.iter().enumerate() {
            if connector.id.trim().is_empty() {
                return Err(RegistryError::EmptyId);
            }
            if connector.id.contains(char::is_whitespace)
                || connector.id.contains(meta::SEPARATOR)
            {
                return Err(RegistryError::InvalidId(connector.id.clone()));
            }
            // O conector embutido é o próprio aplicativo: não há processo filho
            // nem endereço de rede, então não ter transporte é o esperado.
            if connector.kind != ConnectorKind::Builtin && !connector.has_entry() {
                return Err(RegistryError::NoEntry(connector.id.clone()));
            }
            if by_id.insert(connector.id.clone(), index).is_some() {
                return Err(RegistryError::DuplicateId(connector.id.clone()));
            }
        }
        Ok(Self { connectors, by_id })
    }

    pub fn all(&self) -> &[Connector] {
        &self.connectors
    }

    pub fn get(&self, id: &str) -> Option<&Connector> {
        self.by_id.get(id).map(|&i| &self.connectors[i])
    }

    pub fn by_kind(&self, kind: ConnectorKind) -> Vec<&Connector> {
        self.connectors.iter().filter(|c| c.kind == kind).collect()
    }

    pub fn resolve(&self, asked: &str) -> Resolution<'_> {
        let needle = normalize(asked);
        if needle.is_empty() {
            return Resolution::NotFound;
        }

        let exact: Vec<&Connector> = self
            .connectors
            .iter()
            .filter(|c| normalize(&c.id) == needle || normalize(&c.name) == needle)
            .collect();

        match exact.len() {
            1 => return Resolution::Match(exact[0]),
            n if n > 1 => return Resolution::Ambiguous(exact),
            _ => {}
        }

        let loose: Vec<&Connector> = self
            .connectors
            .iter()
            .filter(|c| {
                let id = normalize(&c.id);
                let name = normalize(&c.name);
                id.contains(&needle)
                    || name.contains(&needle)
                    || c.routes.iter().any(|r| normalize(r).contains(&needle))
            })
            .collect();

        match loose.len() {
            0 => Resolution::NotFound,
            1 => Resolution::Match(loose[0]),
            _ => Resolution::Ambiguous(loose),
        }
    }
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stdio(command: &str) -> McpEntry {
        McpEntry::Stdio {
            command: command.to_string(),
            args: vec![],
            env: HashMap::new(),
        }
    }

    fn connector(id: &str, name: &str, routes: &[&str]) -> Connector {
        Connector {
            id: id.to_string(),
            name: name.to_string(),
            description: String::new(),
            kind: ConnectorKind::Mcp,
            category: ConnectorCategory::Development,
            docs_url: String::new(),
            routes: routes.iter().map(|s| s.to_string()).collect(),
            auth: McpAuth::None,
            entry: Some(stdio("npx")),
            local: HashMap::new(),
            fields: vec![],
            env: vec![],
            tool_allowlist: None,
        }
    }

    fn registry() -> Registry {
        Registry::new(vec![
            connector("filesystem", "Filesystem", &["files", "disk"]),
            connector("github", "GitHub", &["git", "repos"]),
            connector("gitlab", "GitLab", &["git", "repos"]),
        ])
        .expect("fixture must be valid")
    }

    #[test]
    fn unlisted_connector_is_inexistent() {
        let r = registry();
        assert!(r.get("slack").is_none());
        assert_eq!(r.resolve("slack"), Resolution::NotFound);
    }

    #[test]
    fn duplicate_id_is_rejected() {
        let err = Registry::new(vec![
            connector("filesystem", "Filesystem", &[]),
            connector("filesystem", "Other", &[]),
        ])
        .unwrap_err();
        assert_eq!(err, RegistryError::DuplicateId("filesystem".into()));
    }

    #[test]
    fn id_with_namespace_separator_is_rejected() {
        let err = Registry::new(vec![connector("file__system", "Bad", &[])]).unwrap_err();
        assert_eq!(err, RegistryError::InvalidId("file__system".into()));
    }

    #[test]
    fn connector_without_transport_is_rejected() {
        let mut c = connector("broken", "Broken", &[]);
        c.entry = None;
        assert_eq!(
            Registry::new(vec![c]).unwrap_err(),
            RegistryError::NoEntry("broken".into())
        );
    }

    #[test]
    fn resolve_matches_id_ignoring_punctuation_and_case() {
        let r = registry();
        let Resolution::Match(c) = r.resolve("  FILE-SYSTEM! ") else {
            panic!("expected a match");
        };
        assert_eq!(c.id, "filesystem");
    }

    #[test]
    fn resolve_surfaces_ambiguity_instead_of_guessing() {
        let r = registry();
        let Resolution::Ambiguous(matches) = r.resolve("git") else {
            panic!("expected ambiguity");
        };
        let mut ids: Vec<&str> = matches.iter().map(|c| c.id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec!["github", "gitlab"]);
    }

    #[test]
    fn exact_match_wins_over_loose_candidates() {
        let r = registry();
        let Resolution::Match(c) = r.resolve("github") else {
            panic!("expected a match");
        };
        assert_eq!(c.id, "github");
    }

    #[test]
    fn empty_query_is_not_found() {
        assert_eq!(registry().resolve("!!!"), Resolution::NotFound);
    }

    #[test]
    fn per_os_entry_overrides_generic_entry() {
        let mut c = connector("multi", "Multi", &[]);
        c.local.insert(
            DesktopOs::Windows,
            McpEntry::Stdio {
                command: "cmd".into(),
                args: vec!["/c".into()],
                env: HashMap::new(),
            },
        );
        assert!(matches!(
            c.entry_on(DesktopOs::Windows),
            Some(McpEntry::Stdio { command, .. }) if command == "cmd"
        ));
        assert!(matches!(
            c.entry_on(DesktopOs::Linux),
            Some(McpEntry::Stdio { command, .. }) if command == "npx"
        ));
    }

    #[test]
    fn connect_ui_is_derived_from_auth() {
        let mut c = connector("x", "X", &[]);
        assert_eq!(c.connect_ui(), ConnectUi::None);
        c.auth = McpAuth::Token;
        assert_eq!(c.connect_ui(), ConnectUi::Fields);
        c.auth = McpAuth::Oauth;
        assert_eq!(c.connect_ui(), ConnectUi::BrowserHop);
    }

    #[test]
    fn tool_allowlist_is_enforced() {
        let mut c = connector("x", "X", &[]);
        assert!(c.allows_tool("anything"));
        c.tool_allowlist = Some(vec!["read_file".into()]);
        assert!(c.allows_tool("read_file"));
        assert!(!c.allows_tool("delete_file"));
    }

    #[test]
    fn serde_roundtrip_preserves_transport_tag() {
        let c = connector("filesystem", "Filesystem", &["files"]);
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains("\"transport\":\"stdio\""));
        let back: Connector = serde_json::from_str(&json).unwrap();
        assert_eq!(back, c);
    }
}