import { useState } from "react";
import { api, message } from "../lib/api";
import { useT } from "../lib/i18n";
import type { GrantedFolder } from "../lib/types";
import { Icon } from "./Icon";

interface Props {
  pastas: GrantedFolder[];
  onChange: (pastas: GrantedFolder[]) => void;
  onClose: () => void;
}

/// A única porta de entrada do Aether aos arquivos: enquanto o usuário não
/// conceder uma pasta, nenhuma ferramenta de arquivo enxerga o disco.
export function PastasDialog({ pastas, onChange, onClose }: Props) {
  const t = useT();
  const [caminho, setCaminho] = useState("");
  const [erro, setErro] = useState<string | null>(null);
  const [ocupada, setOcupada] = useState(false);

  async function conceder(event: React.FormEvent) {
    event.preventDefault();
    if (ocupada) return;
    setErro(null);
    setOcupada(true);
    try {
      onChange(await api.grantFolder(caminho));
      setCaminho("");
    } catch (cause) {
      setErro(message(cause));
    } finally {
      setOcupada(false);
    }
  }

  async function revogar(pasta: GrantedFolder) {
    setErro(null);
    try {
      onChange(await api.revokeFolder(pasta.caminho));
    } catch (cause) {
      setErro(message(cause));
    }
  }

  return (
    <div className="modal" role="dialog" aria-modal="true" aria-label={t("pastas.titulo")}>
      <div className="modal-card">
        <header className="sheet-head">
          <span className="sheet-title">{t("pastas.titulo")}</span>
          <button
            type="button"
            className="sheet-close"
            onClick={onClose}
            aria-label={t("geral.fechar")}
          >
            <Icon name="cross" />
          </button>
        </header>

        <p className="sheet-note">{t("pastas.descricao")}</p>

        {pastas.length === 0 ? (
          <p className="sheet-note">{t("pastas.nenhuma")}</p>
        ) : (
          <ul className="folder-rows">
            {pastas.map((pasta) => (
              <li key={pasta.caminho} className="folder-row">
                <span className="folder-icon">
                  <Icon name="folder" />
                </span>
                <span className="folder-name">{pasta.nome}</span>
                <span className="folder-path">{pasta.caminho}</span>
                <button
                  type="button"
                  className="danger icon-only"
                  onClick={() => void revogar(pasta)}
                  aria-label={t("pastas.revogar", { nome: pasta.nome })}
                >
                  <Icon name="cross" />
                </button>
              </li>
            ))}
          </ul>
        )}

        <form className="folder-form" onSubmit={(event) => void conceder(event)}>
          <label className="field">
            <span className="field-label">{t("pastas.caminho")}</span>
            <input
              value={caminho}
              placeholder={t("pastas.placeholder")}
              autoComplete="off"
              spellCheck={false}
              onChange={(event) => setCaminho(event.target.value)}
            />
          </label>
          <button type="submit" className="primary" disabled={ocupada}>
            <Icon name="plus" />
            {t("pastas.conceder")}
          </button>
        </form>

        {erro !== null && (
          <p className="sheet-error" role="alert">
            <Icon name="warning" />
            {erro}
          </p>
        )}
        <p className="sheet-note">{t("pastas.aviso")}</p>
      </div>
    </div>
  );
}
