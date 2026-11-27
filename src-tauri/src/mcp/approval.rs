use crate::state::trava_estado;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Mutex;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRequest {
    pub id: String,
    pub connector_id: String,
    pub tool: String,
    pub arguments: serde_json::Value,
    /// `None` é chamada manual/externa; só chamadas de um turno carregam `Some`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalOutcome {
    Approved,
    Denied,
    Cancelled,
}

impl ApprovalOutcome {
    pub fn is_approved(self) -> bool {
        self == Self::Approved
    }
}

#[derive(Default)]
pub struct ApprovalBroker {
    waiters: Mutex<HashMap<String, (String, Option<u64>, oneshot::Sender<bool>)>>,
}

impl ApprovalBroker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, request: &ApprovalRequest) -> oneshot::Receiver<bool> {
        let (tx, rx) = oneshot::channel();
        trava_estado(&self.waiters).insert(
            request.id.clone(),
            (request.connector_id.clone(), request.turn_id, tx),
        );
        rx
    }

    pub fn resolve(&self, id: &str, approved: bool) -> bool {
        let entry = trava_estado(&self.waiters).remove(id);
        match entry {
            Some((_, _, tx)) => tx.send(approved).is_ok(),
            None => false,
        }
    }

    /// Remove o waiter antes de devolver. Um `resolve` posterior não encontra o
    /// sender e, portanto, não pode retomar uma chamada cancelada.
    pub fn cancel(&self, id: &str) -> bool {
        trava_estado(&self.waiters).remove(id).is_some()
    }

    // Dropping the sender resolves the waiter as Cancelled, so a stopping
    // server can never leave a tool call resumed against a dead process.
    pub fn cancel_connector(&self, connector_id: &str) -> usize {
        let mut waiters = trava_estado(&self.waiters);
        let stale: Vec<String> = waiters
            .iter()
            .filter(|(_, (owner, _, _))| owner == connector_id)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &stale {
            waiters.remove(id);
        }
        stale.len()
    }

    pub fn cancel_turn(&self, turn_id: u64) -> usize {
        self.cancel_turn_com(turn_id, || {})
    }

    /// Executa `cancelar` com o mapa dos waiters travado. Assim, acordar o token
    /// e remover os cards são um só passo para o waiter: ele não consegue roubar
    /// a própria entrada antes de `cancel_turn` contá-la.
    pub fn cancel_turn_com(&self, turn_id: u64, cancelar: impl FnOnce()) -> usize {
        let mut waiters = trava_estado(&self.waiters);
        cancelar();
        let stale: Vec<String> = waiters
            .iter()
            .filter(|(_, (_, turn, _))| *turn == Some(turn_id))
            .map(|(id, _)| id.clone())
            .collect();
        for id in &stale {
            waiters.remove(id);
        }
        stale.len()
    }

    pub fn pending_ids(&self) -> Vec<String> {
        trava_estado(&self.waiters).keys().cloned().collect()
    }
}

pub async fn wait_for_decision(receiver: oneshot::Receiver<bool>) -> ApprovalOutcome {
    match receiver.await {
        Ok(true) => ApprovalOutcome::Approved,
        Ok(false) => ApprovalOutcome::Denied,
        Err(_) => ApprovalOutcome::Cancelled,
    }
}

/// O token tem prioridade mesmo se a decisão chegar no mesmo instante. A
/// conferência depois do `select!` fecha a corrida em que `resolve` e `cancel`
/// ficam prontos juntos.
pub async fn wait_for_decision_cancelavel(
    receiver: oneshot::Receiver<bool>,
    cancel: &CancellationToken,
) -> ApprovalOutcome {
    if cancel.is_cancelled() {
        return ApprovalOutcome::Cancelled;
    }
    let outcome = tokio::select! {
        biased;
        _ = cancel.cancelled() => ApprovalOutcome::Cancelled,
        decision = async { receiver.await } => match decision {
            Ok(true) => ApprovalOutcome::Approved,
            Ok(false) => ApprovalOutcome::Denied,
            Err(_) => ApprovalOutcome::Cancelled,
        },
    };
    if cancel.is_cancelled() {
        ApprovalOutcome::Cancelled
    } else {
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request(id: &str, connector_id: &str) -> ApprovalRequest {
        request_turn(id, connector_id, None)
    }

    fn request_turn(id: &str, connector_id: &str, turn_id: Option<u64>) -> ApprovalRequest {
        ApprovalRequest {
            id: id.into(),
            connector_id: connector_id.into(),
            tool: "run".into(),
            arguments: json!({}),
            turn_id,
        }
    }

    #[test]
    fn o_turno_chega_ao_cartao_como_turn_id() {
        let pedido = request_turn("a", "browser", Some(42));
        let json = serde_json::to_value(&pedido).expect("aprovação serializa");
        assert_eq!(json["turnId"], serde_json::json!(42));
        assert_eq!(json["connectorId"], serde_json::json!("browser"));

        let manual = serde_json::to_value(&request("b", "browser")).expect("manual serializa");
        assert!(
            manual.get("turnId").is_none(),
            "manual não inventa turno: {manual}"
        );
    }

    #[tokio::test]
    async fn cancelar_um_turno_nega_so_as_aprovacoes_dele() {
        let broker = ApprovalBroker::new();
        let primeira = broker.register(&request_turn("a", "browser", Some(7)));
        let segunda = broker.register(&request_turn("b", "browser", Some(7)));
        let outro_turno = broker.register(&request_turn("c", "browser", Some(8)));
        let manual = broker.register(&request("d", "browser"));

        assert_eq!(broker.cancel_turn(7), 2);
        assert_eq!(
            broker.cancel_turn(7),
            0,
            "cancelar duas vezes não conta o mesmo card"
        );
        assert_eq!(
            wait_for_decision(primeira).await,
            ApprovalOutcome::Cancelled
        );
        assert_eq!(wait_for_decision(segunda).await, ApprovalOutcome::Cancelled);
        assert!(
            !broker.resolve("a", true),
            "resolve tardio não reabre card cancelado"
        );
        assert!(
            !broker.resolve("b", true),
            "resolve tardio não reabre card cancelado"
        );

        assert!(broker.resolve("c", true));
        assert_eq!(
            wait_for_decision(outro_turno).await,
            ApprovalOutcome::Approved
        );
        assert!(broker.resolve("d", true));
        assert_eq!(wait_for_decision(manual).await, ApprovalOutcome::Approved);
    }

    #[tokio::test]
    async fn token_ja_cancelado_nao_espera_decisao() {
        use tokio_util::sync::CancellationToken;

        let (tx, rx) = oneshot::channel();
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert_eq!(
            wait_for_decision_cancelavel(rx, &cancel).await,
            ApprovalOutcome::Cancelled
        );
        assert!(
            tx.send(true).is_err(),
            "decisão posterior não tem destinatário"
        );
    }

    #[tokio::test]
    async fn cancelamento_vence_uma_aprovacao_simultanea() {
        use tokio_util::sync::CancellationToken;

        let (tx, rx) = oneshot::channel();
        let cancel = CancellationToken::new();
        tx.send(true).expect("canal aberto");
        cancel.cancel();
        assert_eq!(
            wait_for_decision_cancelavel(rx, &cancel).await,
            ApprovalOutcome::Cancelled,
            "aprovar não pode vencer um token já cancelado"
        );
    }

    #[tokio::test]
    async fn approval_resumes_the_waiter() {
        let broker = ApprovalBroker::new();
        let rx = broker.register(&request("a", "filesystem"));
        assert!(broker.resolve("a", true));
        assert_eq!(wait_for_decision(rx).await, ApprovalOutcome::Approved);
    }

    #[tokio::test]
    async fn denial_resumes_the_waiter_as_denied() {
        let broker = ApprovalBroker::new();
        let rx = broker.register(&request("a", "filesystem"));
        assert!(broker.resolve("a", false));
        assert_eq!(wait_for_decision(rx).await, ApprovalOutcome::Denied);
    }

    #[tokio::test]
    async fn resolving_an_unknown_id_reports_failure() {
        let broker = ApprovalBroker::new();
        assert!(!broker.resolve("missing", true));
    }

    #[tokio::test]
    async fn double_resolve_does_not_resume_twice() {
        let broker = ApprovalBroker::new();
        let rx = broker.register(&request("a", "filesystem"));
        assert!(broker.resolve("a", true));
        assert!(!broker.resolve("a", false));
        assert_eq!(wait_for_decision(rx).await, ApprovalOutcome::Approved);
    }

    #[tokio::test]
    async fn cancelling_a_connector_denies_only_its_own_waiters() {
        let broker = ApprovalBroker::new();
        let mine = broker.register(&request("a", "browser"));
        let other = broker.register(&request("b", "device"));

        assert_eq!(broker.cancel_connector("browser"), 1);
        assert_eq!(wait_for_decision(mine).await, ApprovalOutcome::Cancelled);
        assert!(!ApprovalOutcome::Cancelled.is_approved());

        assert!(broker.resolve("b", true));
        assert_eq!(wait_for_decision(other).await, ApprovalOutcome::Approved);
    }

    #[tokio::test]
    async fn pending_ids_reflect_live_waiters() {
        let broker = ApprovalBroker::new();
        let _rx = broker.register(&request("a", "browser"));
        assert_eq!(broker.pending_ids(), vec!["a".to_string()]);
        broker.resolve("a", true);
        assert!(broker.pending_ids().is_empty());
    }
}
