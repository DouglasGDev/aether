//! O motor Oracle: driver oficial da Oracle em modo thin — o protocolo TNS/TTC
//! falado em Rust puro, sem Instant Client nem biblioteca nativa instalada. A
//! conexão precisa de usuário e senha (o protocolo não abre sessão anônima) e
//! de um serviço, que chega do cadastro como o nome do banco. Os valores do
//! modelo entram como texto ligado a `:n` e convertido pelo alvo que a coluna
//! declara (`TO_NUMBER`, `TO_DATE`); nomes de tabela e coluna são conferidos
//! contra `user_tables`/`user_tab_columns` antes de qualquer interpolação.

use anyhow::{Result, anyhow, bail};
use oracledb::{
    Config as ConfigOracle, Connection, Cursor, JsonValue, Lob, OracleIntervalDS, OracleIntervalYM,
    OracleNumber, OracleTimestamp, Row, ToDbValue, Vector, VectorData,
};
use serde_json::{Map, Value};

use super::{
    Comparacao, Consulta, Cronometro, Filtro, Motor, PRAZO_DE_LEITURA, ResultadoSql,
    aparar_no_teto, erro_coluna_invalida, ordem_de, teto_de_leitura,
};
use crate::bancos::config::Conexao;
use crate::idiomas::tr;

/// A ida única ao catálogo de uma fonte: a tabela conferida e as colunas dela,
/// na ordem e com o nome que o próprio banco grafa. `UPPER(...) = UPPER(:1)` é o
/// que fazia a busca da lista ser insensível, sem trazer a lista inteira — e o
/// nome entra como parâmetro cru, porque entre aspas ele não é o nome que o
/// dicionário guarda.
const ESQUEMA_SQL: &str = "SELECT t.table_name, c.column_name, c.data_type \
     FROM user_tables t JOIN user_tab_columns c ON c.table_name = t.table_name \
     WHERE UPPER(t.table_name) = UPPER(:1) ORDER BY c.column_id";

/// O nome entre aspas duplas, pronto para entrar no SQL: sempre depois de
/// conferido no dicionário — nunca interpolado do texto do modelo.
fn conferido(nome: &str) -> String {
    format!("\"{}\"", nome.replace('"', "\"\""))
}

/// O nome como o dicionário o guarda: cru. Entre aspas ele vira `""NOME""` e não
/// acha tabela nem coluna nenhuma — é assim que uma fonte existente passa a
/// responder «coluna inexistente» em toda leitura.
fn nome_no_dicionario(fonte: &str) -> String {
    fonte.trim().to_string()
}

/// Como ler uma célula, decidido pelo tipo que o banco declara na coluna. O
/// driver não converte por conta própria — cada estratégia pede o tipo Rust
/// certo ao `Row::get`, e o que não tem leitura de dados no driver (objeto,
/// XML, vetor) vira uma marca que diz o que era, não um valor inventado.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Celula {
    Numero,
    Texto,
    Temporal,
    Real32,
    Real64,
    Booleana,
    Binaria,
    Json,
    IntervaloDS,
    IntervaloYM,
    Vetor,
    CursorAninhado,
    Exotica,
}

fn celula_de_tipo(db_type: &oracledb::DbType) -> Celula {
    match db_type.name() {
        "DB_TYPE_NUMBER" | "DB_TYPE_BINARY_INTEGER" => Celula::Numero,
        "DB_TYPE_VARCHAR"
        | "DB_TYPE_CHAR"
        | "DB_TYPE_NCHAR"
        | "DB_TYPE_NVARCHAR"
        | "DB_TYPE_LONG"
        | "DB_TYPE_LONG_NVARCHAR"
        | "DB_TYPE_ROWID"
        | "DB_TYPE_UROWID" => Celula::Texto,
        "DB_TYPE_DATE"
        | "DB_TYPE_TIMESTAMP"
        | "DB_TYPE_TIMESTAMP_LTZ"
        | "DB_TYPE_TIMESTAMP_TZ" => Celula::Temporal,
        "DB_TYPE_BINARY_FLOAT" => Celula::Real32,
        "DB_TYPE_BINARY_DOUBLE" => Celula::Real64,
        "DB_TYPE_BOOLEAN" => Celula::Booleana,
        "DB_TYPE_BLOB"
        | "DB_TYPE_RAW"
        | "DB_TYPE_LONG_RAW"
        | "DB_TYPE_BFILE"
        | "DB_TYPE_CLOB"
        | "DB_TYPE_NCLOB" => Celula::Binaria,
        "DB_TYPE_JSON" => Celula::Json,
        "DB_TYPE_INTERVAL_DS" => Celula::IntervaloDS,
        "DB_TYPE_INTERVAL_YM" => Celula::IntervaloYM,
        "DB_TYPE_VECTOR" => Celula::Vetor,
        "DB_TYPE_CURSOR" => Celula::CursorAninhado,
        _ => Celula::Exotica,
    }
}

/// O nome que `user_tab_columns` declara para o tipo → o alvo da conversão do
/// parâmetro textual. `None` é coluna de texto: o parâmetro entra cru.
fn alvo_de(tipo: &str) -> Option<&'static str> {
    let tipo = tipo.trim().to_ascii_uppercase();
    match tipo.as_str() {
        "NUMBER"
        | "INTEGER"
        | "FLOAT"
        | "BINARY_INTEGER"
        | "BINARY_FLOAT"
        | "BINARY_DOUBLE"
        | "DOUBLE PRECISION"
        | "REAL" => Some("numero"),
        "BOOLEAN" => Some("boolean"),
        tipo if tipo.starts_with("DATE") => Some("date"),
        tipo if tipo.starts_with("TIMESTAMP") => Some("timestamp"),
        _ => None,
    }
}

/// O número que o Oracle devolve em texto exato vira número JSON quando dá —
/// o que não cabe em i64 nem f64 segue como o texto que o banco entregou.
fn numero_para_json(texto: &str) -> Value {
    if let Ok(i) = texto.parse::<i64>() {
        return Value::Number(i.into());
    }
    if let Ok(f) = texto.parse::<f64>() {
        if let Some(n) = serde_json::Number::from_f64(f) {
            return Value::Number(n);
        }
    }
    Value::String(texto.to_string())
}

fn json_para_json(valor: &JsonValue) -> Value {
    match valor {
        JsonValue::BinaryDouble(d) => numero_para_json(&d.to_string()),
        JsonValue::BinaryFloat(f) => numero_para_json(&f64::from(*f).to_string()),
        JsonValue::Boolean(b) => Value::Bool(*b),
        JsonValue::IntervalDS(i) => Value::String(i.to_string()),
        JsonValue::IntervalYM(i) => Value::String(i.to_string()),
        JsonValue::JsonArray(lista) => Value::Array(lista.iter().map(json_para_json).collect()),
        JsonValue::JsonId(id) | JsonValue::Raw(id) => Value::String(
            id.iter().map(|b| format!("{b:02X}")).collect::<String>(),
        ),
        JsonValue::JsonObject(objeto) => Value::Object(
            objeto
                .iter()
                .map(|(chave, valor)| (chave.clone(), json_para_json(valor)))
                .collect(),
        ),
        JsonValue::Null => Value::Null,
        JsonValue::Number(n) => numero_para_json(&n.to_string()),
        JsonValue::String(texto) => Value::String(texto.clone()),
        JsonValue::Timestamp(t) => Value::String(t.to_string()),
        JsonValue::Vector(v) => Value::String(format!("[vetor {} dimensões]", vetor_dimensoes(v))),
    }
}

/// As dimensões de um vetor: o contador no `Vector` é privado, mas as variantes
/// são públicas — denso conta os valores (binário tem 8 por byte), esparso
/// traz o número dele.
fn vetor_dimensoes(vetor: &Vector) -> usize {
    match vetor {
        Vector::Dense(VectorData::Float32(valores)) => valores.len(),
        Vector::Dense(VectorData::Float64(valores)) => valores.len(),
        Vector::Dense(VectorData::Int8(valores)) => valores.len(),
        Vector::Dense(VectorData::Binary(bytes)) => bytes.len() * 8,
        Vector::Sparse(esparsa) => esparsa.num_dimensions(),
    }
}

fn valor_texto(valor: &Value) -> Result<String> {
    match valor {
        Value::String(texto) => Ok(texto.clone()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        _ => bail!(tr("erros.filtroValorInvalido", &[])),
    }
}

fn erro(e: oracledb::Error) -> anyhow::Error {
    anyhow!(tr("erros.bancoErro", &[("detalhe", &e.to_string())]))
}

pub struct Oracle {
    conexao: Connection,
}

/// A string Easy Connect desta conexão — a única leitura da flag
/// `tls_verificado` neste motor, e ela muda só o protocolo da string.
///
/// Sem a marca, a forma lisa de sempre (`host:porta/serviço`), que o driver
/// resolve como TCP puro: é o que havia antes, e é o que continua havendo para
/// quem não marcou nada. Com a marca, o prefixo `tcps://`: o próprio
/// `oracledb` decide o canal pelo protocolo escrito na string, e o TLS que ele
/// monta em cima é `rustls`, compilado sem feature nenhuma — conferir
/// `Cargo.lock` na entrada de `oracledb`. Não é número escrito à mão: a porta
/// é a mesma dos dois lados.
fn string_de_conexao(conexao: &Conexao) -> String {
    let servidor = format!("{}:{}", conexao.host.trim(), conexao.porta_efetiva());
    let servico = conexao.banco.trim();
    if conexao.tls_verificado {
        format!("tcps://{servidor}/{servico}")
    } else {
        format!("{servidor}/{servico}")
    }
}

impl Oracle {
    pub fn abrir(ficha: &Conexao) -> Result<Self> {
        let usuario = ficha.usuario.trim().to_string();
        if usuario.is_empty() {
            bail!(tr("erros.bancoUsuarioFaltando", &[]));
        }
        if ficha.banco.trim().is_empty() {
            bail!(tr("erros.bancoNomeBancoFaltando", &[]));
        }
        // Easy Connect: host:porta/serviço (ou tcps://host:porta/serviço com a
        // flag ligada). O driver fala TNS por baixo; a string é a forma que o
        // protocolo aceita sem tnsnames.ora.
        let string_conexao = string_de_conexao(ficha);
        let config = ConfigOracle::default()
            .set_connect_string(&string_conexao)
            .map_err(erro)?
            .set_credentials(&usuario, &ficha.senha()?);
        let conexao = oracledb::connect(config).map_err(erro)?;
        // O prazo do motorista: `call_timeout` é o relógio que o próprio driver
        // impõe a cada chamada de rede. Sem ele, um servidor que para de
        // responder à consulta deixa a ferramenta pendurada até o dono fechar o
        // aplicativo — e o thin mode não tem variável de sessão que faça isso.
        conexao.set_call_timeout(Some(PRAZO_DE_LEITURA)).map_err(erro)?;
        Ok(Self { conexao })
    }

    /// A única ida ao catálogo que uma leitura precisa: a fonte conferida, com o
    /// nome que o banco grafa, e as colunas dela. Antes eram duas — e a segunda
    /// recebia o nome já entre aspas, que no dicionário é `""NOME""` e não acha
    /// coluna nenhuma: toda leitura com coluna ou filtro nomeados caía em
    /// «coluna inexistente».
    fn esquema_de(&self, fonte: &str) -> Result<(String, Vec<(String, String)>)> {
        let procurada = nome_no_dicionario(fonte);
        let refs: Vec<&dyn ToDbValue> = vec![&procurada];
        let cursor = self.conexao.query(ESQUEMA_SQL, &refs).map_err(erro)?;
        let mut conferida: Option<String> = None;
        let mut esquema = Vec::new();
        for linha in cursor {
            let linha = linha.map_err(erro)?;
            let nome_tabela = linha.get::<Option<String>>(0).map_err(erro)?.unwrap_or_default();
            let nome = linha.get::<Option<String>>(1).map_err(erro)?.unwrap_or_default();
            let tipo = linha.get::<Option<String>>(2).map_err(erro)?.unwrap_or_default();
            if conferida.is_none() && !nome_tabela.is_empty() {
                conferida = Some(nome_tabela);
            }
            if !nome.is_empty() {
                esquema.push((nome, tipo));
            }
        }
        let conferida = conferida
            .ok_or_else(|| anyhow!(tr("erros.bancoFonteInexistente", &[("fonte", fonte)])))?;
        Ok((conferida, esquema))
    }

    fn conferir_coluna(coluna: &str, conhecidas: &[(String, String)]) -> Result<String> {
        conhecidas
            .iter()
            .find(|(nome, _)| nome.eq_ignore_ascii_case(coluna))
            .map(|(nome, _)| conferido(&nome))
            .ok_or_else(|| erro_coluna_invalida(coluna, conhecidas))
    }

    /// A marca do parâmetro com o valor que o modelo mandou — `TO_NUMBER(:1)`,
    /// `TO_DATE(:2, …)` ou `:3` cru, conforme a coluna. Nulo entra como
    /// `NULL` literal, sem parâmetro nenhum.
    fn marca_com_valor(
        coluna: &str,
        valor: &Value,
        schema: &[(String, String)],
        params: &mut Vec<String>,
    ) -> Result<String> {
        if valor.is_null() {
            return Ok(String::from("NULL"));
        }
        let alvo = schema
            .iter()
            .find(|(nome, _)| nome.eq_ignore_ascii_case(coluna))
            .and_then(|(_, tipo)| alvo_de(tipo));
        params.push(valor_texto(valor)?);
        let indice = params.len();
        Ok(match alvo {
            None => format!(":{indice}"),
            Some("numero") => format!("TO_NUMBER(:{indice})"),
            Some("boolean") => format!("TO_BOOLEAN(:{indice})"),
            Some("date") => format!("TO_DATE(:{indice}, 'YYYY-MM-DD\"T\"HH24:MI:SS')"),
            Some("timestamp") => {
                format!("TO_TIMESTAMP(:{indice}, 'YYYY-MM-DD\"T\"HH24:MI:SS.FF6')")
            }
            Some(_) => format!(":{indice}"),
        })
    }

    fn onde_sql(
        filtro: &Filtro,
        schema: &[(String, String)],
        params: &mut Vec<String>,
    ) -> Result<String> {
        let mut partes = Vec::new();
        for condicao in filtro {
            let coluna = Self::conferir_coluna(&condicao.coluna, schema)?;
            let Comparacao::Contendo = condicao.comparacao else {
                if condicao.valor.is_null() {
                    match condicao.comparacao {
                        Comparacao::Igual => partes.push(format!("{coluna} IS NULL")),
                        Comparacao::Diferente => partes.push(format!("{coluna} IS NOT NULL")),
                        _ => bail!(tr("erros.filtroNuloSemComparacao", &[])),
                    }
                    continue;
                }
                let marca = Self::marca_com_valor(&condicao.coluna, &condicao.valor, schema, params)?;
                let operador = match condicao.comparacao {
                    Comparacao::Igual => "=",
                    Comparacao::Diferente => "<>",
                    Comparacao::Maior => ">",
                    Comparacao::MaiorIgual => ">=",
                    Comparacao::Menor => "<",
                    Comparacao::MenorIgual => "<=",
                    Comparacao::Contendo => unreachable!("conferido acima"),
                };
                partes.push(format!("{coluna} {operador} {marca}"));
                continue;
            };
            // "contendo" é LIKE com o valor escapado e envolto em %…%: o que o
            // modelo escreveu é literal, não padrão que ele improvisa.
            let cru = valor_texto(&condicao.valor)?;
            let escapado = cru
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            params.push(format!("%{escapado}%"));
            let indice = params.len();
            partes.push(format!("{coluna} LIKE :{indice} ESCAPE '\\'"));
        }
        if partes.is_empty() {
            Ok(String::new())
        } else {
            Ok(format!(" WHERE {}", partes.join(" AND ")))
        }
    }

    fn ler_celula(linha: &Row, indice: usize, celula: Celula) -> Result<Value> {
        Ok(match celula {
            Celula::Numero => match linha.get::<Option<OracleNumber>>(indice).map_err(erro)? {
                Some(numero) => numero_para_json(&numero.to_string()),
                None => Value::Null,
            },
            Celula::Texto => linha.get::<Option<String>>(indice).map_err(erro)?.map(Value::String).unwrap_or(Value::Null),
            Celula::Temporal => {
                match linha.get::<Option<OracleTimestamp>>(indice).map_err(erro)? {
                    Some(momento) => Value::String(momento.to_string()),
                    None => Value::Null,
                }
            }
            Celula::Real32 => match linha.get::<Option<f32>>(indice).map_err(erro)? {
                Some(numero) => serde_json::Number::from_f64(f64::from(numero))
                    .map(Value::Number)
                    .unwrap_or(Value::Null),
                None => Value::Null,
            },
            Celula::Real64 => match linha.get::<Option<f64>>(indice).map_err(erro)? {
                Some(numero) => serde_json::Number::from_f64(numero)
                    .map(Value::Number)
                    .unwrap_or(Value::Null),
                None => Value::Null,
            },
            Celula::Booleana => linha.get::<Option<bool>>(indice).map_err(erro)?.map(Value::Bool).unwrap_or(Value::Null),
            Celula::Binaria => {
                match linha.get::<Option<Lob>>(indice).map_err(erro)? {
                    Some(mut lob) => {
                        let tipo = lob.db_type().name().trim_start_matches("DB_TYPE_").to_ascii_lowercase();
                        match lob.get_size() {
                            Ok(tamanho) => Value::String(format!("[{tipo} {tamanho} bytes]")),
                            // BFILE pode não reportar tamanho sem abrir o
                            // arquivo; a marca do tipo já diz o que era.
                            Err(_) => Value::String(format!("[{tipo}]")),
                        }
                    }
                    None => Value::Null,
                }
            }
            Celula::Json => {
                match linha.get::<Option<JsonValue>>(indice).map_err(erro)? {
                    Some(valor) => json_para_json(&valor),
                    None => Value::Null,
                }
            }
            Celula::IntervaloDS => {
                match linha.get::<Option<OracleIntervalDS>>(indice).map_err(erro)? {
                    Some(intervalo) => Value::String(intervalo.to_string()),
                    None => Value::Null,
                }
            }
            Celula::IntervaloYM => {
                match linha.get::<Option<OracleIntervalYM>>(indice).map_err(erro)? {
                    Some(intervalo) => Value::String(intervalo.to_string()),
                    None => Value::Null,
                }
            }
            Celula::Vetor => {
                match linha.get::<Option<Vector>>(indice).map_err(erro)? {
                    Some(vetor) => {
                        Value::String(format!("[vetor {} dimensões]", vetor_dimensoes(&vetor)))
                    }
                    None => Value::Null,
                }
            }
            Celula::CursorAninhado => {
                match linha.get::<Option<Cursor>>(indice) {
                    Ok(Some(_)) | Err(_) => Value::String("[cursor]".into()),
                    Ok(None) => Value::Null,
                }
            }
            Celula::Exotica => {
                // Objeto e XML: o driver não entrega dados prontos aqui. Tentar
                // texto e depois LOB; sem leitura possível, a marca diz o tipo.
                match linha.get::<Option<String>>(indice) {
                    Ok(Some(texto)) => Value::String(texto),
                    Ok(None) => Value::Null,
                    Err(_) => match linha.get::<Option<Lob>>(indice).map_err(erro)? {
                        Some(mut lob) => match lob.get_size() {
                            Ok(tamanho) => Value::String(format!("[lob {tamanho} bytes]")),
                            Err(_) => Value::String("[objeto]".into()),
                        },
                        None => Value::Null,
                    },
                }
            }
        })
    }

    /// Lê um cursor aberto até o teto de linhas. Os nomes e as estratégias
    /// saem dos metadados antes da iteração: o cursor é emprestado de novo a
    /// cada linha e nada de `columns()` vive durante o laço.
    fn ler_cursor(
        mut cursor: Cursor,
        limite: usize,
        cronometro: &Cronometro,
    ) -> Result<Consulta> {
        let estrategias: Vec<(String, Celula)> = cursor
            .columns()
            .iter()
            .map(|meta| (meta.name().to_string(), celula_de_tipo(meta.db_type())))
            .collect();
        let mut linhas: Vec<Vec<Value>> = Vec::new();
        while linhas.len() < teto_de_leitura(limite) {
            let Some(linha) = cursor.next() else {
                break;
            };
            let linha = linha.map_err(erro)?;
            linhas.push(
                estrategias
                    .iter()
                    .enumerate()
                    .map(|(indice, (_, celula))| Self::ler_celula(&linha, indice, *celula))
                    .collect::<Result<Vec<_>>>()?,
            );
        }
        let (linhas, truncado) = aparar_no_teto(linhas, limite);
        let colunas = estrategias.into_iter().map(|(nome, _)| nome).collect();
        Ok(Consulta { colunas, linhas, truncado, duracao_ms: cronometro.ms() })
    }

    fn escrita(&self, sql: &str, params: Vec<String>) -> Result<u64> {
        let refs: Vec<&dyn ToDbValue> =
            params.iter().map(|p| p as &dyn ToDbValue).collect::<Vec<_>>();
        let resultado = self.conexao.execute(sql, &refs).map_err(erro)?;
        // O protocolo Oracle não confirma sozinho: sem commit a escrita
        // evaporaria quando a conexão episódica fechasse.
        self.conexao.commit().map_err(erro)?;
        Ok(resultado.rows_affected())
    }
}

impl Motor for Oracle {
    fn fontes(&mut self) -> Result<Vec<String>> {
        let cursor = self
            .conexao
            .query("SELECT table_name FROM user_tables ORDER BY table_name", &[])
            .map_err(erro)?;
        let mut nomes = Vec::new();
        for linha in cursor {
            let linha = linha.map_err(erro)?;
            nomes.push(linha.get::<Option<String>>(0).map_err(erro)?.unwrap_or_default());
        }
        Ok(nomes)
    }

    fn colunas(&mut self, fonte: &str) -> Result<Vec<(String, String)>> {
        Ok(self.esquema_de(fonte)?.1)
    }

    fn consultar(
        &mut self,
        fonte: &str,
        colunas_pedidas: &[String],
        filtro: &Filtro,
        ordem: Option<(&str, bool)>,
        limite: usize,
    ) -> Result<Consulta> {
        let cronometro = Cronometro::agora();
        let (cru, schema) = self.esquema_de(fonte)?;
        let fonte = conferido(&cru);
        // Sem colunas pedidas entram as que o dicionário tem — que é o que a
        // fonte autoriza. O `*` do lado de fora não alarga nada: quem decide a
        // lista é o embrulho de dentro.
        let colunas_sql = if colunas_pedidas.is_empty() {
            schema
                .iter()
                .map(|(nome, _)| conferido(nome))
                .collect::<Vec<_>>()
                .join(", ")
        } else {
            colunas_pedidas
                .iter()
                .map(|c| Self::conferir_coluna(c, &schema))
                .collect::<Result<Vec<_>>>()?
                .join(", ")
        };
        let mut params = Vec::new();
        let onde = Self::onde_sql(filtro, &schema, &mut params)?;
        let ordenar = match ordem {
            None => String::new(),
            Some((texto, desc)) => {
                let (coluna, desc) = ordem_de(texto, desc, Some(&schema))?;
                let coluna = Self::conferir_coluna(&coluna, &schema)?;
                format!(" ORDER BY {coluna} {}", if desc { "DESC" } else { "ASC" })
            }
        };
        // Uma linha a mais do que o teto: é ela que diz que o corte foi por
        // limite, e não que o banco acabou ali.
        let sql = format!(
            "SELECT * FROM (SELECT {colunas_sql} FROM {fonte}{onde}{ordenar}) WHERE ROWNUM <= {}",
            teto_de_leitura(limite)
        );
        let refs: Vec<&dyn ToDbValue> =
            params.iter().map(|p| p as &dyn ToDbValue).collect::<Vec<_>>();
        let cursor = self.conexao.query(&sql, &refs).map_err(erro)?;
        Self::ler_cursor(cursor, limite, &cronometro)
    }

    fn contar(&mut self, fonte: &str, filtro: &Filtro) -> Result<u64> {
        let (cru, schema) = self.esquema_de(fonte)?;
        let fonte = conferido(&cru);
        let mut params = Vec::new();
        let onde = Self::onde_sql(filtro, &schema, &mut params)?;
        let sql = format!("SELECT COUNT(*) FROM {fonte}{onde}");
        let refs: Vec<&dyn ToDbValue> =
            params.iter().map(|p| p as &dyn ToDbValue).collect::<Vec<_>>();
        let linha = self.conexao.query_row(&sql, &refs).map_err(erro)?;
        Ok(linha
            .get::<Option<OracleNumber>>(0)
            .map_err(erro)?
            .map(|n| n.to_string().parse::<u64>().unwrap_or(0))
            .unwrap_or(0))
    }

    fn inserir(&mut self, fonte: &str, valores: &Map<String, Value>) -> Result<u64> {
        let (cru, schema) = self.esquema_de(fonte)?;
        let fonte = conferido(&cru);
        if valores.is_empty() {
            bail!(tr("erros.bancoValoresVazios", &[]));
        }
        let mut colunas = Vec::new();
        let mut marcas = Vec::new();
        let mut params = Vec::new();
        for (coluna, valor) in valores {
            colunas.push(Self::conferir_coluna(coluna, &schema)?);
            marcas.push(Self::marca_com_valor(coluna, valor, &schema, &mut params)?);
        }
        let sql = format!(
            "INSERT INTO {fonte} ({}) VALUES ({})",
            colunas.join(", "),
            marcas.join(", ")
        );
        self.escrita(&sql, params)
    }

    fn atualizar(
        &mut self,
        fonte: &str,
        valores: &Map<String, Value>,
        filtro: &Filtro,
    ) -> Result<u64> {
        let (cru, schema) = self.esquema_de(fonte)?;
        let fonte = conferido(&cru);
        if valores.is_empty() {
            bail!(tr("erros.bancoValoresVazios", &[]));
        }
        if filtro.is_empty() {
            bail!(tr("erros.bancoOndeObrigatorio", &[]));
        }
        let mut atribuicoes = Vec::new();
        let mut params = Vec::new();
        for (coluna, valor) in valores {
            let coluna_sql = Self::conferir_coluna(coluna, &schema)?;
            let marca = Self::marca_com_valor(coluna, valor, &schema, &mut params)?;
            atribuicoes.push(format!("{coluna_sql} = {marca}"));
        }
        let mut onde_params = Vec::new();
        let onde = Self::onde_sql(filtro, &schema, &mut onde_params)?;
        params.extend(onde_params);
        let sql = format!("UPDATE {fonte} SET {}{onde}", atribuicoes.join(", "));
        self.escrita(&sql, params)
    }

    fn apagar(&mut self, fonte: &str, filtro: &Filtro) -> Result<u64> {
        let (cru, schema) = self.esquema_de(fonte)?;
        let fonte = conferido(&cru);
        if filtro.is_empty() {
            bail!(tr("erros.bancoOndeObrigatorio", &[]));
        }
        let mut params = Vec::new();
        let onde = Self::onde_sql(filtro, &schema, &mut params)?;
        let sql = format!("DELETE FROM {fonte}{onde}");
        self.escrita(&sql, params)
    }

    fn sql(&mut self, declaracao: &str, limite: usize) -> Result<ResultadoSql> {
        let cronometro = Cronometro::agora();
        let cru = declaracao.trim().trim_end_matches(';').trim();
        if cru.is_empty() {
            bail!(tr("erros.bancoSqlVazio", &[]));
        }
        // O parser do próprio driver classifica a declaração pelo texto — é o
        // banco de verdade dizendo o que ela é, antes de executar nada.
        let mut declaracao = self.conexao.statement(cru).map_err(erro)?.build().map_err(erro)?;
        if declaracao.is_query() {
            let cursor = declaracao.query(&[]).map_err(erro)?;
            Ok(ResultadoSql::Consulta(Self::ler_cursor(cursor, limite, &cronometro)?))
        } else {
            let resultado = declaracao.execute(&[]).map_err(erro)?;
            self.conexao.commit().map_err(erro)?;
            Ok(ResultadoSql::Afetadas(resultado.rows_affected()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bancos::config::{conexao_de_teste, Tipo};

    /// O nome da ordem continua sendo conferido no schema real; texto de SQL
    /// não vira coluna, e o erro ajuda o modelo a corrigir a chamada.
    #[test]
    fn coluna_de_ordem_invalida_lista_as_validas_e_o_exemplo() {
        let schema = vec![
            ("ID".to_string(), "NUMBER".to_string()),
            ("QT_VENDA".to_string(), "NUMBER".to_string()),
        ];
        assert_eq!(
            Oracle::conferir_coluna("qt_venda", &schema).unwrap(),
            "\"QT_VENDA\""
        );
        for entrada in [
            "qt_venda DESC",
            "qt_venda, id",
            "qt_venda; DROP TABLE vendas",
        ] {
            let erro = Oracle::conferir_coluna(entrada, &schema)
                .unwrap_err()
                .to_string();
            for trecho in [
                entrada,
                "ID",
                "QT_VENDA",
                "ordem",
                "ordemDesc",
                "bancos_colunas",
            ] {
                assert!(erro.contains(trecho), "{entrada}: {erro}");
            }
        }
    }

    /// O catálogo de uma fonte cabe numa declaração: a tabela conferida e as
    /// colunas dela na mesma junta. Duas viagens — a lista inteira de `user_tables`
    /// e depois `user_tab_columns` — é o que cada leitura pagava antes de ler.
    #[test]
    fn o_catalogo_de_uma_fonte_e_uma_ida_s() {
        assert!(
            ESQUEMA_SQL.contains("user_tables"),
            "defeito 2: sem a tabela no catálogo junto das colunas, a fonte é conferida numa viagem e lida em outra"
        );
        assert!(
            ESQUEMA_SQL.contains("user_tab_columns"),
            "defeito 2: sem as colunas na mesma viagem, cada leitura paga duas idas ao catálogo"
        );
        assert_eq!(
            ESQUEMA_SQL.matches(';').count(),
            0,
            "o catálogo de uma fonte tem de caber numa declaração"
        );
        assert!(
            ESQUEMA_SQL.contains("UPPER(t.table_name) = UPPER(:1)"),
            "sem a conferência insensível no servidor, a fonte digitada em outra caixa sumiria"
        );
    }

    /// O nome que vai para o dicionário é o cru. Entre aspas — que é o que o
    /// identificador citado é — `user_tab_columns.table_name` não tem como
    /// bater, e a fonte existente passava a responder «coluna inexistente» em
    /// toda leitura que nomeasse coluna.
    #[test]
    fn o_nome_que_se_pede_ao_dicionario_e_cru() {
        assert_eq!(nome_no_dicionario("  Pedes  "), "Pedes");
        assert!(
            !nome_no_dicionario("Pedes").contains('"'),
            "defeito 2: o nome citado não é o que o dicionário guarda"
        );
        assert_eq!(conferido("Pedes"), "\"Pedes\"", "o que entra no SQL é citado");
        assert_eq!(conferido("A\"B"), "\"A\"\"B\"", "a aspas dentro do nome se dobra");
    }

    /// O que só um servidor de verdade responde: o `call_timeout` aplicado na
    /// conexão aberta é o prazo do motor. Fica ignorado porque esta máquina não
    /// tem Oracle — medir isto aqui seria fingir.
    #[ignore = "precisa de um Oracle real; sem ele não há conexão em que ler o prazo"]
    #[test]
    fn a_conexao_abre_com_o_prazo_do_motor() {
        let conexao = Oracle::abrir(&conexao_de_teste(Tipo::Oracle)).expect("oracle de verdade");
        assert_eq!(
            conexao.conexao.call_timeout().expect("o driver responde o prazo"),
            Some(PRAZO_DE_LEITURA),
            "defeito 4: a conexão abre sem relógio nenhum"
        );
    }

    /// A flag é a única coisa que muda a string, e ela muda só o protocolo: o
    /// mesmo servidor, a mesma porta de fábrica e o mesmo serviço dos dois
    /// lados. É o que este motor pode medir sem servidor — o `tcps://` é a
    /// resposta; o aperto de mão em cima dele é do `rustls` do driver.
    #[test]
    fn a_marca_troca_so_o_protocolo_da_string() {
        let lisa = conexao_de_teste(Tipo::Oracle);
        assert_eq!(
            string_de_conexao(&lisa),
            "10.0.0.8:1521/estoque",
            "sem a marca continua a forma de sempre"
        );

        let mut conferida = conexao_de_teste(Tipo::Oracle);
        conferida.tls_verificado = true;
        assert_eq!(
            string_de_conexao(&conferida),
            "tcps://10.0.0.8:1521/estoque",
            "com a marca, só o protocolo muda"
        );
    }

    /// Uma porta dita pelo dono vale com ou sem a flag — a marca não empurra a
    /// conexão para a de fábrica.
    #[test]
    fn a_marca_nao_amassa_a_porta_dita() {
        let mut conferida = conexao_de_teste(Tipo::Oracle);
        conferida.porta = Some(2484);
        conferida.tls_verificado = true;
        assert_eq!(string_de_conexao(&conferida), "tcps://10.0.0.8:2484/estoque");
    }
}

