//! Onde um efeito acontece e onde se decide sobre ele é o mesmo lugar.
//!
//! Uma ferramenta local pode ser pedida por duas portas: o laço de conversa do
//! aplicativo (`computador__ler_arquivo`) e o servidor MCP que o próprio
//! aplicativo publica (`aether__ler_arquivo`). As duas chegam aqui, e é aqui que
//! o gate decide, que a aprovação espera e que a ferramenta roda. Uma segunda
//! regra de permissão em cada porta seria duas respostas para o mesmo ato.

use super::approval::{
    ApprovalBroker, ApprovalOutcome, ApprovalRequest, wait_for_decision_cancelavel,
};
use super::manager::McpEvent;
use crate::ai::memoria::Custodio;
use crate::bancos;
use crate::connectors::Registry;
use crate::idiomas::tr;
use crate::local::{self, pastas::Pastas};
use crate::permissions::{Decision, PendingApproval, PermissionGate, ToolEffect};
use crate::state::{trava_estado, trava_leitura};
use anyhow::{Result, anyhow, bail};
use rmcp::model::{CallToolResult, ContentBlock};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

pub struct Execucao {
    gate: Arc<Mutex<PermissionGate>>,
    /// O cadastro com as allowlists do grafo, dividido com o gerente. O modo diz
    /// *como* uma ferramenta roda; isto diz *se ela existe*, e as duas portas
    /// precisam ler a mesma resposta.
    registry: Arc<RwLock<Registry>>,
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
    /// A pasta do depósito de resultados longos. É do turno que ela veio, mas é
    /// daqui que `ler_resultado` a recebe: o corpo que o modelo lê foi guardado
    /// pelo laço de conversa, e ler por fora do laço é ler a mesma pasta.
    derrame: Arc<PathBuf>,
}

impl Execucao {
    pub fn nova(
        gate: Arc<Mutex<PermissionGate>>,
        registry: Arc<RwLock<Registry>>,
        events: broadcast::Sender<McpEvent>,
        pastas: Arc<RwLock<Pastas>>,
        memoria: Arc<Custodio>,
        bancos_path: Arc<PathBuf>,
        derrame: Arc<PathBuf>,
    ) -> Arc<Self> {
        Self::nova_com_aprovacoes(
            gate,
            registry,
            Arc::new(ApprovalBroker::new()),
            events,
            pastas,
            memoria,
            bancos_path,
            derrame,
        )
    }

    pub fn nova_com_aprovacoes(
        gate: Arc<Mutex<PermissionGate>>,
        registry: Arc<RwLock<Registry>>,
        approvals: Arc<ApprovalBroker>,
        events: broadcast::Sender<McpEvent>,
        pastas: Arc<RwLock<Pastas>>,
        memoria: Arc<Custodio>,
        bancos_path: Arc<PathBuf>,
        derrame: Arc<PathBuf>,
    ) -> Arc<Self> {
        Self {
            gate,
            registry,
            approvals,
            events,
            next_id: AtomicU64::new(1),
            pastas,
            memoria,
            bancos_path,
            derrame,
        }
        .into()
    }

    pub fn aprovacoes(&self) -> Arc<ApprovalBroker> {
        Arc::clone(&self.approvals)
    }

    pub fn gate(&self) -> Arc<Mutex<PermissionGate>> {
        Arc::clone(&self.gate)
    }

    /// Se o grafo deixou esta ferramenta ligada no conector embutido. A allowlist
    /// é uma só, guardada no cadastro do `computador`, e quem a escreve é a tela
    /// do grafo (`ligacoes_aplicar`); o servidor publicado roda as mesmas
    /// ferramentas do laço de conversa, então é a mesma lista que ele tem de
    /// obedecer.
    ///
    /// Um cadastro onde o conector embutido não está responde "nada", igual a
    /// `manager::permitidas`: sem cadastro não há decisão registrada, e uma
    /// permissão não se deduz do silêncio de quem decide.
    pub fn permite_local(&self, nome: &str) -> bool {
        trava_leitura(&self.registry)
            .get(local::CONECTOR)
            .is_some_and(|c| c.allows_tool(nome))
    }

    /// As pastas concedidas, como um servidor de linha de comando as recebe: a
    /// lista vai na linha de comando do processo filho, então mudar a concessão
    /// obriga a relançar quem depende dela.
    pub fn pastas_concedidas(&self) -> Vec<String> {
        trava_leitura(&self.pastas)
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
        self.aprovar_cancelavel(
            connector_id,
            ferramenta,
            efeito,
            argumentos,
            None,
            &CancellationToken::new(),
        )
        .await
    }

    pub async fn aprovar_cancelavel(
        &self,
        connector_id: &str,
        ferramenta: &str,
        efeito: ToolEffect,
        argumentos: &serde_json::Map<String, serde_json::Value>,
        turn_id: Option<u64>,
        cancel: &CancellationToken,
    ) -> Result<()> {
        if cancel.is_cancelled() {
            bail!(tr("erros.pedidoCancelado", &[]));
        }
        let compound = argumentos_contem_composto(argumentos);
        let decision = trava_estado(&self.gate).decide(connector_id, efeito, compound);

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
                    turn_id,
                };
                let receiver = self.approvals.register(&request);
                trava_estado(&self.gate).insert_pending(
                    request.id.clone(),
                    PendingApproval {
                        connector_id: request.connector_id.clone(),
                        tool: request.tool.clone(),
                        effect: efeito,
                        compound,
                        created_at: Instant::now(),
                        turn_id,
                    },
                );
                let _ = self.events.send(McpEvent::ApprovalRequested {
                    request: request.clone(),
                });

                // A chamada fica suspensa aqui, não é executada para depois
                // voltar atrás. O token pode vencer mesmo uma aprovação simultânea.
                let mut outcome = wait_for_decision_cancelavel(receiver, cancel).await;
                if cancel.is_cancelled() {
                    outcome = ApprovalOutcome::Cancelled;
                    self.approvals.cancel(&request.id);
                }

                trava_estado(&self.gate).resolve_pending(&request.id);
                let _ = self.events.send(McpEvent::ApprovalSettled {
                    id: request.id.clone(),
                    approved: outcome.is_approved(),
                });
                if cancel.is_cancelled() {
                    bail!(tr("erros.pedidoCancelado", &[]));
                }
                match outcome {
                    ApprovalOutcome::Approved => {}
                    ApprovalOutcome::Denied => bail!(tr("erros.acaoNegada", &[])),
                    ApprovalOutcome::Cancelled => {
                        bail!(tr("erros.servidorCaiuNaAprovacao", &[]))
                    }
                }
            }
        }
        if cancel.is_cancelled() {
            bail!(tr("erros.pedidoCancelado", &[]));
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
        self.ferramenta_cancelavel(
            connector_id,
            ferramenta,
            efeito,
            argumentos,
            None,
            &CancellationToken::new(),
        )
        .await
    }

    pub async fn ferramenta_cancelavel(
        &self,
        connector_id: &str,
        ferramenta: &str,
        efeito: ToolEffect,
        argumentos: serde_json::Value,
        turn_id: Option<u64>,
        cancel: &CancellationToken,
    ) -> Result<local::Saida> {
        if cancel.is_cancelled() {
            bail!(tr("erros.pedidoCancelado", &[]));
        }
        let args = match argumentos {
            serde_json::Value::Object(map) => map,
            _ => bail!(tr("erros.argumentosInvalidos", &[])),
        };
        self.aprovar_cancelavel(connector_id, ferramenta, efeito, &args, turn_id, cancel)
            .await?;
        if cancel.is_cancelled() {
            bail!(tr("erros.pedidoCancelado", &[]));
        }

        if bancos::ferramentas::e_ferramenta(ferramenta) {
            let caminho = Arc::clone(&self.bancos_path);
            let nome = ferramenta.to_string();
            return bloqueante_cancelavel(cancel, move || {
                bancos::ferramentas::executar(&nome, serde_json::Value::Object(args), &caminho)
            })
            .await;
        }

        // Uma cópia das pastas, nunca o lock atravessando o await: a concessão de
        // uma pasta não pode ficar presa atrás de um comando.
        let pastas = trava_leitura(&self.pastas).clone();
        let memoria = Arc::clone(&self.memoria);
        let derrame = Arc::clone(&self.derrame);
        let nome = ferramenta.to_string();
        bloqueante_cancelavel(cancel, move || {
            local::executar(
                &nome,
                serde_json::Value::Object(args),
                &pastas,
                &memoria,
                &derrame,
            )
        })
        .await
    }
}

/// `spawn_blocking` não mata `std::process::Command` quando o `JoinHandle` é
/// dropado. O token corta a espera do turno e é conferido antes de o runner
/// começar; trabalho já iniciado pode terminar até seu próprio timeout, mas seu
/// resultado não tem destinatário e a sessão suprime eventos posteriores ao
/// cancelamento.
async fn bloqueante_cancelavel<T, F>(cancel: &CancellationToken, trabalho: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    if cancel.is_cancelled() {
        bail!(tr("erros.pedidoCancelado", &[]));
    }
    let cancel_tarefa = cancel.clone();
    let tarefa = tokio::task::spawn_blocking(move || {
        if cancel_tarefa.is_cancelled() {
            return Err(anyhow!(tr("erros.pedidoCancelado", &[])));
        }
        trabalho()
    });
    let resultado = tokio::select! {
        biased;
        _ = cancel.cancelled() => bail!(tr("erros.pedidoCancelado", &[])),
        resultado = tarefa => resultado,
    };
    concluir_bloqueante(cancel, resultado)
}

/// O `select!` pode escolher um `JoinHandle` já pronto e o token ser cancelado
/// logo depois. Este ponto único reconfere o token antes de deixar qualquer
/// resultado (inclusive um erro) escapar para o turno.
fn concluir_bloqueante<T>(
    cancel: &CancellationToken,
    resultado: Result<Result<T>, tokio::task::JoinError>,
) -> Result<T> {
    if cancel.is_cancelled() {
        bail!(tr("erros.pedidoCancelado", &[]));
    }
    match resultado {
        Ok(feito) => feito,
        Err(e) => bail!(tr("erros.falhaExecutar", &[("detalhe", &e.to_string())])),
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

// Qualquer argumento textual pode chegar a um shell, então um separador ou uma
// substituição revoga o benefício de uma concessão de sessão. O `&` simples
// (que cobre o `&&`), a quebra de linha e o `<` também separam/redirecionam no
// cmd.exe, e `^`/`%` são escape e expansão dele: ficar só nos separadores do
// sh deixaria `cmd /C "echo ok & payload"` passar como comando único.
pub(crate) fn argumentos_contem_composto(
    argumentos: &serde_json::Map<String, serde_json::Value>,
) -> bool {
    fn scan(value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::String(texto) => {
                texto.contains('|')
                    || texto.contains(';')
                    || texto.contains('&')
                    || texto.contains("$(")
                    || texto.contains('`')
                    || texto.contains('>')
                    || texto.contains('<')
                    || texto.contains('\n')
                    || texto.contains('\r')
                    || texto.contains('^')
                    || texto.contains('%')
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
    use std::time::Duration;

    fn mapa(valor: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        match valor {
            serde_json::Value::Object(mapa) => mapa,
            _ => panic!("a fixture precisa ser um objeto"),
        }
    }

    /// O cadastro do conector embutido como a tela do grafo o escreve: `None` é
    /// o catálogo todo ligado, `Some` é o recorte que o usuário deixou.
    fn cadastro(allowlist: Option<Vec<&str>>) -> Arc<RwLock<Registry>> {
        let mut embutido = local::conector();
        embutido.tool_allowlist = allowlist.map(|v| v.into_iter().map(String::from).collect());
        Arc::new(RwLock::new(
            Registry::new(vec![embutido]).expect("o conector embutido é registrável"),
        ))
    }

    /// A execução de verdade — gate, pastas e memória montados como no
    /// aplicativo — sobre o cadastro que a teste quiser.
    fn execucao_com(
        modo: crate::permissions::AgentMode,
        registry: Arc<RwLock<Registry>>,
    ) -> (Arc<Execucao>, PathBuf) {
        let (execucao, raiz, _) = execucao_com_eventos(modo, registry);
        (execucao, raiz)
    }

    fn execucao_com_eventos(
        modo: crate::permissions::AgentMode,
        registry: Arc<RwLock<Registry>>,
    ) -> (Arc<Execucao>, PathBuf, broadcast::Receiver<McpEvent>) {
        let (eventos, receber) = broadcast::channel(8);
        let raiz = std::env::temp_dir().join(format!("aether-execucao-{}", std::process::id()));
        std::fs::create_dir_all(&raiz).expect("pasta de teste");
        let pastas = Arc::new(RwLock::new(Pastas {
            raizes: vec![raiz.clone()],
        }));
        let execucao = Execucao::nova(
            Arc::new(Mutex::new(PermissionGate::new(modo))),
            registry,
            eventos,
            Arc::clone(&pastas),
            Arc::new(Custodio::novo(
                &[],
                &raiz.join("modelos"),
                &raiz.join("memoria"),
                raiz.join("rag.json"),
                &raiz.join(crate::ai::base::PASTA),
                pastas,
            )),
            Arc::new(raiz.join("bancos.json")),
            Arc::new(raiz.join("resultados")),
        );
        (execucao, raiz, receber)
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

    /// O cmd.exe separa comandos com `&` simples e quebra de linha, redireciona
    /// entrada com `<`, escapa com `^` e expande `%VAR%`: nenhum deles pode
    /// passar como comando único, ou o grant de sessão deixa de reperguntar
    /// diante de `cmd /C "echo ok & payload"`.
    #[test]
    fn separadores_do_cmd_tambem_tornam_composto() {
        for comando in [
            "echo ok & payload",
            "echo ok\npayload",
            "echo ok\rpayload",
            "type saida < entrada.txt",
            "echo ^escapado",
            "echo %PATH%",
        ] {
            assert!(
                argumentos_contem_composto(&mapa(json!({ "comando": comando }))),
                "deveria ser composto: {comando:?}"
            );
        }
    }

    #[test]
    fn comando_unico_sem_separadores_continua_simples() {
        assert!(!argumentos_contem_composto(&mapa(
            json!({ "comando": "git status --short" })
        )));
    }

    /// A recusa do modo não depende do que a ferramenta escreve no disco: um
    /// `Plan` nega a escrita antes de ela chegar ao arquivo. A leitura ele deixa,
    /// e é por isso que o caso testado é o de escrever.
    #[tokio::test]
    async fn o_gate_decide_antes_de_a_ferramenta_rodar() {
        let (execucao, raiz) = execucao_com(crate::permissions::AgentMode::Plan, cadastro(None));
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

    /// O recorte do grafo, lido pela execução que atende as duas portas:
    /// desligada é `false`; sem recorte é o catálogo todo.
    #[test]
    fn a_allowlist_do_grafo_decide_o_que_a_execucao_aceita() {
        let (recortada, _) = execucao_com(
            crate::permissions::AgentMode::FullAccess,
            cadastro(Some(vec!["ler_arquivo"])),
        );
        assert!(recortada.permite_local("ler_arquivo"), "ligada está");
        assert!(
            !recortada.permite_local("escrever_arquivo"),
            "desligada no grafo não passa aqui também"
        );

        let (inteira, _) = execucao_com(crate::permissions::AgentMode::FullAccess, cadastro(None));
        assert!(
            inteira.permite_local("escrever_arquivo"),
            "sem recorte é o catálogo que o aplicativo sabe rodar"
        );
    }

    /// Cadastro vazio não é cadastro aberto: sem o conector embutido não houve
    /// decisão, e permissão não se deduz do silêncio — a mesma resposta de
    /// `manager::permitidas`, para o mesmo ato não ter duas.
    #[test]
    fn sem_cadastro_nao_ha_permicao() {
        let vazio = Arc::new(RwLock::new(Registry::new(vec![]).expect("cadastro vazio")));
        let (execucao, _) = execucao_com(crate::permissions::AgentMode::FullAccess, vazio);
        assert!(
            !execucao.permite_local("ler_arquivo"),
            "um cadastro sem o conector embutido não autoriza nada"
        );
    }

    #[tokio::test]
    async fn token_ja_cancelado_nao_cria_card_nem_executa() {
        use tokio_util::sync::CancellationToken;

        let (execucao, raiz, mut eventos) =
            execucao_com_eventos(crate::permissions::AgentMode::Default, cadastro(None));
        let cancel = CancellationToken::new();
        cancel.cancel();
        let alvo = raiz.join("cancelado-antes.txt");
        let erro = execucao
            .ferramenta_cancelavel(
                local::CONECTOR,
                "escrever_arquivo",
                ToolEffect::Mutating,
                json!({ "caminho": alvo.to_string_lossy(), "conteudo": "não pode existir" }),
                Some(7),
                &cancel,
            )
            .await
            .expect_err("token cancelado recusa a chamada");
        assert_eq!(erro.to_string(), tr("erros.pedidoCancelado", &[]));
        assert!(!alvo.exists(), "execução não começa com token já cancelado");
        assert!(execucao.aprovacoes().pending_ids().is_empty());
        assert!(trava_estado(&execucao.gate()).pending().count() == 0);
        assert!(
            tokio::time::timeout(Duration::from_millis(30), eventos.recv())
                .await
                .is_err(),
            "chamada já cancelada não publica aprovação"
        );
    }

    #[tokio::test]
    async fn cancelar_a_espera_nega_o_card_e_impede_a_escrita() {
        use tokio::time::timeout;
        use tokio_util::sync::CancellationToken;

        let (execucao, raiz, mut eventos) =
            execucao_com_eventos(crate::permissions::AgentMode::Default, cadastro(None));
        let cancel = CancellationToken::new();
        let alvo = raiz.join("aprovacao-cancelada.txt");
        let args = json!({ "caminho": alvo.to_string_lossy(), "conteudo": "tardia" });
        let tarefa = {
            let execucao = Arc::clone(&execucao);
            let cancel_tarefa = cancel.clone();
            tokio::spawn(async move {
                execucao
                    .ferramenta_cancelavel(
                        local::CONECTOR,
                        "escrever_arquivo",
                        ToolEffect::Mutating,
                        args,
                        Some(9),
                        &cancel_tarefa,
                    )
                    .await
            })
        };
        let McpEvent::ApprovalRequested { request } =
            timeout(Duration::from_secs(1), eventos.recv())
                .await
                .expect("card chega")
                .expect("canal de eventos aberto")
        else {
            panic!("era um pedido de aprovação");
        };
        assert_eq!(request.turn_id, Some(9));

        cancel.cancel();
        let resultado = timeout(Duration::from_secs(1), tarefa)
            .await
            .expect("espera termina com o turno")
            .expect("tarefa do turno conclui")
            .expect_err("cancelamento nega a chamada");
        assert_eq!(resultado.to_string(), tr("erros.pedidoCancelado", &[]));
        assert!(!alvo.exists(), "cancelada não escreve");
        assert!(execucao.aprovacoes().pending_ids().is_empty());
        assert!(!execucao.aprovacoes().resolve(&request.id, true));
        assert!(trava_estado(&execucao.gate()).pending().count() == 0);
    }

    #[tokio::test]
    async fn aprovacao_seguida_de_cancelamento_nao_autoriza_execucao() {
        use tokio::time::timeout;
        use tokio_util::sync::CancellationToken;

        let (execucao, raiz, mut eventos) =
            execucao_com_eventos(crate::permissions::AgentMode::Default, cadastro(None));
        let cancel = CancellationToken::new();
        let alvo = raiz.join("aprovada-e-cancelada.txt");
        let args = json!({ "caminho": alvo.to_string_lossy(), "conteudo": "tardia" });
        let tarefa = {
            let execucao = Arc::clone(&execucao);
            let cancel_tarefa = cancel.clone();
            tokio::spawn(async move {
                execucao
                    .ferramenta_cancelavel(
                        local::CONECTOR,
                        "escrever_arquivo",
                        ToolEffect::Mutating,
                        args,
                        Some(10),
                        &cancel_tarefa,
                    )
                    .await
            })
        };
        let McpEvent::ApprovalRequested { request } =
            timeout(Duration::from_secs(1), eventos.recv())
                .await
                .expect("card chega")
                .expect("canal de eventos aberto")
        else {
            panic!("era um pedido de aprovação");
        };

        // Sem yield entre os dois fatos: a aprovação acorda o waiter, mas o token
        // já está cancelado antes de ele voltar a executar.
        assert!(execucao.aprovacoes().resolve(&request.id, true));
        cancel.cancel();
        let resultado = timeout(Duration::from_secs(1), tarefa)
            .await
            .expect("espera termina com o turno")
            .expect("tarefa do turno conclui")
            .expect_err("aprovação/cancelamento nega a chamada");
        assert_eq!(resultado.to_string(), tr("erros.pedidoCancelado", &[]));
        assert!(!alvo.exists(), "resolver não reabre uma chamada cancelada");
    }

    #[test]
    fn resultado_bloqueante_pronto_com_cancelamento_posterior_e_descartado() {
        use tokio_util::sync::CancellationToken;

        let cancel = CancellationToken::new();
        // O select já escolheu o ramo do resultado; o cancelamento chegou
        // depois. A conclusão precisa vencer esse Ok tardio.
        cancel.cancel();
        let resultado = Ok::<_, tokio::task::JoinError>(Ok::<_, anyhow::Error>(42));
        let erro = concluir_bloqueante(&cancel, resultado)
            .expect_err("resultado tardio não pode voltar ao turno");
        assert_eq!(erro.to_string(), tr("erros.pedidoCancelado", &[]));
    }

    #[tokio::test]
    async fn cancelamento_encerra_a_espera_do_spawn_blocking_sem_matar_o_processo() {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::sync::mpsc;
        use tokio::sync::mpsc as tokio_mpsc;
        use tokio::time::timeout;
        use tokio_util::sync::CancellationToken;

        let (iniciou_tx, mut iniciou_rx) = tokio_mpsc::unbounded_channel();
        let (soltar_tx, soltar_rx) = mpsc::channel();
        let terminou = Arc::new(AtomicU64::new(0));
        let cancel = CancellationToken::new();
        let tarefa = {
            let cancel = cancel.clone();
            let terminou = Arc::clone(&terminou);
            tokio::spawn(async move {
                bloqueante_cancelavel(&cancel, move || {
                    iniciou_tx.send(()).expect("aviso de início");
                    soltar_rx
                        .recv_timeout(Duration::from_secs(5))
                        .expect("tarefa bloqueante é solta pelo teste");
                    terminou.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, anyhow::Error>(())
                })
                .await
            })
        };
        timeout(Duration::from_secs(1), iniciou_rx.recv())
            .await
            .expect("tarefa bloqueante começou")
            .expect("aviso de início");

        cancel.cancel();
        assert!(
            timeout(Duration::from_millis(250), tarefa)
                .await
                .expect("o futuro do turno não espera o processo")
                .expect("tarefa do turno conclui")
                .is_err()
        );
        assert_eq!(
            terminou.load(Ordering::SeqCst),
            0,
            "o processo ainda rodava"
        );

        // A limitação é real: dropar o JoinHandle não mata std::process::Command.
        // O resultado tardio fica sem destinatário; a sessão do turno também já
        // suprime resultados depois do cancelamento.
        soltar_tx.send(()).expect("solta o trabalho tardio");
        timeout(Duration::from_secs(1), async {
            while terminou.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("trabalho bloqueante termina depois");
    }
}
