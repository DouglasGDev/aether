//! O servidor MCP que o próprio aplicativo publica.
//!
//! As ferramentas do Aether — arquivos, terminal, memória, sistema — são um
//! catálogo que até aqui atendia só o laço de conversa. Publicá-las como servidor
//! MCP é pô-las no protocolo de verdade: um cliente chama `tools/list`, recebe o
//! catálogo no idioma atual; chama `tools/call`, e a chamada atravessa o mesmo
//! gate e a mesma fila de aprovação de qualquer servidor. Não há segundo conjunto
//! de ferramentas nem atalho: o que o modelo já tem é o que o servidor anuncia.
//!
//! O servidor roda dentro do aplicativo, na placa de retorno, e nunca como
//! processo filho. Um `aether.exe` levantado por outro programa executaria
//! escrita de arquivo e terminal sem ninguém aprovar, porque o gate e os cards de
//! aprovação vivem aqui dentro. Num canto do protocolo isso é visível: a placa só
//! escuta o loopback, cada execução sorteia um token novo e o cadastro guarda
//! `${AETHER_MCP_TOKEN}` — o segredo fica na memória, nunca no `mcp.json`.

use super::execucao::{Execucao, resultado_local};
use crate::bancos;
use crate::connectors::{Connector, ConnectorCategory, ConnectorKind, McpAuth, McpEntry};
use crate::idiomas::tr;
use crate::local::{self, Ferramenta};
use crate::permissions::ToolEffect;
use anyhow::{Context, Result, anyhow};
use bytes::Bytes;
use http::header::{ALLOW, AUTHORIZATION};
use http::{HeaderMap, HeaderName, HeaderValue, Request, Response, StatusCode};
use http_body_util::{Empty, combinators::BoxBody};
use hyper::body::Incoming;
use hyper::server::conn::http1::Builder;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
    ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService,
};
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// Identificador do conector que este servidor publica. É o nome que aparece no
/// cadastro e o prefixo das ferramentas qualificadas (`aether__ler_arquivo`).
pub const CONECTOR: &str = "aether";

/// A variável que o `mcp.json` referencia no lugar do token. O valor real só
/// existe na memória do aplicativo enquanto o servidor estiver de pé.
pub const VARIAVEL_TOKEN: &str = "AETHER_MCP_TOKEN";

/// A ferramenta que o agente chama para abrir esta conexão. Ela não roda em
/// `local::executar`: publicar um servidor e registrar o conector é trabalho do
/// gerente de conexões, que é quem tem o cadastro, o arquivo e o cliente MCP.
pub const FERRAMENTA: &str = "criar_mcp_local";

/// O estado publicado para a tela: o endereço, a porta e quantas ferramentas o
/// servidor anuncia. O token não entra aqui — ele se copia, não se lê na tela.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Estado {
    pub ativa: bool,
    pub url: String,
    pub porta: u16,
    pub ferramentas: usize,
}

impl Estado {
    fn inativa(ferramentas: usize) -> Self {
        Self {
            ativa: false,
            url: String::new(),
            porta: 0,
            ferramentas,
        }
    }
}

/// O que está de pé agora: o endereço, o segredo, o cancelador e a tarefa que
/// aceita as conexões. Guardar o `JoinHandle` é o que permite fechar o servidor
/// de verdade em vez de abandonar a porta aberta.
struct Ativa {
    estado: Estado,
    token: Arc<String>,
    cancelamento: CancellationToken,
    tarefa: JoinHandle<()>,
}

pub struct Publicador {
    execucao: Arc<Execucao>,
    ativa: Mutex<Option<Ativa>>,
}

impl Publicador {
    pub fn novo(execucao: Arc<Execucao>) -> Self {
        Self {
            execucao,
            ativa: Mutex::new(None),
        }
    }

    /// O estado de agora, sem levantar nada. É o que a tela lê a cada aberto.
    pub fn estado(&self) -> Estado {
        self.ativa
            .lock()
            .expect("publicador envenenado")
            .as_ref()
            .map(|ativa| ativa.estado.clone())
            .unwrap_or_else(|| Estado::inativa(self.ferramentas()))
    }

    /// Quantas ferramentas o servidor anuncia agora: o catálogo já recortado
    /// pelo grafo. É este número que a tela mostra, e ele tem de bater com o que
    /// um `tools/list` devolve — contar o catálogo inteiro diria "ligada" uma
    /// ferramenta que o usuário desligou.
    fn ferramentas(&self) -> usize {
        catalogo_publicado(&self.execucao).len()
    }

    /// O token em memória, para quem precisa apresentá-lo a um cliente externo.
    /// `None` com o servidor fechado: sem servidor não há segredo.
    pub fn token(&self) -> Option<String> {
        self.ativa
            .lock()
            .expect("publicador envenenado")
            .as_ref()
            .map(|ativa| ativa.token.as_str().to_string())
    }

    /// Levanta o servidor na placa de retorno e devolve o endereço com o token.
    /// Idempotente por construção: pedir de pé é responder com o que já está de
    /// pé, não abrir uma segunda porta nem trocar o segredo no meio do caminho.
    pub async fn abrir(&self) -> Result<(Estado, String)> {
        if let Some(ativa) = self.ativa.lock().expect("publicador envenenado").as_ref() {
            return Ok((ativa.estado.clone(), ativa.token.as_str().to_string()));
        }

        let token = Arc::new(token_novo()?);
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .map_err(|e| anyhow!(tr("erros.servidorMcpOuvir", &[("detalhe", &e.to_string())])))?;
        let porta = listener
            .local_addr()
            .context(tr("erros.servidorMcpPorta", &[]))?
            .port();

        let cancelamento = CancellationToken::new();
        let execucao = Arc::clone(&self.execucao);
        // A resposta em JSON simples é o que este servidor sabe dar: ele nunca
        // empurra nada para o cliente no meio de uma chamada, não há por que
        // abrir um stream de eventos. Sem sessão guardada, porque o protocolo de
        // 2026 dispensa a mão apertada com estado.
        let mut configuracao = StreamableHttpServerConfig::default();
        configuracao.json_response = true;
        configuracao.legacy_session_mode = false;
        configuracao.cancellation_token = cancelamento.clone();
        let servico = StreamableHttpService::new(
            move || Ok(Manipulador { execucao: Arc::clone(&execucao) }),
            Arc::new(LocalSessionManager::default()),
            configuracao,
        );

        let tarefa = tokio::spawn(aceitar(
            listener,
            servico,
            Arc::clone(&token),
            cancelamento.clone(),
        ));

        let estado = Estado {
            ativa: true,
            url: format!("http://127.0.0.1:{porta}/mcp"),
            porta,
            ferramentas: self.ferramentas(),
        };
        *self.ativa.lock().expect("publicador envenenado") = Some(Ativa {
            estado: estado.clone(),
            token: Arc::clone(&token),
            cancelamento,
            tarefa,
        });
        Ok((estado, token.as_str().to_string()))
    }

    /// Derruba o servidor: cancela o laço de aceitação, espera a tarefa sair e só
    /// então solta a porta. Devolver antes disso deixaria um ouvinte vivendo de
    /// um token que a tela já não conhece.
    pub async fn fechar(&self) -> Result<bool> {
        let antiga = self.ativa.lock().expect("publicador envenenado").take();
        let Some(antiga) = antiga else {
            return Ok(false);
        };
        antiga.cancelamento.cancel();
        if antiga.tarefa.await.is_err() {
            return Err(anyhow!(tr("erros.servidorMcpParar", &[])));
        }
        Ok(true)
    }
}

/// A ferramenta que o agente chama. Sem argumentos: publicar é uma decisão, não
/// uma configuração — o que entra no servidor é o catálogo que já existe.
pub fn ferramenta() -> Ferramenta {
    Ferramenta {
        nome: FERRAMENTA,
        titulo: tr("ferramentas.criarMcpLocal.titulo", &[]),
        descricao: tr("ferramentas.criarMcpLocal.descricao", &[]),
        efeito: ToolEffect::Mutating,
        esquema: Value::Object(Map::from_iter([(
            "type".to_string(),
            Value::String("object".to_string()),
        )])),
    }
}

/// O cadastro do servidor publicado. O endereço é o que a placa abriu agora; o
/// cabeçalho guarda a variável, nunca o segredo.
pub fn conector(estado: &Estado) -> Connector {
    let mut cabecalhos = HashMap::new();
    cabecalhos.insert(
        "Authorization".to_string(),
        format!("Bearer ${{{VARIAVEL_TOKEN}}}"),
    );
    Connector {
        id: CONECTOR.to_string(),
        name: tr("servidorMcp.nome", &[]),
        description: tr("servidorMcp.descricao", &[]),
        kind: ConnectorKind::Mcp,
        category: ConnectorCategory::System,
        docs_url: String::new(),
        routes: vec![
            "mcp".into(),
            "local".into(),
            "ferramentas".into(),
            "arquivos".into(),
            "terminal".into(),
        ],
        auth: McpAuth::Token,
        entry: Some(McpEntry::Http {
            url: estado.url.clone(),
            headers: cabecalhos,
        }),
        local: HashMap::new(),
        fields: vec![],
        env: vec![],
        tool_allowlist: None,
    }
}

/// Nome e descrição do conector publicado, no idioma da vez. O arquivo guarda o
/// endereço, que é sorteado a cada execução; o texto legível vem sempre dos
/// idiomas, pela mesma razão de um servidor do catálogo.
pub fn relocalizar(conector: &mut Connector) {
    conector.name = tr("servidorMcp.nome", &[]);
    conector.description = tr("servidorMcp.descricao", &[]);
}

/// O que o servidor fala: as ferramentas locais, no idioma da vez, com o efeito
/// declarado pela tabela do aplicativo — não por anotação de terceiro.
#[derive(Clone)]
struct Manipulador {
    execucao: Arc<Execucao>,
}

impl ServerHandler for Manipulador {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("aether", env!("CARGO_PKG_VERSION")))
            .with_instructions(tr("servidorMcp.instrucoes", &[]))
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = std::result::Result<ListToolsResult, ErrorData>>
    + Send
    + '_ {
        async move {
            Ok(ListToolsResult::with_all_items(ferramentas_anunciadas(
                &self.execucao,
            )))
        }
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = std::result::Result<CallToolResponse, ErrorData>>
    + Send
    + '_ {
        async move {
            let nome = request.name.to_string();
            let argumentos = match request.arguments {
                Some(mapa) => Value::Object(mapa),
                None => Value::Object(Map::new()),
            };
            // O efeito vem do catálogo, e a resposta de um nome que não está nele
            // diz quais são as ferramentas: a mesma regra do laço de conversa. O
            // catálogo é o recorte do grafo, não o catálogo interno: uma
            // ferramenta desligada na tela não está aqui, e chamar o que não
            // existe é o erro que um `FullAccess` do cliente não reabre.
            let publicadas = catalogo_publicado(&self.execucao);
            let efeito = publicadas
                .iter()
                .find(|ferramenta| ferramenta.nome == nome)
                .map(|ferramenta| ferramenta.efeito);
            let Some(efeito) = efeito else {
                let lista = publicadas
                    .into_iter()
                    .map(|t| format!("{CONECTOR}__{}", t.nome))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Ok(CallToolResult::error(vec![ContentBlock::text(tr(
                    "erros.ferramentaInexistente",
                    &[("nome", &format!("{CONECTOR}__{nome}")), ("lista", &lista)],
                ))])
                .into());
            };
            let saida = match self
                .execucao
                .ferramenta(CONECTOR, &nome, efeito, argumentos)
                .await
            {
                Ok(saida) => saida,
                // A recusa do gate é uma resposta da ferramenta, não uma falha do
                // protocolo: é assim que quem chama lê o motivo — do mesmo jeito
                // que lê no laço de conversa.
                Err(e) => {
                    return Ok(
                        CallToolResult::error(vec![ContentBlock::text(e.to_string())]).into(),
                    );
                }
            };
            Ok(resultado_local(saida).into())
        }
    }
}

/// O catálogo que este servidor anuncia: as ferramentas locais e as de banco de
/// dados, no idioma da vez, passadas pela allowlist do grafo. `criar_mcp_local`
/// fica de fora — dentro do servidor publicado a conexão já estaria de pé, e
/// anunciá-la seria um convite para abrir a porta outra vez.
///
/// O recorte é o do conector embutido, dono dessas ferramentas: a mesma lista
/// que o laço de conversa recebe. Um cliente na placa de retorno é uma segunda
/// mão pedindo a mesma capacidade, não um segundo cadastro.
fn catalogo_publicado(execucao: &Execucao) -> Vec<Ferramenta> {
    local::catalogo()
        .into_iter()
        .chain(bancos::ferramentas::catalogo())
        .filter(|ferramenta| execucao.permite_local(&ferramenta.nome))
        .collect()
}

/// O catálogo como ferramenta MCP. As anotações vão porque um cliente precisa
/// poder dizer leitura de escrita; a decisão do gate, aqui dentro, não usa elas.
fn ferramentas_anunciadas(execucao: &Execucao) -> Vec<Tool> {
    catalogo_publicado(execucao)
        .into_iter()
        .map(|ferramenta| {
            let mut anotacoes = ToolAnnotations::default();
            anotacoes.title = Some(ferramenta.titulo.clone());
            match ferramenta.efeito {
                ToolEffect::ReadOnly => anotacoes.read_only_hint = Some(true),
                ToolEffect::Destructive => anotacoes.destructive_hint = Some(true),
                ToolEffect::Mutating => {}
            }
            let esquema = match ferramenta.esquema {
                Value::Object(mapa) => Arc::new(mapa),
                _ => Arc::new(Map::new()),
            };
            let mut anunciada = Tool::new(ferramenta.nome, ferramenta.descricao, esquema)
                .with_title(ferramenta.titulo);
            anunciada.annotations = Some(anotacoes);
            anunciada
        })
        .collect()
}

/// Trinta e dois bytes do gerador aleatório do sistema, em hexadecimal. O token
/// é longo de propósito: é a única coisa entre um estranho na rede e um `apagar`
/// na máquina do usuário.
fn token_novo() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|e| anyhow!(tr("erros.servidorMcpToken", &[("detalhe", &e.to_string())])))?;
    Ok(bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>())
}

type Servico = StreamableHttpService<Manipulador, LocalSessionManager>;

/// O teto da despedida. `fechar` é chamado pela tela e pela saída do aplicativo,
/// e a saída tem cinco segundos para o gerente inteiro (`lib.rs:252`): os dois
/// segundos de cortesia abaixo cabem nesse prazo de fora com folga, e o que
/// sobrar é cortado antes de estourá-lo.
const PRAZO_DRENO: Duration = Duration::from_secs(2);

/// O laço de aceitação. Uma conexão por tarefa: um cliente que trava no meio de
/// um `tools/call` não pode levar a porta junto. E uma tarefa por conexão não é
/// uma tarefa sem dono: o conjunto é esperado antes do laço devolver, porque
/// `fechar` promete que o servidor publicado caiu — a porta e as conexões que já
/// estavam abertas.
async fn aceitar(
    listener: TcpListener,
    base: Servico,
    token: Arc<String>,
    cancelamento: CancellationToken,
) {
    let mut conexoes: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _ = cancelamento.cancelled() => break,
            aceito = listener.accept() => {
                conectar(&mut conexoes, aceito, &base, &token, &cancelamento);
            }
            // As que se serviram saem do conjunto enquanto o laço continua: sem
            // isto a lista só cresceria até o fecho.
            terminou = conexoes.join_next(), if !conexoes.is_empty() => {
                let _ = terminou;
            }
        }
    }
    drenar(&mut conexoes).await;
}

/// Um soquete aceito vira uma tarefa dentro do conjunto, e o conjunto é do laço.
/// Uma conexão por tarefa porque um cliente que trava no meio de um `tools/call`
/// não pode levar a porta junto; a tarefa registrada porque nenhuma delas pode
/// sobreviver ao `fechar`.
fn conectar(
    conexoes: &mut tokio::task::JoinSet<()>,
    aceito: std::io::Result<(TcpStream, SocketAddr)>,
    base: &Servico,
    token: &Arc<String>,
    cancelamento: &CancellationToken,
) {
    let Ok((soquete, _remoto)) = aceito else {
        // Um erro de aceitação é um pacote fora do lugar, não o fim do servidor:
        // quem ouve a placa continua ouvindo.
        return;
    };
    let um = base.clone();
    let esperado = Arc::clone(token);
    let parada = cancelamento.clone();
    conexoes.spawn(async move {
        let io = TokioIo::new(soquete);
        // A conexão cai junto com o servidor, sem esperar que o cliente pendure
        // a soquete: um keep-alive parado no meio do pedido não é razão para o
        // fecho não acontecer.
        let _ = tokio::select! {
            servido = Builder::new().serve_connection(
                io,
                service_fn(move |pedido: Request<Incoming>| {
                    // O serviço é clonado a cada pedido, como o próprio rmcp
                    // faz: é um `Service` de estado compartilhado (fábrica de
                    // manipuladores, gerente de sessões, configuração), e um
                    // fechamento não pode devolver uma futura que empresta o
                    // que ele captura.
                    let mut um = um.clone();
                    let esperado = Arc::clone(&esperado);
                    async move {
                        // O token é conferido antes de se ler o corpo: a um
                        // pedido negado o servidor nem olha o que lhe mandaram.
                        if !autorizado(&esperado, pedido.headers()) {
                            return Ok::<_, Infallible>(negado());
                        }
                        <Servico as tower_service::Service<
                            Request<Incoming>,
                        >>::call(&mut um, pedido)
                        .await
                    }
                }),
            ) => servido,
            _ = parada.cancelled() => Ok(()),
        };
    });
}

/// Espera as conexões em curso sem prometer esperar por sempre. Cai fora do
/// prazo, corta o que sobrou e pronto — o `fechar` do aplicativo não fica
/// refém de um cliente mudo do outro lado da loopback.
async fn drenar(conexoes: &mut tokio::task::JoinSet<()>) {
    let coube = tokio::time::timeout(PRAZO_DRENO, async {
        while conexoes.join_next().await.is_some() {}
    })
    .await;
    if coube.is_err() {
        conexoes.shutdown().await;
    }
}

fn autorizado(esperado: &str, cabecalhos: &HeaderMap) -> bool {
    let Some(valor) = cabecalhos.get(AUTHORIZATION).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let algum = valor
        .strip_prefix("Bearer ")
        .or_else(|| valor.strip_prefix("bearer "))
        .unwrap_or("");
    mesmo_sigilo(esperado.as_bytes(), algum.as_bytes())
}

/// Comparação sem ramo dependente do conteúdo: um token de endpoint não pode
/// virar oracle de timing para quem chuta byte por byte. Um esperado vazio nega
/// tudo — sem servidor de pé não há segredo que autorize ninguém.
fn mesmo_sigilo(esperado: &[u8], pedido: &[u8]) -> bool {
    if esperado.is_empty() || esperado.len() != pedido.len() {
        return false;
    }
    esperado
        .iter()
        .zip(pedido.iter())
        .fold(0u8, |acumulado, (a, b)| acumulado | (a ^ b))
        == 0
}

fn negado() -> Response<BoxBody<Bytes, Infallible>> {
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header(ALLOW, "POST")
        .body(BoxBody::new(Empty::new()))
        .expect("resposta 401 é sempre construtível")
}

/// O cabeçalho que autentica um cliente no endpoint publicado.
pub fn cabecalho_autenticacao(token: &str) -> Result<(HeaderName, HeaderValue)> {
    let valor = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| {
        anyhow!(tr("erros.valorCabecalhoInvalido", &[("nome", "Authorization")]))
    })?;
    Ok((AUTHORIZATION, valor))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::Registry;
    use crate::local::pastas::Pastas;
    use crate::permissions::{AgentMode, PermissionGate};
    use rmcp::ServiceExt;
    use rmcp::transport::StreamableHttpClientTransport;
    use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
    use std::path::PathBuf;
    use std::sync::RwLock;

    /// Um publicador com gate de verdade e pasta concedida de verdade: o mesmo
    /// objeto que roda no aplicativo, montado com o mínimo de arranjo.
    fn publicador(modo: AgentMode) -> (Publicador, PathBuf) {
        publicador_com_grafo(modo, None)
    }

    /// O mesmo arranjo com a allowlist do grafo no lugar: é o que o cadastro do
    /// `computador` carrega depois de alguém desligar uma ferramenta na tela, e
    /// é ela que o servidor publicado tem de obedecer.
    fn publicador_com_grafo(
        modo: AgentMode,
        allowlist: Option<Vec<String>>,
    ) -> (Publicador, PathBuf) {
        let (eventos, _) = tokio::sync::broadcast::channel(4);
        let raiz = std::env::temp_dir().join(format!("aether-servidor-{}", std::process::id()));
        std::fs::create_dir_all(&raiz).expect("pasta de teste");
        let pastas = Arc::new(RwLock::new(Pastas {
            raizes: vec![raiz.clone()],
        }));
        let mut embutido = crate::local::conector();
        embutido.tool_allowlist = allowlist;
        let registry = Arc::new(RwLock::new(
            Registry::new(vec![embutido]).expect("o conector embutido é registrável"),
        ));
        let execucao = Execucao::nova(
            Arc::new(Mutex::new(PermissionGate::new(modo))),
            registry,
            eventos,
            Arc::clone(&pastas),
            Arc::new(crate::ai::memoria::Custodio::novo(
                &[],
                &raiz.join("modelos"),
                &raiz.join("memoria"),
                raiz.join("rag.json"),
                Arc::clone(&pastas),
            )),
            Arc::new(raiz.join("bancos.json")),
            Arc::new(raiz.join("resultados")),
        );
        (Publicador::novo(execucao), raiz)
    }

    type Cliente = rmcp::service::RunningService<rmcp::RoleClient, ()>;

    /// A mão apertada do cliente que este aplicativo já usa para conectar em
    /// qualquer servidor HTTP: a prova é pelo protocolo, não por chamada interna.
    async fn conectar(url: &str, token: &str) -> Result<Cliente> {
        let config =
            StreamableHttpClientTransportConfig::with_uri(url.to_string()).custom_headers(
                HashMap::from([cabecalho_autenticacao(token).expect("cabeçalho")]),
            );
        ()
            .serve(StreamableHttpClientTransport::from_config(config))
            .await
            .map_err(|e| anyhow!("a mão apertada falhou: {e:?}"))
    }

    #[test]
    fn o_estado_inativo_mostra_o_catalogo_inteiro_sem_endereco() {
        let (publicador, _) = publicador(AgentMode::FullAccess);
        let estado = publicador.estado();
        assert!(!estado.ativa);
        assert_eq!(estado.url, "");
        assert_eq!(estado.porta, 0);
        assert_eq!(
            estado.ferramentas,
            catalogo_publicado(&publicador.execucao).len()
        );
        assert!(
            estado.ferramentas > local::catalogo().len(),
            "as ferramentas de banco entram na contagem publicada"
        );
    }

    /// Fechado ou aberto, o número da tela é o `tools/list` que um cliente
    /// recebe. Contar o catálogo interno diria "ligada" uma ferramenta que o
    /// grafo desligou.
    #[test]
    fn a_contagem_do_estado_e_o_recorte_do_grafo() {
        let (publicador, _) = publicador_com_grafo(
            AgentMode::FullAccess,
            Some(vec!["ler_arquivo".into(), "info_sistema".into()]),
        );
        assert_eq!(publicador.estado().ferramentas, 2, "inativo também conta o recorte");
    }

    #[test]
    fn o_cadastro_referencia_o_token_por_variavel_nunca_por_valor() {
        let estado = Estado {
            ativa: true,
            url: "http://127.0.0.1:4599/mcp".to_string(),
            porta: 4599,
            ferramentas: 11,
        };
        let cadastrado = conector(&estado);
        let json = serde_json::to_string(&cadastrado).expect("conector serializa");
        assert!(json.contains("${AETHER_MCP_TOKEN}"), "{json}");
        assert!(json.contains("4599/mcp"), "o endereço vai no cadastro: {json}");
        assert!(!json.contains("Bearer segredo"), "{json}");
        assert_eq!(cadastrado.auth, McpAuth::Token);
        assert_eq!(cadastrado.kind, ConnectorKind::Mcp);
    }

    #[test]
    fn o_sigilo_compara_o_tamanho_antes_e_sem_antecipar() {
        assert!(mesmo_sigilo(b"abc123", b"abc123"));
        assert!(!mesmo_sigilo(b"abc123", b"abc124"));
        assert!(!mesmo_sigilo(b"abc123", b"abc12"));
        assert!(!mesmo_sigilo(b"abc123", b"abc1234"));
        assert!(!mesmo_sigilo(b"", b""), "sem token não se autoriza ninguém");
    }

    #[test]
    fn so_o_bearer_certificado_passa() {
        assert!(!autorizado("segredo", &HeaderMap::new()));
        let mut com_token = HeaderMap::new();
        com_token.insert(AUTHORIZATION, HeaderValue::from_static("Bearer segredo"));
        assert!(autorizado("segredo", &com_token));
        let mut outro = HeaderMap::new();
        outro.insert(AUTHORIZATION, HeaderValue::from_static("Bearer outro"));
        assert!(!autorizado("segredo", &outro));
        let mut sem_esquema = HeaderMap::new();
        sem_esquema.insert(AUTHORIZATION, HeaderValue::from_static("segredo"));
        assert!(!autorizado("segredo", &sem_esquema));
    }

    #[tokio::test]
    async fn abrir_e_idempotente_e_fechar_solta_a_porta() {
        let (publicador, _) = publicador(AgentMode::FullAccess);
        let (primeiro, token) = publicador.abrir().await.expect("abre");
        let (de_novo, outro_token) = publicador.abrir().await.expect("abre de novo");
        assert_eq!(primeiro, de_novo, "abrir duas vezes não muda a porta");
        assert_eq!(token, outro_token, "abrir duas vezes não troca o segredo");
        assert!(de_novo.ativa);

        assert!(publicador.fechar().await.expect("fecha"));
        assert!(!publicador.estado().ativa);
        assert!(publicador.token().is_none());
        assert!(
            !publicador.fechar().await.expect("fecha de novo"),
            "não havia o que fechar"
        );
    }

    /// `fechar` promete a porta solta e nenhum trabalho em curso. A aceitação
    /// esperava a si mesma, mas cada soquete era uma tarefa órfã: aberta antes do
    /// fecho, ela sobrevivia e continuava atendendo pela mesma conexão. A prova é
    /// pelo que o cliente lê depois do `fechar`, não pelo que o laço devolve.
    #[tokio::test]
    async fn fechar_corta_a_conexao_que_ja_estava_aberta() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (publicador, _) = publicador(AgentMode::FullAccess);
        let (estado, token) = publicador.abrir().await.expect("abre");
        let mut soquete = tokio::net::TcpStream::connect(("127.0.0.1", estado.porta))
            .await
            .expect("soquete na placa publicada");
        // Um pedido completo, com a linha em branco que o fecha. A resposta é a
        // prova de que a aceitação viu esta soquete antes do fecho: sem ela, o
        // teste poderia passar porque a conexão nunca chegou ao servidor.
        soquete
            .write_all(
                format!(
                    "GET /mcp HTTP/1.1\r\nhost: 127.0.0.1\r\nauthorization: Bearer {token}\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .expect("escreve o pedido");
        let mut corpo = [0u8; 256];
        let respondido = soquete
            .read(&mut corpo)
            .await
            .expect("o servidor responde ao pedido");
        assert!(
            respondido > 0,
            "a conexão não foi aceita: nada respondeu antes do fecho"
        );

        // E ela continua de pé depois da resposta — é uma conexão aberta,
        // não um pedido descartado. O que ainda vier é corpo, e corpo não conta
        // como queda.
        let mut caiu = false;
        loop {
            match tokio::time::timeout(Duration::from_millis(100), soquete.read(&mut corpo)).await
            {
                Err(_) => break,
                Ok(Ok(0)) => {
                    caiu = true;
                    break;
                }
                Ok(Ok(_)) => continue,
                Ok(Err(_)) => {
                    caiu = true;
                    break;
                }
            }
        }
        assert!(!caiu, "a conexão caiu antes de fechar: não havia o que drenar");

        publicador.fechar().await.expect("fecha");

        match tokio::time::timeout(Duration::from_secs(2), soquete.read(&mut corpo)).await {
            Err(_) => panic!(
                "fechar devolveu com a conexão em curso viva: nada chegou e nada foi fechado"
            ),
            Ok(Ok(0)) => {}
            Ok(Ok(n)) => panic!("fechar e a conexão ainda entregou {n} bytes"),
            // Um reset também é o servidor tendo soltado a soquete.
            Ok(Err(_)) => {}
        }
    }

    #[tokio::test]
    async fn o_token_de_cada_execucao_e_diferente_e_tem_sessenta_e_quatro_hexadecimais() {
        let um = token_novo().expect("token");
        let dois = token_novo().expect("token");
        assert_eq!(um.len(), 64);
        assert!(um.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(um, dois);
    }

    #[tokio::test]
    async fn o_cliente_do_aplicativo_descobre_e_roda_uma_ferramenta() {
        let (publicador, _) = publicador(AgentMode::FullAccess);
        let (estado, token) = publicador.abrir().await.expect("abre");
        let cliente = conectar(&estado.url, &token).await.expect("conecta");

        let anunciadas = cliente.list_all_tools().await.expect("tools/list responde");
        assert_eq!(
            anunciadas.len(),
            estado.ferramentas,
            "o servidor anuncia o catálogo todo"
        );
        let nomes: Vec<&str> = anunciadas.iter().map(|t| t.name.as_ref()).collect();
        assert!(nomes.contains(&"ler_arquivo"), "{nomes:?}");
        assert!(nomes.contains(&"executar_comando"), "{nomes:?}");
        assert!(nomes.contains(&"bancos_listar"), "{nomes:?}");
        assert!(
            !nomes.contains(&super::FERRAMENTA),
            "publicar o servidor dentro dele mesmo não é anúncio"
        );

        let resposta = cliente
            .call_tool(CallToolRequestParams::new("info_sistema"))
            .await
            .expect("tools/call responde");
        let texto = resposta
            .content
            .iter()
            .filter_map(|bloco| match bloco {
                ContentBlock::Text(texto) => Some(texto.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        assert!(!texto.is_empty(), "a ferramenta respondeu vazia");
        assert_eq!(resposta.is_error, Some(false), "{texto}");
        cliente.cancel().await.ok();
        publicador.fechar().await.expect("fecha");
    }

    #[tokio::test]
    async fn sem_o_token_nao_ha_mao_apertada() {
        let (publicador, _) = publicador(AgentMode::FullAccess);
        let (estado, _) = publicador.abrir().await.expect("abre");
        assert!(
            conectar(&estado.url, "um-token-que-ninguem-emitiu")
                .await
                .is_err(),
            "um estranho não conecta na placa de retorno"
        );
        publicador.fechar().await.expect("fecha");
    }

    /// O gate é o mesmo do laço de conversa, e ele nega antes de a ferramenta
    /// rodar: no modo Plan uma escrita não chega ao disco.
    #[tokio::test]
    async fn no_plan_a_escrita_publicada_nao_roda() {
        let (publicador, raiz) = publicador(AgentMode::Plan);
        let (estado, token) = publicador.abrir().await.expect("abre");
        let cliente = conectar(&estado.url, &token).await.expect("conecta");
        let alvo = raiz.join("proibido.txt");
        let resposta = cliente
            .call_tool(
                CallToolRequestParams::new("escrever_arquivo").with_arguments(
                    serde_json::json!({
                        "caminho": alvo.to_string_lossy(),
                        "conteudo": "escrito por um cliente MCP",
                    })
                    .as_object()
                    .cloned()
                    .expect("objeto"),
                ),
            )
            .await
            .expect("o servidor responde");
        assert_eq!(resposta.is_error, Some(true), "Plan recusa a escrita");
        assert!(!alvo.exists(), "recusada é não escrita");
        cliente.cancel().await.ok();
        publicador.fechar().await.expect("fecha");
    }

    /// Desligar uma ferramenta no grafo decide sobre o catálogo, não sobre uma
    /// porta só. O servidor publicado é a segunda porta pelas mesmas mãos: o que
    /// ele anuncia e o que ele aceita é o recorte, nem um nome a mais — e um
    /// `FullAccess` no cliente não reabre o que o usuário fechou na tela.
    #[tokio::test]
    async fn o_recorte_do_grafo_vale_na_placa_publicada() {
        let (publicador, raiz) = publicador_com_grafo(
            AgentMode::FullAccess,
            Some(vec!["ler_arquivo".into(), "info_sistema".into()]),
        );
        let (estado, token) = publicador.abrir().await.expect("abre");
        let cliente = conectar(&estado.url, &token).await.expect("conecta");

        let nomes: Vec<String> = cliente
            .list_all_tools()
            .await
            .expect("tools/list responde")
            .into_iter()
            .map(|ferramenta| ferramenta.name.to_string())
            .collect();
        assert!(nomes.contains(&"ler_arquivo".to_string()), "{nomes:?}");
        assert!(
            !nomes.contains(&"escrever_arquivo".to_string()),
            "desligada no grafo não é anúncio: {nomes:?}"
        );
        assert_eq!(
            estado.ferramentas,
            nomes.len(),
            "a contagem da tela é o que o servidor anuncia de fato"
        );

        let alvo = raiz.join("desligada.txt");
        let _ = std::fs::remove_file(&alvo);
        let resposta = cliente
            .call_tool(
                CallToolRequestParams::new("escrever_arquivo").with_arguments(
                    serde_json::json!({
                        "caminho": alvo.to_string_lossy(),
                        "conteudo": "escrita que o grafo não deixou existir",
                    })
                    .as_object()
                    .cloned()
                    .expect("objeto"),
                ),
            )
            .await
            .expect("o servidor responde");
        assert_eq!(
            resposta.is_error,
            Some(true),
            "o modo do cliente não reabre uma ferramenta fechada no grafo"
        );
        assert!(!alvo.exists(), "desligada é não escrita");
        cliente.cancel().await.ok();
        publicador.fechar().await.expect("fecha");
    }
}
