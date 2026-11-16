use crate::idiomas::tr;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntry {
    pub qualified_name: String,
    pub connector_id: String,
    pub description: String,
}

// Deliberately an estimate, not a tokenizer: the budget only needs to be
// conservative enough that a small local model never loses its whole context
// to the tool catalog.
pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

pub struct CatalogBudget {
    pub max_tokens: usize,
}

impl Default for CatalogBudget {
    fn default() -> Self {
        Self { max_tokens: 1024 }
    }
}

pub fn render(entries: &[CatalogEntry], budget: &CatalogBudget) -> String {
    let mut sorted: Vec<&CatalogEntry> = entries.iter().collect();
    sorted.sort_by(|a, b| a.qualified_name.cmp(&b.qualified_name));

    let header = tr("catalogo.cabecalho", &[]);
    let mut used = estimate_tokens(&header);
    let mut lines: Vec<String> = Vec::new();
    let mut omitted = 0usize;

    for entry in sorted {
        let line = format!("{} — {}", entry.qualified_name, entry.description);
        let cost = estimate_tokens(&line) + 1;
        if used + cost > budget.max_tokens {
            omitted += 1;
            continue;
        }
        used += cost;
        lines.push(line);
    }

    let mut out = header;
    for line in lines {
        out.push('\n');
        out.push_str(&line);
    }
    if omitted > 0 {
        out.push('\n');
        out.push_str(&tr("catalogo.omitidas", &[("n", &omitted.to_string())]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, description: &str) -> CatalogEntry {
        CatalogEntry {
            qualified_name: name.into(),
            connector_id: name.split("__").next().unwrap_or(name).into(),
            description: description.into(),
        }
    }

    fn cabecalho() -> String {
        tr("catalogo.cabecalho", &[])
    }

    fn nota(omitidas: usize) -> String {
        tr("catalogo.omitidas", &[("n", &omitidas.to_string())])
    }

    #[test]
    fn empty_catalog_still_renders_header() {
        let out = render(&[], &CatalogBudget::default());
        assert_eq!(out, cabecalho());
    }

    #[test]
    fn entries_are_sorted_for_deterministic_prompts() {
        let entries = vec![
            entry("github__create_issue", "Create an issue"),
            entry("filesystem__read_file", "Read a file"),
        ];
        let out = render(&entries, &CatalogBudget { max_tokens: 4096 });
        let fs = out.find("filesystem__read_file").unwrap();
        let gh = out.find("github__create_issue").unwrap();
        assert!(fs < gh);
    }

    #[test]
    fn everything_fits_when_budget_is_generous() {
        let entries = vec![entry("a__b", "short"), entry("c__d", "short")];
        let out = render(&entries, &CatalogBudget { max_tokens: 4096 });
        assert_eq!(out.lines().count(), 3, "cabeçalho e as duas ferramentas");
        assert!(out.contains("a__b"));
        assert!(out.contains("c__d"));
    }

    #[test]
    fn truncation_is_announced_not_silent() {
        let entries: Vec<CatalogEntry> = (0..50)
            .map(|i| entry(&format!("srv__tool_{i}"), &"x".repeat(200)))
            .collect();
        let out = render(&entries, &CatalogBudget { max_tokens: 128 });
        let linhas: Vec<&str> = out.lines().collect();
        let titulo = cabecalho();
        assert_eq!(linhas.first().copied(), Some(titulo.as_str()));
        let cabidas = linhas.len() - 2;
        assert!((1..50).contains(&cabidas), "coube parte, não tudo: {cabidas}");
        assert_eq!(
            linhas.last().copied(),
            Some(nota(50 - cabidas).as_str()),
            "a nota precisa contar exatamente o que ficou de fora"
        );
        assert!(estimate_tokens(&out) < 200);
    }

    #[test]
    fn zero_budget_omits_everything_but_says_so() {
        let entries = vec![entry("a__b", "desc")];
        let out = render(&entries, &CatalogBudget { max_tokens: 0 });
        assert!(!out.contains("a__b"));
        assert_eq!(out.lines().last(), Some(nota(1).as_str()));
    }

    #[test]
    fn token_estimate_is_conservative_for_short_strings() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
    }
}