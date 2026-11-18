//! Onde um efeito acontece e onde se decide sobre ele é o mesmo lugar.
//!
//! Uma ferramenta local pode ser pedida por duas portas: o laço de conversa do
//! aplicativo (`computador__ler_arquivo`) e o servidor MCP que o próprio
//! aplicativo publica (`aether__ler_arquivo`). As duas chegam aqui, e é aqui que
//! o gate decide, que a aprovação espera e que a ferramenta roda. Uma segunda
//! regra de permissão em cada porta seria duas respostas para o mesmo ato.

use super::approval::{ApprovalBroker, ApprovalOutcome, ApprovalRequest, wait_for_decision};
use super::manager::McpEvent;
use crate::ai::memoria::Custodio;
use crate::bancos;
use crate::idiomas::tr;
use crate::local::{self, pastas::Pastas};
use crate::permissions::{Decision, PendingApproval, PermissionGate, ToolEffect};
use anyhow::{Result, anyhow, bail};
use rmcp::model::{CallToolResult, ContentBlock};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;
use tokio::sync::broadcast;

pub struct Execucao {
    gate: Arc<Mutex<PermissionGate>>,
    approvals: Arc<ApprovalBroker>,
    events: broadcast::Sender<McpEvent>,
    next_id: AtomicU64,
    pastas: Arc<RwLock<Pastas>>,
    /// A memória do assistente. Compartilhada pelo mesmo motivo das pastas:
    /// `buscar_memoria` é uma ferramenta local, e uma ferramenta local roda numa
    /// tarefa bloqueante que não pode prender o runtime.
    memoria: Arc<Custodio>,
    /// Onde está o cadastro de bancos de dados. A política por tabela é lida do
    /// disco na hora da chamada, dentro da tarefa bloqueante: o que a tela salvou
    /// agora vale na chamada seguinte, sem cache a invalidar.
    bancos_path: Arc<PathBuf>,
}

impl Execucao {
    pub fn nova(
        gate: Arc<Mutex<PermissionGate>>,
        events: broadcast::Sender<McpEvent>,
        pastas: Arc<RwLock<Pastas>>,
        memoria: Arc<Custodio>,
        bancos_path: Arc<PathBuf>,
    ) -> Arc<Self> {
        Self::nova_com_aprovacoes(
            gate,
            Arc::new(ApprovalBroker::new()),
            events,
            pastas,
            memoria,
            bancos_path,
        )
    }

    pub fn nova_com_aprovacoes(
        gate: Arc<Mutex<PermissionGate>>,
        approvals: Arc<ApprovalBroker>,
        events: broadcast::Sender<McpEvent>,
        pastas: Arc<RwLock<Pastas>>,
        memoria: Arc<Custodio>,
        bancos_path: Arc<PathBuf>,
    ) -> Arc<Self> {
        Self {
            gate,
            approvals,
            events,
            next_id: AtomicU64::new(1),
            pastas,
            memoria,
            bancos_path,
        }
        .into()
    }

    pub fn aprovacoes(&self) -> Arc<ApprovalBroker> {
        Arc::clone(&self.approvals)
    }

    pub fn gate(&self) -> Arc<Mutex<PermissionGate>> {
        Arc::clone(&self.gate)
    }

    /// As pastas concedidas, como um servidor de linha de comando as recebe: a
    /// lista vai na linha de comando do processo filho, então mudar a concessão
    /// obriga a relançar quem depende dela.
    pub fn pastas_concedidas(&self) -> Vec<String> {
        self.pastas
            .read()
            .expect("pastas envenenadas")
            .raizes
            .iter()
            .map(|r| r.display().to_string())
            .collect()
    }

    /// Decide sobre um efeito e, quando o modo pede, espera a aprovação. É o
    /// pedaço da decisão que um servidor remoto também precisa: a ferramenta roda
    /// do outro lado, mas é aqui que se diz se ela pode rodar.
    ///
    /// `connector_id` é o dono do pedido: é por ele que a aprovação pergunta e é
    /// por ele que uma concessão de sessão vale. `efeito` vem da tabela do
    /// próprio aplicativo, não de uma anotação de servidor de terceiros.
    pub async fn aprovar(
        &self,
        connector_id: &str,
        ferramenta: &str,
        efeito: ToolEffect,
        argumentos: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<()> {
        let compound = argumentos_contem_composto(argumentos);
        let decision = self
            .gate
            .lock()
            .expect("permission gate poisoned")
            .decide(connector_id, efeito, compound);

        match decision {
            Decision::Deny => {
                bail!(tr("erros.negadaPeloModo", &[("nome", ferramenta)]))
            }
            Decision::Allow => {}
            Decision::Ask => {
                let request = ApprovalRequest {
                    id: format!("apr-{}", self.next_id.fetch_add(1, Ordering::Relaxed)),
                    connector_id: connector_id.to_string(),
                    tool: ferramenta.to_string(),
                    arguments: serde_json::Value::Object(argumentos.clone()),
                };
                let receiver = self.approvals.register(&request);
                self.gate
                    .lock()
                    .expect("permission gate poisoned")
                    .insert_pending(
                        request.id.clone(),
                        PendingApproval {
                            connector_id: request.connector_id.clone(),
                            tool: request.tool.clone(),
                            effect: efeito,
                            compound,
                            created_at: Instant::now(),
                        },
                    );
                let _ = self
                    .events
                    .send(McpEvent::ApprovalRequested {
                        request: request.clone(),
                    });

                // A chamada fica suspensa aqui, não é executada para depois
                // voltar atrás.
                let outcome = wait_for_decision(receiver).await;

                self.gate
                    .lock()
                    .expect("permission gate poisoned")
                    .resolve_pending(&request.id);
                let _ = self.events.send(McpEvent::ApprovalSettled {
                    id: request.id.clone(),
                    approved: outcome.is_approved(),
                });
                match outcome {
                    ApprovalOutcome::Approved => {}
                    ApprovalOutcome::Denied => bail!(tr("erros.acaoNegada", &[])),
                    ApprovalOutcome::Cancelled => {
                        bail!(tr("erros.servidorCaiuNaAprovacao", &[]))
                    }
                }
            }
        }
        Ok(())
    }

    /// Roda uma ferramenta local depois de o gate decidir sobre ela. Ferramentas
    /// de banco seguem por aqui também, mas com o cadastro do dono no lugar das
    /// pastas: a política por tabela é conferida dentro, antes de o motor abrir.
    pub async fn ferramenta(
        &self,
        connector_id: &str,
        ferramenta: &str,
        efeito: ToolEffect,
        argumentos: serde_json::Value,
    ) -> Result<local::Saida> {
        let args = match argumentos {
            serde_json::Value::Object(map) => map,
            _ => bail!(tr("erros.argumentosInvalidos", &[])),
        };
        self.aprovar(connector_id, ferramenta, efeito, &args)
            .await?;

        if bancos::ferramentas::e_ferramenta(ferramenta) {
            let caminho = Arc::clone(&self.bancos_path);
            let nome = ferramenta.to_string();
            return tokio::task::spawn_blocking(move || {
                bancos::ferramentas::executar(&nome, serde_json::Value::Object(args), &caminho)
            })
            .await
            .map_err(|e| anyhow!(tr("erros.falhaExecutar", &[("detalhe", &e.to_string())])))?;
        }

        // Uma cópia das pastas, nunca o lock atravessando o await: a concessão de
        // uma pasta não pode ficar presa atrás de um comando.
        let pastas = self.pastas.read().expect("pastas envenenadas").clone();
        let memoria = Arc::clone(&self.memoria);
        let nome = ferramenta.to_string();
        tokio::task::spawn_blocking(move || {
            local::executar(&nome, serde_json::Value::Object(args), &pastas, &memoria)
        })
        .await
        .map_err(|e| anyhow!(tr("erros.falhaExecutar", &[("detalhe", &e.to_string())])))?
    }
}

/// O cartão e o texto do modelo saem do mesmo resultado: uma ferramenta local
/// responde para o laço de conversa e para um cliente MCP com o que ela fez.
pub fn resultado_local(saida: local::Saida) -> CallToolResult {
    let mut resultado = CallToolResult::success(vec![ContentBlock::text(saida.texto)]);
    resultado.structured_content = Some(saida.vista.para_json());
    resultado.is_error = Some(saida.erro);
    resultado
}

// Qualquer argumento textual pode chegar a um shell, então um pipe ou uma
// substituição revoga o benefício de uma concessão de sessão.
pub(crate) fn argumentos_contem_composto(
    argumentos: &serde_json::Map<String, serde_json::Value>,
) -> bool {
    fn scan(value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::String(texto) => {
                texto.contains('|')
                    || texto.contains(';')
                    || texto.contains("&&")
                    || texto.contains("$(")
                    || texto.contains('`')
                    || texto.contains('>')
            }
            serde_json::Value::Array(itens) => itens.iter().any(scan),
            serde_json::Value::Object(map) => map.values().any(scan),
            _ => false,
        }
    }
    argumentos.values().any(scan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn mapa(valor: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        match valor {
            serde_json::Value::Object(mapa) => mapa,
            _ => panic!("a fixture precisa ser um objeto"),
        }
    }

    #[test]
    fn argumentos_simples_nao_sao_compostos() {
        assert!(!argumentos_contem_composto(&mapa(
            json!({ "caminho": "/tmp/arquivo.txt" })
        )));
    }

    #[test]
    fn pipe_torna_os_argumentos_compostos() {
        assert!(argumentos_contem_composto(&mapa(
            json!({ "comando": "cat a | grep b" })
        )));
    }

    #[test]
    fn substituicao_torna_os_argumentos_compostos() {
        assert!(argumentos_contem_composto(&mapa(
            json!({ "comando": "echo $(whoami)" })
        )));
        assert!(argumentos_contem_composto(&mapa(
            json!({ "comando": "echo `whoami`" })
        )));
    }

    #[test]
    fn a_deteccao_de_composto_entra_em_listas_e_objetos() {
        assert!(argumentos_contem_composto(&mapa(
            json!({ "passos": [{ "run": "a && b" }] })
        )));
        assert!(!argumentos_contem_composto(&mapa(
            json!({ "passos": [{ "run": "a" }], "contagem": 3, "ligado": true })
        )));
    }

    #[test]
    fn argumentos_vazios_nao_sao_compostos() {
        assert!(!argumentos_contem_composto(&mapa(json!({}))));
    }

    /// A recusa do modo não depende do que a ferramenta escreve no disco: um
    /// `Plan` nega a escrita antes de ela chegar ao arquivo. A leitura ele deixa,
    /// e é por isso que o caso testado é o de escrever.
    #[tokio::test]
    async fn o_gate_decide_antes_de_a_ferramenta_rodar() {
        let (eventos, _) = broadcast::channel(4);
        let raiz = std::env::temp_dir().join(format!("aether-execucao-{}", std::process::id()));
        std::fs::create_dir_all(&raiz).expect("pasta de teste");
        let pastas = Arc::new(RwLock::new(Pastas {
            raizes: vec![raiz.clone()],
        }));
        let execucao = Execucao::nova(
            Arc::new(Mutex::new(PermissionGate::new(crate::permissions::AgentMode::Plan))),
            eventos,
            Arc::clone(&pastas),
            Arc::new(Custodio::novo(
                &[],
                &raiz.join("modelos"),
                &raiz.join("memoria"),
                raiz.join("rag.json"),
                pastas,
            )),
            Arc::new(raiz.join("bancos.json")),
        );
        let alvo = raiz.join("proibido.txt");
        let erro = execucao
            .ferramenta(
                "aether",
                "escrever_arquivo",
                ToolEffect::Mutating,
                json!({ "caminho": alvo.to_string_lossy(), "conteudo": "x" }),
            )
            .await
            .expect_err("Plan não escreve");
        assert_eq!(
            erro.to_string(),
            tr("erros.negadaPeloModo", &[("nome", "escrever_arquivo")])
        );
        assert!(!alvo.exists(), "negada é não escrita");
    }
}
