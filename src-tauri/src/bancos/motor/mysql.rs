//! O motor MySQL/MariaDB: protocolo de texto nativo, sem cliente instalado.
//!
//! O MySQL é coerção antes de rigor: uma comparação entre coluna inteira e
//! parâmetro textual converte sozinha, então os valores chegam tipados pelo
//! que o JSON diz (Int, Double, Bytes, NULL) e o banco decide a conversão.
//! Identificadores, ao contrário, não têm coerção — tabela e coluna são
//! conferidos contra `information_schema` antes de entrar entre crases. O
//! driver é assíncrono: a conexão é da chamada, mas o agendador é do processo —
//! ver `super::agendador`.

use anyhow::{Result, anyhow, bail};
use mysql_async::prelude::*;
use serde_json::{Map, Value};

use super::{
    Comparacao, Consulta, Cronometro, Filtro, Motor, ResultadoSql, aparar_no_teto, erro_de_prazo,
    prazo_de_espera, teto_de_leitura,
};
use crate::bancos::config::Conexao;
use crate::idiomas::tr;
use std::time::Duration;

pub struct Mysql {
    runtime: &'static tokio::runtime::Runtime,
    conexao: mysql_async::Conn,
}

fn citado(nome: &str) -> String {
    format!("`{}`", nome.replace('`', "``"))
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

/// A ida única ao catálogo de uma fonte: a tabela conferida e as colunas dela,
/// na ordem e com o nome que o próprio banco grafa. `LOWER(...) = LOWER(?)` é o
/// que fazia a conferência da lista ser insensível, sem trazer a lista inteira.
const ESQUEMA_SQL: &str = "SELECT t.table_name, c.column_name, c.data_type \
     FROM information_schema.tables AS t \
     JOIN information_schema.columns AS c \
       ON c.table_schema = t.table_schema AND c.table_name = t.table_name \
     WHERE t.table_schema = DATABASE() AND t.table_type = 'BASE TABLE' \
       AND LOWER(t.table_name) = LOWER(?) \
     ORDER BY c.ordinal_position";

/// As tabelas do banco atual, para a lista que o dono escolhe — uma viagem só,
/// e `bancos_tabelas` não descreve coluna nenhuma delas.
const FONTES_SQL: &str = "SELECT table_name FROM information_schema.tables \
     WHERE table_schema = DATABASE() AND table_type = 'BASE TABLE' \
     ORDER BY table_name";

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

/// O SELECT de uma leitura, com o teto escrito: `limite + 1`, porque a linha de
/// barriga é a única prova de que o corte foi por limite e não fim do banco.
fn leitura_sql(citadas: &str, fonte: &str, onde: &str, ordenar: &str, limite: usize) -> String {
    format!(
        "SELECT {citadas} FROM {fonte}{onde}{ordenar} LIMIT {}",
        teto_de_leitura(limite)
    )
}

/// A ficha de abertura, separada da ligação para que se leia o que ela pede ao
/// servidor sem tocar a rede.
fn opcoes_de(conexao: &Conexao, senha: &str) -> mysql_async::OptsBuilder {
    let mut construtor = mysql_async::OptsBuilder::default()
        .ip_or_hostname(conexao.host.trim())
        .tcp_port(conexao.porta_efetiva())
        .db_name(Some(conexao.banco.trim()));
    if !conexao.usuario.trim().is_empty() {
        construtor = construtor.user(Some(conexao.usuario.trim()));
    }
    if !senha.is_empty() {
        construtor = construtor.pass(Some(senha));
    }
    construtor
}

/// A espera de quem não tem prazo do lado do servidor: `mysql_async` 0.35 não
/// expõe prazo de leitura nenhum na ficha — o único `wait_timeout` é variável de
/// sessão, e `MAX_EXECUTION_TIME` é do MySQL e não da MariaDB que este mesmo
/// motor atende, onde o nome certo é outro; um `SET` errado no meio da abertura
/// derrubaria o login de quem tem razão. Sobra o prazo do cliente, que larga o
/// soquete e devolve ao modelo a frase com o número.
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

impl Mysql {
    pub fn abrir(conexao: &Conexao) -> Result<Self> {
        conferir_canal(conexao)?;
        let senha = conexao.senha()?;
        let runtime = super::agendador()?;
        let conexao = esperar(runtime, async {
            mysql_async::Conn::new(opcoes_de(conexao, senha.as_str()))
                .await
                .map_err(erro_abrir)
        })?;
        Ok(Self { runtime, conexao })
    }

    /// As linhas de uma declaração, no máximo até o teto e no máximo até o prazo.
    fn linhas_ate(
        &mut self,
        sql: &str,
        params: mysql_async::Params,
        teto: usize,
    ) -> Result<Vec<mysql_async::Row>> {
        let runtime = self.runtime;
        esperar(runtime, async {
            let mut resultado = self.conexao.exec_iter(sql, params).await.map_err(erro)?;
            let mut linhas = Vec::new();
            // O teto está no laço e não só no `LIMIT`: o que materializa o
            // resultado todo antes de aparar puxa o banco inteiro quando o
            // servidor devolve mais do que foi pedido.
            while linhas.len() < teto {
                let Some(linha) = resultado.next().await.map_err(erro)? else {
                    break;
                };
                linhas.push(linha);
            }
            Ok(linhas)
        })
    }

    /// O catálogo é do tamanho do esquema, não do tamanho de uma tabela: as
    /// declarações dele vêm inteiras, e o que não vem inteiro é o dado de dono.
    fn catalogo(&mut self, sql: &str, params: mysql_async::Params) -> Result<Vec<mysql_async::Row>> {
        self.linhas_ate(sql, params, usize::MAX)
    }

    /// Uma escrita, com o número de linhas que o banco reportou. Cai sob o mesmo
    /// prazo da leitura: escrita pendurada custa a conversa tanto quanto.
    fn escrita(&mut self, sql: &str, params: mysql_async::Params) -> Result<u64> {
        let runtime = self.runtime;
        esperar(runtime, async {
            let resultado = self.conexao.exec_iter(sql, params).await.map_err(erro)?;
            Ok(resultado.affected_rows())
        })
    }

    /// A única ida ao catálogo que uma leitura precisa: a fonte conferida e as
    /// colunas dela, juntas. Antes eram quatro declarações — a lista de tabelas,
    /// a lista de tabelas de novo dentro de `colunas`, as colunas, e o dado.
    fn esquema_de(&mut self, fonte: &str) -> Result<(String, Vec<(String, String)>)> {
        let linhas = self.catalogo(
            ESQUEMA_SQL,
            params_posicionais(&[mysql_async::Value::Bytes(fonte.trim().to_string().into_bytes())]),
        )?;
        let Some(primeira) = linhas.first() else {
            return Err(anyhow!(tr("erros.bancoFonteInexistente", &[("fonte", fonte)])));
        };
        let conferida = texto_de(primeira, 0).unwrap_or_default();
        let mut esquema = Vec::with_capacity(linhas.len());
        for linha in &linhas {
            let Some(nome) = texto_de(linha, 1) else {
                continue;
            };
            esquema.push((nome, texto_de(linha, 2).unwrap_or_default()));
        }
        Ok((conferida, esquema))
    }
}

impl Motor for Mysql {
    fn fontes(&mut self) -> Result<Vec<String>> {
        let linhas = self.catalogo(FONTES_SQL, params_posicionais(&[]))?;
        Ok(linhas.iter().filter_map(|linha| texto_de(linha, 0)).collect())
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
        let teto = teto_de_leitura(limite);
        let sql = leitura_sql(&citadas.join(", "), &citado(&conferida), &onde, &ordenar, limite);
        let linhas = self.linhas_ate(sql.as_str(), params_posicionais(&params), teto)?;
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
        let onde = onde_sql(filtro, &esquema, &mut params)?;
        let sql = format!("SELECT COUNT(*) FROM {}{}", citado(&conferida), onde);
        let linhas = self.linhas_ate(sql.as_str(), params_posicionais(&params), 1)?;
        let Some(linha) = linhas.first() else {
            return Ok(0);
        };
        Ok(contagem_de(linha))
    }

    fn inserir(&mut self, fonte: &str, valores: &Map<String, Value>) -> Result<u64> {
        let (conferida, esquema) = self.esquema_de(fonte)?;
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
        self.escrita(sql.as_str(), params_posicionais(&params))
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
        self.escrita(sql.as_str(), params_posicionais(&params))
    }

    fn apagar(&mut self, fonte: &str, filtro: &Filtro) -> Result<u64> {
        let (conferida, esquema) = self.esquema_de(fonte)?;
        if filtro.is_empty() {
            bail!(tr("erros.bancoOndeObrigatorio", &[]));
        }
        let mut params = Vec::new();
        let onde = onde_sql(filtro, &esquema, &mut params)?;
        let sql = format!("DELETE FROM {}{}", citado(&conferida), onde);
        self.escrita(sql.as_str(), params_posicionais(&params))
    }

    fn sql(&mut self, declaracao: &str, limite: usize) -> Result<ResultadoSql> {
        let cronometro = Cronometro::agora();
        let declaracao = declaracao.trim();
        let declaracao = declaracao.strip_suffix(';').unwrap_or(declaracao).trim();
        if declaracao.is_empty() {
            bail!(tr("erros.bancoSqlVazio", &[]));
        }
        let runtime = self.runtime;
        let preparada = esperar(runtime, async { self.conexao.prep(declaracao).await.map_err(erro) })?;
        // O banco mesmo classifica: uma declaração que devolve colunas é
        // consulta; as outras, escrita. Nenhuma lista de palavras no meio.
        let colunas = preparada
            .columns()
            .iter()
            .map(|coluna| coluna.name_str().into_owned())
            .collect::<Vec<_>>();
        if colunas.is_empty() {
            let afetadas = esperar(runtime, async {
                let resultado = self
                    .conexao
                    .exec_iter(&preparada, mysql_async::Params::Empty)
                    .await
                    .map_err(erro)?;
                Ok(resultado.affected_rows())
            })?;
            return Ok(ResultadoSql::Afetadas(afetadas));
        }
        // A consulta volta embrulhada, com teto próprio: o banco ainda pode
        // ter linhas além do que a leitura levou, e isso é o que se diz. O
        // `*` aqui é sobre a declaração que o modelo escreveu e o banco descreveu
        // — alargar colunas é escolha de quem redigiu o SQL, não do leitor.
        let teto = teto_de_leitura(limite);
        let sql = format!("SELECT * FROM ({declaracao}) AS `__aether_consulta__` LIMIT {teto}");
        let saida = esperar(runtime, async {
            let mut resultado = self.conexao.query_iter(sql.as_str()).await.map_err(erro)?;
            let mut linhas = Vec::new();
            while linhas.len() < teto {
                let Some(linha) = resultado.next().await.map_err(erro)? else {
                    break;
                };
                let mut valores = Vec::with_capacity(colunas.len());
                for indice in 0..linha.len() {
                    valores.push(para_json(
                        linha.as_ref(indice).unwrap_or(&mysql_async::Value::NULL),
                    ));
                }
                linhas.push(valores);
            }
            Ok(linhas)
        })?;
        let (linhas, truncado) = aparar_no_teto(saida, limite);
        Ok(ResultadoSql::Consulta(Consulta {
            colunas,
            linhas,
            truncado,
            duracao_ms: cronometro.ms(),
        }))
    }
}

fn erro_abrir(e: mysql_async::Error) -> anyhow::Error {
    anyhow!(tr("erros.bancoAbrir", &[("detalhe", &e.to_string())]))
}

/// A única leitura da flag `tls_verificado` neste motor — e ela só sabe dizer
/// «não». O `mysql_async` deste aplicativo foi compilado com
/// `features = ["minimal"]`, que não traz TLS nenhum: não há opção a habilitar,
/// e abrir assim mesmo seria o dono marcar o canal conferido, ver a ligação
/// pegar, e continuar mandando senha e consulta em texto puro. O postgres tem
/// o mesmo driver sem adaptador, mas obedece à marca pedindo `SslMode::Require`
/// e deixando a handshake falhar; aqui não há nem isso, então a recusa vem
/// antes de tocar a rede.
fn conferir_canal(conexao: &Conexao) -> Result<()> {
    if conexao.tls_verificado {
        bail!(tr(
            "erros.bancoSemTls",
            &[("tipo", conexao.tipo.rotulo())]
        ));
    }
    Ok(())
}

fn erro(e: mysql_async::Error) -> anyhow::Error {
    anyhow!(tr("erros.bancoErro", &[("detalhe", &e.to_string())]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bancos::config::{conexao_de_teste, Tipo};
    use crate::bancos::motor::PRAZO_DE_LEITURA;

    /// O agendador é um só no processo, e nenhum motor pode montar o seu: a
    /// régua é o texto dos próprios arquivos, porque um runtime por chamada é
    /// justamente o que se quer impedir, e ele reaparece numa linha de qualquer
    /// um dos seis.
    #[test]
    fn nenhum_motor_monta_o_proprio_agendador() {
        let pasta = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("bancos")
            .join("motor");
        let mut acusados = Vec::new();
        // As agulhas são montadas em pedaços: escritas inteiras, elas seriam
        // achadas por esta própria régua no arquivo que as escreve.
        let um_fio = ["new_current", "_thread"].concat();
        let muitos_fios = ["Builder::new_multi", "_thread"].concat();
        for entrada in std::fs::read_dir(&pasta).expect("a pasta dos motores existe") {
            let caminho = entrada.expect("a entrada do diretório é legível").path();
            let nome = caminho
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if nome == "mod.rs" || !nome.ends_with(".rs") {
                continue;
            }
            let texto = std::fs::read_to_string(&caminho).expect("o motor é legível");
            for (numero, linha) in texto.lines().enumerate() {
                if linha.contains(&um_fio) || linha.contains(&muitos_fios) {
                    acusados.push(format!("{nome}:{}", numero + 1));
                }
            }
        }
        assert!(
            acusados.is_empty(),
            "defeito 1: estes motores montam um runtime próprio, um por chamada: {}",
            acusados.join(", ")
        );
    }

    /// O catálogo de uma fonte cabe numa declaração: tabela conferida e colunas
    /// na mesma junta. Duas strings separadas — a lista e depois as colunas — é
    /// o que fazia cada leitura pagar duas viagens antes de ler uma linha.
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
            ESQUEMA_SQL.contains("LOWER(t.table_name) = LOWER(?)"),
            "sem a conferência insensível no servidor, a fonte digitada em outra caixa sumiria"
        );
        assert!(
            !FONTES_SQL.contains("information_schema.columns"),
            "listar tabelas não descreve coluna nenhuma: é a segunda viagem que sobra aqui"
        );
    }

    /// O teto vai escrito no SELECT, e é `limite + 1`: sem a linha de barriga,
    /// uma leitura que encheu o teto é indistinguível de uma que achou o fim do
    /// banco, e o cartão diria «todas as linhas» sobre um corte.
    #[test]
    fn a_leitura_pede_uma_linha_de_barriga() {
        assert!(
            leitura_sql("`a`", "`t`", "", "", 20).ends_with(" LIMIT 21"),
            "defeito 5: sem a linha de barriga o corte vira fim de banco"
        );
        assert!(
            leitura_sql("`a`", "`t`", " WHERE `a` = ?", " ORDER BY `a` DESC", 0).ends_with(" LIMIT 1"),
            "limite zero ainda pergunta se havia linha"
        );
    }

    /// O prazo espera de verdade: o que passa dele volta como o erro do prazo, e
    /// não como resultado. A futura dorme mais que o prazo pedido — o único erro
    /// possível aqui é o relógio do motor, porque a tarefa devolve `Ok`.
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
            Ok(()) => panic!(
                "defeito 4: a ida voltou depois de passar do prazo — ninguém esperou nada"
            ),
        };
        assert!(
            erro.to_string().contains('1'),
            "a frase do prazo tem de dizer segundos: {erro}"
        );
    }

    /// A espera que os motores de rede partilham é um segundo acima do prazo do
    /// servidor, quando ele existe: o erro que chega primeiro tem de ser o do
    /// banco, que sabe dizer o que expirou.
    #[test]
    fn a_espera_do_cliente_fica_acima_do_prazo() {
        assert!(prazo_de_espera() > PRAZO_DE_LEITURA);
    }

    /// O cadastro promete: «cada motor que sabe falar TLS obedece; o que não
    /// sabe se recusa a fingir». Este driver foi compilado sem TLS nenhum,
    /// então a recusa é a única resposta honesta — abrir em texto puro com a
    /// marca ligada faria o dono acreditar num canal que não existe.
    ///
    /// `127.0.0.1` não é o servidor de ninguém: se a recusa da flag não vier
    /// antes do aperto de mão, o teste esbarra numa recusação local imediata e
    /// falha dizendoo, em vez de esperar por uma rede que não responde.
    #[test]
    fn a_marca_de_canal_conferido_nao_vira_texto_puro() {
        let mut conferida = conexao_de_teste(Tipo::Mysql);
        conferida.host = "127.0.0.1".into();
        conferida.tls_verificado = true;
        let erro = Mysql::abrir(&conferida)
            .err()
            .expect("com a marca ligada, a ligação tem de ser recusada");
        assert!(
            erro.to_string().contains("TLS"),
            "a recusa tem de dizer por quê: {erro}"
        );
    }

    /// Sem a marca, nada muda: a ligação de sempre segue para o driver, e a
    /// recusa não é um pedágio cobrado de quem não pediu canal conferido.
    #[test]
    fn sem_a_marca_o_mysql_fala_como_sempre() {
        conferir_canal(&conexao_de_teste(Tipo::Mysql)).expect("sem a marca, nada é negado");
    }
}
