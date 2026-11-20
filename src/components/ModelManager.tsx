import { useCallback, useEffect, useMemo, useState } from "react";
import { api, message, onModelEvent } from "../lib/api";
import { bytes, numero, previsao } from "../lib/display";
import { useIdioma, useT, type Traduzir } from "../lib/i18n";
import { useJobs, type Job } from "../lib/jobs";
import type { Acelerador, Alojado, ModelEvent, ModelStatus } from "../lib/types";
import { useFecharComEscape } from "../lib/teclado";
import { Icon } from "./Icon";
import { BarraDeTrabalho } from "./BarraDeTrabalho";

interface Props {
  onClose: () => void;
  onLoaded: () => void;
}

/// As fases que o downloader deste gerenciador anuncia. `preparando` é a pedida
/// antes do primeiro byte; `apoio` conta arquivos, não bytes.
type Fase = "preparando" | "pesos" | "apoio";

/// Onde os bytes ficaram, nas palavras do dono da máquina. O texto é a previsão
/// feita com o cabeçalho do GGUF antes da carga — a mesma formulação da tela de
/// memória; o título é a VRAM que a carga de fato levou, medida nos dois lados
/// do `llama_load_model_from_file`.
function alojamento(
  alojado: Alojado,
  traduzir: Traduzir,
  idioma: string,
): { texto: string; medida: string } {
  return {
    texto: previsao(alojado.previsto, traduzir, idioma),
    medida: traduzir("modelos.vramCedida", { cedido: bytes(alojado.cedido, idioma) }),
  };
}

export function ModelManager({ onClose, onLoaded }: Props) {
  useFecharComEscape(onClose);
  const t = useT();
  const { id: idioma } = useIdioma();
  const [models, setModels] = useState<ModelStatus[]>([]);
  const [loaded, setLoaded] = useState<string | null>(null);
  const [fixado, setFixado] = useState("");
  const [acelerador, setAcelerador] = useState<Acelerador | null>(null);
  const { jobs, faltas: faults, aplicar, limparFalta } = useJobs<Fase>();
  /// Os downloads que o núcleo tem registrados agora (`ai_status.downloading`).
  const [emCurso, setEmCurso] = useState<string[]>([]);
  const [tab, setTab] = useState<"descobrir" | "meus">("descobrir");
  const [query, setQuery] = useState("");
  const [open, setOpen] = useState<Record<string, boolean>>({});
  const [acting, setActing] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  /// A primeira leitura do estado terminou. Sem isto, a lista vazia de um
  /// `ai_status` que ainda não respondeu é exibida como "nada encontrado".
  const [lido, setLido] = useState(false);

  const refresh = useCallback(async () => {
    try {
      const status = await api.aiStatus();
      setModels(status.models);
      setLoaded(status.loaded);
      setFixado(status.ajustes.modelo);
      setAcelerador(status.acelerador);
      setEmCurso(status.downloading);
      setLido(true);
    } catch (cause) {
      setError(message(cause));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    const handle = (event: ModelEvent) => {
      switch (event.type) {
        case "started":
          aplicar({ tipo: "iniciou", id: event.id, total: event.totalBytes, fase: "preparando" });
          break;
        case "support":
          aplicar({ tipo: "fase", id: event.id, fase: "apoio", arquivo: event.file });
          break;
        case "progress":
          aplicar({
            tipo: "bytes",
            id: event.id,
            feito: event.doneBytes,
            total: event.totalBytes,
            velocidade: event.bytesPerSecond,
            fase: "pesos",
          });
          break;
        case "done":
        case "cancelled":
          aplicar({ tipo: "terminou", id: event.id });
          void refresh();
          break;
        case "failed":
          aplicar({ tipo: "terminou", id: event.id, erro: event.error });
          void refresh();
          break;
      }
    };
    void onModelEvent(handle).then((un) => {
      if (cancelled) un();
      else unlisten = un;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [refresh, aplicar]);

  const families = useMemo(() => {
    const terms = query.trim().toLowerCase().split(/\s+/).filter((t) => t !== "");
    const groups = new Map<string, ModelStatus[]>();
    for (const model of models) {
      const haystack = `${model.series} ${model.quant}`.toLowerCase();
      if (!terms.every((term) => haystack.includes(term))) continue;
      groups.set(model.series, [...(groups.get(model.series) ?? []), model]);
    }
    return [...groups.entries()].map(([series, variants]) => ({
      series,
      variants: [...variants].sort((a, b) => a.weightsBytes - b.weightsBytes),
    }));
  }, [models, query]);

  const mine = useMemo(
    () =>
      models
        .filter((model) => model.downloadedBytes > 0 || model.partialBytes > 0)
        .sort((a, b) => b.downloadedBytes - a.downloadedBytes),
    [models],
  );

  async function download(id: string) {
    setError(null);
    limparFalta(id);
    try {
      await api.modelDownload(id);
    } catch (cause) {
      setError(message(cause));
    }
  }

  async function cancel(id: string) {
    setError(null);
    try {
      await api.modelDownloadCancel(id);
    } catch (cause) {
      setError(message(cause));
    }
  }

  async function activate(id: string) {
    setActing(id);
    setError(null);
    try {
      await api.modelLoad(id);
      await refresh();
      onLoaded();
    } catch (cause) {
      setError(message(cause));
    } finally {
      setActing(null);
    }
  }

  async function soltar() {
    setActing("roteador");
    setError(null);
    try {
      setFixado((await api.modelSolto()).modelo);
      await refresh();
      onLoaded();
    } catch (cause) {
      setError(message(cause));
    } finally {
      setActing(null);
    }
  }

  async function remove(id: string) {
    setActing(id);
    setError(null);
    try {
      await api.modelDelete(id);
      await refresh();
    } catch (cause) {
      setError(message(cause));
    } finally {
      setActing(null);
    }
  }

  /// O trabalho de uma linha é o que os eventos contaram para ela ou o que o
  /// núcleo registra como em curso: reabrir a janela no meio de um download
  /// deixava a linha oferecendo "Baixar", e o núcleo recusava o segundo pedido
  /// com "este modelo já está baixando" (`commands.rs`). Sem os bytes lidos
  /// ainda — eles só chegam por evento — a barra fica vazia e a ponta mostra o
  /// traço; a fase desconhecida não é inventada.
  const trabalho = (id: string): Job<Fase> | undefined =>
    jobs[id] ??
    (emCurso.includes(id) ? { fase: null, feito: 0, total: 0, velocidade: 0 } : undefined);

  const row = (model: ModelStatus) => (
    <VariantRow
      key={model.id}
      model={model}
      job={trabalho(model.id)}
      fault={faults[model.id] ?? ""}
      loaded={loaded === model.id}
      acting={acting === model.id}
      onDownload={() => void download(model.id)}
      onCancel={() => void cancel(model.id)}
      onUse={() => void activate(model.id)}
      onRemove={() => void remove(model.id)}
    />
  );

  /// O nome do modelo que o dono fixou: pode ser um id que já não está no disco,
  /// e nesse caso o próprio id é o que resta mostrar.
  const fixoNome =
    models.find((model) => model.id === fixado)?.display ?? fixado;

  const lugar =
    acelerador !== null && acelerador.alojado !== null
      ? alojamento(acelerador.alojado, t, idioma)
      : null;

  return (
    <div
      className="modal"
      role="dialog"
      aria-modal="true"
      aria-label={t("modelos.titulo")}
    >
      <div className="modal-card model-card">
        <header className="sheet-head">
          <span className="sheet-title">{t("modelos.titulo")}</span>
          {acelerador !== null && (
            <span
              className={
                acelerador.tipo === "gpu"
                  ? "model-acelerador gpu"
                  : "model-acelerador"
              }
              title={acelerador.motivo ?? acelerador.dispositivo}
            >
              {acelerador.tipo === "gpu"
                ? t("modelos.aceleradorGpu")
                : t("modelos.aceleradorCpu")}
              {acelerador.dispositivo !== "" && (
                <span className="model-acelerador-nome">{acelerador.dispositivo}</span>
              )}
            </span>
          )}
          {lugar !== null && (
            <span className="model-alojamento" title={lugar.medida}>
              {lugar.texto}
            </span>
          )}
          <button
            type="button"
            className={fixado === "" ? "model-roteador on" : "model-roteador"}
            disabled={fixado === "" || acting !== null}
            onClick={() => void soltar()}
            title={
              fixado === ""
                ? t("modelos.roteadorNota")
                : t("modelos.devolverNota", { nome: fixoNome })
            }
          >
            <Icon name="spark" />
            {fixado === "" ? t("modelos.roteador") : t("modelos.devolver")}
          </button>
          <button
            type="button"
            className="sheet-close"
            onClick={onClose}
            aria-label={t("geral.fechar")}
          >
            <Icon name="cross" />
          </button>
        </header>

        <div className="model-tabs">
          <button
            type="button"
            className={tab === "descobrir" ? "model-tab active" : "model-tab"}
            onClick={() => setTab("descobrir")}
          >
            <Icon name="search" />
            {t("modelos.descobrir")}
          </button>
          <button
            type="button"
            className={tab === "meus" ? "model-tab active" : "model-tab"}
            onClick={() => setTab("meus")}
          >
            <Icon name="cube" />
            {t("modelos.meus")}
            {mine.length > 0 && <span className="model-count">{mine.length}</span>}
          </button>
        </div>

        {error !== null && (
          <div className="model-fault" role="alert">
            <Icon name="warning" />
            <span>{error}</span>
          </div>
        )}

        {tab === "descobrir" ? (
          <>
            <label className="model-search">
              <Icon name="search" />
              <input
                value={query}
                onChange={(event) => setQuery(event.target.value)}
                placeholder={t("modelos.buscarPlaceholder")}
                aria-label={t("modelos.buscar")}
              />
            </label>
            <div className="model-families">
              {families.map((family) => {
                const auto = family.variants.filter((model) => model.routable);
                const extra = family.variants.filter((model) => !model.routable);
                const expanded = open[family.series] === true;
                const head = family.variants[0];
                return (
                  <section key={family.series} className="model-family">
                    <header className="model-family-head">
                      <span className="model-family-name">{family.series}</span>
                      <span className="model-family-meta">
                        {t("modelos.parametros", {
                          n: numero(head.parametersB, idioma, 1),
                        })}{" "}
                        ·{" "}
                        {t("modelos.contexto", {
                          // O número é o que o cabeçalho GGUF declarou, como no
                          // chip de janela da geração: arredondar para "33k"
                          // punha na tela um limite que ninguém leu.
                          n: numero(head.contextLimit, idioma, 0),
                        })}{" "}
                        · {head.license}
                      </span>
                    </header>
                    <div className="model-variants">
                      {auto.map(row)}
                      {expanded && extra.map(row)}
                    </div>
                    {extra.length > 0 && (
                      <button
                        type="button"
                        className="model-more"
                        onClick={() =>
                          setOpen((current) => ({
                            ...current,
                            [family.series]: !expanded,
                          }))
                        }
                      >
                        {expanded
                          ? t("modelos.menosQuants")
                          : t("modelos.maisQuants", { n: extra.length })}
                      </button>
                    )}
                  </section>
                );
              })}
              {!lido && error === null && (
                <p className="sheet-note">{t("geral.carregando")}</p>
              )}
              {lido && families.length === 0 && (
                <p className="sheet-note">{t("modelos.nadaEncontrado")}</p>
              )}
            </div>
          </>
        ) : (
          <div className="model-variants">
            {mine.map(row)}
            {!lido && error === null && (
              <p className="sheet-note">{t("geral.carregando")}</p>
            )}
            {lido && mine.length === 0 && (
              <p className="sheet-note">{t("modelos.nenhumDisco")}</p>
            )}
          </div>
        )}
      </div>
    </div>
  );
}

interface RowProps {
  model: ModelStatus;
  job: Job<Fase> | undefined;
  fault: string;
  loaded: boolean;
  acting: boolean;
  onDownload: () => void;
  onCancel: () => void;
  onUse: () => void;
  onRemove: () => void;
}

function VariantRow({
  model,
  job,
  fault,
  loaded,
  acting,
  onDownload,
  onCancel,
  onUse,
  onRemove,
}: RowProps) {
  const t = useT();
  const { id: idioma } = useIdioma();
  const onDisk = model.downloadedBytes;
  const interrupted = model.partialBytes > 0 && !model.complete;

  return (
    <div className={loaded ? "model-variant loaded" : "model-variant"}>
      <div className="model-variant-head">
        <span className="model-quant">{model.quant}</span>
        {model.routable && <span className="model-auto">{t("modelos.automatico")}</span>}
        {!model.complete && (
          <span className="model-size">{bytes(model.weightsBytes, idioma)}</span>
        )}
        {model.complete ? (
          <span className="model-state ready">
            <Icon name="check" />
            {bytes(onDisk, idioma)}
          </span>
        ) : onDisk > 0 ? (
          <span className="model-state partial">{bytes(onDisk, idioma)}</span>
        ) : interrupted ? (
          <span className="model-state partial">
            {bytes(model.partialBytes, idioma)}
          </span>
        ) : null}
      </div>

      {job !== undefined ? (
        <BarraDeTrabalho
          fase={
            job.fase === null
              ? ""
              : job.fase === "apoio"
                ? job.arquivo ?? ""
                : t(`modelos.${job.fase}`)
          }
          feito={job.feito}
          total={job.total}
          velocidade={job.velocidade}
          // Os arquivos de apoio são cópias locais: não há byte de rede a
          // contar, então o percentual do peso fica para trás em vez de virar
          // andamento da fase.
          contavel={job.fase !== "apoio"}
          aoCancelar={onCancel}
          rotuloCancelar={t("modelos.cancelar", { nome: model.display })}
        />
      ) : (
        <div className="model-actions">
          {model.complete && (
            <button
              type="button"
              className={loaded ? "primary" : ""}
              disabled={acting || loaded}
              onClick={onUse}
            >
              {loaded ? t("modelos.emUso") : t("modelos.usar")}
            </button>
          )}
          {!model.complete && (
            <button
              type="button"
              className="primary"
              disabled={acting}
              onClick={onDownload}
            >
              <Icon name="download" />
              {interrupted ? t("modelos.continuar") : t("modelos.baixar")}
            </button>
          )}
          {(onDisk > 0 || interrupted) && (
            <button
              type="button"
              className="danger icon-only"
              disabled={acting}
              onClick={onRemove}
              aria-label={t("modelos.excluir", { nome: model.display })}
            >
              <Icon name="trash" />
            </button>
          )}
        </div>
      )}

      {fault !== "" && <p className="model-variant-fault">{fault}</p>}
    </div>
  );
}