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
use std::path::{Path, PathBuf};
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

/// A pasta padrão do Aether: `~/.aether`. É ela, e não o arquivo de cada
/// conversa, que se abre de fora para ver o que o modelo dobrou — o resumo mora
/// dentro da conversa porque a conversa precisa dele para continuar, e mora aqui
/// porque a pessoa precisa dele depois que a conversa fecha.
///
/// `AETHER_CASA` move a raiz da casa, não só a pasta do registro: um caminho
/// escrito à mão em dois lugares é como um teste acaba lendo o `.aether` de
/// verdade.
pub fn casa() -> Result<PathBuf> {
    let raiz = std::env::var_os("AETHER_CASA")
        .or_else(|| std::env::var_os("HOME"))
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|r| !r.as_os_str().is_empty());
    match raiz {
        Some(raiz) => Ok(raiz.join(".aether")),
        None => bail!(tr("erros.semPastaAether", &[])),
    }
}

/// O nome da subpasta dentro de `.aether`. Fica separado das conversas de
/// propósito: apagar uma conversa não é apagar a prova do que ela custou.
pub const COMPACTACOES: &str = "compactacoes";

/// Uma dobra como ela fica no disco: o texto que o modelo escreveu, onde ele
/// corta, e o que a dobra cobrou. Os números vêm todos da mesma rodada que
/// produziu o resumo — um registro escrito com uma conta de fora seria o
/// aplicativo narrando a própria memória em vez de registrá-la.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Dobra {
    /// O id da conversa, que é também o nome do arquivo.
    pub conversa: String,
    /// Quantas dobras esta conversa já teve, contando esta. É o que distingue a
    /// terceira dobrada da primeira: o texto do resumo diz o que ficou, este
    /// número diz quantas vezes aquilo foi lido e escrito de novo.
    pub ordem: usize,
    /// O modelo que escreveu o resumo. Uma dobra é saída daquele modelo: lida
    /// com outro, a conversa começa de outro jeito.
    pub modelo: String,
    /// A janela que o motor aceitou nesta rodada, não a que o arquivo declara.
    pub janela: usize,
    pub texto: String,
    /// Índice da primeira mensagem que este texto não cobre.
    pub ate: usize,
    pub lidas: usize,
    pub descartadas: usize,
    /// Tokens que o resumo gerou — o preço medido da dobra, pago na placa.
    pub tokens: usize,
    /// Millis desde a epoch, do relógio do sistema — a mesma unidade que as
    /// fichas de conversa usam, para que uma dobra e o turno que a provocou se
    /// ordenem na mesma escala.
    pub quando: u64,
}

impl Dobra {
    /// Quantas dobras este registro é o último de uma conversa: lida do arquivo
    /// anterior, antes de ele ser substituído. Arquivo ausente ou ilegível é a
    /// primeira dobra, não um erro: o histórico de dobras é um registro, e a
    /// conversa continua sendo a fonte do resumo que o prompt usa.
    pub fn ordem_anterior(raiz: &Path, conversa: &str) -> usize {
        Self::caminho(raiz, conversa)
            .and_then(|caminho| ler_o_ultimo(caminho))
            .unwrap_or(0)
    }

    /// O caminho desta dobra: `<casa>/compactacoes/<conversa>.json`, onde
    /// `raiz` é a própria pasta de compactações, não a `.aether`.
    pub fn caminho(raiz: &Path, conversa: &str) -> Option<PathBuf> {
        crate::conversas::id_valido(conversa).then(|| raiz.join(format!("{conversa}.json")))
    }

    /// Escrita atômica: um arquivo de registro partido ao meio não conta nada
    /// sobre nenhuma conversa. O temporário leva o pid porque duas janelas do
    /// aplicativo podem dobrar a mesma conversa na mesma hora.
    pub fn gravar(&self, raiz: &Path) -> Result<PathBuf> {
        let caminho = Self::caminho(raiz, &self.conversa).ok_or_else(|| {
            anyhow::anyhow!(tr(
                "erros.conversaIdInvalido",
                &[("id", &self.conversa)]
            ))
        })?;
        if let Some(pai) = caminho.parent() {
            std::fs::create_dir_all(pai).map_err(|e| {
                anyhow::anyhow!(tr(
                    "erros.falhaCriarPasta",
                    &[("caminho", &pai.display().to_string()), ("detalhe", &e.to_string())]
                ))
            })?;
        }
        let texto = serde_json::to_string_pretty(self).map_err(|e| {
            anyhow::anyhow!(tr(
                "erros.falhaSerializar",
                &[("caminho", &caminho.display().to_string()), ("detalhe", &e.to_string())]
            ))
        })?;
        let temporario = caminho.with_extension(format!(
            "json.tmp.{}",
            std::process::id()
        ));
        std::fs::write(&temporario, &texto).map_err(|e| {
            anyhow::anyhow!(tr(
                "erros.falhaEscrever",
                &[("caminho", &temporario.display().to_string()), ("detalhe", &e.to_string())]
            ))
        })?;
        std::fs::rename(&temporario, &caminho).map_err(|e| {
            anyhow::anyhow!(tr(
                "erros.falhaMover",
                &[("caminho", &caminho.display().to_string()), ("detalhe", &e.to_string())]
            ))
        })?;
        Ok(caminho)
    }
}

fn ler_o_ultimo(caminho: PathBuf) -> Option<usize> {
    let texto = std::fs::read_to_string(caminho).ok()?;
    serde_json::from_str::<Dobra>(&texto).ok().map(|dobra| dobra.ordem)
}

pub(crate) fn agora() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Os tokens que o prompt pode ocupar: a janela menos o que a resposta vai
/// levar. É a mesma conta de sempre, escrita uma vez para o turno e para o
/// resumo não divergirem.
pub fn limite(janela: usize, resposta: usize) -> usize {
    janela.saturating_sub(resposta.min(janela / 2)).max(1)
}

/// O gatilho macio chega antes do estouro: compacta com folga para a resposta e
/// para o próprio resumo, sem nunca passar do limite duro da janela.
pub fn limite_macio(janela: usize, resposta: usize) -> usize {
    (janela.saturating_mul(85) / 100)
        .min(limite(janela, resposta))
        .max(1)
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

/// Os ids do pedido de resumo. O trecho inteiro entra ou não há resumo: cortar
/// a ponta antiga deixaria um buraco entre o resumo anterior e as mensagens que
/// continuam no prompt.
fn ids_do_resumo(
    trecho: &[ChatMessage],
    teto: usize,
    medir: &dyn Fn(&[ChatMessage]) -> Result<Vec<u32>>,
) -> Result<(Vec<u32>, usize)> {
    let ids = medir(trecho)?;
    if ids.len() > teto {
        bail!(tr(
            "erros.promptForaDaJanela",
            &[("tokens", &ids.len().to_string()), ("janela", &teto.to_string())]
        ));
    }
    Ok((ids, trecho.len()))
}

pub fn resumo_cabe(
    engine: &Engine,
    template: &ChatTemplate,
    anterior: Option<&str>,
    trecho: &[ChatMessage],
) -> Result<bool> {
    let prompt = template
        .render(&pedido_de_resumo(anterior, trecho), &[], false)
        .context(tr("erros.renderizarConversa", &[]))?;
    Ok(codificar(engine, prompt.as_str())?.len() <= limite(engine.context(), TETO_TOKENS))
}

/// Dobra `trecho` dentro do resumo e devolve o texto novo com o que o motor
/// gastou para escrevê-lo. `anterior` é o resumo dos turnos passados: ele entra
/// na conta para o modelo consolidar, não para esquecer o começo da conversa.
///
/// O terceiro número é quantas mensagens do trecho o modelo leu de fato. Ele não
/// é enfeite: quem mostra "mensagens dobradas" na tela tem de mostrar este, e
/// não o tamanho do trecho.
pub fn dobrar(
    engine: &mut Engine,
    template: &ChatTemplate,
    params: &SamplingParams,
    anterior: Option<&str>,
    trecho: &[ChatMessage],
    cancel: &AtomicBool,
) -> Result<(String, Generation, usize)> {
    if trecho.is_empty() {
        bail!(tr("erros.nadaParaResumir", &[]));
    }
    // Sem ferramentas e sem bloco de pensamento: o que se pede aqui é prosa
    // curta. A gramática de chamada custaria tokens e convidaria o modelo a
    // executar em vez de narrar o que já foi executado.
    let amostra = SamplingParams { max_tokens: TETO_TOKENS, ..params.clone() };
    let (ids, lidas) = ids_do_resumo(trecho, limite(engine.context(), TETO_TOKENS), &|corpo| {
        Ok(codificar(
            engine,
            template
                .render(&pedido_de_resumo(anterior, corpo), &[], false)
                .context(tr("erros.renderizarConversa", &[]))?
                .as_str(),
        )?)
    })?;

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
    Ok((texto, generation, lidas))
}

#[cfg(test)]
mod testes {
    use super::*;

    fn raiz_de_teste(nome: &str) -> PathBuf {
        let raiz = std::env::temp_dir().join(format!(
            "aether-compactacao-{}-{nome}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&raiz);
        raiz
    }

    fn dobra(conversa: &str, ordem: usize, texto: &str) -> Dobra {
        Dobra {
            conversa: conversa.to_string(),
            ordem,
            modelo: "granite-4.2-3b".to_string(),
            janela: 8192,
            texto: texto.to_string(),
            ate: 7,
            lidas: 5,
            descartadas: 1,
            tokens: 214,
            quando: 1_700_000_000_000,
        }
    }

    /// A casa é a `.aether` pedida, e não um caminho escolhido por quem chama.
    #[test]
    fn o_registro_mora_na_pasta_padrao_do_aether() {
        let casa = casa().expect("a máquina tem pasta de casa");
        assert_eq!(
            casa.file_name().map(|n| n.to_string_lossy().to_string()),
            Some(".aether".to_string())
        );
    }

    #[test]
    fn uma_dobra_ocupa_um_arquivo_e_volta_inteira_da_leitura() {
        let raiz = raiz_de_teste("gravada");
        let feita = dobra("11111111-2222", 1, "a pessoa pediu a página");
        let caminho = feita.gravar(&raiz).expect("escreve");
        assert!(caminho.starts_with(&raiz), "{}", caminho.display());
        assert_eq!(caminho.file_name().unwrap().to_string_lossy(), "11111111-2222.json");
        let lida: Dobra =
            serde_json::from_str(&std::fs::read_to_string(&caminho).expect("lê")).expect("JSON");
        assert_eq!(lida, feita);
        // Escrita atômica: o temporário é o que desaparece, não o registro.
        assert!(
            std::fs::read_dir(&raiz)
                .expect("a pasta existe")
                .filter_map(|e| e.ok())
                .all(|e| !e.file_name().to_string_lossy().contains(".tmp.")),
            "nenhum temporário sobra"
        );
    }

    #[test]
    fn a_conta_de_dobras_segue_o_registro_e_o_ilegivel_recomeca_sem_travar() {
        let raiz = raiz_de_teste("ordem");
        let id = "1234-abcd";
        assert_eq!(Dobra::ordem_anterior(&raiz, id), 0, "sem arquivo não houve dobra");
        dobra(id, 1, "primeira").gravar(&raiz).expect("grava a primeira");
        assert_eq!(Dobra::ordem_anterior(&raiz, id), 1);
        std::fs::write(Dobra::caminho(&raiz, id).expect("caminho"), "{ quebrado")
            .expect("corrompe");
        assert_eq!(
            Dobra::ordem_anterior(&raiz, id),
            0,
            "um registro ilegível não pode travar a conversa: a conversa é a fonte"
        );
    }

    /// O id vem da interface. Um `../` nele seria o registro escrito fora da
    /// pasta — e a pasta fora do registro não é criada nem para o erro.
    #[test]
    fn um_id_fora_do_formato_nao_escreve_fora_da_pasta() {
        let raiz = raiz_de_teste("fuga");
        assert!(Dobra::caminho(&raiz, "../escapada").is_none());
        assert!(Dobra::caminho(&raiz, "").is_none());
        let erro = dobra("../escapada", 1, "x")
            .gravar(&raiz)
            .expect_err("o id recusado não escreve");
        assert!(erro.to_string().contains("escapada"), "{erro}");
        assert!(!raiz.exists(), "nada é criado para um id recusado");
    }

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

    /// Custo em tokens de mentira: cada mensagem custa o número escrito no seu
    /// corpo. É a única forma de conferir a conta da régua sem placa — o que se
    /// testa é a aritmética do corte, não o tokenizer.
    fn medir_por_corpo(corpo: &[ChatMessage]) -> Result<Vec<u32>> {
        let n = corpo
            .iter()
            .map(|m| m.content.parse::<usize>().unwrap_or(1))
            .sum();
        Ok(vec![0u32; n])
    }

    #[test]
    fn o_gatilho_macio_antecede_o_duro_sem_ultrapassar_a_resposta() {
        assert_eq!(limite_macio(8192, 512), 6963);
        assert_eq!(limite_macio(65536, 900), 55705);
        assert_eq!(limite_macio(2048, 2048), 1024);
        assert_eq!(limite_macio(1, 1024), 1);
    }

    /// O trecho que não coube não perde as mensagens antigas em silêncio: sem
    /// resumo integral, a fronteira gravada mentiria sobre o que o modelo leu.
    #[test]
    fn o_trecho_acima_da_regua_falha_em_vez_de_descartar_o_comeco() {
        let trecho: Vec<ChatMessage> = (0..5)
            .map(|_| ChatMessage::new("user", "100"))
            .collect();
        let erro = ids_do_resumo(&trecho, 300, &medir_por_corpo)
            .expect_err("um pedido de resumo truncado não pode ser aprovado");
        assert!(erro.to_string().contains("500"), "{erro}");
        assert!(erro.to_string().contains("300"), "{erro}");
    }

    #[test]
    fn quando_cabe_tudo_o_numero_diz_todas_as_mensagens_lidas() {
        let trecho: Vec<ChatMessage> = (0..5)
            .map(|_| ChatMessage::new("user", "100"))
            .collect();
        let (ids, lidas) = ids_do_resumo(&trecho, 900, &medir_por_corpo).unwrap();
        assert_eq!(lidas, 5);
        assert_eq!(ids.len(), 500);
    }

    #[test]
    fn uma_mensagem_acima_da_regua_tambem_falha_sem_fingir_que_foi_lida() {
        let trecho = vec![ChatMessage::new("tool", "900")];
        let erro = ids_do_resumo(&trecho, 100, &medir_por_corpo)
            .expect_err("não há resumo possível de uma mensagem que não cabe inteira");
        assert!(erro.to_string().contains("900"), "{erro}");
        assert!(erro.to_string().contains("100"), "{erro}");
    }
}
