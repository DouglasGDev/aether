use super::approval::{ApprovalBroker, ApprovalRequest};
use super::catalog::{CatalogBudget, CatalogEntry, render};
use super::execucao::{Execucao, resultado_local};
use super::servidor::{self, Publicador};
use super::tool::ToolInfo;
use crate::bancos;
use crate::config::{McpConfig, Segredos};
use crate::connectors::{ConnectorKind, DesktopOs, McpEntry, Registry, ToolMeta, parse_qualified_name};
use crate::idiomas::tr;
use crate::local::{self, pastas::Pastas};
use crate::permissions::{PermissionGate, ToolEffect};
use crate::state::{trava_estado, trava_escrita, trava_leitura};
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

type Client = RunningService<RoleClient, ()>;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
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
    },
    Shutdown,
}

/// Um servidor MCP é um processo separado, com um canal de comandos. O conector
/// embutido é o próprio aplicativo: não há canal, e a chamada roda aqui dentro.
/// O servidor que o aplicativo publica é um terceiro caso: há canal e há rede,
/// mas a outra ponta somos nós — e é lá que o gate decide, uma única vez.
#[derive(Clone)]
enum Canal {
    Remoto(mpsc::Sender<ConnectionCommand>),
    Publicado(mpsc::Sender<ConnectionCommand>),
    Local,
}

struct Connection {
    canal: Canal,
    /// Tudo que o servidor anunciou, sem filtro. O que o modelo pode usar é o
    /// que `tools` devolve, já passado pela allowlist do cadastro: assim, ligar
    /// ou desligar uma ferramenta no grafo vale na chamada seguinte, sem
    /// precisar derrubar e levantar o processo do servidor.
    anunciadas: Vec<ToolInfo>,
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
            let Some(connection) = connections.get_mut(local::CONECTOR) else { return };
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
            trava_escrita(&self.connections).insert(
                connector_id.to_string(),
                Connection { canal: Canal::Local, anunciadas },
            );
            let _ = self.events.send(McpEvent::ToolsDiscovered {
                connector_id: connector_id.to_string(),
                tools: tools.clone(),
            });
            return Ok(tools);
        }

        let connector = crate::config::resolve_connector(&stored, &self.segredos).with_context(
            || tr("erros.falhaResolverSegredos", &[("id", connector_id)]),
        )?;
        let entry = connector.entry_on(DesktopOs::current()).ok_or_else(|| {
            anyhow!(tr("erros.servidorSemEntrada", &[("id", connector_id)]))
        })?;

        // Um servidor do catálogo que só enxerga pastas recebe as concedidas
        // agora, não quando foi instalado: revogar uma pasta tem de valer já.
        let entry = match super::instalador::pronto(connector_id) {
            Some(pronto) if pronto.escopo == super::instalador::Escopo::PastasConcedidas => {
                let pastas = self.pastas_concedidas();
                super::instalador::com_pastas(pronto, entry.clone(), &pastas)?
            }
            _ => entry.clone(),
        };

        let client = dentro_do_prazo(Self::dial(&entry), PRAZO_CONEXAO, connector_id).await?;
        // A descoberta também tem prazo, e aqui há algo a limpar: o cliente já
        // está de pé, com o processo do servidor vivo atrás dele. Devolver o
        // erro sem fechar a conexão deixaria um filho órfão com a tela dizendo
        // apenas que não conseguiu conectar.
        let descoberta = dentro_do_prazo(
            async { client.list_all_tools().await.context(tr("erros.falhaListarFerramentas", &[])) },
            PRAZO_CONEXAO,
            connector_id,
        )
        .await;
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

        let (sender, receiver) = mpsc::channel(32);
        tokio::spawn(run_connection(client, receiver));

        // Ligarse ao servidor que o próprio aplicativo publica é uma ligação de
        // rede como qualquer outra — com uma diferença: a outra ponta decide.
        let canal = if connector_id == servidor::CONECTOR {
            Canal::Publicado(sender)
        } else {
            Canal::Remoto(sender)
        };

        trava_escrita(&self.connections)
            .insert(connector_id.to_string(), Connection { canal, anunciadas });

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

    pub async fn disconnect(&self, connector_id: &str) -> Result<()> {
        let connection = trava_escrita(&self.connections).remove(connector_id);
        let Some(connection) = connection else {
            bail!(tr("erros.servidorNaoConectado", &[("id", connector_id)]));
        };

        // Revoke before the process dies, so no waiter can resume against it.
        self.execucao.aprovacoes().cancel_connector(connector_id);
        trava_estado(&self.execucao.gate()).flush_connector(connector_id);

        match connection.canal {
            Canal::Remoto(sender) | Canal::Publicado(sender) => {
                let _ = sender.send(ConnectionCommand::Shutdown).await;
            }
            Canal::Local => {}
        }
        let _ = self.events.send(McpEvent::Disconnected {
            connector_id: connector_id.to_string(),
        });
        Ok(())
    }

    pub async fn call_tool(
        &self,
        qualified_name: &str,
        arguments: serde_json::Value,
    ) -> Result<CallToolResult> {
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
                    self.tools(id).into_iter().map(|t| t.qualified_name).collect()
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
                anyhow!(tr("erros.servidorNaoConectado", &[("id", &meta.connector_id)]))
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
                        .aprovar(local::CONECTOR, &meta.tool, effect, &arguments)
                        .await?;
                    return Ok(resultado_publicado(&self.publicar().await?));
                }
                let saida = self
                    .execucao
                    .ferramenta(
                        local::CONECTOR,
                        &meta.tool,
                        effect,
                        serde_json::Value::Object(arguments),
                    )
                    .await?;
                Ok(resultado_local(saida))
            }
            Canal::Remoto(sender) => {
                // A ferramenta roda do outro lado, então a decisão tem de acontecer
                // aqui: é o cliente que sabe o que o usuário permitiu.
                self.execucao
                    .aprovar(&meta.connector_id, &meta.tool, effect, &arguments)
                    .await?;
                self.enviar(sender, &meta, arguments).await
            }
            Canal::Publicado(sender) => {
                // A outra ponta é o nosso próprio servidor, que passa pelo mesmo
                // gate com o mesmo id de conector. Decidir aqui também seria
                // pedir a mesma aprovação duas vezes.
                self.enviar(sender, &meta, arguments).await
            }
        }
    }

    async fn enviar(
        &self,
        sender: mpsc::Sender<ConnectionCommand>,
        meta: &ToolMeta,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<CallToolResult> {
        let params = CallToolRequestParams::new(meta.tool.clone()).with_arguments(arguments);
        let (reply_tx, reply_rx) = oneshot::channel();
        sender
            .send(ConnectionCommand::Call {
                params,
                reply: reply_tx,
            })
            .await
            .map_err(|_| anyhow!(tr("erros.servidorParou", &[("id", &meta.connector_id)])))?;

        reply_rx
            .await
            .map_err(|_| anyhow!(tr("erros.servidorDerrubouChamada", &[("id", &meta.connector_id)])))?
            .map_err(|e| {
                anyhow!(tr("erros.falhaChamarFerramenta", &[("detalhe", &e.to_string())]))
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
        let (estado, token) = self.publicador.abrir().await?;
        self.segredos.guardar(servidor::VARIAVEL_TOKEN, &token);

        let registry = {
            let mut config = trava_estado(&self.config);
            config.upsert(servidor::conector(&estado));
            config.save(&self.config_path)?;
            config.registry()?
        };
        self.replace_registry(registry);

        // Uma ligação velha cai com a porta velha; sem isto o conector ficaria
        // "conectado" apontando para um endereço que já não escuta.
        if trava_leitura(&self.connections).contains_key(servidor::CONECTOR) {
            self.disconnect(servidor::CONECTOR).await.ok();
        }
        self.connect(servidor::CONECTOR).await?;
        Ok(estado)
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
                let transport = TokioChildProcess::new(cmd)
                    .context(tr("erros.falhaIniciarServidor", &[]))?;
                ().serve(transport).await.map_err(|e| {
                    anyhow!(tr("erros.falhaConectarServidor", &[("detalhe", &format!("{e:?}"))]))
                })
            }
            McpEntry::Http { url, headers } => {
                let mut config = StreamableHttpClientTransportConfig::with_uri(url.clone());
                if !headers.is_empty() {
                    let mut parsed = HashMap::new();
                    for (name, value) in headers {
                        let name = HeaderName::from_bytes(name.as_bytes()).with_context(|| {
                            tr("erros.cabecalhoInvalido", &[("nome", name)])
                        })?;
                        let value = HeaderValue::from_str(value).with_context(|| {
                            tr("erros.valorCabecalhoInvalido", &[("nome", name.as_str())])
                        })?;
                        parsed.insert(name, value);
                    }
                    config = config.custom_headers(parsed);
                }
                let transport = StreamableHttpClientTransport::from_config(config);
                ().serve(transport).await.map_err(|e| {
                    anyhow!(tr("erros.falhaConectarServidor", &[("detalhe", &format!("{e:?}"))]))
                })
            }
        }
    }
}

async fn run_connection(client: Client, mut receiver: mpsc::Receiver<ConnectionCommand>) {
    while let Some(command) = receiver.recv().await {
        match command {
            ConnectionCommand::Call { params, reply } => {
                let result = client.call_tool(params).await;
                let _ = reply.send(result);
            }
            ConnectionCommand::Shutdown => break,
        }
    }
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
        assert!(inicio.elapsed() < Duration::from_secs(1), "o prazo não cortou a espera: {:?} para lá do que 50 ms", inicio.elapsed());
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
        let nomes: Vec<String> = ferramentas_locais()
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(
            nomes.len(),
            local::catalogo().len() + bancos::ferramentas::catalogo().len() + 1
        );
        assert!(nomes.contains(&servidor::FERRAMENTA.to_string()), "{nomes:?}");
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
        assert!(!json.contains(servidor::VARIAVEL_TOKEN), "o token não vai no resultado");
        assert_eq!(resultado.is_error, Some(false));
        let vista = resultado.structured_content.expect("o cartão lê a vista");
        assert_eq!(vista["porta"], serde_json::json!(4599));
        assert_eq!(vista["ferramentas"], serde_json::json!(11));
    }
}
