//! O motor PostgreSQL: protocolo binário nativo, sem cliente nativo instalado.
//!
//! Toda comparação e todo valor passado em SQL chega aqui como parâmetro
//! textual (`$n::text`) e é convertido para o tipo que a coluna declara — o
//! servidor nunca recebe texto do modelo no lugar de um identificador: nomes
//! de tabela e coluna são conferidos contra o esquema descoberto do banco
//! antes de entrar entre aspas. O driver é assíncrono, então cada conexão
//! traz seu próprio runtime de uma thread: a ferramenta roda bloqueante e
//! episódica, e o runtime morre com ela.

use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value};
use tokio_postgres::NoTls;
use tokio_postgres::types::Type;

use super::{Comparacao, Consulta, Cronometro, Filtro, Motor, ResultadoSql};
use crate::bancos::config::Conexao;
use crate::idiomas::tr;

pub struct Postgres {
    runtime: tokio::runtime::Runtime,
    cliente: tokio_postgres::Client,
}

/// Como ler uma coluna do resultado. `TextoConvertido` é coluna que chega com
/// `::text` no SELECT (numérico exato, temporal, uuid, bytea, arrays,
/// tipos de domínio) — o que o banco declara como texto é texto.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Celula {
    Inteiro16,
    Inteiro32,
    Inteiro64,
    Real32,
    Real64,
    Booleana,
    Texto,
    Jsonb,
    TextoConvertido,
}

fn celula_de_tipo(tipo: &str) -> Celula {
    match tipo {
        "smallint" => Celula::Inteiro16,
        "integer" => Celula::Inteiro32,
        "bigint" => Celula::Inteiro64,
        "real" => Celula::Real32,
        "double precision" => Celula::Real64,
        "boolean" => Celula::Booleana,
        "text" | "character varying" => Celula::Texto,
        "jsonb" => Celula::Jsonb,
        _ => Celula::TextoConvertido,
    }
}

fn celula_de_oid(tipo: &Type) -> Celula {
    match *tipo {
        Type::INT2 => Celula::Inteiro16,
        Type::INT4 => Celula::Inteiro32,
        Type::INT8 => Celula::Inteiro64,
        Type::FLOAT4 => Celula::Real32,
        Type::FLOAT8 => Celula::Real64,
        Type::BOOL => Celula::Booleana,
        Type::TEXT | Type::VARCHAR => Celula::Texto,
        Type::JSONB => Celula::Jsonb,
        _ => Celula::TextoConvertido,
    }
}

/// O alvo do `CAST` para um valor textual chegar com o tipo da coluna. Os
/// tipos temporais e JSON têm alvo próprio em `marca_para`. `None` é coluna de
/// texto: o parâmetro entra como texto puro, sem conversão.
fn alvo_de(tipo: &str) -> Option<&'static str> {
    match tipo {
        "smallint" | "integer" | "bigint" => Some("bigint"),
        "real" | "double precision" | "numeric" | "money" => Some("float8"),
        "boolean" => Some("boolean"),
        "uuid" => Some("uuid"),
        _ => None,
    }
}

/// A expressão que carrega o valor textual na conversão certa. `bytea` é
/// especial: texto → hex decodificado, não `CAST`.
fn marca_para(tipo: &str, marca: &str) -> String {
    match tipo {
        "bytea" => format!("decode({marca}::text, 'hex')"),
        "json" => format!("CAST({marca}::text AS json)"),
        "jsonb" => format!("CAST({marca}::text AS jsonb)"),
        "date" => format!("CAST({marca}::text AS date)"),
        "timestamp without time zone" => format!("CAST({marca}::text AS timestamp)"),
        "timestamp with time zone" => format!("CAST({marca}::text AS timestamptz)"),
        "time without time zone" => format!("CAST({marca}::text AS time)"),
        "time with time zone" => format!("CAST({marca}::text AS timetz)"),
        "interval" => format!("CAST({marca}::text AS interval)"),
        outro => match alvo_de(outro) {
            Some(alvo) => format!("CAST({marca}::text AS {alvo})"),
            None => format!("{marca}::text"),
        },
    }
}

fn citado(nome: &str) -> String {
    format!("\"{}\"", nome.replace('"', "\"\""))
}

fn conferir_fonte(fonte: &str, conhecidas: &[String]) -> Result<String> {
    conhecidas
        .iter()
        .find(|nome| nome.eq_ignore_ascii_case(fonte))
        .cloned()
        .ok_or_else(|| anyhow!(tr("erros.bancoFonteInexistente", &[("fonte", fonte)])))
}

fn conferir_coluna(coluna: &str, conhecidas: &[(String, String)]) -> Result<(String, String)> {
    conhecidas
        .iter()
        .find(|(nome, _)| nome.eq_ignore_ascii_case(coluna))
        .cloned()
        .ok_or_else(|| anyhow!(tr("erros.bancoColunaInexistente", &[("coluna", coluna)])))
}

/// Todo parâmetro sai como texto do JSON: número vira "5", booleano vira
/// "true", objeto JSON vira o próprio texto — é o `::text` que o CAST espera.
fn valor_para_texto(valor: &Value) -> Result<String> {
    match valor {
        Value::String(texto) => Ok(texto.clone()),
        Value::Number(numero) => Ok(numero.to_string()),
        Value::Bool(b) => Ok(if *b { "true".into() } else { "false".into() }),
        Value::Object(_) | Value::Array(_) => Ok(valor.to_string()),
        Value::Null => bail!(tr("erros.filtroValorInvalido", &[])),
    }
}

impl Postgres {
    pub fn abrir(conexao: &Conexao) -> Result<Self> {
        let senha = conexao.senha()?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| anyhow!(tr("erros.bancoAbrir", &[("detalhe", &e.to_string())])))?;
        let (cliente, driver) = runtime
            .block_on(async {
                let mut ficha = tokio_postgres::Config::new();
                ficha.host(conexao.host.trim());
                ficha.port(conexao.porta_efetiva());
                ficha.user(conexao.usuario.trim());
                ficha.dbname(conexao.banco.trim());
                if !senha.is_empty() {
                    ficha.password(senha.as_str());
                }
                ficha.connect(NoTls).await
            })
            .map_err(erro_abrir)?;
        // O driver da conexão é uma tarefa à parte: sem ele rodando, nenhuma
        // consulta é respondida. Morre junto com o runtime, quando o motor é
        // descartado no fim da chamada.
        let _driver = runtime.spawn(driver);
        Ok(Self { runtime, cliente })
    }

    /// As colunas de uma fonte já conferida — sem passar de novo pela lista de
    /// fontes, porque quem chama já conferiu.
    fn colunas_conferidas(&self, conferida: &str) -> Result<Vec<(String, String)>> {
        let linhas = self
            .runtime
            .block_on(self.cliente.query(
                "SELECT column_name::text, data_type::text FROM information_schema.columns \
                 WHERE table_schema = current_schema() AND table_name = $1 \
                 ORDER BY ordinal_position",
                &[&conferida],
            ))
            .map_err(erro)?;
        Ok(linhas
            .iter()
            .map(|linha| {
                Ok((
                    linha.try_get::<_, Option<String>>(0).map_err(erro)?.unwrap_or_default(),
                    linha.try_get::<_, Option<String>>(1).map_err(erro)?.unwrap_or_default(),
                ))
            })
            .collect::<Result<Vec<_>>>()?)
    }

    /// O SELECT de consulta: colunas nativas de pé, as demais com `::text` —
    /// o que volta já é do tipo que a leitura espera, sem conversão às cegas.
    fn selecao(celulas: &[(String, Celula)]) -> String {
        celulas
            .iter()
            .map(|(nome, celula)| {
                if *celula == Celula::TextoConvertido {
                    format!("{nome}::text")
                } else {
                    nome.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn ler(linha: &tokio_postgres::Row, indice: usize, celula: Celula) -> Result<Value> {
        match celula {
            Celula::Inteiro16 => Ok(linha
                .try_get::<_, Option<i16>>(indice)
                .map_err(erro)?
                .map(|v| Value::Number(v.into()))
                .unwrap_or(Value::Null)),
            Celula::Inteiro32 => Ok(linha
                .try_get::<_, Option<i32>>(indice)
                .map_err(erro)?
                .map(|v| Value::Number(v.into()))
                .unwrap_or(Value::Null)),
            Celula::Inteiro64 => Ok(linha
                .try_get::<_, Option<i64>>(indice)
                .map_err(erro)?
                .map(|v| Value::Number(v.into()))
                .unwrap_or(Value::Null)),
            Celula::Real32 => Ok(linha
                .try_get::<_, Option<f32>>(indice)
                .map_err(erro)?
                .and_then(|v| serde_json::Number::from_f64(v as f64).map(Value::Number))
                .unwrap_or(Value::Null)),
            Celula::Real64 => Ok(linha
                .try_get::<_, Option<f64>>(indice)
                .map_err(erro)?
                .and_then(|v| serde_json::Number::from_f64(v).map(Value::Number))
                .unwrap_or(Value::Null)),
            Celula::Booleana => Ok(linha
                .try_get::<_, Option<bool>>(indice)
                .map_err(erro)?
                .map(Value::Bool)
                .unwrap_or(Value::Null)),
            Celula::Texto | Celula::TextoConvertido => Ok(linha
                .try_get::<_, Option<String>>(indice)
                .map_err(erro)?
                .map(Value::String)
                .unwrap_or(Value::Null)),
            Celula::Jsonb => Ok(linha
                .try_get::<_, Option<Value>>(indice)
                .map_err(erro)?
                .unwrap_or(Value::Null)),
        }
    }
}

fn proxima(numero: &mut usize) -> String {
    let marca = format!("${numero}");
    *numero += 1;
    marca
}

/// O WHERE em SQL, com os parâmetros textuais na ordem. `numero` continua de
/// onde parou, porque UPDATE soma os valores antes das condições.
fn onde_sql(
    filtro: &Filtro,
    colunas: &[(String, String)],
    params: &mut Vec<String>,
    numero: &mut usize,
) -> Result<String> {
    let mut partes = Vec::new();
    for condicao in filtro {
        let (nome, tipo) = conferir_coluna(&condicao.coluna, colunas)?;
        let citada = citado(&nome);
        // Nulo compara com IS NULL / IS NOT NULL, não com operador.
        if condicao.valor.is_null() {
            match condicao.comparacao {
                Comparacao::Igual => partes.push(format!("{citada} IS NULL")),
                Comparacao::Diferente => partes.push(format!("{citada} IS NOT NULL")),
                _ => bail!(tr("erros.filtroNuloSemComparacao", &[])),
            }
            continue;
        }
        if matches!(condicao.comparacao, Comparacao::Contendo) {
            // "contendo" é LIKE com o valor escapado: o que o modelo escreveu
            // é literal, não padrão que ele improvisa.
            let cru = valor_para_texto(&condicao.valor)?;
            let escapado = cru
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            let marca = proxima(numero);
            params.push(format!("%{escapado}%"));
            partes.push(format!("{citada}::text LIKE {marca}"));
            continue;
        }
        let operador = match condicao.comparacao {
            Comparacao::Igual => "=",
            Comparacao::Diferente => "<>",
            Comparacao::Maior => ">",
            Comparacao::MaiorIgual => ">=",
            Comparacao::Menor => "<",
            Comparacao::MenorIgual => "<=",
            Comparacao::Contendo => unreachable!("conferido acima"),
        };
        let marca = proxima(numero);
        params.push(valor_para_texto(&condicao.valor)?);
        partes.push(format!("{citada} {operador} {}", marca_para(&tipo, &marca)));
    }
    if partes.is_empty() {
        Ok(String::new())
    } else {
        Ok(format!(" WHERE {}", partes.join(" AND ")))
    }
}

impl Motor for Postgres {
    fn fontes(&mut self) -> Result<Vec<String>> {
        let linhas = self
            .runtime
            .block_on(self.cliente.query(
                "SELECT table_name::text FROM information_schema.tables \
                 WHERE table_schema = current_schema() AND table_type = 'BASE TABLE' \
                 ORDER BY table_name",
                &[],
            ))
            .map_err(erro)?;
        Ok(linhas
            .iter()
            .map(|linha| {
                Ok(linha
                    .try_get::<_, Option<String>>(0)
                    .map_err(erro)?
                    .unwrap_or_default())
            })
            .collect::<Result<Vec<_>>>()?)
    }

    fn colunas(&mut self, fonte: &str) -> Result<Vec<(String, String)>> {
        let conhecidas = self.fontes()?;
        let conferida = conferir_fonte(fonte, &conhecidas)?;
        self.colunas_conferidas(&conferida)
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
        let conferida = conferir_fonte(fonte, &conhecidas)?;
        let esquema = self.colunas_conferidas(&conferida)?;
        let pedidas = if colunas_pedidas.is_empty() {
            esquema
                .iter()
                .map(|(nome, _)| nome.clone())
                .collect::<Vec<_>>()
        } else {
            colunas_pedidas.to_vec()
        };
        let celulas = pedidas
            .iter()
            .map(|pedida| {
                let (nome, tipo) = conferir_coluna(pedida, &esquema)?;
                let celula = celula_de_tipo(&tipo);
                Ok((citado(&nome), celula))
            })
            .collect::<Result<Vec<_>>>()?;
        let mut params = Vec::new();
        let mut numero = 1usize;
        let onde = onde_sql(filtro, &esquema, &mut params, &mut numero)?;
        let ordenar = match ordem {
            None => String::new(),
            Some((coluna, desc)) => {
                let (nome, _) = conferir_coluna(coluna, &esquema)?;
                format!(
                    " ORDER BY {} {}",
                    citado(&nome),
                    if desc { "DESC" } else { "ASC" }
                )
            }
        };
        // Uma linha a mais do que o teto: é ela que diz que o corte foi por
        // limite, e não que o banco acabou ali.
        let sql = format!(
            "SELECT {} FROM {}{}{} LIMIT {}",
            Self::selecao(&celulas),
            citado(&conferida),
            onde,
            ordenar,
            limite.saturating_add(1)
        );
        let referencias = params.iter().map(|p| p as &(dyn tokio_postgres::types::ToSql + Sync)).collect::<Vec<_>>();
        let linhas = self
            .runtime
            .block_on(self.cliente.query(sql.as_str(), &referencias))
            .map_err(erro)?;
        let mut saida = Vec::new();
        for linha in &linhas {
            let mut valores = Vec::with_capacity(celulas.len());
            for (indice, (_, celula)) in celulas.iter().enumerate() {
                valores.push(Self::ler(linha, indice, *celula)?);
            }
            saida.push(valores);
        }
        let truncado = saida.len() > limite;
        if truncado {
            saida.truncate(limite);
        }
        Ok(Consulta {
            colunas: pedidas,
            linhas: saida,
            truncado,
            duracao_ms: cronometro.ms(),
        })
    }

    fn contar(&mut self, fonte: &str, filtro: &Filtro) -> Result<u64> {
        let conhecidas = self.fontes()?;
        let conferida = conferir_fonte(fonte, &conhecidas)?;
        let esquema = self.colunas_conferidas(&conferida)?;
        let mut params = Vec::new();
        let mut numero = 1usize;
        let onde = onde_sql(filtro, &esquema, &mut params, &mut numero)?;
        let sql = format!("SELECT COUNT(*) FROM {}{}", citado(&conferida), onde);
        let referencias = params.iter().map(|p| p as &(dyn tokio_postgres::types::ToSql + Sync)).collect::<Vec<_>>();
        let linha = self
            .runtime
            .block_on(self.cliente.query_one(sql.as_str(), &referencias))
            .map_err(erro)?;
        Ok(linha.try_get::<_, i64>(0).map_err(erro)?.max(0) as u64)
    }

    fn inserir(&mut self, fonte: &str, valores: &Map<String, Value>) -> Result<u64> {
        let conhecidas = self.fontes()?;
        let conferida = conferir_fonte(fonte, &conhecidas)?;
        let esquema = self.colunas_conferidas(&conferida)?;
        if valores.is_empty() {
            bail!(tr("erros.bancoValoresVazios", &[]));
        }
        let mut colunas_sql = Vec::new();
        let mut valores_sql = Vec::new();
        let mut params = Vec::new();
        let mut numero = 1usize;
        for (coluna, valor) in valores {
            let (nome, tipo) = conferir_coluna(coluna, &esquema)?;
            colunas_sql.push(citado(&nome));
            if valor.is_null() {
                valores_sql.push("NULL".to_string());
            } else {
                let marca = proxima(&mut numero);
                params.push(valor_para_texto(valor)?);
                valores_sql.push(marca_para(&tipo, &marca));
            }
        }
        let sql = format!(
            "INSERT INTO {} ({}) VALUES ({})",
            citado(&conferida),
            colunas_sql.join(", "),
            valores_sql.join(", ")
        );
        let referencias = params.iter().map(|p| p as &(dyn tokio_postgres::types::ToSql + Sync)).collect::<Vec<_>>();
        self.runtime
            .block_on(self.cliente.execute(sql.as_str(), &referencias))
            .map(|afetadas| afetadas)
            .map_err(erro)
    }

    fn atualizar(
        &mut self,
        fonte: &str,
        valores: &Map<String, Value>,
        filtro: &Filtro,
    ) -> Result<u64> {
        let conhecidas = self.fontes()?;
        let conferida = conferir_fonte(fonte, &conhecidas)?;
        let esquema = self.colunas_conferidas(&conferida)?;
        if valores.is_empty() {
            bail!(tr("erros.bancoValoresVazios", &[]));
        }
        if filtro.is_empty() {
            bail!(tr("erros.bancoOndeObrigatorio", &[]));
        }
        let mut atribuicoes = Vec::new();
        let mut params = Vec::new();
        let mut numero = 1usize;
        for (coluna, valor) in valores {
            let (nome, tipo) = conferir_coluna(coluna, &esquema)?;
            if valor.is_null() {
                atribuicoes.push(format!("{} = NULL", citado(&nome)));
            } else {
                let marca = proxima(&mut numero);
                params.push(valor_para_texto(valor)?);
                atribuicoes.push(format!("{} = {}", citado(&nome), marca_para(&tipo, &marca)));
            }
        }
        let onde = onde_sql(filtro, &esquema, &mut params, &mut numero)?;
        let sql = format!(
            "UPDATE {} SET {}{}",
            citado(&conferida),
            atribuicoes.join(", "),
            onde
        );
        let referencias = params.iter().map(|p| p as &(dyn tokio_postgres::types::ToSql + Sync)).collect::<Vec<_>>();
        self.runtime
            .block_on(self.cliente.execute(sql.as_str(), &referencias))
            .map_err(erro)
    }

    fn apagar(&mut self, fonte: &str, filtro: &Filtro) -> Result<u64> {
        let conhecidas = self.fontes()?;
        let conferida = conferir_fonte(fonte, &conhecidas)?;
        let esquema = self.colunas_conferidas(&conferida)?;
        if filtro.is_empty() {
            bail!(tr("erros.bancoOndeObrigatorio", &[]));
        }
        let mut params = Vec::new();
        let mut numero = 1usize;
        let onde = onde_sql(filtro, &esquema, &mut params, &mut numero)?;
        let sql = format!("DELETE FROM {}{}", citado(&conferida), onde);
        let referencias = params.iter().map(|p| p as &(dyn tokio_postgres::types::ToSql + Sync)).collect::<Vec<_>>();
        self.runtime
            .block_on(self.cliente.execute(sql.as_str(), &referencias))
            .map_err(erro)
    }

    fn sql(&mut self, declaracao: &str, limite: usize) -> Result<ResultadoSql> {
        let cronometro = Cronometro::agora();
        let declaracao = declaracao.trim();
        let declaracao = declaracao.strip_suffix(';').unwrap_or(declaracao).trim();
        if declaracao.is_empty() {
            bail!(tr("erros.bancoSqlVazio", &[]));
        }
        let preparada = self
            .runtime
            .block_on(self.cliente.prepare(declaracao))
            .map_err(erro)?;
        // O banco mesmo classifica: uma declaração que devolve colunas é
        // consulta; as outras, escrita. Nenhuma lista de palavras no meio.
        if preparada.columns().is_empty() {
            let afetadas = self
                .runtime
                .block_on(self.cliente.execute(&preparada, &[]))
                .map_err(erro)?;
            return Ok(ResultadoSql::Afetadas(afetadas));
        }
        // A consulta volta embrulhada: colunas nativas de pé, as demais com
        // `::text` — uma única execução, os nomes preservados.
        let celulas = preparada
            .columns()
            .iter()
            .map(|coluna| (citado(coluna.name()), celula_de_oid(coluna.type_())))
            .collect::<Vec<_>>();
        let sql = format!(
            "SELECT {} FROM ({declaracao}) AS \"__aether_consulta__\" LIMIT {}",
            Self::selecao(&celulas),
            limite.saturating_add(1)
        );
        let linhas = self
            .runtime
            .block_on(self.cliente.query(sql.as_str(), &[]))
            .map_err(erro)?;
        let mut saida = Vec::new();
        for linha in &linhas {
            let mut valores = Vec::with_capacity(celulas.len());
            for (indice, (_, celula)) in celulas.iter().enumerate() {
                valores.push(Self::ler(linha, indice, *celula)?);
            }
            saida.push(valores);
        }
        let truncado = saida.len() > limite;
        if truncado {
            saida.truncate(limite);
        }
        Ok(ResultadoSql::Consulta(Consulta {
            colunas: preparada.columns().iter().map(|c| c.name().to_string()).collect(),
            linhas: saida,
            truncado,
            duracao_ms: cronometro.ms(),
        }))
    }
}

fn erro_abrir(e: tokio_postgres::Error) -> anyhow::Error {
    anyhow!(tr("erros.bancoAbrir", &[("detalhe", &e.to_string())]))
}

fn erro(e: tokio_postgres::Error) -> anyhow::Error {
    anyhow!(tr("erros.bancoErro", &[("detalhe", &e.to_string())]))
}
