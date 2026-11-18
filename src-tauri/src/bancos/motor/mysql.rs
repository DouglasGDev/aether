//! O motor MySQL/MariaDB: protocolo de texto nativo, sem cliente instalado.
//!
//! O MySQL é coerção antes de rigor: uma comparação entre coluna inteira e
//! parâmetro textual converte sozinha, então os valores chegam tipados pelo
//! que o JSON diz (Int, Double, Bytes, NULL) e o banco decide a conversão.
//! Identificadores, ao contrário, não têm coerção — tabela e coluna são
//! conferidos contra `information_schema` antes de entrar entre crases.
//! O driver é assíncrono, então cada conexão traz seu runtime de uma thread,
//! que morre junto com a ferramenta que o criou.

use anyhow::{Result, anyhow, bail};
use mysql_async::prelude::*;
use serde_json::{Map, Value};

use super::{Comparacao, Consulta, Cronometro, Filtro, Motor, ResultadoSql};
use crate::bancos::config::Conexao;
use crate::idiomas::tr;

pub struct Mysql {
    runtime: tokio::runtime::Runtime,
    conexao: mysql_async::Conn,
}

fn citado(nome: &str) -> String {
    format!("`{}`", nome.replace('`', "``"))
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

/// Um valor do banco virando JSON: o protocolo de texto traz bytes, e as datas
/// vêm como texto — nada é convertido às cegas, cada variante diz o que é.
fn para_json(valor: &mysql_async::Value) -> Value {
    match valor {
        mysql_async::Value::NULL => Value::Null,
        mysql_async::Value::Bytes(b) => Value::String(String::from_utf8_lossy(b).into_owned()),
        mysql_async::Value::Int(i) => Value::Number((*i).into()),
        mysql_async::Value::UInt(u) => Value::Number((*u).into()),
        mysql_async::Value::Float(f) => serde_json::Number::from_f64(*f as f64)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        mysql_async::Value::Double(f) => serde_json::Number::from_f64(*f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        mysql_async::Value::Date(ano, mes, dia, hora, minuto, segundo, micros) => Value::String(
            format!("{ano:04}-{mes:02}-{dia:02} {hora:02}:{minuto:02}:{segundo:02}.{micros:06}"),
        ),
        mysql_async::Value::Time(negativo, dias, horas, minutos, segundos, micros) => {
            Value::String(format!(
                "{}{} {:02}:{:02}:{:02}.{:06}",
                if *negativo { "-" } else { "" },
                dias,
                horas,
                minutos,
                segundos,
                micros
            ))
        }
    }
}

/// Um valor do JSON virando parâmetro tipado. Booleano é 0/1: o MySQL não tem
/// tipo booleano, e "true" textual compararia como 0 — falso em toda linha.
fn para_mysql(valor: &Value) -> mysql_async::Value {
    match valor {
        Value::Null => mysql_async::Value::NULL,
        Value::Bool(b) => mysql_async::Value::Int(*b as i64),
        Value::Number(n) => match n.as_i64() {
            Some(i) => mysql_async::Value::Int(i),
            None => mysql_async::Value::Double(n.as_f64().unwrap_or(0.0)),
        },
        Value::String(texto) => mysql_async::Value::Bytes(texto.clone().into_bytes()),
        // Objetos e listas entram como texto JSON: é o que uma coluna JSON
        // aceita, e o que o banco recusa honestamente quando não aceita.
        outro => mysql_async::Value::Bytes(outro.to_string().into_bytes()),
    }
}

fn valor_para_texto(valor: &Value) -> Result<String> {
    match valor {
        Value::String(texto) => Ok(texto.clone()),
        Value::Number(numero) => Ok(numero.to_string()),
        Value::Bool(b) => Ok(if *b { "true".into() } else { "false".into() }),
        Value::Object(_) | Value::Array(_) => Ok(valor.to_string()),
        Value::Null => bail!(tr("erros.filtroValorInvalido", &[])),
    }
}

fn params_posicionais(params: &[mysql_async::Value]) -> mysql_async::Params {
    if params.is_empty() {
        mysql_async::Params::Empty
    } else {
        mysql_async::Params::Positional(params.to_vec())
    }
}

fn texto_de(linha: &mysql_async::Row, indice: usize) -> Option<String> {
    match linha.as_ref(indice) {
        Some(mysql_async::Value::Bytes(b)) => Some(String::from_utf8_lossy(b).into_owned()),
        _ => None,
    }
}

fn contagem_de(linha: &mysql_async::Row) -> u64 {
    match linha.as_ref(0) {
        Some(mysql_async::Value::Int(i)) => (*i).max(0) as u64,
        Some(mysql_async::Value::UInt(u)) => *u,
        _ => 0,
    }
}

/// O WHERE em SQL, com marcas `?` e os parâmetros na ordem.
fn onde_sql(
    filtro: &Filtro,
    colunas: &[(String, String)],
    params: &mut Vec<mysql_async::Value>,
) -> Result<String> {
    let mut partes = Vec::new();
    for condicao in filtro {
        let (nome, _) = conferir_coluna(&condicao.coluna, colunas)?;
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
            // é literal, não padrão que ele improvisa. A barra invertida é o
            // escape padrão do LIKE no MySQL, então não há cláusula ESCAPE.
            let cru = valor_para_texto(&condicao.valor)?;
            let escapado = cru
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            params.push(mysql_async::Value::Bytes(format!("%{escapado}%").into_bytes()));
            partes.push(format!("{citada} LIKE ?"));
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
        params.push(para_mysql(&condicao.valor));
        partes.push(format!("{citada} {operador} ?"));
    }
    if partes.is_empty() {
        Ok(String::new())
    } else {
        Ok(format!(" WHERE {}", partes.join(" AND ")))
    }
}

impl Mysql {
    pub fn abrir(conexao: &Conexao) -> Result<Self> {
        let senha = conexao.senha()?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| anyhow!(tr("erros.bancoAbrir", &[("detalhe", &e.to_string())])))?;
        let mut construtor = mysql_async::OptsBuilder::default()
            .ip_or_hostname(conexao.host.trim())
            .tcp_port(conexao.porta_efetiva())
            .db_name(Some(conexao.banco.trim()));
        if !conexao.usuario.trim().is_empty() {
            construtor = construtor.user(Some(conexao.usuario.trim()));
        }
        if !senha.is_empty() {
            construtor = construtor.pass(Some(senha.as_str()));
        }
        let conexao = runtime
            .block_on(mysql_async::Conn::new(construtor))
            .map_err(erro_abrir)?;
        Ok(Self { runtime, conexao })
    }

    fn linhas_texto(&mut self, sql: &str, params: mysql_async::Params) -> Result<Vec<mysql_async::Row>> {
        self.runtime.block_on(async {
            let mut resultado = self
                .conexao
                .exec_iter(sql, params)
                .await
                .map_err(erro)?;
            let mut linhas = Vec::new();
            while let Some(linha) = resultado.next().await.map_err(erro)? {
                linhas.push(linha);
            }
            Ok(linhas)
        })
    }
}

impl Motor for Mysql {
    fn fontes(&mut self) -> Result<Vec<String>> {
        let linhas = self.linhas_texto(
            "SELECT table_name FROM information_schema.tables \
             WHERE table_schema = DATABASE() AND table_type = 'BASE TABLE' \
             ORDER BY table_name",
            params_posicionais(&[]),
        )?;
        Ok(linhas.iter().filter_map(|linha| texto_de(linha, 0)).collect())
    }

    fn colunas(&mut self, fonte: &str) -> Result<Vec<(String, String)>> {
        let conhecidas = self.fontes()?;
        let conferida = conferir_fonte(fonte, &conhecidas)?;
        let linhas = self.linhas_texto(
            "SELECT column_name, data_type FROM information_schema.columns \
             WHERE table_schema = DATABASE() AND table_name = ? \
             ORDER BY ordinal_position",
            params_posicionais(&[mysql_async::Value::Bytes(conferida.into_bytes())]),
        )?;
        Ok(linhas
            .iter()
            .filter_map(|linha| {
                let nome = texto_de(linha, 0)?;
                let tipo = texto_de(linha, 1).unwrap_or_default();
                Some((nome, tipo))
            })
            .collect())
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
        let esquema = self.colunas(&conferida)?;
        let pedidas = if colunas_pedidas.is_empty() {
            esquema
                .iter()
                .map(|(nome, _)| nome.clone())
                .collect::<Vec<_>>()
        } else {
            colunas_pedidas.to_vec()
        };
        let citadas = pedidas
            .iter()
            .map(|pedida| Ok(citado(&conferir_coluna(pedida, &esquema)?.0)))
            .collect::<Result<Vec<_>>>()?;
        let mut params = Vec::new();
        let onde = onde_sql(filtro, &esquema, &mut params)?;
        let ordenar = match ordem {
            None => String::new(),
            Some((coluna, desc)) => {
                let (nome, _) = conferir_coluna(coluna, &esquema)?;
                format!(" ORDER BY {} {}", citado(&nome), if desc { "DESC" } else { "ASC" })
            }
        };
        // Uma linha a mais do que o teto: é ela que diz que o corte foi por
        // limite, e não que o banco acabou ali.
        let sql = format!(
            "SELECT {} FROM {}{}{} LIMIT {}",
            citadas.join(", "),
            citado(&conferida),
            onde,
            ordenar,
            limite.saturating_add(1)
        );
        let linhas = self.linhas_texto(sql.as_str(), params_posicionais(&params))?;
        let mut saida = Vec::new();
        for linha in &linhas {
            let mut valores = Vec::with_capacity(citadas.len());
            for indice in 0..linha.len() {
                valores.push(para_json(
                    linha.as_ref(indice).unwrap_or(&mysql_async::Value::NULL),
                ));
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
        let esquema = self.colunas(&conferida)?;
        let mut params = Vec::new();
        let onde = onde_sql(filtro, &esquema, &mut params)?;
        let sql = format!("SELECT COUNT(*) FROM {}{}", citado(&conferida), onde);
        let linhas = self.linhas_texto(sql.as_str(), params_posicionais(&params))?;
        let Some(linha) = linhas.first() else {
            return Ok(0);
        };
        Ok(contagem_de(linha))
    }

    fn inserir(&mut self, fonte: &str, valores: &Map<String, Value>) -> Result<u64> {
        let conhecidas = self.fontes()?;
        let conferida = conferir_fonte(fonte, &conhecidas)?;
        let esquema = self.colunas(&conferida)?;
        if valores.is_empty() {
            bail!(tr("erros.bancoValoresVazios", &[]));
        }
        let mut citadas = Vec::new();
        let mut marcas = Vec::new();
        let mut params = Vec::new();
        for (coluna, valor) in valores {
            let (nome, _) = conferir_coluna(coluna, &esquema)?;
            citadas.push(citado(&nome));
            marcas.push("?".to_string());
            params.push(para_mysql(valor));
        }
        let sql = format!(
            "INSERT INTO {} ({}) VALUES ({})",
            citado(&conferida),
            citadas.join(", "),
            marcas.join(", ")
        );
        let resultado = self
            .runtime
            .block_on(self.conexao.exec_iter(sql.as_str(), params_posicionais(&params)))
            .map_err(erro)?;
        Ok(resultado.affected_rows())
    }

    fn atualizar(
        &mut self,
        fonte: &str,
        valores: &Map<String, Value>,
        filtro: &Filtro,
    ) -> Result<u64> {
        let conhecidas = self.fontes()?;
        let conferida = conferir_fonte(fonte, &conhecidas)?;
        let esquema = self.colunas(&conferida)?;
        if valores.is_empty() {
            bail!(tr("erros.bancoValoresVazios", &[]));
        }
        if filtro.is_empty() {
            bail!(tr("erros.bancoOndeObrigatorio", &[]));
        }
        let mut atribuicoes = Vec::new();
        let mut params = Vec::new();
        for (coluna, valor) in valores {
            let (nome, _) = conferir_coluna(coluna, &esquema)?;
            atribuicoes.push(format!("{} = ?", citado(&nome)));
            params.push(para_mysql(valor));
        }
        let onde = onde_sql(filtro, &esquema, &mut params)?;
        let sql = format!(
            "UPDATE {} SET {}{}",
            citado(&conferida),
            atribuicoes.join(", "),
            onde
        );
        let resultado = self
            .runtime
            .block_on(self.conexao.exec_iter(sql.as_str(), params_posicionais(&params)))
            .map_err(erro)?;
        Ok(resultado.affected_rows())
    }

    fn apagar(&mut self, fonte: &str, filtro: &Filtro) -> Result<u64> {
        let conhecidas = self.fontes()?;
        let conferida = conferir_fonte(fonte, &conhecidas)?;
        let esquema = self.colunas(&conferida)?;
        if filtro.is_empty() {
            bail!(tr("erros.bancoOndeObrigatorio", &[]));
        }
        let mut params = Vec::new();
        let onde = onde_sql(filtro, &esquema, &mut params)?;
        let sql = format!("DELETE FROM {}{}", citado(&conferida), onde);
        let resultado = self
            .runtime
            .block_on(self.conexao.exec_iter(sql.as_str(), params_posicionais(&params)))
            .map_err(erro)?;
        Ok(resultado.affected_rows())
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
            .block_on(self.conexao.prep(declaracao))
            .map_err(erro)?;
        // O banco mesmo classifica: uma declaração que devolve colunas é
        // consulta; as outras, escrita. Nenhuma lista de palavras no meio.
        let colunas = preparada
            .columns()
            .iter()
            .map(|coluna| coluna.name_str().into_owned())
            .collect::<Vec<_>>();
        if colunas.is_empty() {
            let resultado = self
                .runtime
                .block_on(
                    self.conexao
                        .exec_iter(&preparada, mysql_async::Params::Empty),
                )
                .map_err(erro)?;
            return Ok(ResultadoSql::Afetadas(resultado.affected_rows()));
        }
        // A consulta volta embrulhada, com teto próprio: o banco ainda pode
        // ter linhas além do que a leitura levou, e isso é o que se diz.
        let sql = format!(
            "SELECT * FROM ({declaracao}) AS `__aether_consulta__` LIMIT {}",
            limite.saturating_add(1)
        );
        let mut saida = Vec::new();
        self.runtime.block_on(async {
            let mut resultado = self.conexao.query_iter(sql.as_str()).await.map_err(erro)?;
            while let Some(linha) = resultado.next().await.map_err(erro)? {
                let mut valores = Vec::with_capacity(colunas.len());
                for indice in 0..linha.len() {
                    valores.push(para_json(
                        linha.as_ref(indice).unwrap_or(&mysql_async::Value::NULL),
                    ));
                }
                saida.push(valores);
                if saida.len() >= limite.saturating_add(1) {
                    break;
                }
            }
            Ok::<(), anyhow::Error>(())
        })?;
        let truncado = saida.len() > limite;
        if truncado {
            saida.truncate(limite);
        }
        Ok(ResultadoSql::Consulta(Consulta {
            colunas,
            linhas: saida,
            truncado,
            duracao_ms: cronometro.ms(),
        }))
    }
}

fn erro_abrir(e: mysql_async::Error) -> anyhow::Error {
    anyhow!(tr("erros.bancoAbrir", &[("detalhe", &e.to_string())]))
}

fn erro(e: mysql_async::Error) -> anyhow::Error {
    anyhow!(tr("erros.bancoErro", &[("detalhe", &e.to_string())]))
}
