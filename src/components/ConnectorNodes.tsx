import { memo } from "react";
import { useT } from "../lib/i18n";
import type { Connector } from "../lib/types";
import { Icon } from "./Icon";

interface Props {
  connectors: Connector[];
  connected: string[];
  busy: string | null;
  onToggle: (id: string) => void;
  onAdd: () => void;
}

export const ConnectorNodes = memo(function ConnectorNodes({
  connectors,
  connected,
  busy,
  onToggle,
  onAdd,
}: Props) {
  const t = useT();
  return (
    <div className="nodes">
      {connectors.map((connector) => {
        const online = connected.includes(connector.id);
        return (
          <button
            key={connector.id}
            type="button"
            className={online ? "node online" : "node"}
            disabled={busy === connector.id}
            onClick={() => onToggle(connector.id)}
            aria-label={online ? t("servidores.desconectar") : t("servidores.conectar")}
            title={connector.name}
          >
            <span className="node-orb">
              <Icon name="plug" />
              <span className="node-dot" />
            </span>
            <span className="node-name">{connector.name}</span>
          </button>
        );
      })}
      <button
        type="button"
        className="node add"
        onClick={onAdd}
        title={t("servidores.adicionar")}
      >
        <span className="node-orb">
          <Icon name="plus" />
        </span>
        <span className="node-name">{t("geral.adicionar")}</span>
      </button>
    </div>
  );
});
