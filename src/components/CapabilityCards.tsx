import { useState } from "react";
import { EFFECT_ICON, EFFECT_KEY, toolTitle } from "../lib/display";
import { useT } from "../lib/i18n";
import type { Connector, ToolInfo } from "../lib/types";
import { Icon } from "./Icon";

interface Props {
  tools: ToolInfo[];
  /// Quem anunciou cada ferramenta. A lista é agrupada por servidor porque uma
  /// parede de cartões iguais não diz a ninguém de onde vem a capacidade — e é
  /// o servidor de terceiros que fala inglês, não o computador da casa.
  connectors: Connector[];
  onPick: (tool: ToolInfo) => void;
}

interface Grupo {
  id: string;
  nome: string;
  fichas: ToolInfo[];
}

/// A ordem é a dos servidores cadastrados, com o computador da casa no lugar que
/// ele ocupa no resto da tela. Uma ferramenta cujo servidor sumiu da lista
/// aparece com o identificador cru: escondê-la seria pior que um nome feio.
function grupos(tools: ToolInfo[], connectors: Connector[]): Grupo[] {
  const porServidor = new Map<string, ToolInfo[]>();
  for (const tool of tools) {
    const lista = porServidor.get(tool.connectorId);
    if (lista === undefined) porServidor.set(tool.connectorId, [tool]);
    else lista.push(tool);
  }
  const saida: Grupo[] = [];
  for (const connector of connectors) {
    const fichas = porServidor.get(connector.id);
    if (fichas === undefined) continue;
    porServidor.delete(connector.id);
    saida.push({ id: connector.id, nome: connector.name, fichas });
  }
  for (const [id, fichas] of porServidor) saida.push({ id, nome: id, fichas });
  return saida;
}

export function CapabilityCards({ tools, connectors, onPick }: Props) {
  const t = useT();
  const [recolhidas, setRecolhidas] = useState<string[]>([]);
  const alternar = (id: string) =>
    setRecolhidas((atual) =>
      atual.includes(id) ? atual.filter((outro) => outro !== id) : [...atual, id],
    );

  return (
    <div className="capabilities">
      {grupos(tools, connectors).map((grupo) => {
        const recolhida = recolhidas.includes(grupo.id);
        return (
          <section className="cap-grupo" key={grupo.id}>
            <button
              type="button"
              className="cap-grupo-cabeca"
              onClick={() => alternar(grupo.id)}
              aria-expanded={!recolhida}
            >
              <Icon name="chevron" className={recolhida ? "cap-gira" : undefined} />
              <span className="cap-grupo-nome">{grupo.nome}</span>
              <span className="cap-grupo-total">
                {t("servidores.ferramentas", { n: grupo.fichas.length })}
              </span>
            </button>
            {!recolhida && (
              <div className="cap-linha">
                {grupo.fichas.map((tool) => (
                  <button
                    key={tool.qualifiedName}
                    type="button"
                    className={`capability effect-${tool.effect}`}
                    title={tool.description === "" ? toolTitle(tool) : tool.description}
                    onClick={() => onPick(tool)}
                  >
                    <span className="capability-icon">
                      <Icon name={EFFECT_ICON[tool.effect]} />
                    </span>
                    <span className="capability-name">{toolTitle(tool)}</span>
                    <span className="capability-risk">
                      {t(EFFECT_KEY[tool.effect])}
                    </span>
                  </button>
                ))}
              </div>
            )}
          </section>
        );
      })}
    </div>
  );
}
