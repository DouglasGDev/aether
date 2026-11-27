//! O motor MongoDB: coleções de documentos em vez de tabelas de linhas.
//!
//! O que vale como "coluna" aqui é a união das chaves de uma amostra dos
//! documentos — coleção não tem esquema declarado, e fingir que tem mentiria. A
//! amostra é pista, não schema fechado: campos fora dela e caminhos aninhados
//! podem existir. O mesmo vale para tipos: cada campo carrega o tipo que a
//! amostra viu primeiro, e um mesmo campo pode ser número num documento e texto
//! noutro. O nome de campo é conferido por regra própria (nunca `$`, que é
//! operador do Mongo, não identificador) — não por SQL, porque aqui não há SQL:
//! os filtros viram consultas BSON pela API do driver, sem interpolar texto em
//! nenhum ponto. O agendador é do processo, como nos demais motores de rede —
//! ver `super::agendador`.

use anyhow::{Result, anyhow, bail};
use mongodb::{
    Client,
    bson::{Bson, Document as DocBson, doc},
    options::{ClientOptions, Credential, ServerAddress, Tls, TlsOptions},
};
use serde_json::{Map, Value};
use std::time::Duration;

use super::{
    Comparacao, Consulta, Cronometro, Filtro, Motor, PRAZO_DE_LEITURA, ResultadoSql,
    aparar_no_teto, erro_de_prazo, ordem_de, prazo_de_espera, teto_de_leitura,
};
use crate::bancos::config::Conexao;
use crate::idiomas::tr;

/// Quantos documentos entram na amostra que define as "colunas" da coleção.
const AMOSTRA: usize = 100;

pub struct Mongo {
    runtime: &'static tokio::runtime::Runtime,
    cliente: Client,
    banco: String,
}

/// Nome de campo que o Mongo aceita como identificador: não vazio e sem `$`
/// inicial. Tudo que começa com `$` é operador da gramática do Mongo — aceitar
/// do modelo seria deixar a consulta dizer comandos, não dados.
fn conferir_campo(nome: &str) -> Result<&str> {
    let nome = nome.trim();
    if nome.is_empty() || nome.starts_with('$') {
        bail!(tr("erros.bancoCampoInvalido", &[("coluna", nome)]));
    }
    Ok(nome)
}

/// Um caminho de campos, não uma expressão: segmentos não vazios, sem `$`
/// inicial, espaço ou pontuação de comando. O driver recebe a chave por API;
/// esta régua impede que texto de ordem/SQL ou um operador vire nome de campo.
fn conferir_caminho(nome: &str) -> Result<&str> {
    let nome = nome.trim();
    let comando = nome.chars().any(|c| {
        c.is_whitespace()
            || c.is_control()
            || matches!(c, ';' | ',' | '(' | ')' | '\'' | '"' | '`' | '\\')
    });
    if nome.is_empty()
        || comando
        || nome.split('.').any(|parte| parte.is_empty() || parte.starts_with('$'))
    {
        bail!(tr("erros.bancoCampoInvalido", &[("coluna", nome)]));
    }
    Ok(nome)
}

/// A amostra só ajuda a preservar a caixa dos prefixos que viu. Um caminho
/// válido fora dela não é recusado: pode existir depois dos 100 documentos.
/// Operadores, segmentos vazios e texto de comando continuam barrados.
fn conferir_campo_conhecido(nome: &str, conhecidas: &[(String, String)]) -> Result<String> {
    let nome = conferir_caminho(nome)
        .map_err(|_| super::erro_coluna_invalida(nome.trim(), conhecidas))?;
    let mut partes: Vec<String> = nome.split('.').map(str::to_string).collect();
    for tamanho in (1..=partes.len()).rev() {
        let prefixo = partes[..tamanho].join(".");
        if let Some((conhecido, _)) = conhecidas.iter().find(|(conhecido, _)| conhecido == &prefixo)
        {
            for (parte, real) in partes.iter_mut().zip(conhecido.split('.')) {
                *parte = real.to_string();
            }
            break;
        }
    }
    Ok(partes.join("."))
}

fn ordenar_bson(coluna: &str, desc: bool, conhecidas: &[(String, String)]) -> Result<DocBson> {
    let mut ordenar = DocBson::new();
    ordenar.insert(
        conferir_campo_conhecido(coluna, conhecidas)?,
        Bson::Int32(if desc { -1 } else { 1 }),
    );
    Ok(ordenar)
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
                    json_para_bson(&condicao.valor)?
                }
            }
            Comparacao::Diferente => {
                if condicao.valor.is_null() {
                    Bson::Document(doc! { "$ne": Bson::Null })
                } else {
                    Bson::Document(doc! { "$ne": json_para_bson(&condicao.valor)? })
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
                pedaco.insert(rotulo, json_para_bson(&condicao.valor)?);
                Bson::Document(pedaco)
            }
        };
        consulta.insert(campo, pedaco);
    }
    Ok(consulta)
}

fn json_para_bson(valor: &Value) -> Result<Bson> {
    match valor {
        Value::Null => Ok(Bson::Null),
        Value::Bool(b) => Ok(Bson::Boolean(*b)),
        Value::Number(n) => Ok(if let Some(i) = n.as_i64() {
            Bson::Int64(i)
        } else {
            n.as_f64().map(Bson::Double).unwrap_or(Bson::Null)
        }),
        Value::String(texto) => Ok(Bson::String(texto.clone())),
        Value::Array(lista) => Ok(Bson::Array(
            lista.iter().map(json_para_bson).collect::<Result<Vec<_>>>()?,
        )),
        // Um objeto que o modelo manda como valor entra como subdocumento —
        // mas chave com `$` inicial é operador da gramática do Mongo, não
        // identificador: deixá-la passar transformaria um valor de filtro em
        // consulta sempre-verdadeira (`$ne: null`) e o atualizar/apagar em
        // escrita em massa. A recusa vale em qualquer profundidade, e o erro
        // nomeia a chave, como `conferir_campo` já faz para o nível de cima.
        Value::Object(objeto) => {
            let mut documento = DocBson::new();
            for (chave, valor) in objeto {
                if chave.starts_with('$') {
                    bail!(tr("erros.bancoCampoInvalido", &[("coluna", chave)]));
                }
                documento.insert(chave.clone(), json_para_bson(valor)?);
            }
            Ok(Bson::Document(documento))
        }
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
        documento.insert(chave, json_para_bson(valor)?);
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

/// O que a flag `tls_verificado` pede desta conexão. É a única leitura dela
/// neste motor, e não há número escrito à mão: `TlsOptions::default()` deixa
/// `allow_invalid_certificates` vazio, que no driver quer dizer "erro se o
/// servidor apresentar certificado inválido" — a cadeia conferida é a da
/// máquina do dono (as raízes que o `rustls-tls`, feature padrão do `mongodb`,
/// já trazem compilado).
///
/// Sem a marca devolve `None`, e `None` é o defeito do driver: TLS desligado,
/// como sempre foi aqui.
fn tls_da(conexao: &Conexao) -> Option<Tls> {
    conexao.tls_verificado.then(|| Tls::Enabled(TlsOptions::default()))
}

/// A espera de quem tem prazo no servidor mas não tem na conexão inteira: o
/// `maxTimeMS` mata a consulta no Mongo, e o prazo do cliente é o que cobre o
/// resto — um servidor que para de responder ao cursor deixaria a conversa
/// pendurada para sempre sem ele.
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
        opcoes.tls = tls_da(conexao);
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
        Ok(Self {
            runtime: super::agendador()?,
            cliente,
            banco: conexao.banco.trim().to_string(),
        })
    }

    /// A coleção existe? O servidor responde com a lista filtrada pelo nome, e é
    /// uma viagem de uma linha: `fontes()` inteira traz o nome de todas as
    /// coleções do banco para comparar aqui, e a leitura não precisa delas.
    fn colecao_conferida(&mut self, fonte: &str) -> Result<String> {
        let cliente = self.cliente.clone();
        let banco = self.banco.clone();
        let pedida = fonte.trim().to_string();
        let nomes = esperar(self.runtime, {
            let procurada = pedida.clone();
            async move {
                cliente
                    .database(&banco)
                    .list_collection_names()
                    .filter(doc! { "name": procurada })
                    .await
                    .map_err(erro)
            }
        })?;
        conferir_colecao(&pedida, &nomes).map(str::to_string)
    }

    /// A união das chaves de uma amostra, na ordem em que apareceram: é uma
    /// pista do que a coleção tem, não um schema fechado nem uma garantia de
    /// campos que só apareçam fora dos 100 documentos.
    async fn amostrar(&self, colecao: &str) -> Result<Vec<(String, String)>> {
        let cursor = self
            .cliente
            .database(&self.banco)
            .collection::<DocBson>(colecao)
            .find(DocBson::new())
            .limit(AMOSTRA as i64)
            .max_time(PRAZO_DE_LEITURA)
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
        esperar(self.runtime, async move {
            let mut nomes = cliente
                .database(&banco)
                .list_collection_names()
                .await
                .map_err(erro)?;
            // system.* é bookkeeping interno do servidor, como sqlite_* é do
            // SQLite: não é coleção de dono.
            nomes.retain(|nome| !nome.starts_with("system."));
            nomes.sort();
            Ok(nomes)
        })
    }

    fn colunas(&mut self, fonte: &str) -> Result<Vec<(String, String)>> {
        let colecao = self.colecao_conferida(fonte)?;
        let amostra = esperar(self.runtime, self.amostrar(&colecao))?;
        Ok(amostra)
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
        let colecao = self.colecao_conferida(fonte)?;
        let consulta = filtro_bson(filtro)?;
        // Sem colunas pedidas a amostra define a projeção. Com ordem, ela é
        // apenas pista para preservar a caixa dos nomes que viu; não fecha o
        // conjunto de campos que a coleção pode ter.
        let amostra = if colunas_pedidas.is_empty() || ordem.is_some() {
            Some(esperar(self.runtime, self.amostrar(&colecao))?)
        } else {
            None
        };
        let pedidas: Vec<String> = if colunas_pedidas.is_empty() {
            amostra
                .as_ref()
                .expect("colunas implícitas exigem a amostra")
                .iter()
                .map(|(nome, _)| nome.clone())
                .collect()
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
        let mut achado = alvo
            .find(consulta)
            .limit(teto_de_leitura(limite) as i64)
            .max_time(PRAZO_DE_LEITURA);
        if let Some((texto, desc)) = ordem {
            let (coluna, desc) = ordem_de(texto, desc, None)?;
            let conhecidas = amostra
                .as_ref()
                .expect("ordem pedida exige a amostra como pista");
            achado = achado.sort(ordenar_bson(&coluna, desc, conhecidas)?);
        }
        if !pedidas.is_empty() {
            let mut projecao = DocBson::new();
            for coluna in &pedidas {
                projecao.insert(coluna.clone(), Bson::Int32(1));
            }
            achado = achado.projection(projecao);
        }
        // A busca e o despejo do cursor cabem numa espera só: `maxTimeMS` mata a
        // consulta no servidor, mas é o cursor que fica pendurado entre uma
        // página e outra, e é a leitura inteira que tem um prazo.
        let colunas_da_linha = pedidas.clone();
        let saida = esperar(self.runtime, async move {
            let mut cursor = achado.await.map_err(erro)?;
            let mut linhas: Vec<Vec<Value>> = Vec::new();
            while linhas.len() < teto_de_leitura(limite) {
                if !cursor.advance().await.map_err(erro)? {
                    break;
                }
                let documento = cursor.deserialize_current().map_err(erro)?;
                linhas.push(
                    colunas_da_linha
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
            Ok(linhas)
        })?;
        let (linhas, truncado) = aparar_no_teto(saida, limite);
        Ok(Consulta { colunas: pedidas, linhas, truncado, duracao_ms: cronometro.ms() })
    }

    fn contar(&mut self, fonte: &str, filtro: &Filtro) -> Result<u64> {
        let colecao = self.colecao_conferida(fonte)?;
        let consulta = filtro_bson(filtro)?;
        let cliente = self.cliente.clone();
        let banco = self.banco.clone();
        esperar(self.runtime, async move {
            cliente
                .database(&banco)
                .collection::<DocBson>(&colecao)
                .count_documents(consulta)
                .await
                .map_err(erro)
        })
    }

    fn inserir(&mut self, fonte: &str, valores: &Map<String, Value>) -> Result<u64> {
        let colecao = self.colecao_conferida(fonte)?;
        let documento = documento_de(valores)?;
        let cliente = self.cliente.clone();
        let banco = self.banco.clone();
        esperar(self.runtime, async move {
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
        let colecao = self.colecao_conferida(fonte)?;
        if filtro.is_empty() {
            bail!(tr("erros.bancoOndeObrigatorio", &[]));
        }
        if valores.is_empty() {
            bail!(tr("erros.bancoValoresVazios", &[]));
        }
        let mut definicoes = DocBson::new();
        for (chave, valor) in valores {
            definicoes.insert(conferir_campo(chave)?, json_para_bson(valor)?);
        }
        let consulta = filtro_bson(filtro)?;
        let cliente = self.cliente.clone();
        let banco = self.banco.clone();
        let atualizadas = esperar(self.runtime, async move {
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
        let colecao = self.colecao_conferida(fonte)?;
        if filtro.is_empty() {
            bail!(tr("erros.bancoOndeObrigatorio", &[]));
        }
        let consulta = filtro_bson(filtro)?;
        let cliente = self.cliente.clone();
        let banco = self.banco.clone();
        let apagadas = esperar(self.runtime, async move {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bancos::config::{conexao_de_teste, Tipo};
    use crate::bancos::motor::filtro_de;
    use serde_json::json;

    /// O sort recebe do parser uma coluna e um booleano. O BSON sai com uma
    /// única chave; a amostra preserva a caixa conhecida sem excluir campos que
    /// só apareçam fora dela. Texto de ordem ou injeção não vira sort.
    #[test]
    fn o_sort_do_mongo_usa_uma_coluna_real_e_a_direcao_normalizada() {
        let schema = vec![
            ("_id".to_string(), "objectId".to_string()),
            ("qt_venda".to_string(), "int".to_string()),
        ];
        let (coluna, desc) = super::super::ordem_de("qt_venda DESC", false, None).unwrap();
        let sort = ordenar_bson(&coluna, desc, &schema).unwrap();
        assert_eq!(sort.len(), 1);
        assert_eq!(sort.get_i32("qt_venda").unwrap(), -1);

        let (coluna, desc) = super::super::ordem_de("qt_venda ASC", true, None).unwrap();
        let sort = ordenar_bson(&coluna, desc, &schema).unwrap();
        assert_eq!(sort.get_i32("qt_venda").unwrap(), 1);
        assert_eq!(
            ordenar_bson("qt_venda", true, &schema)
                .unwrap()
                .get_i32("qt_venda")
                .unwrap(),
            -1
        );

        for entrada in [
            "$where",
            "qt_venda, id",
            "qt_venda; DROP TABLE vendas",
        ] {
            let erro = ordenar_bson(entrada, false, &schema)
                .unwrap_err()
                .to_string();
            for trecho in [entrada, "qt_venda", "ordem", "ordemDesc", "bancos_colunas"] {
                assert!(erro.contains(trecho), "{entrada}: {erro}");
            }
        }
    }

    /// A amostra é pista: caminhos aninhados e campos que só aparecem depois do
    /// centésimo documento passam; nomes que poderiam virar operador ou comando
    /// continuam sendo recusados em qualquer segmento.
    #[test]
    fn o_sort_aceita_aninhamento_e_fora_da_amostra_sem_aceitar_operadores() {
        let schema = vec![
            ("_id".to_string(), "objectId".to_string()),
            ("Perfil".to_string(), "object".to_string()),
            ("qt_venda".to_string(), "int".to_string()),
        ];
        for caminho in ["Perfil.cidade", "Perfil.endereco.cidade", "criado_em"] {
            let sort = ordenar_bson(caminho, true, &schema).unwrap();
            assert_eq!(sort.len(), 1);
            assert_eq!(sort.get_i32(caminho).unwrap(), -1, "{caminho}");
        }
        // O prefixo conhecido conserva a caixa real da coleção.
        let sort = ordenar_bson("Perfil.endereco.cidade", false, &schema).unwrap();
        assert_eq!(sort.get_i32("Perfil.endereco.cidade").unwrap(), 1);

        for entrada in [
            "",
            ".Perfil",
            "Perfil.",
            "Perfil..cidade",
            "$where",
            "Perfil.$where",
            "Perfil cidade",
            "Perfil;DROP",
            "Perfil,cidade",
        ] {
            assert!(
                ordenar_bson(entrada, false, &schema).is_err(),
                "{entrada} não poderia virar sort"
            );
        }
    }

    /// O prazo espera de verdade, e cobre a leitura inteira: no Mongo o
    /// `maxTimeMS` mata a consulta no servidor, mas o cursor é despejado ida a
    /// ida, e era cada uma delas que não tinha relógio nenhum. A tarefa dorme
    /// mais que o prazo e devolve `Ok`, então o único erro possível é o relógio.
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
            Ok(()) => {
                panic!("defeito 4: a ida voltou depois de passar do prazo — ninguém esperou nada")
            }
        };
        assert!(
            erro.to_string().contains('1'),
            "a frase do prazo tem de dizer segundos: {erro}"
        );
    }

    /// Uma chave `$` no valor é operador da gramática do Mongo, não dado:
    /// aceita-la transformaria o filtro em consulta sempre-verdadeira e o
    /// atualizar/apagar em escrita em massa dentro da tabela autorizada.
    #[test]
    fn chave_de_operador_no_valor_e_recusada_em_qualquer_profundidade() {
        let erro = json_para_bson(&json!({"$ne": null})).unwrap_err().to_string();
        assert!(erro.contains("$ne"), "{erro}");
        assert!(json_para_bson(&json!({"a": {"b": {"$gt": 1}}})).is_err());
        assert!(json_para_bson(&json!([{"$exists": true}])).is_err());
        let valores = json!({"campo": {"$ne": null, "x": 1}});
        assert!(documento_de(valores.as_object().unwrap()).is_err());
    }

    /// O caminho real da injeção: `filtro_de` entrega um objeto com várias
    /// chaves como igualdade, e o valor chegava ao driver como documento de
    /// operadores. A recusa tem de acontecer antes de a consulta existir.
    #[test]
    fn filtro_com_operador_embutido_nao_vira_consulta() {
        let filtro = filtro_de(Some(&json!({"campo": {"$ne": null, "x": 1}}))).unwrap();
        assert!(filtro_bson(&filtro).is_err());
        let filtro = filtro_de(Some(&json!({"campo": {"op": "igual", "valor": {"$ne": null}}}))).unwrap();
        assert!(filtro_bson(&filtro).is_err());
    }

    /// Objeto sem operador continua sendo subdocumento legítimo: a recusa é
    /// do `$`, não de valores compostos.
    #[test]
    fn objeto_sem_operador_continua_entrando_como_subdocumento() {
        let bson = json_para_bson(&json!({"a": 1, "b": {"c": "x"}})).unwrap();
        let Bson::Document(documento) = bson else { panic!("esperava documento") };
        assert_eq!(documento.get_i64("a").unwrap(), 1);
        let filtro =
            filtro_de(Some(&json!({"campo": {"op": "igual", "valor": {"a": 1}}}))).unwrap();
        assert!(filtro_bson(&filtro).is_ok());
    }

    /// A escolha de TLS sai da flag, não de um número: sem marca o driver
    /// continua com o defeito de sempre (desligado); com ela, entra a opção
    /// padrão do driver, que é conferir o certificado.
    #[test]
    fn a_flag_de_tls_escolhe_a_opcao_do_driver() {
        let lisa = conexao_de_teste(Tipo::Mongo);
        assert_eq!(tls_da(&lisa), None, "sem a marca fica o defeito do driver, como hoje");

        let mut outra_porta = conexao_de_teste(Tipo::Mongo);
        outra_porta.porta = Some(27018);
        outra_porta.host = "127.0.0.1".into();
        assert_eq!(tls_da(&outra_porta), None, "nem a porta, nem o host ligam o TLS");

        let mut conferida = conexao_de_teste(Tipo::Mongo);
        conferida.tls_verificado = true;
        assert_eq!(
            tls_da(&conferida),
            Some(Tls::Enabled(TlsOptions::default())),
            "com a marca, as opções do driver sem exceção de certificado"
        );
    }

    /// O que só um servidor de verdade responde: com a flag ligada, um Mongo
    /// que não fala TLS tem de recusar a conexão; um que fala TLS com
    /// certificado auto-assinado tem de ser barrado pela cadeia. Ignorado
    /// porque não há mongod nesta máquina.
    #[ignore = "precisa de um servidor Mongo real com --tlsMode; sem ele não há aperto de mão para medir"]
    #[test]
    fn tls_conferido_so_falha_com_servidor_verdadeiro() {
        let mut conferida = conexao_de_teste(Tipo::Mongo);
        conferida.tls_verificado = true;
        assert!(Mongo::abrir(&conferida).is_err());
    }
}
