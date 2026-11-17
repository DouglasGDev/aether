import { useCallback, useEffect, useRef, useState } from "react";
import { AdaptadoresDialog } from "./components/AdaptadoresDialog";
import { ApprovalCard } from "./components/ApprovalCard";
import { BancadaDialog } from "./components/BancadaDialog";
import { ArgumentForm } from "./components/ArgumentForm";
import { CapabilityCards } from "./components/CapabilityCards";
import { Chats } from "./components/Chats";
import { ConnectorNodes } from "./components/ConnectorNodes";
import { Conversation, emptyTurn, turnsDe, type Turn } from "./components/Conversation";
import { Core, type CorePhase } from "./components/Core";
import { GeracaoDialog } from "./components/GeracaoDialog";
import { GrafoNeural, type Disparo } from "./components/GrafoNeural";
import { Icon } from "./components/Icon";
import { IdiomaDialog } from "./components/IdiomaDialog";
import { IntentBar } from "./components/IntentBar";
import { MemoriaDialog } from "./components/MemoriaDialog";
import { ModelManager } from "./components/ModelManager";
import { PastasDialog } from "./components/PastasDialog";
import { ResultCard } from "./components/ResultCard";
import { RuntimeDialog } from "./components/RuntimeDialog";
import { ServerDialog } from "./components/ServerDialog";
import { TrustDial } from "./components/TrustDial";
import { VozDialog } from "./components/VozDialog";
import { api, message, onChatEvent, onMcpEvent, onMcpLagged, onVozEvent } from "./lib/api";
import { toolTitle } from "./lib/display";
import { useIdioma } from "./lib/i18n";
import type {
  AgentMode,
  AjustesGeracao,
  CallToolResult,
  ChatEvent,
  Connector,
  EstadoConversas,
  EstadoVoz,
  GrantedFolder,
  McpEvent,
  MedidorVoz,
  PendingApproval,
  RuntimePacote,
  ToolInfo,
  VozEvento,
} from "./lib/types";

const SEM_MEDIDOR: MedidorVoz = { falando: false, ouvindo: false, nivel: 0 };

export default function App() {
  const { id: idioma, t } = useIdioma();
  const [connectors, setConnectors] = useState<Connector[]>([]);
  const [connected, setConnected] = useState<string[]>([]);
  const [mode, setMode] = useState<AgentMode | null>(null);
  const [pending, setPending] = useState<PendingApproval[]>([]);
  const [requests, setRequests] = useState<Record<string, Record<string, unknown>>>({});
  const [capabilities, setCapabilities] = useState<ToolInfo[]>([]);
  const [pastas, setPastas] = useState<GrantedFolder[]>([]);
  const [concedidas, setConcedidas] = useState<string[]>([]);

  const [query, setQuery] = useState("");
  const [chosen, setChosen] = useState<ToolInfo | null>(null);
  const [result, setResult] = useState<CallToolResult | null>(null);
  const [resultTitle, setResultTitle] = useState("");

  const [calling, setCalling] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [showServers, setShowServers] = useState(false);
  const [showGeracao, setShowGeracao] = useState(false);
  const [showModels, setShowModels] = useState(false);
  const [showPastas, setShowPastas] = useState(false);
  const [showMemoria, setShowMemoria] = useState(false);
  const [showAjuste, setShowAjuste] = useState(false);
  const [showIdioma, setShowIdioma] = useState(false);
  const [showVoz, setShowVoz] = useState(false);
  const [showRuntime, setShowRuntime] = useState(false);
  const [showBancada, setShowBancada] = useState(false);
  /// O runtime nativo nas pastas do carregador. Incompleto é o estado de um PC
  /// novo: os pesos baixam e a inferência não abre porque a DLL não está lá.
  const [runtime, setRuntime] = useState<RuntimePacote[]>([]);
  const [turns, setTurns] = useState<Turn[]>([]);
  const [talking, setTalking] = useState(false);
  const [loaded, setLoaded] = useState<string | null>(null);
  /// As conversas salvas e qual delas está no buffer vivo. A lista vem do índice
  /// no disco: um turno grava a conversa e a lista relê título, data e contagem.
  const [chats, setChats] = useState<EstadoConversas>({ ativa: null, conversas: [] });
  const [showChats, setShowChats] = useState(true);

  /// O palco é o orbe ou o grafo, não uma folha por cima da conversa: ligar um
  /// fio e ver o efeito dele no turno seguinte é a mesma tarefa, e ver as duas
  /// janelas ao mesmo tempo é o que faz o grafo parecer programação visual.
  const [palco, setPalco] = useState<"orbe" | "grafo">("orbe");
  /// Qual sinapse disparou no turno em curso, keyed pelo nome qualificado que o
  /// núcleo cobra — a mesma identidade que o fio tem no desenho.
  const [disparos, setDisparos] = useState<Record<string, Disparo>>({});
  const [ajustes, setAjustes] = useState<AjustesGeracao | null>(null);
  /// Algum adaptador está sobre os pesos agora. É o que acende o botão do ajuste
  /// fino — a escolha gravada não acende nada, porque escolha não é execução.
  const [ajustado, setAjustado] = useState(false);

  const [voz, setVoz] = useState<EstadoVoz | null>(null);
  const [medidor, setMedidor] = useState<MedidorVoz>(SEM_MEDIDOR);
  const [transcrevendo, setTranscrevendo] = useState(false);

  // O ouvinte da conversa é montado uma vez e vive mais que qualquer troca de
  // catálogo: ele resolve a ferramenta por aqui, não por um valor capturado.
  const ferramentas = useRef<Map<string, ToolInfo>>(new Map());

  const refresh = useCallback(async () => {
    try {
      const [
        nextConnectors,
        nextConnected,
        nextMode,
        nextPending,
        nextTools,
        nextAi,
        nextAjuste,
        nextPastas,
        nextConcedidas,
        nextChats,
      ] = await Promise.all([
          api.listConnectors(),
          api.connectedConnectors(),
          api.permissionMode(),
          api.pendingApprovals(),
          api.allTools(),
          api.aiStatus(),
          api.adaptadoresEstado(),
          api.grantedFolders(),
          api.sessionGrants(),
          api.conversasListar(),
        ]);
      setConnectors(nextConnectors);
      setConnected(nextConnected);
      setMode(nextMode);
      setPending(nextPending);
      setCapabilities(nextTools);
      setPastas(nextPastas);
      setConcedidas(nextConcedidas);
      setLoaded(nextAi.loaded);
      setAjustes(nextAi.ajustes);
      setRuntime(nextAi.runtime);
      setAjustado(nextAjuste.itens.some((item) => item.aplicada));
      setChats(nextChats);
      ferramentas.current = new Map(nextTools.map((tool) => [tool.qualifiedName, tool]));
    } catch (cause) {
      setError(message(cause));
    }
  }, []);

  /// O estado da voz relê o disco dos modelos, então é pedido quando a voz muda
  /// de mãos — um comando, um download que chega, um turno que falou — e não a
  /// cada quadro; o que anda a cada quadro é só o pulso, em `pulsar`.
  const refreshVoz = useCallback(async () => {
    try {
      setVoz(await api.vozEstado());
    } catch (cause) {
      setError(message(cause));
    }
  }, []);

  // Trocar o idioma remonta o catálogo no núcleo: os nomes que voltam daqui já
  // vêm na língua nova, e os cartões se redrawem com eles.
  useEffect(() => {
    void refresh();
    void refreshVoz();
  }, [refresh, refreshVoz, idioma]);

  const pulsar = useCallback(async () => {
    try {
      setMedidor(await api.vozMedidor());
    } catch (cause) {
      setError(message(cause));
    }
  }, []);

  // O pulso da voz só é lido enquanto há alguma coisa acontecendo: fala viva,
  // microfone aberto, tela de voz aberta, ou o auto-falar ligado — que pode
  // começar a falar sem nenhum clique na tela.
  const vivo =
    medidor.falando ||
    medidor.ouvindo ||
    showVoz ||
    voz?.ajustes.autoFalar === true;
  useEffect(() => {
    if (!vivo) return;
    void pulsar();
    const relogio = setInterval(() => void pulsar(), 250);
    return () => clearInterval(relogio);
  }, [vivo, pulsar]);

  // O download de um modelo de voz termina por evento, não por consulta: a
  // lista de discos precisa voltar a ser lida quando ele chega ou falha.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    const handle = (event: VozEvento) => {
      if (event.type === "falhou" && !showVoz) setError(event.erro);
      if (
        event.type === "concluiu" ||
        event.type === "cancelou" ||
        event.type === "falhou"
      )
        void refreshVoz();
    };
    void onVozEvent(handle).then((un) => {
      if (cancelled) un();
      else unlisten = un;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [refreshVoz, showVoz]);

  // Ao abrir, a conversa ativa volta do disco: o histórico sobreviveu à janela
  // fechada, e o núcleo põe o arquivo no buffer vivo antes do primeiro pedido.
  useEffect(() => {
    void (async () => {
      try {
        const estado = await api.conversasListar();
        setChats(estado);
        if (estado.ativa !== null) {
          setTurns(turnsDe(await api.conversaAbrir(estado.ativa)));
        }
      } catch (cause) {
        setError(message(cause));
      }
    })();
  }, []);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    const handle = (event: ChatEvent) => {
      // O pulso do fio vem do mesmo evento que preenche o cartão da ferramenta:
      // o grafo não tem uma segunda fonte de verdade para inventar.
      if (event.type === "toolCall" || event.type === "toolResult") {
        const chave = event.name;
        setDisparos((atual) => ({
          ...atual,
          [chave]: {
            emCurso: event.type === "toolCall",
            ok: event.type === "toolResult" ? event.ok : undefined,
            ms: event.type === "toolResult" ? event.ms : undefined,
            n: (atual[chave]?.n ?? 0) + 1,
          },
        }));
      }
      // A injeção automática não é um pedido do modelo, mas é o mesmo fio do
      // grafo que a autoriza: por isso o pulso sai na mesma sinapse, com o
      // tempo medido na busca e nunca com um disparo inventado.
      if (event.type === "memoria") {
        setDisparos((atual) => ({
          ...atual,
          [event.ferramenta]: {
            emCurso: false,
            ok: true,
            ms: event.ms,
            n: (atual[event.ferramenta]?.n ?? 0) + 1,
          },
        }));
      }
      setTurns((current) => {
        const last = current[current.length - 1];
        if (last === undefined || last.role !== "assistant") return current;
        const turn: Turn = { ...last };
        switch (event.type) {
          case "choice":
            turn.choice = event.choice;
            turn.loading = null;
            break;
          case "loading":
            turn.loading = event.model;
            break;
          case "thinking":
            turn.thinking += event.text;
            break;
          case "delta":
            turn.text += event.text;
            break;
          case "replace":
            turn.text = event.text;
            break;
          case "memoria":
            turn.memoria = {
              type: "memoria",
              consulta: event.consulta,
              achados: event.achados.map((achado) => ({
                caminho: achado.arquivo,
                nome: achado.nome,
                linha: achado.linha,
                texto: achado.texto,
                pontuacao: achado.pontuacao,
                origem: achado.origem,
              })),
              modo: event.modo,
              total: event.total,
              truncado: event.total > event.achados.length,
            };
            break;
          case "toolCall":
            turn.tools = [
              ...turn.tools,
              { name: event.name, info: ferramentas.current.get(event.name) },
            ];
            break;
          case "toolResult": {
            const tools = [...turn.tools];
            for (let at = tools.length - 1; at >= 0; at -= 1) {
              if (tools[at].name === event.name && tools[at].ok === undefined) {
                tools[at] = {
                  ...tools[at],
                  ok: event.ok,
                  text: event.text,
                  vista: event.vista,
                };
                break;
              }
            }
            turn.tools = tools;
            break;
          }
          case "done":
            turn.text = event.text;
            turn.medicao = event.medicao;
            turn.loading = null;
            turn.streaming = false;
            break;
        }
        return [...current.slice(0, -1), turn];
      });
    };
    void onChatEvent(handle).then((un) => {
      if (cancelled) un();
      else unlisten = un;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    let unlistenEvent: (() => void) | undefined;
    let unlistenLagged: (() => void) | undefined;
    const handle = (event: McpEvent) => {
      if (event.type === "approvalRequested") {
        const args = event.request.arguments;
        setRequests((current) => ({
          ...current,
          [event.request.id]:
            args !== null && typeof args === "object" && !Array.isArray(args)
              ? (args as Record<string, unknown>)
              : {},
        }));
      }
      void refresh();
    };
    void onMcpEvent(handle).then((unlisten) => {
      if (cancelled) unlisten();
      else unlistenEvent = unlisten;
    });
    void onMcpLagged(() => void refresh()).then((unlisten) => {
      if (cancelled) unlisten();
      else unlistenLagged = unlisten;
    });
    return () => {
      cancelled = true;
      unlistenEvent?.();
      unlistenLagged?.();
    };
  }, [refresh]);

  const phase: CorePhase =
    error !== null
      ? "fault"
      : pending.length > 0
        ? "awaiting"
        : calling || talking
          ? "working"
          : medidor.ouvindo
            ? "listening"
            : query.trim() !== ""
              ? "listening"
              : connected.length > 0 || loaded !== null
                ? "ready"
                : "dormant";

  async function submitQuery() {
    const trimmed = query.trim();
    if (trimmed === "" || talking) return;
    setError(null);
    setResult(null);
    setChosen(null);
    setQuery("");
    setTurns((current) => [
      ...current,
      { ...emptyTurn("user"), text: trimmed },
      { ...emptyTurn("assistant"), streaming: true },
    ]);
    setTalking(true);
    try {
      const outcome = await api.chatSend(trimmed);
      setTurns((current) => {
        const last = current[current.length - 1];
        if (last === undefined || last.role !== "assistant") return current;
        return [
          ...current.slice(0, -1),
          {
            ...last,
            text: outcome.text,
            thinking: outcome.thinking,
            medicao: outcome.medicao,
            loading: null,
            streaming: false,
          },
        ];
      });
    } catch (cause) {
      setError(message(cause));
      setTurns((current) => {
        const last = current[current.length - 1];
        if (last === undefined || last.role !== "assistant") return current;
        const rest = current.slice(0, -1);
        const settled = { ...last, loading: null, streaming: false };
        const empty =
          settled.text === "" &&
          settled.thinking === "" &&
          settled.tools.length === 0;
        return empty ? rest : [...rest, settled];
      });
    } finally {
      setTalking(false);
      await refresh();
    }
  }

  async function cancelTalk() {
    try {
      await api.chatCancel();
    } catch (cause) {
      setError(message(cause));
    }
  }

  async function resetTalk() {
    setError(null);
    try {
      await api.chatReset();
      setTurns([]);
      await refresh();
    } catch (cause) {
      setError(message(cause));
    }
  }

  async function novaConversa() {
    setError(null);
    try {
      setChats(await api.conversaNova());
      setTurns([]);
      setResult(null);
      setChosen(null);
    } catch (cause) {
      setError(message(cause));
    }
  }

  async function abrirConversa(id: string) {
    setError(null);
    try {
      setTurns(turnsDe(await api.conversaAbrir(id)));
      setResult(null);
      setChosen(null);
      setChats(await api.conversasListar());
    } catch (cause) {
      setError(message(cause));
    }
  }

  async function renomearConversa(id: string, titulo: string) {
    setError(null);
    try {
      await api.conversaRenomear(id, titulo);
      setChats(await api.conversasListar());
    } catch (cause) {
      setError(message(cause));
    }
  }

  async function apagarConversa(id: string) {
    setError(null);
    const eraAtiva = chats.ativa === id;
    try {
      setChats(await api.conversaApagar(id));
      if (eraAtiva) setTurns([]);
    } catch (cause) {
      setError(message(cause));
    }
  }

  /// Abre o microfone; a transcrição vem depois, no segundo clique.
  async function startMic() {
    setError(null);
    try {
      await api.vozOuvir();
      await pulsar();
    } catch (cause) {
      setError(message(cause));
    }
  }

  /// Solta o microfone e traz o texto. O Whisper roda na máquina e leva mais
  /// tempo que um clique: a barra fica sabendo que está transcrevendo.
  async function stopMic() {
    setTranscrevendo(true);
    setError(null);
    try {
      setQuery(await api.vozTranscrever());
    } catch (cause) {
      setError(message(cause));
    } finally {
      setTranscrevendo(false);
      await Promise.all([pulsar(), refreshVoz()]);
    }
  }

  /// Botão solto em cancelamento: o áudio vai fora e o microfone fecha.
  async function discardMic() {
    setError(null);
    try {
      await api.vozCancelarEscuta();
    } catch (cause) {
      setError(message(cause));
    } finally {
      await pulsar();
    }
  }

  function pick(tool: ToolInfo) {
    setError(null);
    setResult(null);
    setChosen(tool);
  }

  async function run(tool: ToolInfo, args: Record<string, unknown>) {
    setCalling(true);
    setError(null);
    setResult(null);
    try {
      const outcome = await api.callTool(tool.qualifiedName, args);
      setResult(outcome);
      setResultTitle(toolTitle(tool));
      setChosen(null);
      setQuery("");
      await refresh();
    } catch (cause) {
      setError(message(cause));
      setChosen(null);
    } finally {
      setCalling(false);
    }
  }

  async function toggleConnector(id: string) {
    setBusy(id);
    setError(null);
    try {
      if (connected.includes(id)) await api.disconnectConnector(id);
      else await api.connectConnector(id);
      await refresh();
    } catch (cause) {
      setError(message(cause));
    } finally {
      setBusy(null);
    }
  }

  async function removeConnector(id: string) {
    setError(null);
    try {
      await api.removeConnector(id);
      await refresh();
    } catch (cause) {
      setError(message(cause));
    }
  }

  async function changeMode(next: AgentMode) {
    setError(null);
    try {
      await api.setPermissionMode(next);
      await refresh();
    } catch (cause) {
      setError(message(cause));
    }
  }

  async function resolveApproval(id: string, approved: boolean) {
    setError(null);
    try {
      await api.resolveApproval(id, approved);
      await refresh();
    } catch (cause) {
      setError(message(cause));
    }
  }

  /// Confiar na sessão é a aprovação deste pedido e o fim das perguntas para
  /// este servidor até a conversa recomeçar.
  async function trustSession(approvalId: string, connectorId: string) {
    setError(null);
    try {
      await api.grantSession(connectorId);
      await api.resolveApproval(approvalId, true);
      await refresh();
    } catch (cause) {
      setError(message(cause));
    }
  }

  async function revokeSession(id: string) {
    setError(null);
    try {
      await api.revokeSession(id);
      await refresh();
    } catch (cause) {
      setError(message(cause));
    }
  }

  const approval = pending[0] ?? null;

  /// O microfone só aparece com a voz disponível e um modelo de escuta escolhido
  /// e baixado: sem os dois, a barra é só texto, como sempre foi.
  const escuta =
    voz !== null &&
    voz.disponivel &&
    voz.ajustes.modeloEscuta !== "" &&
    voz.modelos.some(
      (modelo) => modelo.id === voz.ajustes.modeloEscuta && modelo.baixado,
    );

  /// Um pacote incompleto é o PC novo sem DLL: a faixa leva direto ao
  /// instalador, em vez de deixar o erro do carregador ser a única pista.
  const runtimeFalta = runtime.some((pacote) => !pacote.completo);

  return (
    <div className={showChats ? "app app--chats" : "app"}>
      {showChats && (
        <Chats
          estado={chats}
          ocupado={talking || calling}
          onNova={() => void novaConversa()}
          onAbrir={(id) => void abrirConversa(id)}
          onRenomear={(id, titulo) => void renomearConversa(id, titulo)}
          onApagar={(id) => void apagarConversa(id)}
          onLimpar={() => void resetTalk()}
          onFechar={() => setShowChats(false)}
        />
      )}
      <div className={palco === "grafo" ? "stage stage--grafo" : "stage"}>
        <header className="stage-top">
          <span className="brand">
            <span className="brand-mark" aria-hidden="true" />
            Aether
          </span>
          <span className="stage-actions">
            <button
              type="button"
              className={showChats ? "icon-only ligado" : "icon-only"}
              onClick={() => setShowChats((atual) => !atual)}
              aria-pressed={showChats}
              aria-label={showChats ? t("conversas.esconder") : t("conversas.mostrar")}
              title={showChats ? t("conversas.esconder") : t("conversas.mostrar")}
            >
              <Icon name="conversa" />
            </button>
            <button
              type="button"
              className={palco === "grafo" ? "icon-only ligado" : "icon-only"}
              onClick={() => setPalco((atual) => (atual === "grafo" ? "orbe" : "grafo"))}
              aria-pressed={palco === "grafo"}
              aria-label={palco === "grafo" ? t("app.orbe") : t("app.grafo")}
              title={palco === "grafo" ? t("app.orbe") : t("app.grafo")}
            >
              <Icon name="nodes" />
            </button>
            <button
              type="button"
              className={
                ajustes !== null &&
                (ajustes.modelo !== "" || ajustes.pensamento || ajustes.semente !== 0)
                  ? "icon-only ligado"
                  : "icon-only"
              }
              onClick={() => setShowGeracao(true)}
              aria-label={t("app.escalas")}
              title={t("app.escalas")}
            >
              <Icon name="escalas" />
            </button>
            <button
              type="button"
              className={ajustado ? "icon-only ligado" : "icon-only"}
              onClick={() => setShowAjuste(true)}
              aria-pressed={ajustado}
              aria-label={t("app.ajusteFino")}
              title={t("app.ajusteFino")}
            >
              <Icon name="camadas" />
            </button>
            <button
              type="button"
              className="icon-only"
              onClick={() => setShowModels(true)}
              aria-label={t("app.modelos")}
              title={t("app.modelos")}
            >
              <Icon name="cube" />
            </button>
            <button
              type="button"
              className={runtimeFalta ? "icon-only falta" : "icon-only"}
              onClick={() => setShowRuntime(true)}
              aria-label={t("runtime.titulo")}
              title={t("runtime.titulo")}
            >
              <Icon name="cpu" />
            </button>
            <button
              type="button"
              className="icon-only"
              onClick={() => setShowBancada(true)}
              aria-label={t("app.bancada")}
              title={t("app.bancada")}
            >
              <Icon name="bancada" />
            </button>
            <button
              type="button"
              className={medidor.falando ? "icon-only voz-ligada" : "icon-only"}
              onClick={() => setShowVoz(true)}
              aria-label={t("app.voz")}
              title={t("app.voz")}
            >
              <Icon name="wave" />
            </button>
            <button
              type="button"
              className="icon-only"
              onClick={() => setShowPastas(true)}
              aria-label={t("app.pastas")}
              title={t("app.pastas")}
            >
              <Icon name="folder" />
            </button>
            <button
              type="button"
              className="icon-only"
              onClick={() => setShowMemoria(true)}
              aria-label={t("app.memoria")}
              title={t("app.memoria")}
            >
              <Icon name="memoria" />
            </button>
            <button
              type="button"
              className="icon-only"
              onClick={() => setShowIdioma(true)}
              aria-label={t("app.idioma")}
              title={t("app.idioma")}
            >
              <Icon name="globe" />
            </button>
            <button
              type="button"
              className="icon-only"
              onClick={() => void novaConversa()}
              disabled={talking}
              aria-label={t("conversas.nova")}
              title={t("conversas.nova")}
            >
              <Icon name="plus" />
            </button>
            {mode !== null && (
              <TrustDial
                mode={mode}
                granted={concedidas.map((id) => ({
                  id,
                  nome:
                    connectors.find((entry) => entry.id === id)?.name ?? id,
                }))}
                onChange={changeMode}
                onRevoke={(id) => void revokeSession(id)}
              />
            )}
          </span>
        </header>

        <div className="stage-core">
          {palco === "grafo" ? (
            <GrafoNeural
              disparos={disparos}
              sinal={connected.join("|")}
              onConectar={(id) => void toggleConnector(id)}
              onAdicionar={() => setShowServers(true)}
              onChange={() => void refresh()}
            />
          ) : (
            <Core phase={phase} />
          )}
        </div>

        <div className="stage-sheet">
          {approval !== null ? (
            <ApprovalCard
              approval={approval}
              tool={ferramentas.current.get(
                `${approval.connectorId}__${approval.tool}`,
              )}
              source={
                connectors.find((entry) => entry.id === approval.connectorId)?.name
              }
              args={requests[approval.id] ?? null}
              granted={concedidas.includes(approval.connectorId)}
              onResolve={resolveApproval}
              onTrust={() => void trustSession(approval.id, approval.connectorId)}
              onRevoke={() => void revokeSession(approval.connectorId)}
            />
          ) : chosen !== null ? (
            <ArgumentForm
              tool={chosen}
              busy={calling}
              onCancel={() => setChosen(null)}
              onRun={(args) => void run(chosen, args)}
            />
          ) : result !== null ? (
            <ResultCard
              title={resultTitle}
              result={result}
              onClose={() => setResult(null)}
            />
          ) : turns.length > 0 ? (
            <Conversation turns={turns} aguardando={pending.length > 0} />
          ) : capabilities.length > 0 ? (
            <CapabilityCards
              tools={capabilities}
              connectors={connectors}
              onPick={pick}
            />
          ) : (
            <p className="sheet-note">
              {connectors.length === 0
                ? t("app.semServidor")
                : t("app.semFerramenta")}
            </p>
          )}
        </div>

        <IntentBar
          value={query}
          busy={calling || talking}
          onChange={setQuery}
          onSubmit={() => void submitQuery()}
          onCancel={talking ? () => void cancelTalk() : undefined}
          microfone={escuta}
          ouvindo={medidor.ouvindo}
          transcrevendo={transcrevendo}
          nivel={medidor.nivel}
          onOuvir={() => void startMic()}
          onTranscrever={() => void stopMic()}
          onDescartar={() => void discardMic()}
        />

        {/* No palco do grafo os cartões já são esta faixa: um servidor aparece
            duas vezes na tela quando há chips e nós para o mesmo plug. */}
        {palco !== "grafo" && (
          <ConnectorNodes
            connectors={connectors}
            connected={connected}
            busy={busy}
            onToggle={(id) => void toggleConnector(id)}
            onAdd={() => setShowServers(true)}
          />
        )}

        {(runtimeFalta || error !== null) && (
          <div className="stage-alertas">
            {runtimeFalta && (
              <div className="fault-bar runtime-falta" role="alert">
                <Icon name="warning" />
                <span>{t("runtime.faltaRuntime")}</span>
                <button
                  type="button"
                  className="primary"
                  onClick={() => setShowRuntime(true)}
                >
                  <Icon name="download" />
                  {t("runtime.instalar")}
                </button>
              </div>
            )}
            {error !== null && (
              <div className="fault-bar" role="alert">
                <Icon name="warning" />
                <span>{error}</span>
                <button
                  type="button"
                  className="icon-only"
                  onClick={() => setError(null)}
                  aria-label={t("geral.dispensar")}
                >
                  <Icon name="cross" />
                </button>
              </div>
            )}
          </div>
        )}

        {showModels && (
          <ModelManager
            onClose={() => setShowModels(false)}
            onLoaded={() => void refresh()}
          />
        )}

        {showServers && (
          <ServerDialog
            connectors={connectors}
            onRemove={(id) => void removeConnector(id)}
            onSaved={() => void refresh()}
            onClose={() => setShowServers(false)}
          />
        )}

        {showPastas && (
          <PastasDialog
            pastas={pastas}
            onChange={setPastas}
            onClose={() => setShowPastas(false)}
          />
        )}

        {showMemoria && (
          <MemoriaDialog
            onClose={() => setShowMemoria(false)}
            onChange={() => void refresh()}
          />
        )}

        {showGeracao && (
          <GeracaoDialog
            onClose={() => setShowGeracao(false)}
            onChange={() => void refresh()}
          />
        )}

        {showAjuste && (
          <AdaptadoresDialog
            onClose={() => setShowAjuste(false)}
            onChange={() => void refresh()}
          />
        )}

        {showIdioma && <IdiomaDialog onClose={() => setShowIdioma(false)} />}

        {showRuntime && (
          <RuntimeDialog onClose={() => setShowRuntime(false)} onChange={refresh} />
        )}

        {showBancada && <BancadaDialog onClose={() => setShowBancada(false)} />}

        {showVoz && (
          <VozDialog
            estado={voz}
            medidor={medidor}
            onClose={() => setShowVoz(false)}
            onRefresh={() => void refreshVoz()}
          />
        )}
      </div>
    </div>
  );
}