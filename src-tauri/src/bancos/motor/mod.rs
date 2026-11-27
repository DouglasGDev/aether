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
use std::sync::OnceLock;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::config::{Conexao, Operacao};

/// O prazo de cada ida ao banco. Quinze segundos é o ponto em que uma leitura
/// parada custa mais à conversa do que o resultado dela vale, e o erro com o
/// número na mão diz ao dono o que refazer. Fixo de propósito: prazo é
/// proteção, não preferência — um cadastro antigo não pode acabar sem nenhum.
pub const PRAZO_DE_LEITURA: Duration = Duration::from_secs(15);

pub fn prazo_em_ms() -> u64 {
    PRAZO_DE_LEITURA.as_millis() as u64
}

/// A frase de quem esperou mais que o prazo. A chave já existe em todo idioma e
/// carrega o número — prazo sem número não diz o que ajustar.
pub fn erro_de_prazo() -> anyhow::Error {
    anyhow!(crate::idiomas::tr(
        "erros.tempoEsgotado",
        &[("segundos", &PRAZO_DE_LEITURA.as_secs().to_string())]
    ))
}

/// Quantas linhas uma leitura puxa do cursor: o teto mais uma, que é a prova de
/// que houve corte e não fim do banco. É também o ponto onde o leitor para — o
/// que materializa a tabela inteira antes de aparar não mede nada.
pub fn teto_de_leitura(limite: usize) -> usize {
    limite.saturating_add(1)
}

/// Quanto o cliente espera antes de desistir por conta própria. Um segundo acima
/// do prazo do servidor: quando os dois estão ligados, o erro que chega primeiro
/// é o do banco, que sabe dizer o que expirou; o teto do cliente é o que sobra
/// para o servidor que nem responde ao cancelamento.
pub fn prazo_de_espera() -> Duration {
    PRAZO_DE_LEITURA + Duration::from_secs(1)
}

/// Drena um cursor assíncrono até o teto de leitura, aparando a linha de barriga.
/// Quem chama entrega o «próximo» em vez do cursor, para que o teto seja medido
/// sem servidor: com uma pilha de mentira que não acaba, o leitor que materializa
/// a tabela inteira antes de aparar puxa tudo e a medida entrega o defeito.
pub async fn colher_ate<T, F, Fut>(mut proximo: F, limite: usize) -> Result<(Vec<T>, bool)>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Option<T>>>,
{
    let teto = teto_de_leitura(limite);
    let mut linhas = Vec::new();
    while linhas.len() < teto {
        let Some(linha) = proximo().await? else {
            break;
        };
        linhas.push(linha);
    }
    Ok(aparar_no_teto(linhas, limite))
}

/// Apara o que veio do cursor no teto e diz se sobrou linha depois dele.
pub fn aparar_no_teto<T>(mut linhas: Vec<T>, limite: usize) -> (Vec<T>, bool) {
    let truncado = linhas.len() > limite;
    if truncado {
        linhas.truncate(limite);
    }
    (linhas, truncado)
}

/// Um agendador para o processo inteiro. O que é episódico aqui é a conexão,
/// nunca o runtime: montar um de uma thread por chamada pagava a criação da
/// thread e do reactor a cada ferramenta, e era dele que o `block_on` de cada
/// motor dependia.
static AGENDADOR: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();

/// Só conta para o teste: quantas vezes o processo de fato montou um runtime.
#[cfg(test)]
pub static AGENDADORES_NASCIDOS: AtomicUsize = AtomicUsize::new(0);

pub fn agendador() -> Result<&'static tokio::runtime::Runtime> {
    AGENDADOR
        .get_or_init(|| {
            // A régua só existe no teste: o que ela mede é quantas vezes o
            // processo montou um runtime, e nada no aplicativo depende disso.
            #[cfg(test)]
            AGENDADORES_NASCIDOS.fetch_add(1, Ordering::SeqCst);
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                // Duas threads: o que mora nelas é tarefa de soquete, que espera
                // em vez de trabalhar, e uma consulta por vez é o que a
                // ferramenta tem em voo.
                .worker_threads(2)
                .thread_name("aether-bancos")
                .build()
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|detalhe| anyhow!(crate::idiomas::tr("erros.bancoAbrir", &[("detalhe", detalhe)])))
}

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

/// Separa a direção legada (`"coluna DESC"` / `"coluna ASC"`) sem interpretar
/// SQL. Quando o schema já está na mão, o nome inteiro é conferido primeiro:
/// `status DESC` pode ser uma coluna literal, não um sufixo. Só sem essa
/// correspondência o sufixo é separado. `ordem_desc` vale quando não há sufixo
/// — ou quando o nome inteiro é uma coluna real.
pub fn ordem_de(
    texto: &str,
    ordem_desc: bool,
    schema: Option<&[(String, String)]>,
) -> Result<(String, bool)> {
    let texto = texto.trim();
    if texto.is_empty() {
        bail!(crate::idiomas::tr("erros.bancoOrdemInvalida", &[]));
    }
    if let Some(schema) = schema {
        // Caixa exata primeiro: bancos com identificadores entre aspas podem
        // distinguir nomes que só diferem nela. A busca insensível é o fallback
        // que os motores SQL já usam na conferência de colunas.
        let conhecida = schema
            .iter()
            .find(|(nome, _)| nome == texto)
            .or_else(|| schema.iter().find(|(nome, _)| nome.eq_ignore_ascii_case(texto)));
        if let Some((nome, _)) = conhecida {
            return Ok((nome.clone(), ordem_desc));
        }
    }
    if let Some((coluna, direcao)) = texto.rsplit_once(char::is_whitespace) {
        let coluna = coluna.trim_end();
        if !coluna.is_empty() {
            if direcao.eq_ignore_ascii_case("DESC") {
                return Ok((coluna.to_string(), true));
            }
            if direcao.eq_ignore_ascii_case("ASC") {
                return Ok((coluna.to_string(), false));
            }
        }
    }
    Ok((texto.to_string(), ordem_desc))
}

/// A recusa de identificador carrega o schema que o motor acabou de descobrir.
/// Assim o modelo troca um palpite por um nome real e monta a ordem no formato
/// correto, sem transformar a mensagem em SQL livre.
pub fn erro_coluna_invalida(coluna: &str, colunas: &[(String, String)]) -> anyhow::Error {
    let lista = if colunas.is_empty() {
        crate::idiomas::tr("bancos.semColunas", &[])
    } else {
        colunas
            .iter()
            .map(|(nome, _)| nome.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    anyhow!(crate::idiomas::tr(
        "erros.bancoColunaInexistente",
        &[("coluna", coluna), ("colunas", &lista)]
    ))
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
            Value::Object(objeto) if objeto.contains_key("op") => {
                // O envelope tem exatamente `op`, ou `op` + `valor`. Chave
                // sobrando não é comparação: seria o aplicativo escolher um
                // significado para um JSON que o modelo inventou.
                let sem_valor = objeto.len() == 1;
                let com_valor = objeto.len() == 2 && objeto.contains_key("valor");
                if !sem_valor && !com_valor {
                    bail!(crate::idiomas::tr("erros.filtroInvalido", &[]));
                }
                let Some(Value::String(rotulo)) = objeto.get("op") else {
                    bail!(crate::idiomas::tr("erros.filtroInvalido", &[]));
                };
                let comparacao = Comparacao::de_rotulo(rotulo).ok_or_else(|| {
                    anyhow!(crate::idiomas::tr(
                        "erros.filtroComparacao",
                        &[("op", rotulo)]
                    ))
                })?;
                let valor = if com_valor {
                    objeto.get("valor").cloned().unwrap_or(Value::Null)
                } else {
                    Value::Null
                };
                (comparacao, valor)
            }
            // Sem `op` há duas leituras possíveis, e elas divergem por uma chave:
            // `{"a":1}` é um subdocumento comparado por igualdade, enquanto o
            // `{"valor":10}` sozinho é o envelope que perdeu o operador — virar
            // igualdade contra um objeto seria responder outra pergunta.
            Value::Object(objeto)
                if objeto.len() == 1 && objeto.contains_key("valor") =>
            {
                bail!(crate::idiomas::tr("erros.filtroInvalido", &[]));
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

    /// `ordem` leva o texto cru e a direção separada. O motor resolve o sufixo
    /// só depois de carregar o schema, sem outra viagem ao banco.
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

/// Abre a conexão no motor da família com a alça da operação que a ferramenta
/// vai fazer. Uma conexão por chamada: ferramentas de banco são episódicas, e
/// não deixar soquete vivo é mais barato que geri-lo — o que não é episódico é
/// o agendador, e ele nasce uma vez no processo.
///
/// A operação só tem efeito onde o motor pode escolhê-la: no SQLite a alça de
/// leitura fecha a escrita no arquivo. Nos de rede a política já foi conferida
/// antes de abrir, e o que vale para todos é o prazo.
pub fn abrir_para(conexao: &Conexao, operacao: Operacao) -> Result<Box<dyn Motor>> {
    match conexao.tipo {
        super::config::Tipo::Sqlite => Ok(Box::new(sqlite::Sqlite::abrir(conexao, operacao)?)),
        super::config::Tipo::Postgres => Ok(Box::new(postgres::Postgres::abrir(conexao)?)),
        super::config::Tipo::Mysql => Ok(Box::new(mysql::Mysql::abrir(conexao)?)),
        super::config::Tipo::Mssql => Ok(Box::new(mssql::Mssql::abrir(conexao)?)),
        super::config::Tipo::Mongo => Ok(Box::new(mongo::Mongo::abrir(conexao)?)),
        super::config::Tipo::Oracle => Ok(Box::new(oracle::Oracle::abrir(conexao)?)),
    }
}

/// A alça cheia, de quem abre sem declarar a operação — hoje só o editor de
/// conexão, que lista tabelas antes de a política existir. Toda ferramenta do
/// cadastro passa por `abrir_para`.
pub fn abrir(conexao: &Conexao) -> Result<Box<dyn Motor>> {
    abrir_para(conexao, Operacao::Atualizar)
}

pub mod mongo;
pub mod mssql;
pub mod mysql;
pub mod oracle;
pub mod postgres;
pub mod sqlite;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A compatibilidade tira só a direção final: o nome continua sendo um
    /// identificador para o motor conferir no schema, nunca uma cláusula SQL.
    #[test]
    fn a_ordem_legada_se_separa_em_coluna_e_direcao() {
        for (entrada, desc, coluna, direcao) in [
            ("qt_venda", true, "qt_venda", true),
            ("qt_venda", false, "qt_venda", false),
            ("qt_venda DESC", false, "qt_venda", true),
            ("qt_venda ASC", true, "qt_venda", false),
            (" qt_venda \t dEsC ", true, "qt_venda", true),
            ("QT_VENDA asc", false, "QT_VENDA", false),
        ] {
            assert_eq!(
                ordem_de(entrada, desc, None).unwrap(),
                (coluna.to_string(), direcao)
            );
        }
    }

    /// O parser não tenta adivinhar expressões ou várias colunas: deixa o texto
    /// intacto para a validação real do motor recusar com as colunas conhecidas.
    #[test]
    fn o_parser_nao_transforma_sql_em_identificador() {
        for (entrada, coluna) in [
            ("ORDER BY qt_venda", "ORDER BY qt_venda"),
            ("qt_venda, id", "qt_venda, id"),
            ("qt_venda + 1", "qt_venda + 1"),
            ("qt_venda DESC, id ASC", "qt_venda DESC, id"),
            ("qt_venda; DROP TABLE vendas", "qt_venda; DROP TABLE vendas"),
            (
                "qt_venda DESC; DROP TABLE vendas",
                "qt_venda DESC; DROP TABLE vendas",
            ),
        ] {
            assert_eq!(ordem_de(entrada, false, None).unwrap().0, coluna);
        }
        assert!(ordem_de("   ", false, None).is_err());
    }

    /// O schema vem antes da compatibilidade: quando as duas colunas existem,
    /// `status DESC` é o nome literal e `ordemDesc` escolhe sua direção.
    #[test]
    fn a_coluna_literal_com_sufixo_so_e_desmontada_sem_schema_correspondente() {
        let schema = vec![
            ("status".to_string(), "text".to_string()),
            ("status DESC".to_string(), "text".to_string()),
        ];
        for (entrada, desc, coluna, direcao) in [
            ("status DESC", false, "status DESC", false),
            ("status DESC", true, "status DESC", true),
            ("STATUS DESC", false, "status DESC", false),
            ("status ASC", true, "status", false),
            ("status", true, "status", true),
        ] {
            assert_eq!(
                ordem_de(entrada, desc, Some(&schema)).unwrap(),
                (coluna.to_string(), direcao),
                "{entrada} com ordemDesc={desc}"
            );
        }
        assert_eq!(
            ordem_de("status DESC", false, None).unwrap(),
            ("status".to_string(), true),
            "sem schema, a compatibilidade continua separando o sufixo"
        );
    }

    /// O envelope documentado carrega operador e valor; objetos comuns continuam
    /// sendo igualdade, e envelope malformado não degenera em subdocumento.
    #[test]
    fn o_filtro_aceita_op_e_valor_sem_afrouxar_a_validacao() {
        let filtro = filtro_de(Some(&json!({
            "saldo": { "op": "maior", "valor": 10 },
            "nome": { "op": "contendo", "valor": "a" },
            "documento": { "a": 1 },
            "metadados": { "valor": 1, "x": 2 },
        })))
        .unwrap();
        assert_eq!(filtro.len(), 4);
        assert_eq!(filtro[0].coluna, "saldo");
        assert_eq!(filtro[0].comparacao, Comparacao::Maior);
        assert_eq!(filtro[0].valor, json!(10));
        assert_eq!(filtro[1].comparacao, Comparacao::Contendo);
        assert_eq!(filtro[1].valor, json!("a"));
        assert_eq!(filtro[2].comparacao, Comparacao::Igual);
        assert_eq!(filtro[2].valor, json!({ "a": 1 }));
        assert_eq!(filtro[3].comparacao, Comparacao::Igual);
        assert_eq!(filtro[3].valor, json!({ "valor": 1, "x": 2 }));

        for invalido in [
            json!([]),
            json!({ "saldo": { "valor": 10 } }),
            json!({ "saldo": { "op": 1, "valor": 10 } }),
            json!({ "saldo": { "op": "maior", "valor": 10, "extra": 1 } }),
            json!({ "saldo": { "op": "maior", "outro": 10 } }),
        ] {
            assert!(filtro_de(Some(&invalido)).is_err(), "{invalido}");
        }
        let erro = filtro_de(Some(&json!({ "saldo": { "op": "desconhecida", "valor": 10 } })))
            .unwrap_err()
            .to_string();
        assert!(erro.contains("desconhecida"), "{erro}");

        // Compatibilidade anterior: operador sem valor continuava sendo nulo.
        let antigo = filtro_de(Some(&json!({ "saldo": { "op": "igual" } }))).unwrap();
        assert_eq!(antigo[0].comparacao, Comparacao::Igual);
        assert_eq!(antigo[0].valor, Value::Null);
    }

    #[test]
    fn o_erro_de_coluna_lista_o_schema_e_explica_a_ordem() {
        let schema = vec![
            ("id".to_string(), "integer".to_string()),
            ("qt_venda".to_string(), "integer".to_string()),
        ];
        let erro = erro_coluna_invalida("qt_venda DESC", &schema).to_string();
        for trecho in [
            "qt_venda DESC",
            "id",
            "qt_venda",
            "ordem",
            "ordemDesc",
            "bancos_colunas",
        ] {
            assert!(erro.contains(trecho), "{erro}");
        }
    }

    /// O teto é `limite + 1` e não `limite`: a linha extra é a única prova de
    /// que a leitura parou no teto em vez de parar no fim do banco.
    #[test]
    fn o_teto_de_leitura_deixa_uma_linha_de_barriga() {
        assert_eq!(teto_de_leitura(0), 1, "limite zero ainda pergunta se havia linha");
        assert_eq!(teto_de_leitura(200), 201, "o teto do cadastro mais a prova do corte");
        assert_eq!(
            teto_de_leitura(usize::MAX),
            usize::MAX,
            "somar sem estourar: teto estourado leria o banco inteiro"
        );
    }

    /// aparar_no_teto é o corte que os seis motores partilham — um `>` virado
    /// `<` num deles diria «truncado» numa leitura que coube inteira.
    #[test]
    fn aparar_no_teto_corta_e_diz_se_sobrou() {
        assert_eq!(aparar_no_teto(vec![1, 2], 2), (vec![1, 2], false));
        assert_eq!(aparar_no_teto(vec![1, 2, 3], 2), (vec![1, 2], true));
        assert_eq!(aparar_no_teto(Vec::<u8>::new(), 5), (Vec::new(), false));
    }

    /// O prazo chega ao usuário: a mensagem carrega o número, não só a palavra.
    #[test]
    fn o_prazo_aparece_na_mensagem_que_o_modelo_le() {
        assert_eq!(PRAZO_DE_LEITURA, Duration::from_secs(15));
        assert_eq!(prazo_em_ms(), 15_000, "o texto que vai ao servidor é em ms");
        let texto = erro_de_prazo().to_string();
        assert!(texto.contains("15"), "o erro do prazo tem de dizer o prazo: {texto}");
    }

    /// A peça que era montada a cada chamada nasce uma vez só por processo, e é
    /// a mesma para todos os motores de rede.
    #[test]
    fn o_agendador_nasce_uma_vez_para_o_processo_inteiro() {
        let primeira = agendador().expect("o agendador nasce");
        for _ in 0..8 {
            let outra = agendador().expect("o agendador nasce");
            assert!(
                std::ptr::eq(primeira, outra),
                "cada chamada reconstroi o runtime, que é justamente o defeito"
            );
        }
        assert_eq!(
            AGENDADORES_NASCIDOS.load(Ordering::SeqCst),
            1,
            "o processo montou {} agendadores: um por chamada é o defeito que era",
            AGENDADORES_NASCIDOS.load(Ordering::SeqCst)
        );
        assert!(
            matches!(primeira.handle().runtime_flavor(), tokio::runtime::RuntimeFlavor::MultiThread),
            "com uma thread só, o driver da conexão e a consulta disputam o mesmo fio"
        );
    }
}
