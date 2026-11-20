import { useCallback, useEffect, useState } from "react";
import { api, message, onRuntimeEvent } from "../lib/api";
import { bytes, inteiro } from "../lib/display";
import { useIdioma, useT } from "../lib/i18n";
import type {
  RuntimeEvento,
  RuntimePacote,
  RuntimeVeredito,
} from "../lib/types";
import { useFecharComEscape } from "../lib/teclado";
import { Icon } from "./Icon";
import { BarraDeTrabalho } from "./BarraDeTrabalho";

interface Props {
  onClose: () => void;
  onChange: () => void;
}

interface Job {
  fase: "baixando" | "extraindo" | "conferindo";
  done: number;
  total: number;
  speed: number;
}

/// As DLLs não estão no repositório: num PC novo o download dos pesos termina e
/// a inferência não abre. Esta tela é o passo que faltava — baixar o pacote
/// oficial, conferir cada hash e pô-lo onde o carregador procura.
export function RuntimeDialog({ onClose, onChange }: Props) {
  useFecharComEscape(onClose);
  const t = useT();
  const { id: idioma } = useIdioma();
  const [pacotes, setPacotes] = useState<RuntimePacote[]>([]);
  const [vereditos, setVereditos] = useState<Record<string, RuntimeVeredito>>({});
  const [jobs, setJobs] = useState<Record<string, Job>>({});
  const [faults, setFaults] = useState<Record<string, string>>({});
  const [conferindo, setConferindo] = useState(false);
  const [erro, setErro] = useState<string | null>(null);
  /// A primeira leitura terminou. Sem isto, a lista vazia de um
  /// `runtime_estado` que ainda não respondeu é exibida como "não há pacotes".
  const [lido, setLido] = useState(false);

  const refresh = useCallback(async () => {
    try {
      setPacotes(await api.runtimeEstado());
      setLido(true);
      setErro(null);
    } catch (cause) {
      setErro(message(cause));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    const handle = (event: RuntimeEvento) => {
      const soltar = () =>
        setJobs((current) => {
          const next = { ...current };
          delete next[event.id];
          return next;
        });
      switch (event.type) {
        case "iniciou":
          setFaults((current) => ({ ...current, [event.id]: "" }));
          setJobs((current) => ({
            ...current,
            [event.id]: {
              fase: "baixando",
              done: 0,
              total: event.totalBytes,
              speed: 0,
            },
          }));
          break;
        case "progresso":
          setJobs((current) => ({
            ...current,
            [event.id]: {
              fase: "baixando",
              done: event.doneBytes,
              total: event.totalBytes,
              speed: event.bytesPorSegundo,
            },
          }));
          break;
        case "extraindo":
        case "conferindo":
          setJobs((current) => ({
            ...current,
            [event.id]: {
              ...(current[event.id] ?? { done: 0, total: 0, speed: 0 }),
              fase: event.type,
            },
          }));
          break;
        case "concluiu":
          soltar();
          // O veredito antigo descreve o que estava no disco antes: instalar
          // substituiu os arquivos, então a conferência velha não vale mais.
          setVereditos((current) => {
            const next = { ...current };
            delete next[event.id];
            return next;
          });
          void refresh();
          onChange();
          break;
        case "cancelou":
          soltar();
          void refresh();
          break;
        case "falhou":
          soltar();
          setFaults((current) => ({ ...current, [event.id]: event.erro }));
          void refresh();
          break;
      }
    };
    void onRuntimeEvent(handle).then((un) => {
      if (cancelled) un();
      else unlisten = un;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [refresh, onChange]);

  async function instalar(ids: string[]) {
    setErro(null);
    try {
      await api.runtimeInstalar(ids);
    } catch (cause) {
      setErro(message(cause));
    }
  }

  /// Parar também é um pedido ao núcleo: a falha de antes não pode continuar na
  /// faixa quando este clique já produziu outra resposta. É o que `instalar` e
  /// `conferir` fazem neste mesmo arquivo.
  async function cancelar() {
    setErro(null);
    try {
      await api.runtimeCancelar();
    } catch (cause) {
      setErro(message(cause));
    }
  }

  async function conferir() {
    setConferindo(true);
    setErro(null);
    try {
      const novos = await api.runtimeVerificar();
      setVereditos(Object.fromEntries(novos.map((v) => [v.id, v])));
    } catch (cause) {
      setErro(message(cause));
    } finally {
      setConferindo(false);
    }
  }

  const emCurso = Object.keys(jobs).length > 0;
  const ocupado = emCurso || conferindo;
  const faltam = pacotes.filter((p) => !p.completo).map((p) => p.id);

  return (
    <div
      className="modal"
      role="dialog"
      aria-modal="true"
      aria-label={t("runtime.titulo")}
    >
      <div className="modal-card runtime-card">
        <header className="sheet-head">
          <span className="sheet-title">{t("runtime.titulo")}</span>
          <button
            type="button"
            className="sheet-close"
            onClick={onClose}
            aria-label={t("geral.fechar")}
          >
            <Icon name="cross" />
          </button>
        </header>

        <div className="runtime-acoes">
          <button
            type="button"
            className="primary"
            disabled={ocupado || faltam.length === 0}
            onClick={() => void instalar(faltam)}
            title={faltam.length === 0 ? t("runtime.nadaFalta") : undefined}
          >
            <Icon name="download" />
            {t("runtime.instalar")}
            {faltam.length > 0 && (
              <span className="model-count">{faltam.length}</span>
            )}
          </button>
          <button type="button" disabled={ocupado} onClick={() => void conferir()}>
            <Icon name="shield" />
            {t("runtime.conferir")}
          </button>
          <button type="button" disabled={ocupado} onClick={() => void refresh()}>
            <Icon name="refresh" />
            {t("runtime.recarregar")}
          </button>
          {emCurso && (
            <button
              type="button"
              className="danger"
              onClick={() => void cancelar()}
            >
              <Icon name="stop" />
              {t("runtime.cancelar")}
            </button>
          )}
        </div>

        {!lido && erro === null && (
          <p className="sheet-note">{t("geral.carregando")}</p>
        )}

        <ul className="runtime-lista">
          {pacotes.map((pacote) => {
            const job = jobs[pacote.id];
            const fault = faults[pacote.id] ?? "";
            const veredito = vereditos[pacote.id];
            const batem = veredito?.arquivos.filter((a) => a.bateu).length ?? 0;
            return (
              <li
                key={pacote.id}
                className={pacote.completo ? "runtime-pacote pronto" : "runtime-pacote"}
              >
                <span className="runtime-cabeca">
                  <span className="runtime-nome">{pacote.nome}</span>
                  <span className="runtime-estado">
                    <Icon name={pacote.completo ? "check" : "warning"} />
                    {pacote.completo ? t("runtime.pronto") : t("runtime.faltando")}
                  </span>
                  <button
                    type="button"
                    disabled={ocupado}
                    onClick={() => void instalar([pacote.id])}
                  >
                    <Icon name="download" />
                    {t("runtime.instalar")}
                  </button>
                </span>

                <span className="runtime-chips">
                  <span className="memoria-chip">
                    {bytes(pacote.bytesPacote, idioma)}
                  </span>
                  <span className="memoria-chip">
                    {t("runtime.arquivos", {
                      n: inteiro(pacote.arquivos.length, idioma),
                    })}
                  </span>
                  {pacote.faltando.length > 0 && (
                    <span className="memoria-chip aviso">
                      {t("runtime.faltando")}: {inteiro(pacote.faltando.length, idioma)}
                    </span>
                  )}
                  {veredito !== undefined && (
                    <span
                      className={veredito.conferido ? "memoria-chip gpu" : "memoria-chip aviso"}
                    >
                      <Icon name={veredito.conferido ? "shield" : "warning"} />
                      {veredito.conferido
                        ? t("runtime.conferido")
                        : t("runtime.naoConferido")}
                      {` ${batem}/${veredito.arquivos.length}`}
                    </span>
                  )}
                </span>

                {pacote.pasta !== null && (
                  <span className="runtime-pasta" title={t("runtime.pasta")}>
                    <Icon name="folder" />
                    {pacote.pasta}
                  </span>
                )}

                {job !== undefined && (
                  <BarraDeTrabalho
                    fase={t(`runtime.${job.fase}`)}
                    feito={job.done}
                    total={job.total}
                    velocidade={job.speed}
                    // Só o download conta bytes. Na extração e na conferência a
                    // ponta já dizia "—"; a barra cheia ali era o percentual de
                    // outra fase, relido como se fosse desta.
                    contavel={job.fase === "baixando"}
                  />
                )}

                {fault !== "" && (
                  <p className="sheet-error" role="alert">
                    <Icon name="warning" />
                    {fault}
                  </p>
                )}

                {pacote.faltando.length > 0 && (
                  <ul className="runtime-faltando">
                    {pacote.faltando.map((nome) => (
                      <li key={nome}>
                        <Icon name="warning" />
                        {nome}
                      </li>
                    ))}
                  </ul>
                )}

                {veredito !== undefined && (
                  <ul className="runtime-arquivos">
                    {veredito.arquivos.map((arquivo) => (
                      <li
                        key={arquivo.nome}
                        className={arquivo.bateu ? "bate" : "diverge"}
                        title={`${t("runtime.esperado")}: ${arquivo.esperado}\n${t("runtime.medido")}: ${arquivo.medido || "—"}`}
                      >
                        <Icon name={arquivo.bateu ? "check" : "warning"} />
                        <span className="runtime-arquivo-nome">{arquivo.nome}</span>
                        <span className="runtime-arquivo-veredito">
                          {arquivo.presente
                            ? t("runtime.presente")
                            : t("runtime.ausente")}
                          {" · "}
                          {arquivo.bateu ? t("runtime.bate") : t("runtime.diverge")}
                        </span>
                      </li>
                    ))}
                  </ul>
                )}
              </li>
            );
          })}
        </ul>

        {erro !== null && (
          <p className="sheet-error" role="alert">
            <Icon name="warning" />
            {erro}
          </p>
        )}
      </div>
    </div>
  );
}