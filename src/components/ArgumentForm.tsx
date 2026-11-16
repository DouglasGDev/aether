import { useState } from "react";
import { humanize, toolTitle } from "../lib/display";
import { useT } from "../lib/i18n";
import type { ToolInfo } from "../lib/types";
import { Icon } from "./Icon";

interface Property {
  title?: string;
  type?: string;
  description?: string;
  enum?: unknown[];
  default?: unknown;
  items?: { type?: string };
}

interface Schema {
  properties?: Record<string, Property>;
  required?: string[];
}

interface Props {
  tool: ToolInfo;
  busy: boolean;
  onCancel: () => void;
  onRun: (args: Record<string, unknown>) => void;
}

export function ArgumentForm({ tool, busy, onCancel, onRun }: Props) {
  const t = useT();
  const schema = (tool.inputSchema ?? {}) as Schema;
  const properties = schema.properties ?? {};
  const required = schema.required ?? [];
  const keys = Object.keys(properties);

  const [values, setValues] = useState<Record<string, string>>(() => {
    const initial: Record<string, string> = {};
    for (const key of keys) {
      const value = properties[key].default;
      initial[key] = value === undefined || value === null ? "" : String(value);
    }
    return initial;
  });
  const [missing, setMissing] = useState<string[]>([]);

  function set(key: string, value: string) {
    setValues((current) => ({ ...current, [key]: value }));
    setMissing((current) => current.filter((entry) => entry !== key));
  }

  function submit(event: React.FormEvent) {
    event.preventDefault();
    const absent = required.filter((key) => (values[key] ?? "").trim() === "");
    if (absent.length > 0) {
      setMissing(absent);
      return;
    }
    const args: Record<string, unknown> = {};
    for (const key of keys) {
      const raw = (values[key] ?? "").trim();
      const property = properties[key];
      if (raw === "" && !required.includes(key)) continue;
      if (property.enum) {
        args[key] = raw;
      } else if (property.type === "boolean") {
        args[key] = raw === "true";
      } else if (property.type === "number" || property.type === "integer") {
        args[key] = Number(raw);
      } else if (property.type === "array") {
        const parts = raw.split(",").map((part) => part.trim()).filter(Boolean);
        args[key] =
          property.items?.type === "number" || property.items?.type === "integer"
            ? parts.map(Number)
            : parts;
      } else if (property.type === "object") {
        args[key] = JSON.parse(raw);
      } else {
        args[key] = raw;
      }
    }
    onRun(args);
  }

  return (
    <form className="sheet" onSubmit={submit}>
      <header className="sheet-head">
        <span className={`sheet-icon effect-${tool.effect}`}>
          <Icon name="bolt" />
        </span>
        <span className="sheet-title">{toolTitle(tool)}</span>
        <button
          type="button"
          className="sheet-close"
          onClick={onCancel}
          aria-label={t("geral.fechar")}
        >
          <Icon name="cross" />
        </button>
      </header>

      {/* A descrição é a palavra do servidor sobre a ferramenta, não a nossa:
         vive aqui, inteira, na tela de quem está prestes a rodá-la — não em
         pedaços de duas linhas na parede de botões. */}
      {tool.description !== "" && (
        <p className="sheet-note">{tool.description}</p>
      )}

      {keys.length === 0 ? (
        <p className="sheet-note">{t("geral.nenhumDado")}</p>
      ) : (
        <div className="sheet-fields">
          {keys.map((key) => {
            const property = properties[key];
            const isMissing = missing.includes(key);
            return (
              <label
                key={key}
                className={isMissing ? "field missing" : "field"}
              >
                <span className="field-label">
                  {property.title ?? humanize(key)}
                  {required.includes(key) && <span className="field-dot" />}
                </span>
                {property.enum ? (
                  <select
                    value={values[key] ?? ""}
                    onChange={(event) => set(key, event.target.value)}
                  >
                    <option value="">—</option>
                    {property.enum.map((option) => (
                      <option key={String(option)} value={String(option)}>
                        {String(option)}
                      </option>
                    ))}
                  </select>
                ) : property.type === "boolean" ? (
                  <span className="toggle-row">
                    <button
                      type="button"
                      className={values[key] === "true" ? "toggle on" : "toggle"}
                      onClick={() =>
                        set(key, values[key] === "true" ? "false" : "true")
                      }
                      aria-pressed={values[key] === "true"}
                    >
                      <span className="toggle-knob" />
                    </button>
                  </span>
                ) : property.type === "object" ? (
                  <textarea
                    rows={3}
                    value={values[key] ?? ""}
                    onChange={(event) => set(key, event.target.value)}
                    spellCheck={false}
                  />
                ) : (
                  <input
                    type={
                      property.type === "number" || property.type === "integer"
                        ? "number"
                        : "text"
                    }
                    value={values[key] ?? ""}
                    placeholder={
                      property.type === "array" ? t("geral.listaValores") : ""
                    }
                    autoComplete="off"
                    onChange={(event) => set(key, event.target.value)}
                  />
                )}
                {property.description && (
                  <span className="field-hint">{property.description}</span>
                )}
              </label>
            );
          })}
        </div>
      )}

      <footer className="sheet-actions">
        <button type="button" onClick={onCancel}>
          {t("geral.cancelar")}
        </button>
        <button type="submit" className="primary" disabled={busy}>
          <Icon name="send" />
          {busy ? t("geral.executando") : t("geral.executar")}
        </button>
      </footer>
    </form>
  );
}