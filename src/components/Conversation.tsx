import { useEffect, useRef } from "react";
import {
  STOP_KEY,
  duracao,
  humanize,
  inteiro,
  numero,
  toolTitle,
} from "../lib/display";
import { useIdioma, useT } from "../lib/i18n";
import type { Choice, Medicao, ToolInfo, Vista } from "../lib/types";
import { Icon } from "./Icon";
import { ToolCard } from "./ToolCard";

export interface ToolRun {
  /// Nome qualificado tal como o modelo pediu.
  name: string;
  ok?: boolean;
  text?: string;
  vista?: Vista | null;
  /// A entrada do catálogo, quando existe: dá o título no idioma atual.
  info?: ToolInfo;
}

export interface Turn {
  role: "user" | "assistant";
  text: string;
  thinking: string;
  tools: ToolRun[];
  choice: Choice | null;
  loading: string | null;
  streaming: boolean;
  /// O que o turno mediu; ausente enquanto o motor não devolveu a conta.
  medicao: Medicao | null;
  /// O bloco que a memória pôs no prompt antes do primeiro token, quando pôs.
  /// É o mesmo cartão da ferramenta: muda a origem, não o dado.
  memoria: Extract<Vista, { type: "memoria" }> | null;
}

export function emptyTurn(role: Turn["role"]): Turn {
  return {
    role,
    text: "",
    thinking: "",
    tools: [],
    choice: null,
    loading: null,
    streaming: false,
    medicao: null,
    memoria: null,
  };
}

interface Props {
  turns: Turn[];
  /// Há um pedido parado esperando o usuário decidir.
  aguardando: boolean;
}

export function Conversation({ turns, aguardando }: Props) {
  const end = useRef<HTMLDivElement>(null);
  const t = useT();
  const { id: idioma } = useIdioma();
  useEffect(() => {
    end.current?.scrollIntoView({ block: "end" });
  }, [turns]);

  return (
    <div className="talk">
      {turns.map((turn, index) =>
        turn.role === "user" ? (
          <div className="talk-user" key={index}>
            {turn.text}
          </div>
        ) : (
          <div className="talk-agent" key={index}>
            {turn.choice !== null && (
              <div className="talk-model">
                <Icon name="spark" />
                <span className="talk-model-name">{turn.choice.display}</span>
                <span className="talk-model-why">
                  {t("conversa.motivo")} · {turn.choice.reason}
                </span>
                {turn.choice.signals.length > 0 && (
                  <ul className="talk-signals">
                    {turn.choice.signals.map((sinal, at) => (
                      <li key={at}>{sinal}</li>
                    ))}
                  </ul>
                )}
              </div>
            )}

            {turn.loading !== null && (
              <div className="talk-loading">
                <Icon name="plug" />
                {turn.loading}
              </div>
            )}

            {turn.thinking !== "" && (
              <details className="talk-think">
                <summary>
                  <Icon name="eye" />
                  {t("conversa.raciocinio")}
                </summary>
                <p>{turn.thinking}</p>
              </details>
            )}

            {turn.memoria !== null && (
              <div className="talk-tools">
                <span className="talk-tools-label">{t("conversa.memoria")}</span>
                <div className="talk-tool ok">
                  <ToolCard vista={turn.memoria} falha={false} />
                </div>
              </div>
            )}

            {turn.tools.length > 0 && (
              <div className="talk-tools">
                <span className="talk-tools-label">{t("conversa.ferramentas")}</span>
                {turn.tools.map((tool, at) => (
                  <div
                    className={`talk-tool ${
                      tool.ok === undefined ? "running" : tool.ok ? "ok" : "fail"
                    }`}
                    key={at}
                  >
                    <span className="talk-tool-head">
                      <Icon name="bolt" />
                      <span className="talk-tool-name">
                        {tool.info !== undefined
                          ? toolTitle(tool.info)
                          : humanize(tool.name)}
                      </span>
                      <span className="talk-tool-state">
                        {tool.ok === undefined ? (
                          aguardando ? t("conversa.aguardando") : t("geral.executando")
                        ) : tool.ok ? (
                          <Icon name="check" />
                        ) : (
                          <Icon name="cross" />
                        )}
                      </span>
                    </span>
                    {tool.vista !== null && tool.vista !== undefined ? (
                      <ToolCard vista={tool.vista} falha={tool.ok === false} />
                    ) : (
                      tool.text !== undefined &&
                      tool.text !== "" && (
                        <pre className="talk-tool-out">{tool.text}</pre>
                      )
                    )}
                  </div>
                ))}
              </div>
            )}

            {turn.text !== "" && (
              <div className="talk-text">
                {turn.text}
                {turn.streaming && <span className="talk-caret" />}
              </div>
            )}

            {turn.medicao !== null && !turn.streaming && (
              <div className="talk-medidas">
                <span>
                  {t("conversa.tokens", {
                    saida: inteiro(turn.medicao.generatedTokens, idioma),
                    entrada: inteiro(turn.medicao.promptTokens, idioma),
                  })}
                </span>
                <span>
                  {t("conversa.ritmo", {
                    n: numero(turn.medicao.tokensPorSegundo, idioma, 1),
                  })}
                </span>
                <span>
                  {t("conversa.prefill", {
                    n: numero(turn.medicao.prefillTokensPorSegundo, idioma, 0),
                  })}
                </span>
                <span>
                  {t("conversa.duracao", {
                    tempo: duracao(
                      turn.medicao.promptMs + turn.medicao.decodeMs,
                      idioma,
                    ),
                  })}
                </span>
                {turn.medicao.descartadas > 0 && (
                  <span className="talk-medida aviso">
                    {t("conversa.foraDeContexto", {
                      n: inteiro(turn.medicao.descartadas, idioma),
                    })}
                  </span>
                )}
                <span
                  className={
                    turn.medicao.stop === "maxTokens" ||
                    turn.medicao.stop === "contextFull"
                      ? "talk-medida aviso"
                      : "talk-medida"
                  }
                >
                  {t(STOP_KEY[turn.medicao.stop])}
                </span>
              </div>
            )}

            {turn.streaming && turn.text === "" && turn.tools.length === 0 && (
              <div className="talk-wait">
                <span />
                <span />
                <span />
              </div>
            )}
          </div>
        ),
      )}
      <div ref={end} />
    </div>
  );
}
