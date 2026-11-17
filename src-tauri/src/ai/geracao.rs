//! As escalas da geração, guardadas em `geracao.json`.
//!
//! Antes cada turno samplava números escritos no código: temperatura 0,2, top-k
//! 40, 512 tokens. Nada disso é escolha do dono da máquina. Aqui a escolha é
//! dele, entra num arquivo, e é lida no momento em que o turno é montado — um
//! ajuste que a UI mostra mas o núcleo ignora é exatamente a gambiarra que o
//! produto proíbe.

use crate::idiomas::tr;
use serde::{Deserialize, Serialize};
use std::path::Path;

use super::engine::SamplingParams;

pub const TEMPERATURA_MIN: f32 = 0.0;
pub const TEMPERATURA_MAX: f32 = 2.0;
pub const TOP_K_MAX: u32 = 200;
pub const MAX_TOKENS_MIN: u32 = 16;
pub const MAX_TOKENS_MAX: u32 = 8192;
/// A janela que o núcleo aceita reservar. O teto é o maior `context_limit` do
/// catálogo — hoje 262.144, o que o Ornith 1.5 9B publica no cabeçalho dos
/// próprios pesos. Acima disso cada placa decide o que sobrevive, e o que ela
/// aceitou é o que `Engine::context` devolve: `alojamento::alojar` mede os pesos
/// mais o KV da janela pedida contra a VRAM livre lida agora, e o veredito vira
/// `n_gpu_layers` na carga. Pedir a janela inteira do modelo numa placa que não
/// segura o KV não é falha — é o modelo morando na RAM, mais devagar.
pub const CONTEXTO_MIN: u32 = 2048;
pub const CONTEXTO_MAX: u32 = 262_144;
pub const RODADAS_MIN: u32 = 1;
pub const RODADAS_MAX: u32 = 8;

/// O que a tela de geração edita. Tudo aqui é consumido: uma escala sem efeito
/// no núcleo seria um controle mentiroso.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Ajustes {
    /// Id do modelo que o dono fixou. Vazio é o roteador quem escolhe a cada
    /// pedido; um id aqui vale contra o roteador.
    pub modelo: String,
    /// 0 é ganoso: o motor amostra o logit máximo, sem sorteio.
    pub temperatura: f32,
    /// 0 desliga o top-k; o corte então é só o top-p.
    pub top_k: u32,
    /// 1 desliga o top-p.
    pub top_p: f32,
    /// 1 desliga a penalidade de repetição.
    pub penalidade: f32,
    /// Quantos tokens atrás a penalidade de repetição olha.
    pub janela_repeticao: u32,
    pub max_tokens: u32,
    /// 0 sorteia a semente a cada turno; qualquer outro valor é a mesma
    /// amostragem reproduzível.
    pub semente: u64,
    /// Tokens de KV cache pedidos ao carregar o modelo.
    pub contexto: u32,
    /// O bloco de raciocínio do modelo. Ligado, ele aparece na conversa e custa
    /// tokens antes da primeira palavra da resposta.
    pub pensamento: bool,
    /// Idas e voltas com ferramentas num mesmo pedido.
    pub rodadas: u32,
}

impl Default for Ajustes {
    fn default() -> Self {
        Self {
            // Vazio: enquanto ninguém escolher, quem decide é o roteador.
            modelo: String::new(),
            // Baixa: o assistente responde fatos e executa ferramentas, não
            // escreve ficção.
            temperatura: 0.2,
            top_k: 40,
            top_p: 0.9,
            penalidade: 1.1,
            janela_repeticao: 128,
            max_tokens: 512,
            // 0: sem escolha do usuário, cada turno é um sorteio novo.
            semente: 0,
            contexto: 8192,
            // Medido no Granite 4.2 3B: com o bloco aberto o modelo entra em
            // laço de repetição mesmo num prompt curto que responde certo sem
            // ele. Por isso o padrão é desligado, não escondido.
            pensamento: false,
            rodadas: 4,
        }
    }
}

impl Ajustes {
    /// Lê o arquivo. Ausente ou ilegível é o padrão, não um erro: a primeira
    /// vez que alguém abre a tela de geração ainda não escolheu nada.
    pub fn de(caminho: &Path) -> Self {
        std::fs::read_to_string(caminho)
            .ok()
            .and_then(|texto| serde_json::from_str(&texto).ok())
            .map(|a: Self| a.aprovar())
            .unwrap_or_default()
    }

    /// Escrita atômica: ou o JSON inteiro está no disco, ou fica o anterior.
    pub fn salvar(&self, caminho: &Path) -> anyhow::Result<()> {
        use anyhow::Context;
        if let Some(pai) = caminho.parent() {
            std::fs::create_dir_all(pai).with_context(|| {
                tr("erros.falhaCriarPasta", &[("caminho", &pai.display().to_string())])
            })?;
        }
        let texto = serde_json::to_string_pretty(self)
            .context(tr("erros.falhaSalvarConfig", &[]))?;
        let temporario = caminho.with_extension("json.tmp");
        std::fs::write(&temporario, &texto).with_context(|| {
            tr("erros.falhaEscreverConfig", &[("caminho", &temporario.display().to_string())])
        })?;
        std::fs::rename(&temporario, caminho).with_context(|| {
            tr("erros.falhaSubstituirConfig", &[("caminho", &caminho.display().to_string())])
        })?;
        Ok(())
    }

    /// Traz cada escala para a faixa que o motor aceita. A UI manda o que o
    /// cursor marcou; o que vale é o que passou por aqui.
    pub fn aprovar(self) -> Self {
        Self {
            modelo: self.modelo.trim().to_string(),
            temperatura: self.temperatura.clamp(TEMPERATURA_MIN, TEMPERATURA_MAX),
            top_k: self.top_k.min(TOP_K_MAX),
            top_p: self.top_p.clamp(0.1, 1.0),
            penalidade: self.penalidade.clamp(1.0, 2.0),
            janela_repeticao: self.janela_repeticao.clamp(0, 4096),
            max_tokens: self.max_tokens.clamp(MAX_TOKENS_MIN, MAX_TOKENS_MAX),
            semente: self.semente,
            contexto: self.contexto.clamp(CONTEXTO_MIN, CONTEXTO_MAX),
            pensamento: self.pensamento,
            rodadas: self.rodadas.clamp(RODADAS_MIN, RODADAS_MAX),
        }
    }

    /// Os números que o motor de amostragem consome.
    pub fn amostragem(&self) -> SamplingParams {
        SamplingParams {
            temperature: self.temperatura as f64,
            top_k: (self.top_k > 0).then(|| self.top_k as usize),
            top_p: (self.top_p < 1.0).then(|| self.top_p as f64),
            repeat_penalty: self.penalidade,
            repeat_last_n: self.janela_repeticao as usize,
            seed: self.semente_real(),
            max_tokens: self.max_tokens as usize,
        }
    }

    fn semente_real(&self) -> u64 {
        if self.semente != 0 {
            return self.semente;
        }
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
    }

    /// Se a janela pedida mudou, o modelo em memória é outro modelo: os pesos
    /// continuam em disco, mas o KV cache reserva no carregamento.
    pub fn janela_mudou(&self, antes: &Ajustes) -> bool {
        self.contexto != antes.contexto
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn modelo_em_branco_e_o_roteador_quem_decide() {
        assert_eq!(Ajustes::default().modelo, "");
        // O que chega torto da tela é aparado aqui, porque é esta string que o
        // turno compara com o id do catálogo.
        let a = Ajustes { modelo: "  granite-4.2-3b  ".into(), ..Ajustes::default() }.aprovar();
        assert_eq!(a.modelo, "granite-4.2-3b");
    }

    #[test]
    fn fora_da_faixa_e_trazido_de_volta() {
        let a = Ajustes {
            temperatura: 9.0,
            top_k: 5_000,
            top_p: 0.0,
            penalidade: 0.2,
            max_tokens: 1_000_000,
            contexto: 12,
            rodadas: 0,
            ..Ajustes::default()
        }
        .aprovar();
        assert_eq!(a.temperatura, TEMPERATURA_MAX);
        assert_eq!(a.top_k, TOP_K_MAX);
        assert_eq!(a.top_p, 0.1);
        assert_eq!(a.penalidade, 1.0);
        assert_eq!(a.max_tokens, MAX_TOKENS_MAX);
        assert_eq!(a.contexto, CONTEXTO_MIN);
        assert_eq!(a.rodadas, RODADAS_MIN);
    }

    #[test]
    fn zero_desliga_o_corte_na_amostragem() {
        // O motor distingue `None` de "corte em zero posição": top-k 0 não é
        // top-k 0, é top-k desligado.
        let p = Ajustes { temperatura: 0.5, top_k: 0, top_p: 1.0, ..Ajustes::default() }
            .aprovar()
            .amostragem();
        assert_eq!(p.top_k, None);
        assert_eq!(p.top_p, None);
    }

    #[test]
    fn temperatura_zero_e_ganoso_de_verdade() {
        let p = Ajustes { temperatura: 0.0, ..Ajustes::default() }.aprovar().amostragem();
        assert_eq!(p.temperature, 0.0);
    }

    #[test]
    fn semente_zero_sorteia_e_asoutra_e_reproduzivel() {
        let a = Ajustes { semente: 0, ..Ajustes::default() }.aprovar();
        let b = Ajustes { semente: 777, ..Ajustes::default() }.aprovar();
        assert_eq!(b.amostragem().seed, 777);
        // Dois sorteios no mesmo instante podem coincidir; o que não pode é a
        // semente fixa virar sorteio.
        assert_ne!(a.amostragem().seed, 0);
    }

    #[test]
    fn janela_mudou_somente_quando_o_numero_mudou() {
        let a = Ajustes::default();
        let b = Ajustes { max_tokens: a.max_tokens + 64, ..Ajustes::default() };
        assert!(!b.janela_mudou(&a));
        let c = Ajustes { contexto: a.contexto + 1024, ..Ajustes::default() };
        assert!(c.janela_mudou(&a));
    }

    #[test]
    fn arquivo_de_uma_escrita_só_vira_ajuste() {
        let pasta = std::env::temp_dir().join(format!("aether-geracao-{}", std::process::id()));
        std::fs::create_dir_all(&pasta).unwrap();
        let arquivo = pasta.join("geracao.json");
        assert_eq!(Ajustes::de(&arquivo), Ajustes::default());
        let a = Ajustes { temperatura: 0.9, pensamento: true, ..Ajustes::default() };
        a.salvar(&arquivo).unwrap();
        let lido = Ajustes::de(&arquivo);
        assert_eq!(lido.temperatura, 0.9);
        assert!(lido.pensamento);
        // Um arquivo corrompido não derruba o aplicativo: volta o padrão.
        std::fs::write(&arquivo, "{ isto não é json").unwrap();
        assert_eq!(Ajustes::de(&arquivo), Ajustes::default());
        std::fs::remove_dir_all(&pasta).ok();
    }
}
