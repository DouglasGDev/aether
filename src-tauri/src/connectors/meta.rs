use super::registry::Registry;
use serde::{Deserialize, Serialize};

pub const SEPARATOR: &str = "__";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolMeta {
    pub connector_id: String,
    pub tool: String,
}

pub fn qualified_tool_name(connector_id: &str, tool: &str) -> String {
    format!("{connector_id}{SEPARATOR}{tool}")
}

pub fn parse_qualified_name(name: &str) -> Option<(&str, &str)> {
    let (connector_id, tool) = name.split_once(SEPARATOR)?;
    if connector_id.is_empty() || tool.is_empty() {
        return None;
    }
    Some((connector_id, tool))
}

impl ToolMeta {
    // Revalidated here, not only at listing time: a model can emit a tool name
    // that was never advertised, and the registry is the allowlist.
    pub fn resolve(registry: &Registry, qualified_name: &str) -> Option<Self> {
        let (connector_id, tool) = parse_qualified_name(qualified_name)?;
        let connector = registry.get(connector_id)?;
        if !connector.allows_tool(tool) {
            return None;
        }
        Some(Self {
            connector_id: connector_id.to_string(),
            tool: tool.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::registry::{
        Connector, ConnectorCategory, ConnectorKind, McpAuth, McpEntry,
    };
    use std::collections::HashMap;

    fn registry_with_allowlist(allowlist: Option<Vec<String>>) -> Registry {
        Registry::new(vec![Connector {
            id: "filesystem".into(),
            name: "Filesystem".into(),
            description: String::new(),
            kind: ConnectorKind::Mcp,
            category: ConnectorCategory::Files,
            docs_url: String::new(),
            routes: vec![],
            auth: McpAuth::None,
            entry: Some(McpEntry::Stdio {
                command: "npx".into(),
                args: vec![],
                env: HashMap::new(),
            }),
            local: HashMap::new(),
            fields: vec![],
            env: vec![],
            tool_allowlist: allowlist,
        }])
        .expect("fixture must be valid")
    }

    #[test]
    fn qualified_name_roundtrips() {
        let name = qualified_tool_name("filesystem", "read_file");
        assert_eq!(name, "filesystem__read_file");
        assert_eq!(
            parse_qualified_name(&name),
            Some(("filesystem", "read_file"))
        );
    }

    #[test]
    fn tool_name_containing_separator_is_preserved() {
        let name = qualified_tool_name("filesystem", "a__b");
        assert_eq!(parse_qualified_name(&name), Some(("filesystem", "a__b")));
    }

    #[test]
    fn malformed_names_are_rejected() {
        assert_eq!(parse_qualified_name("read_file"), None);
        assert_eq!(parse_qualified_name("__read_file"), None);
        assert_eq!(parse_qualified_name("filesystem__"), None);
    }

    #[test]
    fn unknown_connector_is_rejected_at_call_time() {
        let r = registry_with_allowlist(None);
        assert_eq!(ToolMeta::resolve(&r, "slack__post_message"), None);
    }

    #[test]
    fn tool_outside_allowlist_is_rejected_at_call_time() {
        let r = registry_with_allowlist(Some(vec!["read_file".into()]));
        assert!(ToolMeta::resolve(&r, "filesystem__read_file").is_some());
        assert_eq!(ToolMeta::resolve(&r, "filesystem__delete_file"), None);
    }

    #[test]
    fn absent_allowlist_permits_advertised_tools() {
        let r = registry_with_allowlist(None);
        let meta = ToolMeta::resolve(&r, "filesystem__anything").expect("must resolve");
        assert_eq!(meta.connector_id, "filesystem");
        assert_eq!(meta.tool, "anything");
    }
}