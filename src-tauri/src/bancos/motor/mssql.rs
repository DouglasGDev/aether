//! O motor SQL Server: protocolo TDS em Rust puro pelo tiberius, sem cliente
//! nativo e sem ODBC.
//!
//! Duas regras valem aqui como nos demais motores: identificadores são
//! conferidos contra `information_schema` antes de entrar entre colchetes, e
//! valores sempre saem como parâmetros `@Pn`. O que muda é a leitura: colunas
//! que o tiberius não tipa sem atraso extra (temporal, decimal, uuid, binário)
//! são convertidas para `nvarchar(max)` no próprio SELECT — o banco renderiza
//! o texto canônico, e nada chega como adivinhação. Para o SQL livre, o
//! `sp_describe_first_result_set` descreve o resultado antes da execução: é
//! ele quem diz consulta de escrita sem executar nada, e é dele que saem os
//! tipos das colunas do embrulho. O driver é assíncrono: a conexão é da
//! chamada, mas o agendador é do processo — ver `super::agendador`.

use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_util::compat::TokioAsyncWriteCompatExt;
use tiberius::{AuthMethod, Client, Config, EncryptionLevel, ExecuteResult, Row};

type ClienteTds = Client<tokio_util::compat::Compat<TcpStream>>;

pub struct Mssql {
    runtime: &'static tokio::runtime::Runtime,
    cliente: ClienteTds,
}

/// Como ler uma coluna do resultado. `Texto` cobre também o que o SELECT
/// converteu para `nvarchar(max)` — a leitura é sempre do tipo que o SQL
/// produz, nunca de um palpite.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Celula {
    Inteiro8,
    Inteiro16,
    Inteiro32,
    Inteiro64,
    Real32,
    Real64,
    Booleana,
    Texto,
}

/// O tipo sem parênteses: `nvarchar(50)` e `varchar(max)` são o mesmo texto.
fn base_de(tipo: &str) -> &str {
    tipo.split('(').next().unwrap_or(tipo).trim()
}

fn celula_de(tipo: &str) -> Celula {
    match base_de(tipo) {
        "tinyint" => Celula::Inteiro8,
        "smallint" => Celula::Inteiro16,
        "int" => Celula::Inteiro32,
        "bigint" => Celula::Inteiro64,
        "real" => Celula::Real32,
        "float" => Celula::Real64,
        "bit" => Celula::Booleana,
        _ => Celula::Texto,
    }
}

/// A expressão do SELECT para uma coluna. Numéricos e booleanos ficam nativos;
/// todo o resto vira `nvarchar(max)` — binário com o estilo 2, que é hex sem
/// o `0x`.
fn fragmento(tipo: &str, citada: &str) -> String {
    match base_de(tipo) {
        "tinyint" | "smallint" | "int" | "bigint" | "bit" | "real" | "float" => citada.to_string(),
        "binary" | "varbinary" | "image" => {
            format!("CONVERT(nvarchar(max), {citada}, 2)")
        }
        _ => format!("CAST({citada} AS nvarchar(max))"),
    }
}

fn citado(nome: &str) -> String {
    format!("[{}]", nome.replace(']', "]]"))
}

/// A ida única ao catálogo de uma fonte: a tabela conferida e as colunas dela,
/// na ordem e com o nome que o próprio banco grafa. `LOWER(...) = LOWER(@P1)` é
/// o que fazia a conferência da lista ser insensível, sem trazer a lista inteira.
const ESQUEMA_SQL: &str = "SELECT t.table_name, c.column_name, c.data_type \
     FROM information_schema.tables AS t \
     JOIN information_schema.columns AS c \
       ON c.table_schema = t.table_schema AND c.table_name = t.table_name \
     WHERE t.table_schema = SCHEMA_NAME() AND t.table_type = 'BASE TABLE' \
       AND LOWER(t.table_name) = LOWER(@P1) \
     ORDER BY c.ordinal_position";

/// As tabelas do esquema atual, para a lista que o dono vê — uma viagem só, e
/// `bancos_tabelas` não descreve coluna nenhuma delas.
const FONTES_SQL: &str = "SELECT table_name FROM information_schema.tables \
     WHERE table_schema = SCHEMA_NAME() AND table_type = 'BASE TABLE' \
     ORDER BY table_name";

fn nome_da(linha: &Row, indice: usize) -> Result<String> {
    Ok(linha
        .try_get::<&str, _>(indice)
        .map_err(erro)?
        .unwrap_or_default()
        .to_string())
}

fn conferir_coluna(coluna: &str, conhecidas: &[(String, String)]) -> Result<(String, String)> {
    conhecidas
        .iter()
        .find(|(nome, _)| nome.eq_ignore_ascii_case(coluna))
        .cloned()
        .ok_or_else(|| erro_coluna_invalida(coluna, conhecidas))
}

/// Um valor do JSON tipado pelo que o TDS entende. Número inteiro vai como
/// bigint e real como float: o SQL Server converte implicitamente para o tipo
/// da coluna na comparação, e objeto JSON vira texto — recusa é do banco.
enum ValorTds {
    Inteiro(i64),
    Real(f64),
    Booleano(bool),
    Texto(String),
}

fn para_tds(valor: &Value) -> Result<ValorTds> {
    match valor {
        Value::Bool(b) => Ok(ValorTds::Booleano(*b)),
        Value::Number(n) => Ok(match n.as_i64() {
            Some(i) => ValorTds::Inteiro(i),
            None => ValorTds::Real(n.as_f64().unwrap_or(0.0)),
        }),
        Value::String(texto) => Ok(ValorTds::Texto(texto.clone())),
        Value::Object(_) | Value::Array(_) => Ok(ValorTds::Texto(valor.to_string())),
        Value::Null => bail!(tr("erros.filtroValorInvalido", &[])),
    }
}

fn referencias(valores: &[ValorTds]) -> Vec<&dyn tiberius::ToSql> {
    valores
        .iter()
        .map(|valor| match valor {
            ValorTds::Inteiro(i) => i as &dyn tiberius::ToSql,
            ValorTds::Real(r) => r as &dyn tiberius::ToSql,
            ValorTds::Booleano(b) => b as &dyn tiberius::ToSql,
            ValorTds::Texto(t) => t as &dyn tiberius::ToSql,
        })
        .collect()
}

fn proxima(numero: &mut usize) -> String {
    let marca = format!("@P{numero}");
    *numero += 1;
    marca
}

/// O WHERE em SQL, com marcas `@Pn` e os valores na ordem.
fn onde_sql(
    filtro: &Filtro,
    colunas: &[(String, String)],
    params: &mut Vec<ValorTds>,
    numero: &mut usize,
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
            // "contendo" é LIKE sobre texto: a conversão é explícita para não
            // depender de coerção implícita em coluna numérica.
            let cru = match &condicao.valor {
                Value::String(texto) => texto.clone(),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                _ => bail!(tr("erros.filtroValorInvalido", &[])),
            };
            let escapado = cru
                .replace('[', "[[]")
                .replace('%', "[%]")
                .replace('_', "[_]");
            let marca = proxima(numero);
            params.push(ValorTds::Texto(format!("%{escapado}%")));
            partes.push(format!("CAST({citada} AS nvarchar(max)) LIKE {marca}"));
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
        params.push(para_tds(&condicao.valor)?);
        partes.push(format!("{citada} {operador} {marca}"));
    }
    if partes.is_empty() {
        Ok(String::new())
    } else {
        Ok(format!(" WHERE {}", partes.join(" AND ")))
    }
}

/// O que a conexão pede do canal TDS. Duas possibilidades, e nenhuma delas é
/// um número: quem decide é a flag `tls_verificado` do cadastro.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Confianca {
    /// Como era antes da flag existir: qualquer certificado é aceito. Numa rede
    /// local com servidor auto-assinado — o caso mais comum desta máquina — é o
    /// que deixa a ligação de pé.
    ConfiarEmQualquerCertificado,
    /// O certificado do servidor só vale se a cadeia da máquina o reconhecer,
    /// e o tráfego sai cifrado de ponta a ponta.
    Conferido,
}

/// A única leitura da flag neste motor. Medível sem servidor: o `Config` do
/// tiberius guarda seus campos como `pub(crate)` e não devolve a postura, então
/// a decisão vive aqui, separada da aplicação.
fn confianca_da(conexao: &Conexao) -> Confianca {
    if conexao.tls_verificado {
        Confianca::Conferido
    } else {
        Confianca::ConfiarEmQualquerCertificado
    }
}

/// A ficha do tiberius, inteira, a partir do cadastro. Nada aqui depende de
/// porta ou host: mudar a porta não muda a confiança, e confiar é sempre um
/// ato do cadastro, nunca um número escrito à mão.
fn ficha_de(conexao: &Conexao, senha: &str) -> Config {
    let mut ficha = Config::new();
    ficha.host(conexao.host.trim());
    ficha.port(conexao.porta_efetiva());
    if !conexao.banco.trim().is_empty() {
        ficha.database(conexao.banco.trim());
    }
    ficha.authentication(AuthMethod::sql_server(conexao.usuario.trim(), senha));
    match confianca_da(conexao) {
        // Sem a marca, o comportamento é o de sempre: confiar. A certificação
        // é da rede do dono, e sem confiar aqui o login falha no certificado
        // antes de qualquer SQL ser falado.
        Confianca::ConfiarEmQualquerCertificado => ficha.trust_cert(),
        // Marcada: nada de `trust_cert`. A cifra é obrigada (o tiberius já
        // traz `native-tls`, e `Required` é o que ele usa por defeito — dito
        // aqui para a intenção ficar no código, não herdada de um defeito) e o
        // certificado passa pela cadeia do sistema.
        Confianca::Conferido => ficha.encryption(EncryptionLevel::Required),
    }
    ficha
}

/// A espera de quem não tem prazo dentro do driver: o `tiberius` 0.12 não expõe
/// tempo de consulta nenhum — não há `set_query_timeout` nem opção de soquete —
/// então o prazo mora aqui. Sem ele, um SQL Server pendurado numa consulta longa
/// segurava a conversa até o dono fechar o aplicativo.
fn esperar_prazo<T>(
    runtime: &'static tokio::runtime::Runtime,
    prazo: Duration,
    futuro: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    runtime.block_on(async move {
        tokio::time::timeout(prazo, futuro).await.map_err(|_| erro_de_prazo())?
    })
}

fn esperar<T>(
    runtime: &'static tokio::runtime::Runtime,
    futuro: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    esperar_prazo(runtime, prazo_de_espera(), futuro)
}

impl Mssql {
    pub fn abrir(conexao: &Conexao) -> Result<Self> {
        let senha = conexao.senha()?;
        let ficha = ficha_de(conexao, senha.as_str());
        let runtime = super::agendador()?;
        let cliente = esperar(runtime, async {
            let tcp = TcpStream::connect(ficha.get_addr())
                .await
                .map_err(|e| anyhow!(tr("erros.bancoAbrir", &[("detalhe", &e.to_string())])))?;
            tcp.set_nodelay(true)
                .map_err(|e| anyhow!(tr("erros.bancoAbrir", &[("detalhe", &e.to_string())])))?;
            Client::connect(ficha, tcp.compat_write()).await.map_err(erro_abrir)
        })?;
        Ok(Self { runtime, cliente })
    }

    /// Uma consulta com parâmetros, lendo o primeiro resultado inteiro. O que se
    /// materializa aqui é o que o próprio SQL pediu — `TOP (teto)` em toda
    /// leitura — e nunca o que o servidor resolveu mandar.
    fn linhas(&mut self, sql: &str, refs: &[&dyn tiberius::ToSql]) -> Result<Vec<Row>> {
        let runtime = self.runtime;
        esperar(runtime, async {
            let stream = self.cliente.query(sql, refs).await.map_err(erro)?;
            stream.into_first_result().await.map_err(erro)
        })
    }

    /// Uma escrita com parâmetros, com o total de linhas afetadas que o banco
    /// reportou.
    fn escrita(&mut self, sql: &str, refs: &[&dyn tiberius::ToSql]) -> Result<u64> {
        let runtime = self.runtime;
        esperar(runtime, async {
            let resultado: ExecuteResult = self.cliente.execute(sql, refs).await.map_err(erro)?;
            Ok(resultado.total())
        })
    }

    /// A única ida ao catálogo que uma leitura precisa: a fonte conferida e as
    /// colunas dela, juntas. Antes eram duas — a lista de tabelas inteira e,
    /// dentro dela, a fonte que o chamador já podia ter pedido direto.
    fn esquema_de(&mut self, fonte: &str) -> Result<(String, Vec<(String, String)>)> {
        let linhas = self.linhas(ESQUEMA_SQL, &[&fonte])?;
        let Some(primeira) = linhas.first() else {
            return Err(anyhow!(tr("erros.bancoFonteInexistente", &[("fonte", fonte)])));
        };
        let conferida = nome_da(primeira, 0)?;
        let mut esquema = Vec::with_capacity(linhas.len());
        for linha in &linhas {
            let nome = nome_da(linha, 1)?;
            if nome.is_empty() {
                continue;
            }
            esquema.push((nome, nome_da(linha, 2).unwrap_or_default()));
        }
        Ok((conferida, esquema))
    }

    fn ler(linha: &Row, indice: usize, celula: Celula) -> Result<Value> {
        match celula {
            Celula::Inteiro8 => Ok(linha
                .try_get::<u8, _>(indice)
                .map_err(erro)?
                .map(|v| Value::Number(v.into()))
                .unwrap_or(Value::Null)),
            Celula::Inteiro16 => Ok(linha
                .try_get::<i16, _>(indice)
                .map_err(erro)?
                .map(|v| Value::Number(v.into()))
                .unwrap_or(Value::Null)),
            Celula::Inteiro32 => Ok(linha
                .try_get::<i32, _>(indice)
                .map_err(erro)?
                .map(|v| Value::Number(v.into()))
                .unwrap_or(Value::Null)),
            Celula::Inteiro64 => Ok(linha
                .try_get::<i64, _>(indice)
                .map_err(erro)?
                .map(|v| Value::Number(v.into()))
                .unwrap_or(Value::Null)),
            Celula::Real32 => Ok(linha
                .try_get::<f32, _>(indice)
                .map_err(erro)?
                .and_then(|v| serde_json::Number::from_f64(v as f64).map(Value::Number))
                .unwrap_or(Value::Null)),
            Celula::Real64 => Ok(linha
                .try_get::<f64, _>(indice)
                .map_err(erro)?
                .and_then(|v| serde_json::Number::from_f64(v).map(Value::Number))
                .unwrap_or(Value::Null)),
            Celula::Booleana => Ok(linha
                .try_get::<bool, _>(indice)
                .map_err(erro)?
                .map(Value::Bool)
                .unwrap_or(Value::Null)),
            Celula::Texto => Ok(linha
                .try_get::<&str, _>(indice)
                .map_err(erro)?
                .map(|s| Value::String(s.to_string()))
                .unwrap_or(Value::Null)),
        }
    }
}

use super::{
    Comparacao, Consulta, Cronometro, Filtro, Motor, ResultadoSql, aparar_no_teto,
    erro_coluna_invalida, erro_de_prazo, ordem_de, prazo_de_espera, teto_de_leitura,
};
use crate::bancos::config::Conexao;
use crate::idiomas::tr;

impl Motor for Mssql {
    fn fontes(&mut self) -> Result<Vec<String>> {
        let linhas = self.linhas(FONTES_SQL, &[])?;
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
            esquema.iter().map(|(nome, _)| nome.clone()).collect::<Vec<_>>()
        } else {
            colunas_pedidas.to_vec()
        };
        let mut celulas = Vec::new();
        for pedida in &pedidas {
            let (nome, tipo) = conferir_coluna(pedida, &esquema)?;
            celulas.push((nome, tipo));
        }
        let selecao = celulas
            .iter()
            .map(|(nome, tipo)| fragmento(tipo, &citado(nome)))
            .collect::<Vec<_>>()
            .join(", ");
        let mut params = Vec::new();
        let mut numero = 1usize;
        let onde = onde_sql(filtro, &esquema, &mut params, &mut numero)?;
        let ordenar = match ordem {
            None => String::new(),
            Some((texto, desc)) => {
                let (coluna, desc) = ordem_de(texto, desc, Some(&esquema))?;
                let (nome, _) = conferir_coluna(&coluna, &esquema)?;
                format!(" ORDER BY {} {}", citado(&nome), if desc { "DESC" } else { "ASC" })
            }
        };
        // Uma linha a mais do que o teto: é ela que diz que o corte foi por
        // limite, e não que o banco acabou ali.
        let sql = format!(
            "SELECT TOP ({}) {} FROM {}{}{}",
            teto_de_leitura(limite),
            selecao,
            citado(&conferida),
            onde,
            ordenar
        );
        let refs = referencias(&params);
        let linhas = self.linhas(sql.as_str(), &refs)?;
        let mut saida = Vec::new();
        for linha in &linhas {
            let mut valores = Vec::with_capacity(celulas.len());
            for (indice, (_, tipo)) in celulas.iter().enumerate() {
                valores.push(Self::ler(linha, indice, celula_de(tipo))?);
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
        // COUNT_BIG devolve bigint: o COUNT(*) do SQL Server é int de 32 bits
        // e estouraria numa tabela grande.
        let sql = format!("SELECT COUNT_BIG(*) FROM {}{}", citado(&conferida), onde);
        let refs = referencias(&params);
        let linhas = self.linhas(sql.as_str(), &refs)?;
        let Some(linha) = linhas.first() else {
            return Ok(0);
        };
        Ok(linha.try_get::<i64, _>(0).map_err(erro)?.unwrap_or(0).max(0) as u64)
    }

    fn inserir(&mut self, fonte: &str, valores: &Map<String, Value>) -> Result<u64> {
        let (conferida, esquema) = self.esquema_de(fonte)?;
        if valores.is_empty() {
            bail!(tr("erros.bancoValoresVazios", &[]));
        }
        let mut citadas = Vec::new();
        let mut marcas = Vec::new();
        let mut params = Vec::new();
        let mut numero = 1usize;
        for (coluna, valor) in valores {
            let (nome, _) = conferir_coluna(coluna, &esquema)?;
            citadas.push(citado(&nome));
            if valor.is_null() {
                marcas.push("NULL".to_string());
            } else {
                marcas.push(proxima(&mut numero));
                params.push(para_tds(valor)?);
            }
        }
        let sql = format!(
            "INSERT INTO {} ({}) VALUES ({})",
            citado(&conferida),
            citadas.join(", "),
            marcas.join(", ")
        );
        let refs = referencias(&params);
        self.escrita(sql.as_str(), &refs)
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
            let (nome, _) = conferir_coluna(coluna, &esquema)?;
            if valor.is_null() {
                atribuicoes.push(format!("{} = NULL", citado(&nome)));
            } else {
                atribuicoes.push(format!("{} = {}", citado(&nome), proxima(&mut numero)));
                params.push(para_tds(valor)?);
            }
        }
        let onde = onde_sql(filtro, &esquema, &mut params, &mut numero)?;
        let sql = format!(
            "UPDATE {} SET {}{}",
            citado(&conferida),
            atribuicoes.join(", "),
            onde
        );
        let refs = referencias(&params);
        self.escrita(sql.as_str(), &refs)
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
        let refs = referencias(&params);
        self.escrita(sql.as_str(), &refs)
    }

    fn sql(&mut self, declaracao: &str, limite: usize) -> Result<ResultadoSql> {
        let cronometro = Cronometro::agora();
        let declaracao = declaracao.trim();
        let declaracao = declaracao.strip_suffix(';').unwrap_or(declaracao).trim();
        if declaracao.is_empty() {
            bail!(tr("erros.bancoSqlVazio", &[]));
        }
        if declaracao.contains(';') {
            bail!(tr("erros.bancoSqlMultipla", &[]));
        }
        // O banco descreve o resultado antes de executar de verdade: zero
        // colunas é escrita; colunas nomeadas são a receita do embrulho.
        let descritas = self.linhas(
            "EXEC sp_describe_first_result_set @tsql = @P1",
            &[&declaracao],
        )?;
        if descritas.is_empty() {
            let refs: Vec<&dyn tiberius::ToSql> = Vec::new();
            return Ok(ResultadoSql::Afetadas(self.escrita(declaracao, &refs)?));
        }
        let mut celulas = Vec::new();
        for linha in &descritas {
            let nome = linha
                .try_get::<&str, _>("name")
                .map_err(erro)?
                .ok_or_else(|| anyhow!(tr("erros.bancoSqlSemNome", &[])))?
                .to_string();
            let tipo = linha
                .try_get::<&str, _>("system_type_name")
                .map_err(erro)?
                .unwrap_or("")
                .to_string();
            celulas.push((nome, tipo));
        }
        let selecao = celulas
            .iter()
            .map(|(nome, tipo)| fragmento(tipo, &citado(nome)))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT TOP ({}) {} FROM ({declaracao}) AS [__aether__]",
            teto_de_leitura(limite),
            selecao
        );
        let refs: Vec<&dyn tiberius::ToSql> = Vec::new();
        let linhas = self.linhas(sql.as_str(), &refs)?;
        let mut saida = Vec::new();
        for linha in &linhas {
            let mut valores = Vec::with_capacity(celulas.len());
            for (indice, (_, tipo)) in celulas.iter().enumerate() {
                valores.push(Self::ler(linha, indice, celula_de(tipo))?);
            }
            saida.push(valores);
        }
        let (linhas, truncado) = aparar_no_teto(saida, limite);
        Ok(ResultadoSql::Consulta(Consulta {
            colunas: celulas.into_iter().map(|(nome, _)| nome).collect(),
            linhas,
            truncado,
            duracao_ms: cronometro.ms(),
        }))
    }
}

fn erro_abrir(e: tiberius::error::Error) -> anyhow::Error {
    anyhow!(tr("erros.bancoAbrir", &[("detalhe", &e.to_string())]))
}

fn erro(e: tiberius::error::Error) -> anyhow::Error {
    anyhow!(tr("erros.bancoErro", &[("detalhe", &e.to_string())]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bancos::config::{conexao_de_teste, Tipo};
    use crate::bancos::motor::PRAZO_DE_LEITURA;

    /// O nome da ordem continua sendo conferido no schema real; texto de SQL
    /// não vira coluna, e o erro ajuda o modelo a corrigir a chamada.
    #[test]
    fn coluna_de_ordem_invalida_lista_as_validas_e_o_exemplo() {
        let schema = vec![
            ("id".to_string(), "int".to_string()),
            ("qt_venda".to_string(), "int".to_string()),
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

    /// O catálogo de uma fonte cabe numa declaração: a tabela conferida e as
    /// colunas dela na mesma junta. Duas viagens — a lista inteira e depois as
    /// colunas — é o que fazia cada leitura pagar duas idas antes de ler nada.
    #[test]
    fn o_catalogo_de_uma_fonte_e_uma_ida_s() {
        assert!(
            ESQUEMA_SQL.contains("information_schema.tables"),
            "defeito 2: sem a tabela no catálogo junto das colunas, a fonte é conferida numa viagem e lida em outra"
        );
        assert!(
            ESQUEMA_SQL.contains("information_schema.columns"),
            "defeito 2: sem as colunas na mesma viagem, cada leitura paga duas idas ao catálogo"
        );
        assert_eq!(
            ESQUEMA_SQL.matches(';').count(),
            0,
            "o catálogo de uma fonte tem de caber numa declaração"
        );
        assert!(
            ESQUEMA_SQL.contains("LOWER(t.table_name) = LOWER(@P1)"),
            "sem a conferência insensível no servidor, a fonte digitada em outra caixa sumiria"
        );
        assert!(
            !FONTES_SQL.contains("information_schema.columns"),
            "listar tabelas não descreve coluna nenhuma: é a segunda viagem que sobra aqui"
        );
    }

    /// O prazo espera de verdade: o que passa dele volta como erro do prazo, e
    /// não como resultado. A tarefa dorme mais que o prazo e devolve `Ok`, então
    /// o único erro possível aqui é o relógio do motor.
    #[test]
    fn uma_ida_que_passa_do_prazo_nao_vira_espera_infinita() {
        let runtime = super::super::agendador().expect("o agendador do processo nasce");
        let resultado = esperar_prazo(
            runtime,
            Duration::from_millis(20),
            async {
                tokio::time::sleep(Duration::from_millis(500)).await;
                Ok::<(), anyhow::Error>(())
            },
        );
        let erro = match resultado {
            Err(erro) => erro,
            Ok(()) => panic!("defeito 4: a ida voltou depois de passar do prazo — ninguém esperou nada"),
        };
        assert!(
            erro.to_string().contains('1'),
            "a frase do prazo tem de dizer segundos: {erro}"
        );
    }

    /// A espera do cliente fica um segundo acima do prazo: quando os dois
    /// existem, o erro que chega primeiro tem de ser o do banco.
    #[test]
    fn a_espera_do_cliente_fica_acima_do_prazo() {
        assert!(prazo_de_espera() > PRAZO_DE_LEITURA);
    }

    /// A confiança sai da flag, e de nada mais. Numeral de porta e host são
    /// escritos à mão pelo dono e não podem, sozinhos, decidir se o canal é
    /// conferido.
    #[test]
    fn a_confianca_tds_sai_da_flag_e_nao_de_um_numero() {
        let mut sem_marca = conexao_de_teste(Tipo::Mssql);
        assert_eq!(
            confianca_da(&sem_marca),
            Confianca::ConfiarEmQualquerCertificado,
            "sem a marca fica o comportamento de antes: `trust_cert`"
        );
        sem_marca.porta = Some(1433);
        sem_marca.host = "127.0.0.1".into();
        assert_eq!(
            confianca_da(&sem_marca),
            Confianca::ConfiarEmQualquerCertificado,
            "nem a porta dita nem o host ligam a confiança"
        );
        sem_marca.tls_verificado = true;
        assert_eq!(
            confianca_da(&sem_marca),
            Confianca::Conferido,
            "a marca é a única coisa que muda a postura"
        );
    }

    /// A ficha é uma só para as duas posturas: o que a flag escolhe é a
    /// confiança, nunca o endereço. `Config` do tiberius não devolve os campos
    /// de TLS (são `pub(crate)`), então o que se mede sem servidor é isto.
    #[test]
    fn a_flag_nao_troca_o_endereco_da_ficha() {
        let lisa = conexao_de_teste(Tipo::Mssql);
        let mut conferida = conexao_de_teste(Tipo::Mssql);
        conferida.tls_verificado = true;
        assert_eq!(
            ficha_de(&conferida, "").get_addr(),
            ficha_de(&lisa, "").get_addr(),
            "a mesma conexão, com ou sem a flag, aponta para o mesmo lugar"
        );
        let mut outra_porta = conexao_de_teste(Tipo::Mssql);
        outra_porta.porta = Some(21433);
        assert_eq!(ficha_de(&outra_porta, "").get_addr(), "10.0.0.8:21433");
    }

    /// O que só um servidor de verdade responde: com a flag ligada, um SQL
    /// Server de certificado auto-assinado tem de ser recusado no certificado
    /// antes de qualquer SQL; desligada, a ligação tem de continuar de pé.
    /// Fica ignorado porque esta máquina não tem SQL Server — medir isto aqui
    /// seria fingir.
    #[ignore = "precisa de um SQL Server real com certificado auto-assinado; sem ele a handshake não existe"]
    #[test]
    fn um_servidor_auto_assinado_e_recusado_com_a_flag_ligada() {
        let mut conferida = conexao_de_teste(Tipo::Mssql);
        conferida.tls_verificado = true;
        assert!(
            Mssql::abrir(&conferida).is_err(),
            "com a marca, o certificado que a máquina não reconhece tem de barrar a ligação"
        );

        let lisa = conexao_de_teste(Tipo::Mssql);
        assert!(Mssql::abrir(&lisa).is_ok(), "sem a marca, a ligação continua como sempre");
    }
}
