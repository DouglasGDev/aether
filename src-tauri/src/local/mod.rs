//! As capacidades locais do Aether: arquivos, terminal, memória e informações da
//! máquina.
//!
//! São ferramentas de verdade, executadas dentro do próprio aplicativo, e passam
//! pelo mesmo gate de permissões e pela mesma fila de aprovação de um servidor
//! MCP. Nada é simulado: sem pasta concedida a ferramenta recusa, e um comando
//! que falhou volta com o código de saída real.

pub mod arquivos;
pub mod base;
pub mod memoria;
pub mod pastas;
pub mod sistema;
pub mod terminal;
pub mod vista;
pub mod web;

use crate::ai::memoria::Custodio;
use crate::connectors::{Connector, ConnectorCategory, ConnectorKind, McpAuth};
use crate::idiomas::tr;
use crate::permissions::ToolEffect;
use anyhow::{Result, bail};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
pub use vista::Vista;

/// Identificador do conector embutido. Não é um servidor MCP: não há processo
/// filho nem rede, é o próprio aplicativo executando.
pub const CONECTOR: &str = "computador";

/// O que uma ferramenta devolve: o texto que vai ao modelo e o cartão que a
/// interface desenha. Os dois saem do mesmo resultado real.
#[derive(Debug)]
pub struct Saida {
    pub texto: String,
    pub vista: Vista,
    pub erro: bool,
}

impl Saida {
    pub fn nova(texto: impl Into<String>, vista: Vista) -> Self {
        Self { texto: texto.into(), vista, erro: false }
    }
}

pub struct Ferramenta {
    pub nome: &'static str,
    pub titulo: String,
    pub descricao: String,
    pub efeito: ToolEffect,
    pub esquema: Value,
}

/// Um campo do esquema. `title` é o nome legível no idioma atual, e a interface
/// o usa no lugar de adivinhar a partir da chave; `description` é o que o modelo lê.
pub(crate) fn campo(tipo: &str, chave: &str) -> Value {
    json!({
        "type": tipo,
        "title": tr(&format!("argumentos.{chave}"), &[]),
        "description": tr(&format!("argumentos.{chave}Descricao"), &[]),
    })
}

pub(crate) fn objeto(propriedades: Value, obrigatorios: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": propriedades,
        "required": obrigatorios,
        "additionalProperties": false,
    })
}

pub(crate) fn ferramenta(
    nome: &'static str,
    chave: &str,
    efeito: ToolEffect,
    esquema: Value,
) -> Ferramenta {
    Ferramenta {
        nome,
        titulo: tr(&format!("ferramentas.{chave}.titulo"), &[]),
        descricao: tr(&format!("ferramentas.{chave}.descricao"), &[]),
        efeito,
        esquema,
    }
}

/// O catálogo local do aplicativo, no idioma da vez e com a rede como ela está
/// ligada agora.
///
/// A escolha mora em `rede.json` e é lida uma vez na abertura (`web::carregar`),
/// mas é consultada aqui, a cada montagem: um catálogo velho em memória é uma
/// ferramenta que continua aparecendo para o modelo depois de o usuário ter
/// desligado a porta.
pub fn catalogo() -> Vec<Ferramenta> {
    catalogo_com_rede(&web::rede_atual())
}

/// O catálogo com a escolha escrita à mão. Separada da memória global porque a
/// memória é uma só por processo e os testes correm em paralelo: ligá-la num
/// teste mudaria a resposta de todos os outros, e isto é exatamente o que a
/// porta de saída não pode permitir por acidente.
pub fn catalogo_com_rede(rede: &web::Rede) -> Vec<Ferramenta> {
    let mut ferramentas = catalogo_base();
    if rede.ativa {
        ferramentas.push(web::ferramenta());
    }
    ferramentas
}

fn catalogo_base() -> Vec<Ferramenta> {
    use ToolEffect::{Destructive, Mutating, ReadOnly};
    vec![
        ferramenta(
            "ler_arquivo",
            "lerArquivo",
            ReadOnly,
            objeto(
                json!({
                    "caminho": campo("string", "caminho"),
                    "inicio": campo("integer", "inicio"),
                    "fim": campo("integer", "fim"),
                }),
                &["caminho"],
            ),
        ),
        ferramenta(
            "listar_pasta",
            "listarPasta",
            ReadOnly,
            objeto(
                json!({
                    "caminho": campo("string", "caminho"),
                    "padrao": campo("string", "padrao"),
                }),
                &["caminho"],
            ),
        ),
        ferramenta(
            "buscar_texto",
            "buscarTexto",
            ReadOnly,
            objeto(
                json!({
                    "termo": campo("string", "termo"),
                    "caminho": campo("string", "caminho"),
                    "recursivo": campo("boolean", "recursivo"),
                }),
                &["termo"],
            ),
        ),
        ferramenta(
            "buscar_memoria",
            "buscarMemoria",
            ReadOnly,
            objeto(
                json!({
                    "consulta": campo("string", "consulta"),
                    "trechos": campo("integer", "trechos"),
                }),
                &["consulta"],
            ),
        ),
        ferramenta(
            "ler_resultado",
            "lerResultado",
            ReadOnly,
            objeto(
                json!({
                    "id": campo("string", "id"),
                    "desde": campo("integer", "desde"),
                    "quantos": campo("integer", "quantos"),
                }),
                &["id"],
            ),
        ),
        ferramenta(
            "escrever_arquivo",
            "escreverArquivo",
            Mutating,
            objeto(
                json!({
                    "caminho": campo("string", "caminho"),
                    "conteudo": campo("string", "conteudo"),
                }),
                &["caminho", "conteudo"],
            ),
        ),
        ferramenta(
            "editar_arquivo",
            "editarArquivo",
            Mutating,
            objeto(
                json!({
                    "caminho": campo("string", "caminho"),
                    "trocas": {
                        "type": "array",
                        "title": tr("argumentos.trocas", &[]),
                        "description": tr("argumentos.trocasDescricao", &[]),
                        "items": {
                            "type": "object",
                            "properties": {
                                "de": campo("string", "de"),
                                "para": campo("string", "para"),
                            },
                            "required": ["de", "para"],
                        },
                    },
                }),
                &["caminho", "trocas"],
            ),
        ),
        ferramenta(
            "criar_pasta",
            "criarPasta",
            Mutating,
            objeto(json!({ "caminho": campo("string", "caminho") }), &["caminho"]),
        ),
        ferramenta(
            "mover",
            "mover",
            Mutating,
            objeto(
                json!({
                    "origem": campo("string", "origem"),
                    "destino": campo("string", "destino"),
                }),
                &["origem", "destino"],
            ),
        ),
        ferramenta(
            "apagar",
            "apagar",
            Destructive,
            objeto(json!({ "caminho": campo("string", "caminho") }), &["caminho"]),
        ),
        // Escrever na base é `Mutating`, e não `Destructive`: a nota nova não tira
        // nada do que estava lá, e reescrever pelo id que o pedido diz é decisão
        // do dono, não acidente. O arquivo cai na pasta do aplicativo, nunca numa
        // pasta concedida — a porta não recebe caminho nenhum como argumento.
        ferramenta(
            "escrever_na_base",
            "escreverNaBase",
            Mutating,
            objeto(
                json!({
                    "titulo": campo("string", "titulo"),
                    "corpo": campo("string", "corpo"),
                    "nota": campo("string", "nota"),
                    "fixar": campo("boolean", "fixar"),
                }),
                &["titulo", "corpo"],
            ),
        ),
        ferramenta(
            "executar_comando",
            "executarComando",
            Mutating,
            objeto(
                json!({
                    "comando": campo("string", "comando"),
                    "caminho": campo("string", "caminho"),
                    "tempoLimite": campo("integer", "tempoLimite"),
                }),
                &["comando"],
            ),
        ),
        ferramenta("info_sistema", "infoSistema", ReadOnly, objeto(json!({}), &[])),
    ]
}

/// O conector embutido entra no mesmo cadastro que os servidores MCP, então a
/// allowlist, o gate e a aprovação valem para ele sem nenhum caminho paralelo.
pub fn conector() -> Connector {
    Connector {
        id: CONECTOR.to_string(),
        name: tr("conectorLocal.nome", &[]),
        description: tr("conectorLocal.descricao", &[]),
        kind: ConnectorKind::Builtin,
        category: ConnectorCategory::System,
        docs_url: String::new(),
        routes: vec![
            "arquivos".into(),
            "pastas".into(),
            "terminal".into(),
            "comando".into(),
            "código".into(),
            "memória".into(),
            "sistema".into(),
        ],
        auth: McpAuth::None,
        entry: None,
        local: HashMap::new(),
        fields: vec![],
        env: vec![],
        tool_allowlist: None,
    }
}

pub fn texto(args: &Map<String, Value>, chave: &str) -> Option<String> {
    match args.get(chave) {
        Some(Value::String(texto)) if !texto.trim().is_empty() => Some(texto.clone()),
        // O modelo às vezes escreve um número onde o esquema pede texto.
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

pub fn obrigatorio(args: &Map<String, Value>, chave: &str) -> Result<String> {
    texto(args, chave).ok_or_else(|| {
        anyhow::anyhow!(tr("erros.argumentoFaltando", &[("nome", chave)]))
    })
}

pub fn inteiro(args: &Map<String, Value>, chave: &str) -> Option<usize> {
    match args.get(chave) {
        Some(Value::Number(n)) => n.as_u64().map(|v| v as usize),
        Some(Value::String(s)) => s.trim().parse::<usize>().ok(),
        _ => None,
    }
}

pub fn booleano(args: &Map<String, Value>, chave: &str, padrao: bool) -> bool {
    match args.get(chave) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s.trim().eq_ignore_ascii_case("true"),
        _ => padrao,
    }
}

/// Executa uma ferramenta local. Síncrono de propósito: quem chama roda isto
/// numa tarefa bloqueante, para não prender o runtime assíncrono do aplicativo.
///
/// `derrame` é a pasta onde o laço de conversa guarda os resultados longos. Só
/// `ler_resultado` usa, mas ela entra pela mesma porta das outras porque o
/// depósito é uma capacidade do computador, não um atalho por fora dele.
pub fn executar(
    nome: &str,
    argumentos: Value,
    pastas: &pastas::Pastas,
    memoria: &Custodio,
    derrame: &Path,
) -> Result<Saida> {
    let args = match argumentos {
        Value::Object(mapa) => mapa,
        _ => bail!(tr("erros.argumentosInvalidos", &[])),
    };
    match nome {
        "ler_arquivo" => arquivos::ler(&args, pastas),
        "listar_pasta" => arquivos::listar(&args, pastas),
        "buscar_texto" => arquivos::buscar(&args, pastas),
        "buscar_memoria" => memoria::buscar(&args, memoria),
        "ler_resultado" => crate::ai::derrame::ferramenta(&args, derrame),
        "escrever_arquivo" => arquivos::escrever(&args, pastas),
        "editar_arquivo" => arquivos::editar(&args, pastas),
        "criar_pasta" => arquivos::criar_pasta(&args, pastas),
        "mover" => arquivos::mover(&args, pastas),
        "apagar" => arquivos::apagar(&args, pastas),
        "escrever_na_base" => base::escrever(&args, memoria),
        "executar_comando" => terminal::executar(&args, pastas),
        "info_sistema" => sistema::informacoes(pastas),
        // A única que fala com o mundo. Sem `rede.json` ligado ela não está no
        // catálogo e, mesmo chamada à mão pelo nome, recusa antes de abrir
        // soquete — o portão não é o catálogo, é a execução.
        "buscar_na_web" => web::buscar(&args),
        outra => {
            // O nome errado e o nome bloqueado são fatos diferentes, e o modelo
            // só corrige o primeiro se a resposta disser quais são as ferramentas.
            let lista = catalogo()
                .into_iter()
                .map(|t| format!("{CONECTOR}__{}", t.nome))
                .collect::<Vec<_>>()
                .join(", ");
            bail!(tr(
                "erros.ferramentaInexistente",
                &[("nome", &format!("{CONECTOR}__{outra}")), ("lista", &lista)]
            ))
        }
    }
}

/// Raiz de trabalho: a primeira pasta concedida, onde um comando roda sem que
/// o modelo precise dizer onde.
pub fn raiz_de_trabalho(pastas: &pastas::Pastas) -> Option<PathBuf> {
    pastas.raizes.first().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Um custódio de teste, sem nada instalado. As ferramentas de arquivo não o
    /// usam; ele existe porque a mesma porta de execução atende as duas.
    fn memoria() -> Custodio {
        let raiz = std::env::temp_dir().join(format!("aether-local-{}", std::process::id()));
        Custodio::novo(
            &[],
            &raiz.join("modelos"),
            &raiz.join("memoria"),
            raiz.join("rag.json"),
            &raiz.join(crate::ai::base::PASTA),
            std::sync::Arc::new(std::sync::RwLock::new(pastas::Pastas::default())),
        )
    }

    /// A pasta do depósito usada pelos testes de execução. Cada um tem a sua,
    /// porque a poda dos resultados mais antigos é shared entre todos.
    fn deposito(nome: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("aether-deposito-{nome}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    /// Nome errado não é bloqueio: a resposta tem de dizer quais ferramentas
    /// existem, senão o modelo procura uma porta alternativa.
    #[test]
    fn nome_desconhecido_devolve_a_lista_de_nomes_reais() {
        let pastas = pastas::Pastas::default();
        let erro = executar(
            "write_file",
            Value::Object(Default::default()),
            &pastas,
            &memoria(),
            &deposito("nome"),
        )
        .expect_err("nome inexistente não roda");
        let texto = format!("{erro:#}");
        assert!(texto.contains("computador__write_file"), "{texto}");
        assert!(texto.contains("computador__escrever_arquivo"), "{texto}");
    }

    #[test]
    fn o_catalogo_cobre_arquivos_memoria_base_resultados_terminal_e_sistema() {
        let nomes: Vec<&str> = catalogo().iter().map(|f| f.nome).collect();
        for esperado in [
            "ler_arquivo",
            "listar_pasta",
            "buscar_texto",
            "buscar_memoria",
            "ler_resultado",
            "escrever_arquivo",
            "editar_arquivo",
            "criar_pasta",
            "mover",
            "apagar",
            "escrever_na_base",
            "executar_comando",
            "info_sistema",
        ] {
            assert!(nomes.contains(&esperado), "falta {esperado}");
        }
        // Treze, e não catorze: `buscar_na_web` não está aqui porque a rede está
        // desligada neste processo. O número é a prova de que desligado significa
        // ausente, e não presente-e-recusando.
        assert_eq!(nomes.len(), 13);
        assert!(!nomes.contains(&"buscar_na_web"), "a rede ligou sozinha no catálogo");
    }

    /// A porta de saída só existe se o arquivo dizer que existe. Este é o único
    /// lugar do aplicativo onde uma ferramenta entra no catálogo por uma escolha
    /// do usuário — e é por isso que ele é testado pelas duas pontas.
    #[test]
    fn a_rede_ligada_e_a_unica_ferramenta_que_entre_e_saia_do_catalogo() {
        let desligada = catalogo_com_rede(&web::Rede { ativa: false });
        let ligada = catalogo_com_rede(&web::Rede { ativa: true });
        assert_eq!(desligada.len(), 13, "catálogo com a rede desligada");
        assert_eq!(ligada.len(), 14, "a rede ligada não acrescentou uma ferramenta");
        let novas: Vec<&str> = ligada
            .iter()
            .map(|f| f.nome)
            .filter(|n| !desligada.iter().any(|d| &d.nome == n))
            .collect();
        assert_eq!(novas, vec!["buscar_na_web"], "a rede trouxe mais alguma coisa");
        // O resto do catálogo não muda de efeito quando a rede liga: uma escolha
        // de rede não reclassifica o que a máquina faz com os arquivos dela.
        for ferramenta in &desligada {
            let mesma = ligada.iter().find(|f| f.nome == ferramenta.nome).expect("ainda está");
            assert_eq!(mesma.efeito, ferramenta.efeito, "{}", ferramenta.nome);
        }
    }

    /// O efeito da única ferramenta de rede é o efeito que nenhum modo aprova
    /// sozinho, e ele é conferido nos dois sentidos: o catálogo declara `Rede`, e
    /// o gate recusa `Rede` em todos os modos. Um `ToolEffect::Rede` que caísse
    /// num `ReadOnly` por engano transformaria o cartão em rodapé.
    #[test]
    fn a_ferramenta_de_rede_tem_o_efeito_que_o_gate_nao_deixa_passar() {
        use crate::permissions::{AgentMode, Decision, evaluate};
        let ferramenta = web::ferramenta();
        assert_eq!(ferramenta.efeito, ToolEffect::Rede);
        for modo in [
            AgentMode::Plan,
            AgentMode::Default,
            AgentMode::AutoAccept,
            AgentMode::Auto,
            AgentMode::FullAccess,
        ] {
            assert_eq!(
                evaluate(modo, ToolEffect::Rede, false, true),
                if modo == AgentMode::Plan { Decision::Deny } else { Decision::Ask },
                "{modo:?} deixou a rede passar sem card"
            );
        }
    }

    /// O portão da rede está na execução, não só no catálogo. Um modelo que
    /// decorou o nome e chama com o `rede.json` desligado tem de receber a recusa
    /// da ferramenta, e não a lista de nomes do catálogo — que diria "isso não
    /// existe" e empurraria a procura por outra porta de saída.
    #[test]
    fn chamar_a_rede_pelo_nome_com_a_porta_desligada_devolve_a_recusa() {
        let pastas = pastas::Pastas::default();
        let argumentos = json!({ "consultaRede": "amazonas" });
        let mapa = argumentos.as_object().expect("objeto").clone();
        let erro = executar(
            web::FERRAMENTA,
            argumentos,
            &pastas,
            &memoria(),
            &deposito("rede-desligada"),
        )
        .expect_err("rede desligada não executa");
        let texto = format!("{erro:#}");
        assert_eq!(
            texto,
            web::buscar(&mapa).unwrap_err().to_string(),
            "quem respondeu não foi a ferramenta de rede"
        );
        // A lista do catálogo não entra na resposta: quem recusou foi a porta, e
        // um nome que a porta fechou não é um nome que o aplicativo desconhece.
        assert!(!texto.contains(&format!("{CONECTOR}__ler_arquivo")), "{texto}");
    }

    #[test]
    fn so_apagar_e_destrutivo_e_so_leitura_e_read_only() {
        for ferramenta in catalogo() {
            let esperado = match ferramenta.nome {
                "apagar" => ToolEffect::Destructive,
                "ler_arquivo" | "listar_pasta" | "buscar_texto" | "buscar_memoria"
                | "ler_resultado" | "info_sistema" => ToolEffect::ReadOnly,
                _ => ToolEffect::Mutating,
            };
            assert_eq!(ferramenta.efeito, esperado, "{}", ferramenta.nome);
        }
    }

    #[test]
    fn todo_esquema_declara_seus_obrigatorios_de_verdade() {
        for ferramenta in catalogo() {
            let propriedades = ferramenta.esquema["properties"].as_object().expect("objeto");
            for obrigatorio in ferramenta.esquema["required"].as_array().expect("lista") {
                let chave = obrigatorio.as_str().expect("texto");
                assert!(propriedades.contains_key(chave), "{}: {chave}", ferramenta.nome);
            }
        }
    }

    /// Rótulo e descrição de cada campo, inclusive dentro de uma lista de objetos.
    fn campos(esquema: &Value) -> Vec<(String, Value)> {
        let mut saídas = Vec::new();
        let Some(propriedades) = esquema["properties"].as_object() else {
            return saídas;
        };
        for (chave, valor) in propriedades {
            saídas.push((chave.clone(), valor.clone()));
            if let Some(itens) = valor.get("items").and_then(|v| v.as_object()) {
                for (filho, valor_filho) in itens["properties"].as_object().cloned().unwrap_or_default() {
                    saídas.push((format!("{chave}.{filho}"), valor_filho));
                }
            }
        }
        saídas
    }

    #[test]
    fn nenhuma_descricao_chega_vazia_ao_modelo() {
        for ferramenta in catalogo() {
            assert!(!ferramenta.titulo.is_empty(), "{} sem título", ferramenta.nome);
            assert!(!ferramenta.titulo.starts_with("ferramentas."), "{}", ferramenta.nome);
            assert!(!ferramenta.descricao.is_empty(), "{} sem descrição", ferramenta.nome);
            assert!(!ferramenta.descricao.starts_with("ferramentas."), "{}", ferramenta.nome);
            for (chave, valor) in campos(&ferramenta.esquema) {
                let descricao = valor["description"].as_str().unwrap_or("");
                assert!(!descricao.is_empty(), "{}: {chave} sem descrição", ferramenta.nome);
                assert!(!descricao.starts_with("argumentos."), "{chave} sem tradução");
            }
        }
    }

    /// O rótulo é o que o usuário vê no formulário: uma chave sem tradução aparece
    /// como "argumentos.x" na tela, e isso é um defeito, não um texto provisório.
    #[test]
    fn todo_campo_tem_rotulo_traduzido_para_a_interface() {
        for ferramenta in catalogo() {
            for (chave, valor) in campos(&ferramenta.esquema) {
                let rotulo = valor["title"].as_str().unwrap_or("");
                assert!(!rotulo.is_empty(), "{}: {chave} sem rótulo", ferramenta.nome);
                assert!(!rotulo.starts_with("argumentos."), "{}: {chave} sem tradução", ferramenta.nome);
            }
        }
    }

    #[test]
    fn o_conector_embutido_nao_tem_transporte_de_rede() {
        let conector = conector();
        assert_eq!(conector.kind, ConnectorKind::Builtin);
        assert!(!conector.has_entry());
        assert_eq!(conector.id, CONECTOR);
        assert!(conector.allows_tool("ler_arquivo"));
    }

    #[test]
    fn ferramenta_desconhecida_e_recusada() {
        let pastas = pastas::Pastas::default();
        assert!(
            executar("apagar_tudo", json!({}), &pastas, &memoria(), &deposito("desconhecida"))
                .is_err()
        );
    }

    #[test]
    fn argumentos_que_nao_sao_objeto_sao_recusados() {
        let pastas = pastas::Pastas::default();
        assert!(
            executar("ler_arquivo", json!("caminho"), &pastas, &memoria(), &deposito("argumentos"))
                .is_err()
        );
    }

    #[test]
    fn argumento_obrigatorio_ausente_nomeia_o_que_falta() {
        let pastas = pastas::Pastas::default();
        let erro = executar("ler_arquivo", json!({}), &pastas, &memoria(), &deposito("faltando"))
            .unwrap_err()
            .to_string();
        assert!(erro.contains("caminho"), "{erro}");
    }

    /// A memória tem a mesma porta das outras ferramentas, e a recusa dela é o
    /// mesmo caminho: um nome que o catálogo não tem não executa.
    #[test]
    fn buscar_memoria_e_o_dispatch_que_a_executa() {
        let pastas = pastas::Pastas::default();
        let erro = executar(
            "buscar_memoria",
            json!({ "consulta": "como ligar a placa" }),
            &pastas,
            &memoria(),
            &deposito("memoria"),
        )
        .unwrap_err()
        .to_string();
        assert_eq!(erro, tr("erros.memoriaSemModelo", &[]));
    }

    /// A base entra pela mesma porta das outras ferramentas, com o mesmo cartão e
    /// o mesmo depósito de caminho: não existe uma `executar` paralela para nota.
    /// O que este teste prova é o despacho, e ele roda sem codificador nenhum —
    /// a escrita da nota é disco, não placa.
    #[test]
    fn escrever_na_base_e_o_dispatch_que_a_executa() {
        let pastas = pastas::Pastas::default();
        let memoria = memoria();
        let saida = executar(
            "escrever_na_base",
            json!({ "titulo": "Despacho", "corpo": "a porta é única" }),
            &pastas,
            &memoria,
            &deposito("base"),
        )
        .expect("despachou");
        assert!(matches!(saida.vista, Vista::Base { .. }), "{:?}", saida.vista);
        let _ = crate::ai::base::apagar(memoria.raiz_base(), "despacho");
        let _ = std::fs::remove_dir_all(memoria.raiz_base());
    }

    /// O depósito não é uma porta paralela: o que o laço guardou sai pela mesma
    /// `executar` que atende um `ler_arquivo`, com o mesmo gate e o mesmo cartão.
    #[test]
    fn o_que_o_laco_guardou_volta_pela_mesma_porta_das_outras_ferramentas() {
        use crate::ai::derrame;
        let pastas = pastas::Pastas::default();
        let p = deposito("ida-e-volta");
        let corpo = "linha larga do despejo da ferramenta\n".repeat(2_000);
        let total = corpo.chars().count();

        // O que o laço escreve no histórico é a janela, e o nome legível ali é o
        // mesmo nome sob o qual o corpo está guardado.
        let id = derrame::guardar(&p, &corpo).expect("guardou");
        let janela = derrame::derramar(&p, &corpo).expect("derramou");
        assert!(janela.chars().count() < total, "a janela é menor que o corpo");

        let saida = executar(
            "ler_resultado",
            json!({ "id": id, "desde": 100, "quantos": 200 }),
            &pastas,
            &memoria(),
            &p,
        )
        .expect("li pelo catálogo");
        assert!(saida.texto.contains("linha larga do despejo"), "{}", saida.texto);
        let Vista::Resultado { desde, ate, total: t, sobrou, .. } = &saida.vista else {
            panic!("cartão de resultado");
        };
        assert_eq!((*desde, *ate, *t, *sobrou), (100, 300, total, total - 300));
        let _ = std::fs::remove_dir_all(&p);
    }
}