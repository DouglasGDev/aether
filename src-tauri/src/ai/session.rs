//! A sessão de conversa do Aether: o modelo local decide, o gate de permissões
//! autoriza, o servidor MCP executa e o resultado volta ao modelo.
//!
//! Nada aqui é simulado. Sem modelo carregado não há resposta, e uma ferramenta
//! que o gate negou volta ao modelo como erro, nunca como sucesso inventado.

use super::chat::{
    CALL_OPEN, ChatMessage, ChatTemplate, ToolCall, ToolSpec, afirma_que_fez, chamada_pendente,
    corpo_de_codigo, inicio_de_chamada, parse_tool_calls, promete_e_pede_licenca, strip_tool_calls,
};
use super::compactacao::{self, Resumo};
use super::derrame;
use super::engine::{Emission, Engine, SamplingParams, StopReason};
use super::memoria::Achado;
use super::router::Choice;
use crate::idiomas::{self, tr};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// Medido no Granite 4.2 3B: system prompt curto e em português é o que o modelo
/// segue. Um prompt longo ou em inglês o faz ignorar a pergunta.
pub const SYSTEM_PROMPT: &str =
    "Você é Aether, um assistente local que roda no computador do usuário. \
     Responda em português, de forma breve e direta.";

/// O mesmo prompt curto, no idioma escolhido. A estrutura medida não muda: só o
/// nome do idioma, para o modelo responder na língua que o usuário lê.
pub fn system_prompt() -> String {
    let atual = idiomas::atual();
    let nome = idiomas::disponiveis()
        .into_iter()
        .find(|i| i.id == atual)
        .map(|i| i.nome)
        .unwrap_or(atual);
    tr("conversa.promptSistema", &[("idioma", &nome)])
}

/// O template fecha o turno com este marcador; gerar além dele é começar a
/// falar pelo usuário.
pub(crate) const TURN_MARKER: &str = concat!("<", "|im_start|>");
/// O bloco de raciocínio do Granite 4.2, lido do `chat_template.jinja` do
/// fabricante. Com o nome errado na frente, o pensamento nunca aparece na tela e
/// a resposta sai vazia: é ele que marca onde a resposta começa.
const THINK_OPEN: &str = concat!("<", "think>");
const THINK_CLOSE: &str = concat!("</", "think>");

/// Rede de segurança da retomada de chamada cortada. O limite real é o espaço
/// que sobra na janela, medido rodada a rodada, não este número: ele só impede
/// que um modelo faça o laço girar sem fim.
const CONTINUACOES_MAX: usize = 16;
/// Tokens que a última geração de um turno precisa ter para valer a pena. Sem
/// esta reserva, a retoma seguinte poderia entrar na janela e ser cortada em
/// zero tokens: o corpo não avançaria e o turno giraria até o teto de rodadas.
const RESERVA_MINIMA: usize = 32;

/// Cabe mais uma retoma do corpo cortado? A resposta é conta, não opinião: a
/// retoma prefila o prompt inteiro com o que já foi escrito, de modo que o
/// espaço consumido é `prompt + gerados` e o próximo corte ainda precisa de
/// `teto` células. `retoma_ate_a_janela_encher` confere a conta com os números
/// do turno real que escreveu a página de 8.447 bytes.
fn retomada_cabe(
    prompt: usize,
    gerados: usize,
    janela: usize,
    teto: usize,
    continuacoes: usize,
) -> bool {
    continuacoes < CONTINUACOES_MAX
        && janela.saturating_sub(prompt + gerados + teto) >= RESERVA_MINIMA
}

/// O que a sessão entrega a quem escuta. Todo evento é fato ocorrido.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum SessionEvent {
    /// Transparência obrigatória: modelo escolhido e motivo auditável.
    Choice { choice: Choice },
    Loading { model: String },
    /// Raciocínio do modelo, visível por exigência do produto.
    Thinking { text: String },
    Delta { text: String },
    Replace { text: String },
    /// O modelo dobrou turnos antigos no resumo para caber na janela. Não é
    /// recado de rodapé: é o assistente dizendo que parou de ler aquilo ao pé da
    /// letra, e o que ele escreveu no lugar.
    Resumo { texto: String, mensagens: usize, tokens: usize },
    ToolCall { name: String, arguments: serde_json::Value },
    /// O que a injeção automática pôs no prompt antes do primeiro token. Não é
    /// uma ferramenta que o modelo pediu: é memória que ele recebeu sem ter que
    /// querer, e o dono da máquina tem o direito de ver o que entrou.
    Memoria {
        /// O nome qualificado da sinapse que autorizou esta injeção, para o
        /// grafo pulsar o fio certo pelo mesmo caminho que a ferramenta pulsa.
        ferramenta: String,
        consulta: String,
        achados: Vec<Achado>,
        /// A palavra de máquina do modo que ordenou esta lista. O cartão traduz;
        /// assim a conversa diz por que aquela ordem é aquela.
        modo: &'static str,
        /// Quantos trechos entraram na ordem que produziu esta lista, quando o
        /// `k` cortou o resto.
        total: usize,
        ms: u64,
    },
    /// `vista` é o cartão visual da ferramenta, quando ela produziu um. Um
    /// servidor MCP qualquer não produz: aí a interface mostra o texto.
    /// `ms` é o tempo wall-clock da execução, medido aqui: é o que o fio do
    /// grafo mostra quando a sinapse dispara.
    ToolResult {
        name: String,
        ok: bool,
        text: String,
        vista: Option<serde_json::Value>,
        ms: u64,
    },
    Done {
        text: String,
        rounds: usize,
        medicao: Medicao,
    },
}

/// O que o turno mediu, somado sobre as rodadas. Está na interface porque é
/// número medido: tokens que entraram, tokens que saíram, o tempo de cada um.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Medicao {
    pub prompt_tokens: usize,
    /// Quantos tokens do prompt o KV cache já tinha e não foram calculados de
    /// novo. É o número que diz, em tokens, o que o reaproveitamento poupou.
    pub reused_tokens: usize,
    pub generated_tokens: usize,
    pub prompt_ms: u64,
    pub decode_ms: u64,
    /// Tokens por segundo na decodificação — o ritmo que a pessoa percebe.
    pub tokens_por_segundo: f64,
    /// Tokens por segundo no prefill, que é o custo de ler o histórico outra vez.
    pub prefill_tokens_por_segundo: f64,
    pub stop: StopReason,
    /// Quantas mensagens entraram no prompt como resumo em vez de como texto.
    /// Zero é a conversa inteira ao pé da letra; um número aqui é o modelo
    /// dizendo que leu o começo da conversa condensado — e mostrando que leu.
    pub dobradas: usize,
}

impl Medicao {
    /// A velocidade é da soma, não a média das velocidades: dez tokens num
    /// segundo e noventa em nove segundos fazem cem tokens em dez, e é isso que
    /// o usuário viu.
    fn novo(
        prompt_tokens: usize,
        reused_tokens: usize,
        generated_tokens: usize,
        prompt_ms: u64,
        decode_ms: u64,
        stop: StopReason,
        dobradas: usize,
    ) -> Self {
        Self {
            prompt_tokens,
            reused_tokens,
            generated_tokens,
            prompt_ms,
            decode_ms,
            tokens_por_segundo: if decode_ms > 0 {
                generated_tokens as f64 * 1000.0 / decode_ms as f64
            } else {
                0.0
            },
            prefill_tokens_por_segundo: if prompt_ms > 0 {
                prompt_tokens.saturating_sub(reused_tokens) as f64 * 1000.0 / prompt_ms as f64
            } else {
                0.0
            },
            stop,
            dobradas,
        }
    }
}

/// O que uma ferramenta devolve ao turno: o texto que volta ao modelo e o
/// cartão que a interface desenha. Os dois saem do mesmo resultado real.
pub struct ToolOutcome {
    pub text: String,
    pub vista: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnOutcome {
    pub text: String,
    pub thinking: String,
    pub rounds: usize,
    pub medicao: Medicao,
    /// O resumo que o turno deixou para a conversa — o que ele escreveu agora,
    /// ou o que já estava guardado e continua cobrindo. Quem persiste a conversa
    /// grava isto junto: recompor o resumo a cada turno seria cobrar da placa o
    /// trabalho que ela já fez.
    pub resumo: Option<Resumo>,
}

pub struct TurnConfig<'a> {
    pub tools: &'a [ToolSpec],
    pub params: &'a SamplingParams,
    pub enable_thinking: bool,
    /// Teto de idas e voltas com ferramentas num pedido. Sem teto, um modelo
    /// que alucina a mesma chamada para sempre queima a placa e a paciência de
    /// quem espera. O valor vem de `geracao.json`.
    pub max_rounds: usize,
    /// O resumo dos turnos que já saíram do prompt, com o índice até onde ele
    /// cobre. `None` é a conversa lida inteira, que é o começo de tudo.
    pub resumo: Option<Resumo>,
    /// O bloco de memória que a busca do RAG devolveu para este pedido. Ele só
    /// existe na renderização: o histórico guardado continua sendo o que o
    /// usuário digitou, e cada pedido novo busca de novo contra as pastas de
    /// agora.
    pub memoria: Option<String>,
    /// As pastas concedidas agora, escritas no prompt de cada pedido. Sem isso o
    /// modelo não tem como saber onde ele pode mexer: ele inventa um caminho, a
    /// contenção recusa, e a pessoa vê o assistente esbarrar três vezes na mesma
    /// parede. É estado vivo, não memória — por isso entra na renderização e
    /// nunca no histórico guardado.
    pub pastas: Option<String>,
    /// A pasta do depósito de resultados longos. Um corpo grande não fica no
    /// histórico: cada rodada seguinte prefilaria aquilo de novo, e o laço de
    /// dez chamadas pagaria dez vezes um despejo que o modelo talvez nunca leia.
    /// `None` é o laço sem depósito: o turno manda o texto inteiro, como mandava
    /// antes de isto existir.
    pub derrame: Option<PathBuf>,
}

/// Onde começa a resposta dentro do texto bruto gerado. Com pensamento ligado o
/// modelo escreve primeiro o bloco de raciocínio; antes de fechá-lo não há
/// resposta nenhuma para mostrar.
fn answer_start(raw: &str, thinking: bool) -> usize {
    if !thinking {
        return 0;
    }
    match raw.find(THINK_CLOSE) {
        Some(i) => i + THINK_CLOSE.len(),
        None => raw.len(),
    }
}

/// Comprimento da maior cauda de `text` que é um prefixo próprio do marcador de
/// chamada. Segurar essa cauda evita mostrar um marcador partido ao usuário.
fn partial_marker(text: &str) -> usize {
    let bytes = text.as_bytes();
    let marker = CALL_OPEN.as_bytes();
    let max = (marker.len() - 1).min(bytes.len());
    for len in (1..=max).rev() {
        if bytes[bytes.len() - len..] == marker[..len] {
            return len;
        }
    }
    0
}

/// Fim da prosa visível: para na primeira chamada de ferramenta e recua a cauda
/// que pode ser um marcador partido.
fn visible_end(raw: &str, from: usize) -> usize {
    let region = &raw[from..];
    let end = match region.find(CALL_OPEN) {
        Some(i) => from + i,
        None => raw.len() - partial_marker(region),
    };
    end.max(from)
}

/// O histórico é compartilhado com a UI, que o lê durante a geração. O lock é
/// tomado só pelo tempo de um push, nunca segurado enquanto o modelo gera.
fn push(history: &Mutex<Vec<ChatMessage>>, message: ChatMessage) {
    history.lock().expect("histórico envenenado").push(message);
}

/// O que um resultado de ferramenta deixa no histórico. O cartão da pessoa já
/// saiu com o corpo inteiro, então a pergunta aqui é outra: quanto deste corpo a
/// próxima rodada vai pagar para reler. Um despejo de duzentos mil bytes dentro
/// da conversa é o mesmo despejo prefilado pela placa a cada chamada que o
/// modelo ainda vai fazer, e o meio de um log raramente é o que ele procura.
///
/// O corpo não some: vai para o depósito e volta por `ler_resultado`, pedaço a
/// pedaço, quando o modelo pedir. Um erro é a resposta da ferramenta, não um
/// despejo — o motivo chega por inteiro.
fn texto_para_modelo(derrame: Option<&PathBuf>, ok: bool, texto: String) -> String {
    match derrame {
        Some(pasta) if ok => derrame::derramar(pasta, &texto).unwrap_or(texto),
        _ => texto,
    }
}

/// O histórico com os blocos de estado encaixados depois do prompt de sistema,
/// antes de qualquer turno: a memória que o RAG recuperou para o pedido e as
/// pastas concedidas agora. A ordem não é estética: o prompt abre a conversa com
/// o que o modelo sempre foi, os blocos dizem o que ele sabe e onde pode mexer,
/// e só então vêm as mensagens. Sem bloco, a lista é a mesma que veio — clonar
/// um histórico inteiro por turno custaria tokens de cópia para não mudar nada.
fn com_estado(
    history: &[ChatMessage],
    memoria: Option<&str>,
    pastas: Option<&str>,
) -> Vec<ChatMessage> {
    let mut blocos: Vec<&str> = Vec::with_capacity(2);
    for bloco in [memoria, pastas].into_iter().flatten() {
        if !bloco.trim().is_empty() {
            blocos.push(bloco);
        }
    }
    let Some((primeiro, resto)) = blocos.split_first() else {
        return history.to_vec();
    };
    let depois_do_prompt = usize::from(history.first().map(|m| m.role == "system").unwrap_or(false));
    let mut prompt = Vec::with_capacity(history.len() + blocos.len());
    prompt.extend_from_slice(&history[..depois_do_prompt]);
    prompt.push(ChatMessage::new("system", (*primeiro).to_string()));
    for bloco in resto {
        prompt.push(ChatMessage::new("system", bloco.to_string()));
    }
    prompt.extend_from_slice(&history[depois_do_prompt..]);
    prompt
}

/// A cabeça do recorte: a posição mais cedo da conversa que ainda deixa o
/// prompt caber. `medir` devolve quantos tokens tem o prompt cortando antes de
/// uma posição, e só encolhe conforme a posição avança — é por isso que a busca
/// é binária. `None` é honesto: nem sobrando só o último pedido cabe.
fn cabeca_que_cabe(
    cabecas: &[usize],
    limite: usize,
    medir: &dyn Fn(usize) -> Result<usize>,
) -> Result<Option<usize>> {
    let Some(&ultimo) = cabecas.last() else {
        return Ok(None);
    };
    if medir(ultimo)? > limite {
        return Ok(None);
    }
    let mut baixo = 0usize;
    let mut alto = cabecas.len() - 1;
    while baixo < alto {
        let meio = (baixo + alto) / 2;
        if medir(cabecas[meio])? <= limite {
            alto = meio;
        } else {
            baixo = meio + 1;
        }
    }
    Ok(Some(cabecas[baixo]))
}

/// Onde a conversa pode ser dobrada sem mentir para o modelo: o começo de um
/// pedido da pessoa ou o começo de uma rodada de ferramenta. Um resultado de
/// ferramenta nunca é fronteira — deixá-lo sozinho, sem a chamada que o produziu,
/// é o modelo lendo uma resposta órfã.
fn fronteiras(history: &[ChatMessage], desde: usize) -> Vec<usize> {
    (desde + 1..history.len())
        .filter(|&i| {
            let m = &history[i];
            m.role == "user" || (m.role == "assistant" && m.tool_calls.is_some())
        })
        .collect()
}

/// O que a janela decidiu sobre uma conversa. Três saídas, cada uma com um dono:
/// o turno escreve o resumo, o turno devolve o erro.
///
/// `pub(crate)` não é folga de visibilidade: é a bancada conferir a decisão
/// mesma que o turno obedece, caso a caso, com o motor real em placa.
pub(crate) enum Encaixe {
    /// Cabe. Os tokens do prompt e quantas mensagens entraram como resumo.
    Cabe { ids: Vec<u32>, dobradas: usize },
    /// Não cabe. Dobre as mensagens de `desde` até `ate` no resumo e meça de
    /// novo; `ate` é fronteira de turno, nunca meio de resposta.
    Dobre { desde: usize, ate: usize },
    /// Nem sobrando só o pedido atual cabe. Os números são os que a interface
    /// precisa dizer: a janela, o prompt fixo (identidade mais ferramentas) e o
    /// que a pessoa escreveu.
    SemLugar { janela: usize, fixo: usize, pedido: usize },
}

/// O prompt cabe na janela por construção: quando não cabe, as mensagens mais
/// antigas não são derrubadas — elas viram resumo escrito pelo próprio modelo,
/// que entra no prompt no lugar delas. O histórico guardado fica inteiro, a
/// conversa na tela não perde linha nenhuma, e o que o modelo deixa de ler ao pé
/// da letra ele passa a ler condensado. O prompt de sistema nunca sai, porque é
/// ele que diz quem o modelo é.
///
/// A reserva é o que impede a compactação de correr atrás do próprio rabo: sem
/// espaço separado para o resumo, o corte escolhido caberia até o resumo nascer
/// — e aí nasceria sem onde ficar.
///
/// `cabecilha` é texto que entra depois do prompt de geração, na boca do
/// assistente. Ela conta na medida porque ocupa a janela igual a qualquer token
/// lido.
pub(crate) fn encaixe(
    engine: &Engine,
    template: &ChatTemplate,
    history: &[ChatMessage],
    config: &TurnConfig,
    cabecilha: Option<&str>,
    resumo: Option<&Resumo>,
) -> Result<Encaixe> {
    let janela = engine.context();
    let limite = compactacao::limite(janela, config.params.max_tokens);
    let base = usize::from(history.first().map(|m| m.role == "system").unwrap_or(false));
    // Índice da primeira mensagem que ainda não está no resumo.
    let desde = resumo.map(|r| r.ate.max(base)).unwrap_or(base);
    // `com_resumo` é o que distingue "o que está de pé agora" de "o que sobra
    // depois de dobrar": na segunda conta o texto velho sai e o resumo novo —
    // ainda não escrito — entra mais adiante, com o teto dele reservado.
    let medir = |cabeca: usize, com_resumo: bool| -> Result<Vec<u32>> {
        let mut recorte = Vec::with_capacity(base + 1 + history.len() - cabeca);
        recorte.extend_from_slice(&history[..base]);
        if com_resumo {
            if let Some(r) = resumo {
                recorte.push(r.mensagem());
            }
        }
        recorte.extend_from_slice(&history[cabeca..]);
        let conversa =
            com_estado(&recorte, config.memoria.as_deref(), config.pastas.as_deref());
        let prompt = template
            .render(&conversa, config.tools, config.enable_thinking)
            .context(tr("erros.renderizarConversa", &[]))?;
        let mut ids = compactacao::codificar(engine, prompt.as_str())?;
        if let Some(texto) = cabecilha {
            ids.extend(compactacao::codificar(engine, texto)?);
        }
        Ok(ids)
    };

    // Cortar em `desde` é não cortar nada agora: o que está de pé, com o resumo
    // que já existe.
    let ids = medir(desde, true)?;
    if ids.len() <= limite {
        return Ok(Encaixe::Cabe { ids, dobradas: desde - base });
    }

    let dobras = fronteiras(history, desde);
    let teto_dobrado = limite.saturating_sub(compactacao::RESERVA).max(1);
    let Some(ate) = cabeca_que_cabe(&dobras, teto_dobrado, &|i| Ok(medir(i, false)?.len()))?
    else {
        // Nada a dobrar, ou o que sobra ainda não cabe — com a janela do modelo
        // inteiro dentro, o que não couber é maior que a conversa.
        let minima = dobras.last().copied().unwrap_or(desde);
        let pedido = medir(minima, true)?.len();
        let fixo = medir(history.len(), false)?.len();
        return Ok(Encaixe::SemLugar { janela, fixo, pedido });
    };
    Ok(Encaixe::Dobre { desde, ate })
}

/// A cabeceira que fecha a prosa, quando a resposta é um arquivo cercado com
/// "eu criei". As duas metades juntas são o defeito: sem o corpo não há arquivo
/// sendo mostrado, sem a afirmação o corpo é só um exemplo que o usuário pediu.
///
/// Devolve `None` quando não há ferramenta de escrita conectada, quando o texto
/// não diz qual delas (mais de uma candidata e nenhuma citada), ou quando a
/// ferramenta não tem no esquema nem um caminho nem um corpo. O app não inventa
/// para onde escrever.
fn cabeca_de_chamada(resposta: &str, tools: &[ToolSpec]) -> Option<String> {
    // Ou ele diz que fez, ou diz que vai fazer e para pedindo licença. Os dois
    // deixam o disco como a resposta descreve, e os dois têm o corpo escrito por
    // ele mesmo abaixo, que é o que impede o app de inventar um arquivo.
    if !afirma_que_fez(resposta) && !promete_e_pede_licenca(resposta) {
        return None;
    }
    corpo_de_codigo(resposta)?;
    let candidatas: Vec<&ToolSpec> = tools.iter().filter(|t| t.alvo.escrita()).collect();
    let escolhida = match candidatas.len() {
        0 => return None,
        1 => candidatas[0],
        _ => {
            let citada: Vec<&ToolSpec> = candidatas
                .iter()
                .copied()
                .filter(|t| resposta.contains(&t.name))
                .collect();
            if citada.len() == 1 {
                citada[0]
            } else {
                return None;
            }
        }
    };
    Some(inicio_de_chamada(&escolhida.name))
}

/// O teto de tokens cortou o turno dentro de um pensamento que nunca fechou? É
/// o caso em que não existe resposta a mostrar: o `answer` fica vazio, e um
/// `Done` com ele seria o app jurando sucesso de uma resposta que a placa nunca
/// terminou de escrever. Uma chamada de ferramenta cortada tem continuação (o
/// `chamada_pendente` retoma o corpo); um bloco de pensamento aberto não tem —
/// a retoma dele é suprimida em `em_chamada` e não devolveria o texto à tela —,
/// então a saída honesta é dizer o motivo, não calar em vez de responder.
fn corte_em_pensamento_aberto(raw: &str, thinking_ligado: bool, stop: StopReason) -> bool {
    thinking_ligado
        && stop == StopReason::MaxTokens
        && raw.contains(THINK_OPEN)
        && !raw.contains(THINK_CLOSE)
}

/// Um turno completo: gera, executa as ferramentas que o modelo pediu e volta a
/// gerar com os resultados, até responder em prosa ou atingir o teto de rodadas.
pub fn run_turn(
    engine: &mut Engine,
    template: &ChatTemplate,
    history: &Mutex<Vec<ChatMessage>>,
    config: &TurnConfig<'_>,
    cancel: &AtomicBool,
    mut emit: impl FnMut(SessionEvent),
    mut run_tool: impl FnMut(&ToolCall) -> Result<ToolOutcome>,
) -> Result<TurnOutcome> {
    // O que fecha um turno é o vocabulário do modelo que está em memória, e não
    // um marcador suposto: um arquivo cujo EOS é outro token pararia sempre no
    // limite de tokens. O marcador de turno continua na lista porque um modelo
    // que desanda a repetir ChatML precisa de onde cair.
    let mut stop: Vec<String> = engine.paradas().to_vec();
    if !stop.iter().any(|s| s == TURN_MARKER) {
        stop.push(TURN_MARKER.to_string());
    }
    // Os nomes das ferramentas conectadas é que dizem onde termina uma chamada:
    // um objeto JSON com um `name` que não está aqui é prosa, não execução.
    let nomes: Vec<&str> = config.tools.iter().map(|t| t.name.as_str()).collect();
    let mut rounds = 0usize;
    // Continuações de uma mesma chamada cortada no teto. Não é rodada de
    // ferramenta — nada foi executado —, mas também não é para ir até o modelo
    // cansar: o teto é o espaço que sobra na janela, e o `CONTINUACOES_MAX` é a
    // rede de segurança. Sem teto nenhum, a ferramenta diria o que faltou.
    let mut continuacoes = 0usize;
    let mut prompt_tokens = 0usize;
    let mut reused_tokens = 0usize;
    let mut generated_tokens = 0usize;
    let mut prompt_ms = 0u64;
    let mut decode_ms = 0u64;
    let mut thinking = String::new();
    // A cabeceira que fecha a saída em prosa, quando o modelo mostrou um arquivo
    // em vez de escrevê-lo. Uma vez só: insistir duas vezes seria o app falando
    // pelo modelo.
    let mut cabecilha: Option<String> = None;
    // `cabecilha` volta a `None` a cada rodada consumida, então ela sozinha não
    // guarda a promessa de "uma vez": sem este bool o turno pode reinvestir a
    // cabeceira indefinidamente enquanto o modelo responder em prosa.
    let mut cabeceira_usada = false;
    // O que a conversa já tem de resumo, e o que este turno pode acrescentar a
    // ele. É aqui dentro e não no `config` porque um turno com ferramentas pode
    // dobrar mais de uma vez antes de responder.
    let mut resumo = config.resumo.clone();

    loop {
        if cancel.load(Ordering::Relaxed) {
            bail!(tr("erros.pedidoCancelado", &[]));
        }
        // A conversa viva, fora do lock: dobrar é um turno de geração, e segurar
        // o histórico enquanto o modelo escreve é prender a tela que o lê.
        let conversa = history.lock().expect("histórico envenenado").to_vec();
        let (ids, dobradas) = loop {
            let decidido =
                encaixe(engine, template, &conversa, config, cabecilha.as_deref(), resumo.as_ref())?;
            match decidido {
                Encaixe::Cabe { ids, dobradas } => break (ids, dobradas),
                Encaixe::Dobre { desde, ate } => {
                    if cancel.load(Ordering::Relaxed) {
                        bail!(tr("erros.pedidoCancelado", &[]));
                    }
                    let (texto, gasto) = compactacao::dobrar(
                        engine,
                        template,
                        config.params,
                        resumo.as_ref().map(|r| r.texto.as_str()),
                        &conversa[desde..ate],
                        cancel,
                    )?;
                    // O resumo é um turno de verdade: leu tokens e escreveu
                    // tokens, na placa. Contá-lo no turno é o que faz a medida
                    // dizer quanto o pedido custou, e não quanto custou ignorando
                    // o que foi preciso para caber.
                    prompt_tokens += gasto.prompt_tokens;
                    reused_tokens += gasto.reused_tokens;
                    generated_tokens += gasto.generated_tokens;
                    prompt_ms += gasto.prompt_ms;
                    decode_ms += gasto.decode_ms;
                    emit(SessionEvent::Resumo {
                        texto: texto.clone(),
                        mensagens: ate - desde,
                        tokens: gasto.generated_tokens,
                    });
                    resumo = Some(Resumo { texto, ate });
                }
                Encaixe::SemLugar { janela, fixo, pedido } => {
                    // A última saída, e só depois de dobrar o que dava para
                    // dobrar: o que não cabe aqui não cabe nem sem conversa.
                    bail!(tr(
                        "erros.janelaSemLugar",
                        &[
                            ("janela", &janela.to_string()),
                            ("fixo", &fixo.to_string()),
                            ("pedido", &pedido.to_string()),
                        ]
                    ));
                }
            }
        };

        let mut raw = String::new();
        let mut shown = 0usize;
        let mut thought = 0usize;
        // A cabeceira já é o começo da chamada: o que vem dela em diante não é
        // prosa para mostrar, são o caminho e o corpo do arquivo.
        let em_chamada = cabecilha.is_some();
        let generation = engine.generate(&ids, config.params, &stop, cancel, |emission| {
            let replaced = match emission {
                Emission::Delta(piece) => {
                    raw.push_str(&piece);
                    false
                }
                Emission::Replace(text) => {
                    raw = text;
                    true
                }
            };
            if em_chamada {
                return;
            }
            let start = answer_start(&raw, config.enable_thinking);
            if config.enable_thinking {
                let close = raw.find(THINK_CLOSE).unwrap_or(raw.len());
                let open = raw.find(THINK_OPEN).map(|i| i + THINK_OPEN.len()).unwrap_or(0);
                if close > open && close > thought {
                    emit(SessionEvent::Thinking { text: raw[open..close].to_string() });
                    thought = close;
                }
            }
            let end = visible_end(&raw, start);
            if replaced {
                emit(SessionEvent::Replace { text: raw[start..end].to_string() });
            } else if end > shown {
                emit(SessionEvent::Delta { text: raw[shown..end].to_string() });
            }
            shown = end;
        })?;
        let cabecilha_usada = cabecilha.take();
        // O texto do assistente é a chamada inteira, não o pedaço que saiu
        // depois da cabeceira: é assim que o histórico renderiza o mesmo prompt.
        let raw = match &cabecilha_usada {
            Some(p) => format!("{p}{}", generation.text),
            None => generation.text,
        };

        prompt_tokens += generation.prompt_tokens;
        reused_tokens += generation.reused_tokens;
        generated_tokens += generation.generated_tokens;
        prompt_ms += generation.prompt_ms;
        decode_ms += generation.decode_ms;
        if generation.stop == StopReason::Cancelled {
            bail!(tr("erros.pedidoCancelado", &[]));
        }

        // O teto de tokens cortou o corpo de um arquivo no meio. Rodar a chamada
        // pela metade é o que cria um arquivo vazio com a cara de sucesso: o
        // `caminho` já fechou, o corpo não. O que fecha o serviço é continuar de
        // onde o modelo parou — o texto dele vira prefilo e a rodada seguinte só
        // escreve o resto, sem uma linha inventada pelo app. Com teto próprio,
        // porque insistir sem fim seria queimar a placa numa resposta que não
        // termina: aí a chamada roda com o que tem e a ferramenta diz o que
        // faltou, em vez de calar.
        // O teto de três retomas foi medido e perdido: o corpo de uma página de
        // 3,6 KB em HTML passa de 1.200 tokens, e 3 × 400 do usuário não chegam
        // lá. A chamada fechava cortada, a ferramenta devolvia "falta o
        // argumento", e o modelo recomeçava o arquivo inteiro a cada rodada até
        // estourar o limite de rodadas. O que limita a retoma é o espaço que
        // sobra na janela — cada retoma prefila o prompt com o corpo já escrito.
        // Cabe, continua; não cabe, roda com o que tem e a ferramenta diz o que
        // faltou, em vez de calar.
        if generation.stop == StopReason::MaxTokens
            && retomada_cabe(
                generation.prompt_tokens,
                generation.generated_tokens,
                engine.context(),
                config.params.max_tokens,
                continuacoes,
            )
        {
            if let Some(aberta) = chamada_pendente(&raw, &nomes) {
                continuacoes += 1;
                emit(SessionEvent::Replace { text: String::new() });
                cabecilha = Some(aberta);
                continue;
            }
        }

        let answer = if config.enable_thinking {
            match raw.find(THINK_CLOSE) {
                Some(i) => {
                    let open = raw.find(THINK_OPEN).map(|j| j + THINK_OPEN.len()).unwrap_or(0);
                    if i > open {
                        thinking = raw[open..i].trim().to_string();
                    }
                    raw[i + THINK_CLOSE.len()..].to_string()
                }
                None => String::new(),
            }
        } else {
            raw.clone()
        };

        // O pensamento foi cortado antes de fechar: `answer` está vazio, mas o
        // modelo não terminou — é falha, não uma resposta em branco de sucesso.
        if corte_em_pensamento_aberto(&raw, config.enable_thinking, generation.stop) {
            bail!(tr("erros.pensamentoTruncado", &[]));
        }

        let calls = parse_tool_calls(&answer, &nomes);
        if calls.is_empty() {
            // O modelo pôs o arquivo na tela e disse que criou. Enquanto a
            // resposta for prosa, nada aconteceu: quem lê vê "criei" e o disco
            // continua vazio. Uma vez, e só uma, a saída em prosa é fechada e
            // ele recomeça de dentro da chamada — o caminho e o corpo continuam
            // sendo os dele, e a aprovação continua sendo de quem assiste.
            if !cabeceira_usada && rounds == 0 {
                if let Some(cabeca) = cabeca_de_chamada(&answer, config.tools) {
                    cabeceira_usada = true;
                    emit(SessionEvent::Replace { text: String::new() });
                    cabecilha = Some(cabeca);
                    continue;
                }
            }
            let text = strip_tool_calls(&answer, &nomes).trim().to_string();
            push(history, ChatMessage::new("assistant", raw.clone()));
            let medicao = Medicao::novo(
                prompt_tokens,
                reused_tokens,
                generated_tokens,
                prompt_ms,
                decode_ms,
                generation.stop,
                dobradas,
            );
            emit(SessionEvent::Done { text: text.clone(), rounds, medicao });
            return Ok(TurnOutcome { text, thinking, rounds, medicao, resumo });
        }

        if rounds >= config.max_rounds {
            bail!(tr(
                "erros.lacoDeFerramentas",
                &[("n", &(rounds + 1).to_string())]
            ));
        }
        rounds += 1;

        let mut assistant = ChatMessage::new("assistant", raw.clone());
        assistant.tool_calls = Some(calls.clone());
        push(history, assistant);

        for call in &calls {
            if cancel.load(Ordering::Relaxed) {
                bail!(tr("erros.pedidoCancelado", &[]));
            }
            emit(SessionEvent::ToolCall {
                name: call.name.clone(),
                arguments: call.arguments.clone(),
            });
            let inicio = std::time::Instant::now();
            let (ok, text, vista) = match run_tool(call) {
                Ok(saida) => (true, saida.text, saida.vista),
                // O erro real vai ao modelo: ele precisa saber que a ferramenta
                // não rodou para não afirmar que rodou.
                Err(error) => (false, format!("{error:#}"), None),
            };
            emit(SessionEvent::ToolResult {
                name: call.name.clone(),
                ok,
                text: text.clone(),
                vista,
                ms: inicio.elapsed().as_millis() as u64,
            });
            // A pessoa vê o resultado inteiro; o histórico recebe uma janela
            // declarada como janela, com o corpo guardado atrás do id.
            let para_modelo = texto_para_modelo(config.derrame.as_ref(), ok, text);
            push(history, ChatMessage::new("tool", para_modelo));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call_block(name: &str) -> String {
        format!(
            "{c}\n{f}{name}{gt}\n{p}message{gt}\noi\n{pc}\n{fc}\n{cc}",
            c = CALL_OPEN,
            f = concat!("<", "function="),
            gt = ">",
            p = concat!("<", "parameter="),
            pc = concat!("</", "parameter>"),
            fc = concat!("</", "function>"),
            cc = concat!("</", "tool_call>"),
        )
    }

    #[test]
    fn partial_marker_holds_back_only_a_real_prefix() {
        assert_eq!(partial_marker("olá <tool_ca"), 8);
        assert_eq!(partial_marker("olá <"), 1);
        assert_eq!(partial_marker("olá mundo"), 0);
        // O marcador completo já é visível como fronteira, não como cauda.
        assert_eq!(partial_marker(CALL_OPEN), 0);
    }

    #[test]
    fn visible_prose_stops_at_the_call() {
        let raw = format!("Vou verificar.{}", call_block("srv__echo"));
        assert_eq!(visible_end(&raw, 0), "Vou verificar.".len());
    }

    #[test]
    fn visible_prose_hides_a_split_marker() {
        let raw = "Vou verificar.<tool_ca";
        assert_eq!(visible_end(raw, 0), "Vou verificar.".len());
    }

    #[test]
    fn multibyte_prose_is_not_cut_mid_character() {
        let raw = "A capital da França é Paris.<tool_ca";
        let end = visible_end(raw, 0);
        assert!(raw.is_char_boundary(end));
        assert_eq!(&raw[..end], "A capital da França é Paris.");
    }

    #[test]
    fn thinking_block_is_not_shown_as_answer() {
        let raw = format!("{THINK_OPEN}\npreciso checar\n{THINK_CLOSE}Paris.");
        let start = answer_start(&raw, true);
        assert_eq!(&raw[start..], "Paris.");
        // Sem pensamento ligado o texto inteiro é resposta.
        assert_eq!(answer_start(&raw, false), 0);
    }

    #[test]
    fn unfinished_thinking_shows_no_answer_yet() {
        let raw = format!("{THINK_OPEN}\npreciso checar");
        assert_eq!(answer_start(&raw, true), raw.len());
    }

    #[test]
    fn corte_em_pensamento_aberto_e_falha_e_nao_sucesso_vazio() {
        let aberto = format!("{THINK_OPEN}\npreciso checar");
        // Pensamento ligado, teto atingido, bloco aberto: é aqui que o turno
        // calava como se tivesse respondido. A recusa é o motivo honesto.
        assert!(corte_em_pensamento_aberto(&aberto, true, StopReason::MaxTokens));
        // O bloco fechou: a resposta existe, o turno termina normal.
        let fechado = format!("{THINK_OPEN}\npensei{THINK_CLOSE}Paris.");
        assert!(!corte_em_pensamento_aberto(&fechado, true, StopReason::MaxTokens));
        // Sem teto (o modelo terminou sozinho) não é corte: é resposta.
        assert!(!corte_em_pensamento_aberto(&aberto, true, StopReason::Eos));
        // Pensamento desligado: prosa truncada é resposta parcial legítima.
        assert!(!corte_em_pensamento_aberto(&aberto, false, StopReason::MaxTokens));
    }

    /// O que a injeção produz, visto de fora: a sequência de papéis e textos,
    /// sem depender de `PartialEq` em `ChatMessage`.
    fn papel_e_texto(messages: &[ChatMessage]) -> Vec<(&str, &str)> {
        messages.iter().map(|m| (m.role.as_str(), m.content.as_str())).collect()
    }

    #[test]
    fn memoria_entra_depois_do_prompt_e_antes_dos_turnos() {
        let history = vec![
            ChatMessage::new("system", "prompt"),
            ChatMessage::new("user", "pergunta"),
            ChatMessage::new("assistant", "resposta"),
        ];
        let com = com_estado(&history, Some("trechos recuperados"), None);
        assert_eq!(
            papel_e_texto(&com),
            vec![
                ("system", "prompt"),
                ("system", "trechos recuperados"),
                ("user", "pergunta"),
                ("assistant", "resposta"),
            ]
        );
    }

    /// O caso que a medição do app real pegou: sem as pastas no prompt, o modelo
    /// inventa o caminho e a contenção recusa. O bloco tem de chegar junto da
    /// memória, e os dois antes do primeiro turno.
    #[test]
    fn pastas_concedidas_entram_junto_da_memoria() {
        let history = vec![
            ChatMessage::new("system", "prompt"),
            ChatMessage::new("user", "crie um arquivo"),
        ];
        let com = com_estado(&history, Some("trechos"), Some("E:\\teste"));
        assert_eq!(
            papel_e_texto(&com),
            vec![
                ("system", "prompt"),
                ("system", "trechos"),
                ("system", "E:\\teste"),
                ("user", "crie um arquivo"),
            ]
        );
    }

    #[test]
    fn sem_bloco_nada_muda_e_bloco_vazio_nao_entra() {
        let history = vec![ChatMessage::new("system", "prompt"), ChatMessage::new("user", "oi")];
        let esperado = papel_e_texto(&history);
        assert_eq!(papel_e_texto(&com_estado(&history, None, None)), esperado);
        assert_eq!(papel_e_texto(&com_estado(&history, Some("   "), Some("  "))), esperado);
    }

    #[test]
    fn sem_prompt_na_historia_o_bloco_abre_a_conversa() {
        let history = vec![ChatMessage::new("user", "oi")];
        assert_eq!(
            papel_e_texto(&com_estado(&history, Some("trechos"), None)),
            vec![("system", "trechos"), ("user", "oi")]
        );
    }

    /// O depósito montado numa pasta de teste, com o corpo que o laço acabou de
    /// receber devolvido de volta por ele.
    fn deposito(nome: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("aether-session-{nome}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn um_resultado_curto_entra_inteiro_no_historico() {
        let p = deposito("curto");
        let texto = "as três primeiras linhas do arquivo".repeat(20);
        assert_eq!(
            texto_para_modelo(Some(&p), true, texto.clone()),
            texto,
            "abaixo do limiar não se guarda nada"
        );
        assert!(!p.exists(), "o depósito ficou de fora");
    }

    #[test]
    fn um_resultado_largo_deixa_a_janela_no_historico_e_o_corpo_no_deposito() {
        let p = deposito("largo");
        let corpo = "linha do meio do despejo\n".repeat(4_000);
        let janela = texto_para_modelo(Some(&p), true, corpo.clone());
        assert!(janela.chars().count() < corpo.chars().count() / 4);
        assert!(janela.contains("ler_resultado"), "{janela}");
        // O corpo está guardado inteiro, e é o corpo que a ferramenta devolveu.
        let guardados: Vec<_> = std::fs::read_dir(&p)
            .expect("depósito")
            .filter_map(|i| i.ok())
            .map(|i| i.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(guardados.len(), 1, "{guardados:?}");
        assert_eq!(
            std::fs::read_to_string(p.join(&guardados[0])).expect("lido"),
            corpo
        );
        let _ = std::fs::remove_dir_all(&p);
    }

    #[test]
    fn um_erro_de_ferramenta_nunca_chega_cortado_ao_modelo() {
        // O motivo da recusa é a resposta: cortá-lo na metade deixaria o modelo
        // afirmando que a ferramenta rodou.
        let p = deposito("erro");
        let erro = "a ferramenta falhou: ".to_string() + &"detalhe do erro ".repeat(1_000);
        assert_eq!(texto_para_modelo(Some(&p), false, erro.clone()), erro);
        assert!(!p.exists());
    }

    #[test]
    fn sem_deposito_o_laco_continua_despejando() {
        let corpo = "x".repeat(derrame::LIMIAR * 4);
        assert_eq!(texto_para_modelo(None, true, corpo.clone()), corpo);
    }

    #[test]
    fn recorte_para_na_cabeca_mais_antiga_que_cabe() {
        let cabecas = [1usize, 3, 5, 8];
        let tamanhos = [900usize, 700, 400, 200];
        let medidas = std::cell::Cell::new(0usize);
        let achada = cabeca_que_cabe(&cabecas, 500, &|i| {
            medidas.set(medidas.get() + 1);
            Ok(tamanhos[cabecas.iter().position(|&c| c == i).unwrap()])
        })
        .unwrap();
        assert_eq!(achada, Some(5));
        // Busca binária, não varredura: quatro cabeças custam no máximo três
        // medidas, e medir é tokenizar o histórico inteiro.
        assert!(medidas.get() <= 3, "medidas = {}", medidas.get());
    }

    #[test]
    fn a_primeira_cabeca_ja_cabe_e_a_conversa_inteira_fica() {
        let cabecas = [2usize, 4, 6];
        let achada = cabeca_que_cabe(&cabecas, 500, &|_| Ok(300)).unwrap();
        assert_eq!(achada, Some(2));
    }

    #[test]
    fn sem_turno_anterior_nao_ha_o_cortar() {
        assert_eq!(cabeca_que_cabe(&[], 500, &|_| Ok(100)).unwrap(), None);
    }

    #[test]
    fn quando_o_minimo_tambem_estoura_nao_ha_cabeca() {
        let cabecas = [2usize, 4];
        let achada =
            cabeca_que_cabe(&cabecas, 500, &|i| Ok(if i == 2 { 800 } else { 640 })).unwrap();
        assert_eq!(achada, None);
    }

    /// A fronteira é onde o modelo pode parar de ler sem ler uma resposta órfã:
    /// o pedido da pessoa e a chamada de ferramenta. Um resultado de ferramenta
    /// sozinho, sem a chamada que o gerou, não é começo de nada.
    #[test]
    fn dobrar_para_na_frenteira_de_turno_nunca_no_meio_de_uma_rodada() {
        let chamada = ChatMessage {
            role: "assistant".to_string(),
            content: String::new(),
            tool_calls: Some(vec![ToolCall {
                name: "local__ler_arquivo".to_string(),
                arguments: serde_json::json!({}),
            }]),
        };
        let history = vec![
            ChatMessage::new("system", "prompt"),
            ChatMessage::new("user", "leia"),
            chamada.clone(),
            ChatMessage::new("tool", "146 linhas"),
            ChatMessage::new("assistant", "são 146 linhas"),
            ChatMessage::new("user", "e agora"),
        ];
        assert_eq!(fronteiras(&history, 1), vec![2, 5]);
        // Dobrar a partir da última fronteira não tem o que dobrar depois dela.
        assert_eq!(fronteiras(&history, 5), Vec::<usize>::new());
        // Uma resposta em prosa não é fronteira: ela é o fim de um turno, e
        // cortá-la do pedido que a provocou deixaria o texto falando sozinho.
        assert!(!fronteiras(&history, 1).contains(&4));
    }

    fn escrita(nome: &str) -> ToolSpec {
        ToolSpec {
            name: nome.to_string(),
            description: String::new(),
            parameters: serde_json::json!({
                "properties": { "caminho": { "type": "string" }, "conteudo": { "type": "string" } }
            }),
            alvo: crate::ai::Alvo::do_esquema(
                &serde_json::json!({
                    "properties": { "caminho": { "type": "string" }, "conteudo": { "type": "string" } }
                }),
            ),
        }
    }

    fn so_leitura(nome: &str) -> ToolSpec {
        ToolSpec {
            name: nome.to_string(),
            description: String::new(),
            parameters: serde_json::json!({ "properties": { "caminho": { "type": "string" } } }),
            alvo: crate::ai::Alvo::do_esquema(
                &serde_json::json!({ "properties": { "caminho": { "type": "string" } } }),
            ),
        }
    }

    /// A resposta medida no Granite 4.2 3B: afirma que criou e mostra o corpo. É
    /// o turno que termina sem arquivo nenhum no disco.
    fn resposta_que_nao_escreveu() -> String {
        "Criei um arquivo HTML de landing page.\n\n```html\n<!DOCTYPE html>\n<body>oi</body>\n```\n"
            .to_string()
    }

    #[test]
    fn corpo_cercado_com_afirmacao_abre_a_chamada_da_escrita() {
        let ferramentas = [escrita("computador__escrever_arquivo")];
        let cabeca = cabeca_de_chamada(&resposta_que_nao_escreveu(), &ferramentas).unwrap();
        assert!(cabeca.starts_with(CALL_OPEN));
        assert!(cabeca.contains("computador__escrever_arquivo"));
        // A cabeceira é lida pelo parser como o que ela é: o começo de uma
        // chamada, ainda sem argumento nenhum fechado.
        let nomes = ["computador__escrever_arquivo"];
        assert!(parse_tool_calls(&cabeca, &nomes).is_empty());
    }

    #[test]
    fn sem_ferramenta_de_escrita_o_app_nao_inventa_para_onde_escrever() {
        let resposta = resposta_que_nao_escreveu();
        assert_eq!(cabeca_de_chamada(&resposta, &[]), None);
        assert_eq!(cabeca_de_chamada(&resposta, &[so_leitura("computador__ler_arquivo")]), None);
        // Duas escritas e o texto não diz qual: escolher uma seria chute.
        let duas = [escrita("a__escrever"), escrita("b__escrever")];
        assert_eq!(cabeca_de_chamada(&resposta, &duas), None);
        // Citada pelo nome, porém, não há mais ambiguidade.
        let citada = format!("Criei com a ferramenta b__escrever.\n\n```html\n<p>x</p>\n```");
        assert!(cabeca_de_chamada(&citada, &duas).is_some());
    }

    #[test]
    fn prosa_sem_arquivo_nao_e_forcada_a_chamar_nada() {
        let ferramentas = [escrita("computador__escrever_arquivo")];
        // Resposta de fato, sem corpo e sem afirmação de feito.
        assert_eq!(cabeca_de_chamada("A capital do Brasil é Brasília.", &ferramentas), None);
        // Pergunta de como prosseguir: é o usuário quem responde, não o app.
        assert_eq!(
            cabeca_de_chamada("Vou criar o arquivo. Quer que eu proceda?", &ferramentas),
            None
        );
        // Corpo cercado sem afirmação de feito: o usuário pediu para ver o código.
        assert_eq!(
            cabeca_de_chamada("Aqui está o HTML:\n\n```html\n<p>x</p>\n```", &ferramentas),
            None
        );
    }

    /// O turno medido na conversa real que motivou isto: o modelo escreve o
    /// arquivo inteiro, diz "vou atualizar agora" e para pedindo "sim". Com o
    /// corpo na resposta, a cabeceira fecha a prosa e a escrita acontece — com a
    /// aprovação na tela, que é onde ela sempre esteve.
    #[test]
    fn promessa_de_escrita_com_corpo_abre_a_chamada() {
        let ferramentas = [escrita("computador__escrever_arquivo")];
        let resposta = concat!(
            "Vou atualizar o arquivo agora com essas melhorias.\n\n",
            "```html\n<!DOCTYPE html>\n<body>oi</body>\n```\n\n",
            "Confirme se esse é exatamente o que você quer (sim/não)."
        );
        assert!(cabeca_de_chamada(resposta, &ferramentas).is_some());
    }

    /// Os pares abaixo foram lidos do turno real no aplicativo nativo, com
    /// Ornith 1.5 9B, janela 8192 e teto 400 (traço `[corte]` de 16/11): a conta
    /// dizia não em `prompt=3654` enquanto havia 3.738 tokens livres, e o
    /// arquivo não nascia. Com a conta do espaço, a mesma chamada fechou em
    /// sete gerações e a ferramenta escreveu 8.447 bytes.
    #[test]
    fn retoma_ate_a_janela_encher() {
        // A linha em que o teto antigo de três desistiu: ainda cabia.
        assert!(retomada_cabe(3654, 400, 8192, 400, 3));
        // As retomas que de fato rodaram naquele turno.
        for (prompt, continuacoes) in [(1988, 0), (2388, 1), (3188, 3), (3988, 5)] {
            assert!(retomada_cabe(prompt, 400, 8192, 400, continuacoes), "prompt {prompt}");
        }
        // A última geração do mesmo turno: sobrou 30 de espaço, e aí insistir
        // seria girar sem escrever nada.
        assert!(!retomada_cabe(7461, 301, 8192, 400, 6));
        // Janela que não comporta nem uma geração inteira.
        assert!(!retomada_cabe(7500, 400, 4096, 400, 0));
        // A rede de segurança continua valendo com espaço de sobra.
        assert!(!retomada_cabe(1988, 400, 8192, 400, CONTINUACOES_MAX));
    }
}
