//! Compactação de contexto: o turno antigo que não cabe na janela deixa de ser
//! derrubado e passa a ser lido pelo modelo, que escreve o resumo dele.
//!
//! Derrubar era o que havia, e o custo era invisível: a conversa na tela
//! continuava inteira, mas o modelo parava de ler o começo — e a pessoa via o
//! assistente esquecer um caminho que ela mesma ditou dois turnos antes, ou a
//! recusa seca de "não há lugar na janela". Resumo é o caminho de fora: a
//! informação não some, muda de forma.
//!
//! O texto sai do motor. Não há frase escrita por nós em lugar nenhum daqui: um
//! resumo pronto seria o aplicativo fingindo que leu o que não leu, e é exatamente
//! o tipo de sucesso inventado que o produto proíbe.

use super::chat::{ChatMessage, ChatTemplate};
use super::engine::{Engine, Generation, SamplingParams, StopReason};
use super::session::TURN_MARKER;
use crate::idiomas::tr;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::sync::atomic::AtomicBool;

/// O teto de tokens do resumo. É a conta que a janela faz quando ainda não
/// existe resumo — ele ainda vai nascer, e o espaço tem de ser reservado — e o
/// teto da geração que o escreve. 320 é o tamanho de um turno curto: o bastante
/// para pedir o pedido, os caminhos e o que falta, sem virar uma segunda
/// conversa dentro da primeira.
pub const TETO_TOKENS: usize = 320;

/// O que o resumo ocupa na janela: o teto do texto que ele gera mais a moldura
/// que o template põe em volta da mensagem. Reservar só o texto seria escolher um
/// corte que cabe até o resumo nascer — e aí ele nasce sem onde ficar.
pub const RESERVA: usize = TETO_TOKENS + 32;

/// O que a conversa guarda além das mensagens: o texto que cobre os turnos que
/// saíram do prompt e até onde ele cobre.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Resumo {
    pub texto: String,
    /// Índice da primeira mensagem que o resumo **não** cobre. As anteriores
    /// entram no prompt como este texto; estas, como foram escritas.
    pub ate: usize,
}

impl Resumo {
    /// A mensagem que ocupa o lugar das que ele cobre. É `system` porque o
    /// modelo tem de ler aquilo como o que já aconteceu, não como um pedido
    /// novo — e fica na cabeça do prompt, junto das outras mensagens de estado,
    /// onde o template já aceita bloco de sistema.
    pub fn mensagem(&self) -> ChatMessage {
        ChatMessage::new("system", tr("conversa.resumo", &[("texto", self.texto.trim())]))
    }
}

/// Os tokens que o prompt pode ocupar: a janela menos o que a resposta vai
/// levar. É a mesma conta de sempre, escrita uma vez para o turno e para o
/// resumo não divergirem.
pub fn limite(janela: usize, resposta: usize) -> usize {
    janela.saturating_sub(resposta.min(janela / 2)).max(1)
}

/// Tokens do texto como o modelo os lê. Uma medida de encaixe que não usa este
/// caminho está contando caracteres, não a janela.
pub fn codificar(engine: &Engine, texto: &str) -> Result<Vec<u32>> {
    engine
        .tokenizer()
        .encode(texto, false)
        .map_err(|e| anyhow::anyhow!(tr("erros.tokenizar", &[("detalhe", &e.to_string())])))
        .map(|t| t.get_ids().to_vec())
}

/// O pedido que produz o resumo: a instrução, o que já estava resumido, as
/// mensagens a dobrar e a ordem de fechar. As mensagens entram com os papéis
/// originais porque é assim que o modelo as viu — e um resumo escrito sobre uma
/// transcrição adulterada herda a adulteração.
fn pedido_de_resumo(anterior: Option<&str>, corpo: &[ChatMessage]) -> Vec<ChatMessage> {
    let mut conversa = vec![ChatMessage::new("system", tr("compactacao.instrucao", &[]))];
    if let Some(texto) = anterior.filter(|t| !t.trim().is_empty()) {
        conversa.push(ChatMessage::new(
            "system",
            tr("compactacao.anterior", &[("texto", texto.trim())]),
        ));
    }
    conversa.extend(corpo.iter().cloned());
    conversa.push(ChatMessage::new("user", tr("compactacao.pedido", &[])));
    conversa
}

/// Dobra `trecho` dentro do resumo e devolve o texto novo com o que o motor
/// gastou para escrevê-lo. `anterior` é o resumo dos turnos passados: ele entra
/// na conta para o modelo consolidar, não para esquecer o começo da conversa.
///
/// O trecho a ser lido também tem de caber. Quando não cabe, cai a mensagem
/// mais antiga dele — dos antigos, o que importa lembrar é o que chegou mais
/// perto do que ainda está na janela.
pub fn dobrar(
    engine: &mut Engine,
    template: &ChatTemplate,
    params: &SamplingParams,
    anterior: Option<&str>,
    trecho: &[ChatMessage],
    cancel: &AtomicBool,
) -> Result<(String, Generation)> {
    if trecho.is_empty() {
        bail!(tr("erros.nadaParaResumir", &[]));
    }
    // Sem ferramentas e sem bloco de pensamento: o que se pede aqui é prosa
    // curta. A gramática de chamada custaria tokens e convidaria o modelo a
    // executar em vez de narrar o que já foi executado.
    let amostra = SamplingParams { max_tokens: TETO_TOKENS, ..params.clone() };
    let teto = limite(engine.context(), TETO_TOKENS);
    let mut corpo = trecho.to_vec();
    let ids = loop {
        let conversa = pedido_de_resumo(anterior, &corpo);
        let prompt = template
            .render(&conversa, &[], false)
            .context(tr("erros.renderizarConversa", &[]))?;
        let ids = codificar(engine, prompt.as_str())?;
        if ids.len() <= teto || corpo.len() <= 1 {
            break ids;
        }
        corpo.remove(0);
    };

    let mut stop = engine.paradas().to_vec();
    if !stop.iter().any(|s| s == TURN_MARKER) {
        stop.push(TURN_MARKER.to_string());
    }
    let generation = engine.generate(&ids, &amostra, &stop, cancel, |_| {})?;
    if generation.stop == StopReason::Cancelled {
        bail!(tr("erros.pedidoCancelado", &[]));
    }
    // O modelo pode fechar o turno e continuar falando do outro lado do
    // marcador; o que é resumo é o que vem antes.
    let texto = match generation.text.find(TURN_MARKER) {
        Some(i) => generation.text[..i].to_string(),
        None => generation.text.clone(),
    };
    let texto = texto.trim().to_string();
    if texto.is_empty() {
        // Resposta vazia não é resumo: dizer que dobrou seria apagar sem
        // guardar. O turno para aqui com a verdade.
        bail!(tr("erros.resumoVazio", &[]));
    }
    Ok((texto, generation))
}

#[cfg(test)]
mod testes {
    use super::*;

    /// A reserva é conta, não adjetivo: metade da janela é o piso do que sobra
    /// para ler, e um teto de resposta maior que isso não aumenta o prompt.
    #[test]
    fn o_limite_desconta_a_resposta_sem_passar_de_metade() {
        assert_eq!(limite(4096, 512), 3584);
        assert_eq!(limite(4096, 8192), 2048);
        assert_eq!(limite(2048, 2048), 1024);
        assert_eq!(limite(1, 1024), 1);
    }

    #[test]
    fn o_resumo_fala_como_estado_e_guarda_onde_para() {
        let r = Resumo { texto:  "  a pessoa pediu a página  ".to_string(), ate: 7 };
        assert_eq!(r.ate, 7);
        let m = r.mensagem();
        assert_eq!(m.role, "system");
        // O miolo do texto entra como ele é; o que sobra em volta é o aviso de
        // que aquilo já aconteceu.
        assert!(m.content.contains("a pessoa pediu a página"), "{}", m.content);
        assert!(!m.content.contains("  a pessoa"), "{}", m.content);
    }

    #[test]
    fn sem_resumo_anterior_nao_ha_fantasma_no_pedido() {
        let corpo = vec![ChatMessage::new("user", "crie"), ChatMessage::new("assistant", "criei")];
        let sem = pedido_de_resumo(None, &corpo);
        let com = pedido_de_resumo(Some("já tinha feito x"), &corpo);
        assert_eq!(com.len(), sem.len() + 1);
        assert_eq!(sem.first().unwrap().role, "system");
        assert_eq!(sem.last().unwrap().role, "user");
        assert_eq!(sem[sem.len() - 2].content, "criei");
    }

    /// O que entra no pedido é a transcrição original, com os papéis que o
    /// modelo viu — inclusive a chamada de ferramenta, que é o fato de que ela
    /// aconteceu.
    #[test]
    fn o_trecho_entra_com_os_papeis_originais() {
        let corpo = vec![
            ChatMessage::new("user", "leia"),
            ChatMessage {
                role: "assistant".to_string(),
                content: String::new(),
                tool_calls: Some(vec![crate::ai::ToolCall {
                    name: "local__ler_arquivo".to_string(),
                    arguments: serde_json::json!({}),
                }]),
            },
            ChatMessage::new("tool", "146 linhas"),
        ];
        let pedido = pedido_de_resumo(None, &corpo);
        let papeis: Vec<&str> = pedido.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(&papeis[1..4], ["user", "assistant", "tool"]);
        assert!(pedido[2].tool_calls.is_some());
    }
}
