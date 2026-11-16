import { EFFECT_KEY, humanize, toolTitle } from "../lib/display";
import { useT } from "../lib/i18n";
import type { PendingApproval, ToolInfo } from "../lib/types";
import { Icon } from "./Icon";

interface Props {
  approval: PendingApproval;
  /// A entrada do catálogo, quando a ferramenta ainda está conectada.
  tool?: ToolInfo;
  /// Nome legível do servidor que pediu a ação.
  source?: string;
  args: Record<string, unknown> | null;
  granted: boolean;
  onResolve: (id: string, approved: boolean) => void;
  onTrust: () => void;
  onRevoke: () => void;
}

export function ApprovalCard({
  approval,
  tool,
  source,
  args,
  granted,
  onResolve,
  onTrust,
  onRevoke,
}: Props) {
  const t = useT();
  const entries = args === null ? [] : Object.entries(args);
  return (
    <div className={`approval-card effect-${approval.effect}`}>
      <span className="approval-shield">
        <Icon name="shield" />
      </span>
      <span className="approval-ask">{t("aprovacao.titulo")}</span>
      <span className="approval-title">
        {tool !== undefined ? toolTitle(tool) : humanize(approval.tool)}
      </span>
      <span className="approval-source">
        {t("aprovacao.origem")} · {source ?? humanize(approval.connectorId)}
      </span>
      <span className="approval-risk">
        {t(EFFECT_KEY[approval.effect])}
        {approval.compound && ` · ${t("aprovacao.comandoComposto")}`}
      </span>
      {entries.length > 0 && (
        <dl className="approval-args">
          <div className="approval-data">{t("aprovacao.dados")}</div>
          {entries.map(([key, value]) => (
            <div key={key} className="approval-arg">
              <dt>{humanize(key)}</dt>
              <dd>
                {typeof value === "string" ? value : JSON.stringify(value)}
              </dd>
            </div>
          ))}
        </dl>
      )}
      <div className="approval-actions">
        <button
          type="button"
          className="danger"
          onClick={() => onResolve(approval.id, false)}
        >
          <Icon name="cross" />
          {t("aprovacao.negar")}
        </button>
        {granted ? (
          <button type="button" onClick={onRevoke}>
            <Icon name="lock" />
            {t("aprovacao.revogarSessao")}
          </button>
        ) : (
          <button type="button" onClick={onTrust}>
            <Icon name="lock" />
            {t("aprovacao.concederSessao")}
          </button>
        )}
        <button
          type="button"
          className="primary"
          onClick={() => onResolve(approval.id, true)}
        >
          <Icon name="check" />
          {t("aprovacao.aprovar")}
        </button>
      </div>
    </div>
  );
}
