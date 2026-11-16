/// A pasta `locales/` na raiz do projeto é a fonte dos idiomas. Cada JSON ali
/// vira um idioma disponível: acrescentar um arquivo não exige mudar código.
const arquivos = import.meta.glob<{ default: Record<string, unknown> }>(
  "../../locales/*.json",
  { eager: true },
);

export const REFERENCIA = "pt-BR";

export type Mensagens = Record<string, string>;

export interface Idioma {
  id: string;
  nome: string;
  mensagens: Mensagens;
}

function achatar(
  valor: Record<string, unknown>,
  prefixo: string,
  saida: Mensagens,
): void {
  for (const [chave, filho] of Object.entries(valor)) {
    const caminho = prefixo === "" ? chave : `${prefixo}.${chave}`;
    if (typeof filho === "string") saida[caminho] = filho;
    else if (filho !== null && typeof filho === "object")
      achatar(filho as Record<string, unknown>, caminho, saida);
  }
}

export const idiomas: Idioma[] = Object.entries(arquivos)
  .map(([caminho, modulo]) => {
    const id = caminho.slice(caminho.lastIndexOf("/") + 1, -".json".length);
    const mensagens: Mensagens = {};
    achatar(modulo.default, "", mensagens);
    return { id, nome: mensagens["idioma.nome"] ?? id, mensagens };
  })
  .sort((a, b) => a.nome.localeCompare(b.nome, "pt-BR"));

const porId = new Map(idiomas.map((idioma) => [idioma.id, idioma]));

export function existe(id: string): boolean {
  return porId.has(id);
}

/// `pt`, `pt-PT` e `pt_BR` caem em `pt-BR`; um idioma sem tradução cai na
/// referência em vez de devolver a tela sem texto.
export function resolver(entrada: string | null | undefined): string {
  const bruto = (entrada ?? "").trim().replace(/_/g, "-").toLowerCase();
  if (bruto === "") return REFERENCIA;
  if (porId.has(bruto)) return bruto;
  const exato = idiomas.find((i) => i.id.toLowerCase() === bruto);
  if (exato !== undefined) return exato.id;
  const prefixo = idiomas.find((i) => bruto.startsWith(`${i.id.toLowerCase()}-`));
  if (prefixo !== undefined) return prefixo.id;
  const base = bruto.split("-")[0];
  const familia = idiomas.find((i) => i.id.toLowerCase().split("-")[0] === base);
  return familia?.id ?? REFERENCIA;
}

export function idiomaDoSistema(): string {
  return resolver(navigator.language);
}

export function traduzir(
  id: string,
  chave: string,
  valores?: Record<string, string | number>,
): string {
  const texto =
    porId.get(id)?.mensagens[chave] ?? porId.get(REFERENCIA)?.mensagens[chave];
  // Chave ausente volta como a própria chave: um defeito visível, nunca um
  // texto inventado no lugar do que deveria estar ali.
  if (texto === undefined) return chave;
  if (valores === undefined) return texto;
  return texto.replace(/\{(\w+)\}/g, (marcador, nome: string) =>
    nome in valores ? String(valores[nome]) : marcador,
  );
}