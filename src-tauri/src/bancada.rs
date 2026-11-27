//! A bancada.
//!
//! Não é uma tela de números decorativos: cada caso roda contra o modelo que
//! está em memória, na placa que o roteador escolheu, e o veredito é a conta que
//! saiu daquela execução. Os casos foram escolhidos um a um porque cada um deles
//! já quebrou nesta aplicação — a faixa que dizia "runtime incompleto" com os 19
//! DLLs presentes, o segundo turno que estourava a janela porque o cache não foi
//! esquecido, o prompt de 29.777 tokens recalculado inteiro a cada rodada de
//! ferramenta. Um caso que não fica vermelho quando o defeito volta não é um
//! teste: é enfeite, e por isso cada um foi rodado com o defeito reinstalado
//! antes de entrar aqui.
//!
//! O custo é o de uma conversa de três rodadas: os casos que geram usam teto
//! pequeno e o prompt do aplicativo, não uma carga arbitrária.

use serde::Serialize;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, MutexGuard};
use std::time::{Duration, Instant};

use crate::ai::alojamento::Capacidade;
use crate::ai::catalog::ModelSpec;
use crate::ai::compactacao::{self, Resumo};
use crate::ai::session::{encaixe, Encaixe, TURN_MARKER, system_prompt};
use crate::ai::{ChatMessage, ChatTemplate, Emission, Engine, Generation, SamplingParams, StopReason, TurnConfig};
use crate::idiomas::{self, tr};
use crate::mcp::{McpManager, servidor};
use crate::runtime;
use crate::state::{AiRuntime, LoadedModel};

/// Os casos da suíte, na ordem em que a tela os mostra.
pub const CASOS: &[&str] = &[
    "placa",
    "runtime",
    "mcp",
    "bancos",
    "esquecer",
    "prefixo",
    "parada",
    "fluxo",
    "dobrar",
];

/// Quanto a bancada espera uma resposta crua da porta publicada antes de dizer
/// que ela não respondeu. Um handshake local leva milissegundos; dez segundos já
/// é a porta mudando de assunto.
const ESPERA_PORTA: Duration = Duration::from_secs(10);

/// As dobras que `caso_dobrar` tem permissão para fazer antes de dar o caso por
/// reprovado. A conversa montada dobra em duas; o resto é folga.
const DOBRAS_MAX: usize = 4;

/// O que a tela mostra de um caso: nunca uma opinião sem o número atrás.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Medida {
    /// Chave de i18n do rótulo — a tela traduz, o motor só sabe o valor.
    pub rotulo: String,
    pub valor: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Estado {
    /// Rodou e a conta bateu com o que o caso exige.
    Aprovado,
    /// Rodou e não bateu. É o defeito de volta.
    Reprovado,
    /// Não tinha como rodar aqui: falta modelo, ou o caminho que o caso mede não
    /// existe nesta máquina. Contar isto como aprovado seria a bancada mentir.
    NaoRodado,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Resultado {
    pub id: String,
    pub estado: Estado,
    pub medidas: Vec<Medida>,
    /// Texto já traduzido do lado do motor: o motivo da recusa da placa, a
    /// reclamação da biblioteca. `None` quando não há o que explicar.
    pub detalhe: Option<String>,
    pub ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Evento {
    Iniciou { total: usize, modelo: Option<String> },
    Caso { id: String },
    Resultado(Resultado),
}

fn veredito(id: &str, estado: Estado, medidas: Vec<Medida>, detalhe: Option<String>, inicio: Instant) -> Resultado {
    Resultado {
        id: id.to_string(),
        estado,
        medidas,
        detalhe,
        ms: inicio.elapsed().as_millis() as u64,
    }
}

fn medida(rotulo: &str, valor: impl Into<String>) -> Medida {
    Medida { rotulo: rotulo.to_string(), valor: valor.into() }
}

/// As chaves sem parâmetro. `tr` exige a lista de args, e escrever `&[]` vinte
/// vezes só faz ruído na leitura do que importa, que é a conta.
fn t(chave: &str) -> String {
    tr(chave, &[])
}

/// A bancada devolve texto pronto, e a janela é do dono da máquina: um número
/// com ponto numa tela em português é a interface falando outra língua.
fn separador_decimal() -> (char, char) {
    if idiomas::atual().starts_with("pt") { (',', '.') } else { ('.', ',') }
}

fn inteiro(n: usize) -> String {
    let (_, agrupador) = separador_decimal();
    let algarismos = n.to_string();
    algarismos
        .as_bytes()
        .rchunks(3)
        .rev()
        .map(|c| String::from_utf8_lossy(c).to_string())
        .collect::<Vec<_>>()
        .join(&agrupador.to_string())
}

fn fracionario(valor: f64, casas: usize) -> String {
    let (decimal, _) = separador_decimal();
    format!("{valor:.casas$}").replace('.', &decimal.to_string())
}

/// O caso não passou porque o motor recusou alguma coisa — tokenizer, template,
/// janela. Não é o mesmo que achar o defeito, e a tela diz qual dos dois foi.
fn quebrou(id: &str, erro: &str, inicio: Instant) -> Resultado {
    veredito(id, Estado::Reprovado, Vec::new(), Some(erro.to_string()), inicio)
}

fn cancelado(id: &str, inicio: Instant) -> Resultado {
    veredito(id, Estado::NaoRodado, Vec::new(), Some(t("bancada.cancelado")), inicio)
}

/// As escalas de um caso: guloso onde o caso precisa de resposta sempre igual,
/// teto pequeno onde ele não precisa de conversa longa.
fn escala(teto: usize) -> SamplingParams {
    SamplingParams { temperature: 0.0, max_tokens: teto, ..SamplingParams::default() }
}

/// As sequências que encerram uma resposta: as que o arquivo publica mais o
/// marcador de turno, exatamente como a sessão passa. Copiar a lista daqui é o
/// que faz a bancada medir o mesmo caminho do chat.
fn paradas(engine: &Engine) -> Vec<String> {
    let mut stop: Vec<String> = engine.paradas().to_vec();
    if !stop.iter().any(|s| s == TURN_MARKER) {
        stop.push(TURN_MARKER.to_string());
    }
    stop
}

/// Renderiza e tokeniza pela template do modelo, como um turno real. Um prompt
/// montado à mão mediria outra coisa.
fn ids_de(engine: &Engine, template: &ChatTemplate, mensagens: &[ChatMessage]) -> anyhow::Result<Vec<u32>> {
    let texto = template.render(mensagens, &[], false)?;
    Ok(engine
        .tokenizer()
        .encode(texto, false)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .get_ids()
        .to_vec())
}

fn pergunta(texto: &str) -> Vec<ChatMessage> {
    vec![ChatMessage::new("system", system_prompt()), ChatMessage::new("user", texto)]
}

/// O histórico de um turno de ferramenta: o que o modelo respondeu e o que a
/// ferramenta devolveu voltam como mensagens — é isso que faz o prompt da rodada
/// seguinte começar exatamente onde o da anterior acabou.
fn com_resposta(base: &[ChatMessage], resposta: &str, ferramenta: &str, nova: &str) -> Vec<ChatMessage> {
    let mut v = base.to_vec();
    v.push(ChatMessage::new("assistant", resposta));
    v.push(ChatMessage::new("tool", ferramenta));
    v.push(ChatMessage::new("user", nova));
    v
}

/// Uma rodada. `Cancelada` não é reprovação: o dono da máquina mandou parar.
enum Rodada {
    Feita(Generation),
    Cancelada,
}

fn gerar(engine: &mut Engine, prompt_ids: &[u32], teto: usize, cancel: &AtomicBool) -> anyhow::Result<Rodada> {
    let g = engine.generate(prompt_ids, &escala(teto), &paradas(engine), cancel, |_| {})?;
    if g.stop == StopReason::Cancelled {
        return Ok(Rodada::Cancelada);
    }
    Ok(Rodada::Feita(g))
}

fn motivo_parada(parada: &StopReason) -> String {
    // As mesmas palavras da barra de medidas do chat: a bancada explica o que a
    // tela do turno já disse, não um vocabulário paralelo.
    match parada {
        StopReason::Eos => t("conversa.paradaFim"),
        StopReason::StopSequence => t("conversa.paradaSequencia"),
        StopReason::MaxTokens => t("conversa.paradaLimite"),
        StopReason::Cancelled => t("conversa.paradaCancelada"),
        StopReason::ContextFull => t("conversa.paradaJanela"),
    }
}

// ─── Os casos ─────────────────────────────────────────────────────────────────

/// Placa primeiro é a regra do produto. O caso não pergunta se existe placa:
/// pergunta se a conta foi para a placa que existe.
fn caso_placa(engine: &Engine, capacidade: Option<&Capacidade>, inicio: Instant) -> Resultado {
    let acelerador = engine.acelerador();
    let mut medidas = vec![
        medida(
            "bancada.medida.motor",
            if acelerador.tipo == "gpu" { t("bancada.motor.placa") } else { t("bancada.motor.processador") },
        ),
        medida(
            "bancada.medida.placa",
            if acelerador.dispositivo.is_empty() {
                t("bancada.placa.nenhuma")
            } else {
                acelerador.dispositivo.clone()
            },
        ),
    ];
    if let Some(alojado) = &acelerador.alojado {
        // O cedido é a VRAM que sumiu entre antes e depois da carga. Onde a
        // carga não pousou em placa não há este lado, e escrevê-lo como 0 MB
        // seria a bancada medindo o que a máquina não tem.
        if let Some(cedido) = alojado.cedido {
            medidas.push(medida("bancada.medida.cedido", fracionario(cedido as f64 / 1_048_576.0, 1)));
        }
        medidas.push(medida(
            "bancada.medida.previsao",
            match &alojado.previsto {
                crate::ai::alojamento::Alojamento::Inteira { .. } => t("bancada.previsao.inteira"),
                crate::ai::alojamento::Alojamento::Dividida { camadas, total, .. } => tr(
                    "bancada.previsao.dividida",
                    &[("camadas", &inteiro(*camadas)), ("total", &inteiro(*total))],
                ),
                crate::ai::alojamento::Alojamento::SoSistema { .. } => t("bancada.previsao.sistema"),
            },
        ));
    }
    match (acelerador.tipo == "gpu", capacidade) {
        (true, _) => veredito("placa", Estado::Aprovado, medidas, None, inicio),
        // A placa existe no registro e a conta não foi nela: é a regra do
        // produto quebrada, e o motivo que o motor guardou é o que serve para
        // arrumar.
        (false, Some(_)) => {
            let motivo = acelerador.motivo.clone().unwrap_or_else(|| t("bancada.placa.semMotivo"));
            veredito("placa", Estado::Reprovado, medidas, Some(motivo), inicio)
        }
        // Sem placa nesta máquina a CPU é a resposta certa, e o motivo dito pelo
        // motor acompanha a medida.
        (false, None) => veredito("placa", Estado::Aprovado, medidas, acelerador.motivo.clone(), inicio),
    }
}

/// As bibliotecas nativas: presentes nas pastas do carregador e com o hash da
/// release que este código executa. É a pergunta de um PC recém-instalado.
fn caso_runtime(ai: &AiRuntime, inicio: Instant) -> Resultado {
    let estados = runtime::estado(&ai.dlls);
    let auditoria = runtime::verificar(&ai.dlls);
    let medidas: Vec<Medida> = estados
        .iter()
        .map(|estado| {
            let batem = auditoria
                .iter()
                .find(|v| v.id == estado.id)
                .map(|v| v.arquivos.iter().filter(|a| a.bateu).count())
                .unwrap_or(0);
            medida(
                "bancada.medida.bibliotecas",
                format!("{} · {}/{}", estado.nome, inteiro(batem), inteiro(estado.arquivos.len())),
            )
        })
        .collect();
    let faltando: Vec<String> = estados
        .iter()
        .flat_map(|e| e.faltando.iter().map(|n| format!("{} · {n}", e.id)))
        .collect();
    let divergentes: Vec<String> = auditoria
        .iter()
        .flat_map(|v| {
            v.arquivos.iter().filter(|a| a.presente && !a.bateu).map(|a| format!("{} · {}", v.id, a.nome))
        })
        .collect();
    if !faltando.is_empty() {
        return veredito(
            "runtime",
            Estado::Reprovado,
            medidas,
            Some(tr("bancada.runtime.faltando", &[("lista", &faltando.join(", "))])),
            inicio,
        );
    }
    if !divergentes.is_empty() {
        return veredito(
            "runtime",
            Estado::Reprovado,
            medidas,
            Some(tr("bancada.runtime.hashDivergente", &[("lista", &divergentes.join(", "))])),
            inicio,
        );
    }
    veredito("runtime", Estado::Aprovado, medidas, None, inicio)
}

/// A bancada roda num fio de bloqueio e a conexão MCP só se mede esperando.
/// Emendar os dois pela runtime do próprio aplicativo é o que faz o caso medir a
/// mesma ligação que o botão da tela e a ferramenta do agente usam: as tarefas do
/// cliente continuam vivas na runtime do aplicativo quando o caso acaba, em vez
/// de morrerem junto com uma runtime emprestada.
fn na_runtime_aplicativo<T, F>(fut: F) -> anyhow::Result<T>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let handle = tokio::runtime::Handle::try_current()
        .map_err(|_| anyhow::anyhow!(t("bancada.mcp.semPonte")))?;
    let (tx, rx) = std::sync::mpsc::channel();
    handle.spawn(async move {
        let _ = tx.send(fut.await);
    });
    rx.recv().map_err(|_| anyhow::anyhow!(t("bancada.mcp.semPonte")))
}

/// Um POST cru, sem cabeçalho de autenticação, direto na porta publicada. O que
/// ela responde é a prova de que o segredo é cobrado antes de qualquer
/// ferramenta — não uma opinião sobre o código que faz a cobrança.
async fn porta_sem_token(porta: u16) -> anyhow::Result<u16> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let corpo = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2026-07-28","capabilities":{},"clientInfo":{"name":"bancada","version":"0"}}}"#;
    let pedido = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{porta}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{corpo}",
        corpo.len()
    );
    let mut fluxo = tokio::net::TcpStream::connect(("127.0.0.1", porta)).await?;
    fluxo.write_all(pedido.as_bytes()).await?;
    let mut resposta = Vec::new();
    fluxo.read_to_end(&mut resposta).await?;
    let texto = String::from_utf8_lossy(&resposta);
    let linha = texto.split_once("\r\n").map(|(c, _)| c).unwrap_or_default();
    linha
        .split_whitespace()
        .nth(1)
        .and_then(|n| n.parse::<u16>().ok())
        .ok_or_else(|| anyhow::anyhow!(t("bancada.mcp.semResposta")))
}

/// A conexão MCP local só vale se um cliente de verdade entra na porta e traz o
/// catálogo de volta. O caso publica pelo gerente — o mesmo caminho da tela e do
/// agente — e lê o que saiu da fiação: quantas ferramentas o cliente anunciou de
/// volta e o que a porta diz a quem chega sem segredo. Rodar uma ferramenta aqui
/// dependeria de o dono da máquina aprovar aquele instante, e a bancada não pede
/// favor a ninguém: o que ela mede é a conexão. O que o caso abriu, o caso fecha.
async fn medir_mcp(gerente: &McpManager, antes: bool) -> anyhow::Result<(Vec<Medida>, Option<String>)> {
    let aberto = gerente.publicar().await?;
    let esperadas = crate::local::catalogo().len() + crate::bancos::ferramentas::catalogo().len();
    let anunciadas = gerente.anunciadas(servidor::CONECTOR).len();
    let recusado = match tokio::time::timeout(ESPERA_PORTA, porta_sem_token(aberto.porta)).await {
        Ok(Ok(status)) => Ok(status),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err(t("bancada.mcp.semResposta")),
    };
    if !antes {
        gerente.despublicar().await.ok();
    }
    let medidas = vec![
        medida("bancada.medida.porta", aberto.porta.to_string()),
        medida("bancada.medida.anunciadas", inteiro(anunciadas)),
        medida("bancada.medida.catalogo", inteiro(esperadas)),
        medida(
            "bancada.medida.semSegredo",
            match &recusado {
                Ok(status) => status.to_string(),
                Err(_) => t("bancada.mcp.semResposta"),
            },
        ),
    ];
    let motivo = match &recusado {
        Err(texto) => Some(texto.clone()),
        // 401 é o que a placa dá para um estranho: qualquer outra linha de
        // status, inclusive um 200, é a porta aberta sem segredo.
        Ok(status) if *status != 401 => Some(tr(
            "bancada.mcp.abriuSemSegredo",
            &[("status", &status.to_string())],
        )),
        Ok(_) if esperadas == 0 || anunciadas != esperadas => Some(tr(
            "bancada.mcp.anuncioFalso",
            &[("anunciadas", &inteiro(anunciadas)), ("esperadas", &inteiro(esperadas))],
        )),
        Ok(_) => None,
    };
    Ok((medidas, motivo))
}

fn caso_mcp(gerente: &Arc<McpManager>, inicio: Instant) -> Resultado {
    let antes = gerente.estado_servidor().ativa;
    let feito = na_runtime_aplicativo({
        let gerente = Arc::clone(gerente);
        async move { medir_mcp(&gerente, antes).await }
    })
    .flatten();
    match feito {
        Err(e) => quebrou("mcp", &e.to_string(), inicio),
        Ok((medidas, Some(motivo))) => veredito("mcp", Estado::Reprovado, medidas, Some(motivo), inicio),
        Ok((medidas, None)) => veredito("mcp", Estado::Aprovado, medidas, None, inicio),
    }
}

/// A política de leitura é a que toda conexão nova traz, e o caso mede o que
/// ela de fato faz num banco real de SQLite: a leitura passa e é cortada pelo
/// teto que o cadastro determina, cada escrita é recusada antes de o motor
/// abrir, e o banco sai com as linhas de onde começou — recusa que escreve
/// seria o pior defeito desta porta.
fn caso_bancos(inicio: Instant) -> Resultado {
    use crate::bancos::config::{Config, Conexao, Fonte, Tipo};
    use crate::bancos::ferramentas::executar;
    use std::collections::BTreeMap;

    let raiz = std::env::temp_dir().join(format!("aether-bancada-bancos-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&raiz);
    let limpar = || {
        let _ = std::fs::remove_dir_all(&raiz);
    };

    let montagem = || -> anyhow::Result<std::path::PathBuf> {
        std::fs::create_dir_all(&raiz)?;
        let arquivo = raiz.join("estoque.db");
        let sqlite = rusqlite::Connection::open(&arquivo)?;
        sqlite.execute_batch(
            "CREATE TABLE clientes (id INTEGER PRIMARY KEY, nome TEXT NOT NULL);
             INSERT INTO clientes (nome) VALUES ('Ana'), ('Bruno');",
        )?;
        drop(sqlite);
        let mut config = Config::default();
        let mut conexao = Conexao {
            id: "bd-bancada".into(),
            nome: "Estoque".into(),
            tipo: Tipo::Sqlite,
            host: String::new(),
            porta: None,
            usuario: String::new(),
            banco: String::new(),
            arquivo: arquivo.display().to_string(),
            senha_cifrada: None,
            senha_clara: None,
            // O teto de uma linha é o que deixa a medida do corte visível: o
            // pedido pede tudo, o cadastro devolve uma.
            limite_linhas: 1,
            sql_livre: false,
            tls_verificado: false,
            fontes: BTreeMap::new(),
        };
        conexao.fontes.insert("clientes".into(), Fonte::default());
        config.conexoes.push(conexao);
        let caminho = raiz.join("bancos.json");
        config.save(&caminho)?;
        Ok(caminho)
    }();

    let caminho = match montagem {
        Ok(caminho) => caminho,
        Err(e) => {
            limpar();
            return quebrou("bancos", &e.to_string(), inicio);
        }
    };

    // Cada passo vira uma medida ou um motivo de reprovação: o caso continua
    // mesmo depois de um passo falhar, para o veredito carregar tudo o que deu.
    let mut medidas = Vec::new();
    let mut motivo: Option<String> = None;

    let leitura = executar(
        "bancos_consultar",
        serde_json::json!({ "conexao": "Estoque", "fonte": "clientes", "limite": 9999 }),
        &caminho,
    );
    match leitura {
        Err(e) => motivo = Some(e.to_string()),
        Ok(saida) => {
            let json = saida.vista.para_json();
            let lidas = json["total"].as_u64().unwrap_or(0);
            let cortado = json["truncado"].as_bool().unwrap_or(false);
            medidas.push(medida("bancada.medida.bancosLidas", inteiro(lidas as usize)));
            if lidas != 1 && motivo.is_none() {
                motivo = Some(tr(
                    "bancada.bancos.leitura",
                    &[("lidas", &lidas.to_string())],
                ));
            }
            if !cortado && motivo.is_none() {
                motivo = Some(t("bancada.bancos.teto"));
            }
        }
    }

    let mut recusadas = 0usize;
    for (op, argumentos) in [
        (
            "inserir",
            serde_json::json!({ "conexao": "Estoque", "fonte": "clientes", "valores": { "nome": "Recusada" } }),
        ),
        (
            "atualizar",
            serde_json::json!({ "conexao": "Estoque", "fonte": "clientes", "valores": { "nome": "Recusada" }, "filtro": { "id": 1 } }),
        ),
        ("apagar", serde_json::json!({ "conexao": "Estoque", "fonte": "clientes", "filtro": { "id": 1 } })),
        (
            "sql",
            serde_json::json!({ "conexao": "Estoque", "sql": "INSERT INTO clientes (nome) VALUES ('Livre')" }),
        ),
    ] {
        let ferramenta = format!("bancos_{op}");
        let negada = executar(&ferramenta, argumentos, &caminho).is_err();
        if negada {
            recusadas += 1;
        } else if motivo.is_none() {
            motivo = Some(tr("bancada.bancos.escrita", &[("op", op)]));
        }
    }
    medidas.push(medida("bancada.medida.bancosRecusadas", inteiro(recusadas)));
    if recusadas != 4 && motivo.is_none() {
        motivo = Some(tr("bancada.bancos.recusas", &[("recusadas", &recusadas.to_string())]));
    }

    let contagem = executar(
        "bancos_contar",
        serde_json::json!({ "conexao": "Estoque", "fonte": "clientes" }),
        &caminho,
    );
    match contagem {
        Err(e) if motivo.is_none() => motivo = Some(e.to_string()),
        Err(_) => {}
        Ok(saida) => {
            let json = saida.vista.para_json();
            let ficaram = json["linhas"].as_u64().unwrap_or(0);
            medidas.push(medida("bancada.medida.bancosFicaram", inteiro(ficaram as usize)));
            if ficaram != 2 && motivo.is_none() {
                motivo = Some(tr("bancada.bancos.bancoMudou", &[("linhas", &ficaram.to_string())]));
            }
        }
    }

    limpar();
    match motivo {
        Some(motivo) => veredito("bancos", Estado::Reprovado, medidas, Some(motivo), inicio),
        None => veredito("bancos", Estado::Aprovado, medidas, None, inicio),
    }
}

/// Trocar de conversa tem de esquecer a conversa anterior. O caso enche o cache
/// com um prompt longo, manda esquecer e refaz a pergunta curta do começo: a
/// resposta tem de ser a mesma de quando o modelo estava novo, e o prefill não
/// pode reaproveitar nada do que foi esquecido. Foi assim que apareceu o
/// `seq_rm(-1)` recusado desta build — cache de pé e ninguém dizendo.
fn caso_esquecer(
    engine: &mut Engine,
    template: &ChatTemplate,
    cancel: &AtomicBool,
    inicio: Instant,
) -> anyhow::Result<Resultado> {
    let curta = ids_de(engine, template, &pergunta("Qual é a capital do Brasil? Responda com uma palavra."))?;
    // Um quarto da janela: o suficiente para que posições que não voltaram a
    // zero empurrem a rodada seguinte para fora dela.
    let alvo = (engine.context() / 4).max(curta.len() + 64);
    let linha = t("bancada.enchimento");
    let mut enchimento = String::new();
    while engine.count_tokens(&enchimento).unwrap_or(usize::MAX) < alvo && enchimento.len() < 2_000_000 {
        enchimento.push_str(&linha);
        enchimento.push('\n');
    }
    let longo = ids_de(engine, template, &pergunta(&enchimento))?;

    engine.reset();
    let Rodada::Feita(antes) = gerar(engine, &curta, 24, cancel)? else {
        return Ok(cancelado("esquecer", inicio));
    };
    let Rodada::Feita(cheio) = gerar(engine, &longo, 1, cancel)? else {
        return Ok(cancelado("esquecer", inicio));
    };
    let ocupadas = engine.cache_ocupado();
    engine.reset();
    let Rodada::Feita(refeita) = gerar(engine, &curta, 24, cancel)? else {
        return Ok(cancelado("esquecer", inicio));
    };

    let medidas = vec![
        medida("bancada.medida.enchimento", inteiro(cheio.prompt_tokens)),
        medida(
            "bancada.medida.cacheOcupado",
            ocupadas.map(inteiro).unwrap_or_else(|| t("bancada.medida.naoMedido")),
        ),
        medida("bancada.medida.reaproveitados", inteiro(refeita.reused_tokens)),
        medida("bancada.medida.prompt", inteiro(refeita.prompt_tokens)),
    ];
    // A prova de que esqueceu é a resposta, não o contador: um `limpar_cache`
    // que virou no-op deixaria `reused` em zero e o KV cheio do texto antigo.
    if refeita.text == antes.text && refeita.reused_tokens == 0 && refeita.stop != StopReason::ContextFull {
        return Ok(veredito("esquecer", Estado::Aprovado, medidas, None, inicio));
    }
    let motivo = if refeita.text != antes.text {
        tr("bancada.esquecer.divergiu", &[("antes", &antes.text), ("depois", &refeita.text)])
    } else {
        t("bancada.esquecer.naoEsqueceu")
    };
    Ok(veredito("esquecer", Estado::Reprovado, medidas, Some(motivo), inicio))
}

/// O começo do prompt não é recalculado. Três rodadas do mesmo histórico: a
/// primeira não tem o que reaproveitar, a segunda é a primeira mais o resultado
/// da ferramenta — e é quase inteira reaproveitável —, a terceira volta no meio
/// do histórico, onde o cache precisa ser recortado em vez de jogado fora. O
/// braço do defeito refaz a segunda rodada com o cache esquecido, e os dois têm
/// de responder a mesma coisa: poupar o prefixo não pode mudar a resposta.
fn caso_prefixo(
    engine: &mut Engine,
    template: &ChatTemplate,
    cancel: &AtomicBool,
    inicio: Instant,
) -> anyhow::Result<Resultado> {
    if !engine.usa_ggml() {
        return Ok(veredito(
            "prefixo",
            Estado::NaoRodado,
            Vec::new(),
            Some(t("bancada.prefixo.soForaDoGgml")),
            inicio,
        ));
    }
    let uma = pergunta("Diga três frutas. Uma por linha.");
    let uma_ids = ids_de(engine, template, &uma)?;
    let teto = 32usize;

    engine.reset();
    let Rodada::Feita(r1) = gerar(engine, &uma_ids, teto, cancel)? else {
        return Ok(cancelado("prefixo", inicio));
    };
    let duas = com_resposta(&uma, &r1.text, "Ferramenta: três frutas registradas.", "E a capital da França?");
    let duas_ids = ids_de(engine, template, &duas)?;
    let Rodada::Feita(r2) = gerar(engine, &duas_ids, teto, cancel)? else {
        return Ok(cancelado("prefixo", inicio));
    };
    // A terceira rodada diverge no meio: a resposta do assistente é outra, o
    // começo continua o mesmo. É o caso em que recortar sai mais barato que
    // esquecer, e o único que exercita o recorte de verdade.
    let tres = com_resposta(
        &uma,
        "Manga, graviola, cupuaçu.",
        "Ferramenta: três frutas registradas.",
        "E a capital do Japão?",
    );
    let tres_ids = ids_de(engine, template, &tres)?;
    let ocupadas = engine.cache_ocupado().unwrap_or(0);
    let Rodada::Feita(r3) = gerar(engine, &tres_ids, teto, cancel)? else {
        return Ok(cancelado("prefixo", inicio));
    };
    // O braço do defeito: o mesmo prompt da segunda rodada, com o cache
    // esquecido. O custo é um prefill inteiro — exatamente o que o
    // reaproveitamento poupa.
    engine.reset();
    let Rodada::Feita(sem_cache) = gerar(engine, &duas_ids, teto, cancel)? else {
        return Ok(cancelado("prefixo", inicio));
    };

    let poupados = sem_cache.prompt_ms.saturating_sub(r2.prompt_ms);
    let vezes = sem_cache.prompt_ms.max(1) as f64 / r2.prompt_ms.max(1) as f64;
    let medidas = vec![
        medida("bancada.medida.prompt", format!("{} → {}", inteiro(uma_ids.len()), inteiro(duas_ids.len()))),
        medida(
            "bancada.medida.reaproveitados",
            format!("{} / {}", inteiro(r2.reused_tokens), inteiro(duas_ids.len())),
        ),
        medida("bancada.medida.recorte", format!("{} → {}", inteiro(ocupadas), inteiro(r3.reused_tokens))),
        medida(
            "bancada.medida.recorteDecisao",
            if ocupadas == 0 {
                t("bancada.prefixo.recorteNada")
            } else if r3.recorte_recusado {
                t("bancada.prefixo.recorteRecusado")
            } else {
                t("bancada.prefixo.recorteFeito")
            },
        ),
        medida(
            "bancada.medida.prefill",
            format!("{} · {}", inteiro(r2.prompt_ms as usize), inteiro(sem_cache.prompt_ms as usize)),
        ),
        medida("bancada.medida.vezes", format!("{}×", fracionario(vezes, 1))),
        medida("bancada.medida.poupados", inteiro(poupados as usize)),
    ];
    let mut falhas: Vec<String> = Vec::new();
    if r1.reused_tokens != 0 {
        falhas.push(tr("bancada.prefixo.cacheVelho", &[("n", &inteiro(r1.reused_tokens))]));
    }
    if r2.reused_tokens * 2 < uma_ids.len() {
        falhas.push(tr(
            "bancada.prefixo.naoReaproveitou",
            &[("reaproveitados", &inteiro(r2.reused_tokens)), ("historico", &inteiro(uma_ids.len()))],
        ));
    }
    // Dois vermelhos são nossos: reaproveitar além do ponto em que o histórico
    // muda (o KV velho responde por células que já não existem no prompt) e
    // reaproveitar zero quando a memória devolveu `true` para o recorte. A
    // recusa dela própria não é um defeito nosso — é a metade recorrente de um
    // modelo híbrido, que só desfaz dentro dos instantâneos do último lote, e
    // por isso volta como medida, não como veredito.
    if ocupadas > 0 && r3.reused_tokens >= ocupadas {
        falhas.push(tr(
            "bancada.prefixo.alemDoPonto",
            &[("reaproveitados", &inteiro(r3.reused_tokens)), ("ocupadas", &inteiro(ocupadas))],
        ));
    } else if !r3.recorte_recusado && ocupadas > 0 && r3.reused_tokens == 0 {
        falhas.push(tr(
            "bancada.prefixo.semRecorte",
            &[("reaproveitados", &inteiro(r3.reused_tokens)), ("ocupadas", &inteiro(ocupadas))],
        ));
    }
    if r2.text != sem_cache.text {
        falhas.push(tr("bancada.prefixo.divergiu", &[("comCache", &r2.text), ("semCache", &sem_cache.text)]));
    }
    if r2.prompt_ms >= sem_cache.prompt_ms {
        falhas.push(tr(
            "bancada.prefixo.naoAcelerou",
            &[("comCache", &inteiro(r2.prompt_ms as usize)), ("semCache", &inteiro(sem_cache.prompt_ms as usize))],
        ));
    }
    if r2.text.contains('\u{fffd}') || r3.text.contains('\u{fffd}') {
        falhas.push(t("bancada.textoQuebrado"));
    }
    if falhas.is_empty() {
        return Ok(veredito("prefixo", Estado::Aprovado, medidas, None, inicio));
    }
    Ok(veredito("prefixo", Estado::Reprovado, medidas, Some(falhas.join(" · ")), inicio))
}

/// O modelo para onde o arquivo dele manda parar, e não no teto de tokens. Um
/// GGUF sem sequências de parada publicadas, ou uma resposta que vaza o marcador
/// de turno, é o assistente falando sozinho.
fn caso_parada(
    engine: &mut Engine,
    template: &ChatTemplate,
    cancel: &AtomicBool,
    inicio: Instant,
) -> anyhow::Result<Resultado> {
    let p = pergunta("Responda em uma frase: para que serve um martelo?");
    let prompt_ids = ids_de(engine, template, &p)?;
    engine.reset();
    let Rodada::Feita(g) = gerar(engine, &prompt_ids, 192, cancel)? else {
        return Ok(cancelado("parada", inicio));
    };
    let vazou = g.text.contains("<|");
    let medidas = vec![
        medida("bancada.medida.paradasDoModelo", inteiro(engine.paradas().len())),
        medida("bancada.medida.motivo", motivo_parada(&g.stop)),
        medida("bancada.medida.gerados", inteiro(g.generated_tokens)),
    ];
    if g.text.trim().is_empty() {
        return Ok(veredito("parada", Estado::Reprovado, medidas, Some(t("bancada.respostaVazia")), inicio));
    }
    if vazou {
        return Ok(veredito("parada", Estado::Reprovado, medidas, Some(t("bancada.parada.vazou")), inicio));
    }
    if matches!(g.stop, StopReason::Eos | StopReason::StopSequence) {
        return Ok(veredito("parada", Estado::Aprovado, medidas, None, inicio));
    }
    Ok(veredito("parada", Estado::Reprovado, medidas, Some(t("bancada.parada.noTeto")), inicio))
}

/// O texto que chega peça a peça é o texto que fica. Foi assim que
/// "arquitetura como" virou "arquiteturacomo": a cauda pendurada na decodificação
/// incremental era descartada quando o próximo delta saía.
fn caso_fluxo(
    engine: &mut Engine,
    template: &ChatTemplate,
    cancel: &AtomicBool,
    inicio: Instant,
) -> anyhow::Result<Resultado> {
    let p = pergunta("Explique em duas frases o que é uma arquitetura de software em camadas.");
    let prompt_ids = ids_de(engine, template, &p)?;
    engine.reset();
    let mut acumulado = String::new();
    let g = engine.generate(&prompt_ids, &escala(96), &paradas(engine), cancel, |emissao| match emissao {
        Emission::Delta(piece) => acumulado.push_str(&piece),
        Emission::Replace(texto) => acumulado = texto,
    })?;
    let medidas = vec![
        medida(
            "bancada.medida.caracteres",
            format!("{} / {}", inteiro(acumulado.chars().count()), inteiro(g.text.chars().count())),
        ),
        medida("bancada.medida.gerados", inteiro(g.generated_tokens)),
        medida("bancada.medida.velocidade", fracionario(g.decode_tokens_per_second(), 1)),
    ];
    if g.stop == StopReason::Cancelled {
        return Ok(cancelado("fluxo", inicio));
    }
    if g.text.contains('\u{fffd}') || acumulado.contains('\u{fffd}') {
        return Ok(veredito("fluxo", Estado::Reprovado, medidas, Some(t("bancada.textoQuebrado")), inicio));
    }
    if g.text.trim().is_empty() {
        return Ok(veredito("fluxo", Estado::Reprovado, medidas, Some(t("bancada.respostaVazia")), inicio));
    }
    if acumulado == g.text {
        return Ok(veredito("fluxo", Estado::Aprovado, medidas, None, inicio));
    }
    Ok(veredito("fluxo", Estado::Reprovado, medidas, Some(t("bancada.fluxo.divergiu")), inicio))
}

/// A conversa que não cabe e o aplicativo recusava em vez de continuar: foi assim
/// que o dono da máquina viu "a janela é de N tokens, aumente a janela nas
/// Escalas" no meio de um trabalho de verdade. O caso monta uma conversa maior
/// que a janela do motor e exige as três condições da correção — a primeira
/// decisão é dobrar (não é `SemLugar`, que é o banner na tela), o resumo sai do
/// modelo dentro da reserva que a dobra separou para ele, e relido com o resumo
/// dentro o prompt cabe. Com o `encaixe` de antes, o caso para no primeiro
/// `SemLugar` e diz a janela, o fixo e o pedido, que são exatamente os números do
/// banner.
fn caso_dobrar(
    engine: &mut Engine,
    template: &ChatTemplate,
    cancel: &AtomicBool,
    inicio: Instant,
) -> anyhow::Result<Resultado> {
    let janela = engine.context();
    let params = escala(256);
    let config = TurnConfig {
        tools: &[],
        params: &params,
        enable_thinking: false,
        max_rounds: 2,
        resumo: None,
        memoria: None,
        pastas: None,
        derrame: None,
    };
    // Um terço de janela por resposta, quatro respostas: a conta passa do limite
    // em qualquer janela, e dobra-se pouco mais de uma vez — que é o que uma
    // conversa longa faz. Um despejo só, do tamanho da janela inteira, não é o
    // caso: nenhum resumo caberia dentro do prompt que resume.
    let alvo = (janela / 3).max(64);
    let linha = t("bancada.enchimento");
    let mut corpo = String::new();
    while engine.count_tokens(&corpo).unwrap_or(usize::MAX) < alvo && corpo.len() < 2_000_000 {
        corpo.push_str(&linha);
        corpo.push('\n');
    }
    let mut history = vec![
        ChatMessage::new("system", system_prompt()),
        ChatMessage::new("user", "Comece um trabalho longo."),
    ];
    for _ in 0..4 {
        history.push(ChatMessage::new("assistant", corpo.as_str()));
        history.push(ChatMessage::new("user", "Continue de onde parou."));
    }

    engine.reset();
    let limite = compactacao::limite(janela, params.max_tokens);
    let mut resumo: Option<Resumo> = None;
    let mut dobras = 0usize;
    let mut tokens_do_resumo = 0usize;
    let decisão = loop {
        match encaixe(engine, template, &history, &config, None, resumo.as_ref())? {
            Encaixe::Cabe { ids, dobradas } => break Ok((ids.len(), dobradas)),
            Encaixe::SemLugar { janela, fixo, pedido } => break Err(tr(
                "bancada.dobrar.semLugar",
                &[
                    ("janela", &inteiro(janela)),
                    ("fixo", &inteiro(fixo)),
                    ("pedido", &inteiro(pedido)),
                ],
            )),
            Encaixe::Dobre { desde, ate } => {
                // Parar aqui com o histórico fora é o caso dizendo que a dobra
                // não converge — não é a bancada girando para sempre na placa.
                if dobras >= DOBRAS_MAX {
                    break Err(t("bancada.dobrar.parou"));
                }
                let anterior = resumo.as_ref().map(|r| r.texto.as_str());
                let (texto, gasto, lidas) = match compactacao::dobrar(
                    engine,
                    template,
                    &params,
                    anterior,
                    &history[desde..ate],
                    cancel,
                ) {
                    Ok(feita) => feita,
                    Err(erro) => {
                        if cancel.load(Ordering::Relaxed) {
                            return Ok(cancelado("dobrar", inicio));
                        }
                        return Err(erro);
                    }
                };
                dobras += 1;
                tokens_do_resumo += gasto.generated_tokens;
                resumo = Some(Resumo { texto, ate: desde + lidas });
            }
        }
    };
    let medidas = vec![
        medida("bancada.medida.janela", inteiro(janela)),
        medida("bancada.medida.limite", inteiro(limite)),
        medida("bancada.medida.dobras", inteiro(dobras)),
        medida("bancada.medida.gerados", inteiro(tokens_do_resumo)),
    ];
    let (prompt, dobradas) = match decisão {
        // Cabe no primeiro exame significa que a conversa montada não passou do
        // limite: o caso não mediu o que diz medir, e a bancada diz isso em vez de
        // aprovar um verde sem defeito nenhum ali.
        Ok((_, 0)) if dobras == 0 => {
            return Ok(veredito("dobrar", Estado::Reprovado, medidas, Some(t("bancada.dobrar.cabeu")), inicio));
        }
        Ok(feito) => feito,
        Err(motivo) => return Ok(veredito("dobrar", Estado::Reprovado, medidas, Some(motivo), inicio)),
    };
    let texto = resumo.map(|r| r.texto).unwrap_or_default();
    if prompt <= limite
        && dobradas > 0
        && !texto.trim().is_empty()
        && tokens_do_resumo <= compactacao::TETO_TOKENS * dobras.max(1)
    {
        return Ok(veredito("dobrar", Estado::Aprovado, medidas, None, inicio));
    }
    Ok(veredito("dobrar", Estado::Reprovado, medidas, Some(t("bancada.dobrar.estourou")), inicio))
}

// ─── A execução ───────────────────────────────────────────────────────────────

/// Roda a suíte inteira contra o modelo que o aplicativo está usando — ou contra
/// nenhum, quando a máquina ainda não tem pesos: aí os casos de motor dizem por
/// que não rodaram, em vez de sumirem da tela. A placa é lida antes de o motor
/// ser tomado, que é a mesma ordem do turno de conversa e o que evita dois donos
/// para o mesmo KV cache.
pub fn rodar(
    ai: &AiRuntime,
    gerente: &Arc<McpManager>,
    spec: Option<&'static ModelSpec>,
    cancel: &AtomicBool,
    mut emit: impl FnMut(Evento),
) -> Vec<Resultado> {
    emit(Evento::Iniciou { total: CASOS.len(), modelo: spec.map(|s| s.display.to_string()) });
    let capacidade = if spec.is_some() { crate::ai::alojamento::capacidade(&ai.dlls).ok().flatten() } else { None };
    // `runtime` confere arquivos em disco, `mcp` fala com a própria aplicação e
    // `bancos` abre um SQLite da bancada: nenhum dos três pede motor, e nenhum
    // deles espera pesos na placa.
    let outros = || CASOS.iter().filter(|id| !matches!(**id, "runtime" | "mcp" | "bancos"));

    let mut resultados = Vec::new();
    let inicio = Instant::now();
    emit(Evento::Caso { id: "runtime".to_string() });
    let r = caso_runtime(ai, inicio);
    emit(Evento::Resultado(r.clone()));
    resultados.push(r);

    let inicio = Instant::now();
    emit(Evento::Caso { id: "mcp".to_string() });
    let r = caso_mcp(gerente, inicio);
    emit(Evento::Resultado(r.clone()));
    resultados.push(r);

    let inicio = Instant::now();
    emit(Evento::Caso { id: "bancos".to_string() });
    let r = caso_bancos(inicio);
    emit(Evento::Resultado(r.clone()));
    resultados.push(r);

    let Some(escolhida) = spec else {
        for id in outros() {
            let r = veredito(id, Estado::NaoRodado, Vec::new(), Some(t("bancada.semModelo")), Instant::now());
            emit(Evento::Resultado(r.clone()));
            resultados.push(r);
        }
        return resultados;
    };

    // `acquire` carrega o modelo e devolve o guard do turno; um `Ok` sem modelo
    // dentro seria o motor tendo se recusado em silêncio.
    let mut guard: MutexGuard<'_, Option<LoadedModel>> = match ai.acquire(escolhida) {
        Ok(g) => g,
        Err(erro) => {
            let texto = erro.to_string();
            for id in outros() {
                let r = quebrou(id, &texto, Instant::now());
                emit(Evento::Resultado(r.clone()));
                resultados.push(r);
            }
            return resultados;
        }
    };
    let carregado = guard.as_mut().expect("acquire devolve o modelo carregado");
    for id in outros() {
        let inicio = Instant::now();
        emit(Evento::Caso { id: id.to_string() });
        let feito = if cancel.load(Ordering::Relaxed) {
            Ok(cancelado(id, inicio))
        } else {
            match *id {
                "placa" => Ok(caso_placa(&carregado.engine, capacidade.as_ref(), inicio)),
                "esquecer" => caso_esquecer(&mut carregado.engine, &carregado.template, cancel, inicio),
                "prefixo" => caso_prefixo(&mut carregado.engine, &carregado.template, cancel, inicio),
                "parada" => caso_parada(&mut carregado.engine, &carregado.template, cancel, inicio),
                "fluxo" => caso_fluxo(&mut carregado.engine, &carregado.template, cancel, inicio),
                "dobrar" => caso_dobrar(&mut carregado.engine, &carregado.template, cancel, inicio),
                outra => Err(anyhow::anyhow!(tr("bancada.casoDesconhecido", &[("id", outra)]))),
            }
        };
        let resultado = match feito {
            Ok(r) => r,
            Err(e) => quebrou(id, &e.to_string(), inicio),
        };
        emit(Evento::Resultado(resultado.clone()));
        resultados.push(resultado);
    }
    resultados
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn o_caso_de_bancos_aprova_a_politica_padrao_de_leitura() {
        let r = caso_bancos(Instant::now());
        assert_eq!(
            r.estado,
            Estado::Aprovado,
            "{}",
            r.detalhe.unwrap_or_default()
        );
        assert_eq!(r.medidas.len(), 3, "{:?}", r.medidas);
    }

    /// A tela nomeia cada caso por `bancada.caso.{id}`, chave montada em runtime
    /// a partir do `CASOS`. A paridade dos arquivos não alcança uma chave assim:
    /// falta ela nos dois idiomas, o nome do caso vira o próprio caminho na tela.
    #[test]
    fn todo_caso_da_bancada_tem_nome_em_todo_idioma() {
        for id in CASOS {
            let chave = format!("bancada.caso.{id}");
            assert!(crate::idiomas::tem_texto(&chave), "{id} apareceria na tela como {chave}");
        }
    }
}
