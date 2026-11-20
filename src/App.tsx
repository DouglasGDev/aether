import { Component, lazy, Suspense, useCallback, useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import { AppMenu, type Marca, type TelaAjuste } from "./components/AppMenu";
import { ApprovalCard } from "./components/ApprovalCard";
import { ArgumentForm } from "./components/ArgumentForm";
import { CapabilityCards } from "./components/CapabilityCards";
import { Chats } from "./components/Chats";
import { ConnectorNodes } from "./components/ConnectorNodes";
import { Conversation, emptyTurn, turnsDe, type Turn } from "./components/Conversation";
import { Core, type CorePhase } from "./components/Core";
import type { Disparo } from "./components/GrafoNeural";
import { Icon } from "./components/Icon";
import { IntentBar } from "./components/IntentBar";
import { ResultCard } from "./components/ResultCard";
import { TrustDial } from "./components/TrustDial";
import { api, message, onBancadaEvent, onChatEvent, onMcpEvent, onMcpLagged, onVozEvent } from "./lib/api";
import { toolTitle } from "./lib/display";
import { useIdioma } from "./lib/i18n";
import type {
  AgentMode,
  AjustesGeracao,
  BancadaEvento,
  BancadaRodada,
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

/// Rota e dialogs por import dinâmico: nada aqui é pintado no primeiro quadro,
/// e o que só aparece num clique não precisa estar no arquivo que o aplicativo
/// lê ao abrir. Os componentes são exports nomeados, então `lazy` recebe o
/// adaptador que devolve o `default` — o chunk continua sendo o módulo próprio.
const GrafoNeural = lazy(() =>
  import("./components/GrafoNeural").then((m) => ({ default: m.GrafoNeural })),
);
const AdaptadoresDialog = lazy(() =>
  import("./components/AdaptadoresDialog").then((m) => ({ default: m.AdaptadoresDialog })),
);
const BancadaDialog = lazy(() =>
  import("./components/BancadaDialog").then((m) => ({ default: m.BancadaDialog })),
);
const BancosDialog = lazy(() =>
  import("./components/BancosDialog").then((m) => ({ default: m.BancosDialog })),
);
const GeracaoDialog = lazy(() =>
  import("./components/GeracaoDialog").then((m) => ({ default: m.GeracaoDialog })),
);
const IdiomaDialog = lazy(() =>
  import("./components/IdiomaDialog").then((m) => ({ default: m.IdiomaDialog })),
);
const MemoriaDialog = lazy(() =>
  import("./components/MemoriaDialog").then((m) => ({ default: m.MemoriaDialog })),
);
const ModelManager = lazy(() =>
  import("./components/ModelManager").then((m) => ({ default: m.ModelManager })),
);
const PastasDialog = lazy(() =>
  import("./components/PastasDialog").then((m) => ({ default: m.PastasDialog })),
);
const RuntimeDialog = lazy(() =>
  import("./components/RuntimeDialog").then((m) => ({ default: m.RuntimeDialog })),
);
const ServerDialog = lazy(() =>
  import("./components/ServerDialog").then((m) => ({ default: m.ServerDialog })),
);
const VozDialog = lazy(() =>
  import("./components/VozDialog").then((m) => ({ default: m.VozDialog })),
);

const SEM_MEDIDOR: MedidorVoz = { falando: false, ouvindo: false, nivel: 0 };

const SEM_RODADA: BancadaRodada = { resultados: {}, andando: false, atual: null, modelo: null };

/// Import dinâmico é leitura de disco: se o chunk não vier, o throw do React
/// desmontaria a árvore inteira e a tela ficaria branca no meio de um turno. A
/// borda devolve o motivo real na barra de falha que o aplicativo já usa.
class FronteiraDeCarregamento extends Component<
  { children: ReactNode },
  { erro: string | null }
> {
  state: { erro: string | null } = { erro: null };

  static getDerivedStateFromError(cause: unknown) {
    return { erro: message(cause) };
  }

  render() {
    if (this.state.erro !== null) {
      return (
        <div className="stage-alertas">
          <div className="fault-bar" role="alert">
            <Icon name="warning" />
            <span>{this.state.erro}</span>
          </div>
        </div>
      );
    }
    return this.props.children;
  }
}

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
  const [showBancos, setShowBancos] = useState(false);
  /// A rodada da bancada mora aqui, não no dialog. A folha é desmontada quando
  /// fecha, e com ela sumia o que cada caso mediu — reabrir era obrigatório
  /// rodar tudo de novo contra a placa. Pior: o núcleo continua correndo depois
  /// de a folha fechar, e sem ouvinte aqui os resultados caiam no vazio.
  const [bancadaCasos, setBancadaCasos] = useState<string[]>([]);
  const [bancada, setBancada] = useState<BancadaRodada>(SEM_RODADA);
  const [bancadaErro, setBancadaErro] = useState<string | null>(null);
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

  /// Cada evento MCP e cada fim de turno dispara um refresh concorrente: sem
  /// ordem, um snapshot mais antigo e mais lento atropela `pending`/`connected`/
  /// `chats` mais novos. Só o último pedido iniciado tem o direito de escrever.
  const refreshPedido = useRef(0);

  const refresh = useCallback(async () => {
    const token = ++refreshPedido.current;
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
      // Um pedido mais novo já começou: este snapshot é velho e não escreve.
      if (token !== refreshPedido.current) return;
      setConnectors(nextConnectors);
      setConnected(nextConnected);
      setMode(nextMode);
      setPending(nextPending);
      /// `requests` só existe para segurar os argumentos que o evento
      /// `approvalRequested` trouxe e que o `pending_approvals` não devolve.
      /// O pedido morre no núcleo — resolvido, expirado ou derrubado com o
      /// conector — então o snapshot que manda o cartão ir embora é também o
      /// que manda a entrada ir junto: sem isto o mapa cresceria uma chave por
      /// aprovação da vida inteira do processo. A poda é aqui, e não no clique
      /// de resolver, porque só o refresh conhece todas as formas de morrer.
      setRequests((current) => {
        const vivos = new Set(nextPending.map((entrada) => entrada.id));
        const proximo: Record<string, Record<string, unknown>> = {};
        for (const [id, args] of Object.entries(current)) {
          if (vivos.has(id)) proximo[id] = args;
        }
        // Nenhuma chave fora: devolve o objeto atual para não reacender render.
        return Object.keys(proximo).length === Object.keys(current).length
          ? current
          : proximo;
      });
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
      // A falha de um pedido já superado não é a falha do estado atual: um
      // refresh mais novo pode ter sucedido depois deste ter falhado.
      if (token !== refreshPedido.current) return;
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
          case "resumo":
            turn.resumo = {
              texto: event.texto,
              mensagens: event.mensagens,
              descartadas: event.descartadas,
            };
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
                fonte: achado.fonte,
                quando: achado.quando,
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
    void api
      .bancadaCasos()
      .then(setBancadaCasos)
      .catch((cause) => setBancadaErro(message(cause)));
  }, []);

  /// O ouvido fica aqui, vivo enquanto o aplicativo existe. A folha é só o
  /// desenho: fechar a bancada no meio de uma rodada não desliga mais o
  /// ouvinte, então o que o núcleo medir depois continua chegando.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    const handle = (event: BancadaEvento) => {
      setBancada((atual) => {
        switch (event.type) {
          case "iniciou":
            return { resultados: {}, andando: true, atual: null, modelo: event.modelo };
          case "caso":
            return { ...atual, atual: event.id };
          case "resultado":
            return {
              ...atual,
              atual: null,
              resultados: { ...atual.resultados, [event.id]: event },
            };
        }
      });
    };
    void onBancadaEvent(handle).then((un) => {
      if (cancelled) un();
      else unlisten = un;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  const rodarBancada = useCallback(async () => {
    setBancadaErro(null);
    try {
      await api.bancadaRodar();
    } catch (cause) {
      setBancadaErro(message(cause));
    } finally {
      setBancada((atual) => ({ ...atual, andando: false, atual: null }));
    }
  }, []);

  const pararBancada = useCallback(async () => {
    try {
      await api.bancadaParar();
    } catch (cause) {
      setBancadaErro(message(cause));
    }
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
    // Um turno em curso anexa cada delta ao último turno assistant do buffer
    // vivo: trocar a conversa agora faria o handler escrever na história de
    // outra. O núcleo recusa a troca e a tela já esconde o botão (Chats
    // desabilita com `ocupado`); esta guarda cobre quem chamar direto.
    if (talking) return;
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

  /// O que está valendo agora, levado ao menu para que o estado não suma junto
  /// com os botões que mostravam cada ajuste na barra.
  const marcasMenu: Partial<Record<TelaAjuste, Marca>> = {};
  if (runtimeFalta) marcasMenu.runtime = "aviso";
  if (medidor.falando) marcasMenu.voz = "pulso";
  if (ajustado) marcasMenu.ajuste = "ligada";
  if (
    ajustes !== null &&
    (ajustes.modelo !== "" || ajustes.pensamento || ajustes.semente !== 0)
  )
    marcasMenu.geracao = "ligada";
  // A bancada também se vê daqui: uma rodada em curso continuava correndo sem
  // ninguém olhando, e uma reprovada some quando a folha fecha.
  if (bancada.andando) marcasMenu.bancada = "pulso";
  else if (Object.values(bancada.resultados).some((r) => r.estado === "reprovado"))
    marcasMenu.bancada = "aviso";

  /// Uma linha por tela: o Record completo é o que impede o menu de anunciar
  /// uma porta que o app não abre.
  function abrirAjuste(tela: TelaAjuste) {
    const aberturas: Record<TelaAjuste, () => void> = {
      modelos: () => setShowModels(true),
      runtime: () => setShowRuntime(true),
      geracao: () => setShowGeracao(true),
      ajuste: () => setShowAjuste(true),
      servidores: () => setShowServers(true),
      bancos: () => setShowBancos(true),
      pastas: () => setShowPastas(true),
      memoria: () => setShowMemoria(true),
      voz: () => setShowVoz(true),
      idioma: () => setShowIdioma(true),
      bancada: () => setShowBancada(true),
    };
    aberturas[tela]();
  }

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
            {/* A marca é uma string visível como qualquer outra: vem do locale,
                pela mesma chave que o hub do grafo já usa. */}
            {t("ligacoes.aether")}
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
            <AppMenu onAbrir={abrirAjuste} marcas={marcasMenu} />
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
            <FronteiraDeCarregamento>
              <Suspense fallback={<p className="sheet-note">{t("geral.carregando")}</p>}>
                <GrafoNeural
                  disparos={disparos}
                  sinal={connected.join("|")}
                  onConectar={(id) => void toggleConnector(id)}
                  onAdicionar={() => setShowServers(true)}
                  onChange={() => void refresh()}
                />
              </Suspense>
            </FronteiraDeCarregamento>
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

        {/* Os dialogs são o caminho lento do import dinâmico: nada aqui existe
            antes de um clique, então nada aqui pesa no chunk da abertura. A
            moldura vazia enquanto o módulo chega é o estado real — não há
            cartão para meio mostrar. */}
        <FronteiraDeCarregamento>
          <Suspense fallback={null}>
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

            {showBancada && (
              <BancadaDialog
                casos={bancadaCasos}
                rodada={bancada}
                erro={bancadaErro}
                onRodar={() => void rodarBancada()}
                onParar={() => void pararBancada()}
                onClose={() => setShowBancada(false)}
              />
            )}

            {showBancos && <BancosDialog onClose={() => setShowBancos(false)} />}

            {showVoz && (
              <VozDialog
                estado={voz}
                medidor={medidor}
                onClose={() => setShowVoz(false)}
                onRefresh={() => void refreshVoz()}
              />
            )}
          </Suspense>
        </FronteiraDeCarregamento>
      </div>
    </div>
  );
}