//! O motor SQLite: um arquivo no disco, nada de rede.
//!
//! É o único motor que abre por caminho, e o único que não precisa de senha:
//! quem pode o arquivo pode o banco. O arquivo é aberto em leitura e escrita,
//! mas sem criar: um caminho errado é um erro na hora, não um banco vazio
//! aparecendo por engano.

use anyhow::{Result, anyhow, bail};
use rusqlite::{Connection, OpenFlags, types::ValueRef};
use serde_json::{Map, Value};
use std::time::Duration;

use super::{Comparacao, Consulta, Cronometro, Filtro, Motor, ResultadoSql};
use crate::bancos::config::Conexao;
use crate::idiomas::tr;

const ESPERA_BLOQUEIO: Duration = Duration::from_secs(5);

pub struct Sqlite {
    conexao: Connection,
}

impl Sqlite {
    pub fn abrir(conexao: &Conexao) -> Result<Self> {
        let arquivo = conexao.arquivo.trim();
        if arquivo.is_empty() {
            bail!(tr("erros.bancoArquivoFaltando", &[]));
        }
        // Sem SQLITE_OPEN_CREATE: o conector não inventa banco. Se o arquivo não
        // está lá, o erro é o caminho, não uma base vazia que engana o modelo.
        let conexao = Connection::open_with_flags(
            arquivo,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_URI,
        )
        .map_err(|e| anyhow!(tr("erros.bancoAbrir", &[("detalhe", &e.to_string())])))?;
        // Uma escrita de outro processo não derruba a ferramenta: esperar um
        // pouco é o correto num banco que o usuário também pode estar usando.
        conexao
            .busy_timeout(ESPERA_BLOQUEIO)
            .map_err(|e| anyhow!(tr("erros.bancoAbrir", &[("detalhe", &e.to_string())])))?;
        Ok(Self { conexao })
    }

    /// O nome entre aspas duplas, depois de confirmado na lista que veio do
    /// banco. O que não está na lista não é interpolado.
    fn conferir_fonte(&self, fonte: &str, conhecidas: &[String]) -> Result<String> {
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

    /// O WHERE traduzido do filtro, com as marcas `?` na ordem dos parâmetros.
    fn where_sql(filtro: &Filtro, colunas: &[(String, String)], params: &mut Vec<Value>) -> Result<String> {
        let mut partes = Vec::new();
        for condicao in filtro {
            let coluna = Self::conferir_coluna(&condicao.coluna, colunas)?;
            let Comparacao::Contendo = condicao.comparacao else {
                // Nulo compara com IS NULL / IS NOT NULL, não com operador.
                if condicao.valor.is_null() {
                    match condicao.comparacao {
                        Comparacao::Igual => partes.push(format!("{coluna} IS NULL")),
                        Comparacao::Diferente => partes.push(format!("{coluna} IS NOT NULL")),
                        _ => bail!(tr("erros.filtroNuloSemComparacao", &[])),
                    }
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
                partes.push(format!("{coluna} {operador} ?"));
                params.push(condicao.valor.clone());
                continue;
            };
            // "contendo" é LIKE com o valor escapado e envolto em %…%: o que o
            // modelo escreveu é literal, não padrão que ele improvisa.
            let cru = valor_texto(&condicao.valor)?;
            let escapado = cru
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            partes.push(format!("{coluna} LIKE ? ESCAPE '\\'"));
            params.push(Value::String(format!("%{escapado}%")));
        }
        if partes.is_empty() {
            Ok(String::new())
        } else {
            Ok(format!(" WHERE {}", partes.join(" AND ")))
        }
    }

    fn para_json(valor: ValueRef<'_>) -> Value {
        match valor {
            ValueRef::Null => Value::Null,
            ValueRef::Integer(i) => Value::Number(i.into()),
            ValueRef::Real(f) => serde_json::Number::from_f64(f)
                .map(Value::Number)
                .unwrap_or(Value::Null),
            ValueRef::Text(t) => {
                Value::String(String::from_utf8_lossy(t).into_owned())
            }
            // Binário não é texto: o cartão diz o tamanho real em vez de fingir
            // que os bytes são uma string.
            ValueRef::Blob(b) => Value::String(format!("[blob {} bytes]", b.len())),
        }
    }

    fn nome_coluna(statement: &rusqlite::Statement<'_>, indice: usize) -> String {
        statement.column_name(indice).unwrap_or("").to_string()
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

impl Motor for Sqlite {
    fn fontes(&mut self) -> Result<Vec<String>> {
        let mut statement = self
            .conexao
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .map_err(erro)?;
        let nomes = statement
            .query_map([], |linha| linha.get::<_, String>(0))
            .map_err(erro)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(erro)?;
        Ok(nomes)
    }

    fn colunas(&mut self, fonte: &str) -> Result<Vec<(String, String)>> {
        let conhecidas = self.fontes()?;
        let conferida = self.conferir_fonte(fonte, &conhecidas)?;
        let mut statement = self
            .conexao
            .prepare(&format!("PRAGMA table_info({conferida})"))
            .map_err(erro)?;
        let colunas = statement
            .query_map([], |linha| {
                Ok((
                    linha.get::<_, String>(1)?,
                    linha.get::<_, String>(2).unwrap_or_default(),
                ))
            })
            .map_err(erro)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(erro)?;
        Ok(colunas)
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
        let onde = Self::where_sql(filtro, &schema, &mut params)?;
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
            "SELECT {colunas_sql} FROM {fonte}{onde}{ordenar} LIMIT {}",
            limite.saturating_add(1)
        );
        let mut statement = self.conexao.prepare(&sql).map_err(erro)?;
        let total_colunas = statement.column_count();
        let marcados = params.iter().map(json_para_sqlite).collect::<Vec<_>>();
        let mut linhas = statement
            .query_map(rusqlite::params_from_iter(marcados.iter()), |linha| {
                let mut valores = Vec::with_capacity(total_colunas);
                for indice in 0..total_colunas {
                    valores.push(Self::para_json(linha.get_ref(indice)?));
                }
                Ok(valores)
            })
            .map_err(erro)?
            .take(limite.saturating_add(1))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(erro)?;
        let truncado = linhas.len() > limite;
        if truncado {
            linhas.truncate(limite);
        }
        let colunas = (0..total_colunas)
            .map(|i| Self::nome_coluna(&statement, i))
            .collect();
        drop(statement);
        Ok(Consulta {
            colunas,
            linhas,
            truncado,
            duracao_ms: cronometro.ms(),
        })
    }

    fn contar(&mut self, fonte: &str, filtro: &Filtro) -> Result<u64> {
        let conhecidas = self.fontes()?;
        let fonte = self.conferir_fonte(fonte, &conhecidas)?;
        let schema = self.colunas_interno(&fonte)?;
        let mut params = Vec::new();
        let onde = Self::where_sql(filtro, &schema, &mut params)?;
        let sql = format!("SELECT COUNT(*) FROM {fonte}{onde}");
        let marcados = params.iter().map(json_para_sqlite).collect::<Vec<_>>();
        let contagem: i64 = self
            .conexao
            .query_row(&sql, rusqlite::params_from_iter(marcados.iter()), |linha| {
                linha.get(0)
            })
            .map_err(erro)?;
        Ok(contagem.max(0) as u64)
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
            marcas.push("?".to_string());
            params.push(valor.clone());
        }
        let marcados = params.iter().map(json_para_sqlite).collect::<Vec<_>>();
        let sql = format!(
            "INSERT INTO {fonte} ({}) VALUES ({})",
            colunas.join(", "),
            marcas.join(", ")
        );
        let afetadas = self
            .conexao
            .execute(&sql, rusqlite::params_from_iter(marcados.iter()))
            .map_err(erro)?;
        Ok(afetadas as u64)
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
        let mut atribuicoes = Vec::new();
        let mut params = Vec::new();
        for (coluna, valor) in valores {
            atribuicoes.push(format!(
                "{} = ?",
                Self::conferir_coluna(coluna, &schema)?
            ));
            params.push(valor.clone());
        }
        let mut onde_params = Vec::new();
        let onde = Self::where_sql(filtro, &schema, &mut onde_params)?;
        params.extend(onde_params);
        if filtro.is_empty() {
            bail!(tr("erros.bancoOndeObrigatorio", &[]));
        }
        let marcados = params.iter().map(json_para_sqlite).collect::<Vec<_>>();
        let sql = format!("UPDATE {fonte} SET {}{onde}", atribuicoes.join(", "));
        let afetadas = self
            .conexao
            .execute(&sql, rusqlite::params_from_iter(marcados.iter()))
            .map_err(erro)?;
        Ok(afetadas as u64)
    }

    fn apagar(&mut self, fonte: &str, filtro: &Filtro) -> Result<u64> {
        let conhecidas = self.fontes()?;
        let fonte = self.conferir_fonte(fonte, &conhecidas)?;
        let schema = self.colunas_interno(&fonte)?;
        if filtro.is_empty() {
            bail!(tr("erros.bancoOndeObrigatorio", &[]));
        }
        let mut params = Vec::new();
        let onde = Self::where_sql(filtro, &schema, &mut params)?;
        let marcados = params.iter().map(json_para_sqlite).collect::<Vec<_>>();
        let sql = format!("DELETE FROM {fonte}{onde}");
        let afetadas = self
            .conexao
            .execute(&sql, rusqlite::params_from_iter(marcados.iter()))
            .map_err(erro)?;
        Ok(afetadas as u64)
    }

    fn sql(&mut self, declaracao: &str, limite: usize) -> Result<ResultadoSql> {
        let cronometro = Cronometro::agora();
        let mut statement = self.conexao.prepare(declaracao).map_err(erro)?;
        // O banco mesmo classifica: uma declaração que devolve colunas é
        // consulta; as outras, escrita. Nenhuma lista de palavras no meio.
        if statement.column_count() == 0 {
            let afetadas = statement.execute([]).map_err(erro)?;
            return Ok(ResultadoSql::Afetadas(afetadas as u64));
        }
        let total_colunas = statement.column_count();
        let mut linhas = Vec::new();
        {
            let mut registros = statement.query([]).map_err(erro)?;
            while let Some(linha) = registros.next().map_err(erro)? {
                if linhas.len() >= limite.saturating_add(1) {
                    break;
                }
                let mut valores = Vec::with_capacity(total_colunas);
                for indice in 0..total_colunas {
                    let cru = linha.get_ref(indice).map_err(erro)?;
                    valores.push(Self::para_json(cru));
                }
                linhas.push(valores);
            }
        }
        // A consulta livre não sabe quantas linhas o banco ainda tinha: o que
        // se pode dizer é que a leitura parou no teto.
        let truncado = linhas.len() > limite;
        if truncado {
            linhas.truncate(limite);
        }
        let colunas = (0..total_colunas)
            .map(|i| Self::nome_coluna(&statement, i))
            .collect();
        drop(statement);
        Ok(ResultadoSql::Consulta(Consulta {
            colunas,
            linhas,
            truncado,
            duracao_ms: cronometro.ms(),
        }))
    }
}

impl Sqlite {
    fn colunas_interno(&self, fonte_conferida: &str) -> Result<Vec<(String, String)>> {
        let mut statement = self
            .conexao
            .prepare(&format!("PRAGMA table_info({fonte_conferida})"))
            .map_err(erro)?;
        statement
            .query_map([], |linha| {
                Ok((
                    linha.get::<_, String>(1)?,
                    linha.get::<_, String>(2).unwrap_or_default(),
                ))
            })
            .map_err(erro)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(erro)
    }
}

fn erro(e: rusqlite::Error) -> anyhow::Error {
    anyhow!(tr("erros.bancoErro", &[("detalhe", &e.to_string())]))
}

fn json_para_sqlite(valor: &Value) -> rusqlite::types::Value {
    match valor {
        Value::Null => rusqlite::types::Value::Null,
        Value::Bool(b) => rusqlite::types::Value::Integer(*b as i64),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                rusqlite::types::Value::Integer(i)
            } else {
                rusqlite::types::Value::Real(n.as_f64().unwrap_or(0.0))
            }
        }
        Value::String(texto) => rusqlite::types::Value::Text(texto.clone()),
        // Objetos e listas entram como texto JSON: é o que a coluna JSON1 do
        // SQLite entende, e o que uma coluna de texto aceita sem inventar.
        outro => rusqlite::types::Value::Text(outro.to_string()),
    }
}
