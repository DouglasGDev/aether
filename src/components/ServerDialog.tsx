import { useCallback, useEffect, useRef, useState } from "react";
import { api, message, onServerEvent } from "../lib/api";
import { useIdioma, useT } from "../lib/i18n";
import type {
  Connector,
  ServerEvent,
  ServidorCatalogo,
  ServidorMcpEstado,
} from "../lib/types";
import { ConnectorForm } from "./ConnectorForm";
import { useFecharComEscape } from "../lib/teclado";
import { Icon } from "./Icon";

interface Props {
  connectors: Connector[];
  onRemove: (id: string) => void;
  onSaved: () => void;
  onClose: () => void;
}

/** O que a fila de eventos sabe de um servidor enquanto o npm trabalha. */
interface Obra {
  fase: "instalando" | "removendo";
  linhas: string[];
}

export function ServerDialog({ connectors, onRemove, onSaved, onClose }: Props) {
  useFecharComEscape(onClose);
  const t = useT();
  const { id: idioma } = useIdioma();
  const [catalogo, setCatalogo] = useState<ServidorCatalogo[] | null>(null);
  const [obras, setObras] = useState<Record<string, Obra>>({});
  const [falhas, setFalhas] = useState<Record<string, string>>({});
  const [erro, setErro] = useState<string | null>(null);
  const [estadoServidor, setEstadoServidor] = useState<ServidorMcpEstado | null>(null);
  const [token, setToken] = useState<string | null>(null);
  // Um clique por servidor: o npm não aceita duas escritas na mesma árvore.
  const ocupado = useRef<Set<string>>(new Set());
  // Abrir a conexão escreve no mcp.json e levanta um ouvinte: dois cliques
  // ao mesmo tempo pediriam duas portas para a mesma conexão.
  const publicando = useRef(false);

  const recarregar = useCallback(async () => {
    try {
      setCatalogo(await api.mcpCatalog());
    } catch (cause) {
      setErro(message(cause));
    }
  }, []);

  const recarregarServidor = useCallback(async () => {
    try {
      const estado = await api.mcpServidorEstado();
      setEstadoServidor(estado);
      setToken(estado.ativa ? await api.mcpServidorToken() : null);
    } catch (cause) {
      setErro(message(cause));
    }
  }, []);

  useEffect(() => {
    void recarregar();
    void recarregarServidor();
  }, [recarregar, recarregarServidor, idioma]);

  useEffect(() => {
    let desfeita = false;
    let desligar: (() => void) | undefined;
    const tratar = (evento: ServerEvent) => {
      const fechar = (id: string) =>
        setObras((atual) => {
          const proxima = { ...atual };
          delete proxima[id];
          return proxima;
        });
      switch (evento.type) {
        case "iniciou":
          setFalhas((atual) => ({ ...atual, [evento.id]: "" }));
          setObras((atual) => ({
            ...atual,
            [evento.id]: { fase: evento.instalando ? "instalando" : "removendo", linhas: [] },
          }));
          break;
        case "linha":
          setObras((atual) => {
            const obra = atual[evento.id] ?? { fase: "instalando", linhas: [] };
            // Guardar só o fim da fila: o npm escreve centenas de linhas e esta
            // janela não é um terminal.
            return {
              ...atual,
              [evento.id]: { ...obra, linhas: [...obra.linhas, evento.texto].slice(-6) },
            };
          });
          break;
        case "concluiu":
        case "removeu":
        case "cancelou":
          fechar(evento.id);
          void recarregar();
          onSaved();
          break;
        case "falhou":
          fechar(evento.id);
          setFalhas((atual) => ({ ...atual, [evento.id]: evento.erro }));
          void recarregar();
          break;
      }
    };
    void onServerEvent(tratar).then((un) => {
      if (desfeita) un();
      else desligar = un;
    });
    return () => {
      desfeita = true;
      desligar?.();
    };
  }, [recarregar, onSaved]);

  const agir = async (
    servidor: ServidorCatalogo,
    acao: "instalar" | "remover" | "conectar" | "desconectar",
  ) => {
    if (ocupado.current.has(servidor.id)) return;
    ocupado.current.add(servidor.id);
    setErro(null);
    try {
      // A barra de trabalho não é adivinhada aqui: o aplicativo emite "iniciou"
      // dizendo o que começou a fazer, e a linha segue o que ele informar.
      if (acao === "instalar") await api.mcpInstall(servidor.id);
      else if (acao === "remover") await api.mcpUninstall(servidor.id);
      else if (acao === "conectar") await api.connectConnector(servidor.id);
      else await api.disconnectConnector(servidor.id);
    } catch (cause) {
      setErro(message(cause));
      setObras((atual) => {
        const proxima = { ...atual };
        delete proxima[servidor.id];
        return proxima;
      });
    } finally {
      ocupado.current.delete(servidor.id);
      await recarregar();
      onSaved();
    }
  };

  const cancelar = async (servidor: ServidorCatalogo) => {
    setErro(null);
    try {
      const obtido = await api.mcpInstallCancel(servidor.id);
      // `false` é a resposta real do núcleo: não havia obra em curso. Uma
      // rejeição é outra coisa — o motivo real vai para a faixa de erro, em
      // vez de virar um "nada para cancelar" que não aconteceu.
      if (!obtido) setErro(t("servidores.nadaParaCancelar", { nome: servidor.nome }));
    } catch (cause) {
      setErro(message(cause));
    }
  };

  const alternar = async () => {
    if (publicando.current) return;
    publicando.current = true;
    setErro(null);
    try {
      if (estadoServidor?.ativa) await api.mcpServidorFechar();
      else await api.mcpServidorPublicar();
    } catch (cause) {
      setErro(message(cause));
    } finally {
      publicando.current = false;
      await recarregarServidor();
      onSaved();
    }
  };

  // O conector embutido não é um servidor que o usuário cadastrou, e os do
  // catálogo já têm linha própria: aqui sobram só os cadastrados à mão.
  const ids = new Set(
    (catalogo ?? [])
      .filter((servidor) => servidor.cadastrado)
      .map((servidor) => servidor.id),
  );
  const manuais = connectors.filter(
    (connector) => connector.kind !== "builtin" && !ids.has(connector.id),
  );

  return (
    <div className="modal" role="dialog" aria-modal="true" aria-label={t("servidores.titulo")}>
      <div className="modal-card">
        <header className="sheet-head">
          <span className="sheet-title">{t("servidores.titulo")}</span>
          <button
            type="button"
            className="sheet-close"
            onClick={onClose}
            aria-label={t("geral.fechar")}
          >
            <Icon name="cross" />
          </button>
        </header>

        <p className="sheet-sep">{t("servidores.catalogo")}</p>
        <ul className="server-rows">
          {catalogo === null && (
            /* `<ul>` só aceita `<li>`: aqui o esqueleto é um item, não a janela. */
            <li className="skeleton" aria-hidden="true">
              <span className="skeleton-row" />
              <span className="skeleton-row" />
              <span className="skeleton-row" />
            </li>
          )}
          {catalogo?.map((servidor) => {
            const obra = obras[servidor.id];
            const falha = falhas[servidor.id];
            const atualizada = servidor.instalada === servidor.versao;
            return (
              <li key={servidor.id} className="server-row server-catalogo">
                <span className="server-name">{servidor.nome}</span>
                <p className="server-pacote">
                  {servidor.pacote}@{servidor.versao}
                </p>
                <p className="server-descricao">{servidor.descricao}</p>
                <div className="server-marcas">
                  {atualizada ? (
                    <span className="server-chip ok">
                      {t("servidores.versaoInstalada", { versao: servidor.instalada ?? "" })}
                    </span>
                  ) : servidor.instalada ? (
                    <span className="server-chip aviso">
                      {t("servidores.versaoDivergente", {
                        instalada: servidor.instalada,
                        versao: servidor.versao,
                      })}
                    </span>
                  ) : (
                    <span className="server-chip">{t("servidores.naoInstalado")}</span>
                  )}
                  {servidor.requerPastas && (
                    <span className="server-chip">{t("servidores.requerPastas")}</span>
                  )}
                  {servidor.conectado && (
                    <span className="server-chip ok">
                      {t("servidores.ferramentas", { n: servidor.ferramentas })}
                    </span>
                  )}
                </div>
                {obra && (
                  <div className="server-obra">
                    <span className="server-obra-fase">
                      {t(obra.fase === "removendo" ? "servidores.removendo" : "servidores.instalando")}
                    </span>
                    <button
                      type="button"
                      className="ghost small"
                      onClick={() => void cancelar(servidor)}
                    >
                      {t("geral.cancelar")}
                    </button>
                    {obra.linhas.length > 0 && (
                      <pre className="server-log">{obra.linhas.join("\n")}</pre>
                    )}
                  </div>
                )}
                {!obra && (
                  <div className="server-acoes">
                    {atualizada && servidor.cadastrado && !servidor.conectado && (
                      <button
                        type="button"
                        className="primary small"
                        onClick={() => void agir(servidor, "conectar")}
                      >
                        {t("servidores.conectar")}
                      </button>
                    )}
                    {servidor.conectado && (
                      <button
                        type="button"
                        className="ghost small"
                        onClick={() => void agir(servidor, "desconectar")}
                      >
                        {t("servidores.desconectar")}
                      </button>
                    )}
                    {!atualizada && (
                      <button
                        type="button"
                        className="primary small"
                        onClick={() => void agir(servidor, "instalar")}
                      >
                        {servidor.instalada
                          ? t("servidores.atualizar", { versao: servidor.versao })
                          : t("servidores.instalar")}
                      </button>
                    )}
                    {atualizada && !servidor.cadastrado && (
                      <button
                        type="button"
                        className="primary small"
                        onClick={() => void agir(servidor, "instalar")}
                      >
                        {t("servidores.adicionar")}
                      </button>
                    )}
                    {servidor.instalada && (
                      <button
                        type="button"
                        className="danger icon-only"
                        onClick={() => void agir(servidor, "remover")}
                        aria-label={t("servidores.desinstalar", { nome: servidor.nome })}
                      >
                        <Icon name="cross" />
                      </button>
                    )}
                  </div>
                )}
                {!obra && falha && <p className="server-falha">{falha}</p>}
              </li>
            );
          })}
        </ul>

        <p className="sheet-sep">{t("servidorMcp.estado")}</p>
        <ul className="server-rows">
          <li className="server-row server-catalogo">
            <span className="server-name">{t("servidorMcp.nome")}</span>
            <div className="server-marcas">
              <span
                className={`server-chip${estadoServidor?.ativa ? " ok" : ""}`}
              >
                {t(estadoServidor?.ativa ? "servidorMcp.servindo" : "servidorMcp.parada")}
              </span>
              {estadoServidor && (
                <span className="server-chip">
                  {t("servidores.ferramentas", { n: estadoServidor.ferramentas })}
                </span>
              )}
            </div>
            {estadoServidor?.ativa && (
              <>
                <Copiavel rotulo={t("servidorMcp.endereco")} valor={estadoServidor.url} />
                {token && (
                  <Copiavel rotulo={t("servidorMcp.token")} valor={token} oculto />
                )}
              </>
            )}
            <div className="server-acoes">
              <button
                type="button"
                className={estadoServidor?.ativa ? "ghost small" : "primary small"}
                onClick={() => void alternar()}
              >
                {t(estadoServidor?.ativa ? "servidorMcp.fechar" : "servidorMcp.abrir")}
              </button>
            </div>
          </li>
        </ul>

        {erro && <p className="sheet-error">{erro}</p>}

        {manuais.length > 0 && (
          <>
            <p className="sheet-sep">{t("servidores.cadastrados")}</p>
            <ul className="server-rows">
              {manuais.map((connector) => (
                <li key={connector.id} className="server-row">
                  <span className="server-name">{connector.name}</span>
                  <button
                    type="button"
                    className="danger icon-only"
                    onClick={() => onRemove(connector.id)}
                    aria-label={t("servidores.remover", { nome: connector.name })}
                  >
                    <Icon name="cross" />
                  </button>
                </li>
              ))}
            </ul>
          </>
        )}
        <ConnectorForm onSaved={onSaved} />
      </div>
    </div>
  );
}

/** Um valor que se cola, não se digita: o endereço e o segredo saem do
    aplicativo exatamente como um cliente externo precisa recebê-los. O segredo
    fica mascarado na tela — copiar é o único caminho dele sair daqui. */
function Copiavel({
  rotulo,
  valor,
  oculto = false,
}: {
  rotulo: string;
  valor: string;
  oculto?: boolean;
}) {
  const t = useT();
  const podeCopiar =
    typeof navigator !== "undefined" &&
    typeof navigator.clipboard?.writeText === "function";
  const [estado, setEstado] = useState<"nenhum" | "copiado" | "falhou">("nenhum");

  async function copiar() {
    try {
      await navigator.clipboard.writeText(valor);
      setEstado("copiado");
    } catch {
      // A recusa é do ambiente, não um sucesso disfarçado.
      setEstado("falhou");
    }
  }

  return (
    <div className="server-valor">
      <span className="server-valor-rotulo">{rotulo}</span>
      <code className="server-valor-texto">
        {oculto ? "•".repeat(Math.min(valor.length, 24)) : valor}
      </code>
      {podeCopiar && (
        <button
          type="button"
          className="ghost small"
          onClick={() => void copiar()}
          title={t("conversa.copiar")}
        >
          <Icon name={estado === "copiado" ? "check" : "copiar"} />
          {estado === "copiado"
            ? t("conversa.copiado")
            : estado === "falhou"
              ? t("conversa.copiarFalhou")
              : t("conversa.copiar")}
        </button>
      )}
    </div>
  );
}
