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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::MutexGuard;
use std::time::Instant;

use crate::ai::alojamento::Capacidade;
use crate::ai::catalog::ModelSpec;
use crate::ai::session::{TURN_MARKER, system_prompt};
use crate::ai::{ChatMessage, ChatTemplate, Emission, Engine, Generation, SamplingParams, StopReason};
use crate::idiomas::{self, tr};
use crate::runtime;
use crate::state::{AiRuntime, LoadedModel};

/// Os casos da suíte, na ordem em que a tela os mostra.
pub const CASOS: &[&str] = &["placa", "runtime", "esquecer", "prefixo", "parada", "fluxo"];

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
        medidas.push(medida("bancada.medida.cedido", fracionario(alojado.cedido as f64 / 1_048_576.0, 1)));
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
    if !engine.na_placa() {
        return Ok(veredito(
            "prefixo",
            Estado::NaoRodado,
            Vec::new(),
            Some(t("bancada.prefixo.soNaPlaca")),
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

// ─── A execução ───────────────────────────────────────────────────────────────

/// Roda a suíte inteira contra o modelo que o aplicativo está usando — ou contra
/// nenhum, quando a máquina ainda não tem pesos: aí os casos de motor dizem por
/// que não rodaram, em vez de sumirem da tela. A placa é lida antes de o motor
/// ser tomado, que é a mesma ordem do turno de conversa e o que evita dois donos
/// para o mesmo KV cache.
pub fn rodar(
    ai: &AiRuntime,
    spec: Option<&'static ModelSpec>,
    cancel: &AtomicBool,
    mut emit: impl FnMut(Evento),
) -> Vec<Resultado> {
    emit(Evento::Iniciou { total: CASOS.len(), modelo: spec.map(|s| s.display.to_string()) });
    let capacidade = if spec.is_some() { crate::ai::alojamento::capacidade(&ai.dlls).ok().flatten() } else { None };
    // O `runtime` corre à parte: ele confere arquivos em disco e não pede motor.
    let outros = || CASOS.iter().filter(|id| **id != "runtime");

    let mut resultados = Vec::new();
    let inicio = Instant::now();
    emit(Evento::Caso { id: "runtime".to_string() });
    let r = caso_runtime(ai, inicio);
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
