import type { TelaAjuste } from "../components/AppMenu";

/// O que a barra pode mencionar: uma pasta concedida ou o modelo que está em
/// memória. A menção é texto puro do pedido — quem a lê é o agente, e o núcleo
/// não ganha parser nenhum por causa dela.
export interface Mencao {
  nome: string;
  /// Chave de locale do tipo, para a folha dizer de onde vem cada nome.
  chave: string;
  icon: string;
  /// O caminho inteiro, quando o nome sozinho não diz qual das pastas é.
  detalhe?: string;
}

/// Um atalho é uma porta que já existe, alcançada pela barra. A lista não segura
/// estado nenhum: quem abre a tela e quem põe a bancada para rodar continua
/// sendo App.tsx — aqui só está o nome que `/` conhece e a mão que aperta.
export interface Atalho {
  /// O que se digita depois da barra.
  nome: string;
  /// Rótulo da folha: chave de locale, como em todo o resto da tela.
  chave: string;
  icon: string;
  onDisparar: () => void;
}

/// As mãos que a barra pode pedir. `abrir` é a mesma função do menu de ajustes.
export interface Mãos {
  abrir: (tela: TelaAjuste) => void;
  grafico: () => void;
  rodarBancada: () => void;
  pararBancada: () => void;
}

export function listaDeAtalhos(maos: Mãos): Atalho[] {
  return [
    { nome: "bancada", chave: "bancada.rodar", icon: "bancada", onDisparar: maos.rodarBancada },
    { nome: "parar", chave: "bancada.cancelar", icon: "stop", onDisparar: maos.pararBancada },
    { nome: "modelos", chave: "modelos.titulo", icon: "cube", onDisparar: () => maos.abrir("modelos") },
    { nome: "bancos", chave: "bancos.titulo", icon: "banco", onDisparar: () => maos.abrir("bancos") },
    { nome: "memoria", chave: "memoria.titulo", icon: "memoria", onDisparar: () => maos.abrir("memoria") },
    { nome: "grafo", chave: "app.grafo", icon: "nodes", onDisparar: maos.grafico },
  ];
}

/// O trecho escrito depois de `/`, ou `null` quando o pedido não é um atalho.
/// Um espaço fecha o atalho: dali para frente a barra voltou a ser texto.
export function buscaDeAtalho(texto: string): string | null {
  if (!texto.startsWith("/")) return null;
  const corpo = texto.slice(1).toLowerCase();
  return corpo.includes(" ") ? null : corpo;
}

/// O atalho inteiro que a pessoa escreveu, para quem aperta Enter sem escolher
/// na folha: `/bancada` nunca vira mensagem de conversa.
export function resolverAtalho(lista: Atalho[], texto: string): Atalho | null {
  const busca = buscaDeAtalho(texto.trim());
  if (busca === null) return null;
  return lista.find((atalho) => atalho.nome === busca) ?? null;
}

export function filtrarAtalhos(lista: Atalho[], busca: string): Atalho[] {
  if (busca === "") return lista;
  return lista.filter((atalho) => atalho.nome.startsWith(busca));
}

/// O `@` em curso: o último token assim antes do cursor. `desde` é onde o
/// arremate começa, para a escolha trocar só o que foi digitado.
export function buscaDeMencao(
  texto: string,
): { busca: string; desde: number } | null {
  const achado = /@([^\s@]*)$/.exec(texto);
  if (achado === null) return null;
  return { busca: achado[1].toLowerCase(), desde: achado.index };
}
