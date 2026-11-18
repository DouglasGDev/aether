pub mod ai;
pub mod bancada;
pub mod bancos;
pub mod commands;
pub mod config;
pub mod connectors;
pub mod conversas;
pub mod idiomas;
pub mod ligacoes;
pub mod local;
pub mod mcp;
pub mod permissions;
pub mod runtime;
pub mod state;
pub mod voz;

use config::McpConfig;
use local::pastas::Pastas;
use mcp::manager::McpManager;
use permissions::{AgentMode, PermissionGate};
use state::AppState;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tauri::{Emitter, Manager};
use tokio::sync::broadcast::error::RecvError;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let config_path = Arc::new(app.path().app_config_dir()?.join("mcp.json"));
            let config = Arc::new(Mutex::new(McpConfig::load(&config_path)?));
            let registry = config.lock().expect("config envenenada").registry()?;
            let segredos = Arc::new(crate::config::Segredos::default());
            let gate = Arc::new(Mutex::new(PermissionGate::new(AgentMode::Default)));
            let pastas_path = app.path().app_config_dir()?.join("pastas.json");
            let pastas = Arc::new(RwLock::new(Pastas::carregar(&pastas_path)?));
            let models_root = app.path().app_local_data_dir()?.join("models");
            // A mesma lista serve os dois motores nativos: é ao lado do
            // executável que `build.rs` publica a ggml e a sherpa-onnx.
            let dlls = pastas_de_dll(app);
            let runtime_dir = runtime::pasta(&app.path().app_local_data_dir()?);
            let geracao_path = app.path().app_config_dir()?.join("geracao.json");
            let adaptadores_path = app.path().app_config_dir()?.join("adaptadores.json");
            let ai = Arc::new(state::AiRuntime::new(
                models_root.clone(),
                dlls.clone(),
                geracao_path,
                adaptadores_path,
            ));
            let conversas = Arc::new(conversas::Conversas::novo(
                app.path().app_config_dir()?.join("conversas"),
            ));
            let servidores_root = app.path().app_local_data_dir()?.join("mcp-servers");
            let ligacoes_path = app.path().app_config_dir()?.join("ligacoes.json");
            // O cadastro de conexões de banco: conexões, senhas cifradas e a
            // política por tabela que o dono escolheu.
            let bancos_path = Arc::new(app.path().app_config_dir()?.join("bancos.json"));
            // O depósito dos resultados longos de ferramenta. Fica no dado local,
            // ao lado dos modelos: é corpo de conversa, não escolha de configuração.
            let derrame_path = Arc::new(app.path().app_local_data_dir()?.join("resultados"));
            // A memória tem pasta própria dentro de `models`: apagar um LLM de
            // conversa não leva o índice nem o codificador junto.
            let memoria = Arc::new(ai::memoria::Custodio::novo(
                &dlls,
                &models_root,
                &models_root.join("memoria"),
                app.path().app_config_dir()?.join("rag.json"),
                Arc::clone(&pastas),
            ));
            let manager = Arc::new(McpManager::new(
                registry,
                Arc::clone(&gate),
                Arc::clone(&pastas),
                Arc::clone(&memoria),
                Arc::clone(&config),
                Arc::clone(&config_path),
                Arc::clone(&segredos),
                Arc::clone(&bancos_path),
                Arc::clone(&derrame_path),
            ));
            // Os modelos de voz têm pasta própria dentro de `models`: apagar um
            // LLM não leva o Kokoro junto, e vice-versa.
            let voz = Arc::new(voz::MotorVoz::novo(
                &dlls,
                models_root.join("voz"),
                app.path().app_config_dir()?.join("voz.json"),
            ));

            // O conector embutido já entra conectado: não há processo filho nem
            // rede para esperar. Quem limita o que ele pode fazer são as pastas
            // concedidas e o gate de permissões, não um botão de conectar.
            tauri::async_runtime::block_on(manager.connect(local::CONECTOR))?;

            app.manage(AppState {
                manager: Arc::clone(&manager),
                gate,
                config,
                config_path,
                segredos,
                ai,
                pastas,
                pastas_path,
                memoria,
                servidores_root,
                instalacoes: mcp::instalador::Instalacoes::novo(),
                ligacoes_path,
                bancos_path: (*bancos_path).clone(),
                derrame_path: (*derrame_path).clone(),
                voz,
                runtime_dir,
                runtime_job: Mutex::new(None),
                conversas,
            });

            let handle = app.handle().clone();
            let mut events = manager.subscribe();
            tauri::async_runtime::spawn(async move {
                loop {
                    match events.recv().await {
                        Ok(event) => {
                            let _ = handle.emit("mcp-event", &event);
                        }
                        // A lagged receiver must not silently hide state from the
                        // UI: tell it how many events it missed so it can refetch.
                        Err(RecvError::Lagged(missed)) => {
                            let _ = handle.emit("mcp-events-lagged", missed);
                        }
                        Err(RecvError::Closed) => break,
                    }
                }
            });

            let expiry = Arc::clone(&manager);
            tauri::async_runtime::spawn(async move {
                let mut tick = tokio::time::interval(Duration::from_secs(30));
                loop {
                    tick.tick().await;
                    expiry.expire_approvals();
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_connectors,
            commands::save_connector,
            commands::remove_connector,
            commands::connect_connector,
            commands::disconnect_connector,
            commands::connected_connectors,
            commands::connector_tools,
            commands::tool_catalog,
            commands::resolve_intent,
            commands::all_tools,
            commands::call_tool,
            commands::pending_approvals,
            commands::resolve_approval,
            commands::permission_mode,
            commands::set_permission_mode,
            commands::session_grants,
            commands::grant_session,
            commands::revoke_session,
            commands::granted_folders,
            commands::grant_folder,
            commands::revoke_folder,
            commands::mcp_catalog,
            commands::mcp_install,
            commands::mcp_install_cancel,
            commands::mcp_uninstall,
            commands::mcp_servidor_estado,
            commands::mcp_servidor_publicar,
            commands::mcp_servidor_fechar,
            commands::mcp_servidor_token,
            commands::bancos_listar,
            commands::bancos_salvar,
            commands::bancos_remover,
            commands::bancos_descobrir,
            commands::ligacoes_obter,
            commands::ligacoes_aplicar,
            commands::ligacoes_layout,
            commands::set_locale,
            commands::ai_status,
            commands::ai_ajustar,
            commands::model_download,
            commands::model_download_cancel,
            commands::model_delete,
            commands::model_load,
            commands::model_solto,
            commands::chat_history,
            commands::chat_reset,
            commands::chat_cancel,
            commands::chat_send,
            commands::conversas_listar,
            commands::conversa_nova,
            commands::conversa_abrir,
            commands::conversa_renomear,
            commands::conversa_apagar,
            commands::voz_estado,
            commands::voz_medidor,
            commands::voz_ajustar,
            commands::voz_falar,
            commands::voz_prever,
            commands::voz_parar,
            commands::voz_ouvir,
            commands::voz_transcrever,
            commands::voz_cancelar_escuta,
            commands::voz_baixar,
            commands::voz_baixar_cancelar,
            commands::voz_apagar,
            commands::rag_estado,
            commands::rag_ajustar,
            commands::rag_indexar,
            commands::rag_indexar_cancelar,
            commands::rag_apagar,
            commands::memoria_buscar,
            commands::adaptadores_estado,
            commands::adaptadores_ajustar,
            commands::adaptador_adicionar,
            commands::adaptador_apagar,
            commands::runtime_estado,
            commands::runtime_verificar,
            commands::runtime_instalar,
            commands::runtime_cancelar,
            commands::bancada_casos,
            commands::bancada_rodar,
            commands::bancada_cancelar,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Aether");
}

/// Onde o carregador pode achar as DLLs nativas — `sherpa-onnx-c-api.dll` para a
/// voz e `llama.dll`/`ggml*.dll` para a inferência. É ao lado do executável que
/// o `build.rs` as publica, que é como o `cargo tauri dev` e o instalador as
/// distribui, e na pasta de recursos do pacote. A ordem importa — a primeira que
/// existir vence, e é dela que o `onnxruntime.dll` tem de vir junto.
///
/// A pasta do runtime baixado pelo aplicativo entra no meio: num PC onde o
/// instalador não levou as DLLs, é ela que atende, e o conjunto tem de estar
/// inteiro numa pasta só — a `ggml.dll` carrega os próprios backends da pasta de
/// onde foi aberta.
fn pastas_de_dll(app: &tauri::App) -> Vec<PathBuf> {
    let mut pastas = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(pai) = exe.parent() {
            pastas.push(pai.to_path_buf());
        }
    }
    if let Ok(dados) = app.path().app_local_data_dir() {
        pastas.push(runtime::pasta(&dados));
    }
    if let Ok(recursos) = app.path().resource_dir() {
        pastas.push(recursos);
    }
    pastas
}