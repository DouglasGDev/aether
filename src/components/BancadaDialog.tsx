import { useT } from "../lib/i18n";
import type { BancadaRodada } from "../lib/types";
import { useFecharComEscape } from "../lib/teclado";
import { Icon } from "./Icon";

interface Props {
  casos: string[];
  rodada: BancadaRodada;
  erro: string | null;
  onRodar: () => void;
  onParar: () => void;
  onClose: () => void;
}

/// Cada linha é uma conta medida nesta máquina, contra o modelo que está em
/// memória. Não há caso sem número: o veredito é o que a rodada devolveu.
/// A folha só desenha: quem segura a medição é o aplicativo, porque o núcleo
/// continua mandando resultado mesmo quando ninguém está olhando.
export function BancadaDialog({ casos, rodada, erro, onRodar, onParar, onClose }: Props) {
  useFecharComEscape(onClose);
  const t = useT();
  const { resultados, andando, atual, modelo } = rodada;

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
            onClick={onRodar}
          >
            <Icon name="play" />
            {t("bancada.rodar")}
          </button>
          {andando && (
            <button type="button" className="danger" onClick={onParar}>
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
