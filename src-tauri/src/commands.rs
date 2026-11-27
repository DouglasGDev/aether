use crate::ai::adaptadores;
use crate::ai::base::{self, Nota};
use crate::ai::catalog::{self, ModelSpec, ModelStatus};
use crate::ai::compactacao;
use crate::ai::download;
use crate::ai::geracao;
use crate::ai::memoria::{self, Custodio, Resposta};
use crate::ai::router::{self, Choice, Request};
use crate::ai::session::{self, SessionEventKind, TurnConfig, TurnOutcome};
use crate::ai::{Alvo, ChatMessage, ToolCall, ToolSpec};
use crate::connectors::Connector;
use crate::conversas;
use crate::idiomas::{self, tr};
use crate::ligacoes;
use crate::local::{self, pastas::Pastas};
use crate::mcp::catalog::CatalogBudget;
use crate::mcp::instalador::{self, ServidorEvent};
use crate::mcp::manager::McpManager;
use crate::mcp::{servidor, tool::ToolInfo};
use crate::permissions::{AgentMode, ToolEffect};
use crate::runtime;
use crate::state::{AiRuntime, AppState};
use crate::voz::{self, Ajustes, EstadoVoz, VozEvento};
use rmcp::model::CallToolResult;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Instant;
use tauri::{AppHandle, Emitter, Manager, State};

fn report(error: anyhow::Error) -> String {
    format!("{error:#}")
}

struct RegistroTurnoMcp {
    manager: Arc<McpManager>,
    turn_id: u64,
}

impl RegistroTurnoMcp {
    fn novo(
        manager: Arc<McpManager>,
        turn_id: u64,
        token: tokio_util::sync::CancellationToken,
    ) -> Self {
        manager.register_turn(turn_id, token);
        Self { manager, turn_id }
    }
}

impl Drop for RegistroTurnoMcp {
    fn drop(&mut self) {
        self.manager.finish_turn(self.turn_id);
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingApprovalView {
    pub id: String,
    pub connector_id: String,
    pub tool: String,
    pub effect: ToolEffect,
    pub compound: bool,
    pub age_seconds: u64,
}

#[tauri::command]
pub fn list_connectors(state: State<'_, AppState>) -> Vec<Connector> {
    state.manager.registry_snapshot().all().to_vec()
}

#[tauri::command]
pub fn save_connector(state: State<'_, AppState>, connector: Connector) -> Result<(), String> {
    if connector.id == local::CONECTOR || connector.id == servidor::CONECTOR {
        return Err(tr("erros.conectorReservado", &[("id", &connector.id)]));
    }
    let mut config = crate::state::trava_estado(&state.config);
    config.upsert(connector);
    let registry = config.registry().map_err(|e| e.to_string())?;
    config.save(&state.config_path).map_err(report)?;
    state.manager.replace_registry(registry);
    Ok(())
}

#[tauri::command]
pub async fn remove_connector(state: State<'_, AppState>, id: String) -> Result<(), String> {
    if id == local::CONECTOR {
        return Err(tr("erros.conectorReservado", &[("id", &id)]));
    }
    // Feito pelo aplicativo, o servidor publicado se desliga — não se apaga por
    // fora: a porta e o token continuam vivos até alguém fechar.
    if id == servidor::CONECTOR {
        state.manager.despublicar().await.map_err(report)?;
    }
    if state.manager.connected_ids().contains(&id) {
        state.manager.disconnect(&id).await.map_err(report)?;
    }
    let mut config = crate::state::trava_estado(&state.config);
    config.remove(&id);
    let registry = config.registry().map_err(|e| e.to_string())?;
    config.save(&state.config_path).map_err(report)?;
    state.manager.replace_registry(registry);
    Ok(())
}

#[tauri::command]
pub async fn connect_connector(
    state: State<'_, AppState>,
    id: String,
) -> Result<Vec<ToolInfo>, String> {
    state.manager.connect(&id).await.map_err(report)
}

#[tauri::command]
pub async fn disconnect_connector(state: State<'_, AppState>, id: String) -> Result<(), String> {
    state.manager.disconnect(&id).await.map_err(report)
}

#[tauri::command]
pub fn connected_connectors(state: State<'_, AppState>) -> Vec<String> {
    let mut ids = state.manager.connected_ids();
    ids.sort();
    ids
}

#[tauri::command]
pub fn connector_tools(state: State<'_, AppState>, id: String) -> Vec<ToolInfo> {
    state.manager.tools(&id)
}

#[tauri::command]
pub fn tool_catalog(state: State<'_, AppState>) -> String {
    state.manager.catalog(&CatalogBudget::default())
}

#[tauri::command]
pub fn resolve_intent(state: State<'_, AppState>, query: String) -> Vec<ToolInfo> {
    state.manager.search_tools(&query)
}

#[tauri::command]
pub fn all_tools(state: State<'_, AppState>) -> Vec<ToolInfo> {
    let mut tools: Vec<ToolInfo> = state
        .manager
        .connected_ids()
        .iter()
        .flat_map(|id| state.manager.tools(id))
        .collect();
    tools.sort_by(|a, b| a.qualified_name.cmp(&b.qualified_name));
    tools
}

#[tauri::command]
pub async fn call_tool(
    state: State<'_, AppState>,
    qualified_name: String,
    arguments: serde_json::Value,
) -> Result<CallToolResult, String> {
    state
        .manager
        .call_tool(&qualified_name, arguments)
        .await
        .map_err(report)
}

#[tauri::command]
pub fn pending_approvals(state: State<'_, AppState>) -> Vec<PendingApprovalView> {
    let now = Instant::now();
    let gate = crate::state::trava_estado(&state.gate);
    let mut pending: Vec<PendingApprovalView> = gate
        .pending()
        .map(|(id, approval)| PendingApprovalView {
            id: id.clone(),
            connector_id: approval.connector_id.clone(),
            tool: approval.tool.clone(),
            effect: approval.effect,
            compound: approval.compound,
            age_seconds: now.duration_since(approval.created_at).as_secs(),
        })
        .collect();
    pending.sort_by(|a, b| a.id.cmp(&b.id));
    pending
}

#[tauri::command]
pub fn resolve_approval(state: State<'_, AppState>, id: String, approved: bool) -> bool {
    state.manager.approvals().resolve(&id, approved)
}

#[tauri::command]
pub fn permission_mode(state: State<'_, AppState>) -> AgentMode {
    crate::state::trava_estado(&state.gate).mode()
}

#[tauri::command]
pub fn set_permission_mode(state: State<'_, AppState>, mode: AgentMode) {
    crate::state::trava_estado(&state.gate).set_mode(mode);
}

#[tauri::command]
pub fn session_grants(state: State<'_, AppState>) -> Vec<String> {
    crate::state::trava_estado(&state.gate).session_grants()
}

#[tauri::command]
pub fn grant_session(state: State<'_, AppState>, connector_id: String) {
    crate::state::trava_estado(&state.gate).grant_session(&connector_id);
}

#[tauri::command]
pub fn revoke_session(state: State<'_, AppState>, connector_id: String) {
    crate::state::trava_estado(&state.gate).revoke_session(&connector_id);
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PastaView {
    pub caminho: String,
    pub nome: String,
}

fn vista_pastas(pastas: &Pastas) -> Vec<PastaView> {
    pastas
        .raizes
        .iter()
        .map(|raiz| PastaView {
            nome: raiz
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| raiz.display().to_string()),
            caminho: raiz.display().to_string(),
        })
        .collect()
}

#[tauri::command]
pub fn granted_folders(state: State<'_, AppState>) -> Vec<PastaView> {
    vista_pastas(&crate::state::trava_leitura(&state.pastas))
}

/// Conceder uma pasta é a única forma de o Aether enxergar arquivos. O caminho é
/// canonicalizado antes de guardado, para a contenção valer contra o alvo real.
#[tauri::command]
pub async fn grant_folder(
    app: AppHandle,
    state: State<'_, AppState>,
    caminho: String,
) -> Result<Vec<PastaView>, String> {
    {
        let mut pastas = crate::state::trava_escrita(&state.pastas);
        pastas.conceder(&caminho).map_err(report)?;
        pastas.salvar(&state.pastas_path).map_err(report)?;
    }
    reacoplar_pastas(&app, &Arc::clone(&state.manager)).await;
    Ok(vista_pastas(&crate::state::trava_leitura(&state.pastas)))
}

#[tauri::command]
pub async fn revoke_folder(
    app: AppHandle,
    state: State<'_, AppState>,
    caminho: String,
) -> Result<Vec<PastaView>, String> {
    {
        let mut pastas = crate::state::trava_escrita(&state.pastas);
        if !pastas.revogar(&caminho) {
            return Err(tr("erros.pastaNaoConcedida", &[("caminho", &caminho)]));
        }
        pastas.salvar(&state.pastas_path).map_err(report)?;
    }
    // O conteúdo da pasta saiu da busca neste instante: a política manda na
    // leitura, não no arquivo. Isto aqui só devolve o espaço que sobrou no
    // índice — se a regravação falhar, a próxima varredura re-apaga.
    let _ = state.memoria.revogar();
    reacoplar_pastas(&app, &Arc::clone(&state.manager)).await;
    Ok(vista_pastas(&crate::state::trava_leitura(&state.pastas)))
}

/// Um servidor de arquivos instalado roda com as pastas na linha de comando,
/// então a concessão nova só vale depois de relançá-lo. O que não relançou vai
/// para a mesma fila de eventos que a instalação: a interface mostra o motivo.
async fn reacoplar_pastas(app: &AppHandle, manager: &Arc<McpManager>) {
    for (id, erro) in manager.recarregar_pastas().await {
        let _ = app.emit("server-event", ServidorEvent::Falhou { id, erro });
    }
}

/// Uma linha do catálogo de servidores, já com o que só o aplicativo sabe: se o
/// pacote está em disco, se está cadastrado, se está conectado e quantas
/// ferramentas ele anunciou. Nada aqui é suposto.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServidorView {
    #[serde(flatten)]
    pub ficha: instalador::Ficha,
    pub instalando: bool,
    pub cadastrado: bool,
    pub conectado: bool,
    pub ferramentas: usize,
}

#[tauri::command]
pub fn mcp_catalog(state: State<'_, AppState>) -> Vec<ServidorView> {
    let instalando = state.instalacoes.em_curso();
    let conectados = state.manager.connected_ids();
    let registro = state.manager.registry_snapshot();
    instalador::fichas(&state.servidores_root)
        .into_iter()
        .map(|ficha| {
            let conectado = conectados.contains(&ficha.id);
            ServidorView {
                instalando: instalando.contains(&ficha.id),
                cadastrado: registro.get(&ficha.id).is_some(),
                conectado,
                ferramentas: if conectado {
                    state.manager.tools(&ficha.id).len()
                } else {
                    0
                },
                ficha,
            }
        })
        .collect()
}

/// Instala o servidor no diretório do aplicativo e o cadastra.
///
/// A espera acontece aqui de propósito: cadastrar um servidor cujo `npm install`
/// ainda não terminou é registrar um transporte que não existe. O comando
/// transmite cada linha do instalador por evento, e o cancelamento é pedido por
/// outro comando, porque o flag vive em `Instalacoes`, não neste `State`.
#[tauri::command]
pub async fn mcp_install(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    let pronto = instalador::catalogado(&id).map_err(report)?;
    let cancel = state
        .instalacoes
        .iniciar(&id)
        .ok_or_else(|| tr("erros.instalacaoEmCurso", &[]))?;
    let raiz = state.servidores_root.clone();
    let _ = app.emit("server-event", ServidorEvent::Iniciou { id: id.clone(), instalando: true });
    let emissor = {
        let app = app.clone();
        let id = id.clone();
        move |linha: &str| {
            let _ = app.emit(
                "server-event",
                ServidorEvent::Linha { id: id.clone(), texto: linha.to_string() },
            );
        }
    };
    let resultado = instalador::instalar(pronto, &raiz, &cancel, emissor).await;
    state.instalacoes.encerrar(&id);

    let versao = match resultado {
        Ok(()) => instalador::versao_instalada(&raiz, pronto.pacote).unwrap_or_default(),
        Err(_) if cancel.load(std::sync::atomic::Ordering::Relaxed) => {
            let _ = app.emit("server-event", ServidorEvent::Cancelou { id });
            return Ok(());
        }
        Err(e) => {
            let erro = report(e);
            let _ = app.emit("server-event", ServidorEvent::Falhou { id, erro: erro.clone() });
            return Err(erro);
        }
    };

    // Só cadastramos o que provou estar em disco.
    let conector = instalador::conector(pronto, &raiz).map_err(report)?;
    {
        let mut config = crate::state::trava_estado(&state.config);
        config.upsert(conector);
        let registry = config.registry().map_err(|e| e.to_string())?;
        config.save(&state.config_path).map_err(report)?;
        state.manager.replace_registry(registry);
    }
    let _ = app.emit("server-event", ServidorEvent::Concluiu { id, versao });
    Ok(())
}

#[tauri::command]
pub fn mcp_install_cancel(state: State<'_, AppState>, id: String) -> bool {
    state.instalacoes.cancelar(&id)
}

/// Tira o servidor do cadastro e apaga o pacote. Primeiro o processo, depois o
/// arquivo: o contrário deixaria um cadastro apontando para nada.
#[tauri::command]
pub async fn mcp_uninstall(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    let pronto = instalador::catalogado(&id).map_err(report)?;
    if state.instalacoes.em_curso().contains(&id) {
        return Err(tr("erros.instalacaoEmCurso", &[]));
    }
    if state.manager.connected_ids().contains(&id) {
        state.manager.disconnect(&id).await.map_err(report)?;
    }
    let cancel = state
        .instalacoes
        .iniciar(&id)
        .ok_or_else(|| tr("erros.instalacaoEmCurso", &[]))?;
    let raiz = state.servidores_root.clone();
    let _ = app.emit("server-event", ServidorEvent::Iniciou { id: id.clone(), instalando: false });
    let emissor = {
        let app = app.clone();
        let id = id.clone();
        move |linha: &str| {
            let _ = app.emit(
                "server-event",
                ServidorEvent::Linha { id: id.clone(), texto: linha.to_string() },
            );
        }
    };
    let resultado = instalador::desinstalar(pronto, &raiz, &cancel, emissor).await;
    state.instalacoes.encerrar(&id);
    if let Err(e) = resultado {
        // Cancelar uma remoção não é um erro: o pacote fica como estava e o
        // cadastro também.
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            let _ = app.emit("server-event", ServidorEvent::Cancelou { id });
            return Ok(());
        }
        let erro = report(e);
        let _ = app.emit("server-event", ServidorEvent::Falhou { id, erro: erro.clone() });
        return Err(erro);
    }

    let mut config = crate::state::trava_estado(&state.config);
    config.remove(&id);
    let registry = config.registry().map_err(|e| e.to_string())?;
    config.save(&state.config_path).map_err(report)?;
    state.manager.replace_registry(registry);
    let _ = app.emit("server-event", ServidorEvent::Removeu { id });
    Ok(())
}

/// O estado do servidor MCP que o próprio aplicativo publica. Não levanta nada:
/// é o que a tela lê a cada aberto.
#[tauri::command]
pub fn mcp_servidor_estado(state: State<'_, AppState>) -> servidor::Estado {
    state.manager.estado_servidor()
}

/// Abrir a conexão pela tela é o mesmo caminho da ferramenta do agente: o
/// gerente publica, cadastra no `mcp.json` e conecta o cliente do aplicativo.
#[tauri::command]
pub async fn mcp_servidor_publicar(
    state: State<'_, AppState>,
) -> Result<servidor::Estado, String> {
    state.manager.publicar().await.map_err(report)
}

#[tauri::command]
pub async fn mcp_servidor_fechar(state: State<'_, AppState>) -> Result<bool, String> {
    state.manager.despublicar().await.map_err(report)
}

/// O token se copia, não se escreve na tela. Sem esta leitura não há como
/// apontar um cliente externo para o endereço publicado.
#[tauri::command]
pub fn mcp_servidor_token(state: State<'_, AppState>) -> Option<String> {
    state.manager.token_servidor()
}

// ---------------------------------------------------------------------------
// Bancos de dados: o cadastro que a tela edita e o que as ferramentas cobram.
// ---------------------------------------------------------------------------

/// Uma conexão como a tela a vê: sem senha, nem cifra — `temSenha` diz apenas
/// que existe uma guardada. É o que a lista mostra e o que o formulário de
/// edição recebe de volta; a cifra nunca atravessa a fronteira de novo.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConexaoView {
    pub id: String,
    pub nome: String,
    pub tipo: String,
    pub host: String,
    pub porta: Option<u16>,
    pub usuario: String,
    pub banco: String,
    pub arquivo: String,
    pub tem_senha: bool,
    pub limite_linhas: u32,
    pub sql_livre: bool,
    /// Se o canal desta conexão é conferido. A tela mostra o estado porque o
    /// motor obedece a ele — sem este campo o interruptor não existiria.
    pub tls_verificado: bool,
    pub fontes: BTreeMap<String, crate::bancos::config::Fonte>,
}

fn vista_conexoes(config: &crate::bancos::config::Config) -> Vec<ConexaoView> {
    config
        .conexoes
        .iter()
        .map(|c| ConexaoView {
            id: c.id.clone(),
            nome: c.nome.clone(),
            tipo: c.tipo.rotulo().to_string(),
            host: c.host.clone(),
            porta: c.porta,
            usuario: c.usuario.clone(),
            banco: c.banco.clone(),
            arquivo: c.arquivo.clone(),
            tem_senha: c.senha_cifrada.as_ref().is_some_and(|s| !s.is_empty()),
            limite_linhas: c.limite_linhas,
            sql_livre: c.sql_livre,
            tls_verificado: c.tls_verificado,
            fontes: c.fontes.clone(),
        })
        .collect()
}

#[tauri::command]
pub fn bancos_listar(state: State<'_, AppState>) -> Result<Vec<ConexaoView>, String> {
    let config = crate::bancos::config::Config::load(&state.bancos_path).map_err(report)?;
    Ok(vista_conexoes(&config))
}

/// Salvar é o ato do dono: validar, cifrar a senha que chegou em claro e gravar.
/// A senha nunca volta para a tela — numa edição vazia vale a que já está.
#[tauri::command]
pub fn bancos_salvar(
    state: State<'_, AppState>,
    conexao: crate::bancos::config::Conexao,
    senha: Option<String>,
) -> Result<Vec<ConexaoView>, String> {
    let mut config = crate::bancos::config::Config::load(&state.bancos_path).map_err(report)?;
    let mut entrada = conexao;
    entrada.senha_clara = senha;
    config.upsert(&mut entrada).map_err(report)?;
    config.save(&state.bancos_path).map_err(report)?;
    Ok(vista_conexoes(&config))
}

#[tauri::command]
pub fn bancos_remover(state: State<'_, AppState>, id: String) -> Result<Vec<ConexaoView>, String> {
    let mut config = crate::bancos::config::Config::load(&state.bancos_path).map_err(report)?;
    config.remover(&id);
    config.save(&state.bancos_path).map_err(report)?;
    Ok(vista_conexoes(&config))
}

/// Descobrir é um ato do dono pela tela: abre o banco de verdade e lista todas
/// as tabelas, inclusive as que ainda não estão expostas — é desta lista que o
/// dono escolhe o que o modelo pode ver. Sem política aqui: a política é o que
/// se salva depois, e é o conector que a cobra na chamada.
#[tauri::command]
pub async fn bancos_descobrir(
    state: State<'_, AppState>,
    conexao: crate::bancos::config::Conexao,
    senha: Option<String>,
) -> Result<Vec<String>, String> {
    let caminho = state.bancos_path.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut entrada = conexao;
        if let Some(clara) = senha.filter(|s| !s.is_empty()) {
            entrada.senha_cifrada =
                Some(crate::bancos::sigilo::cifrar(&clara).map_err(report)?);
        } else if entrada.senha_cifrada.is_none() {
            // Edição sem senha nova: o que está cifrado e salvo é o que vale.
            let salvo = crate::bancos::config::Config::load(&caminho).map_err(report)?;
            let velha = salvo.conexoes.iter().find(|c| {
                if entrada.id.trim().is_empty() {
                    c.nome.eq_ignore_ascii_case(entrada.nome.trim())
                } else {
                    c.id == entrada.id
                }
            });
            if let Some(velha) = velha {
                entrada.senha_cifrada = velha.senha_cifrada.clone();
            }
        }
        let mut motor = crate::bancos::motor::abrir(&entrada).map_err(report)?;
        motor.fontes().map_err(report)
    })
    .await
    .map_err(|e| report(anyhow::anyhow!("{e}")))?
}

/// Trocar o idioma remonta o cadastro e o catálogo embutido: o nome e a
/// descrição que o modelo lê passam a vir na língua que o usuário escolheu.
#[tauri::command]
pub fn set_locale(state: State<'_, AppState>, id: String) -> Result<String, String> {
    if !idiomas::definir(&id) {
        return Err(tr("erros.idiomaDesconhecido", &[("id", &id)]));
    }
    let registry = {
        let config = crate::state::trava_estado(&state.config);
        config.registry().map_err(|e| e.to_string())?
    };
    state.manager.replace_registry(registry);
    state.manager.recarregar_embutido();

    let mut history = session::trava_historico(&state.ai.history);
    if let Some(primeira) = history.first_mut() && primeira.role == "system" {
        primeira.content = session::system_prompt();
    }
    Ok(idiomas::atual())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiStatus {
    pub models: Vec<ModelStatus>,
    pub loaded: Option<String>,
    /// Onde o modelo carregado roda: placa ou CPU, com o motivo da troca. É
    /// estado, não explicação — a interface mostra a placa, não um parágrafo.
    pub acelerador: Option<crate::ai::Acelerador>,
    /// As escalas que valem no próximo turno, lidas do disco pelo motor.
    pub ajustes: geracao::Ajustes,
    /// A janela que o backend aceitou para o modelo em memória — não a que foi
    /// pedida. Sem modelo, sem número.
    pub janela: Option<usize>,
    pub busy: bool,
    pub turns: usize,
    pub downloading: Vec<String>,
    /// O runtime nativo: sem ele o modelo baixa e não carrega. A tela mostra o
    /// que falta em vez de deixar o usuário descobrir no erro do motor.
    pub runtime: Vec<runtime::EstadoPacote>,
}

#[tauri::command]
pub fn ai_status(state: State<'_, AppState>) -> AiStatus {
    let ai = &state.ai;
    AiStatus {
        models: catalog::all_status(&ai.models_root),
        loaded: ai.loaded_id(),
        acelerador: ai.acelerador(),
        ajustes: ai.ajustes(),
        janela: ai.janela(),
        busy: ai.is_busy(),
        turns: session::trava_historico(&ai.history).len(),
        downloading: ai.downloading(),
        runtime: runtime::estado(&ai.dlls),
    }
}

/// Grava as escalas de geração e devolve o que valeu depois da faixa. A tela
/// mostra o número que o núcleo vai usar, não o número que o cursor marcou.
#[tauri::command]
pub fn ai_ajustar(state: State<'_, AppState>, ajustes: geracao::Ajustes) -> Result<geracao::Ajustes, String> {
    state.ai.ajustar(ajustes).map_err(report)
}

/// Inicia o download e devolve imediatamente: o progresso chega por evento, e a
/// UI não pode ficar presa num invoke de vários gigabytes.
#[tauri::command]
pub async fn model_download(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    let spec = catalog::find(&id)
        .ok_or_else(|| tr("erros.modeloNaoCatalogado", &[("id", &id)]))?;
    let ai = Arc::clone(&state.ai);
    let cancel = ai
        .begin_download(&id)
        .ok_or_else(|| tr("erros.modeloBaixando", &[]))?;
    let root = ai.models_root.clone();
    let done = Arc::clone(&ai);
    let job_id = id.clone();
    tauri::async_runtime::spawn(async move {
        let emit = {
            let app = app.clone();
            move |event: download::DownloadEvent| {
                let _ = app.emit("model-event", &event);
            }
        };
        let outcome = download::fetch(spec, &root, &cancel, emit.clone()).await;
        // O disco mudou: o cartão da biblioteca (bytes do peso e do `.part`,
        // arquivos de apoio) e a ficha do roteador são os dois caches que medem
        // este modelo. Cada um é assinado e cairia sozinho na próxima leitura,
        // mas este é o ponto em que a verdade muda — e invalidação tem de estar
        // visível onde a escrita acontece, não depender de quem lembra. O
        // cabeçalho GGUF não tem cache próprio: ver a razão medida na doc de
        // `cabecalho::ler`.
        catalog::invalidar(&root, spec);
        done.descartar_ficha(spec);
        done.end_download(&job_id);
        match outcome {
            Ok(()) => {}
            // O flag é a prova de que foi o usuário que parou, não uma falha de
            // rede: o `.part` fica em disco e a próxima tentativa continua.
            Err(_) if cancel.load(std::sync::atomic::Ordering::Relaxed) => {
                emit(download::DownloadEvent::Cancelled { id: job_id });
            }
            Err(error) => {
                emit(download::DownloadEvent::Failed { id: job_id, error: report(error) });
            }
        }
    });
    Ok(())
}

#[tauri::command]
pub fn model_download_cancel(state: State<'_, AppState>, id: String) -> bool {
    state.ai.cancel_download(&id)
}

/// Apagar um modelo solta o codificador — que a varredura da memória segura
/// minutos inteiros — e risca gigabytes do disco. Nada disso é trabalho para a
/// thread que desenha: prender ela é prender a janela inteira, como em
/// `memoria_buscar`.
#[tauri::command]
pub async fn model_delete(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let spec = catalog::find(&id)
        .ok_or_else(|| tr("erros.modeloNaoCatalogado", &[("id", &id)]))?;
    let ai = Arc::clone(&state.ai);
    let custodio = Arc::clone(&state.memoria);
    tauri::async_runtime::spawn_blocking(move || -> anyhow::Result<()> {
        if ai.downloading().contains(&id) {
            anyhow::bail!(tr("erros.canceleDownload", &[]));
        }
        ai.unload(&id)?;
        // Um codificador em uso prende o `.gguf`: no Windows um arquivo mapeado não
        // sai do disco, e o llama.cpp mapeia. Solta a memória antes de apagar.
        custodio.soltar(&id)?;
        download::remove_weights(&ai.models_root, spec)?;
        // O peso saiu do disco e os dois caches que o medem não podem continuar
        // dizendo que ele está lá: mesmo motivo e mesmo lugar de
        // `model_download`.
        catalog::invalidar(&ai.models_root, spec);
        ai.descartar_ficha(spec);
        // O peso saiu do disco: um pino apontando para ele faria o próximo turno
        // falhar por escolha de um modelo que não existe mais.
        if ai.ajustes().modelo == spec.id {
            let ajustes = geracao::Ajustes { modelo: String::new(), ..ai.ajustes() };
            ai.ajustar(ajustes)?;
        }
        if custodio.ajustes().modelo == spec.id {
            let ajustes =
                memoria::Ajustes { modelo: String::new(), ..custodio.ajustes() };
            custodio.ajustar(ajustes)?;
        }
        Ok(())
    })
    .await
    .map_err(|e| tr("erros.remocaoNaoTerminou", &[("detalhe", &e.to_string())]))?
    .map_err(report)
}

/// Carrega os pesos em memória e fixa o modelo: "Usar" é uma escolha do dono,
/// e escolha que o roteador atropela no próximo turno não é uma escolha. A
/// decisão vai para `geracao.json` e sobrevive à reinicialização.
#[tauri::command]
pub async fn model_load(state: State<'_, AppState>, id: String) -> Result<String, String> {
    let spec = catalog::find(&id)
        .ok_or_else(|| tr("erros.modeloNaoCatalogado", &[("id", &id)]))?;
    let status = catalog::status(&state.ai.models_root, spec);
    if !status.complete {
        return Err(tr("erros.modeloIncompleto", &[("nome", spec.display)]));
    }
    let display = spec.display.to_string();
    // "Usar" num codificador não pode fixá-lo como cérebro da conversa: ele
    // produz vetor e não diz uma palavra. O que ele passa a ser é o modelo da
    // memória, e a escolha vai para `rag.json`. O aquecimento carrega os pesos
    // agora, para a primeira busca não pagar a carga sozinha.
    if spec.genero == catalog::Genero::Embeddings {
        let memoria = Arc::clone(&state.memoria);
        let ajustes = memoria::Ajustes { modelo: spec.id.to_string(), ..memoria.ajustes() };
        tauri::async_runtime::spawn_blocking(move || {
            memoria.ajustar(ajustes)?;
            memoria.aquecer()
        })
        .await
        .map_err(|e| tr("erros.cargaNaoTerminou", &[("detalhe", &e.to_string())]))?
        .map_err(report)?;
        return Ok(display);
    }
    let ai = Arc::clone(&state.ai);
    tauri::async_runtime::spawn_blocking(move || ai.preload(spec))
        .await
        .map_err(|e| tr("erros.cargaNaoTerminou", &[("detalhe", &e.to_string())]))?
        .map_err(report)?;
    let fixar = Arc::clone(&state.ai);
    let ajustes = geracao::Ajustes { modelo: spec.id.to_string(), ..fixar.ajustes() };
    fixar.ajustar(ajustes).map_err(report)?;
    Ok(display)
}

/// Solta o modelo: volta ao roteador, que decide por complexidade e RAM medida.
#[tauri::command]
pub fn model_solto(state: State<'_, AppState>) -> Result<geracao::Ajustes, String> {
    let ajustes = geracao::Ajustes { modelo: String::new(), ..state.ai.ajustes() };
    state.ai.ajustar(ajustes).map_err(report)
}

#[tauri::command]
pub fn chat_history(state: State<'_, AppState>) -> Vec<ChatMessage> {
    session::trava_historico(&state.ai.history).clone()
}

#[tauri::command]
pub async fn chat_reset(state: State<'_, AppState>) -> Result<(), String> {
    if let Some(turno) = state.ai.active_turn() {
        state.ai.request_cancel(turno.id);
        state.manager.cancel_turn(turno.id);
        // O comando é async justamente para esperar sem prender a thread da janela.
        // Só depois de o turno soltar o motor é seguro apagar o que ele ainda usava.
        state.ai.wait_stopped(turno.id).await;
    }
    state
        .ai
        .troca_exclusiva(|| {
            state.ai.esquecer()?;
            session::trava_historico(&state.ai.history).clear();
            *session::trava_resumo(&state.ai.resumo) = None;
            // O arquivo tem de dizer o mesmo que o buffer vivo: uma conversa limpa na
            // tela e cheia no disco voltaria cheia no próximo arranque.
            if let Some(id) = state.conversas.estado().ativa {
                state.conversas.gravar(&id, &[], None)?;
            }
            Ok(())
        })
        .map_err(report)
}

#[tauri::command]
pub fn chat_cancel(state: State<'_, AppState>) {
    if let Some(turno) = state.ai.active_turn() {
        state.ai.request_cancel(turno.id);
        state.manager.cancel_turn(turno.id);
    }
}

// ─── Conversas salvas ─────────────────────────────────────────────────────────

#[tauri::command]
pub fn conversas_listar(state: State<'_, AppState>) -> conversas::Estado {
    state.conversas.estado()
}

/// Uma conversa nova. Se a ativa ainda não tem mensagem, ela já é nova: criar
/// outra deixaria uma vazia na lista a cada clique. A troca inteira corre com o
/// lugar do turno reservado (`troca_exclusiva`): um turno que começasse no meio
/// dela disputaria o mesmo KV e o mesmo histórico, e o comando síncrono não pode
/// esperar o lock do motor — ele corre na thread da janela.
#[tauri::command]
pub fn conversa_nova(state: State<'_, AppState>) -> Result<conversas::Estado, String> {
    state
        .ai
        .troca_exclusiva(|| {
            // O KV é esquecido primeiro: se o motor estiver ocupado, a recusa
            // sai sem ter criado conversa nem tocado no buffer vivo.
            state.ai.esquecer()?;
            let estado = state.conversas.estado();
            let vazia = estado
                .ativa
                .as_ref()
                .and_then(|id| estado.conversas.iter().find(|f| &f.id == id))
                .map(|f| f.mensagens == 0)
                .unwrap_or(false);
            if !vazia {
                state.conversas.criar()?;
            }
            session::trava_historico(&state.ai.history).clear();
            *session::trava_resumo(&state.ai.resumo) = None;
            Ok(state.conversas.estado())
        })
        .map_err(report)
}

/// Troca a conversa ativa: o histórico do arquivo entra no buffer vivo, com o
/// resumo que pertence a ele, e o motor esquece o KV — porque o cache da
/// conversa anterior não é prefixo desta. Tudo sob a reserva do turno: abrir no
/// meio de uma geração substituiria o histórico que o turno está escrevendo, e
/// esperar o lock do motor prenderia a thread da janela o turno inteiro.
#[tauri::command]
pub fn conversa_abrir(state: State<'_, AppState>, id: String) -> Result<Vec<ChatMessage>, String> {
    state
        .ai
        .troca_exclusiva(|| {
            state.ai.esquecer()?;
            let aberta = state.conversas.abrir(&id)?;
            *session::trava_historico(&state.ai.history) = aberta.mensagens.clone();
            *session::trava_resumo(&state.ai.resumo) = aberta.resumo.clone();
            Ok(aberta.mensagens)
        })
        .map_err(report)
}

#[tauri::command]
pub fn conversa_renomear(
    state: State<'_, AppState>,
    id: String,
    titulo: String,
) -> Result<conversas::Ficha, String> {
    state.conversas.renomear(&id, &titulo).map_err(report)
}

#[tauri::command]
pub fn conversa_apagar(state: State<'_, AppState>, id: String) -> Result<conversas::Estado, String> {
    state
        .ai
        .troca_exclusiva(|| {
            let era_ativa = state.conversas.estado().ativa.as_deref() == Some(id.as_str());
            if era_ativa {
                // Esquece antes de apagar: se o motor estiver ocupado, a recusa
                // sai limpa — nada foi apagado nem o buffer vivo tocado.
                state.ai.esquecer()?;
            }
            state.conversas.apagar(&id)?;
            if era_ativa {
                session::trava_historico(&state.ai.history).clear();
                *session::trava_resumo(&state.ai.resumo) = None;
            }
            Ok(state.conversas.estado())
        })
        .map_err(report)
}

/// Texto que o modelo recebe de uma ferramenta. Só o conteúdo textual: imagem e
/// áudio não significam nada para um modelo de texto, e inventar uma descrição
/// deles seria exatamente o tipo de resultado falso que o produto proíbe.
fn result_text(result: &CallToolResult) -> String {
    let mut out = String::new();
    for block in &result.content {
        if let Some(text) = block.as_text() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&text.text);
        }
    }
    if out.is_empty() {
        serde_json::to_string(result).unwrap_or_else(|_| tr("erros.semConteudoTextual", &[]))
    } else {
        out
    }
}

#[tauri::command]
pub async fn chat_send(
    app: AppHandle,
    state: State<'_, AppState>,
    text: String,
) -> Result<TurnOutcome, String> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err(tr("erros.pedidoVazio", &[]));
    }
    let ai = Arc::clone(&state.ai);
    let manager = Arc::clone(&state.manager);
    let memoria = Arc::clone(&state.memoria);
    let pastas = Arc::clone(&state.pastas);
    let conversas = Arc::clone(&state.conversas);
    let derrame = state.derrame_path.clone();
    let compactacoes = state.compactacoes_path.clone();
    let Some(reserva) = ai.reservar_dono() else {
        return Err(tr("erros.turnoEmAndamento", &[]));
    };
    let turn_id = reserva.id();
    let cancel = reserva.cancel();
    let token = reserva.token();
    // A conversa tem de existir antes do turno: é nela que este histórico fica,
    // e um pedido sem conversa seria um pedido perdido ao fechar a janela.
    let id = conversas.garantir().map_err(report)?;
    // A identidade do turno é publicada antes de qualquer trabalho. Um `started`
    // emitido lá dentro do corpo bloqueante chega depois de a tela ter vinculado
    // outro turno — e os deltas deste iriam para a bolha errada.
    let _ = app.emit(
        "chat-event",
        &session::SessionEvent::new(turn_id, &id, SessionEventKind::Started),
    );
    let voz = Arc::clone(&state.voz);
    let app_fala = app.clone();
    let joined = tauri::async_runtime::spawn_blocking(move || {
        let registro = RegistroTurnoMcp::novo(Arc::clone(&manager), turn_id, token.clone());
        let resultado = run_chat_turn(
            &app, &ai, &manager, &memoria, &pastas, &conversas, &derrame, &compactacoes, &id,
            &text, turn_id, &cancel, &token,
        );
        // Grava mesmo quando o turno falhou: o que o modelo já disse e as
        // ferramentas que já rodaram aconteceram, e a conversa é o registro
        // disso. Só este turno pode publicar no buffer que ele reservou.
        let resultado = if ai.active_turn().map(|t| t.id) == Some(turn_id) {
            let mensagens = session::trava_historico(&ai.history).clone();
            let resumo = session::trava_resumo(&ai.resumo).clone();
            match (resultado, conversas.gravar(&id, &mensagens, resumo.as_ref())) {
                (Ok(desfecho), Ok(_)) => Ok(desfecho),
                // A causa do turno vem primeiro: ela é o que a pessoa precisa ver.
                (Err(erro), _) => Err(erro),
                (Ok(_), Err(erro)) => Err(report(erro)),
            }
        } else {
            resultado
        };
        drop(registro);
        drop(reserva);
        resultado
    })
    .await;
    let resultado =
        joined.map_err(|e| tr("erros.turnoNaoTerminou", &[("detalhe", &e.to_string())]))?;

    // Ler a resposta é escolha do usuário, e o pedido de fala é barato: entra na
    // fila e sai no ritmo do alto-falante. Um erro de voz não pode estragar um
    // turno que deu certo — ele vira evento, e a tela mostra.
    if let Ok(desfecho) = &resultado && voz.ajustes().auto_falar {
        if let Err(erro) = voz.dizer(&desfecho.text) {
            let _ = app_fala.emit(
                "voz-evento",
                VozEvento::Falhou {
                    id: voz.ajustes().modelo_fala,
                    erro,
                },
            );
        }
    }
    resultado
}

fn run_chat_turn(
    app: &AppHandle,
    ai: &AiRuntime,
    manager: &Arc<McpManager>,
    memoria: &Custodio,
    pastas: &Arc<RwLock<Pastas>>,
    conversas: &Arc<conversas::Conversas>,
    // A pasta do depósito de resultados longos, vinda do estado. O turno escreve
    // nela e a ferramenta `ler_resultado` lê dela: as duas pontas têm de ser a
    // mesma pasta, senão o id que o modelo recebe não leva a lugar nenhum.
    derrame: &std::path::Path,
    // `<casa>/.aether/compactacoes`: onde cada dobra automática deixa o registro
    // dela. É parâmetro e não cálculo aqui dentro porque a raiz é uma decisão da
    // abertura, e uma dobra escrita em outro lugar seria um segundo arquivo que
    // ninguém procura.
    compactacoes: &std::path::Path,
    conversa: &str,
    text: &str,
    turn_id: u64,
    cancel: &Arc<AtomicBool>,
    token: &tokio_util::sync::CancellationToken,
) -> Result<TurnOutcome, String> {
    let emit = |kind: session::SessionEventKind| {
        if cancel.load(Ordering::SeqCst) || token.is_cancelled() {
            return;
        }
        let event = session::SessionEvent::new(turn_id, conversa, kind);
        let _ = app.emit("chat-event", &event);
    };

    // Só as ferramentas realmente conectadas entram no prompt. Um bloco de
    // ferramentas vazio custa tokens e não oferece nada ao modelo.
    let tools: Vec<ToolInfo> = manager
        .connected_ids()
        .iter()
        .flat_map(|id| manager.tools(id))
        .collect();
    let specs: Vec<ToolSpec> = tools
        .iter()
        .map(|t| ToolSpec {
            name: t.qualified_name.clone(),
            description: t.description.clone(),
            parameters: t.input_schema.clone(),
            alvo: Alvo::do_esquema(&t.input_schema),
        })
        .collect();

    let available: Vec<&'static ModelSpec> = catalog::CATALOG
        .iter()
        .filter(|m| catalog::status(&ai.models_root, m).complete)
        .collect();
    if available.is_empty() {
        return Err(tr("erros.semModelo", &[]));
    }

    let ajustes = ai.ajustes();

    // O modelo não enxerga o disco do dono da máquina: o que ele sabe do sistema
    // de arquivos é o que está escrito aqui. Sem as pastas concedidas no prompt,
    // a primeira chamada de arquivo inventa um caminho, a contenção recusa, e a
    // pessoa vê o assistente esbarrar três vezes na mesma parede — medido no app
    // real. É estado do pedido, não memória: entra na renderização e nunca no
    // histórico, então revogar uma pasta muda o prompt no turno seguinte.
    let raizes: Vec<String> = crate::state::trava_leitura(pastas)
        .raizes
        .iter()
        .map(|r| r.display().to_string())
        .collect();
    let bloco_pastas = if raizes.is_empty() {
        None
    } else {
        Some(tr("conversa.pastasConcedidas", &[("lista", &raizes.join(", "))]))
    };

    // A memória entra antes do primeiro token, e antes de o modelo de conversa
    // ocupar a placa: o codificador tem a placa inteiro para ele por alguns
    // milissegundos. O fio dela no grafo é a mesma política que governa a
    // ferramenta `buscar_memoria`, então desligar a ligação no grafo desliga a
    // injeção no mesmo instante — sem segundo interruptor a divergir.
    let sinapse = format!("{}__{}", local::CONECTOR, "buscar_memoria");
    let bloco = if manager.ferramenta_ativa(local::CONECTOR, "buscar_memoria") {
        // As conversas salvas entram no índice antes de a busca rodar: é isto que
        // faz "sobre o que falamos antes?" ter resposta numa sessão que começou
        // vazia. Um turno sem conversa nova não paga vetor nenhum — a prova de
        // cada conversa mora no próprio índice — e o que mudou cabe no orçamento
        // de `TOKENS_POR_TURNO`, com o resto entrando no varrer.
        let _ = memoria.revalidar_conversas(&conversas.para_memoria());
        let inicio = Instant::now();
        // A conversa em curso fica de fora da injeção: o que ela já disse está no
        // buffer do modelo, e reler isso custa as células que faltam para a
        // lembrança que ele realmente não tem. A ferramenta, quando o modelo
        // chama, responde por ela também — aí a pergunta é sobre o que a janela
        // já perdeu.
        let ignorar = vec![conversa.to_string()];
        // O bloco do turno, e não a busca crua: é aqui que a nota presa entra, na
        // frente e sem precisar ser achada, dividindo com o resto o mesmo teto de
        // `trechos`. Chamar `buscar` diretamente deixaria o marcador do dono como
        // um enfeite gravado no arquivo — e "preso" só é real se mudar o prompt.
        let resposta = memoria.bloco_do_turno(text, &ignorar);
        if resposta.achados.is_empty() {
            None
        } else {
            let Resposta { achados, alcancados, modo } = resposta;
            let texto = Custodio::texto_do_bloco(&achados);
            emit(SessionEventKind::Memoria {
                ferramenta: sinapse,
                consulta: text.to_string(),
                modo,
                achados,
                total: alcancados,
                ms: inicio.elapsed().as_millis() as u64,
            });
            Some(texto)
        }
    } else {
        None
    };

    // Fixado pelo usuário: o roteador não passa na frente de uma escolha. O
    // pedido ainda é classificado — os sinais são reais e continuam visíveis —
    // mas o motivo diz quem escolheu.
    let fixo = ajustes.modelo.trim().to_string();
    let (spec, escolha) = if fixo.is_empty() {
        // O menor modelo completo é o ponto de partida: é preciso um tokenizer
        // real para contar os tokens do pedido, e o motivo exibido cita essa
        // contagem.
        let starter = *available
            .iter()
            .min_by_key(|m| m.weights_bytes)
            .expect("available não é vazio");
        emit(SessionEventKind::Loading { model: starter.display.to_string() });
        let mut guard = ai.acquire(starter).map_err(report)?;
        let prompt_tokens = guard
            .as_mut()
            .expect("acquire acabou de carregar")
            .engine
            .count_tokens(text)
            .map_err(report)?;
        let request = Request {
            text: text.to_string(),
            prompt_tokens,
            needs_tools: !specs.is_empty(),
            janela: ajustes.contexto as usize,
        };
        // A escolha é pela placa medida agora, não pela RAM livre do sistema:
        // cada candidato entra com o próprio cabeçalho lido, e o veredito de
        // alojamento é o que ordena.
        let hardware = router::hardware_agora(&ai.dlls).map_err(report)?;
        let candidatos = ai.candidatos(&available);
        let escolha = router::choose(&request, hardware, &candidatos)
            .ok_or_else(|| tr("erros.semModeloParaPedido", &[]))?;
        emit(SessionEventKind::Choice { choice: escolha.clone() });
        let spec = catalog::find(&escolha.model_id).ok_or_else(|| {
            tr("erros.modeloForaDoCatalogo", &[("modelo", &escolha.model_id)])
        })?;
        (spec, Some(escolha))
    } else {
        let spec = catalog::find(&fixo)
            .ok_or_else(|| tr("erros.modeloNaoCatalogado", &[("id", &fixo)]))?;
        if !catalog::status(&ai.models_root, spec).complete {
            return Err(tr("erros.modeloFixoIncompleto", &[("nome", spec.display)]));
        }
        (spec, None)
    };

    emit(SessionEventKind::Loading { model: spec.display.to_string() });
    // O guard abaixo é segurado até o fim desta função — geração, rodadas de
    // ferramenta e a espera do gate incluídas — e é ele que serializa um turno por
    // vez sobre o KV cache do modelo em memória. Não é descuido: dois turnos no
    // mesmo motor não são dois turnos mais rápidos, são os tokens de um pedido
    // misturados com os do outro. Quem não pode esperar por ele é a thread da
    // janela, e é por isso que `ai_status` lê os caches do `state.rs`, que a
    // `esquecer`/`unload`/esta liberação de janela tentam a trava em vez de
    // bloquear, e que `chat_send` responde `erros.turnoEmAndamento` no CAS de
    // `begin` antes de chegar aqui. Ver `CONTRIBUTING.md` e `docs/melhorias/CONC2.md`.
    let mut guard = ai.acquire(spec).map_err(report)?;
    let loaded = guard.as_mut().expect("acquire acabou de carregar");

    // Com o modelo fixado a contagem de tokens sai do tokenizer dele, que é o
    // mesmo que vai ler o prompt: o sinal citado no motivo é medido, nunca
    // suposto.
    if escolha.is_none() {
        let prompt_tokens = loaded.engine.count_tokens(text).map_err(report)?;
        let pedido = Request {
            text: text.to_string(),
            prompt_tokens,
            needs_tools: !specs.is_empty(),
            janela: ajustes.contexto as usize,
        };
        let (complexidade, signals) = router::classify(&pedido);
        emit(SessionEventKind::Choice {
            choice: Choice {
                model_id: spec.id.to_string(),
                display: spec.display.to_string(),
                complexity: complexidade,
                reason: tr("roteador.fixado", &[("nome", spec.display)]),
                signals,
            },
        });
    }
    let params = ajustes.amostragem();
    let resumo = session::trava_resumo(&ai.resumo).clone();
    let config = TurnConfig {
        tools: &specs,
        params: &params,
        // O bloco de pensamento é escolha do dono da máquina, guardada em
        // `geracao.json`. Medido no Granite 4.2 3B: com ele aberto o modelo
        // entra em laço mesmo num prompt curto que responde certo sem ele.
        enable_thinking: ajustes.pensamento,
        max_rounds: ajustes.rodadas as usize,
        memoria: bloco,
        pastas: bloco_pastas,
        derrame: Some(derrame.to_path_buf()),
        resumo,
    };

    {
        let mut history = session::trava_historico(&ai.history);
        if history.is_empty() {
            history.push(ChatMessage::new("system", session::system_prompt()));
        }
        history.push(ChatMessage::new("user", text));
    }

    let runtime = tokio::runtime::Handle::current();
    let run_tool = |call: &ToolCall| -> anyhow::Result<session::ToolOutcome> {
        let result = runtime.block_on(manager.call_tool_cancellable(
            &call.name,
            call.arguments.clone(),
            token.clone(),
        ))?;
        let text = result_text(&result);
        if result.is_error.unwrap_or(false) {
            anyhow::bail!("{text}");
        }
        Ok(session::ToolOutcome { text, vista: result.structured_content.clone() })
    };
    let janela = loaded.engine.context();
    let modelo = loaded.spec.id.to_string();
    let mut persistir_resumo =
        |novo: &crate::ai::compactacao::Resumo, feito: &session::FeitoDeDobra| -> anyhow::Result<()>
            {
                if ai.active_turn().map(|t| t.id) != Some(turn_id) {
                    anyhow::bail!(tr("erros.pedidoCancelado", &[]));
                }
                *session::trava_resumo(&ai.resumo) = Some(novo.clone());
                let mensagens = session::trava_historico(&ai.history).clone();
                conversas.gravar(conversa, &mensagens, Some(novo))?;
                // A cópia da conversa é o que o prompt lê; esta é o que a pessoa
                // lê depois. Uma dobra só chega aqui depois de gravada na
                // conversa, senão o registro contaria uma memória que o motor
                // ainda não tem.
                let dobra = crate::ai::compactacao::Dobra {
                    conversa: conversa.to_string(),
                    ordem: crate::ai::compactacao::Dobra::ordem_anterior(
                        compactacoes, conversa,
                    ) + 1,
                    modelo: modelo.clone(),
                    janela,
                    texto: novo.texto.clone(),
                    ate: novo.ate,
                    lidas: feito.lidas,
                    descartadas: feito.descartadas,
                    tokens: feito.tokens,
                    quando: crate::ai::compactacao::agora(),
                };
                dobra.gravar(compactacoes)?;
                Ok(())
            };

    let turno = session::run_turn(
        &mut loaded.engine,
        &loaded.template,
        &ai.history,
        &config,
        turn_id,
        cancel,
        emit,
        run_tool,
        &mut persistir_resumo,
    );
    if let Ok(desfecho) = &turno {
        // O resumo que o turno escreveu é do buffer vivo, como o histórico: sem
        // isto o próximo pedido dobraria de novo os mesmos turnos, na placa, para
        // chegar ao mesmo texto.
        *session::trava_resumo(&ai.resumo) = desfecho.resumo.clone();
    }
    if turno.is_err() {
        // O turno acima pôs o pedido no histórico antes de medir a janela, e
        // pode ter morrido antes do primeiro token: sem este desfazimento, cada
        // tentativa malsucedida soma uma linha igual e empurra a janela para
        // frente. Só desfaz enquanto a última linha for o pedido — depois que o
        // modelo falou (ou uma ferramenta rodou), aquilo aconteceu e fica.
        let mut history = session::trava_historico(&ai.history);
        if history.last().map(|m| m.role.as_str()) == Some("user") {
            history.pop();
        }
    }
    turno.map_err(report)
}

/// O que a dobra pedida devolve: os números que o modelo produziu, e a
/// identidade do turno que a tela já atrelou pelo `started`. O texto volta aqui e
/// no evento de propósito: o evento pinta a bolha em curso, e este é o que a
/// promessa usa para assentar a mesma bolha quando o turno acaba.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Compactacao {
    pub turn_id: u64,
    pub conversa: String,
    pub texto: String,
    pub lidas: usize,
    pub descartadas: usize,
    pub tokens: usize,
    pub ordem: usize,
}

/// Dobra a conversa agora, sem esperar a janela apertar. Não é um segundo
/// mecanismo: é o mesmo `compactacao::dobrar`, o mesmo evento `resumo`, a mesma
/// gravação na conversa e o mesmo registro em `<casa>/.aether/compactacoes` que o
/// turno usa. O que muda é só o gatilho — e o quanto se dobra, que aqui é o
/// máximo que o resumo consegue carregar em vez do mínimo que faz caber.
#[tauri::command]
pub async fn chat_compactar(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Compactacao, String> {
    let ai = Arc::clone(&state.ai);
    let conversas = Arc::clone(&state.conversas);
    let compactacoes = state.compactacoes_path.clone();
    // A dobra é um turno: ela segura o motor e escreve no histórico. Começá-la
    // durante uma geração leria um histórico mudando sob o resumo.
    let Some(reserva) = ai.reservar_dono() else {
        return Err(tr("erros.turnoEmAndamento", &[]));
    };
    let turn_id = reserva.id();
    let cancel = reserva.cancel();
    let id = conversas.garantir().map_err(report)?;
    // Pela mesma razão de `chat_send`: o `started` é o que vincula a bolha ao
    // turno, e precisa existir antes de o corpo bloqueante começar.
    let _ = app.emit(
        "chat-event",
        &session::SessionEvent::new(turn_id, &id, SessionEventKind::Started),
    );
    let joined = tauri::async_runtime::spawn_blocking(move || {
        let resultado =
            run_dobra(&app, &ai, &conversas, &compactacoes, &id, turn_id, &cancel);
        drop(reserva);
        resultado
    })
    .await;
    joined.map_err(|e| tr("erros.turnoNaoTerminou", &[("detalhe", &e.to_string())]))?
}

fn run_dobra(
    app: &AppHandle,
    ai: &Arc<AiRuntime>,
    conversas: &Arc<conversas::Conversas>,
    compactacoes: &Path,
    conversa: &str,
    turn_id: u64,
    cancel: &Arc<AtomicBool>,
) -> Result<Compactacao, String> {
    let emit = |kind: session::SessionEventKind| {
        if cancel.load(Ordering::SeqCst) {
            return;
        }
        let event = session::SessionEvent::new(turn_id, conversa, kind);
        let _ = app.emit("chat-event", &event);
    };
    // O modelo que está em memória é o que resume. Escolher outro aqui trocaria os
    // pesos e o KV da conversa, e a pessoa pediu que comprimisse o que ela vê, não
    // que ligasse outra máquina.
    let id = ai.loaded_id().ok_or_else(|| tr("erros.nenhumModeloEmMemoria", &[]))?;
    let spec =
        catalog::find(&id).ok_or_else(|| tr("erros.modeloNaoCatalogado", &[("id", &id)]))?;
    let mut guard = ai.acquire(spec).map_err(report)?;
    let loaded = guard.as_mut().expect("acquire acabou de carregar");
    // A conversa inteira fora do lock: medir e escrever o resumo são geração, e
    // segurar o histórico enquanto a placa trabalha prende a tela que o lê.
    let history = session::trava_historico(&ai.history).clone();
    let resumo = session::trava_resumo(&ai.resumo).clone();
    let Some((desde, ate)) =
        session::dobras_pedidas(&loaded.engine, &loaded.template, &history, resumo.as_ref())
            .map_err(report)?
    else {
        return Err(tr("erros.nadaParaCompactar", &[]));
    };
    let params = ai.ajustes().amostragem();
    let (texto, gasto, lidas) = compactacao::dobrar(
        &mut loaded.engine,
        &loaded.template,
        &params,
        resumo.as_ref().map(|r| r.texto.as_str()),
        &history[desde..ate],
        cancel,
    )
    .map_err(report)?;
    let feito = session::FeitoDeDobra {
        lidas,
        descartadas: (ate - desde).saturating_sub(lidas),
        tokens: gasto.generated_tokens,
    };
    let novo = compactacao::Resumo { texto, ate: desde + lidas };
    emit(SessionEventKind::Resumo {
        texto: novo.texto.clone(),
        mensagens: feito.lidas,
        descartadas: feito.descartadas,
        tokens: feito.tokens,
    });
    // O Parar já soltou o turno e outro pode ter começado: nada de escrever o
    // resumo de um pedido desistido no buffer vivo de quem chegou depois.
    if ai.active_turn().map(|t| t.id) != Some(turn_id) {
        return Err(tr("erros.pedidoCancelado", &[]));
    }
    *session::trava_resumo(&ai.resumo) = Some(novo.clone());
    let mensagens = session::trava_historico(&ai.history).clone();
    conversas.gravar(conversa, &mensagens, Some(&novo)).map_err(report)?;
    let ordem = compactacao::Dobra::ordem_anterior(compactacoes, conversa) + 1;
    let dobra = compactacao::Dobra {
        conversa: conversa.to_string(),
        ordem,
        modelo: spec.id.to_string(),
        janela: loaded.engine.context(),
        texto: novo.texto.clone(),
        ate: novo.ate,
        lidas: feito.lidas,
        descartadas: feito.descartadas,
        tokens: feito.tokens,
        quando: compactacao::agora(),
    };
    dobra.gravar(compactacoes).map_err(report)?;
    Ok(Compactacao {
        turn_id,
        conversa: conversa.to_string(),
        texto: novo.texto,
        lidas: feito.lidas,
        descartadas: feito.descartadas,
        tokens: feito.tokens,
        ordem,
    })
}

// ─── Grafo de ligações ────────────────────────────────────────────────────────

/// O que a tela de ligações desenha. Vem todo do estado vivo: um nó para cada
/// conector cadastrado, uma porta para cada ferramenta que ele anunciou, e as
/// pastas que valem para todos. Nada aqui é inventado para a tela ficar bonita.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EstadoLigacoes {
    pub nos: Vec<ligacoes::No>,
    pub pastas: Vec<PastaView>,
    pub posicoes: Vec<ligacoes::Posicao>,
}

fn estado_ligacoes(state: &AppState) -> EstadoLigacoes {
    let registry = state.manager.registry_snapshot();
    let conectados = state.manager.connected_ids();
    let nos = registry
        .all()
        .iter()
        .map(|conector| {
            let anunciadas = state.manager.anunciadas(&conector.id);
            let portas = anunciadas
                .iter()
                .map(|ferramenta| ligacoes::Porta {
                    nome: ferramenta.name.clone(),
                    chave: ferramenta.qualified_name.clone(),
                    titulo: ferramenta
                        .titulo
                        .clone()
                        .unwrap_or_else(|| ferramenta.name.clone()),
                    descricao: ferramenta.description.clone(),
                    efeito: ferramenta.effect,
                    ligada: conector.allows_tool(&ferramenta.name),
                })
                .collect();
            ligacoes::No {
                portas,
                id: conector.id.clone(),
                nome: conector.name.clone(),
                descricao: conector.description.clone(),
                conectado: conectados.contains(&conector.id),
            }
        })
        .collect();
    EstadoLigacoes {
        nos,
        pastas: vista_pastas(&crate::state::trava_leitura(&state.pastas)),
        posicoes: ligacoes::Layout::de(&state.ligacoes_path).posicoes,
    }
}

#[tauri::command]
pub fn ligacoes_obter(state: State<'_, AppState>) -> EstadoLigacoes {
    estado_ligacoes(&state)
}

/// Compila o grafo na política que o núcleo já cobra. As allowlists vão para
/// `mcp.json` e as pastas para `pastas.json`; o que o grafo guarda é só o
/// desenho das posições.
#[tauri::command]
pub async fn ligacoes_aplicar(
    app: AppHandle,
    state: State<'_, AppState>,
    plano: ligacoes::Plano,
) -> Result<EstadoLigacoes, String> {
    let registry = state.manager.registry_snapshot();
    let compilado = {
        let manager = &state.manager;
        let anunciadas = |id: &str| {
            manager
                .anunciadas(id)
                .into_iter()
                .map(|ferramenta| ferramenta.name)
                .collect()
        };
        ligacoes::recortar(&plano, &registry, &anunciadas)?
    };

    if !compilado.is_empty() {
        let mut config = crate::state::trava_estado(&state.config);
        for (id, ferramentas) in &compilado {
            // O conector embutido é o próprio aplicativo e costuma não estar no
            // arquivo: para guardar a allowlist dele, ele entra primeiro.
            match config.servers.iter_mut().find(|c| c.id == *id) {
                Some(servidor) => servidor.tool_allowlist = Some(ferramentas.clone()),
                None => {
                    let mut novo = registry.get(id).cloned().ok_or_else(|| {
                        tr("erros.servidorNaoRegistrado", &[("id", id)])
                    })?;
                    novo.tool_allowlist = Some(ferramentas.clone());
                    config.upsert(novo);
                }
            }
        }
        config.save(&state.config_path).map_err(report)?;
        let novo = config.registry().map_err(|e| e.to_string())?;
        state.manager.replace_registry(novo);
    }

    // O plano é a lista completa do que fica concedido. Montar a lista nova à
    // parte deixa o estado intacto se um caminho não existir: meio grafo
    // aplicado é pior que o grafo não aplicado.
    let mudancas = {
        let mut novas = Pastas::default();
        for caminho in &plano.pastas {
            novas.conceder(caminho).map_err(report)?;
        }
        let mut pastas = crate::state::trava_escrita(&state.pastas);
        let mudou = novas.raizes != pastas.raizes;
        if mudou {
            *pastas = novas;
            pastas.salvar(&state.pastas_path).map_err(report)?;
        }
        mudou
    };
    if mudancas {
        // O grafo é a única forma de conceder e revogar pastas em lote, então é
        // aqui também que a memória perde o que deixou de estar concedido.
        let _ = state.memoria.revogar();
        reacoplar_pastas(&app, &Arc::clone(&state.manager)).await;
    }
    Ok(estado_ligacoes(&state))
}

/// Guardar o desenho. As posições não são política: apagá-las não tira
/// ferramenta nenhuma do assistente.
#[tauri::command]
pub fn ligacoes_layout(
    state: State<'_, AppState>,
    posicoes: Vec<ligacoes::Posicao>,
) -> Result<(), String> {
    ligacoes::Layout { posicoes }
        .guardar(&state.ligacoes_path)
        .map_err(report)
}

// ---------------------------------------------------------------------------
// Voz
// ---------------------------------------------------------------------------

/// Tudo que a tela de voz precisa numa chamada: se a voz existe, o que está
/// escolhido, o que está instalado, se está falando e o que o microfone ouve.
#[tauri::command]
pub fn voz_estado(state: State<'_, AppState>) -> EstadoVoz {
    state.voz.estado()
}

/// O pulso da voz: se está falando, se está ouvindo e o nível do microfone
/// agora. São três atômicos lidos do motor — é o que a tela pergunta a cada
/// quadro, e por isso não paga a leitura de disco do estado inteiro.
#[tauri::command]
pub fn voz_medidor(state: State<'_, AppState>) -> voz::Medidor {
    voz::Medidor {
        falando: state.voz.falando(),
        ouvindo: state.voz.ouvindo(),
        nivel: state.voz.nivel(),
    }
}

/// Grava as escolhas do usuário. Um modelo que não existe no catálogo é
/// recusado com motivo, nunca trocado por um parecido.
#[tauri::command]
pub fn voz_ajustar(state: State<'_, AppState>, ajustes: Ajustes) -> Result<Ajustes, String> {
    state.voz.ajustar(ajustes)
}

/// Manda falar. Devolve quando o pedido entra na fila: a voz começa a sair
/// enquanto o resto ainda está sendo gerado.
#[tauri::command]
pub fn voz_falar(state: State<'_, AppState>, texto: String) -> Result<(), String> {
    state.voz.dizer(&texto)
}

/// Amostra uma voz da lista sem mudar o que está guardado.
#[tauri::command]
pub fn voz_prever(
    state: State<'_, AppState>,
    texto: String,
    id_voz: String,
    sid: i32,
    velocidade: f32,
) -> Result<(), String> {
    state.voz.prever(&texto, &id_voz, sid, velocidade)
}

#[tauri::command]
pub fn voz_parar(state: State<'_, AppState>) {
    state.voz.parar_fala();
}

/// Liga o microfone. Fica ligado até `voz_transcrever` ou `voz_cancelar_escuta`.
#[tauri::command]
pub fn voz_ouvir(state: State<'_, AppState>) -> Result<(), String> {
    state.voz.ouvir()
}

/// Solta o microfone e devolve o texto. O Whisper é pesado o bastante para não
/// rodar na thread do comando: ela travaria a interface.
#[tauri::command]
pub async fn voz_transcrever(state: State<'_, AppState>) -> Result<String, String> {
    let voz = Arc::clone(&state.voz);
    tauri::async_runtime::spawn_blocking(move || voz.transcrever())
        .await
        .map_err(|e| tr("erros.vozTranscricaoNaoTerminou", &[("detalhe", &e.to_string())]))?
}

#[tauri::command]
pub fn voz_cancelar_escuta(state: State<'_, AppState>) {
    state.voz.descartar_escuta();
}

/// Inicia a busca de um modelo de voz e devolve na hora: o progresso chega por
/// evento, como no download dos pesos do LLM.
#[tauri::command]
pub async fn voz_baixar(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    let espec = voz::modelo_de(&id, espec_servico(&id)?)?;
    let voz = Arc::clone(&state.voz);
    let cancel = voz
        .iniciar_download(&id)
        .ok_or_else(|| tr("erros.vozDownloadEmCurso", &[]))?;
    let raiz = voz.raiz().to_path_buf();
    let job = id.clone();
    tauri::async_runtime::spawn(async move {
        let emit = {
            let app = app.clone();
            move |event: VozEvento| {
                let _ = app.emit("voz-evento", &event);
            }
        };
        let outcome = voz::baixar::baixar(espec, &raiz, &cancel, emit.clone()).await;
        voz.encerrar_download(&job);
        match outcome {
            Ok(()) => {}
            // O flag é a prova de que foi o usuário que parou, não a rede.
            Err(_) if cancel.load(Ordering::Relaxed) => {
                emit(VozEvento::Cancelou { id: job });
            }
            Err(error) => {
                emit(VozEvento::Falhou { id: job, erro: report(error) });
            }
        }
    });
    Ok(())
}

/// O serviço de uma entrada do catálogo, lido da própria ficha: a UI não
/// precisa dizer se é fala ou escuta para baixar.
fn espec_servico(id: &str) -> Result<voz::Servico, String> {
    voz::modelos::encontrar(id)
        .map(|e| e.servico)
        .ok_or_else(|| tr("erros.vozModeloNaoCatalogado", &[("id", id)]))
}

#[tauri::command]
pub fn voz_baixar_cancelar(state: State<'_, AppState>, id: String) -> bool {
    state.voz.cancelar_download(&id)
}

/// Apaga um modelo de voz. Primeiro se solta a instância aberta — no Windows um
/// `.onnx` em uso não sai do disco — depois se remove a pasta.
#[tauri::command]
pub async fn voz_apagar(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let espec = voz::modelos::encontrar(&id)
        .ok_or_else(|| tr("erros.vozModeloNaoCatalogado", &[("id", &id)]))?;
    let voz = Arc::clone(&state.voz);
    if voz.baixando().contains(&id) {
        return Err(tr("erros.canceleDownload", &[]));
    }
    tauri::async_runtime::spawn_blocking(move || {
        voz.liberar(espec)?;
        voz::baixar::apagar(espec, voz.raiz()).map_err(report)
    })
    .await
    .map_err(|e| tr("erros.vozRemocaoNaoTerminou", &[("detalhe", &e.to_string())]))?
}

// ---------------------------------------------------------------------------
// Memória (RAG)
// ---------------------------------------------------------------------------

/// Tudo que a tela de memória pergunta de uma vez: o estado do motor e do
/// índice, mais as quants do codificador com o que o disco diz de cada uma. A
/// tela não precisa saber id nenhum para oferecer o download.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EstadoMemoria {
    #[serde(flatten)]
    pub estado: memoria::Estado,
    pub codificadores: Vec<ModelStatus>,
}

#[tauri::command]
pub fn rag_estado(state: State<'_, AppState>) -> EstadoMemoria {
    // O fio decide a injeção; a tela mostra o que o núcleo realmente está
    // fazendo com ela, lido do grafo aplicado, não de um interruptor próprio.
    let fio = state.manager.ferramenta_ativa(local::CONECTOR, "buscar_memoria");
    EstadoMemoria {
        estado: state.memoria.estado(fio),
        codificadores: catalog::status_embeddings(&state.ai.models_root),
    }
}

/// Grava as escalas da memória e devolve o que valeu depois das faixas: o
/// número que a tela mostra é o número que o núcleo usou.
#[tauri::command]
pub fn rag_ajustar(
    state: State<'_, AppState>,
    ajustes: memoria::Ajustes,
) -> Result<memoria::Ajustes, String> {
    state.memoria.ajustar(ajustes).map_err(report)
}

/// Manda ler as pastas concedidas e as conversas salvas, e vectorizar o que
/// mudou. Devolve na hora: a varredura é longa e o progresso chega por evento,
/// como no download dos pesos. O erro já saiu como `Evento::Falhou` e ficou
/// guardado no estado, por isso aqui só o fio que não terminou é erro deste
/// comando.
#[tauri::command]
pub async fn rag_indexar(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let memoria = Arc::clone(&state.memoria);
    let conversas = Arc::clone(&state.conversas);
    tauri::async_runtime::spawn_blocking(move || {
        let emit = move |evento: memoria::Evento| {
            let _ = app.emit("memoria-evento", &evento);
        };
        let _ = memoria.indexar(&conversas.para_memoria(), emit);
    })
    .await
    .map_err(|e| tr("erros.memoriaIndexacaoNaoTerminou", &[("detalhe", &e.to_string())]))?;
    Ok(())
}

/// Pede a parada da varredura em curso. `false` é "não havia nada para parar",
/// e a tela distingue as duas coisas.
#[tauri::command]
pub fn rag_indexar_cancelar(state: State<'_, AppState>) -> bool {
    state.memoria.cancelar()
}

/// Apaga o índice. Os pesos do codificador ficam onde estão: o que sai é o
/// trabalho de ler as pastas, e ele é refeito a qualquer momento.
#[tauri::command]
pub fn rag_apagar(state: State<'_, AppState>) -> Result<(), String> {
    state.memoria.apagar().map_err(report)
}

/// Busca direta do usuário para a memória, sem passar pelo modelo: o mesmo `k`,
/// o mesmo piso e a mesma política de pastas da ferramenta. A primeira busca
/// carrega o codificador, por isso não roda na thread do comando.
#[tauri::command]
pub async fn memoria_buscar(
    state: State<'_, AppState>,
    consulta: String,
    trechos: u32,
) -> Result<Resposta, String> {
    let memoria = Arc::clone(&state.memoria);
    let conversas = Arc::clone(&state.conversas);
    tauri::async_runtime::spawn_blocking(move || {
        // O teste procura como o turno procura, e isso inclui estar atrás do
        // histórico: sem esta linha a tela mostraria "nada encontrado" para uma
        // conversa que o turno de agora já lembraria.
        let _ = memoria.revalidar_conversas(&conversas.para_memoria());
        memoria.buscar(&consulta, trechos, &[])
    })
        .await
        .map_err(|e| tr("erros.memoriaBuscaNaoTerminou", &[("detalhe", &e.to_string())]))?
        .map_err(report)
}

// ---------------------------------------------------------------------------
// Base de conhecimento
// ---------------------------------------------------------------------------

/// O censo da pasta `base/` com o que o índice hoje sabe dela. Os dois números
/// não se igualam por acaso: nota escrita com o codificador descarregado está na
/// pasta e fora do índice, e é a tela dizendo isso em voz alta que evita o
/// "escrevi e ele não lembra".
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EstadoBase {
    /// Notas no disco agora, contidas na pasta.
    pub notas: usize,
    /// Bytes somados desses arquivos.
    pub bytes: u64,
    pub fixadas: usize,
    /// Peças de nota que têm vetor no índice. Menor que `notas` é a base escrita
    /// e ainda não aprendida, e é este par que a tela mostra junto.
    pub trechos_notas: usize,
    /// A varredura está segurando o índice: uma sincronização agora devolveria
    /// zero peças sem que nada esteja errado.
    pub indexando: bool,
    /// Por que a injeção automática vale ou não. A mesma palavra de `rag_estado`,
    /// porque é o mesmo motor decidindo.
    pub auto: &'static str,
}

/// O que o disco respondeu e o que o índice aprendeu, juntos de propósito: uma
/// escrita pela tela é o ciclo inteiro, e devolver só a primeira metade deixaria
/// a interface dizer "salvo" quando nenhuma peça entrou.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Sincronizada {
    pub registro: base::Registro,
    pub sincronia: memoria::Sincronia,
    /// `true` só quando o id não estava na pasta antes da escrita. Prender uma
    /// nota que já estava lá responde `false`, e a tela não anuncia novidade.
    pub nova: bool,
}

#[tauri::command]
pub fn base_estado(state: State<'_, AppState>) -> Result<EstadoBase, String> {
    let lista = state.memoria.notas().map_err(report)?;
    let fio = state.manager.ferramenta_ativa(local::CONECTOR, "buscar_memoria");
    let estado = state.memoria.estado(fio);
    Ok(EstadoBase {
        notas: lista.notas,
        bytes: lista.bytes,
        fixadas: lista.fixadas,
        trechos_notas: estado.trechos_notas,
        indexando: estado.indexando,
        auto: estado.auto,
    })
}

/// As notas com o corpo delas, da mais mexida para a menos. É a lista que a tela
/// mostra e o editor abre; o censo vem junto, medido na mesma leitura.
#[tauri::command]
pub fn base_listar(state: State<'_, AppState>) -> Result<base::Lista, String> {
    state.memoria.notas().map_err(report)
}

/// O ciclo completo de uma nota escrita pela tela: grava o arquivo e põe o índice
/// atrás dele. Roda fora da thread do comando porque a segunda metade pode
/// carregar o codificador — a mesma razão de `memoria_buscar` ser assim.
#[tauri::command]
pub async fn base_escrever(
    state: State<'_, AppState>,
    titulo: String,
    corpo: String,
    nota: Option<String>,
    fixar: Option<bool>,
) -> Result<Sincronizada, String> {
    let memoria = Arc::clone(&state.memoria);
    tauri::async_runtime::spawn_blocking(move || -> Result<Sincronizada, String> {
        let raiz = memoria.raiz_base();
        // Lido antes da escrita: depois de o arquivo existir não há mais como
        // saber se a pasta já o conhecia.
        let antes = base::ids(raiz).map_err(report)?;
        let registro =
            base::escrever(raiz, &titulo, &corpo, nota.as_deref(), fixar).map_err(report)?;
        let nova = !antes.iter().any(|id| *id == registro.nota.id);
        let itens = lista_de_notas(&memoria)?;
        let sincronia = memoria.sincronizar_base(&itens).map_err(report)?;
        Ok(Sincronizada { registro, sincronia, nova })
    })
    .await
    .map_err(|e| tr("erros.baseNaoTerminou", &[("detalhe", &e.to_string())]))?
}

/// Apaga uma nota e tira as peças dela do índice na mesma viagem. Sem a
/// segunda metade, apagar o arquivo deixaria o texto lembrado — e memória que o
/// dono mandou esquecer não pode voltar na busca seguinte.
#[tauri::command]
pub async fn base_apagar(state: State<'_, AppState>, id: String) -> Result<base::Removida, String> {
    let memoria = Arc::clone(&state.memoria);
    tauri::async_runtime::spawn_blocking(move || -> Result<base::Removida, String> {
        let raiz = memoria.raiz_base();
        let removida = base::apagar(raiz, &id).map_err(report)?;
        let itens = lista_de_notas(&memoria)?;
        // A lista que entra já não tem o arquivo, e é por diferença que a peça sai.
        memoria.sincronizar_base(&itens).map_err(report)?;
        Ok(removida)
    })
    .await
    .map_err(|e| tr("erros.baseNaoTerminou", &[("detalhe", &e.to_string())]))?
}

/// Prende ou solta uma nota. Presa, ela entra no turno sem ser buscada, e é o
/// índice quem carrega essa decisão junto com a ficha da nota — por isso a
/// fixação sincroniza também, em vez de mexer só no arquivo.
#[tauri::command]
pub async fn base_fixar(
    state: State<'_, AppState>,
    id: String,
    fixada: bool,
) -> Result<Sincronizada, String> {
    let memoria = Arc::clone(&state.memoria);
    tauri::async_runtime::spawn_blocking(move || -> Result<Sincronizada, String> {
        let registro = base::fixar(memoria.raiz_base(), &id, fixada).map_err(report)?;
        let itens = lista_de_notas(&memoria)?;
        let sincronia = memoria.sincronizar_base(&itens).map_err(report)?;
        Ok(Sincronizada { registro, sincronia, nova: false })
    })
    .await
    .map_err(|e| tr("erros.baseNaoTerminou", &[("detalhe", &e.to_string())]))?
}

/// As notas como o disco as responde agora. Uma função só para os três comandos
/// acima porque a lista que entra no índice tem de ser a lida depois da escrita,
/// e não a que o pedido trouxe.
fn lista_de_notas(memoria: &Custodio) -> Result<Vec<Nota>, String> {
    Ok(memoria.notas().map_err(report)?.itens)
}

// ---------------------------------------------------------------------------
// Ajuste fino (adaptadores LoRA)
// ---------------------------------------------------------------------------

/// Os três comandos que mudam estado devolvem o estado inteiro, e não um "ok":
/// entre clicar e ver a resposta há uma cópia de arquivo e uma leitura de pesos
/// na placa, e o que a tela tem de mostrar é o que o motor respondeu depois
/// disso — não a intenção que tinha antes.
#[tauri::command]
pub fn adaptadores_estado(state: State<'_, AppState>) -> adaptadores::Estado {
    state.ai.estado_adaptadores()
}

/// Grava ligados e forças e põe os deltas sobre o modelo em memória. Um turno
/// em curso recusa: trocar pesos no meio de uma resposta entregaria um texto
/// escrito por dois modelos.
#[tauri::command]
pub async fn adaptadores_ajustar(
    state: State<'_, AppState>,
    ajustes: adaptadores::Ajustes,
) -> Result<adaptadores::Estado, String> {
    let ai = Arc::clone(&state.ai);
    tauri::async_runtime::spawn_blocking(move || ai.ajustar_adaptadores(ajustes))
        .await
        .map_err(|e| tr("erros.adaptadorOperacaoNaoTerminou", &[("detalhe", &e.to_string())]))?
        .map_err(report)
}

/// Importa um GGUF de adaptador para a pasta do aplicativo e já o liga na força
/// cheia. A cópia pode ser de duzentos megabytes, por isso não roda na thread do
/// comando.
#[tauri::command]
pub async fn adaptador_adicionar(
    state: State<'_, AppState>,
    caminho: String,
) -> Result<adaptadores::Estado, String> {
    let ai = Arc::clone(&state.ai);
    tauri::async_runtime::spawn_blocking(move || ai.importar_adaptador(Path::new(&caminho)))
        .await
        .map_err(|e| tr("erros.adaptadorOperacaoNaoTerminou", &[("detalhe", &e.to_string())]))?
        .map_err(report)
}

/// Apaga um adaptador: o arquivo da pasta e o pin que o ligava. Um arquivo da
/// placa em uso não sai do disco, daí a recusa quando há turno em curso.
#[tauri::command]
pub async fn adaptador_apagar(
    state: State<'_, AppState>,
    caminho: String,
) -> Result<adaptadores::Estado, String> {
    let ai = Arc::clone(&state.ai);
    tauri::async_runtime::spawn_blocking(move || ai.apagar_adaptador(Path::new(&caminho)))
        .await
        .map_err(|e| tr("erros.adaptadorOperacaoNaoTerminou", &[("detalhe", &e.to_string())]))?
        .map_err(report)
}
// ─── Runtime nativo ───────────────────────────────────────────────────────────

/// O que está instalado e o que falta, nas pastas que o carregador procura. É
/// presença, não conteúdo: a conferência de hash é `runtime_verificar`, porque
/// ler 46 MB a cada consulta de estado prenderia a interface.
#[tauri::command]
pub fn runtime_estado(state: State<'_, AppState>) -> Vec<runtime::EstadoPacote> {
    runtime::estado(&state.ai.dlls)
}

/// Confere o conteúdo de cada DLL contra o SHA256SUMS.txt versionado. Roda fora
/// da thread da janela: são dezenas de megabytes de hash.
#[tauri::command]
pub async fn runtime_verificar(state: State<'_, AppState>) -> Result<Vec<runtime::Veredito>, String> {
    let dlls = state.ai.dlls.clone();
    tauri::async_runtime::spawn_blocking(move || runtime::verificar(&dlls))
        .await
        .map_err(|e| tr("erros.runtimeOperacaoNaoTerminou", &[("detalhe", &e.to_string())]))
}

/// Instala o que falta e devolve imediatamente: o progresso chega por evento, e
/// a UI não pode ficar presa num invoke de 46 MB.
#[tauri::command]
pub async fn runtime_instalar(
    app: AppHandle,
    state: State<'_, AppState>,
    ids: Vec<String>,
) -> Result<(), String> {
    let cancel = {
        let mut job = crate::state::trava_estado(&state.runtime_job);
        if job.is_some() {
            return Err(tr("erros.runtimeEmCurso", &[]));
        }
        let novo = Arc::new(AtomicBool::new(false));
        *job = Some(Arc::clone(&novo));
        novo
    };
    let destino = state.runtime_dir.clone();
    tauri::async_runtime::spawn(async move {
        let emit = {
            let app = app.clone();
            move |evento: runtime::Evento| {
                let _ = app.emit("runtime-evento", &evento);
            }
        };
        let _ = runtime::instalar(&destino, &ids, &cancel, emit).await;
        // O slot tem de ser devolvido mesmo quando a instalação falha: sem isto
        // a segunda tentativa diria "já em curso" para sempre.
        let estado = app.state::<AppState>();
        *crate::state::trava_estado(&estado.runtime_job) = None;
    });
    Ok(())
}

#[tauri::command]
pub fn runtime_cancelar(state: State<'_, AppState>) -> Result<(), String> {
    let job = crate::state::trava_estado(&state.runtime_job);
    match job.as_ref() {
        Some(cancel) => {
            cancel.store(true, Ordering::Relaxed);
            Ok(())
        }
        None => Err(tr("erros.runtimeNadaEmCurso", &[])),
    }
}

// ─── Bancada ──────────────────────────────────────────────────────────────────

/// Os casos da suíte, para a tela desenhar a lista antes de a primeira medida
/// existir. Um caso que ainda não rodou não é um caso aprovado.
#[tauri::command]
pub fn bancada_casos() -> Vec<&'static str> {
    crate::bancada::CASOS.to_vec()
}

/// O modelo que a bancada vai usar: o fixado pelo dono, senão o que já está em
/// memória, senão o menor que a máquina tem inteiro. É a ordem do turno de
/// conversa sem o roteador por meio — a bancada mede um modelo, não uma escolha.
fn bancada_spec(ai: &AiRuntime) -> Option<&'static ModelSpec> {
    let fixo = ai.ajustes().modelo;
    let fixo = fixo.trim();
    if !fixo.is_empty() {
        if let Some(spec) = catalog::find(fixo) {
            if catalog::status(&ai.models_root, spec).complete {
                return Some(spec);
            }
        }
    }
    if let Some(id) = ai.loaded_id() {
        if let Some(spec) = catalog::find(&id) {
            if catalog::status(&ai.models_root, spec).complete {
                return Some(spec);
            }
        }
    }
    catalog::CATALOG
        .iter()
        .filter(|m| m.genero != crate::ai::catalog::Genero::Embeddings)
        .filter(|m| catalog::status(&ai.models_root, m).complete)
        .min_by_key(|m| m.weights_bytes)
}

/// Roda a suíte e devolve na hora: as medidas chegam por evento, uma por caso,
/// porque a bancada gera texto de verdade e isso leva segundos.
#[tauri::command]
pub async fn bancada_rodar(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let ai = Arc::clone(&state.ai);
    let Some(reserva) = ai.reservar_dono() else {
        return Err(tr("erros.turnoEmAndamento", &[]));
    };
    let spec = bancada_spec(&ai);
    let cancel = reserva.cancel();
    let gerente = Arc::clone(&state.manager);
    let resultado = tauri::async_runtime::spawn_blocking(move || {
        let emit = {
            let app = app.clone();
            move |evento: crate::bancada::Evento| {
                let _ = app.emit("bancada-evento", &evento);
            }
        };
        let resultado = crate::bancada::rodar(&ai, &gerente, spec, &cancel, emit);
        drop(reserva);
        resultado
    })
    .await;
    resultado
        .map_err(|e| tr("erros.turnoNaoTerminou", &[("detalhe", &e.to_string())]))?;
    Ok(())
}

/// A bancada toma o motor como um turno toma, então é o mesmo cancelador que a
/// para. Não há segundo interruptor a divergir do primeiro.
#[tauri::command]
pub fn bancada_cancelar(state: State<'_, AppState>) -> Result<(), String> {
    let Some(turno) = state.ai.active_turn() else {
        return Err(tr("erros.nadaEmCurso", &[]));
    };
    state.ai.request_cancel(turno.id);
    state.manager.cancel_turn(turno.id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bancos::config::{conexao_de_teste, Config, Tipo};

    /// O que a tela pode marcar tem de ser o que a lista devolve. Uma flag que
    /// existe no cadastro e não aparece na vista é um interruptor que ninguém
    /// encosta: o motor obedece, o dono não sabe que ele existe.
    #[test]
    fn a_vista_carrega_o_canal_que_o_cadastro_marcou() {
        let mut config = Config::default();
        let mut conferida = conexao_de_teste(Tipo::Postgres);
        conferida.tls_verificado = true;
        config.conexoes.push(conferida);

        let vista = serde_json::to_value(vista_conexoes(&config)).expect("serializa a vista");
        assert_eq!(
            vista[0]["tlsVerificado"],
            serde_json::Value::Bool(true),
            "a vista conta à tela o canal que o cadastro guarda: {vista}"
        );
    }
}
