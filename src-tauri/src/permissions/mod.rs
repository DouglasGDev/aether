use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

pub const DEFAULT_APPROVAL_TIMEOUT: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentMode {
    Plan,
    Default,
    AutoAccept,
    Auto,
    FullAccess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolEffect {
    ReadOnly,
    Mutating,
    Destructive,
    /// Tira bytes da máquina. Não é um degrau acima de `Destructive`: é outro
    /// eixo, e por isso não se graduou junto com os três de sempre. O
    /// `Destructive` mexe no que está aqui, e o que estava aqui continua sendo
    /// da pessoa que apaga; o que sai pela rede já está do outro lado no momento
    /// em que a pessoa veria o cartão, e não há volta para o texto enviado.
    /// Nenhum modo autoriza este efeito em silêncio — ver `evaluate`.
    Rede,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Allow,
    Ask,
    Deny,
}

pub fn evaluate(
    mode: AgentMode,
    effect: ToolEffect,
    compound: bool,
    session_granted: bool,
) -> Decision {
    match mode {
        AgentMode::Plan => {
            if effect == ToolEffect::ReadOnly {
                Decision::Allow
            } else {
                Decision::Deny
            }
        }
        // Os dois modos que liberam o resto da máquina sem perguntar param na
        // porta de saída. O que um `Auto` compra é efeito sobre o que está aqui:
        // apagar um arquivo admite arrependimento e a máquina continua sendo da
        // pessoa, enquanto o texto enviado para fora já está do outro lado antes
        // de qualquer cartão. A rede não entra nesse pacote — ela é o único
        // efeito que todo modo ainda tem de aprovar a olho.
        AgentMode::Auto | AgentMode::FullAccess => {
            if effect == ToolEffect::Rede {
                Decision::Ask
            } else {
                Decision::Allow
            }
        }
        AgentMode::AutoAccept => match effect {
            ToolEffect::ReadOnly | ToolEffect::Mutating => Decision::Allow,
            ToolEffect::Destructive | ToolEffect::Rede => Decision::Ask,
        },
        AgentMode::Default => {
            // A prefix allowlist cannot verify what a pipe or substitution does,
            // so a compound command always earns a fresh prompt.
            if compound {
                Decision::Ask
            } else if matches!(effect, ToolEffect::Destructive | ToolEffect::Rede) {
                // Apagar e sair pela rede exigem um prompt novo mesmo com a
                // confiança da sessão: o Default é mais cauteloso que o
                // AutoAccept, que já pergunta para os dois. Um grant não pode
                // tornar o modo padrão menos seguro.
                Decision::Ask
            } else if session_granted {
                Decision::Allow
            } else {
                // ToolEffect comes from the server's own annotations, which the
                // MCP SDK warns are untrusted hints. Letting a read-only claim
                // skip approval would let a server grant itself trust, so the
                // default mode asks for everything until the user grants it.
                Decision::Ask
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingApproval {
    pub connector_id: String,
    pub tool: String,
    pub effect: ToolEffect,
    pub compound: bool,
    pub created_at: Instant,
}

#[derive(Debug)]
pub struct PermissionGate {
    mode: AgentMode,
    timeout: Duration,
    session_grants: HashSet<String>,
    pending: HashMap<String, PendingApproval>,
}

impl PermissionGate {
    pub fn new(mode: AgentMode) -> Self {
        Self {
            mode,
            timeout: DEFAULT_APPROVAL_TIMEOUT,
            session_grants: HashSet::new(),
            pending: HashMap::new(),
        }
    }

    pub fn mode(&self) -> AgentMode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: AgentMode) {
        self.mode = mode;
    }

    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    pub fn grant_session(&mut self, connector_id: &str) {
        self.session_grants.insert(connector_id.to_string());
    }

    pub fn revoke_session(&mut self, connector_id: &str) {
        self.session_grants.remove(connector_id);
    }

    pub fn session_grants(&self) -> Vec<String> {
        let mut grants: Vec<String> = self.session_grants.iter().cloned().collect();
        grants.sort();
        grants
    }

    pub fn decide(
        &self,
        connector_id: &str,
        effect: ToolEffect,
        compound: bool,
    ) -> Decision {
        evaluate(
            self.mode,
            effect,
            compound,
            self.session_grants.contains(connector_id),
        )
    }

    pub fn insert_pending(&mut self, id: String, approval: PendingApproval) {
        self.pending.insert(id, approval);
    }

    pub fn pending(&self) -> impl Iterator<Item = (&String, &PendingApproval)> {
        self.pending.iter()
    }

    pub fn resolve_pending(&mut self, id: &str) -> Option<PendingApproval> {
        self.pending.remove(id)
    }

    /// Um conector que para não pode deixar aprovações apontando para um
    /// processo morto.
    ///
    /// A confiança cai antes das aprovações, e a ordem é o que permite recuperar a
    /// trava: os dois conjuntos são mutados um depois do outro, então um panic no
    /// meio deixa um estado que não é o de antes nem o de depois. Escolhido o
    /// meio que sobra, sobra o restrito — sem a confiança, uma aprovação órfã não
    /// autoriza chamada nenhuma, e ela morre sozinha no `expire_pending`. Pelo
    /// caminho contrário a aprovação apagada deixaria o conector reconectado
    /// decidido como `Allow` sem card, e o motor nem saberia perguntar.
    pub fn flush_connector(&mut self, connector_id: &str) -> Vec<String> {
        self.session_grants.remove(connector_id);
        let stale: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, a)| a.connector_id == connector_id)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &stale {
            self.pending.remove(id);
        }
        stale
    }

    pub fn expire_pending(&mut self, now: Instant) -> Vec<String> {
        if self.timeout.is_zero() {
            return Vec::new();
        }
        let expired: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, a)| now.duration_since(a.created_at) >= self.timeout)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &expired {
            self.pending.remove(id);
        }
        expired
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approval(connector_id: &str) -> PendingApproval {
        PendingApproval {
            connector_id: connector_id.to_string(),
            tool: "run".into(),
            effect: ToolEffect::Mutating,
            compound: false,
            created_at: Instant::now(),
        }
    }

    #[test]
    fn plan_mode_is_read_only() {
        assert_eq!(
            evaluate(AgentMode::Plan, ToolEffect::ReadOnly, false, true),
            Decision::Allow
        );
        assert_eq!(
            evaluate(AgentMode::Plan, ToolEffect::Mutating, false, true),
            Decision::Deny
        );
        assert_eq!(
            evaluate(AgentMode::Plan, ToolEffect::Destructive, false, true),
            Decision::Deny
        );
        // O plano não executa nada que altere a máquina, e a rede altera mais do
        // que a máquina: manda fora o que aqui está.
        assert_eq!(
            evaluate(AgentMode::Plan, ToolEffect::Rede, false, true),
            Decision::Deny
        );
    }

    #[test]
    fn auto_and_full_access_bypass_prompts() {
        for mode in [AgentMode::Auto, AgentMode::FullAccess] {
            assert_eq!(
                evaluate(mode, ToolEffect::Destructive, true, false),
                Decision::Allow
            );
            // A exceção é uma só, e ela é o motivo deste efeito existir: o modo
            // que libera o disco inteiro ainda pergunta antes de mandar um byte
            // para fora da máquina.
            assert_eq!(
                evaluate(mode, ToolEffect::Rede, false, false),
                Decision::Ask,
                "{mode:?} liberou a rede sem perguntar"
            );
        }
    }

    /// A régua inteira, num só lugar: todo modo, comando composto ou não, com ou
    /// sem a confiança da sessão. Nenhum dos vinte cruzamentos devolve `Allow`
    /// para a rede — e é isto que impede um modo novo de herdar egresso de
    /// silêncio, porque a lista de modos é o que o teste percorre.
    #[test]
    fn nenhum_modo_aprova_a_rede() {
        for mode in [
            AgentMode::Plan,
            AgentMode::Default,
            AgentMode::AutoAccept,
            AgentMode::Auto,
            AgentMode::FullAccess,
        ] {
            for compound in [false, true] {
                for granted in [false, true] {
                    assert_ne!(
                        evaluate(mode, ToolEffect::Rede, compound, granted),
                        Decision::Allow,
                        "{mode:?} composto={compound} concedido={granted} aprovou a rede"
                    );
                }
            }
        }
        // O plano recusa (não executa nada que não leia); os outros quatro
        // perguntam — e perguntar é a decisão explícita que o efeito exige.
        assert_eq!(evaluate(AgentMode::Plan, ToolEffect::Rede, false, false), Decision::Deny);
        for mode in [
            AgentMode::Default,
            AgentMode::AutoAccept,
            AgentMode::Auto,
            AgentMode::FullAccess,
        ] {
            assert_eq!(evaluate(mode, ToolEffect::Rede, false, false), Decision::Ask);
        }
    }

    #[test]
    fn a_confianca_da_sessao_nao_compra_a_porta_de_saida() {
        // O grant de sessão é a pessoa dizendo "confio neste conector" uma vez.
        // Ele cobre o que acontece dentro da máquina; um pedido que sai dela
        // continua tendo de ser aprovado a cada vez, no modo mais cauteloso que
        // existe e no mais liberado que existe.
        for mode in [AgentMode::Default, AgentMode::FullAccess] {
            let mut gate = PermissionGate::new(mode);
            gate.grant_session("computador");
            assert_eq!(
                gate.decide("computador", ToolEffect::Rede, false),
                Decision::Ask,
                "{mode:?} deixou a rede passar pela confiança da sessão"
            );
            // A mesma confiança continua valendo para o que é daqui — sem isto o
            // teste não provaria que foi a rede que mudou a resposta.
            assert_eq!(gate.decide("computador", ToolEffect::Mutating, false), Decision::Allow);
        }
    }

    #[test]
    fn o_efeito_da_rede_serdeja_como_a_palavra_rede() {
        // O efeito viaja para a interface como texto: uma chave nova aqui é uma
        // chave que a tela e o `mcp.json` vão ler.
        assert_eq!(
            serde_json::to_value(ToolEffect::Rede).unwrap(),
            serde_json::json!("rede")
        );
    }

    #[test]
    fn auto_accept_still_asks_for_destructive() {
        assert_eq!(
            evaluate(AgentMode::AutoAccept, ToolEffect::Mutating, false, false),
            Decision::Allow
        );
        assert_eq!(
            evaluate(AgentMode::AutoAccept, ToolEffect::Destructive, false, false),
            Decision::Ask
        );
        assert_eq!(
            evaluate(AgentMode::AutoAccept, ToolEffect::Rede, false, true),
            Decision::Ask
        );
    }

    #[test]
    fn compound_command_overrides_session_grant() {
        assert_eq!(
            evaluate(AgentMode::Default, ToolEffect::Mutating, false, true),
            Decision::Allow
        );
        assert_eq!(
            evaluate(AgentMode::Default, ToolEffect::Mutating, true, true),
            Decision::Ask
        );
    }

    #[test]
    fn session_grant_does_not_skip_destructive_prompt() {
        // O Default é mais cauteloso que o AutoAccept, que já pergunta para
        // destrutivo. Um grant de sessão não pode tornar o modo padrão menos
        // seguro: apagar exige um prompt novo mesmo com a confiança da sessão.
        assert_eq!(
            evaluate(AgentMode::Default, ToolEffect::Destructive, false, true),
            Decision::Ask
        );
    }

    #[test]
    fn default_mode_asks_without_grant() {
        assert_eq!(
            evaluate(AgentMode::Default, ToolEffect::Mutating, false, false),
            Decision::Ask
        );
        assert_eq!(
            evaluate(AgentMode::Default, ToolEffect::ReadOnly, false, false),
            Decision::Ask
        );
    }

    #[test]
    fn a_server_read_only_claim_cannot_grant_itself_trust() {
        assert_eq!(
            evaluate(AgentMode::Default, ToolEffect::ReadOnly, false, false),
            Decision::Ask
        );
        assert_eq!(
            evaluate(AgentMode::Default, ToolEffect::ReadOnly, false, true),
            Decision::Allow
        );
    }

    #[test]
    fn session_grant_is_scoped_to_one_connector() {
        let mut gate = PermissionGate::new(AgentMode::Default);
        gate.grant_session("filesystem");
        assert_eq!(
            gate.decide("filesystem", ToolEffect::Mutating, false),
            Decision::Allow
        );
        assert_eq!(
            gate.decide("github", ToolEffect::Mutating, false),
            Decision::Ask
        );
        gate.revoke_session("filesystem");
        assert_eq!(
            gate.decide("filesystem", ToolEffect::Mutating, false),
            Decision::Ask
        );
    }

    #[test]
    fn flushing_a_connector_revokes_grant_and_pending() {
        let mut gate = PermissionGate::new(AgentMode::Default);
        gate.grant_session("browser");
        gate.insert_pending("a".into(), approval("browser"));
        gate.insert_pending("b".into(), approval("device"));

        let flushed = gate.flush_connector("browser");
        assert_eq!(flushed, vec!["a".to_string()]);
        assert_eq!(gate.pending().count(), 1);
        assert_eq!(
            gate.decide("browser", ToolEffect::Mutating, false),
            Decision::Ask
        );
    }

    #[test]
    fn unanswered_approval_expires() {
        let mut gate = PermissionGate::new(AgentMode::Default);
        gate.set_timeout(Duration::from_secs(900));
        gate.insert_pending("a".into(), approval("browser"));

        let now = Instant::now();
        assert!(gate.expire_pending(now).is_empty());
        assert_eq!(
            gate.expire_pending(now + Duration::from_secs(901)),
            vec!["a".to_string()]
        );
        assert_eq!(gate.pending().count(), 0);
    }

    #[test]
    fn zero_timeout_never_expires() {
        let mut gate = PermissionGate::new(AgentMode::Default);
        gate.set_timeout(Duration::ZERO);
        gate.insert_pending("a".into(), approval("browser"));
        let expired = gate.expire_pending(Instant::now() + Duration::from_secs(86_400));
        assert!(expired.is_empty());
        assert_eq!(gate.pending().count(), 1);
    }

    #[test]
    fn resolving_pending_removes_it() {
        let mut gate = PermissionGate::new(AgentMode::Default);
        gate.insert_pending("a".into(), approval("browser"));
        assert!(gate.resolve_pending("a").is_some());
        assert!(gate.resolve_pending("a").is_none());
    }
}