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

function valoresIniciais(
  keys: string[],
  properties: Record<string, Property>,
): Record<string, string> {
  const initial: Record<string, string> = {};
  for (const key of keys) {
    const value = properties[key].default;
    initial[key] = value === undefined || value === null ? "" : String(value);
  }
  return initial;
}

export function ArgumentForm({ tool, busy, onCancel, onRun }: Props) {
  const t = useT();
  const schema = (tool.inputSchema ?? {}) as Schema;
  const properties = schema.properties ?? {};
  const required = schema.required ?? [];
  const keys = Object.keys(properties);

  const [values, setValues] = useState<Record<string, string>>(() =>
    valoresIniciais(keys, properties),
  );
  const [missing, setMissing] = useState<string[]>([]);
  const [invalidos, setInvalidos] = useState<Record<string, string>>({});
  const [ferramenta, setFerramenta] = useState(tool.name);

  // O cartão é o mesmo formulário trocando de ferramenta: os valores nascem uma
  // vez no estado, e sem este reset a segunda ferramenta receberia os argumentos
  // da primeira — um caminho de arquivo onde devia ir um termo de busca.
  if (ferramenta !== tool.name) {
    setFerramenta(tool.name);
    setValues(valoresIniciais(keys, properties));
    setMissing([]);
    setInvalidos({});
  }

  function set(key: string, value: string) {
    setValues((current) => ({ ...current, [key]: value }));
    setMissing((current) => current.filter((entry) => entry !== key));
    setInvalidos((current) => {
      if (!(key in current)) return current;
      const limpo = { ...current };
      delete limpo[key];
      return limpo;
    });
  }

  function submit(event: React.FormEvent) {
    event.preventDefault();
    const absent = required.filter((key) => (values[key] ?? "").trim() === "");
    if (absent.length > 0) {
      setMissing(absent);
      return;
    }
    const args: Record<string, unknown> = {};
    const erros: Record<string, string> = {};
    for (const key of keys) {
      const raw = (values[key] ?? "").trim();
      const property = properties[key];
      if (raw === "" && !required.includes(key)) continue;
      if (property.enum) {
        args[key] = raw;
      } else if (property.type === "boolean") {
        args[key] = raw === "true";
      } else if (property.type === "number" || property.type === "integer") {
        // Number("duas") é NaN, e JSON.stringify(NaN) é null: a ferramenta
        // receberia null sem ninguém ter digitado null.
        const numero = Number(raw);
        if (Number.isFinite(numero)) args[key] = numero;
        else erros[key] = t("geral.invalidoNumero");
      } else if (property.type === "array") {
        const partes = raw.split(",").map((parte) => parte.trim()).filter(Boolean);
        if (
          property.items?.type === "number" ||
          property.items?.type === "integer"
        ) {
          const numeros = partes.map(Number);
          if (numeros.every((numero) => Number.isFinite(numero))) args[key] = numeros;
          else erros[key] = t("geral.invalidoNumero");
        } else {
          args[key] = partes;
        }
      } else if (property.type === "object") {
        try {
          args[key] = JSON.parse(raw);
        } catch {
          // Sem o try, o JSON malformado estourava aqui dentro e o clique em
          // Executar não fazia nada: o erro ficava só no console.
          erros[key] = t("geral.invalidoJson");
        }
      } else {
        args[key] = raw;
      }
    }
    setInvalidos(erros);
    if (Object.keys(erros).length === 0) onRun(args);
  }

  const camposRuins = Object.keys(invalidos);

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
            const erro = invalidos[key];
            const obrigatoria = required.includes(key);
            return (
              <label
                key={key}
                className={isMissing || erro ? "field missing" : "field"}
              >
                <span className="field-label">
                  {property.title ?? humanize(key)}
                  {obrigatoria && <span className="field-dot" aria-hidden="true" />}
                </span>
                {property.enum ? (
                  <select
                    value={values[key] ?? ""}
                    aria-required={obrigatoria || undefined}
                    aria-invalid={erro ? true : undefined}
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
                    aria-required={obrigatoria || undefined}
                    aria-invalid={erro ? true : undefined}
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
                    aria-required={obrigatoria || undefined}
                    aria-invalid={erro ? true : undefined}
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

      {camposRuins.length > 0 && (
        <p className="sheet-error" role="alert">
          <Icon name="warning" />
          <span>
            {camposRuins
              .map(
                (key) =>
                  `${properties[key].title ?? humanize(key)}: ${invalidos[key]}`,
              )
              .join(" · ")}
          </span>
        </p>
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