import { useCallback, useEffect, useState } from "react";
import { api, message, onBancadaEvent } from "../lib/api";
import { useT } from "../lib/i18n";
import type { BancadaEvento, BancadaResultado } from "../lib/types";
import { Icon } from "./Icon";

interface Props {
  onClose: () => void;
}

/// Cada linha é uma conta medida nesta máquina, contra o modelo que está em
/// memória. Não há caso sem número: o veredito é o que a rodada devolveu.
export function BancadaDialog({ onClose }: Props) {
  const t = useT();
  const [casos, setCasos] = useState<string[]>([]);
  const [resultados, setResultados] = useState<Record<string, BancadaResultado>>(
    {},
  );
  const [andando, setAndando] = useState(false);
  const [atual, setAtual] = useState<string | null>(null);
  const [modelo, setModelo] = useState<string | null>(null);
  const [erro, setErro] = useState<string | null>(null);

  useEffect(() => {
    void api
      .bancadaCasos()
      .then(setCasos)
      .catch((cause) => setErro(message(cause)));
  }, []);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    const handle = (event: BancadaEvento) => {
      switch (event.type) {
        case "iniciou":
          setResultados({});
          setAndando(true);
          setModelo(event.modelo);
          setErro(null);
          break;
        case "caso":
          setAtual(event.id);
          break;
        case "resultado":
          setResultados((current) => ({ ...current, [event.id]: event }));
          setAtual(null);
          break;
      }
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

  const rodar = useCallback(async () => {
    setErro(null);
    try {
      await api.bancadaRodar();
    } catch (cause) {
      setErro(message(cause));
    } finally {
      setAndando(false);
      setAtual(null);
    }
  }, []);

  const parar = useCallback(async () => {
    try {
      await api.bancadaParar();
    } catch (cause) {
      setErro(message(cause));
    }
  }, []);

  const feitos = Object.values(resultados);
  const reprovados = feitos.filter((r) => r.estado === "reprovado").length;

  return (
    <div className="modal" role="dialog" aria-modal="true" aria-label={t("bancada.titulo")}>
      <div className="modal-card bancada-card">
        <header className="sheet-head">
          <span className="sheet-title">{t("bancada.titulo")}</span>
          <button
            type="button"
            className="sheet-close"
            onClick={onClose}
            aria-label={t("geral.fechar")}
          >
            <Icon name="cross" />
          </button>
        </header>

        <div className="bancada-acoes">
          <button
            type="button"
            className="primary"
            disabled={andando}
            onClick={() => void rodar()}
          >
            <Icon name="play" />
            {t("bancada.rodar")}
          </button>
          {andando && (
            <button type="button" className="danger" onClick={() => void parar()}>
              <Icon name="stop" />
              {t("bancada.cancelar")}
            </button>
          )}
          {andando && <span className="bancada-andando">{t("bancada.executando")}</span>}
          {!andando && reprovados > 0 && (
            <span className="bancada-contagem aviso">{reprovados}</span>
          )}
        </div>

        {modelo !== null && <p className="bancada-modelo">{modelo}</p>}

        <ul className="bancada-lista">
          {casos.map((id) => {
            const r = resultados[id];
            const estado = r?.estado ?? "esperando";
            return (
              <li key={id} className={`bancada-caso ${estado}`}>
                <span className="bancada-cabeca">
                  <Icon
                    name={
                      estado === "aprovado"
                        ? "check"
                        : estado === "reprovado"
                          ? "warning"
                          : estado === "naoRodado"
                            ? "clock"
                            : "spark"
                    }
                  />
                  <span className="bancada-nome">{t(`bancada.caso.${id}`)}</span>
                  {r !== undefined && (
                    <span className="bancada-ms">{r.ms} ms</span>
                  )}
                  {andando && atual === id && (
                    <span className="bancada-andando">{t("bancada.executando")}</span>
                  )}
                </span>

                {r !== undefined && r.medidas.length > 0 && (
                  <span className="bancada-chips">
                    {r.medidas.map((m) => (
                      <span className="memoria-chip" key={m.rotulo}>
                        {t(m.rotulo)}
                        {": "}
                        {m.valor}
                      </span>
                    ))}
                  </span>
                )}

                {r?.detalhe && (
                  <p className="bancada-motivo" role={r.estado === "reprovado" ? "alert" : undefined}>
                    {r.detalhe}
                  </p>
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
