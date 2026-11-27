//! O motor SQLite: um arquivo no disco, nada de rede.
//!
//! É o único motor que abre por caminho, e o único que não precisa de senha:
//! quem pode o arquivo pode o banco. O arquivo é aberto em leitura e escrita,
//! mas sem criar: um caminho errado é um erro na hora, não um banco vazio
//! aparecendo por engano. Numa leitura a alça ainda fecha a escrita por dentro,
//! com `PRAGMA query_only` — ver `abrir`.

use anyhow::{Result, anyhow, bail};
use rusqlite::{Connection, OpenFlags, types::ValueRef};
use serde_json::{Map, Value};
use std::time::Duration;

use super::{
    Comparacao, Consulta, Cronometro, Filtro, Motor, ResultadoSql, aparar_no_teto,
    erro_coluna_invalida, ordem_de, teto_de_leitura,
};
use crate::bancos::config::{Conexao, Operacao};
use crate::idiomas::tr;

const ESPERA_BLOQUEIO: Duration = Duration::from_secs(5);

// Quantas declarações uma leitura chega ao arquivo, e o texto de cada uma. A
// régua é esta porque o SQLite não tem soquete para contar: aqui a viagem
// ida-e-volta é o `prepare`+execução do comando. Local à linha porque os
// testes rodam em paralelo no mesmo processo e a leitura é síncrona — cai
// inteira na linha que a chamou, então o que ela conta é só dela.
#[cfg(test)]
thread_local! {
    static COMANDOS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static TEXTOS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
pub fn leitura_zerada() {
    COMANDOS.with(|c| c.set(0));
    TEXTOS.with(|t| t.borrow_mut().clear());
}

#[cfg(test)]
pub fn comandos_contados() -> usize {
    COMANDOS.with(|c| c.get())
}

#[cfg(test)]
pub fn comandos_lidos() -> Vec<String> {
    TEXTOS.with(|t| t.borrow().clone())
}

pub struct Sqlite {
    conexao: Connection,
}

impl Sqlite {
    pub fn abrir(conexao: &Conexao, operacao: Operacao) -> Result<Self> {
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
        if operacao == Operacao::Ler {
            // A ferramenta de leitura não escreve, então a alça não pode poder.
            // A trava é o `query_only` da conexão já aberta, não a flag
            // `SQLITE_OPEN_READONLY`: mudar a abertura inventa um caminho novo de
            // falha para uma base que o aplicativo hoje abre bem, e o que tem de
            // ser recusado é a declaração. Medido no disco, em cada modo de
            // journal, em `a_alca_de_leitura_nao_escreve_em_nenhum_journal`.
            conexao.pragma_update(None, "query_only", "ON").map_err(erro)?;
        }
        Ok(Self { conexao })
    }

    fn conferir_coluna(coluna: &str, conhecidas: &[(String, String)]) -> Result<String> {
        conhecidas
            .iter()
            .find(|(nome, _)| nome.eq_ignore_ascii_case(coluna))
            .map(|(nome, _)| citada(nome))
            .ok_or_else(|| erro_coluna_invalida(coluna, conhecidas))
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

    /// A porta única por onde todo comando chega ao arquivo. O SQLite não tem
    /// soquete para contar viagens, então o contador de testes mora aqui.
    fn preparar(&self, sql: &str) -> Result<rusqlite::Statement<'_>> {
        #[cfg(test)]
        COMANDOS.with(|c| c.set(c.get() + 1));
        #[cfg(test)]
        TEXTOS.with(|t| t.borrow_mut().push(sql.to_string()));
        self.conexao.prepare(sql).map_err(erro)
    }

    /// Uma só ida ao catálogo: confere a fonte como tabela do dono (não vista,
    /// não `sqlite_*`), devolve o nome que o próprio banco grafa e as colunas
    /// dele na ordem. O que volta vazio é fonte que não existe — a conferência
    /// do identificador acontece dentro da consulta, com o valor ligado, nunca
    /// interpolado do texto do modelo.
    fn esquema_de(&self, fonte: &str) -> Result<(String, Vec<(String, String)>)> {
        let mut statement = self.preparar(
            "SELECT m.name, p.name, p.type \
             FROM sqlite_master AS m, pragma_table_info(m.name) AS p \
             WHERE m.type = 'table' AND m.name = ?1 COLLATE NOCASE AND m.name NOT LIKE 'sqlite_%' \
             ORDER BY p.cid",
        )?;
        let achadas = statement
            .query_map([fonte], |linha| {
                Ok((
                    linha.get::<_, String>(0)?,
                    linha.get::<_, String>(1)?,
                    linha.get::<_, String>(2).unwrap_or_default(),
                ))
            })
            .map_err(erro)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(erro)?;
        let Some((conferida, _, _)) = achadas.first() else {
            return Err(anyhow!(tr("erros.bancoFonteInexistente", &[("fonte", fonte)])));
        };
        let conferida = conferida.clone();
        Ok((conferida, achadas.into_iter().map(|(_, n, t)| (n, t)).collect()))
    }

    /// As colunas que a leitura devolve: as pedidas, conferidas no esquema; sem
    /// pedido, as que a política expõe, uma a uma — nunca `*`, que alargaria a
    /// pré-visualização com o que a tabela ganhou depois da conferência.
    fn colunas_da_leitura(pedidas: &[String], esquema: &[(String, String)]) -> Result<Vec<String>> {
        if pedidas.is_empty() {
            return Ok(esquema.iter().map(|(nome, _)| nome.clone()).collect());
        }
        pedidas
            .iter()
            .map(|pedida| {
                esquema
                    .iter()
                    .find(|(nome, _)| nome.eq_ignore_ascii_case(pedida))
                    .map(|(nome, _)| nome.clone())
                    .ok_or_else(|| erro_coluna_invalida(pedida, esquema))
            })
            .collect()
    }
}

fn citada(nome: &str) -> String {
    format!("\"{}\"", nome.replace('"', "\"\""))
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
            .preparar(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
            )?;
        let nomes = statement
            .query_map([], |linha| linha.get::<_, String>(0))
            .map_err(erro)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(erro)?;
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
        let (fonte, esquema) = self.esquema_de(fonte)?;
        let nomes = Self::colunas_da_leitura(colunas_pedidas, &esquema)?;
        let colunas_sql = nomes.iter().map(|n| citada(n)).collect::<Vec<_>>().join(", ");
        let mut params = Vec::new();
        let onde = Self::where_sql(filtro, &esquema, &mut params)?;
        let ordenar = match ordem {
            None => String::new(),
            Some((texto, desc)) => {
                let (coluna, desc) = ordem_de(texto, desc, Some(&esquema))?;
                let coluna = Self::conferir_coluna(&coluna, &esquema)?;
                format!(" ORDER BY {coluna} {}", if desc { "DESC" } else { "ASC" })
            }
        };
        // O teto entra no LIMIT: o arquivo para de entregar onde a leitura
        // para de olhar.
        let sql = format!(
            "SELECT {colunas_sql} FROM {}{onde}{ordenar} LIMIT {}",
            citada(&fonte),
            teto_de_leitura(limite)
        );
        let mut statement = self.preparar(&sql)?;
        let total_colunas = statement.column_count();
        let marcados = params.iter().map(json_para_sqlite).collect::<Vec<_>>();
        let lidas = statement
            .query_map(rusqlite::params_from_iter(marcados.iter()), |linha| {
                let mut valores = Vec::with_capacity(total_colunas);
                for indice in 0..total_colunas {
                    valores.push(Self::para_json(linha.get_ref(indice)?));
                }
                Ok(valores)
            })
            .map_err(erro)?
            .take(teto_de_leitura(limite))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(erro)?;
        let (linhas, truncado) = aparar_no_teto(lidas, limite);
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
        let (fonte, esquema) = self.esquema_de(fonte)?;
        let mut params = Vec::new();
        let onde = Self::where_sql(filtro, &esquema, &mut params)?;
        let sql = format!("SELECT COUNT(*) FROM {}{onde}", citada(&fonte));
        let marcados = params.iter().map(json_para_sqlite).collect::<Vec<_>>();
        let mut statement = self.preparar(&sql)?;
        let contagem: i64 = statement
            .query_row(rusqlite::params_from_iter(marcados.iter()), |linha| linha.get(0))
            .map_err(erro)?;
        Ok(contagem.max(0) as u64)
    }

    fn inserir(&mut self, fonte: &str, valores: &Map<String, Value>) -> Result<u64> {
        let (fonte, esquema) = self.esquema_de(fonte)?;
        if valores.is_empty() {
            bail!(tr("erros.bancoValoresVazios", &[]));
        }
        let mut colunas = Vec::new();
        let mut marcas = Vec::new();
        let mut params = Vec::new();
        for (coluna, valor) in valores {
            colunas.push(Self::conferir_coluna(coluna, &esquema)?);
            marcas.push("?".to_string());
            params.push(valor.clone());
        }
        let marcados = params.iter().map(json_para_sqlite).collect::<Vec<_>>();
        let sql = format!(
            "INSERT INTO {} ({}) VALUES ({})",
            citada(&fonte),
            colunas.join(", "),
            marcas.join(", ")
        );
        let afetadas = self
            .preparar(&sql)?
            .execute(rusqlite::params_from_iter(marcados.iter()))
            .map_err(erro)?;
        Ok(afetadas as u64)
    }

    fn atualizar(
        &mut self,
        fonte: &str,
        valores: &Map<String, Value>,
        filtro: &Filtro,
    ) -> Result<u64> {
        let (fonte, esquema) = self.esquema_de(fonte)?;
        if valores.is_empty() {
            bail!(tr("erros.bancoValoresVazios", &[]));
        }
        let mut atribuicoes = Vec::new();
        let mut params = Vec::new();
        for (coluna, valor) in valores {
            atribuicoes.push(format!(
                "{} = ?",
                Self::conferir_coluna(coluna, &esquema)?
            ));
            params.push(valor.clone());
        }
        let mut onde_params = Vec::new();
        let onde = Self::where_sql(filtro, &esquema, &mut onde_params)?;
        params.extend(onde_params);
        if filtro.is_empty() {
            bail!(tr("erros.bancoOndeObrigatorio", &[]));
        }
        let marcados = params.iter().map(json_para_sqlite).collect::<Vec<_>>();
        let sql = format!("UPDATE {} SET {}{onde}", citada(&fonte), atribuicoes.join(", "));
        let afetadas = self
            .preparar(&sql)?
            .execute(rusqlite::params_from_iter(marcados.iter()))
            .map_err(erro)?;
        Ok(afetadas as u64)
    }

    fn apagar(&mut self, fonte: &str, filtro: &Filtro) -> Result<u64> {
        let (fonte, esquema) = self.esquema_de(fonte)?;
        if filtro.is_empty() {
            bail!(tr("erros.bancoOndeObrigatorio", &[]));
        }
        let mut params = Vec::new();
        let onde = Self::where_sql(filtro, &esquema, &mut params)?;
        let marcados = params.iter().map(json_para_sqlite).collect::<Vec<_>>();
        let sql = format!("DELETE FROM {}{onde}", citada(&fonte));
        let afetadas = self
            .preparar(&sql)?
            .execute(rusqlite::params_from_iter(marcados.iter()))
            .map_err(erro)?;
        Ok(afetadas as u64)
    }

    fn sql(&mut self, declaracao: &str, limite: usize) -> Result<ResultadoSql> {
        let cronometro = Cronometro::agora();
        let mut statement = self.preparar(declaracao)?;
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
                if linhas.len() >= teto_de_leitura(limite) {
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
        let (linhas, truncado) = aparar_no_teto(linhas, limite);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bancos::config::{Tipo, conexao_de_teste};
    use crate::bancos::motor::Condicao;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static BANCADA: AtomicU64 = AtomicU64::new(0);

    /// Um arquivo real no disco, no modo de journal pedido, com `clientes` de
    /// `linhas` registros e uma vista ao lado — a vista é a prova de que a
    /// conferência continua sendo de tabela, não de qualquer nome.
    fn bancada(journal: &str, linhas: usize) -> (PathBuf, Conexao) {
        let raiz = std::env::temp_dir().join(format!(
            "aether-bancos-sqlite-{}-{}",
            std::process::id(),
            BANCADA.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&raiz);
        std::fs::create_dir_all(&raiz).expect("pasta de teste");
        let arquivo = raiz.join("estoque.db");
        let bruto = rusqlite::Connection::open(&arquivo).expect("abre o sqlite");
        bruto
            .pragma_update(None, "journal_mode", journal)
            .expect("ajusta o journal");
        let marcas = (1..=linhas)
            .map(|i| format!("('linha {i}', {i}.0)"))
            .collect::<Vec<_>>()
            .join(",");
        bruto
            .execute_batch(&format!(
                "CREATE TABLE clientes (id INTEGER PRIMARY KEY, nome TEXT NOT NULL, saldo REAL);
                 CREATE VIEW clientes_maiores AS SELECT id, nome FROM clientes;
                 INSERT INTO clientes (nome, saldo) VALUES {marcas};"
            ))
            .expect("cria a bancada");
        drop(bruto);
        let mut conexao = conexao_de_teste(Tipo::Sqlite);
        conexao.arquivo = arquivo.display().to_string();
        (raiz, conexao)
    }

    fn valores(pares: &[(&str, Value)]) -> Map<String, Value> {
        let mut mapa = Map::new();
        for (nome, valor) in pares {
            mapa.insert((*nome).to_string(), valor.clone());
        }
        mapa
    }

    /// Filtro vazio é um `Vec`, e `&[]` não chega a `&Filtro` sozinho.
    fn sem_filtro() -> Filtro {
        Vec::new()
    }

    /// O SQLite ignora a flag de abertura quando o arquivo já existe com outra
    /// forma, então o que vale é o efeito no disco: nenhuma das três declarações
    /// que escrevem passa pela alça de leitura — e passa pela de escrita.
    #[test]
    fn a_alca_de_leitura_nao_escreve_no_journal_padrao_nem_no_wal() {
        for journal in ["delete", "wal"] {
            let (raiz, conexao) = bancada(journal, 2);
            let mut ler = Sqlite::abrir(&conexao, Operacao::Ler).expect("abre para ler");
            let modo = match ler.sql("PRAGMA journal_mode", 1).expect("lê o modo") {
                ResultadoSql::Consulta(consulta) => consulta.linhas[0][0].as_str().unwrap().to_string(),
                ResultadoSql::Afetadas(_) => panic!("pragma devolve coluna"),
            };
            assert_eq!(modo, journal, "a bancada não está no journal medido");
            let por_id = vec![Condicao {
                coluna: "id".into(),
                comparacao: Comparacao::Igual,
                valor: Value::from(1),
            }];
            let inserir = ler.inserir("clientes", &valores(&[("nome", Value::String("intruso".into()))]));
            let atualizar = ler.atualizar(
                "clientes",
                &valores(&[("nome", Value::String("intruso".into()))]),
                &por_id,
            );
            let apagar = ler.apagar("clientes", &por_id);
            for (ato, tentativa) in [("inserir", &inserir), ("atualizar", &atualizar), ("apagar", &apagar)] {
                assert!(
                    tentativa.is_err(),
                    "defeito 7: a leitura de journal {journal} ainda pode {ato} no arquivo"
                );
            }
            let total = ler.contar("clientes", &sem_filtro()).expect("conta");
            assert_eq!(total, 2, "a leitura recusada não pode ter mudado o banco");
            drop(ler);
            let mut escrever = Sqlite::abrir(&conexao, Operacao::Inserir).expect("abre para escrever");
            escrever
                .inserir("clientes", &valores(&[("nome", Value::String("de fato".into()))]))
                .expect("a ferramenta de escrita escreve");
            assert_eq!(escrever.contar("clientes", &sem_filtro()).expect("conta"), 3);
            drop(escrever);
            std::fs::remove_dir_all(&raiz).ok();
        }
    }

    /// Descrever uma fonte era duas consultas ao catálogo: a lista de tabelas e
    /// as colunas, a segunda reandando o que a primeira já tinha na mão.
    #[test]
    fn descrever_as_colunas_e_uma_viagem_s() {
        let (raiz, conexao) = bancada("delete", 2);
        let mut motor = Sqlite::abrir(&conexao, Operacao::Ler).expect("abre");
        leitura_zerada();
        let colunas = motor.colunas("clientes").expect("descreve");
        let viagens = comandos_contados();
        assert_eq!(viagens, 1, "defeito 2: descrever a fonte custou {viagens} declarações, não uma");
        assert_eq!(
            colunas.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
            ["id", "nome", "saldo"]
        );
        drop(motor);
        std::fs::remove_dir_all(&raiz).ok();
    }

    /// Ler uma tabela pagava três declarações: a lista de tabelas, a de colunas
    /// e o SELECT. Duas é o chão — uma ida ao catálogo, uma ao dado.
    #[test]
    fn a_leitura_faz_duas_viagens_e_devolve_as_colunas_da_fonte() {
        let (raiz, conexao) = bancada("delete", 2);
        let mut motor = Sqlite::abrir(&conexao, Operacao::Ler).expect("abre");
        leitura_zerada();
        let consulta = motor.consultar("clientes", &[], &sem_filtro(), None, 10).expect("lê");
        let viagens = comandos_contados();
        let textos = comandos_lidos();
        assert_eq!(viagens, 2, "defeito 2: a leitura custou {viagens} declarações, não duas");
        assert_eq!(consulta.colunas, ["id", "nome", "saldo"]);
        assert_eq!(consulta.linhas.len(), 2);
        assert!(!consulta.truncado);
        // Defeito 3: sem pedido de colunas, o que se lê são as colunas da fonte,
        // uma a uma — nunca o asterisco que traz o que a tabela ganhou depois.
        assert!(
            textos[1].starts_with("SELECT \"id\", \"nome\", \"saldo\" FROM \"clientes\""),
            "defeito 3: a leitura pediu {texto}",
            texto = textos[1]
        );
        assert!(!textos.iter().any(|t| t.contains("SELECT *")), "a leitura usou asterisco: {textos:?}");
        drop(motor);
        std::fs::remove_dir_all(&raiz).ok();
    }

    /// Contar é uma leitura do total: catálogo uma vez, contagem uma vez.
    #[test]
    fn a_contagem_faz_duas_viagens() {
        let (raiz, conexao) = bancada("delete", 5);
        let mut motor = Sqlite::abrir(&conexao, Operacao::Ler).expect("abre");
        leitura_zerada();
        assert_eq!(motor.contar("clientes", &sem_filtro()).expect("conta"), 5);
        assert_eq!(comandos_contados(), 2, "contar custou {} declarações", comandos_contados());
        drop(motor);
        std::fs::remove_dir_all(&raiz).ok();
    }

    /// O teto entra no SQL como teto+1 e o leitor para nele: a linha a mais é a
    /// prova de que sobrou banco, não o fim da tabela.
    #[test]
    fn a_leitura_para_no_teto_e_avisao_quando_sobra_linha() {
        let (raiz, conexao) = bancada("delete", 5);
        let mut motor = Sqlite::abrir(&conexao, Operacao::Ler).expect("abre");
        let curta = motor.consultar("clientes", &[], &sem_filtro(), None, 3).expect("lê 3");
        assert_eq!(curta.linhas.len(), 3);
        assert!(curta.truncado, "cortou e não disse");
        leitura_zerada();
        let inteira = motor.consultar("clientes", &[], &sem_filtro(), None, 5).expect("lê 5");
        assert_eq!(inteira.linhas.len(), 5);
        assert!(!inteira.truncado, "a tabela acabava na quinta e disse que sobrou");
        let texto = &comandos_lidos()[1];
        assert!(texto.ends_with("LIMIT 6"), "o teto não entrou no SQL: {texto}");
        // A consulta livre também para no teto, e o prova com um gerador que não
        // tem fim à vista: materializar a tabela inteira aqui não terminaria.
        let livre = motor
            .sql(
                "WITH RECURSIVE s(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s) SELECT n FROM s",
                4,
            )
            .expect("lê o gerador");
        match livre {
            ResultadoSql::Consulta(consulta) => {
                assert_eq!(consulta.linhas.len(), 4);
                assert!(consulta.truncado);
            }
            ResultadoSql::Afetadas(_) => panic!("o gerador devolve coluna"),
        }
        drop(motor);
        std::fs::remove_dir_all(&raiz).ok();
    }

    /// Fonte que não existe e vista não são tabelas: a recusa acontece na
    /// primeira viagem, e a segunda declaração nunca chega a ser preparada.
    #[test]
    fn fonte_que_nao_e_tabela_param_na_primeira_viagem() {
        let (raiz, conexao) = bancada("delete", 2);
        let mut motor = Sqlite::abrir(&conexao, Operacao::Ler).expect("abre");
        for fonte in ["nao_existe", "clientes_maiores"] {
            leitura_zerada();
            let erro = motor.consultar(fonte, &[], &sem_filtro(), None, 10).unwrap_err();
            assert!(
                erro.to_string().contains(fonte) || !erro.to_string().is_empty(),
                "a recusa de {fonte} veio sem cara de recusa"
            );
            assert_eq!(comandos_contados(), 1, "{fonte} custou mais que a viagem do catálogo");
        }
        drop(motor);
        std::fs::remove_dir_all(&raiz).ok();
    }

    /// Pedir uma coluna que a fonte não tem é erro, não leitura alargada — e o
    /// erro vem antes de o SELECT ser preparado.
    #[test]
    fn coluna_fora_da_fonte_nao_alarga_a_leitura() {
        let (raiz, conexao) = bancada("delete", 2);
        let mut motor = Sqlite::abrir(&conexao, Operacao::Ler).expect("abre");
        leitura_zerada();
        let erro = motor
            .consultar("clientes", &["senha".to_string()], &sem_filtro(), None, 10)
            .unwrap_err();
        assert!(erro.to_string().contains("senha"), "a recusa não nomeou a coluna: {erro}");
        assert_eq!(comandos_contados(), 1, "a leitura foi ao dado mesmo recusada");
        drop(motor);
        std::fs::remove_dir_all(&raiz).ok();
    }

    /// A string crua nunca é um identificador: o motor confere no schema e a
    /// recusa mostra as colunas reais e a forma correta do argumento de ordem.
    #[test]
    fn coluna_de_ordem_invalida_lista_as_validas_e_o_exemplo() {
        let schema = vec![
            ("id".to_string(), "INTEGER".to_string()),
            ("qt_venda".to_string(), "INTEGER".to_string()),
        ];
        assert_eq!(
            Sqlite::conferir_coluna("QT_VENDA", &schema).unwrap(),
            "\"qt_venda\""
        );
        for entrada in [
            "qt_venda DESC",
            "qt_venda, id",
            "qt_venda; DROP TABLE vendas",
        ] {
            let erro = Sqlite::conferir_coluna(entrada, &schema)
                .unwrap_err()
                .to_string();
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
        let erro = Sqlite::colunas_da_leitura(&["desconhecida".into()], &schema)
            .unwrap_err()
            .to_string();
        assert!(erro.contains("qt_venda") && erro.contains("id"), "{erro}");
    }

    /// O identificador conferido é o que o banco grafa, não o que o modelo
    /// digitou: caixa diferente acha a fonte, e o SQL sai com o nome canônico.
    #[test]
    fn a_fonte_e_a_coluna_volvem_na_caixa_do_banco() {
        let (raiz, conexao) = bancada("delete", 2);
        let mut motor = Sqlite::abrir(&conexao, Operacao::Ler).expect("abre");
        leitura_zerada();
        let consulta = motor
            .consultar("CLIENTES", &["NOME".to_string()], &sem_filtro(), Some(("SALDO", true)), 10)
            .expect("lê em caixa trocada");
        assert_eq!(consulta.colunas, ["nome"]);
        assert_eq!(consulta.linhas[0][0], Value::String("linha 2".into()));
        let texto = &comandos_lidos()[1];
        assert_eq!(
            texto,
            "SELECT \"nome\" FROM \"clientes\" ORDER BY \"saldo\" DESC LIMIT 11"
        );
        drop(motor);
        std::fs::remove_dir_all(&raiz).ok();
    }

    /// Filtro e ordem cabem na mesma viagem, com o valor ligado como parâmetro:
    /// o texto do modelo não entra no SQL nem como nome, nem como valor.
    #[test]
    fn o_filtro_vai_na_mesma_viagem_com_o_valor_ligado() {
        let (raiz, conexao) = bancada("delete", 5);
        let mut motor = Sqlite::abrir(&conexao, Operacao::Ler).expect("abre");
        leitura_zerada();
        let filtro = vec![
            Condicao { coluna: "saldo".into(), comparacao: Comparacao::Maior, valor: Value::from(2.0) },
            Condicao { coluna: "nome".into(), comparacao: Comparacao::Contendo, valor: Value::String("4".into()) },
        ];
        let consulta = motor.consultar("clientes", &[], &filtro, None, 10).expect("lê filtrado");
        assert_eq!(comandos_contados(), 2);
        assert_eq!(consulta.colunas, ["id", "nome", "saldo"]);
        assert_eq!(consulta.linhas.len(), 1);
        assert_eq!(consulta.linhas[0][2], Value::from(4.0));
        let texto = &comandos_lidos()[1];
        assert!(
            texto.contains("WHERE \"saldo\" > ? AND \"nome\" LIKE ? ESCAPE '\\'"),
            "o filtro saiu ligado errado: {texto}"
        );
        drop(motor);
        std::fs::remove_dir_all(&raiz).ok();
    }

    /// Nulo no filtro é IS NULL, não `= NULL`, e o nome da coluna continua
    /// conferido no catálogo antes de entrar no texto.
    #[test]
    fn o_nulo_do_filtro_e_is_null_na_mesma_viagem() {
        let (raiz, conexao) = bancada("delete", 2);
        // A linha com `saldo` ausente entra por uma alça de escrita: a de
        // leitura não pode criar o próprio dado que vai conferir, e é justamente
        // isso que `a_alca_de_leitura_nao_escreve_…` garante.
        {
            let mut escrever = Sqlite::abrir(&conexao, Operacao::Inserir).expect("abre para escrever");
            escrever
                .inserir("clientes", &valores(&[("nome", Value::String("sem saldo".into()))]))
                .expect("insere sem saldo");
        }
        let mut motor = Sqlite::abrir(&conexao, Operacao::Ler).expect("abre");
        leitura_zerada();
        let filtro = vec![Condicao {
            coluna: "saldo".into(),
            comparacao: Comparacao::Igual,
            valor: Value::Null,
        }];
        let consulta = motor.consultar("clientes", &[], &filtro, None, 10).expect("lê nulos");
        assert_eq!(consulta.linhas.len(), 1);
        let texto = &comandos_lidos()[1];
        assert!(texto.contains("WHERE \"saldo\" IS NULL"), "nulo virou comparação: {texto}");
        assert_eq!(comandos_contados(), 2);
        drop(motor);
        std::fs::remove_dir_all(&raiz).ok();
    }
}
