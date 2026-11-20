import { useState, type ReactElement, type ReactNode } from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import { useT } from "../lib/i18n";
import { Icon } from "./Icon";

/// Um bloco de código com mais linhas que isto entra dobrado. O número vem do
/// caso real: a resposta do modelo despejou 106 linhas de HTML no meio da
/// conversa, e o cartão da ferramenta já mostra o arquivo que foi escrito.
const DOBRA = 16;

/// Achata o que o markdown devolveu em texto puro. O bloco de código chega como
/// um <code> cujo conteúdo é a própria fonte; é ela que se copia e se conta.
function textoDe(filho: ReactNode): string {
  if (filho === null || filho === undefined || typeof filho === "boolean") {
    return "";
  }
  if (typeof filho === "string" || typeof filho === "number") {
    return String(filho);
  }
  if (Array.isArray(filho)) {
    return filho.map(textoDe).join("");
  }
  return textoDe((filho as ReactElement<{ children?: ReactNode }>).props?.children);
}

/// O filho de `<pre>` que é o código. O react-markdown já trocou a tag pelo
/// componente daqui, então `type` não é mais a string `"code"`: o que identifica
/// o bloco é o nó hast que ele carrega e a classe `language-`.
type Elemento = ReactElement<{
  className?: string;
  children?: ReactNode;
  node?: { tagName?: string };
}>;

function ehCodigo(filho: ReactNode): filho is Elemento {
  if (typeof filho !== "object" || filho === null || Array.isArray(filho)) return false;
  const el = filho as Elemento;
  if (el.type === "code") return true;
  if (el.props?.node?.tagName === "code") return true;
  return typeof el.props?.className === "string" && /(^|\s)language-/.test(el.props.className);
}

function codigoDe(filhos: ReactNode): { corpo: string; linguagem: string } | null {
  const lista = (Array.isArray(filhos) ? filhos : [filhos]).filter(ehCodigo);
  const alvo = lista[0];
  if (alvo === undefined) return null;
  const casa = /language-([\w-]+)/.exec(alvo.props.className ?? "");
  const corpo = textoDe(alvo.props.children).replace(/\n+$/, "");
  return { corpo, linguagem: casa === null ? "" : casa[1] };
}

function Bloco({ corpo, linguagem }: { corpo: string; linguagem: string }) {
  const t = useT();
  const linhas = corpo.split("\n");
  const dobravel = linhas.length > DOBRA;
  const [aberto, setAberto] = useState(!dobravel);
  const podeCopiar =
    typeof navigator !== "undefined" &&
    typeof navigator.clipboard?.writeText === "function";
  const [estado, setEstado] = useState<"nenhum" | "copiado" | "falhou">("nenhum");

  async function copiar() {
    try {
      await navigator.clipboard.writeText(corpo);
      setEstado("copiado");
    } catch {
      // A recusa é do ambiente, não um sucesso disfarçado: o botão diz o que
      // aconteceu e a pessoa continua podendo selecionar o texto.
      setEstado("falhou");
    }
  }

  const visiveis = aberto ? corpo : linhas.slice(0, DOBRA).join("\n");

  return (
    <div className="rich-bloco">
      <div className="rich-bloco-head">
        <span className="rich-bloco-linguagem">
          {linguagem === "" ? t("conversa.codigoSemLinguagem") : linguagem}
        </span>
        <span className="rich-bloco-meta">
          {t("conversa.codigoLinhas", { n: linhas.length })}
        </span>
        <span className="rich-bloco-acoes">
          {dobravel && (
            <button
              type="button"
              className="rich-bloco-botao"
              onClick={() => setAberto(!aberto)}
            >
              {aberto ? t("conversa.recolher") : t("conversa.mostrarTudo")}
            </button>
          )}
          {podeCopiar && (
            <button
              type="button"
              className="rich-bloco-botao"
              onClick={copiar}
              title={t("conversa.copiar")}
            >
              <Icon name={estado === "copiado" ? "check" : "copiar"} />
              {estado === "copiado"
                ? t("conversa.copiado")
                : estado === "falhou"
                  ? t("conversa.copiarFalhou")
                  : t("conversa.copiar")}
            </button>
          )}
        </span>
      </div>
      <pre className="rich-bloco-codigo">{visiveis}</pre>
    </div>
  );
}

const COMPONENTES: Components = {
  pre: ({ children }) => {
    const bloco = codigoDe(children);
    if (bloco === null) {
      // Código indentado sem <code> dentro: continua código, sem inventar cabo.
      return <pre className="rich-bloco-codigo">{textoDe(children)}</pre>;
    }
    return <Bloco corpo={bloco.corpo} linguagem={bloco.linguagem} />;
  },
  /// O bloco em linha sai do <pre> e é tratado pelo CSS de <code> solto.
  code: ({ children }) => <code className="rich-inline">{textoDe(children)}</code>,
  a: ({ href, children }) => {
    const seguro = typeof href === "string" && /^https?:\/\//i.test(href);
    if (!seguro) {
      // Link que não é http(s) não vira âncora: o endereço continua à vista,
      // mas nada de chamar um protocolo que o app não pediu.
      return <span className="rich-endereco">{textoDe(children)}</span>;
    }
    return (
      <a className="rich-link" href={href} target="_blank" rel="noreferrer noopener">
        {children}
      </a>
    );
  },
  img: ({ alt }) => (
    <span className="rich-endereco">{alt ?? ""}</span>
  ),
  h1: ({ children }) => <h1 className="rich-titulo rich-titulo-1">{children}</h1>,
  h2: ({ children }) => <h2 className="rich-titulo rich-titulo-2">{children}</h2>,
  h3: ({ children }) => <h3 className="rich-titulo rich-titulo-3">{children}</h3>,
  h4: ({ children }) => <h4 className="rich-titulo rich-titulo-4">{children}</h4>,
  h5: ({ children }) => <h5 className="rich-titulo rich-titulo-5">{children}</h5>,
  h6: ({ children }) => <h6 className="rich-titulo rich-titulo-6">{children}</h6>,
  table: ({ children }) => (
    <div className="rich-tabela-pasta">
      <table className="rich-tabela">{children}</table>
    </div>
  ),
  input: ({ checked, type }) =>
    type === "checkbox" ? (
      <span className={`rich-caixa ${checked ? "marcada" : ""}`} aria-hidden="true" />
    ) : null,
};

interface Props {
  text: string;
}

/// A resposta do modelo é markdown: ele escreve listas, negrito e cercas de
/// código porque foi treinado assim. Pintar aquilo como texto mostrava os
/// marcadores na tela em vez do que eles significam.
export function RichText({ text }: Props) {
  return (
    <ReactMarkdown remarkPlugins={[remarkGfm]} components={COMPONENTES}>
      {text}
    </ReactMarkdown>
  );
}
