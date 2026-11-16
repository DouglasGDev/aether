import { useState } from "react";
import { useT } from "../lib/i18n";
import type { AgentMode } from "../lib/types";
import { Icon } from "./Icon";

const MODES: { value: AgentMode; nome: string; descricao: string; icon: string }[] =
  [
    { value: "plan", nome: "confianca.observar", descricao: "confianca.observarDescricao", icon: "eye" },
    { value: "default", nome: "confianca.perguntar", descricao: "confianca.perguntarDescricao", icon: "shield" },
    { value: "auto-accept", nome: "confianca.aceitar", descricao: "confianca.aceitarDescricao", icon: "check" },
    { value: "auto", nome: "confianca.confiar", descricao: "confianca.confiarDescricao", icon: "bolt" },
    { value: "full-access", nome: "confianca.total", descricao: "confianca.totalDescricao", icon: "spark" },
  ];

interface Props {
  mode: AgentMode;
  /// Servidores em que o usuário confiou para esta conversa, já com o nome
  /// legível: a confiança dada precisa poder ser retirada sem novo pedido.
  granted: { id: string; nome: string }[];
  onChange: (mode: AgentMode) => void;
  onRevoke: (id: string) => void;
}

export function TrustDial({ mode, granted, onChange, onRevoke }: Props) {
  const t = useT();
  const [open, setOpen] = useState(false);
  const current = MODES.find((entry) => entry.value === mode) ?? MODES[1];
  return (
    <div className="trust">
      <button
        type="button"
        className={`trust-current trust-${mode}`}
        aria-expanded={open}
        aria-label={t("confianca.titulo")}
        title={t(current.descricao)}
        onClick={() => setOpen((value) => !value)}
      >
        <Icon name={current.icon} />
        {t(current.nome)}
        {granted.length > 0 && (
          <span className="trust-badge" title={t("confianca.nestaSessao")}>
            {granted.length}
          </span>
        )}
      </button>
      {open && (
        <div className="trust-menu" role="menu">
          {MODES.map((entry) => (
            <button
              key={entry.value}
              type="button"
              role="menuitemradio"
              aria-checked={entry.value === mode}
              className={entry.value === mode ? "trust-option active" : "trust-option"}
              title={t(entry.descricao)}
              onClick={() => {
                onChange(entry.value);
                setOpen(false);
              }}
            >
              <Icon name={entry.icon} />
              {t(entry.nome)}
            </button>
          ))}
          {granted.length > 0 && (
            <>
              <p className="trust-sep">{t("confianca.nestaSessao")}</p>
              {granted.map((entrada) => (
                <button
                  key={entrada.id}
                  type="button"
                  className="trust-grant"
                  onClick={() => {
                    onRevoke(entrada.id);
                    setOpen(false);
                  }}
                >
                  <span className="trust-grant-head">
                    <Icon name="lock" />
                    <span className="trust-grant-name">{entrada.nome}</span>
                  </span>
                  <span className="trust-grant-action">
                    {t("aprovacao.revogarSessao")}
                  </span>
                </button>
              ))}
            </>
          )}
        </div>
      )}
    </div>
  );
}
