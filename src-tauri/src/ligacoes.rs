//! O que o grafo desenha é o que o núcleo executa.
//!
//! Não há política paralela aqui: um plano de ligações é compilado na
//! `tool_allowlist` de cada conector (que `ToolMeta::resolve` já cobra em toda
//! chamada) e nas pastas concedidas (que a contenção de caminho já cobra). O
//! arquivo do grafo guarda apenas onde cada nó foi colocado.

use crate::connectors::Registry;
use crate::idiomas::tr;
use crate::permissions::ToolEffect;
use serde::{Deserialize, Serialize};

/// Uma linha do grafo: o conector e as ferramentas dele presas ao Aether.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ligacao {
    pub id: String,
    pub ferramentas: Vec<String>,
}

/// O plano inteiro que a tela de ligações compila.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plano {
    #[serde(default)]
    pub conectores: Vec<Ligacao>,
    #[serde(default)]
    pub pastas: Vec<String>,
}

/// Uma ferramenta visível no grafo: a porta do nó.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Porta {
    pub nome: String,
    /// O nome qualificado que o núcleo cobra em cada chamada
    /// (`conector__ferramenta`). É a identidade do fio: a tela o usa como chave
    /// e o evento de turno o usa para dizer qual sinapse disparou. Um segundo
    /// identificador criado aqui só poderia divergir deste.
    pub chave: String,
    pub titulo: String,
    pub descricao: String,
    pub efeito: ToolEffect,
    /// Derivada da allowlist real do conector, nunca de um estado desenhado.
    pub ligada: bool,
}

/// Um nó de conector, com o que ele anunciou e o que está ligado hoje.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct No {
    pub id: String,
    pub nome: String,
    pub descricao: String,
    pub conectado: bool,
    pub portas: Vec<Porta>,
}

/// A posição de um nó, desenhada pelo usuário e guardada entre sessões. Nada
/// aqui é política: apagar o arquivo não muda o que o núcleo permite.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Posicao {
    pub id: String,
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Layout {
    #[serde(default)]
    pub posicoes: Vec<Posicao>,
}

impl Layout {
    pub fn de(caminho: &std::path::Path) -> Self {
        std::fs::read_to_string(caminho)
            .ok()
            .and_then(|texto| serde_json::from_str(&texto).ok())
            .unwrap_or_default()
    }

    pub fn guardar(&self, caminho: &std::path::Path) -> anyhow::Result<()> {
        use anyhow::Context;
        if let Some(pai) = caminho.parent() {
            std::fs::create_dir_all(pai).with_context(|| {
                tr("erros.falhaCriarPasta", &[("caminho", &pai.display().to_string())])
            })?;
        }
        let texto = serde_json::to_string_pretty(self)
            .context(tr("erros.falhaSalvarConfig", &[]))?;
        // Um arquivo de desenho corrompido custa só o desenho; escrito de uma vez,
        // não há metade de uma posição sobrevivendo a uma queda de energia.
        let temporario = caminho.with_extension("json.tmp");
        std::fs::write(&temporario, &texto).with_context(|| {
            tr("erros.falhaEscreverConfig", &[("caminho", &temporario.display().to_string())])
        })?;
        std::fs::rename(&temporario, caminho).with_context(|| {
            tr("erros.falhaSubstituirConfig", &[("caminho", &caminho.display().to_string())])
        })?;
        Ok(())
    }

    pub fn posicao(&self, id: &str) -> Option<(f64, f64)> {
        self.posicoes
            .iter()
            .find(|p| p.id == id)
            .map(|p| (p.x, p.y))
    }
}

/// Compila o plano nas allowlists a escrever.
///
/// `anunciadas` devolve o que o núcleo sabe que aquele conector oferece: uma
/// ferramenta fora dessa lista é recusada, porque o grafo não pode inventar uma
/// capacidade que ninguém anunciou. Um conector sem nada anunciado fica de fora
/// — ligar o que não se vê seria desenhar política em cima de suposição.
pub fn recortar(
    plano: &Plano,
    registry: &Registry,
    anunciadas: &dyn Fn(&str) -> Vec<String>,
) -> Result<Vec<(String, Vec<String>)>, String> {
    let mut saida = Vec::new();
    for ligacao in &plano.conectores {
        if registry.get(&ligacao.id).is_none() {
            return Err(tr("erros.servidorNaoRegistrado", &[("id", &ligacao.id)]));
        }
        let portas = anunciadas(&ligacao.id);
        if portas.is_empty() {
            if ligacao.ferramentas.is_empty() {
                continue;
            }
            return Err(tr("erros.ferramentaSemServidorConectado", &[
                ("id", &ligacao.id)
            ]));
        }
        for ferramenta in &ligacao.ferramentas {
            if !portas.contains(ferramenta) {
                return Err(tr("erros.ferramentaNaoAnunciada", &[
                    ("ferramenta", ferramenta),
                    ("id", &ligacao.id),
                ]));
            }
        }
        saida.push((ligacao.id.clone(), ligacao.ferramentas.clone()));
    }
    Ok(saida)
}

#[cfg(test)]
mod testes {
    use super::*;
    use crate::connectors::{Connector, ConnectorCategory, ConnectorKind, McpAuth, McpEntry};
    use std::collections::HashMap;

    fn servidor(id: &str) -> Connector {
        Connector {
            id: id.into(),
            name: id.into(),
            description: String::new(),
            kind: ConnectorKind::Mcp,
            category: ConnectorCategory::Development,
            docs_url: String::new(),
            routes: vec![],
            auth: McpAuth::None,
            entry: Some(McpEntry::Stdio {
                command: "node".into(),
                args: vec![],
                env: HashMap::new(),
            }),
            local: HashMap::new(),
            fields: vec![],
            env: vec![],
            tool_allowlist: None,
        }
    }

    fn registro(ids: &[&str]) -> Registry {
        Registry::new(ids.iter().map(|i| servidor(i)).collect())
            .expect("registro de teste")
    }

    fn anunciadas(map: &[(&str, &[&str])]) -> impl Fn(&str) -> Vec<String> {
        let map: HashMap<String, Vec<String>> = map
            .iter()
            .map(|(id, ferramentas)| {
                (
                    id.to_string(),
                    ferramentas.iter().map(|f| f.to_string()).collect(),
                )
            })
            .collect();
        move |id: &str| map.get(id).cloned().unwrap_or_default()
    }

    fn plano(conector: &str, ferramentas: &[&str]) -> Plano {
        Plano {
            conectores: vec![Ligacao {
                id: conector.into(),
                ferramentas: ferramentas.iter().map(|f| f.to_string()).collect(),
            }],
            pastas: vec![],
        }
    }

    #[test]
    fn o_que_eh_desenhado_vira_allowlist_verbatim() {
        let registry = registro(&["memory"]);
        let portas = anunciadas(&[("memory", &["read_graph", "write_graph"])]);
        let compilado =
            recortar(&plano("memory", &["read_graph"]), &registry, &portas).unwrap();
        assert_eq!(compilado, vec![("memory".to_string(), vec!["read_graph".to_string()])]);
    }

    #[test]
    fn desligar_tudo_e_escrito_como_lista_vazia() {
        // Ligação nenhuma é uma decisão, não uma ausência: `None` significaria
        // "tudo o que ele anunciar amanhã" e religaria sozinho o que foi cortado.
        let registry = registro(&["memory"]);
        let portas = anunciadas(&[("memory", &["read_graph"])]);
        let compilado = recortar(&plano("memory", &[]), &registry, &portas).unwrap();
        assert_eq!(compilado, vec![("memory".to_string(), vec![])]);
    }

    #[test]
    fn ferramenta_que_ninguem_anunciou_e_recusada() {
        let registry = registro(&["memory"]);
        let portas = anunciadas(&[("memory", &["read_graph"])]);
        let erro = recortar(&plano("memory", &["apagar_disco"]), &registry, &portas).unwrap_err();
        assert!(erro.contains("apagar_disco"), "{erro}");
        assert!(!erro.starts_with("erros."), "mensagem crua: {erro}");
    }

    #[test]
    fn conector_fora_do_cadastro_e_recusado() {
        let registry = registro(&["memory"]);
        let portas = anunciadas(&[]);
        let erro = recortar(&plano("fantasma", &[]), &registry, &portas).unwrap_err();
        assert!(erro.contains("fantasma"), "{erro}");
    }

    #[test]
    fn servidor_desligado_sem_portas_nao_reescreve_a_allowlist() {
        // Um servidor do catálogo ainda não conectado não tem portas: ele tem de
        // continuar como está, senão aplicar o grafo o deixaria mudo para sempre.
        let registry = registro(&["memory"]);
        let portas = anunciadas(&[]);
        let compilado = recortar(&plano("memory", &[]), &registry, &portas).unwrap();
        assert!(compilado.is_empty(), "{compilado:?}");
    }

    #[test]
    fn ligar_um_servidor_desligado_ped_conexao_anterior() {
        let registry = registro(&["memory"]);
        let portas = anunciadas(&[]);
        let erro = recortar(&plano("memory", &["read_graph"]), &registry, &portas).unwrap_err();
        assert!(!erro.starts_with("erros."), "mensagem crua: {erro}");
    }

    #[test]
    fn layout_guarda_so_onde_os_nois_estao() {
        let pasta = std::env::temp_dir().join(format!("aether-layout-{}", std::process::id()));
        std::fs::create_dir_all(&pasta).unwrap();
        let arquivo = pasta.join("ligacoes.json");
        let layout = Layout {
            posicoes: vec![Posicao { id: "memory".into(), x: 12.5, y: -8.0 }],
        };
        layout.guardar(&arquivo).unwrap();
        let lido = Layout::de(&arquivo);
        assert_eq!(lido.posicao("memory"), Some((12.5, -8.0)));
        assert_eq!(lido.posicao("ausente"), None);
        // Um arquivo que não existe é o estado inicial, não um erro: a primeira
        // vez que alguém abre o grafo não tem desenho nenhum.
        assert!(Layout::de(&pasta.join("inexistente.json")).posicoes.is_empty());
        std::fs::remove_dir_all(&pasta).ok();
    }
}
