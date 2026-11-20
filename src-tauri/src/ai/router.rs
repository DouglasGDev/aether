//! Escolha de modelo por complexidade e hardware.
//!
//! Isto é uma política de recurso, não uma alegação de compreensão: a classe de
//! complexidade sai de sinais contáveis (tokens, ferramentas, marcadores) e o
//! motivo exibido cita exatamente esses sinais.

use super::catalog::{self, ModelSpec, Tier};
use crate::idiomas::tr;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Complexity {
    Trivial,
    Media,
    Alta,
    Critica,
}

impl Complexity {
    pub fn label(self) -> String {
        tr(
            match self {
                Self::Trivial => "roteador.trivial",
                Self::Media => "roteador.media",
                Self::Alta => "roteador.alta",
                Self::Critica => "roteador.critica",
            },
            &[],
        )
    }

    fn tier(self) -> Tier {
        match self {
            Self::Trivial | Self::Media => Tier::Leve,
            Self::Alta | Self::Critica => Tier::Pesado,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Request {
    pub text: String,
    pub prompt_tokens: usize,
    pub needs_tools: bool,
    /// A janela de contexto que será pedida ao carregar. O KV cache é proporcional
    /// a ela, e é por isso que a mesma placa pode levar um modelo inteiro num tamanho
    /// de janela e não levar no outro.
    pub janela: usize,
}

/// As duas memórias que existem numa máquina com placa. A decisão de velocidade
/// olha para a placa; a RAM do sistema entra onde ela não existe, e é por isso
/// que as duas são medidas e nenhuma é inferida da outra.
#[derive(Debug, Clone)]
pub struct Hardware {
    pub total_ram_bytes: u64,
    pub available_ram_bytes: u64,
    /// A placa que o motor escolheria agora, com a VRAM livre medida neste
    /// instante. `None` é "esta máquina não tem placa" — o mesmo caso em que
    /// `ggml::Motor::novo` falha e a carga cai para CPU.
    pub placa: Option<super::alojamento::Capacidade>,
}

/// RAM do sistema e VRAM da placa, medidas na hora. Sem as pastas da ggml não há
/// como perguntar à placa, e o resultado é o honesto `placa: None` — não um zero
/// que faria qualquer modelo parecer maior do que a memória disponível.
pub fn hardware_agora(dlls: &[std::path::PathBuf]) -> anyhow::Result<Hardware> {
    let mut hardware = measure_hardware()?;
    hardware.placa = super::alojamento::capacidade(dlls).ok().flatten();
    Ok(hardware)
}

/// RAM medida no sistema operacional, não estimada. O motivo exibido ao usuário
/// cita este número, então ele tem de vir de uma medição real.
#[cfg(windows)]
pub fn measure_hardware() -> anyhow::Result<Hardware> {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: `status` é um MEMORYSTATUSEX válido com dwLength preenchido, e a
    // função só escreve nele.
    unsafe { GlobalMemoryStatusEx(&mut status) }
        .map_err(|e| anyhow::anyhow!(tr("erros.medirRam", &[("detalhe", &e.to_string())])))?;
    Ok(Hardware {
        total_ram_bytes: status.ullTotalPhys,
        available_ram_bytes: status.ullAvailPhys,
        placa: None,
    })
}

#[cfg(not(windows))]
pub fn measure_hardware() -> anyhow::Result<Hardware> {
    anyhow::bail!("{}", tr("erros.ramNaoImplementada", &[]))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Choice {
    pub model_id: String,
    pub display: String,
    pub complexity: Complexity,
    pub reason: String,
    pub signals: Vec<String>,
}

/// Marcadores de intenção. Diferente das demais strings, isto não vem dos
/// catálogos: o usuário escreve no idioma dele, não no idioma da interface,
/// então a lista cobre os três idiomas que a aplicação fala de uma vez.
const DESTRUCTIVE: [&str; 21] = [
    "apagar", "deletar", "excluir", "remover", "formatar", "drop ", "truncate", "destruir",
    "sobrescrever", "rm -rf", "delete", "erase", "wipe", "remove ", "destroy", "overwrite",
    "borrar", "eliminar", "supprimer", "löschen", "cancellare",
];
const CODE: [&str; 17] = [
    "```", "código", "codigo", "implemente", "refatore", "função", "funcao", "script", "sql",
    "code", "function", "refactor", "compile", "debug", "depurar", "programm", "compilar",
];
const MULTI_STEP: [&str; 14] = [
    "passo a passo", "compare", "analise", "análise", "planeje", "estratégia", "estrategia",
    "step by step", "analyze", "plan ", "strategy", "outline", "etapa a etapa", "planifique",
];

/// Classifica e devolve os sinais que dispararam, para o motivo ser auditável.
pub fn classify(request: &Request) -> (Complexity, Vec<String>) {
    let lower = request.text.to_lowercase();
    let mut signals = Vec::new();
    signals.push(tr("roteador.sinalTokens", &[("n", &request.prompt_tokens.to_string())]));

    let hits = |words: &[&str]| -> Vec<String> {
        words.iter().filter(|w| lower.contains(**w)).map(|w| w.trim().to_string()).collect()
    };

    let destructive = hits(&DESTRUCTIVE);
    if !destructive.is_empty() {
        signals.push(tr("roteador.sinalIrreversivel", &[("marcas", &destructive.join(", "))]));
        return (Complexity::Critica, signals);
    }

    let code = hits(&CODE);
    let steps = hits(&MULTI_STEP);
    if !code.is_empty() {
        signals.push(tr("roteador.sinalCodigo", &[("marcas", &code.join(", "))]));
    }
    if !steps.is_empty() {
        signals.push(tr("roteador.sinalEtapas", &[("marcas", &steps.join(", "))]));
    }
    if request.needs_tools {
        signals.push(tr("roteador.sinalFerramentas", &[]));
    }
    if request.prompt_tokens > 400 {
        signals.push(tr("roteador.sinalLongo", &[]));
    }
    if !code.is_empty() || !steps.is_empty() || request.prompt_tokens > 400 {
        return (Complexity::Alta, signals);
    }

    if request.prompt_tokens <= 12 && !request.needs_tools {
        signals.push(tr("roteador.sinalCurto", &[]));
        return (Complexity::Trivial, signals);
    }
    (Complexity::Media, signals)
}

/// Um candidato do roteador: a entrada do catálogo e o cabeçalho do arquivo que
/// está em disco. `None` é "não medimos" — falta o GGUF, ou ele não é legível —
/// e nunca deve ser lido como "cabe".
#[derive(Debug, Clone)]
pub struct Candidato {
    pub spec: &'static ModelSpec,
    pub ficha: Option<super::alojamento::Ficha>,
}

/// A janela que seria pedida por este candidato: o que está configurado, cortado
/// pelo teto do próprio modelo. O KV cache é proporcional a ela, e medir com
/// outra janela seria prometer um alojamento que a carga não faz.
fn janela_efetiva(c: &Candidato, pedida: usize) -> usize {
    pedida.min(c.spec.context_limit)
}

/// O posto de um candidato: a colocação na placa, da mais rápida para a mais
/// lenta, e se a parte que sobra para fora dela cabe na RAM livre medida.
///
/// O segundo lado existe porque modelo grande dividido é legítimo — é assim que
/// um 70B roda numa placa de 16 GiB —, mas as camadas que ficam de fora têm de
/// morar em memória real. Passar da RAM livre não é ficar lento aos poucos: é
/// trocar com disco a cada token, e aí a placa deixa de ser o motivo da escolha.
fn posto(c: &Candidato, hardware: &Hardware, pedida: usize) -> (u8, bool) {
    use super::alojamento::Alojamento;
    let Some(ficha) = &c.ficha else {
        // Sem ficha não há o que prometer: entra por último na colocação e,
        // principalmente, não afirma que cabe. O `false` é o invariante deste
        // módulo — `None` é "não medimos", nunca "cabe" — e é ele que impede um
        // arquivo pela metade de passar no freio `cabem` como se tivesse sido
        // medido. Ainda entra, porque é o que há em disco e a escolha final
        // recorre ao último candidato quando ninguém foi medido.
        return (3, false);
    };
    let janela = janela_efetiva(c, pedida);
    match super::alojamento::alojar(ficha, hardware.placa.as_ref(), janela) {
        Alojamento::Inteira { .. } => (0, true),
        Alojamento::Dividida { camadas, em_ram, .. } => {
            (if camadas > 0 { 1 } else { 2 }, em_ram <= hardware.available_ram_bytes)
        }
        Alojamento::SoSistema { pesos, kv } => (2, pesos + kv <= hardware.available_ram_bytes),
    }
}

/// A frase de colocação que vai no motivo: o que a placa leva, com o número que
/// ela mesma permitiu.
fn situacao(c: &Candidato, hardware: &Hardware, pedida: usize) -> String {
    use super::alojamento::{Alojamento, media_da_camada};
    let Some(ficha) = &c.ficha else {
        return tr("roteador.semMedida", &[]);
    };
    let janela = janela_efetiva(c, pedida);
    match super::alojamento::alojar(ficha, hardware.placa.as_ref(), janela) {
        Alojamento::Inteira { pesos, kv, .. } => tr(
            "roteador.naPlaca",
            &[("pesos", &gibs(pesos)), ("kv", &gibs(kv))],
        ),
        Alojamento::Dividida { camadas, total, na_placa, em_ram, .. } => {
            let passo = gibs(media_da_camada(ficha));
            tr(
                "roteador.dividido",
                &[
                    ("camadas", &camadas.to_string()),
                    ("total", &total.to_string()),
                    ("na_placa", &gibs(na_placa)),
                    ("em_ram", &gibs(em_ram)),
                    ("passo", &passo),
                ],
            )
        }
        Alojamento::SoSistema { pesos, kv } => tr(
            "roteador.noSistema",
            &[("pesos", &gibs(pesos)), ("kv", &gibs(kv))],
        ),
    }
}

fn gibs(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
}

pub fn choose(
    request: &Request,
    hardware: Hardware,
    available: &[Candidato],
) -> Option<Choice> {
    let (complexity, mut signals) = classify(request);
    if available.is_empty() {
        return None;
    }
    let wanted = complexity.tier();
    let janela = request.janela;

    // Quants abaixo do piso de qualidade só entram se forem a única coisa em
    // disco. Dentro do tier pedido, e depois dele, o desempate é de velocidade:
    // o que fica inteiro na placa ganha do que divide, e o que divide ganha do
    // que não entra na placa. O tamanho decide só quando a colocação empata.
    //
    // A ordem é a regra que ele deu: primeiro o lugar mais rápido. O tier pedido
    // vem antes disso enquanto houver quem o use, mas um modelo que não pega
    // nada da placa perde para o que pega — e aí a nota diz que caiu de nível.
    // `cabem` é o freio: dividir é legítimo, passar da RAM livre medida é paging
    // a cada token, e paging não é um lugar lento, é a ausência dele.
    let chave = |c: &&Candidato| {
        let (colocacao, cabem) = posto(c, &hardware, janela);
        (colocacao, !cabem, c.spec.weights_bytes)
    };
    let na_placa = |c: &&Candidato| posto(c, &hardware, janela).0 < 2;
    let cabem = |c: &&Candidato| posto(c, &hardware, janela).1;
    let serve_e_usa_a_placa = |c: &&Candidato| cabem(c) && na_placa(c);
    let mut pool: Vec<&Candidato> =
        available.iter().filter(|c| c.spec.routable).collect();
    if pool.is_empty() {
        pool = available.iter().collect();
    }
    let do_tier: Vec<&Candidato> =
        pool.iter().copied().filter(|c| c.spec.tier == wanted).collect();
    let picked = do_tier
        .iter()
        .copied()
        .filter(serve_e_usa_a_placa)
        .min_by_key(chave)
        .or_else(|| pool.iter().copied().filter(serve_e_usa_a_placa).min_by_key(chave))
        .or_else(|| do_tier.iter().copied().filter(|c| cabem(c)).min_by_key(chave))
        .or_else(|| pool.iter().copied().filter(|c| cabem(c)).min_by_key(chave))
        .or_else(|| do_tier.iter().copied().min_by_key(chave))
        .or_else(|| pool.first().copied())?;

    if let Some(placa) = &hardware.placa {
        signals.push(tr(
            "roteador.sinalPlaca",
            &[
                ("nome", &placa.placa),
                ("livre", &gibs(placa.livre)),
                ("total", &gibs(placa.total)),
            ],
        ));
    } else {
        signals.push(tr("roteador.sinalSemPlaca", &[]));
    }
    signals.push(tr(
        "roteador.sinalMemoria",
        &[("livre", &gibs(hardware.available_ram_bytes)),
          ("total", &gibs(hardware.total_ram_bytes))],
    ));
    if picked.spec.tier != wanted {
        signals.push(tr("roteador.reduzido", &[]));
    }
    if picked.ficha.is_none() {
        signals.push(tr("roteador.sinalSemFicha", &[]));
    }
    let reason = tr(
        "roteador.motivo",
        &[
            ("complexidade", &complexity.label()),
            ("situacao", &situacao(picked, &hardware, janela)),
        ],
    );

    Some(Choice {
        model_id: picked.spec.id.to_string(),
        display: picked.spec.display.to_string(),
        complexity,
        reason,
        signals,
    })
}

/// Menor modelo completo em disco: o padrão quando nada foi pedido ainda.
pub fn default_model(root: &std::path::Path) -> Option<&'static ModelSpec> {
    let complete: Vec<&ModelSpec> = catalog::CATALOG
        .iter()
        .filter(|m| catalog::status(root, m).complete)
        .collect();
    complete
        .iter()
        .filter(|m| m.routable)
        .min_by_key(|m| m.weights_bytes)
        .or_else(|| complete.iter().min_by_key(|m| m.weights_bytes))
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::alojamento::{Capacidade, Ficha};

    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * MIB;

    fn request(text: &str, tokens: usize, tools: bool) -> Request {
        Request { text: text.to_string(), prompt_tokens: tokens, needs_tools: tools, janela: 4096 }
    }

    fn pedido(janela: usize) -> Request {
        Request { text: "implemente um parser".to_string(), prompt_tokens: 12, needs_tools: false, janela }
    }

    /// Máquina sem placa: só a RAM do sistema entra na conta.
    fn hardware(livre_ram_gib: u64) -> Hardware {
        Hardware {
            total_ram_bytes: 32 * GIB,
            available_ram_bytes: livre_ram_gib * GIB,
            placa: None,
        }
    }

    fn hardware_na_placa(livre_ram_gib: u64, livre_vram_mib: u64) -> Hardware {
        Hardware {
            total_ram_bytes: 32 * GIB,
            available_ram_bytes: livre_ram_gib * GIB,
            placa: Some(Capacidade {
                placa: "AMD Radeon RX 9060 XT".to_string(),
                livre: livre_vram_mib * MIB,
                total: 16 * GIB,
            }),
        }
    }

    /// Fichas com a geometria medida nos arquivos reais: o 3B Q4_K_M tem 40
    /// camadas de 20 MiB sobre 2136 MiB de pesos e 8 cabeças de KV de 64; o 8B
    /// tem 40 camadas de 116 MiB sobre 5099 MiB e cabeças de 128. Os testes não
    /// lêem disco — o que se confere aqui é a decisão; a sonda `engine-probe
    /// alojar:` é quem confere a ficha contra a carga real.
    fn ficha_3b() -> Ficha {
        Ficha {
            arquivo: "granite-4.2-3b-Q4_K_M.gguf".to_string(),
            arquitetura: "granite".to_string(),
            camadas: 40,
            pesos_total: 2136 * MIB,
            camada: vec![20 * MIB; 40],
            fixo: 2136 * MIB - 40 * 20 * MIB,
            kv_por_token: 2 * 40 * 8 * 64 * 2,
            janela_maxima: 131_072,
            template: None,
            eos: vec![100257],
            paradas: vec!["<|im_end|>".to_string()],
        }
    }

    fn ficha_8b() -> Ficha {
        Ficha {
            arquivo: "granite-4.2-8b-Q4_K_M.gguf".to_string(),
            arquitetura: "granite".to_string(),
            camadas: 40,
            pesos_total: 5099 * MIB,
            camada: vec![116 * MIB; 40],
            fixo: 5099 * MIB - 40 * 116 * MIB,
            kv_por_token: 2 * 40 * 8 * 128 * 2,
            janela_maxima: 131_072,
            template: None,
            eos: vec![100257],
            paradas: vec!["<|im_end|>".to_string()],
        }
    }

    fn candidato(id: &str, ficha: Option<Ficha>) -> Candidato {
        Candidato { spec: catalog::find(id).unwrap(), ficha }
    }

    fn par() -> Vec<Candidato> {
        vec![candidato("granite-4.2-3b-Q4_K_M", Some(ficha_3b())),
             candidato("granite-4.2-8b-Q4_K_M", Some(ficha_8b()))]
    }

    #[test]
    fn short_greeting_is_trivial() {
        let (c, _) = classify(&request("oi, tudo bem?", 6, false));
        assert_eq!(c, Complexity::Trivial);
    }

    #[test]
    fn destructive_intent_is_critical_even_when_short() {
        let (c, signals) = classify(&request("apagar tudo", 3, false));
        assert_eq!(c, Complexity::Critica);
        assert!(signals.iter().any(|s| s.contains("irreversível")));
    }

    /// Os marcadores não seguem o idioma da interface: o usuário escreve no dele.
    #[test]
    fn destructive_intent_in_english_is_still_critical() {
        let (c, _) = classify(&request("delete every file in the folder", 8, false));
        assert_eq!(c, Complexity::Critica);
    }

    #[test]
    fn code_intent_in_english_is_still_high_complexity() {
        let (c, _) = classify(&request("refactor this function step by step", 14, false));
        assert_eq!(c, Complexity::Alta);
    }

    #[test]
    fn code_request_is_high_complexity() {
        let (c, _) = classify(&request("implemente uma função em rust", 12, false));
        assert_eq!(c, Complexity::Alta);
    }

    #[test]
    fn long_request_is_high_complexity() {
        let (c, _) = classify(&request("explique", 500, false));
        assert_eq!(c, Complexity::Alta);
    }

    #[test]
    fn ordinary_question_is_medium() {
        // Acima do limiar de 12 tokens do Trivial, sem código nem múltiplas etapas.
        let (c, _) = classify(&request(
            "qual é a capital do Brasil e em que ano ela foi inaugurada?",
            20,
            false,
        ));
        assert_eq!(c, Complexity::Media);
    }

    #[test]
    fn no_downloaded_model_means_no_choice() {
        assert!(choose(&request("oi", 2, false), hardware(16), &[]).is_none());
    }

    #[test]
    fn picks_heavy_tier_for_complex_request_when_it_fits() {
        let choice = choose(&request("implemente um parser", 12, false),
                            hardware_na_placa(16, 15 * 1024),
                            &par()).unwrap();
        assert_eq!(choice.model_id, "granite-4.2-8b-Q4_K_M");
        assert_eq!(choice.complexity, Complexity::Alta);
    }

    /// A reclamação do usuário, em forma de teste: com 3 GiB de RAM livre e a
    /// placa vazia, quem decide é a placa. Antes o roteador olhava só para a RAM
    /// do sistema e derrubava o modelo que a placa levaria inteira.
    #[test]
    fn a_placa_segura_o_modelo_que_a_ram_do_sistema_recusaria() {
        let choice = choose(&request("implemente um parser", 12, false),
                            hardware_na_placa(3, 15 * 1024),
                            &par()).unwrap();
        assert_eq!(choice.model_id, "granite-4.2-8b-Q4_K_M");
        assert!(!choice.signals.iter().any(|s| s.contains("caiu de nível")), "{:?}", choice.signals);
        assert!(choice.signals.iter().any(|s| s.contains("9060")), "{:?}", choice.signals);
        assert!(choice.reason.contains("na placa"), "{}", choice.reason);
    }

    /// Sem placa não há para onde correr: o que não cabe na RAM vira disco a
    /// cada token, e aí cair de nível é a resposta certa.
    #[test]
    fn sem_placa_a_ram_apertada_forcai_o_modelo_menor() {
        let choice = choose(&request("implemente um parser", 12, false),
                            hardware(4),
                            &par()).unwrap();
        assert_eq!(choice.model_id, "granite-4.2-3b-Q4_K_M");
        assert!(choice.signals.iter().any(|s| s.contains("caiu de nível")), "{:?}", choice.signals);
        assert!(choice.reason.contains("memória do sistema"), "{}", choice.reason);
    }

    /// Modelo grande dividido não é motivo de rebaixamento: é a conta da placa.
    /// O usuário pediu para poder baixar modelos bem maiores, então 27 camadas
    /// na placa e o resto em RAM ganham do 3B inteiro.
    #[test]
    fn placa_apertada_mantem_o_grande_dividido_sem_cair_de_nivel() {
        let (colocacao, cabem) = posto(&par()[1], &hardware_na_placa(32, 4 * 1024), 4096);
        assert_eq!(colocacao, 1);
        assert!(cabem);
        let choice = choose(&pedido(4096), hardware_na_placa(32, 4 * 1024), &par()).unwrap();
        assert_eq!(choice.model_id, "granite-4.2-8b-Q4_K_M");
        assert!(choice.reason.contains("camadas na placa"), "{}", choice.reason);
        assert!(choice.reason.contains("memória do sistema"), "{}", choice.reason);
    }

    /// Placa sem orçamento para nenhuma camada do modelo grande: ele não perde
    /// só a qualidade, perde o motivo. Ficar inteiro na RAM é paging em relação
    /// a quem usa a placa, e a regra dele é o lugar mais rápido.
    #[test]
    fn sem_orcamento_na_placa_para_o_grande_fica_com_o_que_a_usa() {
        let hardware = hardware_na_placa(32, 700);
        assert_eq!(posto(&par()[1], &hardware, 4096), (2, true));
        assert_eq!(posto(&par()[0], &hardware, 4096), (1, true));
        let choice = choose(&pedido(4096), hardware, &par()).unwrap();
        assert_eq!(choice.model_id, "granite-4.2-3b-Q4_K_M");
        assert!(choice.signals.iter().any(|s| s.contains("caiu de nível")), "{:?}", choice.signals);
        assert!(choice.reason.contains("camadas na placa"), "{}", choice.reason);
    }

    /// A janela pedida é parte da conta: a mesma placa leva o modelo inteiro
    /// numa janela e não leva na outra, e o que muda é o KV, não o peso.
    #[test]
    fn a_janela_pedida_muda_a_colocacao_do_mesmo_modelo() {
        let hardware = hardware_na_placa(32, 8 * 1024);
        let tres = candidato("granite-4.2-3b-Q4_K_M", Some(ficha_3b()));
        assert_eq!(posto(&tres, &hardware, 4096).0, 0);
        assert_eq!(posto(&tres, &hardware, 16_384).0, 0);
        assert_eq!(posto(&tres, &hardware, 131_072).0, 2);
    }

    #[test]
    fn trivial_request_prefers_the_smallest() {
        let choice = choose(&request("oi", 2, false),
                            hardware_na_placa(16, 15 * 1024),
                            &par()).unwrap();
        assert_eq!(choice.model_id, "granite-4.2-3b-Q4_K_M");
    }

    /// Uma quant menor não é "o menor modelo capaz": abaixo do piso a qualidade
    /// cai, então o roteador só a usa se não houver alternativa.
    #[test]
    fn aggressive_quant_loses_to_a_good_one_even_though_it_is_smaller() {
        let candidatos = vec![
            candidato("granite-4.2-3b-Q2_K", Some(ficha_3b())),
            candidato("granite-4.2-3b-Q4_K_M", Some(ficha_3b())),
        ];
        let choice = choose(&request("oi", 2, false), hardware_na_placa(16, 15 * 1024), &candidatos).unwrap();
        assert_eq!(choice.model_id, "granite-4.2-3b-Q4_K_M");
    }

    #[test]
    fn an_aggressive_quant_is_still_used_when_it_is_all_there_is() {
        let candidatos = vec![candidato("granite-4.2-3b-Q2_K", Some(ficha_3b()))];
        let choice = choose(&request("oi", 2, false), hardware_na_placa(16, 15 * 1024), &candidatos).unwrap();
        assert_eq!(choice.model_id, "granite-4.2-3b-Q2_K");
    }

    /// Arquivo pela metade é o caso em que não há ficha. O roteador ainda
    /// escolhe — é o que existe em disco —, mas diz que não mediu.
    #[test]
    fn sem_ficha_a_escolha_acontece_e_diz_que_nao_mediu() {
        let candidatos = vec![candidato("granite-4.2-3b-Q4_K_M", None)];
        let choice = choose(&request("oi", 2, false), hardware_na_placa(16, 15 * 1024), &candidatos).unwrap();
        assert_eq!(choice.model_id, "granite-4.2-3b-Q4_K_M");
        assert!(choice.reason.contains("não medido"), "{}", choice.reason);
        assert!(choice.signals.iter().any(|s| s.contains("sem cabeçalho")), "{:?}", choice.signals);
    }

    /// O invariante escrito na struct: `None` é "não medimos" e nunca deve ser
    /// lido como "cabe". Antes o `posto` de um candidato sem ficha devolvia
    /// `(3, true)` — último na colocação, mas afirmando caber sem nunca ter
    /// medido. Aqui é o `false` que se cobra.
    #[test]
    fn candidato_sem_ficha_nao_afirma_que_cabe() {
        let hardware = hardware_na_placa(16, 15 * 1024);
        let sem_ficha = candidato("granite-4.2-3b-Q4_K_M", None);
        assert_eq!(
            posto(&sem_ficha, &hardware, 4096),
            (3, false),
            "sem ficha: entra por último e não jura que cabe"
        );
    }

    #[test]
    fn reason_cites_complexity_and_memory_and_display_names_the_model() {
        let candidatos = vec![candidato("granite-4.2-3b-Q4_K_M", Some(ficha_3b()))];
        let choice = choose(&request("oi", 2, false), hardware_na_placa(16, 15 * 1024), &candidatos).unwrap();
        assert!(choice.reason.contains("Trivial"));
        assert!(choice.reason.contains("GiB"));
        assert_eq!(choice.display, "Granite 4.2 3B Q4_K_M");
        assert!(choice.signals.iter().any(|s| s.contains("tokens")));
    }

    /// Sem placa a tela não pode insinuar uma: o sinal diz que a conta foi pela
    /// memória do sistema, porque foi exatamente isso que ela fez.
    #[test]
    fn sem_placa_o_sinal_diz_que_a_conta_foi_pela_ram() {
        let candidatos = vec![candidato("granite-4.2-3b-Q4_K_M", Some(ficha_3b()))];
        let choice = choose(&request("oi", 2, false), hardware(16), &candidatos).unwrap();
        assert!(choice.signals.iter().any(|s| s.contains("nenhuma placa")), "{:?}", choice.signals);
    }
}