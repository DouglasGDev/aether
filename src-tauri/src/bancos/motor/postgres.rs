//! O motor PostgreSQL: protocolo binário nativo, sem cliente nativo instalado.
//!
//! Toda comparação e todo valor passado em SQL chega aqui como parâmetro
//! textual (`$n::text`) e é convertido para o tipo que a coluna declara — o
//! servidor nunca recebe texto do modelo no lugar de um identificador: nomes
//! de tabela e coluna são conferidos contra o esquema descoberto do banco
//! antes de entrar entre aspas. O driver é assíncrono: a conexão é da chamada,
//! mas o agendador é do processo — ver `super::agendador`.

use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value};
use tokio_postgres::NoTls;
use tokio_postgres::config::SslMode;
use tokio_postgres::types::Type;

use super::{
    Comparacao, Consulta, Cronometro, Filtro, Motor, ResultadoSql, aparar_no_teto,
    erro_coluna_invalida, erro_de_prazo, ordem_de, prazo_de_espera, prazo_em_ms, teto_de_leitura,
};
use crate::bancos::config::Conexao;
use crate::idiomas::tr;

pub struct Postgres {
    runtime: &'static tokio::runtime::Runtime,
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

fn conferir_coluna(coluna: &str, conhecidas: &[(String, String)]) -> Result<(String, String)> {
    conhecidas
        .iter()
        .find(|(nome, _)| nome.eq_ignore_ascii_case(coluna))
        .cloned()
        .ok_or_else(|| erro_coluna_invalida(coluna, conhecidas))
}

/// A ida única ao catálogo de uma fonte: a tabela conferida e as colunas dela,
/// na ordem e com o nome que o próprio banco grafa. `LOWER(...) = LOWER($n)` é o
/// que fazia a busca insensível da lista, sem trazer a lista inteira.
const ESQUEMA_SQL: &str = "SELECT t.table_name::text, c.column_name::text, c.data_type::text \
     FROM information_schema.tables AS t \
     JOIN information_schema.columns AS c \
       ON c.table_schema = t.table_schema AND c.table_name = t.table_name \
     WHERE t.table_schema = current_schema() AND t.table_type = 'BASE TABLE' \
       AND LOWER(t.table_name) = LOWER($1) \
     ORDER BY c.ordinal_position";

/// As tabelas do esquema atual, para a lista que o dono vê — uma viagem só, e o
/// `bancos_tabelas` não descreve coluna nenhuma delas.
const FONTES_SQL: &str = "SELECT table_name::text FROM information_schema.tables \
     WHERE table_schema = current_schema() AND table_type = 'BASE TABLE' \
     ORDER BY table_name";

fn nome_da(linha: &tokio_postgres::Row, indice: usize) -> Result<String> {
    Ok(linha
        .try_get::<_, Option<String>>(indice)
        .map_err(erro)?
        .unwrap_or_default())
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

/// A ficha do tokio-postgres, inteira, a partir do cadastro. É aqui, e só
/// aqui, que a flag `tls_verificado` é lida neste motor.
///
/// Sem a marca, o modo fica `Prefer` — o de fábrica — e como este build só
/// tem `NoTls`, `Prefer` derruba para texto puro sem avisar: exatamente o
/// comportamento de sempre, preservado de propósito, porque um servidor local
/// com certificado auto-assinado pararia de conectar.
///
/// Com a marca, o modo vira `Require`, o mais forte que este driver conhece.
/// `Require` não aceita cair para texto puro: se o servidor não sobe para TLS,
/// ou sobe e o `NoTls` deste build não sabe fazer o aperto de mão, a ligação
/// falha com erro. Ou seja, a flag nunca é ignorada em silêncio — ela falha
/// fechado. Para o canal ser cifrado *e* o certificado conferido de verdade
/// falta um adaptador TLS no `Cargo.toml` (`tokio-postgres-rustls` ou
/// `postgres-native-tls`); sem ele não há `MakeTlsConnect` neste crate, e
/// inventar um estaria mentindo sobre o que a rede faz.
fn ficha_de(conexao: &Conexao, senha: &str) -> tokio_postgres::Config {
    let mut ficha = tokio_postgres::Config::new();
    ficha.host(conexao.host.trim());
    ficha.port(conexao.porta_efetiva());
    ficha.user(conexao.usuario.trim());
    ficha.dbname(conexao.banco.trim());
    if !senha.is_empty() {
        ficha.password(senha);
    }
    if conexao.tls_verificado {
        ficha.ssl_mode(SslMode::Require);
    }
    // O prazo vai na ficha de abertura, como parâmetro do servidor: uma
    // declaração pendurada custa a conversa inteira, e `statement_timeout` é o
    // que faz o próprio Postgres desistir — sem ida extra ao servidor e sem
    // depender de o cliente saber cancelar.
    ficha.options(format!("-c statement_timeout={}", prazo_em_ms()));
    ficha
}

/// A espera com relógio. O `timeout` nasce DENTRO do `block_on`: o `Sleep` do
/// prazo não se constrói fora de um agendador — não há reactor ali — e montá-lo
/// antes do `block_on` fazia toda chamada paniar na primeira consulta, o que é
/// um jeito novo de nunca ler nada.
fn esperar_prazo<T>(
    runtime: &'static tokio::runtime::Runtime,
    prazo: std::time::Duration,
    futuro: impl std::future::Future<Output = std::result::Result<T, tokio_postgres::Error>>,
) -> Result<T> {
    runtime.block_on(async move {
        tokio::time::timeout(prazo, futuro)
            .await
            .map_err(|_| erro_de_prazo())?
            .map_err(erro)
    })
}

impl Postgres {
    pub fn abrir(conexao: &Conexao) -> Result<Self> {
        let senha = conexao.senha()?;
        let runtime = super::agendador()?;
        let (cliente, driver) = runtime
            .block_on(async { ficha_de(conexao, senha.as_str()).connect(NoTls).await })
            .map_err(erro_abrir)?;
        // O driver da conexão é uma tarefa à parte: sem ela rodando, nenhuma
        // consulta é respondida. Vive no agendador do processo e morre quando a
        // conexão cai, no fim da chamada.
        let _driver = runtime.spawn(driver);
        Ok(Self { runtime, cliente })
    }

    /// A espera de quem já tem `statement_timeout` do lado do servidor: um
    /// segundo a mais, para que o erro que chega primeiro seja o do banco, que
    /// sabe dizer o que expirou. Sem ele, um servidor que não responde ao
    /// cancelamento seguraria a conversa para sempre.
    fn espera<T>(
        &self,
        futuro: impl std::future::Future<Output = std::result::Result<T, tokio_postgres::Error>>,
    ) -> Result<T> {
        esperar_prazo(self.runtime, prazo_de_espera(), futuro)
    }

    /// A única ida ao catálogo que uma leitura precisa: a tabela conferida e as
    /// colunas dela, juntas. Antes eram duas — a lista de tabelas e, para cada
    /// tabela da lista, o que o chamador já podia ter pedido direto.
    fn esquema_de(&mut self, fonte: &str) -> Result<(String, Vec<(String, String)>)> {
        let linhas = self.espera(self.cliente.query(ESQUEMA_SQL, &[&fonte]))?;
        let Some(primeira) = linhas.first() else {
            return Err(anyhow!(tr("erros.bancoFonteInexistente", &[("fonte", fonte)])));
        };
        let conferida = nome_da(primeira, 0)?;
        let mut esquema = Vec::with_capacity(linhas.len());
        for linha in &linhas {
            esquema.push((nome_da(linha, 1)?, nome_da(linha, 2).unwrap_or_default()));
        }
        Ok((conferida, esquema))
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
        let linhas = self.espera(self.cliente.query(FONTES_SQL, &[]))?;
        Ok(linhas.iter().map(|linha| nome_da(linha, 0)).collect::<Result<Vec<_>>>()?)
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
        let (conferida, esquema) = self.esquema_de(fonte)?;
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
            Some((texto, desc)) => {
                let (coluna, desc) = ordem_de(texto, desc, Some(&esquema))?;
                let (nome, _) = conferir_coluna(&coluna, &esquema)?;
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
            teto_de_leitura(limite)
        );
        let referencias = params.iter().map(|p| p as &(dyn tokio_postgres::types::ToSql + Sync)).collect::<Vec<_>>();
        let linhas = self.espera(self.cliente.query(sql.as_str(), &referencias))?;
        let mut saida = Vec::new();
        for linha in &linhas {
            let mut valores = Vec::with_capacity(celulas.len());
            for (indice, (_, celula)) in celulas.iter().enumerate() {
                valores.push(Self::ler(linha, indice, *celula)?);
            }
            saida.push(valores);
        }
        let (linhas, truncado) = aparar_no_teto(saida, limite);
        Ok(Consulta {
            colunas: pedidas,
            linhas,
            truncado,
            duracao_ms: cronometro.ms(),
        })
    }

    fn contar(&mut self, fonte: &str, filtro: &Filtro) -> Result<u64> {
        let (conferida, esquema) = self.esquema_de(fonte)?;
        let mut params = Vec::new();
        let mut numero = 1usize;
        let onde = onde_sql(filtro, &esquema, &mut params, &mut numero)?;
        let sql = format!("SELECT COUNT(*) FROM {}{}", citado(&conferida), onde);
        let referencias = params.iter().map(|p| p as &(dyn tokio_postgres::types::ToSql + Sync)).collect::<Vec<_>>();
        let linha = self.espera(self.cliente.query_one(sql.as_str(), &referencias))?;
        Ok(linha.try_get::<_, i64>(0).map_err(erro)?.max(0) as u64)
    }

    fn inserir(&mut self, fonte: &str, valores: &Map<String, Value>) -> Result<u64> {
        let (conferida, esquema) = self.esquema_de(fonte)?;
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
        self.espera(self.cliente.execute(sql.as_str(), &referencias))
    }

    fn atualizar(
        &mut self,
        fonte: &str,
        valores: &Map<String, Value>,
        filtro: &Filtro,
    ) -> Result<u64> {
        let (conferida, esquema) = self.esquema_de(fonte)?;
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
        self.espera(self.cliente.execute(sql.as_str(), &referencias))
    }

    fn apagar(&mut self, fonte: &str, filtro: &Filtro) -> Result<u64> {
        let (conferida, esquema) = self.esquema_de(fonte)?;
        if filtro.is_empty() {
            bail!(tr("erros.bancoOndeObrigatorio", &[]));
        }
        let mut params = Vec::new();
        let mut numero = 1usize;
        let onde = onde_sql(filtro, &esquema, &mut params, &mut numero)?;
        let sql = format!("DELETE FROM {}{}", citado(&conferida), onde);
        let referencias = params.iter().map(|p| p as &(dyn tokio_postgres::types::ToSql + Sync)).collect::<Vec<_>>();
        self.espera(self.cliente.execute(sql.as_str(), &referencias))
    }

    fn sql(&mut self, declaracao: &str, limite: usize) -> Result<ResultadoSql> {
        let cronometro = Cronometro::agora();
        let declaracao = declaracao.trim();
        let declaracao = declaracao.strip_suffix(';').unwrap_or(declaracao).trim();
        if declaracao.is_empty() {
            bail!(tr("erros.bancoSqlVazio", &[]));
        }
        let preparada = self.espera(self.cliente.prepare(declaracao))?;
        // O banco mesmo classifica: uma declaração que devolve colunas é
        // consulta; as outras, escrita. Nenhuma lista de palavras no meio.
        if preparada.columns().is_empty() {
            let afetadas = self.espera(self.cliente.execute(&preparada, &[]))?;
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
            teto_de_leitura(limite)
        );
        let linhas = self.espera(self.cliente.query(sql.as_str(), &[]))?;
        let mut saida = Vec::new();
        for linha in &linhas {
            let mut valores = Vec::with_capacity(celulas.len());
            for (indice, (_, celula)) in celulas.iter().enumerate() {
                valores.push(Self::ler(linha, indice, *celula)?);
            }
            saida.push(valores);
        }
        let (linhas, truncado) = aparar_no_teto(saida, limite);
        Ok(ResultadoSql::Consulta(Consulta {
            colunas: preparada.columns().iter().map(|c| c.name().to_string()).collect(),
            linhas,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bancos::config::{conexao_de_teste, Tipo};

    /// A escolha do canal sai da flag. O estado da ficha é legível de verdade
    /// (`Config::get_ssl_mode`), então isto mede o que o driver vai fazer, não
    /// uma cópia da decisão.
    #[test]
    fn a_flag_de_tls_escolhe_o_modo_ssl_da_ficha() {
        let lisa = conexao_de_teste(Tipo::Postgres);
        assert_eq!(
            ficha_de(&lisa, "").get_ssl_mode(),
            SslMode::Prefer,
            "sem a marca fica o de fábrica, que com `NoTls` é texto puro como sempre"
        );

        let mut conferida = conexao_de_teste(Tipo::Postgres);
        conferida.tls_verificado = true;
        assert_eq!(
            ficha_de(&conferida, "").get_ssl_mode(),
            SslMode::Require,
            "com a marca, o modo mais forte que este driver conhece: sem queda para texto puro"
        );
    }

    /// Nem a porta, nem o host, nem o nome do banco mudam a postura: só a flag.
    #[test]
    fn o_numero_escrito_a_mao_nao_escolhe_o_canal() {
        let mut outra_porta = conexao_de_teste(Tipo::Postgres);
        outra_porta.porta = Some(5433);
        outra_porta.host = "127.0.0.1".into();
        outra_porta.banco = "outro".into();
        assert_eq!(outra_porta.porta_efetiva(), 5433, "a porta dita continua valendo");
        assert_eq!(ficha_de(&outra_porta, "").get_ssl_mode(), SslMode::Prefer);

        outra_porta.tls_verificado = true;
        assert_eq!(ficha_de(&outra_porta, "").get_ssl_mode(), SslMode::Require);
    }

    /// O endereço e a credencial não dependem da flag: a mesma conexão aponta
    /// para o mesmo lugar com ou sem ela, e a senha só entra quando existe.
    #[test]
    fn a_flag_nao_troca_o_que_a_ficha_aponta() {
        use tokio_postgres::config::Host;
        let lisa = ficha_de(&conexao_de_teste(Tipo::Postgres), "");
        let mut com_flag = conexao_de_teste(Tipo::Postgres);
        com_flag.tls_verificado = true;
        let conferida = ficha_de(&com_flag, "");
        for ficha in [lisa, conferida] {
            assert!(
                matches!(ficha.get_hosts().first(), Some(Host::Tcp(h)) if h == "10.0.0.8"),
                "a flag não escolhe o servidor"
            );
            assert_eq!(ficha.get_ports(), [5432], "a porta de fábrica de cada família");
            assert_eq!(ficha.get_dbname(), Some("estoque"));
        }
    }

    /// O prazo não é uma promessa do cliente: ele vai escrito na ficha de
    /// abertura, como parâmetro do servidor, e é legível de volta.
    #[test]
    fn a_ficha_leva_o_prazo_para_o_servidor() {
        let ficha = ficha_de(&conexao_de_teste(Tipo::Postgres), "");
        assert_eq!(
            ficha.get_options(),
            Some("-c statement_timeout=15000"),
            "defeito 4: a ficha abre sem prazo, e uma declaração pendurada segura a conversa"
        );
    }

    /// O nome da ordem continua sendo conferido no schema real; texto de SQL
    /// não vira coluna, e o erro ajuda o modelo a corrigir a chamada.
    #[test]
    fn coluna_de_ordem_invalida_lista_as_validas_e_o_exemplo() {
        let schema = vec![
            ("id".to_string(), "integer".to_string()),
            ("qt_venda".to_string(), "integer".to_string()),
        ];
        let (nome, _) = conferir_coluna("QT_VENDA", &schema).unwrap();
        assert_eq!(nome, "qt_venda");
        for entrada in [
            "qt_venda DESC",
            "qt_venda, id",
            "qt_venda; DROP TABLE vendas",
        ] {
            let erro = conferir_coluna(entrada, &schema).unwrap_err().to_string();
            for trecho in [
                entrada,
                "id",
                "qt_venda",
                "ordem",
                "ordemDesc",
                "bancos_colunas",
            ] {
                assert!(erro.contains(trecho), "{entrada}: {erro}");
            }
        }
    }

    /// A conferência de fonte e colunas cabe numa consulta ao catálogo: duas
    /// tabelas do information_schema unidas, uma ida. Duas strings separadas é o
    /// que fazia a leitura pagar duas viagens.
    #[test]
    fn o_catalogo_de_uma_fonte_e_uma_ida_s() {
        assert!(ESQUEMA_SQL.contains("information_schema.tables"));
        assert!(ESQUEMA_SQL.contains("information_schema.columns"));
        assert_eq!(
            ESQUEMA_SQL.matches(';').count(),
            0,
            "o catálogo de uma fonte tem de caber numa declaração"
        );
        assert!(
            ESQUEMA_SQL.contains("LOWER(t.table_name) = LOWER($1)"),
            "sem a conferência insensível no servidor, a fonte digitada em outra caixa sumiria"
        );
    }

    /// O relógio da espera se constrói DENTRO do agendador: fora dele não há
    /// reactor nenhum, e a primeira consulta paniava em vez de ler. A tarefa
    /// dorme mais que o prazo e devolve `Ok`, então o único erro possível é o
    /// prazo — e o pânico do reactor aparece como falha, não como susto.
    #[test]
    fn a_espera_nao_panie_fora_do_agendador() {
        let runtime = super::super::agendador().expect("o agendador do processo nasce");
        let resultado = esperar_prazo(
            runtime,
            std::time::Duration::from_millis(20),
            async {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                Ok::<(), tokio_postgres::Error>(())
            },
        );
        match resultado {
            Err(erro) => assert!(
                erro.to_string().contains('1'),
                "a frase do prazo tem de dizer segundos: {erro}"
            ),
            Ok(()) => {
                panic!("defeito 4: a ida voltou depois de passar do prazo — ninguém esperou nada")
            }
        }
    }

    /// O que só um servidor de verdade responde: com a flag ligada contra um
    /// Postgres que não fala TLS, a ligação tem de falhar; contra um que fala,
    /// o `NoTls` deste build ainda assim tem de recusar — falhar fechado, nunca
    /// cair para texto puro. Ignorado porque não há Postgres nesta máquina.
    #[ignore = "precisa de um Postgres real; sem servidor não há aperto de mão para medir, e um falso diria o contrário do que a rede faz"]
    #[test]
    fn com_a_flag_ligada_texto_puro_nunca_e_aceito() {
        let mut conferida = conexao_de_teste(Tipo::Postgres);
        conferida.tls_verificado = true;
        assert!(
            Postgres::abrir(&conferida).is_err(),
            "sem adaptador TLS neste build, a marca tem de barrar a ligação em vez de falar em claro"
        );
    }
}
