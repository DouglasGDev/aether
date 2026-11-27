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
    let mut motor = motor::abrir_para(&conexao, Operacao::Ler)?;
    let lista = motor.colunas(&fonte)?;
    let cartoes: Vec<ColunaCartao> = lista
        .iter()
        .map(|(nome, tipo)| ColunaCartao { nome: nome.clone(), tipo: tipo.clone() })
        .collect();
    let mut texto = lista
        .iter()
        .map(|(nome, tipo)| format!("{nome} ({tipo})"))
        .collect::<Vec<_>>()
        .join(", ");
    texto.push_str("\n\n");
    texto.push_str(&tr(
        "bancos.orientacaoColunas",
        &[("tipo", conexao.tipo.rotulo())],
    ));
    if conexao.tipo == Tipo::Mongo {
        texto.push(' ');
        texto.push_str(&tr("bancos.colunasAmostraMongo", &[]));
    }
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

/// `ordem` é um nome de coluna, com uma direção separada ou o sufixo legado
/// `ASC`/`DESC`. O texto chega cru ao motor: só ele, com o schema já carregado,
/// pode saber se o sufixo é direção ou parte de um nome literal. Tipo errado ou
/// texto vazio é erro, não ordem ausente.
fn ordem_pedida(args: &Map<String, Value>) -> Result<Option<(String, bool)>> {
    let ordem_desc = match args.get("ordemDesc") {
        None => false,
        Some(Value::Bool(desc)) => *desc,
        _ => bail!(tr("erros.bancoOrdemInvalida", &[])),
    };
    let Some(valor) = args.get("ordem") else {
        return Ok(None);
    };
    if valor.is_null() {
        return Ok(None);
    }
    let Value::String(texto) = valor else {
        bail!(tr("erros.bancoOrdemInvalida", &[]));
    };
    if texto.trim().is_empty() {
        bail!(tr("erros.bancoOrdemInvalida", &[]));
    }
    Ok(Some((texto.trim().to_string(), ordem_desc)))
}

/// A grelha serializada por referência: `json!` copiava cada `Vec<Value>` da
/// consulta para depois serializar a cópia, e a mesma grelha ainda era movida
/// para a vista. Uma passada sobre o que já está na mão, com os campos na ordem
/// que o modelo sempre leu.
#[derive(serde::Serialize)]
struct ConsultaSerializada<'a> {
    colunas: &'a [String],
    linhas: &'a [Vec<Value>],
    truncado: bool,
    #[serde(rename = "duracaoMs")]
    duracao_ms: u64,
}

fn texto_da_consulta(consulta: &motor::Consulta) -> String {
    serde_json::to_string(&ConsultaSerializada {
        colunas: &consulta.colunas,
        linhas: &consulta.linhas,
        truncado: consulta.truncado,
        duracao_ms: consulta.duracao_ms,
    })
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
    let ordem = ordem_pedida(args)?;
    let limite = teto_da_consulta(&conexao, args);
    let mut motor = motor::abrir_para(&conexao, Operacao::Ler)?;
    let consulta = motor.consultar(
        &fonte,
        &pedidas,
        &filtro,
        ordem
            .as_ref()
            .map(|(coluna, desc)| (coluna.as_str(), *desc)),
        limite,
    )?;
    Ok(local::Saida::nova(
        texto_da_consulta(&consulta),
        vista_da_consulta(&conexao.nome, &fonte, consulta),
    ))
}

fn contar(config: &Config, args: &Map<String, Value>) -> Result<local::Saida> {
    let conexao = conexao_pedida(config, args)?;
    let fonte = fonte_pedida(&conexao, args, Operacao::Ler)?;
    let filtro = motor::filtro_de(args.get("filtro"))?;
    let mut motor = motor::abrir_para(&conexao, Operacao::Ler)?;
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
    let mut motor = motor::abrir_para(&conexao, Operacao::Inserir)?;
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
    let mut motor = motor::abrir_para(&conexao, Operacao::Atualizar)?;
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
    let mut motor = motor::abrir_para(&conexao, Operacao::Apagar)?;
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
    // A declaração livre pode escrever — o dono ligou `sql_livre` exatamente para
    // isso — então a alça é a cheia. O que a leitura tem de teto aqui é o mesmo
    // das ferramentas: o cursor para no limite+1, nunca na tabela inteira.
    let mut motor = motor::abrir_para(&conexao, Operacao::Atualizar)?;
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
            tls_verificado: false,
            fontes: BTreeMap::new(),
        };
        conexao.fontes.insert("clientes".into(), Fonte::default());
        config.conexoes.push(conexao);
        let config_path = raiz.join("bancos.json");
        config.save(&config_path).expect("salva o cadastro");
        Bancada { raiz, config_path }
    }

    /// Uma tabela real com valores distintos para provar a direção da ordem.
    /// Fica fora da bancada de `clientes` para não mudar as saídas existentes.
    fn bancada_vendas() -> Bancada {
        let b = bancada();
        let sqlite = rusqlite::Connection::open(&b.raiz.join("estoque.db")).expect("abre o sqlite");
        sqlite
            .execute_batch(
                "CREATE TABLE vendas (id INTEGER PRIMARY KEY, qt_venda INTEGER NOT NULL);
                 INSERT INTO vendas (qt_venda) VALUES (10), (30), (20);",
            )
            .expect("cria a tabela de vendas");
        drop(sqlite);
        let mut config = Config::load(&b.config_path).expect("carrega");
        config.conexoes[0]
            .fontes
            .insert("vendas".into(), Fonte::default());
        config.save(&b.config_path).expect("salva o cadastro");
        b
    }

    /// As duas colunas diferem só pelo sufixo. Os valores são incompatíveis de
    /// propósito: ordenar pela coluna errada não produz o mesmo primeiro registro.
    fn bancada_ordem_literal() -> Bancada {
        let b = bancada();
        let sqlite = rusqlite::Connection::open(&b.raiz.join("estoque.db")).expect("abre o sqlite");
        sqlite
            .execute_batch(
                "CREATE TABLE ordens (id INTEGER PRIMARY KEY, status INTEGER NOT NULL, \"status DESC\" INTEGER NOT NULL);
                 INSERT INTO ordens (status, \"status DESC\") VALUES (1, 30), (2, 10), (3, 20);",
            )
            .expect("cria colunas com e sem sufixo");
        drop(sqlite);
        let mut config = Config::load(&b.config_path).expect("carrega");
        config.conexoes[0]
            .fontes
            .insert("ordens".into(), Fonte::default());
        config.save(&b.config_path).expect("salva o cadastro");
        b
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
    fn as_descricoes_de_ordem_explicam_as_duas_formas_sem_prometer_sql() {
        let ferramenta = catalogo()
            .into_iter()
            .find(|f| f.nome == "bancos_consultar")
            .expect("consulta no catálogo");
        let propriedades = &ferramenta.esquema["properties"];
        let ordem = propriedades["ordem"]["description"].as_str().unwrap();
        let desc = propriedades["ordemDesc"]["description"].as_str().unwrap();
        for trecho in [
            "qt_venda DESC",
            "qt_venda ASC",
            "ordemDesc",
            "uma única coluna",
        ] {
            assert!(ordem.contains(trecho), "{ordem}");
        }
        assert!(desc.contains("ASC") && desc.contains("DESC"), "{desc}");
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
    fn ordem_com_sufixo_desc_ordena_de_verdade() {
        let b = bancada_vendas();
        let saida = executar(
            "bancos_consultar",
            json!({ "conexao": "Estoque", "fonte": "vendas", "ordem": "qt_venda DESC" }),
            &b.config_path,
        )
        .expect("ordem descendente legada");
        assert_eq!(saida.vista.para_json()["linhas"][0][1], 30);
        std::fs::remove_dir_all(&b.raiz).ok();
    }

    #[test]
    fn ordem_com_sufixo_asc_ordena_de_verdade() {
        let b = bancada_vendas();
        let saida = executar(
            "bancos_consultar",
            json!({ "conexao": "Estoque", "fonte": "vendas", "ordem": "qt_venda ASC" }),
            &b.config_path,
        )
        .expect("ordem crescente legada");
        assert_eq!(saida.vista.para_json()["linhas"][0][1], 10);
        std::fs::remove_dir_all(&b.raiz).ok();
    }

    #[test]
    fn sufixo_de_ordem_aceita_minusculas_e_caixa_da_coluna() {
        let b = bancada_vendas();
        for (ordem, esperado) in [("qt_venda desc", 30), ("QT_VENDA asc", 10)] {
            let saida = executar(
                "bancos_consultar",
                json!({ "conexao": "Estoque", "fonte": "vendas", "ordem": ordem }),
                &b.config_path,
            )
            .expect("sufixo sem diferenciar caixa");
            assert_eq!(saida.vista.para_json()["linhas"][0][1], esperado, "{ordem}");
        }
        std::fs::remove_dir_all(&b.raiz).ok();
    }

    #[test]
    fn coluna_valida_com_ordem_desc_continua_funcionando() {
        let b = bancada_vendas();
        for (desc, esperado) in [(true, 30), (false, 10)] {
            let saida = executar(
                "bancos_consultar",
                json!({
                    "conexao": "Estoque",
                    "fonte": "vendas",
                    "ordem": "qt_venda",
                    "ordemDesc": desc,
                }),
                &b.config_path,
            )
            .expect("nome de coluna com direção separada");
            assert_eq!(saida.vista.para_json()["linhas"][0][1], esperado);
        }
        std::fs::remove_dir_all(&b.raiz).ok();
    }

    #[test]
    fn sufixo_explicito_prevalece_sobre_ordem_desc() {
        let b = bancada_vendas();
        for (ordem, desc, esperado) in [("qt_venda ASC", true, 10), ("qt_venda DESC", false, 30)] {
            let saida = executar(
                "bancos_consultar",
                json!({
                    "conexao": "Estoque",
                    "fonte": "vendas",
                    "ordem": ordem,
                    "ordemDesc": desc,
                }),
                &b.config_path,
            )
            .expect("direção explícita no argumento legado");
            assert_eq!(saida.vista.para_json()["linhas"][0][1], esperado, "{ordem}");
        }
        std::fs::remove_dir_all(&b.raiz).ok();
    }

    #[test]
    fn coluna_literal_com_sufixo_usa_o_schema_sem_viagem_extra() {
        let b = bancada_ordem_literal();
        for (ordem, desc, primeiro_id) in [
            ("status DESC", false, 2),
            ("status DESC", true, 1),
            ("status", false, 1),
            ("status", true, 3),
        ] {
            crate::bancos::motor::sqlite::leitura_zerada();
            let saida = executar(
                "bancos_consultar",
                json!({
                    "conexao": "Estoque",
                    "fonte": "ordens",
                    "ordem": ordem,
                    "ordemDesc": desc,
                }),
                &b.config_path,
            )
            .expect("ordem sobre coluna literal e direção separada");
            assert_eq!(saida.vista.para_json()["linhas"][0][0], primeiro_id, "{ordem}");
            assert_eq!(
                crate::bancos::motor::sqlite::comandos_contados(),
                2,
                "schema e dado continuam sendo as duas únicas viagens"
            );
        }
        std::fs::remove_dir_all(&b.raiz).ok();
    }

    #[test]
    fn ordem_desc_aceita_so_booleano_e_o_texto_chega_cru_ao_motor() {
        for valor in [json!("DESC"), json!("true"), json!(1), json!({}), json!([]), json!(null)] {
            let args = json!({ "ordem": "qt_venda", "ordemDesc": valor });
            let erro = ordem_pedida(args.as_object().unwrap()).unwrap_err();
            assert_eq!(erro.to_string(), tr("erros.bancoOrdemInvalida", &[]));
        }
        let args = json!({ "ordemDesc": 1 });
        assert!(ordem_pedida(args.as_object().unwrap()).is_err());

        for (valor, esperado) in [(json!(true), true), (json!(false), false)] {
            let args = json!({ "ordem": " status DESC ", "ordemDesc": valor });
            assert_eq!(
                ordem_pedida(args.as_object().unwrap()).unwrap(),
                Some(("status DESC".to_string(), esperado)),
                "o sufixo não pode ser decidido antes do schema"
            );
        }
        let args = json!({ "ordem": "status DESC" });
        assert_eq!(
            ordem_pedida(args.as_object().unwrap()).unwrap(),
            Some(("status DESC".to_string(), false))
        );
    }

    #[test]
    fn ordem_invalida_ou_injetada_e_recusada_com_orientacao_util() {
        let b = bancada_vendas();
        for entrada in [
            "ORDER BY qt_venda",
            "qt_venda, id",
            "qt_venda + 1",
            "qt_venda DESC, id ASC",
            "qt_venda; DROP TABLE vendas",
            "qt_venda DESC; DROP TABLE vendas",
            "qt_venda DESC NULLS LAST",
            "nao_existe DESC",
            "$where",
        ] {
            let erro = executar(
                "bancos_consultar",
                json!({ "conexao": "Estoque", "fonte": "vendas", "ordem": entrada }),
                &b.config_path,
            )
            .expect_err("ordem inválida não chega ao banco");
            let texto = erro.to_string();
            for trecho in ["id", "qt_venda", "ordem", "ordemDesc", "bancos_colunas"] {
                assert!(texto.contains(trecho), "{entrada}: {texto}");
            }
        }
        let contagem = executar(
            "bancos_contar",
            json!({ "conexao": "Estoque", "fonte": "vendas" }),
            &b.config_path,
        )
        .expect("a tabela continua íntegra");
        assert_eq!(contagem.vista.para_json()["linhas"], 3);
        std::fs::remove_dir_all(&b.raiz).ok();
    }

    #[test]
    fn ordem_com_tipo_invalido_ou_vazia_nao_e_ignorada() {
        let b = bancada_vendas();
        for entrada in [json!([]), json!({}), json!(""), json!(42)] {
            let erro = executar(
                "bancos_consultar",
                json!({ "conexao": "Estoque", "fonte": "vendas", "ordem": entrada }),
                &b.config_path,
            )
            .expect_err("ordem inválida não vira ausência silenciosa");
            assert!(erro.to_string().contains("ordem"), "{erro}");
        }
        std::fs::remove_dir_all(&b.raiz).ok();
    }

    #[test]
    fn ordem_normalizada_mantem_o_filtro_como_parametro() {
        let b = bancada_vendas();
        crate::bancos::motor::sqlite::leitura_zerada();
        let saida = executar(
            "bancos_consultar",
            json!({
                "conexao": "Estoque",
                "fonte": "vendas",
                "filtro": { "id": 2 },
                "ordem": "qt_venda DESC",
            }),
            &b.config_path,
        )
        .expect("filtro e ordem na mesma leitura");
        let json = saida.vista.para_json();
        assert_eq!(json["linhas"][0][1], 30);
        assert_eq!(json["total"], 1);
        let textos = crate::bancos::motor::sqlite::comandos_lidos();
        let select = &textos[1];
        assert!(
            select.contains("WHERE \"id\" = ? ORDER BY \"qt_venda\" DESC"),
            "o valor não pode entrar no SQL: {select}"
        );
        assert!(
            !select.contains("= 2"),
            "o valor do filtro foi interpolado: {select}"
        );
        std::fs::remove_dir_all(&b.raiz).ok();
    }

    /// A forma documentada do filtro mantém a comparação no banco, o valor
    /// ligado e a alça de leitura: nada de SQL interpolado nem escrita no arquivo.
    #[test]
    fn filtro_com_op_e_valor_compara_como_parametro_em_leitura() {
        let b = bancada_vendas();
        let antes = std::fs::read(&b.raiz.join("estoque.db")).expect("lê o arquivo");
        crate::bancos::motor::sqlite::leitura_zerada();
        let saida = executar(
            "bancos_consultar",
            json!({
                "conexao": "Estoque",
                "fonte": "vendas",
                "filtro": { "qt_venda": { "op": "maior", "valor": 10 } },
            }),
            &b.config_path,
        )
        .expect("filtro documentado");
        let json = saida.vista.para_json();
        assert_eq!(json["total"], 2);
        assert_eq!(json["linhas"][0][1], 30);
        assert_eq!(json["linhas"][1][1], 20);
        let textos = crate::bancos::motor::sqlite::comandos_lidos();
        let select = &textos[1];
        assert!(select.contains("WHERE \"qt_venda\" > ?"), "{select}");
        assert!(!select.contains("> 10"), "o valor foi interpolado: {select}");
        assert_eq!(crate::bancos::motor::sqlite::comandos_contados(), 2);
        let depois = std::fs::read(&b.raiz.join("estoque.db")).expect("relê o arquivo");
        assert_eq!(antes, depois, "a consulta não pode escrever");
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
        for trecho in [
            "sqlite",
            "bancos_colunas",
            "ordem",
            "ordemDesc",
            "ASC",
            "DESC",
        ] {
            assert!(
                saida.texto.contains(trecho),
                "orientação ausente: {}",
                saida.texto
            );
        }
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

    /// A régua de alocações do teste de serialização. Local à linha e ligada
    /// só durante a medida: os outros testes do crate alocam em paralelo na
    /// mesma hora e não têm nada a ver com o que está sendo medido aqui.
    mod regua {
        use std::alloc::{GlobalAlloc, Layout, System};
        use std::cell::Cell;

        thread_local! {
            static CONTANDO: Cell<bool> = const { Cell::new(false) };
            static ALOCACOES: Cell<usize> = const { Cell::new(0) };
        }

        pub struct Regua;

        // A medida não toca em memória própria: conta e entrega ao sistema.
        unsafe impl GlobalAlloc for Regua {
            unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
                CONTANDO.with(|ligada| {
                    if ligada.get() {
                        ALOCACOES.with(|a| a.set(a.get() + 1));
                    }
                });
                unsafe { System.alloc(layout) }
            }
            unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, novo: usize) -> *mut u8 {
                CONTANDO.with(|ligada| {
                    if ligada.get() {
                        ALOCACOES.with(|a| a.set(a.get() + 1));
                    }
                });
                unsafe { System.realloc(ptr, layout, novo) }
            }
            unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
                unsafe { System.dealloc(ptr, layout) }
            }
        }

        #[global_allocator]
        static REGUA: Regua = Regua;

        /// Roda `f` devolvendo o resultado e quantas vezes a linha alocou.
        pub fn medindo<T>(f: impl FnOnce() -> T) -> (T, usize) {
            ALOCACOES.with(|a| a.set(0));
            CONTANDO.with(|ligada| ligada.set(true));
            let resultado = f();
            CONTANDO.with(|ligada| ligada.set(false));
            (resultado, ALOCACOES.with(|a| a.get()))
        }
    }

    /// A resposta da leitura era montada duas vezes: `json!` copiava a grelha
    /// inteira para um Value e a mesma grelha ainda movia para a vista. O texto
    /// é o mesmo, byte a byte — o que não pode mais é custar uma cópia.
    #[test]
    fn a_saida_da_leitura_nao_copia_a_grelha() {
        let consulta = motor::Consulta {
            colunas: (0..8).map(|i| format!("coluna {i}")).collect(),
            linhas: (0..200)
                .map(|l| (0..8).map(|i| Value::String(format!("valor {i} da linha {l}"))).collect())
                .collect(),
            truncado: false,
            duracao_ms: 12,
        };
        // A forma antiga, construída fora da medida: serve de referência do que
        // o modelo sempre leu, e de prova de que a mudança não altera a saída.
        let referencia = serde_json::to_string(&json!({
            "colunas": consulta.colunas,
            "linhas": consulta.linhas,
            "truncado": consulta.truncado,
            "duracaoMs": consulta.duracao_ms,
        }))
        .expect("referência serializável");
        let (texto, alocacoes) = regua::medindo(|| texto_da_consulta(&consulta));
        assert_eq!(texto, referencia, "defeito 6: o texto que o modelo lê mudou de forma");
        assert!(
            alocacoes <= 32,
            "defeito 6: serializar a grelha de 200×8 custou {alocacoes} alocações — ela está \
             sendo copiada antes de virar texto, em vez de serializada no lugar"
        );
    }

    /// A ferramenta de leitura faz as duas viagens do motor no arquivo — catálogo
    /// uma, dado uma — e devolve o texto com as chaves de sempre, na ordem de
    /// sempre.
    #[test]
    fn a_ferramenta_de_leitura_faz_duas_viagens_no_arquivo() {
        let b = bancada();
        crate::bancos::motor::sqlite::leitura_zerada();
        let saida = executar(
            "bancos_consultar",
            json!({ "conexao": "Estoque", "fonte": "clientes" }),
            &b.config_path,
        )
        .expect("consulta");
        let viagens = crate::bancos::motor::sqlite::comandos_contados();
        assert_eq!(viagens, 2, "defeito 2: bancos_consultar custou {viagens} declarações no arquivo");
        assert!(
            saida.texto.starts_with(
                "{\"colunas\":[\"id\",\"nome\"],\"linhas\":[[1,\"Ana\"],[2,\"Bruno\"]],\"truncado\":false,\"duracaoMs\":"
            ),
            "o texto da leitura saiu diferente: {}",
            saida.texto
        );
        assert!(saida.texto.ends_with('}'), "o texto não fecha o objeto: {}", saida.texto);
        std::fs::remove_dir_all(&b.raiz).ok();
    }

    /// Ler pela ferramenta não pode escrever: a alça que `bancos_consultar` abre
    /// é a de leitura, e uma escrita pelo `bancos_sql` continua possível onde o
    /// dono ligou o SQL livre.
    #[test]
    fn a_leitura_da_ferramenta_nao_deixa_rastro_no_arquivo() {
        let b = bancada();
        let antes = std::fs::read(&b.raiz.join("estoque.db")).expect("lê o arquivo");
        for _ in 0..2 {
            executar(
                "bancos_consultar",
                json!({ "conexao": "Estoque", "fonte": "clientes" }),
                &b.config_path,
            )
            .expect("consulta");
        }
        let depois = std::fs::read(&b.raiz.join("estoque.db")).expect("relê o arquivo");
        assert_eq!(antes, depois, "defeito 7: a ferramenta de leitura mexeu no arquivo");
        std::fs::remove_dir_all(&b.raiz).ok();
    }
}
