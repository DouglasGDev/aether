use serde::Serialize;
use std::collections::HashMap;
use std::sync::Mutex;
use tokio::sync::oneshot;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRequest {
    pub id: String,
    pub connector_id: String,
    pub tool: String,
    pub arguments: serde_json::Value,
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
    waiters: Mutex<HashMap<String, (String, oneshot::Sender<bool>)>>,
}

impl ApprovalBroker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, request: &ApprovalRequest) -> oneshot::Receiver<bool> {
        let (tx, rx) = oneshot::channel();
        self.waiters.lock().expect("approval broker poisoned").insert(
            request.id.clone(),
            (request.connector_id.clone(), tx),
        );
        rx
    }

    pub fn resolve(&self, id: &str, approved: bool) -> bool {
        let entry = self
            .waiters
            .lock()
            .expect("approval broker poisoned")
            .remove(id);
        match entry {
            Some((_, tx)) => tx.send(approved).is_ok(),
            None => false,
        }
    }

    // Dropping the sender resolves the waiter as Cancelled, so a stopping
    // server can never leave a tool call resumed against a dead process.
    pub fn cancel_connector(&self, connector_id: &str) -> usize {
        let mut waiters = self.waiters.lock().expect("approval broker poisoned");
        let stale: Vec<String> = waiters
            .iter()
            .filter(|(_, (owner, _))| owner == connector_id)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &stale {
            waiters.remove(id);
        }
        stale.len()
    }

    pub fn pending_ids(&self) -> Vec<String> {
        self.waiters
            .lock()
            .expect("approval broker poisoned")
            .keys()
            .cloned()
            .collect()
    }
}

pub async fn wait_for_decision(receiver: oneshot::Receiver<bool>) -> ApprovalOutcome {
    match receiver.await {
        Ok(true) => ApprovalOutcome::Approved,
        Ok(false) => ApprovalOutcome::Denied,
        Err(_) => ApprovalOutcome::Cancelled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request(id: &str, connector_id: &str) -> ApprovalRequest {
        ApprovalRequest {
            id: id.into(),
            connector_id: connector_id.into(),
            tool: "run".into(),
            arguments: json!({}),
        }
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