//! As ferramentas de banco de dados que o modelo enxerga, atrás do mesmo gate
//! das ferramentas de arquivo.
//!
//! Duas camadas de política, cada uma no seu lugar: o efeito da ferramenta
//! (leitura, escrita, destruição) é o que o gate decide a cada chamada — e por
//! isso a aprovação pergunta por `bancos_apagar` como pergunta por `apagar`. O
//! que pode ser feito em cada tabela é o cadastro do dono, e é conferido aqui,
//! dentro da execução, antes de qualquer motor abrir: o gate aprova o ato, o
//! cadastro aprova o alvo. O que não está exposto em `bancos.json` o modelo nem
//! descreve, quanto menos consulta.

use super::config::{Conexao, Config, Operacao, Tipo};
use super::motor::{self, Cronometro, ResultadoSql};
use crate::idiomas::tr;
use crate::local::vista::{ColunaCartao, ConexaoCartao, FonteCartao};
use crate::local::{self, Ferramenta, Vista, campo, ferramenta, objeto};
use crate::permissions::ToolEffect;
use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value, json};
use std::path::Path;

pub fn catalogo() -> Vec<Ferramenta> {
    use ToolEffect::{Destructive, Mutating, ReadOnly};
    vec![
        ferramenta("bancos_listar", "bancosListar", ReadOnly, objeto(json!({}), &[])),
        ferramenta(
            "bancos_tabelas",
            "bancosTabelas",
            ReadOnly,
            objeto(json!({ "conexao": campo("string", "conexao") }), &["conexao"]),
        ),
        ferramenta(
            "bancos_colunas",
            "bancosColunas",
            ReadOnly,
            objeto(
                json!({
                    "conexao": campo("string", "conexao"),
                    "fonte": campo("string", "fonte"),
                }),
                &["conexao", "fonte"],
            ),
        ),
        ferramenta(
            "bancos_consultar",
            "bancosConsultar",
            ReadOnly,
            objeto(
                json!({
                    "conexao": campo("string", "conexao"),
                    "fonte": campo("string", "fonte"),
                    "colunas": campo("array", "colunas"),
                    "filtro": campo("object", "filtro"),
                    "ordem": campo("string", "ordem"),
                    "ordemDesc": campo("boolean", "ordemDesc"),
                    "limite": campo("integer", "limite"),
                }),
                &["conexao", "fonte"],
            ),
        ),
        ferramenta(
            "bancos_contar",
            "bancosContar",
            ReadOnly,
            objeto(
                json!({
                    "conexao": campo("string", "conexao"),
                    "fonte": campo("string", "fonte"),
                    "filtro": campo("object", "filtro"),
                }),
                &["conexao", "fonte"],
            ),
        ),
        ferramenta(
            "bancos_inserir",
            "bancosInserir",
            Mutating,
            objeto(
                json!({
                    "conexao": campo("string", "conexao"),
                    "fonte": campo("string", "fonte"),
                    "valores": campo("object", "valores"),
                }),
                &["conexao", "fonte", "valores"],
            ),
        ),
        ferramenta(
            "bancos_atualizar",
            "bancosAtualizar",
            Mutating,
            objeto(
                json!({
                    "conexao": campo("string", "conexao"),
                    "fonte": campo("string", "fonte"),
                    "valores": campo("object", "valores"),
                    "filtro": campo("object", "filtro"),
                }),
                &["conexao", "fonte", "valores", "filtro"],
            ),
        ),
        ferramenta(
            "bancos_apagar",
            "bancosApagar",
            Destructive,
            objeto(
                json!({
                    "conexao": campo("string", "conexao"),
                    "fonte": campo("string", "fonte"),
                    "filtro": campo("object", "filtro"),
                }),
                &["conexao", "fonte", "filtro"],
            ),
        ),
        ferramenta(
            "bancos_sql",
            "bancosSql",
            Mutating,
            objeto(
                json!({
                    "conexao": campo("string", "conexao"),
                    "sql": campo("string", "sql"),
                    "limite": campo("integer", "limite"),
                }),
                &["conexao", "sql"],
            ),
        ),
    ]
}

/// O roteamento de `Execucao::ferramenta` precisa decidir sem montar o catálogo:
/// o nome é estável e o teste abaixo cobra que os dois digam a mesma coisa.
pub fn e_ferramenta(nome: &str) -> bool {
    matches!(
        nome,
        "bancos_listar"
            | "bancos_tabelas"
            | "bancos_colunas"
            | "bancos_consultar"
            | "bancos_contar"
            | "bancos_inserir"
            | "bancos_atualizar"
            | "bancos_apagar"
            | "bancos_sql"
    )
}

/// Síncrono de propósito: quem chama roda numa tarefa bloqueante. O cadastro é
/// lido do disco a cada chamada — a política que a tela salvou agora é a que a
/// ferramenta vê na próxima chamada, sem cache a desligar.
pub fn executar(nome: &str, argumentos: Value, caminho_config: &Path) -> Result<local::Saida> {
    let args = match argumentos {
        Value::Object(mapa) => mapa,
        _ => bail!(tr("erros.argumentosInvalidos", &[])),
    };
    let config = Config::load(caminho_config)?;
    match nome {
        "bancos_listar" => listar(&config),
        "bancos_tabelas" => tabelas(&config, &args),
        "bancos_colunas" => colunas(&config, &args),
        "bancos_consultar" => consultar(&config, &args),
        "bancos_contar" => contar(&config, &args),
        "bancos_inserir" => inserir(&config, &args),
        "bancos_atualizar" => atualizar(&config, &args),
        "bancos_apagar" => apagar(&config, &args),
        "bancos_sql" => sql(&config, &args),
        outra => {
            let lista = local::catalogo()
                .into_iter()
                .map(|t| format!("{}__{}", local::CONECTOR, t.nome))
                .chain(
                    catalogo().into_iter().map(|t| format!("{}__{}", local::CONECTOR, t.nome)),
                )
                .collect::<Vec<_>>()
                .join(", ");
            bail!(tr(
                "erros.ferramentaInexistente",
                &[("nome", &format!("{}__{outra}", local::CONECTOR)), ("lista", &lista)]
            ))
        }
    }
}

/// A conexão pelo nome que o modelo escreveu. O nome é a chave do cadastro, não
/// um id: é ele que a tela mostra e que a política segue.
fn conexao_pedida(config: &Config, args: &Map<String, Value>) -> Result<Conexao> {
    let nome = local::obrigatorio(args, "conexao")?;
    config.por_nome(&nome).cloned().ok_or_else(|| {
        anyhow!(tr("erros.bancoConexaoInexistente", &[("nome", &nome)]))
    })
}

/// O alvo tem de estar exposto no cadastro — sem isso nem coluna é descrita.
fn fonte_exposta(conexao: &Conexao, args: &Map<String, Value>) -> Result<String> {
    let nome = local::obrigatorio(args, "fonte")?;
    if !conexao.fontes.contains_key(&nome) {
        bail!(tr(
            "erros.bancoFonteInexistente",
            &[("fonte", &format!("{nome} ({})", conexao.nome))]
        ));
    }
    Ok(nome)
}

/// A operação pedida tem de ser a que o dono autorizou para esta fonte. A
/// recusa acontece aqui, antes de o motor abrir: negada é não escrita.
fn fonte_pedida(
    conexao: &Conexao,
    args: &Map<String, Value>,
    operacao: Operacao,
) -> Result<String> {
    let nome = fonte_exposta(conexao, args)?;
    let autorizada = conexao
        .fontes
        .get(&nome)
        .expect("fonte_exposta conferiu a existência")
        .permite(operacao);
    if !autorizada {
        bail!(tr(
            "erros.bancoOperacaoNegada",
            &[
                ("op", operacao.rotulo()),
                ("fonte", &nome),
                ("conexao", &conexao.nome)
            ]
        ));
    }
    Ok(nome)
}

fn destino(conexao: &Conexao) -> String {
    match conexao.tipo {
        Tipo::Sqlite => conexao.arquivo.clone(),
        _ => format!("{}:{}/{}", conexao.host.trim(), conexao.porta_efetiva(), conexao.banco.trim()),
    }
}

fn listar(config: &Config) -> Result<local::Saida> {
    let itens: Vec<ConexaoCartao> = config
        .conexoes
        .iter()
        .map(|c| ConexaoCartao {
            nome: c.nome.clone(),
            tipo: c.tipo.rotulo().to_string(),
            destino: destino(c),
            fontes: c.fontes.len(),
            limite_linhas: c.limite_linhas,
            sql_livre: c.sql_livre,
        })
        .collect();
    let texto = if itens.is_empty() {
        tr("bancos.nenhumaConexao", &[])
    } else {
        itens
            .iter()
            .map(|c| {
                format!(
                    "{} | {} | {} | fontes: {} | limite: {} | sql_livre: {}",
                    c.nome, c.tipo, c.destino, c.fontes, c.limite_linhas, c.sql_livre
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    Ok(local::Saida::nova(texto, Vista::BancoConexoes { itens }))
}

fn tabelas(config: &Config, args: &Map<String, Value>) -> Result<local::Saida> {
    let conexao = conexao_pedida(config, args)?;
    let itens: Vec<FonteCartao> = conexao
        .fontes
        .iter()
        .map(|(nome, fonte)| FonteCartao {
            nome: nome.clone(),
            ler: fonte.ler,
            inserir: fonte.inserir,
            atualizar: fonte.atualizar,
            apagar: fonte.apagar,
        })
        .collect();
    let texto = if itens.is_empty() {
        tr("bancos.nenhumaFonte", &[("conexao", &conexao.nome)])
    } else {
        itens
            .iter()
            .map(|f| {
                let atos = [(f.ler, "ler"), (f.inserir, "inserir"), (f.atualizar, "atualizar"), (f.apagar, "apagar")]
                    .into_iter()
                    .filter_map(|(ligado, ato)| ligado.then_some(ato))
                    .collect::<Vec<_>>()
                    .join(",");
                format!("{} | {}", f.nome, atos)
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    Ok(local::Saida::nova(texto, Vista::BancoFontes { conexao: conexao.nome, itens }))
}

fn colunas(config: &Config, args: &Map<String, Value>) -> Result<local::Saida> {
    let conexao = conexao_pedida(config, args)?;
    // Descrever o esquema é leitura: sem o `ler` do dono, a tabela não exposta
    // para consulta também não tem as colunas divulgadas — estar no cadastro
    // não basta, a política por operação é que manda.
    let fonte = fonte_pedida(&conexao, args, Operacao::Ler)?;
    let mut motor = motor::abrir(&conexao)?;
    let lista = motor.colunas(&fonte)?;
    let cartoes: Vec<ColunaCartao> = lista
        .iter()
        .map(|(nome, tipo)| ColunaCartao { nome: nome.clone(), tipo: tipo.clone() })
        .collect();
    let texto = lista
        .iter()
        .map(|(nome, tipo)| format!("{nome} ({tipo})"))
        .collect::<Vec<_>>()
        .join(", ");
    Ok(local::Saida::nova(
        texto,
        Vista::BancoEsquema { conexao: conexao.nome, fonte, colunas: cartoes },
    ))
}

/// O teto de linhas é do cadastro, e o pedido do modelo nunca o passa: o que o
/// dono configurou é o que a consulta devolve, no máximo.
fn teto_da_consulta(conexao: &Conexao, args: &Map<String, Value>) -> usize {
    let teto = conexao.limite_linhas as usize;
    local::inteiro(args, "limite").unwrap_or(teto).clamp(1, teto)
}

fn colunas_pedidas(args: &Map<String, Value>) -> Result<Vec<String>> {
    match args.get("colunas") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(itens)) => {
            let mut nomes = Vec::new();
            for item in itens {
                match item {
                    Value::String(texto) => nomes.push(texto.clone()),
                    _ => bail!(tr("erros.argumentosInvalidos", &[])),
                }
            }
            Ok(nomes)
        }
        _ => bail!(tr("erros.argumentosInvalidos", &[])),
    }
}

fn texto_da_consulta(consulta: &motor::Consulta) -> String {
    serde_json::to_string(&json!({
        "colunas": consulta.colunas,
        "linhas": consulta.linhas,
        "truncado": consulta.truncado,
        "duracaoMs": consulta.duracao_ms,
    }))
    .expect("consulta é sempre serializável")
}

fn vista_da_consulta(conexao: &str, fonte: &str, consulta: motor::Consulta) -> Vista {
    Vista::BancoConsulta {
        conexao: conexao.to_string(),
        fonte: fonte.to_string(),
        total: consulta.linhas.len(),
        colunas: consulta.colunas,
        linhas: consulta.linhas,
        truncado: consulta.truncado,
        duracao_ms: consulta.duracao_ms,
    }
}

fn consultar(config: &Config, args: &Map<String, Value>) -> Result<local::Saida> {
    let conexao = conexao_pedida(config, args)?;
    let fonte = fonte_pedida(&conexao, args, Operacao::Ler)?;
    let filtro = motor::filtro_de(args.get("filtro"))?;
    let pedidas = colunas_pedidas(args)?;
    let ordem_coluna = local::texto(args, "ordem");
    let ordem =
        ordem_coluna.as_deref().map(|c| (c, local::booleano(args, "ordemDesc", false)));
    let limite = teto_da_consulta(&conexao, args);
    let mut motor = motor::abrir(&conexao)?;
    let consulta = motor.consultar(&fonte, &pedidas, &filtro, ordem, limite)?;
    Ok(local::Saida::nova(
        texto_da_consulta(&consulta),
        vista_da_consulta(&conexao.nome, &fonte, consulta),
    ))
}

fn contar(config: &Config, args: &Map<String, Value>) -> Result<local::Saida> {
    let conexao = conexao_pedida(config, args)?;
    let fonte = fonte_pedida(&conexao, args, Operacao::Ler)?;
    let filtro = motor::filtro_de(args.get("filtro"))?;
    let mut motor = motor::abrir(&conexao)?;
    let cronometro = Cronometro::agora();
    let total = motor.contar(&fonte, &filtro)?;
    let texto = tr(
        "bancos.contagem",
        &[("total", &total.to_string()), ("fonte", &fonte)],
    );
    Ok(local::Saida::nova(
        texto,
        Vista::BancoLinhas {
            conexao: conexao.nome,
            fonte: Some(fonte),
            operacao: "contar".into(),
            linhas: total,
            duracao_ms: cronometro.ms(),
        },
    ))
}

fn valores_pedidos(args: &Map<String, Value>) -> Result<Map<String, Value>> {
    match args.get("valores") {
        Some(Value::Object(mapa)) => Ok(mapa.clone()),
        Some(_) => bail!(tr("erros.argumentosInvalidos", &[])),
        None => bail!(tr("erros.argumentoFaltando", &[("nome", "valores")])),
    }
}

fn inserir(config: &Config, args: &Map<String, Value>) -> Result<local::Saida> {
    let conexao = conexao_pedida(config, args)?;
    let fonte = fonte_pedida(&conexao, args, Operacao::Inserir)?;
    let valores = valores_pedidos(args)?;
    let mut motor = motor::abrir(&conexao)?;
    let cronometro = Cronometro::agora();
    let linhas = motor.inserir(&fonte, &valores)?;
    let texto = tr(
        "bancos.linhasInseridas",
        &[("total", &linhas.to_string()), ("fonte", &fonte)],
    );
    Ok(local::Saida::nova(
        texto,
        Vista::BancoLinhas {
            conexao: conexao.nome,
            fonte: Some(fonte),
            operacao: "inserir".into(),
            linhas,
            duracao_ms: cronometro.ms(),
        },
    ))
}

fn atualizar(config: &Config, args: &Map<String, Value>) -> Result<local::Saida> {
    let conexao = conexao_pedida(config, args)?;
    let fonte = fonte_pedida(&conexao, args, Operacao::Atualizar)?;
    let valores = valores_pedidos(args)?;
    let filtro = motor::filtro_de(args.get("filtro"))?;
    let mut motor = motor::abrir(&conexao)?;
    let cronometro = Cronometro::agora();
    let linhas = motor.atualizar(&fonte, &valores, &filtro)?;
    let texto = tr(
        "bancos.linhasAtualizadas",
        &[("total", &linhas.to_string()), ("fonte", &fonte)],
    );
    Ok(local::Saida::nova(
        texto,
        Vista::BancoLinhas {
            conexao: conexao.nome,
            fonte: Some(fonte),
            operacao: "atualizar".into(),
            linhas,
            duracao_ms: cronometro.ms(),
        },
    ))
}

fn apagar(config: &Config, args: &Map<String, Value>) -> Result<local::Saida> {
    let conexao = conexao_pedida(config, args)?;
    let fonte = fonte_pedida(&conexao, args, Operacao::Apagar)?;
    let filtro = motor::filtro_de(args.get("filtro"))?;
    let mut motor = motor::abrir(&conexao)?;
    let cronometro = Cronometro::agora();
    let linhas = motor.apagar(&fonte, &filtro)?;
    let texto = tr(
        "bancos.linhasApagadas",
        &[("total", &linhas.to_string()), ("fonte", &fonte)],
    );
    Ok(local::Saida::nova(
        texto,
        Vista::BancoLinhas {
            conexao: conexao.nome,
            fonte: Some(fonte),
            operacao: "apagar".into(),
            linhas,
            duracao_ms: cronometro.ms(),
        },
    ))
}

/// SQL escrito à mão é uma concessão do cadastro, não do gate: ligar
/// `sql_livre` na conexão é o dono autorizando qualquer declaração nela.
fn sql(config: &Config, args: &Map<String, Value>) -> Result<local::Saida> {
    let conexao = conexao_pedida(config, args)?;
    if !conexao.sql_livre {
        bail!(tr("erros.bancoSqlBloqueado", &[("conexao", &conexao.nome)]));
    }
    let declaracao = local::obrigatorio(args, "sql")?;
    let limite = teto_da_consulta(&conexao, args);
    let mut motor = motor::abrir(&conexao)?;
    let cronometro = Cronometro::agora();
    match motor.sql(&declaracao, limite)? {
        ResultadoSql::Consulta(consulta) => Ok(local::Saida::nova(
            texto_da_consulta(&consulta),
            vista_da_consulta(&conexao.nome, "", consulta),
        )),
        ResultadoSql::Afetadas(linhas) => {
            let texto = tr("bancos.linhasAfetadas", &[("total", &linhas.to_string())]);
            Ok(local::Saida::nova(
                texto,
                Vista::BancoLinhas {
                    conexao: conexao.nome,
                    fonte: None,
                    operacao: "sql".into(),
                    linhas,
                    duracao_ms: cronometro.ms(),
                },
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bancos::config::Fonte;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Cada bancada tem pasta própria: os testes rodam em paralelo no mesmo
    /// processo, e dois na mesma pasta apagariam o SQLite um do outro.
    static BANCADA: AtomicU64 = AtomicU64::new(0);

    struct Bancada {
        raiz: PathBuf,
        config_path: PathBuf,
    }

    /// Um SQLite real no disco e um cadastro que expõe `clientes` só para ler.
    /// O cadastro passa pelo arquivo — é o mesmo caminho que `bancos.json` segue
    /// no aplicativo, não um atalho de memória.
    fn bancada() -> Bancada {
        let raiz = std::env::temp_dir().join(format!(
            "aether-bancos-ferr-{}-{}",
            std::process::id(),
            BANCADA.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&raiz);
        std::fs::create_dir_all(&raiz).expect("pasta de teste");
        let arquivo = raiz.join("estoque.db");
        let sqlite = rusqlite::Connection::open(&arquivo).expect("abre o sqlite");
        sqlite
            .execute_batch(
                "CREATE TABLE clientes (id INTEGER PRIMARY KEY, nome TEXT NOT NULL);
                 INSERT INTO clientes (nome) VALUES ('Ana'), ('Bruno');",
            )
            .expect("cria a tabela");
        drop(sqlite);

        let mut config = Config::default();
        let mut conexao = Conexao {
            id: "bd-teste".into(),
            nome: "Estoque".into(),
            tipo: Tipo::Sqlite,
            host: String::new(),
            porta: None,
            usuario: String::new(),
            banco: String::new(),
            arquivo: arquivo.display().to_string(),
            senha_cifrada: None,
            senha_clara: None,
            limite_linhas: 200,
            sql_livre: false,
            fontes: BTreeMap::new(),
        };
        conexao.fontes.insert("clientes".into(), Fonte::default());
        config.conexoes.push(conexao);
        let config_path = raiz.join("bancos.json");
        config.save(&config_path).expect("salva o cadastro");
        Bancada { raiz, config_path }
    }

    #[test]
    fn o_catalogo_e_o_nome_estatico_dizem_a_mesma_coisa() {
        let ferramentas = catalogo();
        assert_eq!(ferramentas.len(), 9);
        for ferramenta in &ferramentas {
            assert!(e_ferramenta(ferramenta.nome), "{} fora do roteamento", ferramenta.nome);
        }
        let roteaveis = [
            "bancos_listar",
            "bancos_tabelas",
            "bancos_colunas",
            "bancos_consultar",
            "bancos_contar",
            "bancos_inserir",
            "bancos_atualizar",
            "bancos_apagar",
            "bancos_sql",
        ];
        assert_eq!(ferramentas.iter().map(|f| f.nome).collect::<Vec<_>>(), roteaveis);
        for ferramenta in &ferramentas {
            assert!(!ferramenta.descricao.is_empty(), "{}", ferramenta.nome);
            assert!(!ferramenta.descricao.starts_with("ferramentas."), "{}", ferramenta.nome);
        }
    }

    #[test]
    fn so_apagar_e_destrutivo_e_as_leituras_sao_read_only() {
        for ferramenta in catalogo() {
            let esperado = match ferramenta.nome {
                "bancos_apagar" => ToolEffect::Destructive,
                "bancos_inserir" | "bancos_atualizar" | "bancos_sql" => ToolEffect::Mutating,
                _ => ToolEffect::ReadOnly,
            };
            assert_eq!(ferramenta.efeito, esperado, "{}", ferramenta.nome);
        }
    }

    #[test]
    fn a_leitura_da_fonte_exposta_volta_com_as_linhas() {
        let b = bancada();
        let saida = executar(
            "bancos_consultar",
            json!({ "conexao": " estoque ", "fonte": "clientes" }),
            &b.config_path,
        )
        .expect("consulta");
        let json = saida.vista.para_json();
        assert_eq!(json["conexao"], "Estoque");
        assert_eq!(json["total"], 2);
        assert_eq!(json["colunas"][0], "id");
        assert_eq!(json["linhas"][1][1], "Bruno");
        assert_eq!(json["truncado"], false);
        assert!(saida.texto.contains("Bruno"), "{}", saida.texto);
        std::fs::remove_dir_all(&b.raiz).ok();
    }

    #[test]
    fn fonte_que_nao_esta_no_cadastro_nao_e_descrita_nem_consultada() {
        let b = bancada();
        let erro = executar(
            "bancos_consultar",
            json!({ "conexao": "Estoque", "fonte": "pedidos" }),
            &b.config_path,
        )
        .expect_err("não exposta não consulta");
        assert!(erro.to_string().contains("pedidos"), "{erro}");

        let erro = executar(
            "bancos_colunas",
            json!({ "conexao": "Estoque", "fonte": "pedidos" }),
            &b.config_path,
        )
        .expect_err("não exposta nem descreve");
        assert!(erro.to_string().contains("pedidos"), "{erro}");

        let erro = executar(
            "bancos_tabelas",
            json!({ "conexao": "Nada" }),
            &b.config_path,
        )
        .expect_err("conexão inexistente");
        assert!(erro.to_string().contains("Nada"), "{erro}");
        std::fs::remove_dir_all(&b.raiz).ok();
    }

    /// A recusa do cadastro acontece antes de o motor abrir: o arquivo segue com
    /// as duas linhas que tinha, e a contagem depois é a prova.
    #[test]
    fn a_politica_do_dono_negada_e_o_banco_fica_como_estava() {
        let b = bancada();
        for (ferramenta, argumentos) in [
            (
                "bancos_inserir",
                json!({ "conexao": "Estoque", "fonte": "clientes", "valores": { "nome": "Clara" } }),
            ),
            (
                "bancos_atualizar",
                json!({ "conexao": "Estoque", "fonte": "clientes", "valores": { "nome": "X" }, "filtro": { "id": 1 } }),
            ),
            (
                "bancos_apagar",
                json!({ "conexao": "Estoque", "fonte": "clientes", "filtro": { "id": 1 } }),
            ),
            (
                "bancos_sql",
                json!({ "conexao": "Estoque", "sql": "DELETE FROM clientes" }),
            ),
        ] {
            let erro = executar(ferramenta, argumentos, &b.config_path)
                .expect_err("só leitura não escreve");
            assert!(
                erro.to_string().contains("Estoque") || erro.to_string().contains("clientes"),
                "{ferramenta}: {erro}"
            );
        }
        let contagem =
            executar("bancos_contar", json!({ "conexao": "Estoque", "fonte": "clientes" }), &b.config_path)
                .expect("conta");
        assert!(contagem.texto.contains('2'), "{}", contagem.texto);
        std::fs::remove_dir_all(&b.raiz).ok();
    }

    /// Com as permissões concedidas pelo dono, as três escritas rodam de verdade
    /// no SQLite e o banco muda de fato.
    #[test]
    fn escritas_concedidas_mudam_o_banco_de_verdade() {
        let b = bancada();
        let mut config = Config::load(&b.config_path).expect("carrega");
        {
            let fonte = config.conexoes[0].fontes.get_mut("clientes").expect("fonte");
            fonte.inserir = true;
            fonte.atualizar = true;
            fonte.apagar = true;
        }
        config.save(&b.config_path).expect("salva");

        let saida = executar(
            "bancos_inserir",
            json!({ "conexao": "Estoque", "fonte": "clientes", "valores": { "nome": "Clara" } }),
            &b.config_path,
        )
        .expect("insere");
        let json = saida.vista.para_json();
        assert_eq!(json["operacao"], "inserir");
        assert_eq!(json["linhas"], 1);

        let saida = executar(
            "bancos_atualizar",
            json!({
                "conexao": "Estoque",
                "fonte": "clientes",
                "valores": { "nome": "Clara S." },
                "filtro": { "nome": "Clara" },
            }),
            &b.config_path,
        )
        .expect("atualiza");
        assert_eq!(saida.vista.para_json()["linhas"], 1);

        let saida = executar(
            "bancos_apagar",
            json!({ "conexao": "Estoque", "fonte": "clientes", "filtro": { "id": 1 } }),
            &b.config_path,
        )
        .expect("apaga");
        assert_eq!(saida.vista.para_json()["operacao"], "apagar");

        let contagem =
            executar("bancos_contar", json!({ "conexao": "Estoque", "fonte": "clientes" }), &b.config_path)
                .expect("conta");
        assert!(contagem.texto.contains('2'), "{}", contagem.texto);
        std::fs::remove_dir_all(&b.raiz).ok();
    }

    #[test]
    fn listar_mostra_as_conexoes_sem_um_segredo() {
        let b = bancada();
        let saida = executar("bancos_listar", json!({}), &b.config_path).expect("lista");
        let json = saida.vista.para_json();
        assert_eq!(json["itens"][0]["nome"], "Estoque");
        assert_eq!(json["itens"][0]["tipo"], "sqlite");
        assert_eq!(json["itens"][0]["fontes"], 1);
        assert_eq!(json["itens"][0]["sqlLivre"], false);
        assert!(
            !serde_json::to_string(&json).unwrap().to_lowercase().contains("senha"),
            "a lista de conexões não fala de senha: {json}"
        );
        std::fs::remove_dir_all(&b.raiz).ok();
    }

    #[test]
    fn tabelas_mostra_a_politica_de_cada_fonte() {
        let b = bancada();
        let saida = executar("bancos_tabelas", json!({ "conexao": "Estoque" }), &b.config_path)
            .expect("tabelas");
        let json = saida.vista.para_json();
        assert_eq!(json["conexao"], "Estoque");
        assert_eq!(json["itens"][0]["nome"], "clientes");
        assert_eq!(json["itens"][0]["ler"], true);
        assert_eq!(json["itens"][0]["inserir"], false);
        std::fs::remove_dir_all(&b.raiz).ok();
    }

    #[test]
    fn colunas_da_fonte_exposta_vem_do_banco_real() {
        let b = bancada();
        let saida = executar(
            "bancos_colunas",
            json!({ "conexao": "Estoque", "fonte": "clientes" }),
            &b.config_path,
        )
        .expect("colunas");
        let json = saida.vista.para_json();
        assert_eq!(json["colunas"][0]["nome"], "id");
        assert_eq!(json["colunas"][1]["nome"], "nome");
        std::fs::remove_dir_all(&b.raiz).ok();
    }

    /// Descrever o esquema também é leitura: a tabela que o dono não autorizou
    /// ler não tem as colunas divulgadas pelo `bancos_colunas`, do mesmo jeito
    /// que `bancos_consultar` já recusava.
    #[test]
    fn colunas_de_fonte_nao_legivel_sao_recusadas() {
        let b = bancada();
        let mut config = Config::load(&b.config_path).expect("carrega");
        config.conexoes[0].fontes.get_mut("clientes").expect("fonte").ler = false;
        config.save(&b.config_path).expect("salva");
        let erro = executar(
            "bancos_colunas",
            json!({ "conexao": "Estoque", "fonte": "clientes" }),
            &b.config_path,
        )
        .expect_err("sem ler não descreve o esquema");
        assert!(erro.to_string().contains("ler"), "{erro}");
        assert!(erro.to_string().contains("clientes"), "{erro}");
        std::fs::remove_dir_all(&b.raiz).ok();
    }

    /// O teto é do cadastro, não do pedido: uma conexão com limite 1 devolve uma
    /// linha e marca truncado, mesmo que o modelo peça mais.
    #[test]
    fn o_limite_do_cadastro_manda_sobre_o_pedido() {
        let b = bancada();
        let mut config = Config::load(&b.config_path).expect("carrega");
        config.conexoes[0].limite_linhas = 1;
        config.save(&b.config_path).expect("salva");

        let saida = executar(
            "bancos_consultar",
            json!({ "conexao": "Estoque", "fonte": "clientes", "limite": 9999 }),
            &b.config_path,
        )
        .expect("consulta");
        let json = saida.vista.para_json();
        assert_eq!(json["total"], 1);
        assert_eq!(json["truncado"], true);
        std::fs::remove_dir_all(&b.raiz).ok();
    }

    #[test]
    fn valores_fora_do_formato_sao_recusados() {
        let b = bancada();
        // A política é avaliada antes do formato dos argumentos, então a
        // recusa de formato só aparece com a inserção autorizada no cadastro.
        let mut config = Config::load(&b.config_path).expect("cadastro");
        config
            .conexoes
            .iter_mut()
            .find(|c| c.nome == "Estoque")
            .expect("conexão")
            .fontes
            .get_mut("clientes")
            .expect("fonte")
            .inserir = true;
        config.save(&b.config_path).expect("salvar cadastro");
        let erro = executar(
            "bancos_inserir",
            json!({ "conexao": "Estoque", "fonte": "clientes", "valores": "nome" }),
            &b.config_path,
        )
        .expect_err("valores não é objeto");
        assert!(!erro.to_string().is_empty());

        let erro = executar(
            "bancos_inserir",
            json!({ "conexao": "Estoque", "fonte": "clientes" }),
            &b.config_path,
        )
        .expect_err("valores faltando");
        assert!(erro.to_string().contains("valores"), "{erro}");

        let erro = executar(
            "bancos_consultar",
            json!({ "conexao": "Estoque", "fonte": "clientes", "colunas": [1, 2] }),
            &b.config_path,
        )
        .expect_err("colunas não são texto");
        assert!(!erro.to_string().is_empty());
        std::fs::remove_dir_all(&b.raiz).ok();
    }
}
