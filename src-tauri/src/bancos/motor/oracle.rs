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

use super::{Comparacao, Consulta, Cronometro, Filtro, Motor, ResultadoSql};
use crate::bancos::config::Conexao;
use crate::idiomas::tr;

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

impl Oracle {
    pub fn abrir(ficha: &Conexao) -> Result<Self> {
        let usuario = ficha.usuario.trim().to_string();
        if usuario.is_empty() {
            bail!(tr("erros.bancoUsuarioFaltando", &[]));
        }
        if ficha.banco.trim().is_empty() {
            bail!(tr("erros.bancoNomeBancoFaltando", &[]));
        }
        // Easy Connect: host:porta/serviço. O driver fala TNS por baixo; a
        // string é a forma que o protocolo aceita sem tnsnames.ora.
        let string_conexao =
            format!("{}:{}/{}", ficha.host.trim(), ficha.porta_efetiva(), ficha.banco.trim());
        let config = ConfigOracle::default()
            .set_connect_string(&string_conexao)
            .map_err(erro)?
            .set_credentials(&usuario, &ficha.senha()?);
        let conexao = oracledb::connect(config).map_err(erro)?;
        Ok(Self { conexao })
    }

    /// O nome entre aspas duplas, depois de confirmado na lista que veio do
    /// banco. O que não está na lista não é interpolado.
    fn conferir_fonte<'a>(&self, fonte: &str, conhecidas: &'a [String]) -> Result<String> {
        conhecidas
            .iter()
            .find(|nome| nome.eq_ignore_ascii_case(fonte))
            .map(|nome| format!("\"{}\"", nome.replace('"', "\"\"")))
            .ok_or_else(|| anyhow!(tr("erros.bancoFonteInexistente", &[("fonte", fonte)])))
    }

    fn conferir_coluna(coluna: &str, conhecidas: &[(String, String)]) -> Result<String> {
        conhecidas
            .iter()
            .find(|(nome, _)| nome.eq_ignore_ascii_case(coluna))
            .map(|(nome, _)| format!("\"{}\"", nome.replace('"', "\"\"")))
            .ok_or_else(|| anyhow!(tr("erros.bancoColunaInexistente", &[("coluna", coluna)])))
    }

    fn colunas_interno(&self, fonte: &str) -> Result<Vec<(String, String)>> {
        let conferida = fonte.replace('"', "\"\"");
        let refs: Vec<&dyn ToDbValue> = vec![&conferida];
        let cursor = self
            .conexao
            .query(
                "SELECT column_name, data_type FROM user_tab_columns WHERE table_name = :1 ORDER BY column_id",
                &refs,
            )
            .map_err(erro)?;
        let mut colunas = Vec::new();
        for linha in cursor {
            let linha = linha.map_err(erro)?;
            let nome = linha.get::<Option<String>>(0).map_err(erro)?.unwrap_or_default();
            let tipo = linha.get::<Option<String>>(1).map_err(erro)?.unwrap_or_default();
            colunas.push((nome, tipo));
        }
        Ok(colunas)
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
        while linhas.len() < limite.saturating_add(1) {
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
        let truncado = linhas.len() > limite;
        if truncado {
            linhas.truncate(limite);
        }
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
        let conhecidas = self.fontes()?;
        let fonte = self.conferir_fonte(fonte, &conhecidas)?;
        self.colunas_interno(&fonte)
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
        let conhecidas = self.fontes()?;
        let fonte = self.conferir_fonte(fonte, &conhecidas)?;
        let schema = self.colunas_interno(&fonte)?;
        let colunas_sql = if colunas_pedidas.is_empty() {
            "*".to_string()
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
            Some((coluna, desc)) => {
                let coluna = Self::conferir_coluna(coluna, &schema)?;
                format!(" ORDER BY {coluna} {}", if desc { "DESC" } else { "ASC" })
            }
        };
        // Uma linha a mais do que o teto: é ela que diz que o corte foi por
        // limite, e não que o banco acabou ali.
        let sql = format!(
            "SELECT * FROM (SELECT {colunas_sql} FROM {fonte}{onde}{ordenar}) WHERE ROWNUM <= {}",
            limite.saturating_add(1)
        );
        let refs: Vec<&dyn ToDbValue> =
            params.iter().map(|p| p as &dyn ToDbValue).collect::<Vec<_>>();
        let cursor = self.conexao.query(&sql, &refs).map_err(erro)?;
        Self::ler_cursor(cursor, limite, &cronometro)
    }

    fn contar(&mut self, fonte: &str, filtro: &Filtro) -> Result<u64> {
        let conhecidas = self.fontes()?;
        let fonte = self.conferir_fonte(fonte, &conhecidas)?;
        let schema = self.colunas_interno(&fonte)?;
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
        let conhecidas = self.fontes()?;
        let fonte = self.conferir_fonte(fonte, &conhecidas)?;
        let schema = self.colunas_interno(&fonte)?;
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
        let conhecidas = self.fontes()?;
        let fonte = self.conferir_fonte(fonte, &conhecidas)?;
        let schema = self.colunas_interno(&fonte)?;
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
        let conhecidas = self.fontes()?;
        let fonte = self.conferir_fonte(fonte, &conhecidas)?;
        let schema = self.colunas_interno(&fonte)?;
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

