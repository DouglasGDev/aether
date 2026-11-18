//! O motor MongoDB: coleções de documentos em vez de tabelas de linhas.
//!
//! O que vale como "coluna" aqui é a união das chaves de uma amostra dos
//! documentos — coleção não tem esquema declarado, e fingir que tem mentiria. O
//! mesmo vale para tipos: cada campo carrega o tipo que a amostra viu primeiro,
//! e um mesmo campo pode ser número num documento e texto noutro. O nome de
//! campo é conferido por regra própria (nunca `$`, que é operador do Mongo, não
//! identificador) — não por SQL, porque aqui não há SQL: os filtros viram
//! consultas BSON pela API do driver, sem interpolar texto em nenhum ponto.

use anyhow::{Result, anyhow, bail};
use mongodb::{
    Client,
    bson::{Bson, Document as DocBson, doc},
    options::{ClientOptions, Credential, ServerAddress},
};
use serde_json::{Map, Value};
use std::time::Duration;

use super::{Comparacao, Consulta, Cronometro, Filtro, Motor, ResultadoSql};
use crate::bancos::config::Conexao;
use crate::idiomas::tr;

/// Quantos documentos entram na amostra que define as "colunas" da coleção.
const AMOSTRA: usize = 100;

pub struct Mongo {
    runtime: tokio::runtime::Runtime,
    cliente: Client,
    banco: String,
}

/// Nome de campo que o Mongo aceita como identificador: não vazio e sem `$`
/// inicial. Tudo que começa com `$` é operador da gramática do Mongo — aceitar
/// do modelo seria deixar a consulta dizer comandos, não dados.
fn conferir_campo(nome: &str) -> Result<&str> {
    let nome = nome.trim();
    if nome.is_empty() || nome.starts_with('$') {
        bail!(tr("erros.bancoColunaInexistente", &[("coluna", nome)]));
    }
    Ok(nome)
}

fn conferir_colecao<'a>(nome: &str, conhecidas: &'a [String]) -> Result<&'a str> {
    conhecidas
        .iter()
        .find(|c| c.as_str() == nome)
        .map(|c| c.as_str())
        .ok_or_else(|| anyhow!(tr("erros.bancoFonteInexistente", &[("fonte", nome)])))
}

/// Regex do "contendo": o que o modelo escreveu é literal, não padrão que ele
/// improvisa — todo metacaractere vira literal escapado.
fn escapar_regex(texto: &str) -> String {
    let mut saida = String::with_capacity(texto.len());
    for caractere in texto.chars() {
        if "\\.^$|?*+()[]{}-".contains(caractere) {
            saida.push('\\');
        }
        saida.push(caractere);
    }
    saida
}

fn valor_texto(valor: &Value) -> Result<String> {
    match valor {
        Value::String(texto) => Ok(texto.clone()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        _ => bail!(tr("erros.filtroValorInvalido", &[])),
    }
}

/// O filtro vira consulta BSON pela API do driver. Nulo compara com null /
/// $ne null, que no Mongo cobre tanto campo ausente quanto nulo.
fn filtro_bson(filtro: &Filtro) -> Result<DocBson> {
    let mut consulta = DocBson::new();
    for condicao in filtro {
        let campo = conferir_campo(&condicao.coluna)?;
        let pedaco = match condicao.comparacao {
            Comparacao::Igual => {
                if condicao.valor.is_null() {
                    Bson::Null
                } else {
                    json_para_bson(&condicao.valor)
                }
            }
            Comparacao::Diferente => {
                if condicao.valor.is_null() {
                    Bson::Document(doc! { "$ne": Bson::Null })
                } else {
                    Bson::Document(doc! { "$ne": json_para_bson(&condicao.valor) })
                }
            }
            Comparacao::Contendo => {
                let cru = valor_texto(&condicao.valor)?;
                Bson::Document(doc! { "$regex": Bson::String(escapar_regex(&cru)) })
            }
            operador => {
                if condicao.valor.is_null() {
                    bail!(tr("erros.filtroNuloSemComparacao", &[]));
                }
                let rotulo = match operador {
                    Comparacao::Maior => "$gt",
                    Comparacao::MaiorIgual => "$gte",
                    Comparacao::Menor => "$lt",
                    Comparacao::MenorIgual => "$lte",
                    _ => unreachable!("conferido acima"),
                };
                let mut pedaco = DocBson::new();
                pedaco.insert(rotulo, json_para_bson(&condicao.valor));
                Bson::Document(pedaco)
            }
        };
        consulta.insert(campo, pedaco);
    }
    Ok(consulta)
}

fn json_para_bson(valor: &Value) -> Bson {
    match valor {
        Value::Null => Bson::Null,
        Value::Bool(b) => Bson::Boolean(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Bson::Int64(i)
            } else {
                n.as_f64().map(Bson::Double).unwrap_or(Bson::Null)
            }
        }
        Value::String(texto) => Bson::String(texto.clone()),
        Value::Array(lista) => Bson::Array(lista.iter().map(json_para_bson).collect()),
        // Um objeto que o modelo manda como valor entra como subdocumento.
        Value::Object(objeto) => Bson::Document(
            objeto
                .iter()
                .map(|(chave, valor)| (chave.clone(), json_para_bson(valor)))
                .collect(),
        ),
    }
}

/// O `_id` que o modelo manda como texto hexadecimal de 24 caracteres é um
/// ObjectId de verdade; do contrário entra como está e o Mongo gera o seu.
fn documento_de(valores: &Map<String, Value>) -> Result<DocBson> {
    if valores.is_empty() {
        bail!(tr("erros.bancoValoresVazios", &[]));
    }
    let mut documento = DocBson::new();
    for (chave, valor) in valores {
        let chave = conferir_campo(chave)?;
        if chave == "_id" {
            if let Value::String(hex) = valor {
                if hex.len() == 24 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
                    if let Ok(oid) = mongodb::bson::oid::ObjectId::parse_str(hex) {
                        documento.insert("_id", Bson::ObjectId(oid));
                        continue;
                    }
                }
            }
        }
        documento.insert(chave, json_para_bson(valor));
    }
    Ok(documento)
}

fn bson_para_json(valor: &Bson) -> Value {
    match valor {
        Bson::Double(d) => serde_json::Number::from_f64(*d)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        Bson::Int32(i) => Value::Number((*i).into()),
        Bson::Int64(i) => Value::Number((*i).into()),
        Bson::String(texto) | Bson::Symbol(texto) => Value::String(texto.clone()),
        Bson::Boolean(b) => Value::Bool(*b),
        Bson::Null | Bson::Undefined | Bson::MinKey | Bson::MaxKey => Value::Null,
        Bson::ObjectId(oid) => Value::String(oid.to_hex()),
        Bson::DateTime(data) => Value::String(
            data.try_to_rfc3339_string()
                .unwrap_or_else(|_| format!("[data {} ms]", data.timestamp_millis())),
        ),
        // Binário não é texto: o cartão diz o tamanho real, como nos demais
        // motores, em vez de fingir que os bytes são uma string.
        Bson::Binary(binaria) => Value::String(format!("[blob {} bytes]", binaria.bytes.len())),
        Bson::Decimal128(decimal) => Value::String(decimal.to_string()),
        Bson::Timestamp(marca) => Value::Number((marca.time as i64).into()),
        Bson::RegularExpression(regex) => {
            Value::String(format!("/{}/{}", regex.pattern, regex.options))
        }
        Bson::JavaScriptCode(codigo) => Value::String(codigo.clone()),
        Bson::JavaScriptCodeWithScope(codigo) => Value::String(codigo.code.clone()),
        Bson::Array(lista) => Value::Array(lista.iter().map(bson_para_json).collect()),
        Bson::Document(subdocumento) => Value::Object(
            subdocumento
                .iter()
                .map(|(chave, valor)| (chave.clone(), bson_para_json(valor)))
                .collect(),
        ),
        Bson::DbPointer(ponteiro) => Value::String(format!("{ponteiro:?}")),
    }
}

/// O tipo que a amostra viu primeiro para cada campo — o que o cartão mostra
/// como "tipo" da coluna. O rótulo é o nome do valor BSON, o vocabulário do
/// próprio banco.
fn rotulo_bson(valor: &Bson) -> &'static str {
    match valor {
        Bson::Double(_) | Bson::Decimal128(_) => "double",
        Bson::Int32(_) => "int",
        Bson::Int64(_) => "long",
        Bson::String(_) | Bson::Symbol(_) => "string",
        Bson::Boolean(_) => "bool",
        Bson::ObjectId(_) => "objectId",
        Bson::DateTime(_) => "date",
        Bson::Timestamp(_) => "timestamp",
        Bson::Binary(_) => "binary",
        Bson::RegularExpression(_) => "regex",
        Bson::JavaScriptCode(_) | Bson::JavaScriptCodeWithScope(_) => "code",
        Bson::Array(_) => "array",
        Bson::Document(_) => "object",
        Bson::Null | Bson::Undefined => "null",
        Bson::MinKey | Bson::MaxKey | Bson::DbPointer(_) => "outro",
    }
}

fn erro(e: mongodb::error::Error) -> anyhow::Error {
    anyhow!(tr("erros.bancoErro", &[("detalhe", &e.to_string())]))
}

impl Mongo {
    pub fn abrir(conexao: &Conexao) -> Result<Self> {
        if conexao.banco.trim().is_empty() {
            bail!(tr("erros.bancoNomeBancoFaltando", &[]));
        }
        let endereco = ServerAddress::Tcp {
            host: conexao.host.trim().to_string(),
            port: Some(conexao.porta_efetiva()),
        };
        let mut opcoes = ClientOptions::builder()
            .hosts(vec![endereco])
            .connect_timeout(Some(Duration::from_secs(10)))
            .server_selection_timeout(Some(Duration::from_secs(10)))
            .build();
        if !conexao.usuario.trim().is_empty() {
            opcoes.credential = Some(
                Credential::builder()
                    .username(conexao.usuario.trim().to_string())
                    .password(conexao.senha()?)
                    .build(),
            );
        }
        // O cliente do Mongo não conecta aqui: conecta na primeira operação, e é
        // lá que um host errado vira erro de verdade.
        let cliente = Client::with_options(opcoes).map_err(erro)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        Ok(Self { runtime, cliente, banco: conexao.banco.trim().to_string() })
    }

    /// A união das chaves de uma amostra, na ordem em que apareceram: é o
    /// esquema que a coleção de fato tem, não o que alguém declarou.
    async fn amostrar(&self, colecao: &str) -> Result<Vec<(String, String)>> {
        let cursor = self
            .cliente
            .database(&self.banco)
            .collection::<DocBson>(colecao)
            .find(DocBson::new())
            .limit(AMOSTRA as i64)
            .await
            .map_err(erro)?;
        let mut cursor = cursor;
        let mut colunas: Vec<(String, String)> = vec![("_id".into(), "objectId".into())];
        while cursor.advance().await.map_err(erro)? {
            let documento = cursor.deserialize_current().map_err(erro)?;
            for (chave, valor) in documento.iter() {
                if !colunas.iter().any(|(nome, _)| nome == chave) {
                    colunas.push((chave.clone(), rotulo_bson(valor).to_string()));
                }
            }
        }
        Ok(colunas)
    }
}

impl Motor for Mongo {
    fn fontes(&mut self) -> Result<Vec<String>> {
        let cliente = self.cliente.clone();
        let banco = self.banco.clone();
        self.runtime.block_on(async move {
            let mut nomes = cliente
                .database(&banco)
                .list_collection_names()
                .await
                .map_err(erro)?;
            // system.* é bookkeeping interno do servidor, como sqlite_* é do
            // SQLite: não é coleção de dono.
            nomes.retain(|nome| !nome.starts_with("system."));
            nomes.sort();
            Ok::<_, anyhow::Error>(nomes)
        })
    }

    fn colunas(&mut self, fonte: &str) -> Result<Vec<(String, String)>> {
        let conhecidas = self.fontes()?;
        let colecao = conferir_colecao(fonte, &conhecidas)?.to_string();
        self.runtime.block_on(self.amostrar(&colecao))
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
        let colecao = conferir_colecao(fonte, &conhecidas)?.to_string();
        let consulta = filtro_bson(filtro)?;
        let pedidas: Vec<String> = if colunas_pedidas.is_empty() {
            let amostra = self.runtime.block_on(self.amostrar(&colecao))?;
            amostra.into_iter().map(|(nome, _)| nome).collect()
        } else {
            colunas_pedidas
                .iter()
                .map(|c| {
                    conferir_campo(c)?;
                    Ok(c.trim().to_string())
                })
                .collect::<Result<Vec<_>>>()?
        };
        let alvo = self
            .cliente
            .database(&self.banco)
            .collection::<DocBson>(&colecao);
        // Campos pedidos entram como projeção — o Mongo devolve o que pediu, e
        // o que não tem no documento volta nulo na linha.
        let mut achado = alvo.find(consulta).limit(limite.saturating_add(1) as i64);
        if let Some((coluna, desc)) = ordem {
            let mut ordenar = DocBson::new();
            ordenar.insert(conferir_campo(coluna)?, Bson::Int32(if desc { -1 } else { 1 }));
            achado = achado.sort(ordenar);
        }
        if !pedidas.is_empty() {
            let mut projecao = DocBson::new();
            for coluna in &pedidas {
                projecao.insert(coluna.clone(), Bson::Int32(1));
            }
            achado = achado.projection(projecao);
        }
        let mut cursor = self
            .runtime
            .block_on(async move { achado.await })
            .map_err(erro)?;
        let mut linhas: Vec<Vec<Value>> = Vec::new();
        while linhas.len() < limite.saturating_add(1) {
            if !self.runtime.block_on(cursor.advance()).map_err(erro)? {
                break;
            }
            let documento = cursor.deserialize_current().map_err(erro)?;
            linhas.push(
                pedidas
                    .iter()
                    .map(|coluna| {
                        documento
                            .get(coluna)
                            .map(bson_para_json)
                            .unwrap_or(Value::Null)
                    })
                    .collect(),
            );
        }
        let truncado = linhas.len() > limite;
        if truncado {
            linhas.truncate(limite);
        }
        Ok(Consulta { colunas: pedidas, linhas, truncado, duracao_ms: cronometro.ms() })
    }

    fn contar(&mut self, fonte: &str, filtro: &Filtro) -> Result<u64> {
        let conhecidas = self.fontes()?;
        let colecao = conferir_colecao(fonte, &conhecidas)?.to_string();
        let consulta = filtro_bson(filtro)?;
        let cliente = self.cliente.clone();
        let banco = self.banco.clone();
        self.runtime.block_on(async move {
            cliente
                .database(&banco)
                .collection::<DocBson>(&colecao)
                .count_documents(consulta)
                .await
                .map_err(erro)
        })
    }

    fn inserir(&mut self, fonte: &str, valores: &Map<String, Value>) -> Result<u64> {
        let conhecidas = self.fontes()?;
        let colecao = conferir_colecao(fonte, &conhecidas)?.to_string();
        let documento = documento_de(valores)?;
        let cliente = self.cliente.clone();
        let banco = self.banco.clone();
        self.runtime.block_on(async move {
            cliente
                .database(&banco)
                .collection::<DocBson>(&colecao)
                .insert_one(documento)
                .await
                .map_err(erro)
        })?;
        Ok(1)
    }

    fn atualizar(
        &mut self,
        fonte: &str,
        valores: &Map<String, Value>,
        filtro: &Filtro,
    ) -> Result<u64> {
        let conhecidas = self.fontes()?;
        let colecao = conferir_colecao(fonte, &conhecidas)?.to_string();
        if filtro.is_empty() {
            bail!(tr("erros.bancoOndeObrigatorio", &[]));
        }
        if valores.is_empty() {
            bail!(tr("erros.bancoValoresVazios", &[]));
        }
        let mut definicoes = DocBson::new();
        for (chave, valor) in valores {
            definicoes.insert(conferir_campo(chave)?, json_para_bson(valor));
        }
        let consulta = filtro_bson(filtro)?;
        let cliente = self.cliente.clone();
        let banco = self.banco.clone();
        let atualizadas = self.runtime.block_on(async move {
            cliente
                .database(&banco)
                .collection::<DocBson>(&colecao)
                .update_many(consulta, doc! { "$set": definicoes })
                .await
                .map_err(erro)
        })?;
        Ok(atualizadas.modified_count)
    }

    fn apagar(&mut self, fonte: &str, filtro: &Filtro) -> Result<u64> {
        let conhecidas = self.fontes()?;
        let colecao = conferir_colecao(fonte, &conhecidas)?.to_string();
        if filtro.is_empty() {
            bail!(tr("erros.bancoOndeObrigatorio", &[]));
        }
        let consulta = filtro_bson(filtro)?;
        let cliente = self.cliente.clone();
        let banco = self.banco.clone();
        let apagadas = self.runtime.block_on(async move {
            cliente
                .database(&banco)
                .collection::<DocBson>(&colecao)
                .delete_many(consulta)
                .await
                .map_err(erro)
        })?;
        Ok(apagadas.deleted_count)
    }

    fn sql(&mut self, _declaracao: &str, _limite: usize) -> Result<ResultadoSql> {
        bail!(tr("erros.bancoSemSql", &[("tipo", "MongoDB")]))
    }
}
