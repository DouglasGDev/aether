use crate::ai::adaptadores;
use crate::ai::catalog::{self, ModelSpec, ModelStatus};
use crate::ai::download;
use crate::ai::geracao;
use crate::ai::memoria::{self, Custodio, Resposta};
use crate::ai::router::{self, Choice, Request};
use crate::ai::session::{self, SessionEvent, TurnConfig, TurnOutcome};
use crate::ai::{Alvo, ChatMessage, ToolCall, ToolSpec};
use crate::connectors::Connector;
use crate::conversas;
use crate::idiomas::{self, tr};
use crate::ligacoes;
use crate::local::{self, pastas::Pastas};
use crate::mcp::catalog::CatalogBudget;
use crate::mcp::instalador::{self, ServidorEvent};
use crate::mcp::manager::McpManager;
use crate::mcp::tool::ToolInfo;
use crate::permissions::{AgentMode, ToolEffect};
use crate::runtime;
use crate::state::{AiRuntime, AppState};
use crate::voz::{self, Ajustes, EstadoVoz, VozEvento};
use rmcp::model::CallToolResult;
use serde::Serialize;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Instant;
use tauri::{AppHandle, Emitter, Manager, State};

fn report(error: anyhow::Error) -> String {
    format!("{error:#}")
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
    if connector.id == local::CONECTOR {
        return Err(tr("erros.conectorReservado", &[("id", &connector.id)]));
    }
    let mut config = state.config.lock().expect("config lock poisoned");
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
    if state.manager.connected_ids().contains(&id) {
        state.manager.disconnect(&id).await.map_err(report)?;
    }
    let mut config = state.config.lock().expect("config lock poisoned");
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
    let gate = state.gate.lock().expect("permission gate poisoned");
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
    state.gate.lock().expect("permission gate poisoned").mode()
}

#[tauri::command]
pub fn set_permission_mode(state: State<'_, AppState>, mode: AgentMode) {
    state
        .gate
        .lock()
        .expect("permission gate poisoned")
        .set_mode(mode);
}

#[tauri::command]
pub fn session_grants(state: State<'_, AppState>) -> Vec<String> {
    state
        .gate
        .lock()
        .expect("permission gate poisoned")
        .session_grants()
}

#[tauri::command]
pub fn grant_session(state: State<'_, AppState>, connector_id: String) {
    state
        .gate
        .lock()
        .expect("permission gate poisoned")
        .grant_session(&connector_id);
}

#[tauri::command]
pub fn revoke_session(state: State<'_, AppState>, connector_id: String) {
    state
        .gate
        .lock()
        .expect("permission gate poisoned")
        .revoke_session(&connector_id);
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
    vista_pastas(&state.pastas.read().expect("pastas envenenadas"))
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
        let mut pastas = state.pastas.write().expect("pastas envenenadas");
        pastas.conceder(&caminho).map_err(report)?;
        pastas.salvar(&state.pastas_path).map_err(report)?;
    }
    reacoplar_pastas(&app, &Arc::clone(&state.manager)).await;
    Ok(vista_pastas(&state.pastas.read().expect("pastas envenenadas")))
}

#[tauri::command]
pub async fn revoke_folder(
    app: AppHandle,
    state: State<'_, AppState>,
    caminho: String,
) -> Result<Vec<PastaView>, String> {
    {
        let mut pastas = state.pastas.write().expect("pastas envenenadas");
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
    Ok(vista_pastas(&state.pastas.read().expect("pastas envenenadas")))
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
        let mut config = state.config.lock().expect("config lock poisoned");
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

    let mut config = state.config.lock().expect("config lock poisoned");
    config.remove(&id);
    let registry = config.registry().map_err(|e| e.to_string())?;
    config.save(&state.config_path).map_err(report)?;
    state.manager.replace_registry(registry);
    let _ = app.emit("server-event", ServidorEvent::Removeu { id });
    Ok(())
}

/// Trocar o idioma remonta o cadastro e o catálogo embutido: o nome e a
/// descrição que o modelo lê passam a vir na língua que o usuário escolheu.
#[tauri::command]
pub fn set_locale(state: State<'_, AppState>, id: String) -> Result<String, String> {
    if !idiomas::definir(&id) {
        return Err(tr("erros.idiomaDesconhecido", &[("id", &id)]));
    }
    let registry = {
        let config = state.config.lock().expect("config lock poisoned");
        config.registry().map_err(|e| e.to_string())?
    };
    state.manager.replace_registry(registry);
    state.manager.recarregar_embutido();

    let mut history = state.ai.history.lock().expect("histórico envenenado");
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
        turns: ai.history.lock().expect("histórico envenenado").len(),
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
    state.ai.history.lock().expect("histórico envenenado").clone()
}

#[tauri::command]
pub fn chat_reset(state: State<'_, AppState>) -> Result<(), String> {
    state.ai.request_cancel();
    state.ai.history.lock().expect("histórico envenenado").clear();
    *state.ai.resumo.lock().expect("resumo envenenado") = None;
    // O arquivo tem de dizer o mesmo que o buffer vivo: uma conversa limpa na
    // tela e cheia no disco voltaria cheia no próximo arranque.
    if let Some(id) = state.conversas.estado().ativa {
        state.conversas.gravar(&id, &[], None).map_err(report)?;
    }
    Ok(())
}

#[tauri::command]
pub fn chat_cancel(state: State<'_, AppState>) {
    state.ai.request_cancel();
}

// ─── Conversas salvas ─────────────────────────────────────────────────────────

#[tauri::command]
pub fn conversas_listar(state: State<'_, AppState>) -> conversas::Estado {
    state.conversas.estado()
}

/// Uma conversa nova. Se a ativa ainda não tem mensagem, ela já é nova: criar
/// outra deixaria uma vazia na lista a cada clique.
#[tauri::command]
pub fn conversa_nova(state: State<'_, AppState>) -> Result<conversas::Estado, String> {
    if state.ai.is_busy() {
        return Err(tr("erros.turnoEmAndamento", &[]));
    }
    let estado = state.conversas.estado();
    let vazia = estado
        .ativa
        .as_ref()
        .and_then(|id| estado.conversas.iter().find(|f| &f.id == id))
        .map(|f| f.mensagens == 0)
        .unwrap_or(false);
    if !vazia {
        state.conversas.criar().map_err(report)?;
    }
    state.ai.history.lock().expect("histórico envenenado").clear();
    *state.ai.resumo.lock().expect("resumo envenenado") = None;
    state.ai.esquecer();
    Ok(state.conversas.estado())
}

/// Troca a conversa ativa: o histórico do arquivo entra no buffer vivo, com o
/// resumo que pertence a ele, e o motor esquece o KV — porque o cache da
/// conversa anterior não é prefixo desta.
#[tauri::command]
pub fn conversa_abrir(state: State<'_, AppState>, id: String) -> Result<Vec<ChatMessage>, String> {
    if state.ai.is_busy() {
        return Err(tr("erros.turnoEmAndamento", &[]));
    }
    let aberta = state.conversas.abrir(&id).map_err(report)?;
    *state.ai.history.lock().expect("histórico envenenado") = aberta.mensagens.clone();
    *state.ai.resumo.lock().expect("resumo envenenado") = aberta.resumo.clone();
    state.ai.esquecer();
    Ok(aberta.mensagens)
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
    if state.ai.is_busy() {
        return Err(tr("erros.turnoEmAndamento", &[]));
    }
    let era_ativa = state.conversas.estado().ativa.as_deref() == Some(id.as_str());
    state.conversas.apagar(&id).map_err(report)?;
    if era_ativa {
        state.ai.history.lock().expect("histórico envenenado").clear();
        state.ai.esquecer();
    }
    Ok(state.conversas.estado())
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
    if !ai.begin() {
        return Err(tr("erros.turnoEmAndamento", &[]));
    }
    // A conversa tem de existir antes do turno: é nela que este histórico fica,
    // e um pedido sem conversa seria um pedido perdido ao fechar a janela.
    let id = match conversas.garantir() {
        Ok(id) => id,
        Err(erro) => {
            ai.end();
            return Err(report(erro));
        }
    };
    let done = Arc::clone(&ai);
    let voz = Arc::clone(&state.voz);
    let app_fala = app.clone();
    let joined = tauri::async_runtime::spawn_blocking(move || {
        let resultado = run_chat_turn(&app, &ai, &manager, &memoria, &pastas, &text);
        // Grava mesmo quando o turno falhou: o que o modelo já disse e as
        // ferramentas que já rodaram aconteceram, e a conversa é o registro
        // disso. O lock do histórico é tomado e largado antes do das conversas.
        let mensagens = ai.history.lock().expect("histórico envenenado").clone();
        let resumo = ai.resumo.lock().expect("resumo envenenado").clone();
        match (resultado, conversas.gravar(&id, &mensagens, resumo.as_ref())) {
            (Ok(desfecho), Ok(_)) => Ok(desfecho),
            // A causa do turno vem primeiro: ela é o que a pessoa precisa ver.
            (Err(erro), _) => Err(erro),
            (Ok(_), Err(erro)) => Err(report(erro)),
        }
    })
    .await;
    done.end();
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
    text: &str,
) -> Result<TurnOutcome, String> {
    let emit = |event: SessionEvent| {
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
    let raizes: Vec<String> = pastas
        .read()
        .expect("pastas envenenadas")
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
        let inicio = Instant::now();
        let resposta = memoria.buscar(text, 0).ok();
        match resposta.filter(|r| !r.achados.is_empty()) {
            Some(resposta) => {
                let Resposta { achados, alcancados, modo } = resposta;
                let texto = Custodio::texto_do_bloco(&achados);
                emit(SessionEvent::Memoria {
                    ferramenta: sinapse,
                    consulta: text.to_string(),
                    modo,
                    achados,
                    total: alcancados,
                    ms: inicio.elapsed().as_millis() as u64,
                });
                Some(texto)
            }
            None => None,
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
        emit(SessionEvent::Loading { model: starter.display.to_string() });
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
        emit(SessionEvent::Choice { choice: escolha.clone() });
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

    emit(SessionEvent::Loading { model: spec.display.to_string() });
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
        emit(SessionEvent::Choice {
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
    let resumo = ai.resumo.lock().expect("resumo envenenado").clone();
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
        resumo,
    };

    {
        let mut history = ai.history.lock().expect("histórico envenenado");
        if history.is_empty() {
            history.push(ChatMessage::new("system", session::system_prompt()));
        }
        history.push(ChatMessage::new("user", text));
    }

    let runtime = tokio::runtime::Handle::current();
    let run_tool = |call: &ToolCall| -> anyhow::Result<session::ToolOutcome> {
        let result = runtime.block_on(manager.call_tool(&call.name, call.arguments.clone()))?;
        let text = result_text(&result);
        if result.is_error.unwrap_or(false) {
            anyhow::bail!("{text}");
        }
        Ok(session::ToolOutcome { text, vista: result.structured_content.clone() })
    };

    let turno = session::run_turn(
        &mut loaded.engine,
        &loaded.template,
        &ai.history,
        &config,
        &ai.cancel,
        emit,
        run_tool,
    );
    if let Ok(desfecho) = &turno {
        // O resumo que o turno escreveu é do buffer vivo, como o histórico: sem
        // isto o próximo pedido dobraria de novo os mesmos turnos, na placa, para
        // chegar ao mesmo texto.
        *ai.resumo.lock().expect("resumo envenenado") = desfecho.resumo.clone();
    }
    if turno.is_err() {
        // O turno acima pôs o pedido no histórico antes de medir a janela, e
        // pode ter morrido antes do primeiro token: sem este desfazimento, cada
        // tentativa malsucedida soma uma linha igual e empurra a janela para
        // frente. Só desfaz enquanto a última linha for o pedido — depois que o
        // modelo falou (ou uma ferramenta rodou), aquilo aconteceu e fica.
        let mut history = ai.history.lock().expect("histórico envenenado");
        if history.last().map(|m| m.role.as_str()) == Some("user") {
            history.pop();
        }
    }
    turno.map_err(report)
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
        pastas: vista_pastas(&state.pastas.read().expect("pastas envenenadas")),
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
        let mut config = state.config.lock().expect("config lock poisoned");
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
        let mut pastas = state.pastas.write().expect("pastas envenenadas");
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

/// Manda ler as pastas concedidas e vectorizar o que mudou. Devolve na hora: a
/// varredura é longa e o progresso chega por evento, como no download dos
/// pesos. O erro já saiu como `Evento::Falhou` e ficou guardado no estado, por
/// isso aqui só o fio que não terminou é erro deste comando.
#[tauri::command]
pub async fn rag_indexar(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let memoria = Arc::clone(&state.memoria);
    tauri::async_runtime::spawn_blocking(move || {
        let emit = move |evento: memoria::Evento| {
            let _ = app.emit("memoria-evento", &evento);
        };
        let _ = memoria.indexar(emit);
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
    tauri::async_runtime::spawn_blocking(move || memoria.buscar(&consulta, trechos))
        .await
        .map_err(|e| tr("erros.memoriaBuscaNaoTerminou", &[("detalhe", &e.to_string())]))?
        .map_err(report)
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
        let mut job = state.runtime_job.lock().expect("runtime envenenado");
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
        *estado.runtime_job.lock().expect("runtime envenenado") = None;
    });
    Ok(())
}

#[tauri::command]
pub fn runtime_cancelar(state: State<'_, AppState>) -> Result<(), String> {
    let job = state.runtime_job.lock().expect("runtime envenenado");
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
    if !ai.begin() {
        return Err(tr("erros.turnoEmAndamento", &[]));
    }
    let spec = bancada_spec(&ai);
    let cancel = Arc::clone(&ai.cancel);
    let feito = Arc::clone(&ai);
    let resultado = tauri::async_runtime::spawn_blocking(move || {
        let emit = {
            let app = app.clone();
            move |evento: crate::bancada::Evento| {
                let _ = app.emit("bancada-evento", &evento);
            }
        };
        crate::bancada::rodar(&ai, spec, &cancel, emit)
    })
    .await;
    feito.end();
    resultado
        .map_err(|e| tr("erros.turnoNaoTerminou", &[("detalhe", &e.to_string())]))?;
    Ok(())
}

/// A bancada toma o motor como um turno toma, então é o mesmo cancelador que a
/// para: `ai.cancel`. Não há segundo interruptor a divergir do primeiro.
#[tauri::command]
pub fn bancada_cancelar(state: State<'_, AppState>) -> Result<(), String> {
    if !state.ai.is_busy() {
        return Err(tr("erros.nadaEmCurso", &[]));
    }
    state.ai.request_cancel();
    Ok(())
}
