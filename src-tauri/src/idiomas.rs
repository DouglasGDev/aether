//! Os idiomas da aplicação, lidos da pasta `locales/` na raiz do projeto.
//!
//! O build embute cada JSON daquela pasta, então o núcleo e a interface leem
//! exatamente as mesmas chaves. Acrescentar um arquivo ali cria um idioma.

use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

include!(concat!(env!("OUT_DIR"), "/idiomas.rs"));

/// Idioma de referência: é contra ele que a paridade das chaves é conferida.
pub const REFERENCIA: &str = "pt-BR";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Idioma {
    pub id: String,
    pub nome: String,
}

struct Catalogo {
    nome: String,
    mensagens: HashMap<String, String>,
}

fn achatar(valor: &Value, prefixo: &str, saida: &mut HashMap<String, String>) {
    match valor {
        Value::Object(mapa) => {
            for (chave, filho) in mapa {
                let caminho = if prefixo.is_empty() {
                    chave.clone()
                } else {
                    format!("{prefixo}.{chave}")
                };
                achatar(filho, &caminho, saida);
            }
        }
        Value::String(texto) => {
            saida.insert(prefixo.to_string(), texto.clone());
        }
        _ => panic!("catálogo de idiomas só aceita texto e objetos, em {prefixo}"),
    }
}

fn catalogos() -> &'static HashMap<String, Catalogo> {
    static CATALOGOS: OnceLock<HashMap<String, Catalogo>> = OnceLock::new();
    CATALOGOS.get_or_init(|| {
        let mut todos = HashMap::new();
        for (id, json) in EMBUTIDOS {
            let valor: Value = serde_json::from_str(json)
                .unwrap_or_else(|e| panic!("locales/{id}.json não é JSON válido: {e}"));
            let mut mensagens = HashMap::new();
            achatar(&valor, "", &mut mensagens);
            let nome = mensagens
                .get("idioma.nome")
                .unwrap_or_else(|| panic!("locales/{id}.json não declara idioma.nome"))
                .clone();
            todos.insert(id.to_string(), Catalogo { nome, mensagens });
        }
        todos
    })
}

fn escolhido() -> &'static RwLock<String> {
    static ATUAL: OnceLock<RwLock<String>> = OnceLock::new();
    ATUAL.get_or_init(|| RwLock::new(REFERENCIA.to_string()))
}

pub fn disponiveis() -> Vec<Idioma> {
    let mut lista: Vec<Idioma> = catalogos()
        .iter()
        .map(|(id, catalogo)| Idioma { id: id.clone(), nome: catalogo.nome.clone() })
        .collect();
    lista.sort_by(|a, b| a.nome.cmp(&b.nome));
    lista
}

pub fn atual() -> String {
    escolhido().read().expect("idioma envenenado").clone()
}

/// Devolve falso para um idioma que não existe, em vez de trocar por outro.
pub fn definir(id: &str) -> bool {
    if !catalogos().contains_key(id) {
        return false;
    }
    *escolhido().write().expect("idioma envenenado") = id.to_string();
    true
}

fn interpolar(texto: &str, args: &[(&str, &str)]) -> String {
    let mut saida = String::with_capacity(texto.len());
    let mut resto = texto;
    while let Some(abre) = resto.find('{') {
        saida.push_str(&resto[..abre]);
        let depois = &resto[abre + 1..];
        let Some(fecha) = depois.find('}') else {
            saida.push('{');
            resto = depois;
            continue;
        };
        let nome = &depois[..fecha];
        match args.iter().find(|(chave, _)| *chave == nome) {
            Some((_, valor)) => saida.push_str(valor),
            // Um argumento esquecido fica visível em vez de virar texto vazio.
            None => {
                saida.push('{');
                saida.push_str(nome);
                saida.push('}');
            }
        }
        resto = &depois[fecha + 1..];
    }
    saida.push_str(resto);
    saida
}

/// Traduz uma chave no idioma atual. Chave ausente cai na referência; ausente
/// nas duas, a própria chave volta — um defeito visível, nunca um texto inventado.
pub fn tr(chave: &str, args: &[(&str, &str)]) -> String {
    let todos = catalogos();
    let idioma = escolhido().read().expect("idioma envenenado").clone();
    let texto = todos
        .get(&idioma)
        .and_then(|c| c.mensagens.get(chave))
        .or_else(|| todos.get(REFERENCIA).and_then(|c| c.mensagens.get(chave)));
    match texto {
        Some(texto) => interpolar(texto, args),
        None => chave.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn todos_os_idiomas_tem_exatamente_as_mesmas_chaves() {
        let todos = catalogos();
        let referencia = &todos[REFERENCIA].mensagens;
        for (id, catalogo) in todos {
            let faltando: Vec<&String> =
                referencia.keys().filter(|k| !catalogo.mensagens.contains_key(*k)).collect();
            let sobrando: Vec<&String> =
                catalogo.mensagens.keys().filter(|k| !referencia.contains_key(*k)).collect();
            assert!(faltando.is_empty(), "{id} não traduz: {faltando:?}");
            assert!(sobrando.is_empty(), "{id} tem chave a mais: {sobrando:?}");
        }
    }

    #[test]
    fn nenhuma_mensagem_fica_vazia() {
        for (id, catalogo) in catalogos() {
            for (chave, texto) in &catalogo.mensagens {
                assert!(!texto.trim().is_empty(), "{id}:{chave} está vazia");
            }
        }
    }

    #[test]
    fn os_placeholders_de_uma_chave_sao_os_mesmos_em_todo_idioma() {
        fn marcadores(texto: &str) -> Vec<String> {
            let mut achados = Vec::new();
            let mut resto = texto;
            while let Some(abre) = resto.find('{') {
                let depois = &resto[abre + 1..];
                let Some(fecha) = depois.find('}') else { break };
                achados.push(depois[..fecha].to_string());
                resto = &depois[fecha + 1..];
            }
            achados.sort();
            achados
        }
        let todos = catalogos();
        let referencia = &todos[REFERENCIA].mensagens;
        for (id, catalogo) in todos {
            for (chave, texto) in &catalogo.mensagens {
                assert_eq!(
                    marcadores(texto),
                    marcadores(&referencia[chave]),
                    "{id}:{chave} usa placeholders diferentes"
                );
            }
        }
    }

    #[test]
    fn a_referencia_existe_e_esta_entre_os_disponiveis() {
        assert!(catalogos().contains_key(REFERENCIA));
        assert!(disponiveis().iter().any(|i| i.id == REFERENCIA));
    }

    #[test]
    fn cada_idioma_declara_o_proprio_nome_nativo() {
        for idioma in disponiveis() {
            assert!(!idioma.nome.trim().is_empty());
            assert_ne!(idioma.nome, idioma.id);
        }
    }

    #[test]
    fn interpolar_substitui_todas_as_ocorrencias() {
        assert_eq!(
            interpolar("{n} de {n} em {pasta}", &[("n", "3"), ("pasta", "C:/x")]),
            "3 de 3 em C:/x"
        );
    }

    #[test]
    fn argumento_esquecido_permanece_visivel() {
        assert_eq!(interpolar("olá {nome}", &[]), "olá {nome}");
    }

    #[test]
    fn chave_inexistente_devolve_a_propria_chave() {
        assert_eq!(tr("nao.existe.em.lugar.nenhum", &[]), "nao.existe.em.lugar.nenhum");
    }

    #[test]
    fn idioma_desconhecido_e_recusado_sem_trocar_o_atual() {
        let antes = atual();
        assert!(!definir("xx-YY"));
        assert_eq!(atual(), antes);
    }
}