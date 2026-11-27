import { memo, useEffect, useRef } from "react";
import {
  STOP_KEY,
  duracao,
  humanize,
  inteiro,
  numero,
  toolTitle,
} from "../lib/display";
import { useIdioma, useT } from "../lib/i18n";
import type {
  BancadaRodada,
  ChatMessage,
  Choice,
  Medicao,
  ToolInfo,
  Vista,
} from "../lib/types";
import { Icon } from "./Icon";
import { RichText } from "./RichText";
import { ToolCard } from "./ToolCard";

export interface ToolRun {
  /// Nome qualificado tal como o modelo pediu.
  name: string;
  ok?: boolean;
  text?: string;
  vista?: Vista | null;
  /// A entrada do catálogo, quando existe: dá o título no idioma atual.
  info?: ToolInfo;
  /// Só o cancelamento explícito do turno marca isto. Uma ferramenta que
  /// terminou sem resultado continua sendo uma falha, não um cancelamento.
  cancelado?: boolean;
}

export interface Turn {
  role: "user" | "assistant";
  /// Identificador do desenho, não do conteúdo. O índice do array não serve de
  /// chave: os turnos crescem durante o streaming, somem quando a conversa é
  /// trocada e são recortados quando a compactação entra — e com chave de índice
  /// o React empresta o `<details>` já aberto de um turno para o outro.
  chave: string;
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
  /// O resumo que este turno escreveu no lugar das mensagens que saíram do
  /// prompt. O cartão é a prova de que ele leu o começo da conversa condensado
  /// — e do que pôs no lugar. `descartadas` são as que saíram sem ser lidas:
  /// couberam no corte, não coube na régua do resumo.
  resumo: { texto: string; mensagens: number; descartadas: number } | null;
}

/// A fileira dos turnos desenhados. Só precisa não repetir: cada um nasce com
/// a sua e a leva enquanto existir.
let turnosDesenhados = 0;

export function emptyTurn(role: Turn["role"]): Turn {
  turnosDesenhados += 1;
  return {
    role,
    chave: `t${turnosDesenhados}`,
    text: "",
    thinking: "",
    tools: [],
    choice: null,
    loading: null,
    streaming: false,
    medicao: null,
    memoria: null,
    resumo: null,
  };
}

/// O histórico do núcleo como a conversa desenha. Só o que a pessoa pediu e o
/// que o modelo respondeu: o prompt de sistema não é turno, e o resultado de
/// uma ferramenta passada não tem veredito gravado — dizer que ela está
/// "executando" agora seria inventar um estado que não existe.
export function turnsDe(history: ChatMessage[]): Turn[] {
  return history
    .filter(
      (entry) =>
        (entry.role === "user" || entry.role === "assistant") &&
        entry.content.trim() !== "",
    )
    .map((entry) => ({
      ...emptyTurn(entry.role === "user" ? "user" : "assistant"),
      text: entry.content,
    }));
}

interface Props {
  turns: Turn[];
  /// Há um pedido parado esperando o usuário decidir.
  aguardando: boolean;
  /// A última rodada da bancada, ou `null` quando ela nunca rodou nesta janela.
  /// A conta mora no aplicativo: a linha é a mesma medição que a folha mostra,
  /// não uma cópia que aqui se atualizasse por conta própria.
  rodada: BancadaRodada | null;
  totalDeCasos: number;
  onAbrirRodada: () => void;
  onDispensarRodada: () => void;
}

/// A rodada inteira em uma linha: o que já se sabe da bancada sem abrir a
/// folha. Cada número vem de um evento que o núcleo mandou — `iniciou` dá o
/// modelo, `caso` o que está em curso, `resultado` o veredito e os milissegundos.
function LinhaDeRodada({
  rodada,
  total,
  onAbrir,
  onDispensar,
}: {
  rodada: BancadaRodada;
  total: number;
  onAbrir: () => void;
  onDispensar: () => void;
}) {
  const t = useT();
  const { id: idioma } = useIdioma();
  const feitos = Object.values(rodada.resultados);
  const aprovados = feitos.filter((r) => r.estado === "aprovado").length;
  const reprovados = feitos.filter((r) => r.estado === "reprovado").length;
  const naoRodados = feitos.filter((r) => r.estado === "naoRodado").length;
  const somados = feitos.reduce((soma, r) => soma + r.ms, 0);
  const convite = `${t("bancada.abrirDetalhe")}${
    rodada.modelo === null ? "" : ` · ${rodada.modelo}`
  }`;

  return (
    <div className="talk-rodada">
      <button
        type="button"
        className="talk-rodada-linha"
        onClick={onAbrir}
        title={convite}
      >
        <Icon name="bancada" />
        <span className="talk-rodada-nome">{t("bancada.titulo")}</span>
        <span className="memoria-chip">
          {rodada.andando ? `${t("bancada.executando")} ` : ""}
          {inteiro(feitos.length, idioma)}
          {total > 0 ? `/${inteiro(total, idioma)}` : ""}
        </span>
        {rodada.atual !== null && (
          <span className="memoria-chip">{t(`bancada.caso.${rodada.atual}`)}</span>
        )}
        {aprovados > 0 && (
          <span className="memoria-chip">
            <Icon name="check" />
            {t("bancada.aprovados", { n: inteiro(aprovados, idioma) })}
          </span>
        )}
        {reprovados > 0 && (
          <span className="memoria-chip aviso">
            <Icon name="warning" />
            {t("bancada.reprovados", { n: inteiro(reprovados, idioma) })}
          </span>
        )}
        {naoRodados > 0 && (
          <span className="memoria-chip aviso">
            <Icon name="clock" />
            {t("bancada.naoRodados", { n: inteiro(naoRodados, idioma) })}
          </span>
        )}
        {feitos.length > 0 && (
          <span className="memoria-chip">
            <Icon name="clock" />
            {`${t("cartao.duracao")} ${duracao(somados, idioma)}`}
          </span>
        )}
      </button>
      <button
        type="button"
        className="icon-only talk-rodada-fechar"
        onClick={onDispensar}
        aria-label={t("geral.dispensar")}
        title={t("geral.dispensar")}
      >
        <Icon name="cross" />
      </button>
    </div>
  );
}

export const Conversation = memo(function Conversation({
  turns,
  aguardando,
  rodada,
  totalDeCasos,
  onAbrirRodada,
  onDispensarRodada,
}: Props) {
  const end = useRef<HTMLDivElement>(null);
  const t = useT();
  const { id: idioma } = useIdioma();
  /// `null` é a montagem: uma conversa aberta cai no fim sem passar pela régua,
  /// porque ali ainda não existe leitura em curso para respeitar.
  const quantidade = useRef<number | null>(null);

  /// O turno novo chega em rajada de token, e o efeito abaixo rodava uma vez por
  /// delta: cada um forçava layout e arrancava de volta ao fim quem tinha subido
  /// para reler. A régua é a posição do rodapé — perto dele se persegue, longe
  /// não; e uma virada de turno é perseguida sozinha, porque a mensagem recém-
  /// escrita deve ir para a frente dos olhos mesmo com a tela rolada para cima.
  /// O `requestAnimationFrame` junta os deltas que caem no mesmo quadro.
  useEffect(() => {
    const alvo = end.current;
    if (alvo === null) return;
    const novaRajada = quantidade.current === null || turns.length !== quantidade.current;
    quantidade.current = turns.length;
    const abaixo = alvo.getBoundingClientRect().bottom - window.innerHeight;
    if (!novaRajada && abaixo > 160) return;
    const quadro = requestAnimationFrame(() => alvo.scrollIntoView({ block: "end" }));
    return () => cancelAnimationFrame(quadro);
  }, [turns]);

  return (
    <div className="talk">
      {turns.map((turn) =>
        turn.role === "user" ? (
          <div className="talk-user" key={turn.chave}>
            {turn.text}
          </div>
        ) : (
          <div className="talk-agent" key={turn.chave}>
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

            {turn.resumo !== null && (
              <details className="talk-resumo">
                <summary>
                  <Icon name="camadas" />
                  {t("conversa.resumoTitulo")}
                  <span className="contagem">
                    {t("conversa.resumoMensagens", {
                      n: inteiro(turn.resumo.mensagens, idioma),
                    })}
                  </span>
                  {turn.resumo.descartadas > 0 && (
                    <span className="contagem aviso">
                      {t("conversa.resumoDescartadas", {
                        n: inteiro(turn.resumo.descartadas, idioma),
                      })}
                    </span>
                  )}
                </summary>
                <p>{turn.resumo.texto}</p>
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
                {turn.tools.map((tool, at) => {
                  const estado =
                    tool.ok === undefined
                      ? turn.streaming
                        ? "running"
                        : "fail"
                      : tool.ok
                        ? "ok"
                        : "fail";
                  const cartao =
                    tool.vista !== null && tool.vista !== undefined ? (
                      <ToolCard vista={tool.vista} falha={tool.ok === false} />
                    ) : null;
                  /// Sem vista, o que a ferramenta devolveu é texto cru. Ele não
                  /// é resposta do modelo e não merece o corpo da conversa: entra
                  /// medido, numa linha, e o corpo só aparece para quem pedir.
                  const bruto = cartao === null ? (tool.text ?? "") : "";
                  const cabeca = (
                    <>
                      <Icon name="bolt" />
                      <span className="talk-tool-name">
                        {tool.info !== undefined
                          ? toolTitle(tool.info)
                          : humanize(tool.name)}
                      </span>
                      <span className="talk-tool-state">
                        {tool.ok === undefined ? (
                          turn.streaming
                            ? aguardando
                              ? t("conversa.aguardando")
                              : t("geral.executando")
                            : t("geral.erro")
                        ) : tool.ok ? (
                          <Icon name="check" />
                        ) : tool.cancelado ? (
                          t("erros.pedidoCancelado")
                        ) : (
                          <Icon name="cross" />
                        )}
                      </span>
                    </>
                  );
                  return bruto === "" ? (
                    <div className={`talk-tool ${estado}`} key={at}>
                      <span className="talk-tool-head">{cabeca}</span>
                      {cartao}
                    </div>
                  ) : (
                    <details className={`talk-tool ${estado}`} key={at}>
                      <summary className="talk-tool-head">
                        {cabeca}
                        <span className="talk-tool-medida">
                          {t("conversa.caracteres", {
                            n: inteiro(bruto.length, idioma),
                          })}
                        </span>
                        <Icon name="chevron" className="talk-tool-seta" />
                      </summary>
                      <pre className="talk-tool-out">{bruto}</pre>
                    </details>
                  );
                })}
              </div>
            )}

            {turn.text !== "" && (
              <div className="talk-text">
                <RichText text={turn.text} />
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
                {turn.medicao.reusedTokens > 0 && (
                  <span>
                    {t("conversa.reaproveitados", {
                      n: inteiro(turn.medicao.reusedTokens, idioma),
                    })}
                  </span>
                )}
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
                {turn.medicao.dobradas > 0 && (
                  <span className="talk-medida aviso">
                    {t("conversa.dobradas", {
                      n: inteiro(turn.medicao.dobradas, idioma),
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
      {rodada !== null && (
        <LinhaDeRodada
          rodada={rodada}
          total={totalDeCasos}
          onAbrir={onAbrirRodada}
          onDispensar={onDispensarRodada}
        />
      )}
      <div ref={end} />
    </div>
  );
});
