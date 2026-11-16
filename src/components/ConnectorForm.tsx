import { useState } from "react";
import { api, message } from "../lib/api";
import { useT, type Traduzir } from "../lib/i18n";
import type {
  Connector,
  ConnectorCategory,
  McpAuth,
  McpEntry,
} from "../lib/types";

const CATEGORIAS: ConnectorCategory[] = [
  "system",
  "files",
  "web",
  "development",
  "data",
  "productivity",
];

const AUTENTICACOES: McpAuth[] = ["none", "token", "key", "oauth", "oauth-client"];

/// O valor enumerado viaja até o núcleo como chave; só o que aparece na tela
/// vem da pasta de idiomas.
const ROTULOS: Record<string, string> = {
  system: "servidores.categoriaSistema",
  files: "servidores.categoriaArquivos",
  web: "servidores.categoriaWeb",
  development: "servidores.categoriaDesenvolvimento",
  data: "servidores.categoriaDados",
  productivity: "servidores.categoriaProdutividade",
  none: "servidores.authNenhuma",
  token: "servidores.authToken",
  key: "servidores.authChave",
  oauth: "servidores.authOauth",
  "oauth-client": "servidores.authOauthCliente",
  stdio: "servidores.transporteProcesso",
  http: "servidores.transporteEndereco",
};

function rotulo(t: Traduzir, valor: string): string {
  return t(ROTULOS[valor] ?? valor);
}

function pares(texto: string, t: Traduzir): Record<string, string> {
  const saida: Record<string, string> = {};
  for (const linha of texto.split("\n")) {
    const aparada = linha.trim();
    if (aparada === "") continue;
    const separador = aparada.indexOf("=");
    if (separador <= 0) {
      throw new Error(t("servidores.erroLinhaInvalida", { linha: aparada }));
    }
    saida[aparada.slice(0, separador).trim()] = aparada
      .slice(separador + 1)
      .trim();
  }
  return saida;
}

interface Props {
  onSaved: () => void;
}

export function ConnectorForm({ onSaved }: Props) {
  const t = useT();
  const [id, setId] = useState("");
  const [name, setName] = useState("");
  const [category, setCategory] = useState<ConnectorCategory>("development");
  const [auth, setAuth] = useState<McpAuth>("none");
  const [transport, setTransport] = useState<"stdio" | "http">("stdio");
  const [command, setCommand] = useState("");
  const [argsText, setArgsText] = useState("");
  const [envText, setEnvText] = useState("");
  const [url, setUrl] = useState("");
  const [headersText, setHeadersText] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  async function submit(event: React.FormEvent) {
    event.preventDefault();
    setError(null);

    const trimmedId = id.trim();
    if (trimmedId === "") {
      setError(t("servidores.erroIdObrigatorio"));
      return;
    }
    if (/\s/.test(trimmedId) || trimmedId.includes("__")) {
      setError(t("servidores.erroIdInvalido"));
      return;
    }

    let entry: McpEntry;
    try {
      entry =
        transport === "stdio"
          ? {
              transport: "stdio",
              command: command.trim(),
              args: argsText
                .split("\n")
                .map((line) => line.trim())
                .filter((line) => line !== ""),
              env: pares(envText, t),
            }
          : {
              transport: "http",
              url: url.trim(),
              headers: pares(headersText, t),
            };
    } catch (cause) {
      setError(message(cause));
      return;
    }

    if (entry.transport === "stdio" && entry.command === "") {
      setError(t("servidores.erroComandoObrigatorio"));
      return;
    }
    if (entry.transport === "http" && entry.url === "") {
      setError(t("servidores.erroEnderecoObrigatorio"));
      return;
    }

    const connector: Connector = {
      id: trimmedId,
      name: name.trim() === "" ? trimmedId : name.trim(),
      description: "",
      kind: "mcp",
      category,
      docsUrl: "",
      routes: [],
      auth,
      entry,
      local: {},
      fields: [],
      env: [],
      toolAllowlist: null,
    };

    setSaving(true);
    try {
      await api.saveConnector(connector);
      setId("");
      setName("");
      setCommand("");
      setArgsText("");
      setEnvText("");
      setUrl("");
      setHeadersText("");
      onSaved();
    } catch (cause) {
      setError(message(cause));
    } finally {
      setSaving(false);
    }
  }

  return (
    <form className="connector-form" onSubmit={submit}>
      <div className="field-row">
        <label>
          {t("servidores.identificador")}
          <input
            value={id}
            onChange={(event) => setId(event.target.value)}
            autoComplete="off"
            spellCheck={false}
          />
        </label>
        <label>
          {t("servidores.nome")}
          <input
            value={name}
            onChange={(event) => setName(event.target.value)}
            autoComplete="off"
          />
        </label>
      </div>

      <div className="field-row">
        <label>
          {t("servidores.categoria")}
          <select
            value={category}
            onChange={(event) =>
              setCategory(event.target.value as ConnectorCategory)
            }
          >
            {CATEGORIAS.map((value) => (
              <option key={value} value={value}>
                {rotulo(t, value)}
              </option>
            ))}
          </select>
        </label>
        <label>
          {t("servidores.autenticacao")}
          <select
            value={auth}
            onChange={(event) => setAuth(event.target.value as McpAuth)}
          >
            {AUTENTICACOES.map((value) => (
              <option key={value} value={value}>
                {rotulo(t, value)}
              </option>
            ))}
          </select>
        </label>
        <label>
          {t("servidores.transporte")}
          <select
            value={transport}
            onChange={(event) =>
              setTransport(event.target.value as "stdio" | "http")
            }
          >
            <option value="stdio">{rotulo(t, "stdio")}</option>
            <option value="http">{rotulo(t, "http")}</option>
          </select>
        </label>
      </div>

      {transport === "stdio" ? (
        <>
          <label>
            {t("servidores.comando")}
            <input
              value={command}
              onChange={(event) => setCommand(event.target.value)}
              autoComplete="off"
              spellCheck={false}
            />
          </label>
          <label>
            {t("servidores.argumentos")}
            <textarea
              rows={2}
              value={argsText}
              onChange={(event) => setArgsText(event.target.value)}
              spellCheck={false}
            />
          </label>
          <label>
            {t("servidores.ambiente")}
            <textarea
              rows={2}
              value={envText}
              onChange={(event) => setEnvText(event.target.value)}
              spellCheck={false}
            />
          </label>
        </>
      ) : (
        <>
          <label>
            {t("servidores.endereco")}
            <input
              value={url}
              onChange={(event) => setUrl(event.target.value)}
              autoComplete="off"
              spellCheck={false}
            />
          </label>
          <label>
            {t("servidores.cabecalhos")}
            <textarea
              rows={2}
              value={headersText}
              onChange={(event) => setHeadersText(event.target.value)}
              spellCheck={false}
            />
          </label>
        </>
      )}

      {error !== null && <p className="error">{error}</p>}
      <button type="submit" className="primary" disabled={saving}>
        {saving ? t("servidores.salvando") : t("geral.adicionar")}
      </button>
    </form>
  );
}