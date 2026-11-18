import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  AgentMode,
  AiStatus,
  AjustesAdaptadores,
  AjustesGeracao,
  AjustesMemoria,
  AjustesVoz,
  BancadaEvento,
  CallToolResult,
  ChatEvent,
  ChatMessage,
  ConexaoBanco,
  ConexaoRegistro,
  Connector,
  EstadoAdaptadores,
  EstadoConversas,
  EstadoLigacoes,
  EstadoMemoria,
  EstadoVoz,
  FichaConversa,
  GrantedFolder,
  McpEvent,
  MemoriaEvento,
  MedidorVoz,
  ModelEvent,
  PendingApproval,
  Plano,
  Posicao,
  RespostaMemoria,
  RuntimeEvento,
  RuntimePacote,
  RuntimeVeredito,
  ServerEvent,
  ServidorCatalogo,
  ServidorMcpEstado,
  ToolInfo,
  TurnOutcome,
  VozEvento,
} from "./types";

export const api = {
  listConnectors: () => invoke<Connector[]>("list_connectors"),
  saveConnector: (connector: Connector) =>
    invoke<void>("save_connector", { connector }),
  removeConnector: (id: string) => invoke<void>("remove_connector", { id }),
  connectConnector: (id: string) =>
    invoke<ToolInfo[]>("connect_connector", { id }),
  disconnectConnector: (id: string) =>
    invoke<void>("disconnect_connector", { id }),
  connectedConnectors: () => invoke<string[]>("connected_connectors"),
  connectorTools: (id: string) => invoke<ToolInfo[]>("connector_tools", { id }),
  toolCatalog: () => invoke<string>("tool_catalog"),
  resolveIntent: (query: string) =>
    invoke<ToolInfo[]>("resolve_intent", { query }),
  allTools: () => invoke<ToolInfo[]>("all_tools"),
  callTool: (qualifiedName: string, args: unknown) =>
    invoke<CallToolResult>("call_tool", { qualifiedName, arguments: args }),
  pendingApprovals: () => invoke<PendingApproval[]>("pending_approvals"),
  resolveApproval: (id: string, approved: boolean) =>
    invoke<boolean>("resolve_approval", { id, approved }),
  permissionMode: () => invoke<AgentMode>("permission_mode"),
  setPermissionMode: (mode: AgentMode) =>
    invoke<void>("set_permission_mode", { mode }),
  sessionGrants: () => invoke<string[]>("session_grants"),
  grantSession: (connectorId: string) =>
    invoke<void>("grant_session", { connectorId }),
  revokeSession: (connectorId: string) =>
    invoke<void>("revoke_session", { connectorId }),
  aiStatus: () => invoke<AiStatus>("ai_status"),
  aiAjustar: (ajustes: AjustesGeracao) =>
    invoke<AjustesGeracao>("ai_ajustar", { ajustes }),
  modelDownload: (id: string) => invoke<void>("model_download", { id }),
  modelDownloadCancel: (id: string) =>
    invoke<boolean>("model_download_cancel", { id }),
  modelDelete: (id: string) => invoke<void>("model_delete", { id }),
  modelLoad: (id: string) => invoke<string>("model_load", { id }),
  modelSolto: () => invoke<AjustesGeracao>("model_solto"),
  chatHistory: () => invoke<ChatMessage[]>("chat_history"),
  chatReset: () => invoke<void>("chat_reset"),
  chatCancel: () => invoke<void>("chat_cancel"),
  chatSend: (text: string) => invoke<TurnOutcome>("chat_send", { text }),
  conversasListar: () => invoke<EstadoConversas>("conversas_listar"),
  conversaNova: () => invoke<EstadoConversas>("conversa_nova"),
  conversaAbrir: (id: string) => invoke<ChatMessage[]>("conversa_abrir", { id }),
  conversaRenomear: (id: string, titulo: string) =>
    invoke<FichaConversa>("conversa_renomear", { id, titulo }),
  conversaApagar: (id: string) => invoke<EstadoConversas>("conversa_apagar", { id }),
  grantedFolders: () => invoke<GrantedFolder[]>("granted_folders"),
  grantFolder: (caminho: string) =>
    invoke<GrantedFolder[]>("grant_folder", { caminho }),
  revokeFolder: (caminho: string) =>
    invoke<GrantedFolder[]>("revoke_folder", { caminho }),
  ligacoesObter: () => invoke<EstadoLigacoes>("ligacoes_obter"),
  ligacoesAplicar: (plano: Plano) =>
    invoke<EstadoLigacoes>("ligacoes_aplicar", { plano }),
  ligacoesLayout: (posicoes: Posicao[]) =>
    invoke<void>("ligacoes_layout", { posicoes }),
  mcpCatalog: () => invoke<ServidorCatalogo[]>("mcp_catalog"),
  mcpInstall: (id: string) => invoke<void>("mcp_install", { id }),
  mcpInstallCancel: (id: string) => invoke<boolean>("mcp_install_cancel", { id }),
  mcpUninstall: (id: string) => invoke<void>("mcp_uninstall", { id }),
  mcpServidorEstado: () => invoke<ServidorMcpEstado>("mcp_servidor_estado"),
  mcpServidorPublicar: () => invoke<ServidorMcpEstado>("mcp_servidor_publicar"),
  mcpServidorFechar: () => invoke<boolean>("mcp_servidor_fechar"),
  mcpServidorToken: () => invoke<string | null>("mcp_servidor_token"),
  bancosListar: () => invoke<ConexaoBanco[]>("bancos_listar"),
  bancosSalvar: (conexao: ConexaoRegistro, senha: string | null) =>
    invoke<ConexaoBanco[]>("bancos_salvar", { conexao, senha }),
  bancosRemover: (id: string) => invoke<ConexaoBanco[]>("bancos_remover", { id }),
  bancosDescobrir: (conexao: ConexaoRegistro, senha: string | null) =>
    invoke<string[]>("bancos_descobrir", { conexao, senha }),
  definirIdioma: (id: string) => invoke<string>("set_locale", { id }),
  vozEstado: () => invoke<EstadoVoz>("voz_estado"),
  vozMedidor: () => invoke<MedidorVoz>("voz_medidor"),
  vozAjustar: (ajustes: AjustesVoz) =>
    invoke<AjustesVoz>("voz_ajustar", { ajustes }),
  vozFalar: (texto: string) => invoke<void>("voz_falar", { texto }),
  vozPrever: (texto: string, idVoz: string, sid: number, velocidade: number) =>
    invoke<void>("voz_prever", { texto, idVoz, sid, velocidade }),
  vozParar: () => invoke<void>("voz_parar"),
  vozOuvir: () => invoke<void>("voz_ouvir"),
  vozTranscrever: () => invoke<string>("voz_transcrever"),
  vozCancelarEscuta: () => invoke<void>("voz_cancelar_escuta"),
  vozBaixar: (id: string) => invoke<void>("voz_baixar", { id }),
  vozBaixarCancelar: (id: string) =>
    invoke<boolean>("voz_baixar_cancelar", { id }),
  vozApagar: (id: string) => invoke<void>("voz_apagar", { id }),
  ragEstado: () => invoke<EstadoMemoria>("rag_estado"),
  ragAjustar: (ajustes: AjustesMemoria) =>
    invoke<AjustesMemoria>("rag_ajustar", { ajustes }),
  ragIndexar: () => invoke<void>("rag_indexar"),
  ragIndexarCancelar: () => invoke<boolean>("rag_indexar_cancelar"),
  ragApagar: () => invoke<void>("rag_apagar"),
  memoriaBuscar: (consulta: string, trechos: number) =>
    invoke<RespostaMemoria>("memoria_buscar", { consulta, trechos }),
  adaptadoresEstado: () =>
    invoke<EstadoAdaptadores>("adaptadores_estado"),
  adaptadoresAjustar: (ajustes: AjustesAdaptadores) =>
    invoke<EstadoAdaptadores>("adaptadores_ajustar", { ajustes }),
  adaptadorAdicionar: (caminho: string) =>
    invoke<EstadoAdaptadores>("adaptador_adicionar", { caminho }),
  adaptadorApagar: (caminho: string) =>
    invoke<EstadoAdaptadores>("adaptador_apagar", { caminho }),
  runtimeEstado: () => invoke<RuntimePacote[]>("runtime_estado"),
  runtimeVerificar: () => invoke<RuntimeVeredito[]>("runtime_verificar"),
  runtimeInstalar: (ids: string[]) => invoke<void>("runtime_instalar", { ids }),
  runtimeCancelar: () => invoke<void>("runtime_cancelar"),
  bancadaCasos: () => invoke<string[]>("bancada_casos"),
  bancadaRodar: () => invoke<void>("bancada_rodar"),
  bancadaParar: () => invoke<void>("bancada_cancelar"),
};

export function onVozEvent(
  handler: (event: VozEvento) => void,
): Promise<UnlistenFn> {
  return listen<VozEvento>("voz-evento", (event) => handler(event.payload));
}

export function onChatEvent(
  handler: (event: ChatEvent) => void,
): Promise<UnlistenFn> {
  return listen<ChatEvent>("chat-event", (event) => handler(event.payload));
}

export function onMcpEvent(
  handler: (event: McpEvent) => void,
): Promise<UnlistenFn> {
  return listen<McpEvent>("mcp-event", (event) => handler(event.payload));
}

export function onMcpLagged(
  handler: (missed: number) => void,
): Promise<UnlistenFn> {
  return listen<number>("mcp-events-lagged", (event) => handler(event.payload));
}

export function onModelEvent(
  handler: (event: ModelEvent) => void,
): Promise<UnlistenFn> {
  return listen<ModelEvent>("model-event", (event) => handler(event.payload));
}

export function onServerEvent(
  handler: (event: ServerEvent) => void,
): Promise<UnlistenFn> {
  return listen<ServerEvent>("server-event", (event) => handler(event.payload));
}

export function onMemoriaEvent(
  handler: (event: MemoriaEvento) => void,
): Promise<UnlistenFn> {
  return listen<MemoriaEvento>("memoria-evento", (event) =>
    handler(event.payload),
  );
}

export function onRuntimeEvent(
  handler: (event: RuntimeEvento) => void,
): Promise<UnlistenFn> {
  return listen<RuntimeEvento>("runtime-evento", (event) =>
    handler(event.payload),
  );
}

export function onBancadaEvent(
  handler: (event: BancadaEvento) => void,
): Promise<UnlistenFn> {
  return listen<BancadaEvento>("bancada-evento", (event) =>
    handler(event.payload),
  );
}

export function message(error: unknown): string {
  return typeof error === "string" ? error : JSON.stringify(error);
}