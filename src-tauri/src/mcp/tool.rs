use crate::connectors::qualified_tool_name;
use crate::permissions::ToolEffect;
use rmcp::model::{Tool, ToolAnnotations};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolInfo {
    pub qualified_name: String,
    pub connector_id: String,
    pub name: String,
    /// Nome legível no idioma atual. Uma ferramenta de servidor de terceiros não
    /// tem título nosso: a interface usa o nome dela.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub titulo: Option<String>,
    pub description: String,
    pub effect: ToolEffect,
    pub input_schema: serde_json::Value,
}

// An unannotated tool is treated as mutating: a server that says nothing about
// its effects has not earned read-only trust.
pub fn classify_effect(annotations: Option<&ToolAnnotations>) -> ToolEffect {
    let Some(annotations) = annotations else {
        return ToolEffect::Mutating;
    };
    if annotations.destructive_hint == Some(true) {
        ToolEffect::Destructive
    } else if annotations.read_only_hint == Some(true) {
        ToolEffect::ReadOnly
    } else {
        ToolEffect::Mutating
    }
}

impl ToolInfo {
    pub fn from_rmcp(connector_id: &str, tool: &Tool) -> Self {
        let name = tool.name.to_string();
        Self {
            qualified_name: qualified_tool_name(connector_id, &name),
            connector_id: connector_id.to_string(),
            name,
            titulo: None,
            description: tool.description.as_deref().unwrap_or("").to_string(),
            effect: classify_effect(tool.annotations.as_ref()),
            input_schema: serde_json::to_value(&tool.input_schema)
                .expect("MCP input schema is always JSON"),
        }
    }

    /// Uma ferramenta do conector embutido: o efeito é declarado pelo próprio
    /// aplicativo, não deduzido de anotações de um servidor de terceiros.
    pub fn local(connector_id: &str, ferramenta: &crate::local::Ferramenta) -> Self {
        Self {
            qualified_name: qualified_tool_name(connector_id, ferramenta.nome),
            connector_id: connector_id.to_string(),
            name: ferramenta.nome.to_string(),
            titulo: Some(ferramenta.titulo.clone()),
            description: ferramenta.descricao.clone(),
            effect: ferramenta.efeito,
            input_schema: ferramenta.esquema.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn annotations(read_only: Option<bool>, destructive: Option<bool>) -> ToolAnnotations {
        let mut annotations = ToolAnnotations::default();
        annotations.read_only_hint = read_only;
        annotations.destructive_hint = destructive;
        annotations
    }

    #[test]
    fn missing_annotations_default_to_mutating() {
        assert_eq!(classify_effect(None), ToolEffect::Mutating);
    }

    #[test]
    fn empty_annotations_default_to_mutating() {
        assert_eq!(
            classify_effect(Some(&annotations(None, None))),
            ToolEffect::Mutating
        );
    }

    #[test]
    fn read_only_hint_is_honoured() {
        assert_eq!(
            classify_effect(Some(&annotations(Some(true), None))),
            ToolEffect::ReadOnly
        );
    }

    #[test]
    fn destructive_hint_wins_over_read_only() {
        assert_eq!(
            classify_effect(Some(&annotations(Some(true), Some(true)))),
            ToolEffect::Destructive
        );
    }

    #[test]
    fn false_hints_do_not_grant_read_only() {
        assert_eq!(
            classify_effect(Some(&annotations(Some(false), Some(false)))),
            ToolEffect::Mutating
        );
    }
}