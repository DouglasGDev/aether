use super::tool::ToolInfo;

fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(|token| token.to_ascii_lowercase())
        .collect()
}

// Lexical matching, not a model: every query token must appear in the tool name
// or its description. All matches are returned so the UI can surface a choice
// instead of silently guessing which tool the user meant.
pub fn rank(tools: &[ToolInfo], query: &str) -> Vec<ToolInfo> {
    let needles = tokens(query);
    if needles.is_empty() {
        return Vec::new();
    }

    let mut scored: Vec<(usize, usize, &ToolInfo)> = Vec::new();
    for tool in tools {
        let name = tool.name.to_ascii_lowercase();
        let description = tool.description.to_ascii_lowercase();
        let mut name_hits = 0usize;
        let mut matched = true;
        for needle in &needles {
            if name.contains(needle.as_str()) {
                name_hits += 1;
            } else if !description.contains(needle.as_str()) {
                matched = false;
                break;
            }
        }
        if matched {
            scored.push((name_hits, name.len(), tool));
        }
    }

    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(a.1.cmp(&b.1))
            .then(a.2.qualified_name.cmp(&b.2.qualified_name))
    });
    scored.into_iter().map(|(_, _, tool)| tool.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::ToolEffect;
    use serde_json::json;

    fn tool(name: &str, description: &str) -> ToolInfo {
        ToolInfo {
            qualified_name: format!("srv__{name}"),
            connector_id: "srv".into(),
            name: name.into(),
            titulo: None,
            description: description.into(),
            effect: ToolEffect::ReadOnly,
            input_schema: json!({}),
        }
    }

    #[test]
    fn empty_query_matches_nothing() {
        let tools = vec![tool("echo", "Echoes back the input string")];
        assert!(rank(&tools, "   ").is_empty());
    }

    #[test]
    fn name_match_outranks_description_match() {
        let tools = vec![
            tool("get-sum", "Adds two numbers"),
            tool("echo", "Echoes back the input string"),
        ];
        let ranked = rank(&tools, "echo");
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].name, "echo");
    }

    #[test]
    fn every_token_must_match() {
        let tools = vec![tool("echo", "Echoes back the input string")];
        assert!(rank(&tools, "echo banana").is_empty());
    }

    #[test]
    fn multi_word_query_matches_hyphenated_name() {
        let tools = vec![tool("get-sum", "Adds two numbers")];
        let ranked = rank(&tools, "get sum");
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].name, "get-sum");
    }

    #[test]
    fn description_is_searchable() {
        let tools = vec![tool("get-sum", "Adds two numbers")];
        let ranked = rank(&tools, "numbers");
        assert_eq!(ranked.len(), 1);
    }

    #[test]
    fn ambiguous_matches_are_all_returned() {
        let tools = vec![
            tool("read-file", "Reads a file"),
            tool("read-dir", "Reads a directory"),
        ];
        let ranked = rank(&tools, "read");
        assert_eq!(ranked.len(), 2);
    }

    #[test]
    fn shorter_name_wins_a_tie() {
        let tools = vec![tool("sum-all", "Adds"), tool("sum", "Adds")];
        let ranked = rank(&tools, "sum");
        assert_eq!(ranked[0].name, "sum");
    }
}