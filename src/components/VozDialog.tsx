import { useCallback, useEffect, useState } from "react";
import { api, message, onVozEvent } from "../lib/api";
import { GENERO_KEY, SERVICO_KEY, bytes } from "../lib/display";
import { useIdioma, useT } from "../lib/i18n";
import type { AjustesVoz, EstadoVoz, MedidorVoz, StatusVoz, VozEvento } from "../lib/types";
import { useFecharComEscape } from "../lib/teclado";
import { Icon } from "./Icon";
import { BarraDeTrabalho, Regua } from "./BarraDeTrabalho";

interface Props {
  estado: EstadoVoz | null;
  medidor: MedidorVoz;
  onClose: () => void;
  onRefresh: () => void;
}

interface Job {
  fase: "baixando" | "extraindo";
  done: number;
  total: number;
  speed: number;
}

type Aba = "fala" | "escuta" | "modelos";

export function VozDialog({ estado, medidor, onClose, onRefresh }: Props) {
  useFecharComEscape(onClose);
  const t = useT();
  const [aba, setAba] = useState<Aba>("fala");
  const [jobs, setJobs] = useState<Record<string, Job>>({});
  const [faults, setFaults] = useState<Record<string, string>>({});
  const [velocidade, setVelocidade] = useState<number | null>(null);
  const [erro, setErro] = useState<string | null>(null);

  const ajustes = estado?.ajustes ?? null;
  const ritmo = velocidade ?? ajustes?.velocidade ?? 1;

  // O valor salvo voltou do disco: a régua volta a mostrar a verdade, e uma
  // arrastada que falhou não fica pendurada num número que nunca toca.
  useEffect(() => {
    setVelocidade(null);
  }, [ajustes?.velocidade]);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    const handle = (event: VozEvento) => {
      switch (event.type) {
        case "iniciou":
          setFaults((current) => ({ ...current, [event.id]: "" }));
          setJobs((current) => ({
            ...current,
            [event.id]: { fase: "baixando", done: 0, total: event.totalBytes, speed: 0 },
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
        // O pacote do Kokoro são trezentos e setenta e oito arquivos: aqui não
        // há byte de rede a contar, só o `tar` do Windows trabalhando.
        case "extraindo":
          setJobs((current) => ({
            ...current,
            [event.id]: { fase: "extraindo", done: 0, total: 0, speed: 0 },
          }));
          break;
        case "concluiu":
        case "cancelou":
          setJobs((current) => {
            const next = { ...current };
            delete next[event.id];
            return next;
          });
          onRefresh();
          break;
        case "falhou":
          setJobs((current) => {
            const next = { ...current };
            delete next[event.id];
            return next;
          });
          setFaults((current) => ({ ...current, [event.id]: event.erro }));
          onRefresh();
          break;
      }
    };
    void onVozEvent(handle).then((un) => {
      if (cancelled) un();
      else unlisten = un;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [onRefresh]);

  const gravar = useCallback(
    async (proximos: AjustesVoz) => {
      setErro(null);
      try {
        await api.vozAjustar(proximos);
        onRefresh();
      } catch (cause) {
        setErro(message(cause));
        setVelocidade(null);
      }
    },
    [onRefresh],
  );

  async function prever(sid: number) {
    if (ajustes === null) return;
    setErro(null);
    try {
      await api.vozPrever(t("voz.amostraTexto"), ajustes.modeloFala, sid, ritmo);
    } catch (cause) {
      setErro(message(cause));
    }
  }

  /// Parar a fala pode falhar como qualquer invoke: o motivo real vai para a
  /// faixa de erro, não para uma rejeição sem dono no console.
  async function parar() {
    setErro(null);
    try {
      await api.vozParar();
    } catch (cause) {
      setErro(message(cause));
    }
  }

  async function baixar(modelo: StatusVoz) {
    setErro(null);
    setFaults((current) => ({ ...current, [modelo.id]: "" }));
    try {
      await api.vozBaixar(modelo.id);
    } catch (cause) {
      setErro(message(cause));
    }
  }

  async function cancela(modelo: StatusVoz) {
    setErro(null);
    try {
      await api.vozBaixarCancelar(modelo.id);
    } catch (cause) {
      setErro(message(cause));
    }
  }

  async function apagar(modelo: StatusVoz) {
    setErro(null);
    try {
      await api.vozApagar(modelo.id);
      onRefresh();
    } catch (cause) {
      setErro(message(cause));
    }
  }

  const doServico = (servico: StatusVoz["servico"]) =>
    (estado?.modelos ?? []).filter((modelo) => modelo.servico === servico);

  /// O download em curso é o que o motor registra em `baixando`, não só o que
  /// esta janela viu passar: reabri-la no meio do pacote do Kokoro deixava a
  /// linha oferecendo "Baixar" de novo, e o núcleo recusaria o segundo pedido
  /// com "já está baixando". Sem bytes lidos ainda, a barra fica vazia e a
  /// ponta mostra o traço — o que se sabe é a fase, não o andamento.
  const emCurso = (id: string): Job | undefined =>
    jobs[id] ??
    (estado?.baixando.includes(id) === true
      ? { fase: "baixando", done: 0, total: 0, speed: 0 }
      : undefined);

  const falha = erro ?? estado?.erro ?? null;

  return (
    <div
      className="modal"
      role="dialog"
      aria-modal="true"
      aria-label={t("voz.titulo")}
    >
      <div className="modal-card model-card">
        <header className="sheet-head">
          <span className="sheet-title">{t("voz.titulo")}</span>
          <span className="voz-pulso">
            {medidor.falando && (
              <button type="button" className="danger" onClick={() => void parar()}>
                <Icon name="stop" />
                {t("voz.pararFala")}
              </button>
            )}
          </span>
          <button
            type="button"
            className="sheet-close"
            onClick={onClose}
            aria-label={t("geral.fechar")}
          >
            <Icon name="cross" />
          </button>
        </header>

        {estado === null && <p className="sheet-note">{t("geral.carregando")}</p>}

        {estado !== null && !estado.disponivel && (
          <div className="model-fault" role="alert">
            <Icon name="warning" />
            <span>{estado.motivo ?? t("erros.vozNaoDisponivel")}</span>
          </div>
        )}

        {estado !== null && estado.disponivel && ajustes !== null && (
          <>
            <div className="model-tabs">
              {(["fala", "escuta", "modelos"] as Aba[]).map((nome) => (
                <button
                  key={nome}
                  type="button"
                  className={aba === nome ? "model-tab active" : "model-tab"}
                  onClick={() => setAba(nome)}
                >
                  <Icon name={nome === "fala" ? "speaker" : nome === "escuta" ? "mic" : "cube"} />
                  {t(`voz.aba${nome[0].toUpperCase()}${nome.slice(1)}`)}
                </button>
              ))}
            </div>

            {falha !== null && (
              <div className="model-fault" role="alert">
                <Icon name="warning" />
                <span>{falha}</span>
              </div>
            )}

            {aba === "fala" && (
              <div className="sheet-fields">
                <label className="field">
                  <span className="field-label">{t("voz.modelo")}</span>
                  <select
                    value={ajustes.modeloFala}
                    onChange={(event) =>
                      void gravar({ ...ajustes, modeloFala: event.target.value })
                    }
                  >
                    {doServico("fala").map((modelo) => (
                      <option key={modelo.id} value={modelo.id}>
                        {modelo.nome}
                        {modelo.baixado
                          ? ""
                          : ` · ${t("voz.faltando", { n: modelo.faltando.length })}`}
                      </option>
                    ))}
                  </select>
                </label>

                <div className="field">
                  <span className="field-label">{t("voz.vozes")}</span>
                  <div className="voz-vozes">
                    {estado.vozes.map((voz) => (
                      <div key={voz.sid} className="voz-voz">
                        <button
                          type="button"
                          className={voz.sid === ajustes.sid ? "voz-escolha on" : "voz-escolha"}
                          onClick={() => void gravar({ ...ajustes, sid: voz.sid })}
                        >
                          <span className="voz-nome">{voz.nome}</span>
                          <span className="voz-genero">{t(GENERO_KEY[voz.genero])}</span>
                        </button>
                        <button
                          type="button"
                          className="icon-only"
                          onClick={() => void prever(voz.sid)}
                          aria-label={t("voz.amostra", { nome: voz.nome })}
                        >
                          <Icon name="play" />
                        </button>
                      </div>
                    ))}
                  </div>
                </div>

                <Regua
                  titulo={t("voz.velocidade")}
                  valor={ritmo}
                  minimo={0.5}
                  maximo={2}
                  passo={0.05}
                  casas={2}
                  classeValor="voz-valor"
                  sufixo="×"
                  aoMudar={setVelocidade}
                  aoGravar={() => void gravar({ ...ajustes, velocidade: ritmo })}
                />

                <div className="field">
                  <span className="field-label">{t("voz.autoFalar")}</span>
                  <span className="toggle-row">
                    <button
                      type="button"
                      className={ajustes.autoFalar ? "toggle on" : "toggle"}
                      aria-pressed={ajustes.autoFalar}
                      onClick={() => void gravar({ ...ajustes, autoFalar: !ajustes.autoFalar })}
                    >
                      <span className="toggle-knob" />
                    </button>
                  </span>
                </div>

                <div className="field">
                  <span className="field-label">{t("voz.saida")}</span>
                  <span className="voz-dado">
                    {estado.dispositivos.saida ?? t("erros.vozSemSaida")}
                  </span>
                </div>
              </div>
            )}

            {aba === "escuta" && (
              <div className="sheet-fields">
                <label className="field">
                  <span className="field-label">{t("voz.modelo")}</span>
                  <select
                    value={ajustes.modeloEscuta}
                    onChange={(event) =>
                      void gravar({ ...ajustes, modeloEscuta: event.target.value })
                    }
                  >
                    <option value="">{t("voz.semEscuta")}</option>
                    {doServico("escuta").map((modelo) => (
                      <option key={modelo.id} value={modelo.id}>
                        {modelo.nome}
                        {modelo.baixado
                          ? ""
                          : ` · ${t("voz.faltando", { n: modelo.faltando.length })}`}
                      </option>
                    ))}
                  </select>
                </label>

                <div className="field">
                  <span className="field-label">{t("voz.entrada")}</span>
                  {estado.dispositivos.entradas.length > 0 ? (
                    <select
                      value={
                        ajustes.entrada || estado.dispositivos.entrada || ""
                      }
                      onChange={(event) =>
                        void gravar({ ...ajustes, entrada: event.target.value })
                      }
                    >
                      {estado.dispositivos.entradas.map((nome) => (
                        <option key={nome} value={nome}>
                          {nome}
                        </option>
                      ))}
                    </select>
                  ) : (
                    <span className="voz-dado">{t("erros.vozSemEntrada")}</span>
                  )}
                </div>

                <div className="field">
                  <span className="field-label">
                    {medidor.ouvindo ? t("voz.gravando") : t("voz.nivel")}
                  </span>
                  <span
                    className="voz-nivel"
                    role="progressbar"
                    aria-label={t("voz.nivel")}
                    aria-valuenow={Math.round(medidor.nivel * 100)}
                    aria-valuemin={0}
                    aria-valuemax={100}
                  >
                    <span
                      className="voz-nivel-fill"
                      style={{ width: `${Math.round(medidor.nivel * 100)}%` }}
                    />
                  </span>
                </div>
              </div>
            )}

            {aba === "modelos" && (
              <div className="model-variants">
                {estado.modelos.map((modelo) => (
                  <ModeloRow
                    key={modelo.id}
                    modelo={modelo}
                    job={emCurso(modelo.id)}
                    fault={faults[modelo.id] ?? ""}
                    escolhido={
                      modelo.id === ajustes.modeloFala ||
                      modelo.id === ajustes.modeloEscuta
                    }
                    onBaixar={() => void baixar(modelo)}
                    onCancelar={() => void cancela(modelo)}
                    onApagar={() => void apagar(modelo)}
                  />
                ))}
              </div>
            )}
          </>
        )}
      </div>
    </div>
  );
}

interface RowProps {
  modelo: StatusVoz;
  job: Job | undefined;
  fault: string;
  escolhido: boolean;
  onBaixar: () => void;
  onCancelar: () => void;
  onApagar: () => void;
}

function ModeloRow({ modelo, job, fault, escolhido, onBaixar, onCancelar, onApagar }: RowProps) {
  const t = useT();
  const { id: idioma } = useIdioma();

  return (
    <div className={escolhido ? "model-variant loaded" : "model-variant"}>
      <div className="model-variant-head">
        <span className="model-quant">{modelo.nome}</span>
        <span className="model-auto">{t(SERVICO_KEY[modelo.servico])}</span>
        {!modelo.baixado && <span className="model-size">{bytes(modelo.bytes, idioma)}</span>}
        {modelo.baixado ? (
          <span className="model-state ready">
            <Icon name="check" />
            {t("voz.ocupa", { bytes: bytes(modelo.bytes, idioma) })}
          </span>
        ) : (
          <span className="model-state partial">
            {t("voz.faltando", { n: modelo.faltando.length })}
          </span>
        )}
      </div>

      {job !== undefined ? (
        <BarraDeTrabalho
          fase={t(`voz.${job.fase}`)}
          feito={job.done}
          total={job.total}
          velocidade={job.speed}
          // Na extração do pacote não há byte de rede a contar: a barra fica
          // vazia e a ponta mostra o traço, em vez do último percentual.
          contavel={job.fase === "baixando"}
          aoCancelar={onCancelar}
          rotuloCancelar={t("voz.cancelar", { nome: modelo.nome })}
        />
      ) : (
        <div className="model-actions">
          {!modelo.baixado && (
            <button type="button" className="primary" onClick={onBaixar}>
              <Icon name="download" />
              {t("voz.baixar")}
            </button>
          )}
          {modelo.baixado && (
            <button
              type="button"
              className="danger icon-only"
              onClick={onApagar}
              aria-label={t("voz.excluir", { nome: modelo.nome })}
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
