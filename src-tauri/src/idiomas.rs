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

/// A chave tem texto em todo idioma publicado. Serve para as chaves montadas em
/// runtime (`bancada.caso.{id}`, `servidores.pronto.{id}.nome`), que o teste de
/// paridade não alcança: ele compara os arquivos entre si, e uma chave que falta
/// nos dois passa nele. Sem texto, a tela mostra o caminho da chave como palavra.
pub fn tem_texto(chave: &str) -> bool {
    catalogos().values().all(|c| c.mensagens.contains_key(chave))
}

/// A chave tem texto no idioma atual, com a referência por baixo. Chave ausente
/// cai na referência; ausente nas duas, a própria chave volta — um defeito
/// visível, nunca um texto inventado.
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

    /// A guarda existe para a chave montada em runtime, que a paridade não vê.
    /// Ela só vale se recusar o que não tem texto: aqui está a prova disso.
    #[test]
    fn tem_texto_recusa_o_que_nao_foi_escrito_e_aceita_o_que_esta_la() {
        assert!(
            !tem_texto("bancada.caso.quem-nunca-foi-escrito"),
            "chave sem texto passou na guarda"
        );
        assert!(tem_texto("idioma.nome"), "idioma.nome não está nos catálogos");
    }

    /// A paridade compara um catálogo com o outro e por isso não enxerga a chave
    /// que falta nos dois: foi assim que `erros.motorEnvenenado` entrou no código
    /// sem texto em lugar nenhum. Esta guarda parte do outro lado — lê o que o
    /// próprio código pede e cobra que todo idioma publicado tenha a palavra.
    #[test]
    fn toda_chave_pedida_pelo_codigo_existe_em_todo_idioma() {
        // Chaves montadas em runtime têm `{}` no meio e caem fora da varredura;
        // cada família composta tem guarda própria no módulo que a forma
        // (`bancada::testes`, `mcp::instalador::testes`).
        const EXCECOES: &[&str] = &[
            // não tem texto de propósito: prova que a chave sem catálogo devolve
            // o próprio caminho em vez de inventar palavra
            "nao.existe.em.lugar.nenhum",
        ];

        let raiz = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut faltando: Vec<String> = Vec::new();
        let mut pedidas = 0usize;
        for arquivo in arquivos_rs(&raiz) {
            let codigo = std::fs::read_to_string(&arquivo)
                .unwrap_or_else(|e| panic!("não li {}: {e}", arquivo.display()));
            for chave in chaves_pedidas(&codigo) {
                pedidas += 1;
                if EXCECOES.contains(&chave.as_str()) || tem_texto(&chave) {
                    continue;
                }
                faltando.push(format!("{} → {chave}", arquivo.display()));
            }
        }
        // Um varredor que não acha nada passa verde sobre o defeito que existe
        // para impedir: o piso está aqui, medido, não é uma impressão.
        assert!(
            pedidas > 200,
            "a varredura achou só {pedidas} chaves: o leitor de `tr(\"...\")` quebrou"
        );
        assert!(
            faltando.is_empty(),
            "chaves pedidas pelo código sem texto em algum idioma:\n{}",
            faltando.join("\n")
        );
    }

    /// Caminha a árvore de código-fonte do núcleo procurando os `.rs`.
    fn arquivos_rs(pasta: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut saida = Vec::new();
        let mut pendentes = vec![pasta.to_path_buf()];
        while let Some(atual) = pendentes.pop() {
            let Ok(entradas) = std::fs::read_dir(&atual) else {
                continue;
            };
            for entrada in entradas.flatten() {
                let caminho = entrada.path();
                if caminho.is_dir() {
                    pendentes.push(caminho);
                } else if caminho.extension().is_some_and(|e| e == "rs") {
                    saida.push(caminho);
                }
            }
        }
        saida.sort();
        saida
    }

    /// As chaves pedidas por `tr("...")` num texto-fonte, inclusive quando o
    /// argumento quebra linha depois do parêntese.
    fn chaves_pedidas(codigo: &str) -> Vec<String> {
        let bytes = codigo.as_bytes();
        let mut achadas = Vec::new();
        let mut from = 0usize;
        while let Some(relativo) = codigo[from..].find("tr(") {
            let inicio = from + relativo;
            let anterior = if inicio == 0 { b' ' } else { bytes[inicio - 1] };
            from = inicio + 3;
            // `traduzir(`, `contra(`: só conta o nome exato da função.
            if anterior.is_ascii_alphanumeric() || anterior == b'_' {
                continue;
            }
            let mut abre = from;
            while abre < bytes.len() && matches!(bytes[abre], b' ' | b'\n' | b'\t') {
                abre += 1;
            }
            if abre >= bytes.len() || bytes[abre] != b'"' {
                continue;
            }
            let Some(resto) = codigo[abre + 1..].find('"') else {
                continue;
            };
            let chave = &codigo[abre + 1..abre + 1 + resto];
            from = abre + 1 + resto;
            // runtime não é literal: `tr(&format!("servidores.pronto.{id}.nome"))`
            if chave.contains('{') || chave.contains('}') || !parece_chave(chave) {
                continue;
            }
            achadas.push(chave.to_string());
        }
        achadas
    }

    /// Chave de catálogo é um caminho de identificadores separados por ponto. O
    /// filtro importa porque o texto-fonte também carrega `tr("...")` dentro de
    /// comentário e de mensagem de teste, e um texto assim não pede nada.
    fn parece_chave(chave: &str) -> bool {
        chave.split('.').all(|pedaco| {
            let mut letras = pedaco.chars();
            match letras.next() {
                Some(inicial) if inicial.is_ascii_alphabetic() || inicial == '_' => {
                    pedaco.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                }
                _ => false,
            }
        })
    }

    #[test]
    fn idioma_desconhecido_e_recusado_sem_trocar_o_atual() {
        let antes = atual();
        assert!(!definir("xx-YY"));
        assert_eq!(atual(), antes);
    }
}