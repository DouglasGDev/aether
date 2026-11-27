use super::approval::{ApprovalBroker, ApprovalRequest};
use super::catalog::{CatalogBudget, CatalogEntry, render};
use super::execucao::{Execucao, resultado_local};
use super::servidor::{self, Publicador};
use super::tool::ToolInfo;
use crate::bancos;
use crate::config::{McpConfig, Segredos};
use crate::connectors::{
    Connector, ConnectorKind, DesktopOs, McpEntry, Registry, ToolMeta, parse_qualified_name,
};
use crate::idiomas::tr;
use crate::local::{self, pastas::Pastas};
use crate::permissions::{PermissionGate, ToolEffect};
use crate::state::{trava_escrita, trava_estado, trava_leitura};
use anyhow::{Context, Result, anyhow, bail};
use http::{HeaderName, HeaderValue};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock};
use rmcp::service::{RoleClient, RunningService, ServiceError};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::process::Command;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

type Client = RunningService<RoleClient, ()>;

#[derive(Debug, Clone, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum McpEvent {
    ToolsDiscovered {
        connector_id: String,
        tools: Vec<ToolInfo>,
    },
    ApprovalRequested {
        request: ApprovalRequest,
    },
    ApprovalSettled {
        id: String,
        approved: bool,
    },
    Disconnected {
        connector_id: String,
    },
}

enum ConnectionCommand {
    Call {
        params: CallToolRequestParams,
        reply: oneshot::Sender<Result<CallToolResult, ServiceError>>,
        cancel: CancellationToken,
    },
}

/// Um servidor MCP é um processo separado, com um canal de comandos. O conector
/// embutido é o próprio aplicativo: não há canal, e a chamada roda aqui dentro.
/// O servidor que o aplicativo publica mantém a conexão de descoberta, mas as
/// chamadas do laço interno rodam no `Execucao`: o turno nunca atravessa a rede
/// num metadado que um cliente externo também poderia escrever.
#[derive(Clone)]
enum Canal {
    Remoto(mpsc::Sender<ConnectionCommand>),
    Publicado,
    Local,
}

struct Connection {
    canal: Canal,
    cliente_descoberta: Option<Client>,
    /// Tudo que o servidor anunciou, sem filtro. O que o modelo pode usar é o
    /// que `tools` devolve, já passado pela allowlist do cadastro: assim, ligar
    /// ou desligar uma ferramenta no grafo vale na chamada seguinte, sem
    /// precisar derrubar e levantar o processo do servidor.
    anunciadas: Vec<ToolInfo>,
    /// Desligar a conexão não pode ficar atrás de `client.call_tool`. Este token
    /// é observado tanto pelo laço de comandos quanto pela chamada ativa.
    desligamento: CancellationToken,
    /// Esperar a tarefa é o que garante que `client.cancel()` — e, no stdio, o
    /// Drop do transporte que mata o filho — aconteceu antes de `disconnect`
    /// devolver.
    tarefa: Option<tokio::task::JoinHandle<()>>,
}

pub struct McpManager {
    /// Compartilhado com a execução: o grafo decide uma vez, e as duas portas
    /// (laço de conversa e servidor publicado) leem a mesma allowlist.
    registry: Arc<RwLock<Registry>>,
    /// O único lugar onde um efeito é decidido e executado. O laço de conversa e
    /// o servidor publicado chegam por portas diferentes e passam por aqui.
    execucao: Arc<Execucao>,
    connections: RwLock<HashMap<String, Connection>>,
    events: broadcast::Sender<McpEvent>,
    /// O servidor MCP das próprias ferramentas do Aether. Publicar é decisão do
    /// agente, e quem tem o cadastro, o arquivo e o cliente é o gerente.
    publicador: Arc<Publicador>,
    config: Arc<Mutex<McpConfig>>,
    config_path: Arc<PathBuf>,
    /// O token do servidor publicado, que nunca vai para o disco.
    segredos: Arc<Segredos>,
    /// Um `CancellationToken` não carrega ID de usuário. Esta é a ligação
    /// explícita token → turno; sem ela a chamada seria tratada como manual.
    turns: Mutex<HashMap<CancellationToken, u64>>,
}

/// As ferramentas que o próprio aplicativo sabe executar, na ordem do catálogo.
/// Os bancos entram porque o conector embutido é quem os executa: as duas
/// famílias passam pelo mesmo gate e pela mesma fila de aprovação. Sem filtro
/// aqui: a allowlist é aplicada na leitura, por `permitidas`.
fn ferramentas_locais() -> Vec<ToolInfo> {
    let mut ferramentas: Vec<ToolInfo> = local::catalogo()
        .iter()
        .chain(bancos::ferramentas::catalogo().iter())
        .map(|f| ToolInfo::local(local::CONECTOR, f))
        .collect();
    // Abrir a conexão MCP é uma ferramenta como as outras: o agente pede, o
    // gerente publica. Ela só existe no conector embutido — dentro do servidor
    // publicado a conexão já estaria de pé.
    ferramentas.push(ToolInfo::local(local::CONECTOR, &servidor::ferramenta()));
    ferramentas
}

/// O recorte da allowlist. Uma ferramenta que o servidor anunciou e o usuário
/// desligou no grafo some daqui: não aparece para o modelo nem passa na chamada.
fn permitidas(registry: &Registry, connector_id: &str, anunciadas: &[ToolInfo]) -> Vec<ToolInfo> {
    match registry.get(connector_id) {
        Some(connector) => anunciadas
            .iter()
            .filter(|tool| connector.allows_tool(&tool.name))
            .cloned()
            .collect(),
        None => Vec::new(),
    }
}

impl McpManager {
    pub fn new(
        registry: Registry,
        gate: Arc<Mutex<PermissionGate>>,
        pastas: Arc<RwLock<Pastas>>,
        memoria: Arc<crate::ai::memoria::Custodio>,
        config: Arc<Mutex<McpConfig>>,
        config_path: Arc<PathBuf>,
        segredos: Arc<Segredos>,
        bancos_path: Arc<PathBuf>,
        derrame: Arc<PathBuf>,
    ) -> Self {
        let (events, _) = broadcast::channel(256);
        let registry = Arc::new(RwLock::new(registry));
        let execucao = Execucao::nova(
            gate,
            Arc::clone(&registry),
            events.clone(),
            pastas,
            memoria,
            bancos_path,
            derrame,
        );
        let publicador = Arc::new(Publicador::novo(Arc::clone(&execucao)));
        Self {
            registry,
            execucao,
            connections: RwLock::new(HashMap::new()),
            events,
            publicador,
            config,
            config_path,
            segredos,
            turns: Mutex::new(HashMap::new()),
        }
    }

    /// Trocar o idioma muda o nome e a descrição que o modelo lê. Sem isto a
    /// conversa continuaria recebendo as ferramentas no idioma anterior.
    pub fn recarregar_embutido(&self) {
        let anunciadas = ferramentas_locais();
        let tools = {
            let registry = trava_leitura(&self.registry);
            if registry.get(local::CONECTOR).is_none() {
                return;
            }
            permitidas(&registry, local::CONECTOR, &anunciadas)
        };
        {
            let mut connections = trava_escrita(&self.connections);
            let Some(connection) = connections.get_mut(local::CONECTOR) else {
                return;
            };
            connection.anunciadas = anunciadas;
        }
        let _ = self.events.send(McpEvent::ToolsDiscovered {
            connector_id: local::CONECTOR.to_string(),
            tools,
        });
    }

    pub fn subscribe(&self) -> broadcast::Receiver<McpEvent> {
        self.events.subscribe()
    }

    pub fn approvals(&self) -> Arc<ApprovalBroker> {
        self.execucao.aprovacoes()
    }

    /// Associa o token criado pela reserva do turno ao seu ID. É necessária uma
    /// vez por turno porque `CancellationToken` não transporta metadado; o main
    /// chama isto ao reservar e `finish_turn` ao soltar a reserva.
    pub fn register_turn(&self, turn_id: u64, cancel: CancellationToken) {
        trava_estado(&self.turns).insert(cancel, turn_id);
    }

    pub fn finish_turn(&self, turn_id: u64) {
        trava_estado(&self.turns).retain(|_, turn| *turn != turn_id);
    }

    pub fn registry_snapshot(&self) -> Registry {
        trava_leitura(&self.registry).clone()
    }

    // Registering a server from the UI must not tear down live connections, so
    // the allowlist is swapped in place rather than rebuilding the manager.
    pub fn replace_registry(&self, registry: Registry) {
        *trava_escrita(&self.registry) = registry;
    }

    pub fn connected_ids(&self) -> Vec<String> {
        trava_leitura(&self.connections).keys().cloned().collect()
    }

    fn pastas_concedidas(&self) -> Vec<String> {
        self.execucao.pastas_concedidas()
    }

    /// O processo filho já nasceu com a lista de pastas na linha de comando,
    /// então mudar a concessão obriga a relançar quem depende dela. Chamado
    /// depois de conceder ou revogar uma pasta.
    ///
    /// Devolve (servidor, motivo) para cada um que não voltou a subir: revogar a
    /// última pasta, por exemplo, deixa o servidor de arquivos desconectado, e
    /// isso é um fato que a interface precisa poder mostrar.
    pub async fn recarregar_pastas(&self) -> Vec<(String, String)> {
        let alvos: Vec<String> = trava_leitura(&self.connections)
            .keys()
            .filter(|id| {
                super::instalador::pronto(id)
                    .is_some_and(|p| p.escopo == super::instalador::Escopo::PastasConcedidas)
            })
            .cloned()
            .collect();
        let mut falhas = Vec::new();
        for id in alvos {
            // Desconectamos sempre: o processo velho tem as pastas antigas.
            if let Err(e) = self.disconnect(&id).await {
                falhas.push((id, format!("{e:#}")));
                continue;
            }
            if let Err(e) = self.connect(&id).await {
                falhas.push((id, format!("{e:#}")));
            }
        }
        falhas
    }

    pub fn tools(&self, connector_id: &str) -> Vec<ToolInfo> {
        let anunciadas = self.anunciadas(connector_id);
        let registry = trava_leitura(&self.registry);
        permitidas(&registry, connector_id, &anunciadas)
    }

    /// Se uma ferramenta está ligada de fato: anunciada pelo servidor e dentro da
    /// allowlist do cadastro. É isto que decide se a memória entra no prompt — o
    /// núcleo não tem uma segunda regra para ela.
    pub fn ferramenta_ativa(&self, connector_id: &str, nome: &str) -> bool {
        let qualificado = format!("{connector_id}__{nome}");
        self.tools(connector_id)
            .iter()
            .any(|tool| tool.qualified_name == qualificado)
    }

    /// O que o servidor anunciou de fato, sem o recorte da allowlist. São essas
    /// portas que o grafo desenha: o que está desligado precisa continuar
    /// visível para poder ser religado.
    pub fn anunciadas(&self, connector_id: &str) -> Vec<ToolInfo> {
        trava_leitura(&self.connections)
            .get(connector_id)
            .map(|c| c.anunciadas.clone())
            .unwrap_or_default()
    }

    pub fn search_tools(&self, query: &str) -> Vec<ToolInfo> {
        let tools: Vec<ToolInfo> = self
            .connected_ids()
            .into_iter()
            .flat_map(|id| self.tools(&id))
            .collect();
        super::intent::rank(&tools, query)
    }

    pub fn catalog(&self, budget: &CatalogBudget) -> String {
        let entries: Vec<CatalogEntry> = self
            .connected_ids()
            .into_iter()
            .flat_map(|id| self.tools(&id))
            .map(|t| CatalogEntry {
                qualified_name: t.qualified_name,
                connector_id: t.connector_id,
                description: t.description,
            })
            .collect();
        render(&entries, budget)
    }

    pub async fn connect(&self, connector_id: &str) -> Result<Vec<ToolInfo>> {
        self.connect_inner(connector_id, None).await
    }

    async fn connect_inner(
        &self,
        connector_id: &str,
        cancel: Option<&CancellationToken>,
    ) -> Result<Vec<ToolInfo>> {
        if cancel.is_some_and(CancellationToken::is_cancelled) {
            bail!(tr("erros.pedidoCancelado", &[]));
        }
        if trava_leitura(&self.connections).contains_key(connector_id) {
            bail!(tr("erros.servidorJaConectado", &[("id", connector_id)]));
        }

        let stored = {
            let registry = trava_leitura(&self.registry);
            registry.get(connector_id).cloned().ok_or_else(|| {
                anyhow!(tr("erros.servidorNaoRegistrado", &[("id", connector_id)]))
            })?
        };

        // Conectar o conector embutido não inicia processo nem abre rede: só
        // publica as ferramentas que o próprio aplicativo sabe executar.
        if stored.kind == ConnectorKind::Builtin {
            let anunciadas = ferramentas_locais();
            let tools = {
                let registry = trava_leitura(&self.registry);
                permitidas(&registry, connector_id, &anunciadas)
            };
            if cancel.is_some_and(CancellationToken::is_cancelled) {
                bail!(tr("erros.pedidoCancelado", &[]));
            }
            trava_escrita(&self.connections).insert(
                connector_id.to_string(),
                Connection {
                    canal: Canal::Local,
                    cliente_descoberta: None,
                    anunciadas,
                    desligamento: CancellationToken::new(),
                    tarefa: None,
                },
            );
            let _ = self.events.send(McpEvent::ToolsDiscovered {
                connector_id: connector_id.to_string(),
                tools: tools.clone(),
            });
            return Ok(tools);
        }

        let connector = crate::config::resolve_connector(&stored, &self.segredos)
            .with_context(|| tr("erros.falhaResolverSegredos", &[("id", connector_id)]))?;
        let entry = connector
            .entry_on(DesktopOs::current())
            .ok_or_else(|| anyhow!(tr("erros.servidorSemEntrada", &[("id", connector_id)])))?;

        // Um servidor do catálogo que só enxerga pastas recebe as concedidas
        // agora, não quando foi instalado: revogar uma pasta tem de valer já.
        let entry = match super::instalador::pronto(connector_id) {
            Some(pronto) if pronto.escopo == super::instalador::Escopo::PastasConcedidas => {
                let pastas = self.pastas_concedidas();
                super::instalador::com_pastas(pronto, entry.clone(), &pastas)?
            }
            _ => entry.clone(),
        };

        let client = match cancel {
            Some(cancel) => {
                dentro_do_prazo_cancelavel(Self::dial(&entry), PRAZO_CONEXAO, connector_id, cancel)
                    .await?
            }
            None => dentro_do_prazo(Self::dial(&entry), PRAZO_CONEXAO, connector_id).await?,
        };
        // A descoberta também tem prazo, e aqui há algo a limpar: o cliente já
        // está de pé, com o processo do servidor vivo atrás dele. Devolver o
        // erro sem fechar a conexão deixaria um filho órfão com a tela dizendo
        // apenas que não conseguiu conectar.
        let descoberta = match cancel {
            Some(cancel) => {
                dentro_do_prazo_cancelavel(
                    async {
                        client
                            .list_all_tools()
                            .await
                            .context(tr("erros.falhaListarFerramentas", &[]))
                    },
                    PRAZO_CONEXAO,
                    connector_id,
                    cancel,
                )
                .await
            }
            None => {
                dentro_do_prazo(
                    async {
                        client
                            .list_all_tools()
                            .await
                            .context(tr("erros.falhaListarFerramentas", &[]))
                    },
                    PRAZO_CONEXAO,
                    connector_id,
                )
                .await
            }
        };
        let advertised = match descoberta {
            Ok(ferramentas) => ferramentas,
            Err(erro) => {
                let _ = client.cancel().await;
                return Err(erro);
            }
        };

        let anunciadas: Vec<ToolInfo> = advertised
            .iter()
            .map(|tool| ToolInfo::from_rmcp(connector_id, tool))
            .collect();
        let tools: Vec<ToolInfo> = anunciadas
            .iter()
            .filter(|tool| connector.allows_tool(&tool.name))
            .cloned()
            .collect();

        if cancel.is_some_and(CancellationToken::is_cancelled) {
            let _ = client.cancel().await;
            bail!(tr("erros.pedidoCancelado", &[]));
        }
        // A conexão com o servidor publicado serve à descoberta. As chamadas do
        // laço interno não voltam pela rede: `Canal::Publicado` executa aqui.
        let (canal, cliente_descoberta, desligamento, tarefa) =
            if connector_id == servidor::CONECTOR {
                (Canal::Publicado, Some(client), CancellationToken::new(), None)
            } else {
                let (sender, receiver) = mpsc::channel(32);
                let desligamento = CancellationToken::new();
                let tarefa = tokio::spawn(run_connection(client, receiver, desligamento.clone()));
                (Canal::Remoto(sender), None, desligamento, Some(tarefa))
            };

        trava_escrita(&self.connections).insert(
            connector_id.to_string(),
            Connection {
                canal,
                cliente_descoberta,
                anunciadas,
                desligamento,
                tarefa,
            },
        );

        let _ = self.events.send(McpEvent::ToolsDiscovered {
            connector_id: connector_id.to_string(),
            tools: tools.clone(),
        });
        Ok(tools)
    }

    // An unanswered approval must not suspend a tool call forever, and expiring
    // it has to resume the waiter as denied rather than leave it parked.
    pub fn expire_approvals(&self) -> Vec<String> {
        let expired = trava_estado(&self.execucao.gate()).expire_pending(Instant::now());
        let approvals = self.execucao.aprovacoes();
        for id in &expired {
            approvals.resolve(id, false);
            let _ = self.events.send(McpEvent::ApprovalSettled {
                id: id.clone(),
                approved: false,
            });
        }
        expired
    }

    /// Cancela só o turno pedido. O token corta chamadas que ainda não chegaram
    /// ao broker; broker e gate removem os cards que já existiam. O waiter emite
    /// `ApprovalSettled` ao acordar, sem um segundo evento aqui.
    pub fn cancel_turn(&self, turn_id: u64) -> usize {
        let tokens: Vec<CancellationToken> = trava_estado(&self.turns)
            .iter()
            .filter(|(_, turn)| **turn == turn_id)
            .map(|(token, _)| token.clone())
            .collect();
        // O broker cancela os tokens enquanto segura o mapa dos waiters. Sem
        // isso, um worker acordado pelo token pode remover o card antes de o
        // gerente contá-lo, fazendo `cancel_turn` devolver zero indevidamente.
        let cancelados = self.execucao.aprovacoes().cancel_turn_com(turn_id, || {
            for token in &tokens {
                token.cancel();
            }
        });
        trava_estado(&self.execucao.gate()).cancel_turn(turn_id);
        trava_estado(&self.turns).retain(|_, turn| *turn != turn_id);
        cancelados
    }

    pub async fn disconnect(&self, connector_id: &str) -> Result<()> {
        let connection = trava_escrita(&self.connections).remove(connector_id);
        let Some(connection) = connection else {
            bail!(tr("erros.servidorNaoConectado", &[("id", connector_id)]));
        };

        // Revoke before the process dies, so no waiter can resume against it.
        self.execucao.aprovacoes().cancel_connector(connector_id);
        trava_estado(&self.execucao.gate()).flush_connector(connector_id);

        // O token é independente da fila de comandos: mesmo com `call_tool`
        // pendurado, o laço larga a chamada, fecha o cliente e devolve. Só então
        // `disconnect` confirma que o transporte (e o filho stdio) foi encerrado.
        connection.desligamento.cancel();
        if let Some(tarefa) = connection.tarefa {
            let _ = tarefa.await;
        }
        if let Some(cliente) = connection.cliente_descoberta {
            let _ = cliente.cancel().await;
        }
        let _ = self.events.send(McpEvent::Disconnected {
            connector_id: connector_id.to_string(),
        });
        Ok(())
    }

    /// Comando manual da UI: sem turno e sem cancelamento.
    pub async fn call_tool(
        &self,
        qualified_name: &str,
        arguments: serde_json::Value,
    ) -> Result<CallToolResult> {
        self.call_tool_inner(qualified_name, arguments, None, &CancellationToken::new())
            .await
    }

    pub async fn call_tool_cancellable(
        &self,
        qualified_name: &str,
        arguments: serde_json::Value,
        cancel: CancellationToken,
    ) -> Result<CallToolResult> {
        let turn_id = trava_estado(&self.turns).get(&cancel).copied();
        self.call_tool_inner(qualified_name, arguments, turn_id, &cancel)
            .await
    }

    async fn call_tool_inner(
        &self,
        qualified_name: &str,
        arguments: serde_json::Value,
        turn_id: Option<u64>,
        cancel: &CancellationToken,
    ) -> Result<CallToolResult> {
        if cancel.is_cancelled() {
            bail!(tr("erros.pedidoCancelado", &[]));
        }
        let meta = {
            let registry = trava_leitura(&self.registry);
            ToolMeta::resolve(&registry, qualified_name)
        };
        let meta = match meta {
            Some(meta) => meta,
            // O nome que o modelo escreveu não é uma ferramenta que ele possa
            // chamar. Dizer só isso faz ele procurar bloqueio político onde houve
            // um nome errado: medido no Granite 4.2 3B, que chamou
            // `computador__write_file` e gastou 3.483 tokens tentando gravar o
            // arquivo por `echo` no terminal. Com a lista na resposta do erro, a
            // correção cabe na rodada seguinte.
            None => {
                let nomes = |id: &str| -> Vec<String> {
                    self.tools(id)
                        .into_iter()
                        .map(|t| t.qualified_name)
                        .collect()
                };
                // O conector pode nem estar conectado: aí a lista útil é a de
                // tudo o que existe agora, senão o erro não ensina nada.
                let lista = match parse_qualified_name(qualified_name) {
                    Some((id, _)) if !nomes(id).is_empty() => nomes(id),
                    _ => self
                        .connected_ids()
                        .iter()
                        .flat_map(|id| nomes(id))
                        .collect(),
                };
                bail!(tr(
                    "erros.ferramentaInexistente",
                    &[("nome", qualified_name), ("lista", &lista.join(", "))]
                ));
            }
        };

        let arguments = match arguments {
            serde_json::Value::Object(map) => map,
            _ => bail!(tr("erros.argumentosInvalidos", &[])),
        };

        let (canal, effect) = {
            let connections = trava_leitura(&self.connections);
            let connection = connections.get(&meta.connector_id).ok_or_else(|| {
                anyhow!(tr(
                    "erros.servidorNaoConectado",
                    &[("id", &meta.connector_id)]
                ))
            })?;
            let effect = connection
                .anunciadas
                .iter()
                .find(|t| t.name == meta.tool)
                .map(|t| t.effect)
                .unwrap_or(ToolEffect::Mutating);
            (connection.canal.clone(), effect)
        };

        match canal {
            Canal::Local => {
                // Publicar o servidor é trabalho do gerente, não de `local::executar`:
                // só aqui existem o cadastro, o arquivo e o segredo do token. O gate
                // decide antes, como decide para qualquer efeito que escreve.
                if meta.tool == servidor::FERRAMENTA {
                    self.execucao
                        .aprovar_cancelavel(
                            local::CONECTOR,
                            &meta.tool,
                            effect,
                            &arguments,
                            turn_id,
                            cancel,
                        )
                        .await?;
                    if cancel.is_cancelled() {
                        bail!(tr("erros.pedidoCancelado", &[]));
                    }
                    let estado = self.publicar_cancelavel(cancel).await?;
                    return Ok(resultado_publicado(&estado));
                }
                let saida = self
                    .execucao
                    .ferramenta_cancelavel(
                        local::CONECTOR,
                        &meta.tool,
                        effect,
                        serde_json::Value::Object(arguments),
                        turn_id,
                        cancel,
                    )
                    .await?;
                Ok(resultado_local(saida))
            }
            Canal::Remoto(sender) => {
                // A ferramenta roda do outro lado, então a decisão tem de acontecer
                // aqui: é o cliente que sabe o que o usuário permitiu.
                self.execucao
                    .aprovar_cancelavel(
                        &meta.connector_id,
                        &meta.tool,
                        effect,
                        &arguments,
                        turn_id,
                        cancel,
                    )
                    .await?;
                self.enviar(sender, &meta, arguments, cancel).await
            }
            Canal::Publicado => {
                // O turno é uma identidade interna, não um metadado de protocolo.
                // Executar aqui conserva esse vínculo e evita uma segunda decisão
                // no endpoint externo, onde toda chamada é manual.
                let saida = self
                    .execucao
                    .ferramenta_cancelavel(
                        &meta.connector_id,
                        &meta.tool,
                        effect,
                        serde_json::Value::Object(arguments),
                        turn_id,
                        cancel,
                    )
                    .await?;
                Ok(resultado_local(saida))
            }
        }
    }

    async fn enviar(
        &self,
        sender: mpsc::Sender<ConnectionCommand>,
        meta: &ToolMeta,
        arguments: serde_json::Map<String, serde_json::Value>,
        cancel: &CancellationToken,
    ) -> Result<CallToolResult> {
        if cancel.is_cancelled() {
            bail!(tr("erros.pedidoCancelado", &[]));
        }
        let params = CallToolRequestParams::new(meta.tool.clone()).with_arguments(arguments);
        let (reply_tx, reply_rx) = oneshot::channel();
        tokio::select! {
            biased;
            _ = cancel.cancelled() => bail!(tr("erros.pedidoCancelado", &[])),
            enviado = sender.send(ConnectionCommand::Call {
                params,
                reply: reply_tx,
                cancel: cancel.clone(),
            }) => {
                enviado.map_err(|_| anyhow!(tr("erros.servidorParou", &[("id", &meta.connector_id)])))?;
            }
        }
        if cancel.is_cancelled() {
            bail!(tr("erros.pedidoCancelado", &[]));
        }

        let resposta = tokio::select! {
            biased;
            _ = cancel.cancelled() => bail!(tr("erros.pedidoCancelado", &[])),
            resposta = async { reply_rx.await } => {
                resposta.map_err(|_| anyhow!(tr("erros.servidorDerrubouChamada", &[("id", &meta.connector_id)])))?
            }
        };
        if cancel.is_cancelled() {
            bail!(tr("erros.pedidoCancelado", &[]));
        }
        resposta.map_err(|e| {
            anyhow!(tr(
                "erros.falhaChamarFerramenta",
                &[("detalhe", &e.to_string())]
            ))
        })
    }

    /// Levanta o servidor MCP das ferramentas do Aether e o entra no cadastro,
    /// no arquivo e nas ligações. É o único caminho que publica: a ferramenta do
    /// agente e o comando da tela chegam aqui.
    ///
    /// O token vai para a memória (`Segredos`); o `mcp.json` guarda a referência
    /// `${AETHER_MCP_TOKEN}`, nunca o valor. Reconectar vale porque a porta muda a
    /// cada execução: o cliente velho aponta para uma porta que não existe mais.
    pub async fn publicar(&self) -> Result<servidor::Estado> {
        let cancel = CancellationToken::new();
        self.publicar_com(&cancel, || async {
            self.connect_inner(servidor::CONECTOR, Some(&cancel))
                .await
                .map(|_| ())
        })
        .await
    }

    async fn publicar_cancelavel(&self, cancel: &CancellationToken) -> Result<servidor::Estado> {
        self.publicar_com(cancel, || async {
            self.connect_inner(servidor::CONECTOR, Some(cancel))
                .await
                .map(|_| ())
        })
        .await
    }

    /// O futuro de conexão entra como parâmetro para que o cancelamento possa
    /// ser observado antes de qualquer handshake. Em produção é o `connect_inner`
    /// cancelável; nos testes, uma conexão parada prova o rollback no meio do
    /// caminho sem depender de um servidor que por acaso responda devagar.
    async fn publicar_com<F, Fut>(
        &self,
        cancel: &CancellationToken,
        conectar: F,
    ) -> Result<servidor::Estado>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        if cancel.is_cancelled() {
            bail!(tr("erros.pedidoCancelado", &[]));
        }
        let conector_anterior = trava_estado(&self.config)
            .servers
            .iter()
            .find(|conector| conector.id == servidor::CONECTOR)
            .cloned();
        let mut config_alterada = false;
        let (estado, token) = self.publicador.abrir().await?;

        let resultado: Result<()> = async {
            if cancel.is_cancelled() {
                bail!(tr("erros.pedidoCancelado", &[]));
            }
            self.segredos.guardar(servidor::VARIAVEL_TOKEN, &token);
            let registry = {
                let mut config = trava_estado(&self.config);
                config.upsert(servidor::conector(&estado));
                config_alterada = true;
                config.save(&self.config_path)?;
                config.registry()?
            };
            self.replace_registry(registry);
            if cancel.is_cancelled() {
                bail!(tr("erros.pedidoCancelado", &[]));
            }

            // Uma ligação velha cai com a porta velha; sem isto o conector ficaria
            // "conectado" apontando para um endereço que já não escuta.
            if trava_leitura(&self.connections).contains_key(servidor::CONECTOR) {
                self.disconnect(servidor::CONECTOR).await.ok();
            }
            if cancel.is_cancelled() {
                bail!(tr("erros.pedidoCancelado", &[]));
            }
            let conexao = conectar();
            let resultado = tokio::select! {
                biased;
                _ = cancel.cancelled() => Err(anyhow!(tr("erros.pedidoCancelado", &[]))),
                resultado = conexao => resultado,
            };
            // Um handshake que ficou pronto no mesmo poll do cancelamento não
            // pode publicar uma conexão que o Stop já mandou desfazer.
            if cancel.is_cancelled() {
                bail!(tr("erros.pedidoCancelado", &[]));
            }
            resultado?;
            Ok(())
        }
        .await;

        if resultado.is_err() || cancel.is_cancelled() {
            let original = if cancel.is_cancelled() {
                anyhow!(tr("erros.pedidoCancelado", &[]))
            } else {
                resultado.expect_err("o caminho de rollback só roda com erro")
            };
            if let Err(limpeza) = self
                .restaurar_publicacao(conector_anterior, config_alterada)
                .await
            {
                // `to_string()` continua sendo o motivo original; a cadeia
                // mantém a falha de limpeza para quem precisar diagnosticá-la.
                return Err(limpeza.context(format!("{original:#}")));
            }
            return Err(original);
        }
        Ok(estado)
    }

    /// Desfaz somente o conector publicado, sem sobrescrever alterações que a
    /// tela tenha feito em outros servidores durante a tentativa. O servidor
    /// sempre fecha e o segredo sempre sai: uma publicação cancelada não deixa
    /// uma porta viva atrás do Stop.
    async fn restaurar_publicacao(
        &self,
        conector_anterior: Option<Connector>,
        config_alterada: bool,
    ) -> Result<()> {
        let mut primeira_falha: Option<anyhow::Error> = None;
        if trava_leitura(&self.connections).contains_key(servidor::CONECTOR)
            && let Err(e) = self.disconnect(servidor::CONECTOR).await
        {
            primeira_falha.get_or_insert_with(|| anyhow!(e));
        }
        if let Err(e) = self.publicador.fechar().await {
            primeira_falha.get_or_insert_with(|| anyhow!(e));
        }
        self.segredos.esquecer(servidor::VARIAVEL_TOKEN);

        if config_alterada {
            let (salvar, registro) = {
                let mut config = trava_estado(&self.config);
                if let Some(conector) = conector_anterior {
                    config.upsert(conector);
                } else {
                    config.remove(servidor::CONECTOR);
                }
                (config.save(&self.config_path), config.registry())
            };
            if let Err(e) = salvar {
                primeira_falha.get_or_insert(e);
            }
            match registro {
                Ok(registry) => self.replace_registry(registry),
                Err(e) => {
                    primeira_falha.get_or_insert_with(|| anyhow!(e));
                }
            }
        }
        match primeira_falha {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Derruba o servidor e esquece o segredo. O cadastro fica onde está: é o
    /// conector de sempre, desconectado, pronto para publicar de novo.
    pub async fn despublicar(&self) -> Result<bool> {
        if trava_leitura(&self.connections).contains_key(servidor::CONECTOR) {
            self.disconnect(servidor::CONECTOR).await.ok();
        }
        let parada = self.publicador.fechar().await?;
        if parada {
            self.segredos.esquecer(servidor::VARIAVEL_TOKEN);
        }
        Ok(parada)
    }

    /// Desliga tudo o que o aplicativo abriu. A porta de retorno vem primeiro
    /// porque derrubar a própria ligação é o que fecha o `CONECTOR` publicado;
    /// depois vem cada servidor stdio — é o `Drop` do transporte que mata o
    /// filho, e ele só acontece quando a conexão sai daqui. Sem este caminho,
    /// fechar a janela deixava `node.exe` servindo uma ferramenta que ninguém
    /// mais chama, com o token ainda válido na memória de um processo órfão.
    pub async fn desligar(&self) {
        let _ = self.despublicar().await;
        for id in self.connected_ids() {
            let _ = self.disconnect(&id).await;
        }
    }

    pub fn estado_servidor(&self) -> servidor::Estado {
        self.publicador.estado()
    }

    pub fn token_servidor(&self) -> Option<String> {
        self.publicador.token()
    }

    async fn dial(entry: &McpEntry) -> Result<Client> {
        match entry {
            McpEntry::Stdio { command, args, env } => {
                let mut cmd = Command::new(command);
                cmd.args(args).envs(env);
                let transport =
                    TokioChildProcess::new(cmd).context(tr("erros.falhaIniciarServidor", &[]))?;
                ().serve(transport).await.map_err(|e| {
                    anyhow!(tr(
                        "erros.falhaConectarServidor",
                        &[("detalhe", &format!("{e:?}"))]
                    ))
                })
            }
            McpEntry::Http { url, headers } => {
                let mut config = StreamableHttpClientTransportConfig::with_uri(url.clone());
                if !headers.is_empty() {
                    let mut parsed = HashMap::new();
                    for (name, value) in headers {
                        let name = HeaderName::from_bytes(name.as_bytes())
                            .with_context(|| tr("erros.cabecalhoInvalido", &[("nome", name)]))?;
                        let value = HeaderValue::from_str(value).with_context(|| {
                            tr("erros.valorCabecalhoInvalido", &[("nome", name.as_str())])
                        })?;
                        parsed.insert(name, value);
                    }
                    config = config.custom_headers(parsed);
                }
                let transport = StreamableHttpClientTransport::from_config(config);
                ().serve(transport).await.map_err(|e| {
                    anyhow!(tr(
                        "erros.falhaConectarServidor",
                        &[("detalhe", &format!("{e:?}"))]
                    ))
                })
            }
        }
    }
}

/// Dropar o futuro do rmcp **enfileira** o `notifications/cancelled`; quem o
/// despeja é o laço do serviço, no mesmo canal que `close()` vai fechar. Sem uma
/// volta do executor entre os dois, o desligamento cala o aviso antes de ele
/// chegar — e o servidor remoto continua executando a ferramenta sem ninguém do
/// outro lado para receber o resultado. É espera com fim, não sondagem: o prazo
/// é o teto do aviso, não uma aposta de que a conexão está lenta.
const AVISO_DE_CANCELAMENTO: Duration = Duration::from_millis(50);

async fn run_connection(
    client: Client,
    mut receiver: mpsc::Receiver<ConnectionCommand>,
    desligamento: CancellationToken,
) {
    loop {
        let comando = tokio::select! {
            biased;
            _ = desligamento.cancelled() => break,
            comando = receiver.recv() => match comando {
                Some(comando) => comando,
                None => break,
            },
        };
        match comando {
            ConnectionCommand::Call {
                params,
                reply,
                cancel,
            } => {
                // Dropar o futuro do rmcp envia `notifications/cancelled` de
                // forma best-effort. Só esta chamada é dropada; o cliente serial e
                // a conexão continuam vivos para os outros turnos. Já o token de
                // desligamento corta a chamada ativa e encerra o cliente inteiro.
                tokio::select! {
                    biased;
                    _ = desligamento.cancelled() => break,
                    _ = cancel.cancelled() => {}
                    result = client.call_tool(params) => {
                        let _ = reply.send(result);
                    }
                }
            }
        }
    }
    let _ = tokio::time::sleep(AVISO_DE_CANCELAMENTO).await;
    let _ = client.cancel().await;
}

/// O cartão do `criar_mcp_local`: o que o agente precisa saber é onde a conexão
/// está e quantas ferramentas ela atende. O token não vai aqui — ele se copia na
/// tela, não se despeja no histórico do modelo.
fn resultado_publicado(estado: &servidor::Estado) -> CallToolResult {
    let texto = tr(
        "servidorMcp.publicada",
        &[
            ("url", &estado.url),
            ("ferramentas", &estado.ferramentas.to_string()),
        ],
    );
    let mut resultado = CallToolResult::success(vec![ContentBlock::text(texto)]);
    resultado.structured_content = Some(serde_json::json!({
        "url": estado.url,
        "porta": estado.porta,
        "ferramentas": estado.ferramentas,
    }));
    resultado.is_error = Some(false);
    resultado
}

/// O aperto de mão e a descoberta de ferramentas têm prazo. Sem ele, um
/// servidor que nasce mudo — o script certo esperando uma entrada que nunca
/// chega, o binário que abre o socket e não responde — pendurava `conectar` para
/// sempre: a tela girava sem devolver erro nenhum.
const PRAZO_CONEXAO: Duration = Duration::from_secs(45);

/// Espera `fazer` até `prazo` e diz qual servidor não respondeu. O prazo é
/// parâmetro, não constante embutida, para que a conta possa ser conferida num
/// teste sem esperar quarenta e cinco segundos reais.
///
/// Cortar pelo meio é seguro nos dois transportes: no stdio a futura carregava
/// o `TokioChildProcess` e o `Drop` dele mata o filho (rmcp
/// `transport/child_process.rs`, `ChildWithCleanup`); no HTTP não há processo
/// nenhum para matar.
async fn dentro_do_prazo<T, F>(fazer: F, prazo: Duration, id: &str) -> Result<T>
where
    F: std::future::Future<Output = Result<T>>,
{
    match tokio::time::timeout(prazo, fazer).await {
        Ok(feito) => feito,
        Err(_) => bail!(tr(
            "erros.servidorSemResposta",
            &[("id", id), ("segundos", &prazo.as_secs().to_string())],
        )),
    }
}

/// A mesma regra do timeout, com o Stop do turno na frente. Depois do resultado
/// o token é reconferido: ficar pronto junto com o cancelamento não é sucesso.
async fn dentro_do_prazo_cancelavel<T, F>(
    fazer: F,
    prazo: Duration,
    id: &str,
    cancel: &CancellationToken,
) -> Result<T>
where
    F: std::future::Future<Output = Result<T>>,
{
    if cancel.is_cancelled() {
        bail!(tr("erros.pedidoCancelado", &[]));
    }
    let espera = async {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(anyhow!(tr("erros.pedidoCancelado", &[]))),
            resultado = fazer => resultado,
        }
    };
    let resultado = tokio::time::timeout(prazo, espera).await;
    if cancel.is_cancelled() {
        bail!(tr("erros.pedidoCancelado", &[]));
    }
    match resultado {
        Ok(feito) => feito,
        Err(_) => bail!(tr(
            "erros.servidorSemResposta",
            &[("id", id), ("segundos", &prazo.as_secs().to_string())],
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Um servidor mudo não pode pendurar o botão de conectar: o prazo estoura,
    /// o erro diz qual servidor ficou sem resposta e a espera acaba na hora, não
    /// quando o processo do servidor resolver falar.
    #[tokio::test]
    async fn o_servidor_que_nao_responde_devolve_um_erro_em_vezes_de_ficar_pendurado() {
        let mudo = async {
            tokio::time::sleep(Duration::from_secs(5)).await;
            Ok(())
        };
        let inicio = Instant::now();
        let erro = dentro_do_prazo(mudo, Duration::from_millis(50), "filesystem")
            .await
            .expect_err("um servidor mudo não pode devolver sucesso");
        let texto = erro.to_string();
        assert!(texto.contains("filesystem"), "{texto}");
        assert!(texto.contains("respondeu"), "{texto}");
        assert!(
            inicio.elapsed() < Duration::from_secs(1),
            "o prazo não cortou a espera: {:?} para lá do que 50 ms",
            inicio.elapsed()
        );
    }

    #[tokio::test]
    async fn o_servidor_que_responde_passa_pela_regua_inteiro() {
        let resposta = dentro_do_prazo(async { Ok(7usize) }, Duration::from_secs(5), "fetch")
            .await
            .expect("resposta dentro do prazo é sucesso");
        assert_eq!(resposta, 7);
    }

    /// A ferramenta de publicar é do conector embutido. Dentro do servidor
    /// publicado a conexão já estaria de pé; anunciá-la lá seria um convite para
    /// abrir a porta outra vez.
    #[test]
    fn o_catalogo_embutido_ganha_a_ferramenta_de_publicar() {
        let nomes: Vec<String> = ferramentas_locais().into_iter().map(|t| t.name).collect();
        assert_eq!(
            nomes.len(),
            local::catalogo().len() + bancos::ferramentas::catalogo().len() + 1
        );
        assert!(
            nomes.contains(&servidor::FERRAMENTA.to_string()),
            "{nomes:?}"
        );
        assert!(nomes.contains(&"bancos_listar".to_string()), "{nomes:?}");
        assert!(nomes.contains(&"bancos_apagar".to_string()), "{nomes:?}");
        let apagar = ferramentas_locais()
            .into_iter()
            .find(|t| t.name == "bancos_apagar")
            .expect("bancos_apagar anunciada");
        assert_eq!(apagar.effect, ToolEffect::Destructive);
    }

    #[test]
    fn o_resultado_da_publicacao_diz_onde_sem_dizer_o_segredo() {
        let estado = servidor::Estado {
            ativa: true,
            url: "http://127.0.0.1:4599/mcp".to_string(),
            porta: 4599,
            ferramentas: 11,
        };
        let resultado = resultado_publicado(&estado);
        let json = serde_json::to_string(&resultado.content).expect("o conteúdo serializa");
        assert!(json.contains("4599"), "{json}");
        assert!(
            !json.contains(servidor::VARIAVEL_TOKEN),
            "o token não vai no resultado"
        );
        assert_eq!(resultado.is_error, Some(false));
        let vista = resultado.structured_content.expect("o cartão lê a vista");
        assert_eq!(vista["porta"], serde_json::json!(4599));
        assert_eq!(vista["ferramentas"], serde_json::json!(11));
    }

    async fn manager_conectado(modo: crate::permissions::AgentMode) -> (Arc<McpManager>, PathBuf) {
        let raiz = std::env::temp_dir().join(format!("aether-manager-{}", std::process::id()));
        std::fs::create_dir_all(&raiz).expect("pasta de teste");
        let pastas = Arc::new(RwLock::new(Pastas {
            raizes: vec![raiz.clone()],
        }));
        let manager = Arc::new(McpManager::new(
            Registry::new(vec![local::conector()]).expect("conector embutido"),
            Arc::new(Mutex::new(PermissionGate::new(modo))),
            Arc::clone(&pastas),
            Arc::new(crate::ai::memoria::Custodio::novo(
                &[],
                &raiz.join("modelos"),
                &raiz.join("memoria"),
                raiz.join("rag.json"),
                &raiz.join(crate::ai::base::PASTA),
                pastas,
            )),
            Arc::new(Mutex::new(McpConfig::default())),
            Arc::new(raiz.join("mcp.json")),
            Arc::new(Segredos::default()),
            Arc::new(raiz.join("bancos.json")),
            Arc::new(raiz.join("resultados")),
        ));
        manager
            .connect(local::CONECTOR)
            .await
            .expect("conector embutido conecta");
        (manager, raiz)
    }

    async fn proxima_aprovacao(eventos: &mut broadcast::Receiver<McpEvent>) -> ApprovalRequest {
        tokio::time::timeout(Duration::from_secs(1), eventos.recv())
            .await
            .expect("aprovação chega")
            .expect("canal de eventos aberto")
            .into()
    }

    impl From<McpEvent> for ApprovalRequest {
        fn from(event: McpEvent) -> Self {
            match event {
                McpEvent::ApprovalRequested { request } => request,
                other => panic!("esperava aprovação, veio {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn cancelar_turno_nao_atinge_aprovacao_de_outro_turno() {
        use tokio_util::sync::CancellationToken;

        let (manager, raiz) = manager_conectado(crate::permissions::AgentMode::Default).await;
        let token_um = CancellationToken::new();
        let token_dois = CancellationToken::new();
        manager.register_turn(11, token_um.clone());
        manager.register_turn(12, token_dois.clone());
        let mut eventos = manager.subscribe();
        let alvo_um = raiz.join("turno-11.txt");
        let alvo_dois = raiz.join("turno-12.txt");
        let args_um = serde_json::json!({
            "caminho": alvo_um.to_string_lossy(),
            "conteudo": "cancelado",
        });
        let args_dois = serde_json::json!({
            "caminho": alvo_dois.to_string_lossy(),
            "conteudo": "autorizado",
        });
        let primeira = {
            let manager = Arc::clone(&manager);
            tokio::spawn(async move {
                manager
                    .call_tool_cancellable(
                        &format!("{}__escrever_arquivo", local::CONECTOR),
                        args_um,
                        token_um,
                    )
                    .await
            })
        };
        let segunda = {
            let manager = Arc::clone(&manager);
            let token_tarefa = token_dois.clone();
            tokio::spawn(async move {
                manager
                    .call_tool_cancellable(
                        &format!("{}__escrever_arquivo", local::CONECTOR),
                        args_dois,
                        token_tarefa,
                    )
                    .await
            })
        };

        let mut pedidos = Vec::new();
        while pedidos.len() < 2 {
            pedidos.push(proxima_aprovacao(&mut eventos).await);
        }
        let pedido_um = pedidos
            .iter()
            .find(|p| p.turn_id == Some(11))
            .expect("card do turno 11");
        let pedido_dois = pedidos
            .iter()
            .find(|p| p.turn_id == Some(12))
            .expect("card do turno 12");

        assert_eq!(manager.cancel_turn(11), 1);
        assert!(
            !token_dois.is_cancelled(),
            "o token do outro turno continua vivo"
        );
        assert!(
            tokio::time::timeout(Duration::from_secs(1), primeira)
                .await
                .expect("turno cancelado termina")
                .expect("tarefa do turno conclui")
                .expect_err("turno cancelado não executa")
                .to_string()
                == tr("erros.pedidoCancelado", &[])
        );
        assert!(!segunda.is_finished(), "o outro turno continua esperando");
        assert!(!manager.approvals().resolve(&pedido_um.id, true));
        assert!(manager.approvals().resolve(&pedido_dois.id, true));
        assert!(
            tokio::time::timeout(Duration::from_secs(1), segunda)
                .await
                .expect("outro turno termina")
                .is_ok()
        );
        assert!(!alvo_um.exists(), "cancelado não escreve");
        assert!(alvo_dois.exists(), "aprovado escreve");
    }

    #[tokio::test]
    async fn chamada_manual_continua_sem_turn_id() {
        let (manager, raiz) = manager_conectado(crate::permissions::AgentMode::Default).await;
        let mut eventos = manager.subscribe();
        let alvo = raiz.join("manual.txt");
        let args = serde_json::json!({
            "caminho": alvo.to_string_lossy(),
            "conteudo": "manual",
        });
        let tarefa = {
            let manager = Arc::clone(&manager);
            tokio::spawn(async move {
                manager
                    .call_tool(&format!("{}__escrever_arquivo", local::CONECTOR), args)
                    .await
            })
        };
        let pedido = proxima_aprovacao(&mut eventos).await;
        assert_eq!(pedido.turn_id, None);
        assert!(manager.approvals().resolve(&pedido.id, true));
        assert!(
            tokio::time::timeout(Duration::from_secs(1), tarefa)
                .await
                .expect("manual termina")
                .is_ok()
        );
        assert!(alvo.exists());
    }

    #[tokio::test]
    async fn token_ja_cancelado_nao_envia_chamada_remota() {
        use tokio::sync::mpsc;
        use tokio_util::sync::CancellationToken;

        let (manager, _) = manager_conectado(crate::permissions::AgentMode::FullAccess).await;
        let (sender, mut receptor) = mpsc::channel(1);
        let meta = ToolMeta {
            connector_id: local::CONECTOR.to_string(),
            tool: "info_sistema".to_string(),
        };
        let cancel = CancellationToken::new();
        cancel.cancel();
        let erro = manager
            .enviar(sender, &meta, serde_json::Map::new(), &cancel)
            .await
            .expect_err("token já cancelado recusa envio");
        assert_eq!(erro.to_string(), tr("erros.pedidoCancelado", &[]));
        assert!(
            matches!(
                receptor.try_recv(),
                Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected)
            ),
            "nenhum comando chega ao servidor"
        );
    }

    #[tokio::test]
    async fn cancelar_o_token_encerra_a_espera_da_resposta_remota() {
        use tokio::sync::mpsc;
        use tokio_util::sync::CancellationToken;

        let (manager, _) = manager_conectado(crate::permissions::AgentMode::FullAccess).await;
        let (sender, mut receptor) = mpsc::channel(1);
        let meta = ToolMeta {
            connector_id: local::CONECTOR.to_string(),
            tool: "info_sistema".to_string(),
        };
        let cancel = CancellationToken::new();
        let mut futura = Box::pin(manager.enviar(sender, &meta, serde_json::Map::new(), &cancel));
        let comando = tokio::select! {
            comando = receptor.recv() => comando.expect("comando enviado"),
            _ = &mut futura => panic!("espera terminou antes do envio"),
        };
        assert!(matches!(comando, ConnectionCommand::Call { .. }));
        drop(comando); // derruba o oneshot sem responder

        cancel.cancel();
        let erro = tokio::time::timeout(Duration::from_millis(250), &mut futura)
            .await
            .expect("não fica esperando resposta depois do cancelamento")
            .expect_err("resposta foi derrubada");
        assert_eq!(erro.to_string(), tr("erros.pedidoCancelado", &[]));
    }

    #[tokio::test]
    async fn cancelamento_corta_o_handshake_sem_esperar_o_prazo_inteiro() {
        use tokio_util::sync::CancellationToken;

        let cancel = CancellationToken::new();
        let inicio = Instant::now();
        let espera = async {
            cancel.cancel();
            tokio::time::sleep(Duration::from_secs(5)).await;
            Ok::<_, anyhow::Error>(())
        };
        let erro = tokio::time::timeout(
            Duration::from_millis(250),
            dentro_do_prazo_cancelavel(espera, Duration::from_secs(45), "aether", &cancel),
        )
        .await
        .expect("o cancelamento não espera os 45 segundos")
        .expect_err("handshake cancelado não tem sucesso");
        assert_eq!(erro.to_string(), tr("erros.pedidoCancelado", &[]));
        assert!(inicio.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn publicacao_cancelada_no_meio_nao_deixa_servidor_segredo_nem_cadastro() {
        let (manager, _) = manager_conectado(crate::permissions::AgentMode::FullAccess).await;
        let cancel = CancellationToken::new();
        let (chegou_na_conexao, mut aviso) = mpsc::channel(1);
        let tarefa = {
            let manager = Arc::clone(&manager);
            let cancel_tarefa = cancel.clone();
            tokio::spawn(async move {
                manager
                    .publicar_com(&cancel_tarefa, move || {
                        // `send` é assíncrono: segurar o futuro sem aguardar não
                        // entrega nada. `try_send` empurra na hora, que é o que o
                        // aviso precisa — a conexão em si continua pendurada.
                        let _ = chegou_na_conexao.try_send(());
                        std::future::pending::<Result<()>>()
                    })
                    .await
            })
        };
        tokio::time::timeout(Duration::from_secs(1), aviso.recv())
            .await
            .expect("publicação chega à conexão")
            .expect("aviso aberto");
        // O servidor, o segredo e o cadastro já existem neste ponto. Parar aqui
        // é o caso que antes podia esperar o handshake e ainda publicar.
        assert!(manager.estado_servidor().ativa);
        assert!(manager.token_servidor().is_some());
        assert!(manager.segredos.obter(servidor::VARIAVEL_TOKEN).is_some());
        assert!(
            manager
                .registry_snapshot()
                .get(servidor::CONECTOR)
                .is_some()
        );
        cancel.cancel();

        let erro = tokio::time::timeout(Duration::from_secs(1), tarefa)
            .await
            .expect("publicação cancelada termina sem o handshake")
            .expect("tarefa conclui")
            .expect_err("Stop não devolve publicação");
        assert_eq!(erro.to_string(), tr("erros.pedidoCancelado", &[]));
        assert!(!manager.estado_servidor().ativa, "servidor foi fechado");
        assert!(manager.token_servidor().is_none(), "token foi esquecido");
        assert!(
            manager.segredos.obter(servidor::VARIAVEL_TOKEN).is_none(),
            "segredo foi removido"
        );
        assert!(
            !manager
                .connected_ids()
                .contains(&servidor::CONECTOR.to_string()),
            "nenhuma conexão publicada sobrou"
        );
        assert!(
            manager
                .registry_snapshot()
                .get(servidor::CONECTOR)
                .is_none(),
            "cadastro em memória foi restaurado"
        );
        let config = McpConfig::load(&manager.config_path).expect("mcp.json lê após rollback");
        assert!(
            config
                .servers
                .iter()
                .all(|conector| conector.id != servidor::CONECTOR),
            "cadastro em disco foi restaurado"
        );

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !manager.estado_servidor().ativa,
            "nenhum trabalho tardio torna a publicar"
        );
    }

    #[tokio::test]
    async fn chamada_publicada_interna_executa_uma_vez_com_o_turno_interno() {
        let (manager, raiz) = manager_conectado(crate::permissions::AgentMode::Default).await;
        manager.replace_registry(
            Registry::new(vec![
                local::conector(),
                servidor::conector(&servidor::Estado {
                    ativa: true,
                    url: "http://127.0.0.1:4599/mcp".to_string(),
                    porta: 4599,
                    ferramentas: 1,
                }),
            ])
            .expect("registro interno e publicado"),
        );
        let anunciadas: Vec<ToolInfo> = local::catalogo()
            .iter()
            .chain(bancos::ferramentas::catalogo().iter())
            .map(|ferramenta| ToolInfo::local(servidor::CONECTOR, ferramenta))
            .collect();
        trava_escrita(&manager.connections).insert(
            servidor::CONECTOR.to_string(),
            Connection {
                canal: Canal::Publicado,
                cliente_descoberta: None,
                anunciadas,
                desligamento: CancellationToken::new(),
                tarefa: None,
            },
        );
        let cancel = CancellationToken::new();
        manager.register_turn(91, cancel.clone());
        let mut eventos = manager.subscribe();
        let alvo = raiz.join("publicado-interno.txt");
        let args = serde_json::json!({
            "caminho": alvo.to_string_lossy(),
            "conteudo": "turno interno",
        });
        let tarefa = {
            let manager = Arc::clone(&manager);
            tokio::spawn(async move {
                manager
                    .call_tool_cancellable(
                        &format!("{}__escrever_arquivo", servidor::CONECTOR),
                        args,
                        cancel,
                    )
                    .await
            })
        };
        let pedido = proxima_aprovacao(&mut eventos).await;
        assert_eq!(pedido.turn_id, Some(91), "o turno continua sendo interno");
        {
            // A conexão publicada não tem canal nem worker: não existe por onde a
            // chamada interna sair pela rede, e é isso que a assertiva de baixo
            // ("roda uma única vez") passa a significar sozinha.
            let conexoes = trava_leitura(&manager.connections);
            let publicada = conexoes
                .get(servidor::CONECTOR)
                .expect("a conexão publicada continua de pé");
            assert!(
                matches!(publicada.canal, Canal::Publicado) && publicada.tarefa.is_none(),
                "a conexão interna não tem caminho de rede"
            );
        }
        assert!(manager.approvals().resolve(&pedido.id, true));
        assert!(
            tokio::time::timeout(Duration::from_secs(1), tarefa)
                .await
                .expect("chamada interna termina")
                .expect("tarefa conclui")
                .is_ok()
        );
        assert!(alvo.exists(), "Execucao roda a ferramenta uma única vez");
        let mut reapareceram = 0;
        while let Ok(evento) = eventos.try_recv() {
            if matches!(evento, McpEvent::ApprovalRequested { .. }) {
                reapareceram += 1;
            }
        }
        assert_eq!(reapareceram, 0, "nenhuma segunda aprovação para a mesma chamada");
    }

    #[derive(Clone)]
    struct ServidorSondo {
        iniciou: Arc<tokio::sync::Notify>,
        cancelou: Arc<std::sync::atomic::AtomicBool>,
    }

    impl rmcp::ServerHandler for ServidorSondo {
        fn get_info(&self) -> rmcp::model::ServerConfig {
            rmcp::model::ServerConfig::new(
                rmcp::model::ServerCapabilities::builder()
                    .enable_tools()
                    .build(),
            )
        }

        async fn call_tool(
            &self,
            request: CallToolRequestParams,
            context: rmcp::service::RequestContext<rmcp::RoleServer>,
        ) -> std::result::Result<rmcp::model::CallToolResponse, rmcp::ErrorData> {
            if request.name.as_ref() == "pendurar" {
                self.iniciou.notify_one();
                tokio::select! {
                    _ = context.ct.cancelled() => {
                        self.cancelou
                            .store(true, std::sync::atomic::Ordering::SeqCst);
                        Err(rmcp::ErrorData::internal_error("cancelado", None))
                    }
                    _ = std::future::pending::<()>() => unreachable!(),
                }
            } else {
                Ok(CallToolResult::success(vec![ContentBlock::text("ok")]).into())
            }
        }
    }

    #[tokio::test]
    async fn resposta_remota_cancelada_nao_derruba_a_conexao() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use tokio::sync::Notify;
        use tokio_util::sync::CancellationToken;

        let iniciou = Arc::new(Notify::new());
        let cancelou = Arc::new(AtomicBool::new(false));
        let (servidor_io, cliente_io) = tokio::io::duplex(8192);
        let servidor = {
            let iniciou = Arc::clone(&iniciou);
            let cancelou = Arc::clone(&cancelou);
            tokio::spawn(async move {
                let running = rmcp::service::serve_directly::<rmcp::RoleServer, _, _, _, _>(
                    ServidorSondo { iniciou, cancelou },
                    servidor_io,
                    None,
                );
                running.waiting().await.expect("servidor direto encerra");
            })
        };
        let cliente = rmcp::service::serve_directly::<RoleClient, _, _, _, _>((), cliente_io, None);
        let (sender, receptor) = mpsc::channel(4);
        let desligamento = CancellationToken::new();
        let conexao = tokio::spawn(run_connection(cliente, receptor, desligamento.clone()));
        let cancel = CancellationToken::new();
        let (reply_tx, reply_rx) = oneshot::channel();
        sender
            .send(ConnectionCommand::Call {
                params: CallToolRequestParams::new("pendurar"),
                reply: reply_tx,
                cancel: cancel.clone(),
            })
            .await
            .expect("comando na fila");
        tokio::time::timeout(Duration::from_secs(1), iniciou.notified())
            .await
            .expect("servidor recebeu a chamada");
        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(1), reply_rx)
            .await
            .expect("resposta cancelada não fica pendurada");
        tokio::time::timeout(Duration::from_secs(1), async {
            while !cancelou.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("notifications/cancelled chega ao servidor");

        let (reply_tx, reply_rx) = oneshot::channel();
        sender
            .send(ConnectionCommand::Call {
                params: CallToolRequestParams::new("responder"),
                reply: reply_tx,
                cancel: CancellationToken::new(),
            })
            .await
            .expect("próxima chamada na mesma conexão");
        let resposta = tokio::time::timeout(Duration::from_secs(1), reply_rx)
            .await
            .expect("conexão continua responsiva")
            .expect("oneshot aberto")
            .expect("segunda chamada funciona");
        assert_eq!(resposta.is_error, Some(false));

        desligamento.cancel();
        tokio::time::timeout(Duration::from_secs(1), conexao)
            .await
            .expect("conexão encerra")
            .expect("sem pânico");
        tokio::time::timeout(Duration::from_secs(1), servidor)
            .await
            .expect("servidor encerra")
            .expect("sem pânico");
    }

    #[tokio::test]
    async fn disconnect_interrompe_a_chamada_ativa_e_encerra_o_cliente() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use tokio::sync::Notify;

        let (manager, _) = manager_conectado(crate::permissions::AgentMode::FullAccess).await;
        let anunciadas = manager.anunciadas(local::CONECTOR);
        let iniciou = Arc::new(Notify::new());
        let cancelou = Arc::new(AtomicBool::new(false));
        let (servidor_io, cliente_io) = tokio::io::duplex(8192);
        let servidor = {
            let iniciou = Arc::clone(&iniciou);
            let cancelou = Arc::clone(&cancelou);
            tokio::spawn(async move {
                let running = rmcp::service::serve_directly::<rmcp::RoleServer, _, _, _, _>(
                    ServidorSondo { iniciou, cancelou },
                    servidor_io,
                    None,
                );
                running.waiting().await.expect("servidor direto encerra");
            })
        };
        let cliente = rmcp::service::serve_directly::<RoleClient, _, _, _, _>((), cliente_io, None);
        let (sender, receptor) = mpsc::channel(4);
        let desligamento = CancellationToken::new();
        let tarefa = tokio::spawn(run_connection(cliente, receptor, desligamento.clone()));
        trava_escrita(&manager.connections).insert(
            local::CONECTOR.to_string(),
            Connection {
                canal: Canal::Remoto(sender),
                cliente_descoberta: None,
                anunciadas,
                desligamento,
                tarefa: Some(tarefa),
            },
        );

        let chamada = {
            let manager = Arc::clone(&manager);
            tokio::spawn(async move {
                manager
                    .call_tool(
                        &format!("{}__pendurar", local::CONECTOR),
                        serde_json::json!({}),
                    )
                    .await
            })
        };
        tokio::time::timeout(Duration::from_secs(1), iniciou.notified())
            .await
            .expect("servidor recebeu a chamada ativa");

        tokio::time::timeout(Duration::from_secs(1), manager.disconnect(local::CONECTOR))
            .await
            .expect("disconnect não fica atrás de call_tool")
            .expect("conexão é removida");
        let erro = tokio::time::timeout(Duration::from_secs(1), chamada)
            .await
            .expect("chamada ativa acorda")
            .expect("tarefa conclui")
            .expect_err("desconexão derruba a chamada");
        assert!(
            erro.to_string().contains(&tr(
                "erros.servidorDerrubouChamada",
                &[("id", local::CONECTOR)]
            )),
            "{erro:#}"
        );
        assert!(
            !manager
                .connected_ids()
                .contains(&local::CONECTOR.to_string())
        );
        tokio::time::timeout(Duration::from_secs(1), async {
            while !cancelou.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("o cliente cancela a chamada no servidor");
        tokio::time::timeout(Duration::from_secs(1), servidor)
            .await
            .expect("cliente/canal do servidor encerram")
            .expect("sem pânico");
    }
}
