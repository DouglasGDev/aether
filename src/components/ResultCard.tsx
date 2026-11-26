import { humanize } from "../lib/display";
import { useT } from "../lib/i18n";
import type { CallToolResult, ContentBlock, Vista } from "../lib/types";
import { Icon } from "./Icon";
import { ToolCard } from "./ToolCard";

interface Props {
  title: string;
  result: CallToolResult;
  onClose: () => void;
}

/// Uma linha por vista que o `ToolCard` desenha. É um `Record` sobre o próprio
/// tipo, não uma lista copiada: quando a união ganha um membro, o `tsc` para
/// aqui — antes, sete vistas (memória, resultado e as cinco de bancos) ficaram
/// de fora e o cartão pronto virava despejo de JSON na tela.
const TIPOS_VISTA: Record<Vista["type"], true> = {
  arquivo: true,
  lista: true,
  busca: true,
  memoria: true,
  escrita: true,
  edicao: true,
  base: true,
  pasta: true,
  movido: true,
  apagado: true,
  comando: true,
  sistema: true,
  resultado: true,
  bancoConexoes: true,
  bancoFontes: true,
  bancoEsquema: true,
  bancoConsulta: true,
  bancoLinhas: true,
};

/// O conteúdo estruturado de uma ferramenta deste computador já é o cartão; o
/// de um servidor de terceiros é JSON qualquer, e só entra no cartão se tiver a
/// marcação de uma vista conhecida.
function comoVista(valor: unknown): Vista | null {
  if (valor === null || typeof valor !== "object" || Array.isArray(valor)) {
    return null;
  }
  const tipo = (valor as { type?: unknown }).type;
  return typeof tipo === "string" && tipo in TIPOS_VISTA
    ? (valor as Vista)
    : null;
}

function Block({ block }: { block: ContentBlock }) {
  if (block.type === "text") {
    return <p className="result-text">{block.text}</p>;
  }
  if (block.type === "image") {
    return (
      <img
        className="result-image"
        src={`data:${block.mimeType};base64,${block.data}`}
        alt=""
      />
    );
  }
  if (block.type === "audio") {
    return (
      <audio
        className="result-audio"
        controls
        src={`data:${block.mimeType};base64,${block.data}`}
      />
    );
  }
  return <p className="result-text">{JSON.stringify(block, null, 2)}</p>;
}

function Structured({ value }: { value: unknown }) {
  if (value !== null && typeof value === "object" && !Array.isArray(value)) {
    return (
      <dl className="result-structured">
        {Object.entries(value as Record<string, unknown>).map(([key, entry]) => (
          <div key={key}>
            <dt>{humanize(key)}</dt>
            <dd>
              {typeof entry === "string" ? entry : JSON.stringify(entry)}
            </dd>
          </div>
        ))}
      </dl>
    );
  }
  return <p className="result-text">{JSON.stringify(value, null, 2)}</p>;
}

export function ResultCard({ title, result, onClose }: Props) {
  const t = useT();
  const vista = comoVista(result.structuredContent);
  return (
    <div className={result.isError === true ? "sheet fault" : "sheet"}>
      <header className="sheet-head">
        <span className="sheet-icon">
          <Icon name={result.isError === true ? "warning" : "check"} />
        </span>
        <span className="sheet-title">{title}</span>
        <button
          type="button"
          className="sheet-close"
          onClick={onClose}
          aria-label={t("geral.fechar")}
        >
          <Icon name="cross" />
        </button>
      </header>
      {vista !== null ? (
        <div className="result-body">
          <ToolCard vista={vista} falha={result.isError === true} />
        </div>
      ) : (
        <div className="result-body">
          {result.content.map((block, index) => (
            <Block key={index} block={block} />
          ))}
          {result.structuredContent !== undefined && (
            <Structured value={result.structuredContent} />
          )}
          {result.content.length === 0 &&
            result.structuredContent === undefined && (
              <p className="sheet-note">{t("geral.vazio")}</p>
            )}
        </div>
      )}
    </div>
  );
}