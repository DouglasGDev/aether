import type {
  GeneroVoz,
  ModoMemoria,
  OrigemMemoria,
  ServicoVoz,
  StopReason,
  ToolEffect,
  ToolInfo,
  VistaPar,
} from "./types";

/// Nomes de ferramenta de servidor de terceiros: sem título nosso, a interface
/// legibiliza o nome que o servidor anunciou.
export function humanize(name: string): string {
  return name
    .replace(/[-_]+/g, " ")
    .replace(/\b\w/g, (character) => character.toUpperCase());
}

export function toolTitle(tool: ToolInfo): string {
  if (tool.titulo !== undefined) return tool.titulo;
  return humanize(tool.name);
}

export const EFFECT_KEY: Record<ToolEffect, string> = {
  "read-only": "efeito.soLe",
  mutating: "efeito.altera",
  destructive: "efeito.podeApagar",
};

export const EFFECT_ICON: Record<ToolEffect, string> = {
  "read-only": "eye",
  mutating: "pencil",
  destructive: "warning",
};

/// Os dois serviços do catálogo de voz. O núcleo manda a variante do enum; a
/// palavra que a tela mostra é nossa e sai da pasta de idiomas.
export const SERVICO_KEY: Record<ServicoVoz, string> = {
  fala: "voz.servicoFala",
  escuta: "voz.servicoEscuta",
};

export const GENERO_KEY: Record<GeneroVoz, string> = {
  feminina: "voz.generoFeminina",
  masculina: "voz.generoMasculina",
};

/// A ordem que a busca usou. O núcleo devolve a palavra de máquina do modo; a
/// tela só traduz, inclusive no cartão da conversa, para que as duas visões da
/// mesma busca digam a mesma coisa.
export const MODO_KEY: Record<ModoMemoria, string> = {
  vetorial: "memoria.modoVetorial",
  hibrido: "memoria.modoHibrido",
  lexico: "memoria.modoLexico",
};

/// O que trouxe um achado: o vetor, a palavra escrita ou os dois.
export const ORIGEM_KEY: Record<OrigemMemoria, string> = {
  vetorial: "memoria.origemVetorial",
  lexica: "memoria.origemLexica",
  ambas: "memoria.origemAmbas",
};

/// Por que o motor parou. A palavra é nossa; o motivo vem medido do motor.
export const STOP_KEY: Record<StopReason, string> = {
  eos: "conversa.paradaFim",
  stopSequence: "conversa.paradaSequencia",
  maxTokens: "conversa.paradaLimite",
  cancelled: "conversa.paradaCancelada",
  contextFull: "conversa.paradaJanela",
};

/// Unidades binárias com o separador decimal do idioma escolhido.
export function bytes(quantidade: number, idioma: string): string {
  const unidades = ["B", "KiB", "MiB", "GiB", "TiB"];
  let valor = quantidade;
  let unidade = 0;
  while (valor >= 1024 && unidade < unidades.length - 1) {
    valor /= 1024;
    unidade += 1;
  }
  const formatado = new Intl.NumberFormat(idioma, {
    maximumFractionDigits: unidade === 0 ? 0 : 1,
  }).format(valor);
  return `${formatado} ${unidades[unidade]}`;
}

export function numero(valor: number, idioma: string, casas = 1): string {
  return new Intl.NumberFormat(idioma, { maximumFractionDigits: casas }).format(
    valor,
  );
}

export function inteiro(valor: number, idioma: string): string {
  return new Intl.NumberFormat(idioma).format(valor);
}

export function data(milissegundos: number, idioma: string): string {
  return new Intl.DateTimeFormat(idioma, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(new Date(milissegundos));
}

/// A data de uma conversa na lista: cabe numa linha estreita. O valor completo
/// continua disponível no `title` da linha.
export function dataCurta(milissegundos: number, idioma: string): string {
  return new Intl.DateTimeFormat(idioma, {
    day: "2-digit",
    month: "short",
    hour: "2-digit",
    minute: "2-digit",
  }).format(new Date(milissegundos));
}

/// Duração de comando: abaixo de um segundo o valor cru em milissegundos é mais
/// útil que "0,0 s".
export function duracao(ms: number, idioma: string): string {
  if (ms < 1000) return `${Math.round(ms)} ms`;
  return `${numero(ms / 1000, idioma, 1)} s`;
}

/// Uma linha de medida: o número cru vem do Rust e é formatado aqui, para que
/// "31,9 GiB" em português seja "31.9 GiB" em inglês.
export function medida(linha: VistaPar, idioma: string): string {
  if (linha.numero === undefined) return linha.valor;
  const valor = numero(linha.numero, idioma, 1);
  return linha.unidade === undefined ? valor : `${valor} ${linha.unidade}`;
}
