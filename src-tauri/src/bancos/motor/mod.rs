//! Um motor por família de banco, atrás do mesmo traço.
//!
//! O que o conector promete ao modelo é o mesmo em todos: listar as fontes,
//! descrever as colunas, consultar com filtro e ordem, contar, e as três
//! escritas com a política de cada tabela. Cada motor traduz isso na gramática
//! do seu banco — e cada motor é responsável por duas regras que não podem
//! variar: valores sempre parametrizados, identificadores sempre conferidos
//! contra o esquema descoberto do banco (nunca interpolados do texto do modelo).

use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value};
use std::time::Instant;

use super::config::Conexao;

/// O que uma consulta devolve: colunas na ordem do banco, linhas como JSON e a
/// medida real da duração. `truncado` diz que o teto de linhas cortou antes do
/// fim — a leitura é limitada, não incompleta por erro.
#[derive(Debug, Clone, PartialEq)]
pub struct Consulta {
    pub colunas: Vec<String>,
    pub linhas: Vec<Vec<Value>>,
    pub truncado: bool,
    pub duracao_ms: u64,
}

pub enum ResultadoSql {
    Consulta(Consulta),
    Afetadas(u64),
}

/// As comparações que um filtro aceita. Vêm de palavras que o modelo escreve,
/// nunca de SQL — o motor que as traduz na sintaxe do banco.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Comparacao {
    Igual,
    Diferente,
    Maior,
    MaiorIgual,
    Menor,
    MenorIgual,
    Contendo,
}

impl Comparacao {
    pub fn de_rotulo(rotulo: &str) -> Option<Self> {
        match rotulo.trim().to_ascii_lowercase().as_str() {
            "igual" => Some(Self::Igual),
            "diferente" => Some(Self::Diferente),
            "maior" => Some(Self::Maior),
            "maiorigual" => Some(Self::MaiorIgual),
            "menor" => Some(Self::Menor),
            "menorigual" => Some(Self::MenorIgual),
            "contendo" => Some(Self::Contendo),
            _ => None,
        }
    }

    pub fn rotulo(&self) -> &'static str {
        match self {
            Self::Igual => "igual",
            Self::Diferente => "diferente",
            Self::Maior => "maior",
            Self::MaiorIgual => "maiorigual",
            Self::Menor => "menor",
            Self::MenorIgual => "menorigual",
            Self::Contendo => "contendo",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Condicao {
    pub coluna: String,
    pub comparacao: Comparacao,
    pub valor: Value,
}

/// Um filtro é uma conjunção: todas as condições precisam valer.
pub type Filtro = Vec<Condicao>;

/// O filtro chega do modelo como JSON: `{"col": valor}` para igualdade, ou
/// `{"col": {"op": "maior", "valor": 10}}` para o resto. Tudo o que não é isso
/// é recusado, não interpretado.
pub fn filtro_de(json: Option<&Value>) -> Result<Filtro> {
    let Some(json) = json else {
        return Ok(Vec::new());
    };
    let mapa = json
        .as_object()
        .ok_or_else(|| anyhow!(crate::idiomas::tr("erros.filtroInvalido", &[])))?;
    let mut filtro = Vec::new();
    for (coluna, condicao) in mapa {
        let (comparacao, valor) = match condicao {
            Value::Object(objeto) if objeto.len() == 1 => {
                let Some(Value::String(rotulo)) = objeto.get("op") else {
                    bail!(crate::idiomas::tr("erros.filtroInvalido", &[]));
                };
                let comparacao = Comparacao::de_rotulo(rotulo).ok_or_else(|| {
                    anyhow!(crate::idiomas::tr(
                        "erros.filtroComparacao",
                        &[("op", rotulo)]
                    ))
                })?;
                (comparacao, objeto.get("valor").cloned().unwrap_or(Value::Null))
            }
            outro => (Comparacao::Igual, outro.clone()),
        };
        filtro.push(Condicao {
            coluna: coluna.clone(),
            comparacao,
            valor,
        });
    }
    Ok(filtro)
}

pub fn filtro_vazio(filtro: &Filtro) -> bool {
    filtro.is_empty()
}

/// Mede de verdade o que uma consulta levou — o mesmo número vai ao cartão.
pub struct Cronometro {
    inicio: Instant,
}

impl Cronometro {
    pub fn agora() -> Self {
        Self { inicio: Instant::now() }
    }

    pub fn ms(&self) -> u64 {
        self.inicio.elapsed().as_millis() as u64
    }
}

/// O que o conector promete ao modelo, em qualquer família de banco. Síncrono
/// de propósito: quem chama roda numa tarefa bloqueante.
pub trait Motor: Send {
    /// Todas as tabelas (ou coleções) que o banco tem — a lista que a tela usa
    /// para o dono escolher o que expor. Não confere política: política é
    /// trabalho do conector, o motor vê o banco inteiro.
    fn fontes(&mut self) -> Result<Vec<String>>;

    /// Colunas e o tipo que o banco declara, na ordem delas.
    fn colunas(&mut self, fonte: &str) -> Result<Vec<(String, String)>>;

    fn consultar(
        &mut self,
        fonte: &str,
        colunas: &[String],
        filtro: &Filtro,
        ordem: Option<(&str, bool)>,
        limite: usize,
    ) -> Result<Consulta>;

    fn contar(&mut self, fonte: &str, filtro: &Filtro) -> Result<u64>;

    fn inserir(&mut self, fonte: &str, valores: &Map<String, Value>) -> Result<u64>;

    fn atualizar(&mut self, fonte: &str, valores: &Map<String, Value>, filtro: &Filtro)
    -> Result<u64>;

    fn apagar(&mut self, fonte: &str, filtro: &Filtro) -> Result<u64>;

    /// A declaração escrita pelo modelo, só quando a conexão tem SQL livre.
    /// Uma declaração que devolve linhas volta como consulta; as outras voltam
    /// com o número de linhas afetadas que o banco reportou.
    fn sql(&mut self, declaracao: &str, limite: usize) -> Result<ResultadoSql>;
}

/// Abre a conexão no motor da família. Uma conexão por chamada: ferramentas de
/// banco são episódicas, e não deixar soquete vivo é mais barato que geri-lo.
pub fn abrir(conexao: &Conexao) -> Result<Box<dyn Motor>> {
    match conexao.tipo {
        super::config::Tipo::Sqlite => Ok(Box::new(sqlite::Sqlite::abrir(conexao)?)),
        super::config::Tipo::Postgres => Ok(Box::new(postgres::Postgres::abrir(conexao)?)),
        super::config::Tipo::Mysql => Ok(Box::new(mysql::Mysql::abrir(conexao)?)),
        super::config::Tipo::Mssql => Ok(Box::new(mssql::Mssql::abrir(conexao)?)),
        super::config::Tipo::Mongo => Ok(Box::new(mongo::Mongo::abrir(conexao)?)),
        super::config::Tipo::Oracle => Ok(Box::new(oracle::Oracle::abrir(conexao)?)),
    }
}

pub mod mongo;
pub mod mssql;
pub mod mysql;
pub mod oracle;
pub mod postgres;
pub mod sqlite;
