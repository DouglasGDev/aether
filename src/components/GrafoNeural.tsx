import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, message } from "../lib/api";
import { useIdioma, useT } from "../lib/i18n";
import type {
  EstadoLigacoes,
  NoLigacao,
  Plano,
  Porta,
  Posicao,
  ToolEffect,
} from "../lib/types";
import { Icon } from "./Icon";

/** Uma sinapse que disparou (ou está disparando) agora. `n` é o contador de
 *  eventos da mesma chave: é ele que faz a animação recomeçar num fio que já
 *  estava desenhado. A chave é o nome qualificado que o núcleo cobra. */
export interface Disparo {
  emCurso: boolean;
  ok?: boolean;
  ms?: number;
  n: number;
}

interface Props {
  disparos: Record<string, Disparo>;
  /// O retrato dos conectores ligados, tal como o núcleo os vê. Muda quando um
  /// servidor entra ou sai e é o sinal de que o grafo precisa ser relido: ligar
  /// um conector tem que mostrar as ferramentas dele, não pedir um clique no
  /// botão de atualizar.
  sinal: string;
  onConectar: (id: string) => void;
  /// Na tela do grafo os cartões do mundo substituem a faixa de conectores, e é
  /// daqui que se chama um servidor novo.
  onAdicionar: () => void;
  onChange: () => void;
}

const LARGURA = 960;
/// O mundo mede o que há para desenhar, com um piso apenas o bastante para o
/// Aether ter onde ficar: um grafo que mostra só o que está ligado cabe na
/// tela, e caber na tela é o que o separa de uma folha com a lista de
/// ferramentas colada.
const ALTURA_MINIMA = 420;
const LARGURA_NO = 210;
const LARGURA_HUB = 220;
const ALTURA_HUB = 124;
/// O teto do hub: passado isto a escada passa a caber por passo, não por sorte.
const ALTURA_HUB_MAXIMA = 340;
const COLUNA_ESQUERDA = 12;
const COLUNA_DIREITA = LARGURA - 12 - LARGURA_NO;
const ALTURA_PORTA = 27;
const ALTURA_CABECA = 34;
/// A fileira que abre ou fecha as portas desligadas de um nó.
const ALTURA_DOBRA = 24;
// A borda é a caixa do nó: o ponto da porta nasce encostado nela por dentro, e
// é desse centro que o fio sai.
const BORDA = 1;
const FOLGA = 6;
const RODAPE = 10;
const AVISO_ALTURA = 26;
const LARGURA_PASTA = 184;
const ALTURA_PASTA = 46;
const PASSO_PASTA = 196;
const SALTO_PASTA = ALTURA_PASTA + 10;
const PASTAS_POR_LINHA = 4;
const CIMA_PASTA = 14;
const CIMA_NO = CIMA_PASTA + ALTURA_PASTA + 22;
/// As sinapses se distribuem na borda do destino, ordenadas pela altura de
/// origem: é isso que mantém o feixe aberto com o menor número de cruzamentos.
const PASSO_LADDER = 13;
const COTOVELO = 18;
/// Quantas coordenadas do mundo separam um toque curto de um arraste de fio.
const DISTANCIA_TOQUE = 9;

const variaveis = {
  "--lig-borda": `${BORDA}px`,
  "--lig-porta": `${ALTURA_PORTA}px`,
  "--lig-portas-topo": `${FOLGA}px`,
  "--lig-portas-rodape": `${RODAPE}px`,
} as React.CSSProperties;

type Ponto = { x: number; y: number };
type Caixa = { x: number; y: number; largura: number; altura: number };
type Lado = "esquerda" | "direita" | "topo";
type Mundo = Caixa & { hub: Caixa };

function limitar(valor: number, minimo: number, maximo: number): number {
  return Math.max(minimo, Math.min(valor, maximo));
}

const dentroDe = (p: Ponto, c: Caixa): boolean =>
  p.x >= c.x && p.x <= c.x + c.largura && p.y >= c.y && p.y <= c.y + c.altura;

/** O que um nó desenha: as portas ligadas — ou todas, quando o dono abriu o
 *  cartão. É esta lista, e não o catálogo que o servidor anunciou, que mede a
 *  altura do nó e o ponto de onde sai cada fio. O retrato do grafo é a
 *  política que o núcleo vai cobrar, não a vitrine do servidor. */
type Ficha = {
  no: NoLigacao;
  /// As portas desenhadas: as ligadas, ou todas quando o cartão está aberto. É
  /// esta lista — não o catálogo que o servidor anunciou — que mede a altura do
  /// nó e o ponto de onde sai cada fio.
  portas: Porta[];
  /// Quantas sinapses o nó tem: o número do cartão, e o que a escada do Aether
  /// precisa receber.
  sinapses: number;
  /// Há uma fileira de dobra no cartão: algo a mostrar ou a esconder.
  dobra: boolean;
  aberta: boolean;
  ocultas: number;
};

function fichasDe(nos: NoLigacao[], abertos: Record<string, boolean>): Ficha[] {
  return nos.map((no) => {
    const ligadas = no.portas.filter((p) => p.ligada);
    const aberta = abertos[no.id] === true;
    const ocultas = no.portas.length - ligadas.length;
    // Desligado, o nó não tem sinapse nenhuma no desenho: o que o servidor
    // anuncia só entra em cena quando ele está de pé, e o primeiro gesto é
    // conectar. Um nó com tudo ligado também não tem dobra: esconderia a
    // própria verdade.
    return {
      no,
      portas: no.conectado ? (aberta ? no.portas : ligadas) : [],
      sinapses: no.conectado ? ligadas.length : 0,
      dobra: no.conectado && ocultas > 0,
      aberta,
      ocultas,
    };
  });
}

function alturaDaFicha(ficha: Ficha): number {
  const cabeca = BORDA * 2 + ALTURA_CABECA;
  if (ficha.portas.length === 0) {
    return cabeca + (ficha.dobra ? ALTURA_DOBRA : AVISO_ALTURA);
  }
  return (
    cabeca +
    FOLGA +
    ficha.portas.length * ALTURA_PORTA +
    (ficha.dobra ? ALTURA_DOBRA : 0) +
    RODAPE
  );
}

/** Centro da porta dentro do nó, na mesma conta da altura dele. */
function centroPorta(indice: number): number {
  return BORDA + ALTURA_CABECA + FOLGA + indice * ALTURA_PORTA + ALTURA_PORTA / 2;
}

/** O mundo mede o que os nós precisam: o grafo cresce com o que está ligado em
 *  vez de espremer o catálogo inteiro numa caixa fixa. O Aether fica no meio
 *  das sinapses, as pastas entram por cima e as ferramentas pelos lados. */
function mundoDe(fichas: Ficha[]): Mundo {
  const { encaixe, fundo: fundoColunas } = colunas(fichas);
  let ligadasEsquerda = 0;
  let ligadasDireita = 0;
  for (const ficha of fichas) {
    if (!ficha.no.conectado) continue;
    if (encaixe[ficha.no.id].x === COLUNA_ESQUERDA) ligadasEsquerda += ficha.sinapses;
    else ligadasDireita += ficha.sinapses;
  }
  // O hub cresce com o feixe que precisa receber: sem isto vinte sinapses
  // disputariam os mesmos pixels da borda.
  const alturaHub = limitar(
    24 + Math.max(ligadasEsquerda, ligadasDireita) * PASSO_LADDER,
    ALTURA_HUB,
    ALTURA_HUB_MAXIMA,
  );
  const centro = CIMA_NO + (fundoColunas - CIMA_NO) / 2;
  const yHub = limitar(
    centro - alturaHub / 2,
    CIMA_NO,
    Math.max(CIMA_NO, fundoColunas - alturaHub),
  );
  // O Aether manda: nenhum cartão pode começar acima dele nem terminar dentro
  // do que ele ocupe.
  const fundo = Math.max(fundoColunas, yHub + alturaHub);
  return {
    x: 0,
    y: 0,
    largura: LARGURA,
    altura: Math.max(ALTURA_MINIMA, fundo + 12),
    hub: {
      x: (LARGURA - LARGURA_HUB) / 2,
      y: yHub,
      largura: LARGURA_HUB,
      altura: alturaHub,
    },
  };
}

/** O desenho padrão: as duas bordas do Aether, cada uma descendo pela coluna
 *  que tem mais chão. Quem tem fio entra primeiro, porque é o cartão alto; o
 *  que ainda espera por uma sinapse ocupa o espaço que sobra do outro lado, em
 *  vez de arrastar o desenho inteiro para baixo de uma prateleira. */
function colunas(fichas: Ficha[]): { encaixe: Record<string, Posicao>; fundo: number } {
  const encaixe: Record<string, Posicao> = {};
  let esquerda = CIMA_NO;
  let direita = CIMA_NO;
  const empilhar = (ficha: Ficha) => {
    const altura = alturaDaFicha(ficha) + 18;
    const lado = esquerda <= direita ? "esquerda" : "direita";
    encaixe[ficha.no.id] = {
      id: ficha.no.id,
      x: lado === "esquerda" ? COLUNA_ESQUERDA : COLUNA_DIREITA,
      y: lado === "esquerda" ? esquerda : direita,
    };
    if (lado === "esquerda") esquerda += altura;
    else direita += altura;
  };
  // Os ligados primeiro: são os cartões altos e é em volta deles que o Aether
  // se centra. Quem ainda espera um fio entra no chão que sobrou na outra borda.
  fichas.filter((ficha) => ficha.no.conectado).forEach(empilhar);
  fichas.filter((ficha) => !ficha.no.conectado).forEach(empilhar);
  return { encaixe, fundo: Math.max(esquerda, direita) };
}

const portaDoNo = (p: Ponto, indice: number, lado: Lado): Ponto => ({
  x: lado === "direita" ? p.x + BORDA : p.x + LARGURA_NO - BORDA,
  y: p.y + centroPorta(indice),
});

/** A caixa de um cartão no mundo. */
type Retangulo = { x: number; y: number; largura: number; altura: number };

const rectDoNo = (p: Posicao, ficha: Ficha): Retangulo => ({
  x: p.x,
  y: p.y,
  largura: LARGURA_NO,
  altura: alturaDaFicha(ficha),
});

const seChoca = (a: Retangulo, b: Retangulo): boolean =>
  a.x < b.x + b.largura &&
  a.x + a.largura > b.x &&
  a.y < b.y + b.altura &&
  a.y + a.altura > b.y;

/** A posição pedida dentro dos limites do mundo. */
function apertar(ficha: Ficha, p: Posicao, mundo: Mundo): Posicao {
  return {
    id: ficha.no.id,
    x: limitar(p.x, COLUNA_ESQUERDA, LARGURA - COLUNA_ESQUERDA - LARGURA_NO),
    y: limitar(p.y, CIMA_NO, mundo.altura - alturaDaFicha(ficha) - 12),
  };
}

/** Onde cada cartão fica. A posição guardada vale enquanto não tapar o Aether
 *  nem cobrir o lugar de outro cartão — dois cartões sobrepostos são exatamente
 *  a bagunça que fazia o grafo ser ilegível; o que sobra volta para o slot do
 *  padrão. `segurando` é o cartão que a mão tem agora: esse vai aonde o dedo o
 *  leva. O buraco de origem de cada cartão nunca é obstáculo para ele mesmo, e
 *  um cartão que aceitou uma posição sai do slot que deixou para trás. */
function lugares(
  fichas: Ficha[],
  guardado: Record<string, Posicao>,
  mundo: Mundo,
  segurando: string | null,
): Record<string, Posicao> {
  const { encaixe } = colunas(fichas);
  const ocupado = new Map<string, Retangulo>();
  for (const ficha of fichas) {
    const p = encaixe[ficha.no.id];
    if (p !== undefined) ocupado.set(ficha.no.id, rectDoNo(p, ficha));
  }
  const ditados: Record<string, Posicao> = {};
  for (const ficha of fichas) {
    const pedida = guardado[ficha.no.id];
    if (pedida === undefined) continue;
    const p = apertar(ficha, pedida, mundo);
    const r = rectDoNo(p, ficha);
    if (segurando !== ficha.no.id) {
      const choca =
        seChoca(r, mundo.hub) ||
        [...ocupado].some(([id, o]) => id !== ficha.no.id && seChoca(r, o));
      if (choca) continue;
    }
    ditados[ficha.no.id] = p;
    ocupado.set(ficha.no.id, r);
  }
  const saida: Record<string, Posicao> = {};
  for (const ficha of fichas) {
    const p = ditados[ficha.no.id] ?? encaixe[ficha.no.id];
    if (p !== undefined) saida[ficha.no.id] = p;
  }
  return saida;
}

const pastaX = (indice: number): number =>
  COLUNA_ESQUERDA + (indice % PASTAS_POR_LINHA) * PASSO_PASTA;

const pastaY = (indice: number): number =>
  CIMA_PASTA + Math.floor(indice / PASTAS_POR_LINHA) * SALTO_PASTA;

/** A pasta despeja no Aether pela borda de baixo do cartão. */
const portaDaPasta = (indice: number): Ponto => ({
  x: pastaX(indice) + LARGURA_PASTA / 2,
  y: pastaY(indice) + ALTURA_PASTA - BORDA,
});

/** Onde uma linha encosta no Aether. O índice é a posição dela na lista
 *  ordenada pela altura de origem, do mesmo lado. O passo se ajusta ao que a
 *  borda tem: caber sempre cabe, e dois fios no mesmo pixel não são dois fios. */
function portaDoAether(k: number, total: number, lado: Lado, hub: Caixa): Ponto {
  if (lado === "topo") {
    const passo = Math.min(PASSO_LADDER, (hub.largura - 24) / Math.max(1, total - 1));
    const meio = hub.x + hub.largura / 2;
    return { x: meio + (k - (total - 1) / 2) * passo, y: hub.y };
  }
  const passo = Math.min(PASSO_LADDER, (hub.altura - 20) / Math.max(1, total - 1));
  const meio = hub.y + hub.altura / 2;
  return {
    x: lado === "esquerda" ? hub.x : hub.x + hub.largura,
    y: meio + (k - (total - 1) / 2) * passo,
  };
}

/** Linha com cotovelo, como um axônio desenhado à mão: sai no prumo da porta,
 *  vira na metade do caminho e chega no prumo do destino. É a forma que um
 *  sinal elétrico parece percorrer, e ela dá ao feixe uma direção legível. */
function caminhoFio(de: Ponto, para: Ponto): string {
  const dx = para.x - de.x;
  const dy = para.y - de.y;
  if (Math.abs(dy) >= Math.abs(dx)) {
    const rumo = dy >= 0 ? 1 : -1;
    const curva = Math.max(COTOVELO, Math.abs(dy) * 0.35);
    const meio = (de.y + para.y) / 2;
    return [
      `M ${de.x} ${de.y}`,
      `L ${de.x} ${de.y + rumo * curva}`,
      `L ${para.x} ${meio}`,
      `L ${para.x} ${para.y - rumo * curva}`,
      `L ${para.x} ${para.y}`,
    ].join(" ");
  }
  const rumo = dx >= 0 ? 1 : -1;
  const curva = Math.max(COTOVELO, Math.abs(dx) * 0.35);
  const meio = (de.x + para.x) / 2;
  return [
    `M ${de.x} ${de.y}`,
    `L ${de.x + rumo * curva} ${de.y}`,
    `L ${meio} ${para.y}`,
    `L ${para.x - rumo * curva} ${para.y}`,
    `L ${para.x} ${para.y}`,
  ].join(" ");
}

/** O plano é a lista completa do que fica ligado, derivada do que o núcleo
 *  cobra hoje — mais o gesto que o usuário acabou de fazer. */
function planoDe(estado: EstadoLigacoes, pastas: string[]): Plano {
  return {
    conectores: estado.nos
      .filter((no) => no.portas.length > 0)
      .map((no) => ({
        id: no.id,
        ferramentas: no.portas.filter((p) => p.ligada).map((p) => p.nome),
      })),
    pastas,
  };
}

function cor(efeito: ToolEffect | "pasta"): string {
  return efeito === "read-only"
    ? "le"
    : efeito === "mutating"
      ? "altera"
      : efeito === "destructive"
        ? "apaga"
        : "pasta";
}

const ZOOM_MINIMO = 0.6;
const ZOOM_MAXIMO = 1.6;
/// Ajustar nunca encolhe o grafo abaixo disto: embaixo disso o rótulo do cartão
/// deixa de ser leitura, e o que sobra cabe na rolagem da moldura.
const AJUSTE_MINIMO = 0.8;

type Arraste = { id: string; dx: number; dy: number };
/// Um fio que está sendo puxado: a porta de origem e a ponta solta seguindo o
/// cursor. `alvo` nulo até o primeiro movimento é o que separa o toque curto do
/// arraste no momento da soltura.
type FioTirado = {
  noId: string;
  porta: string;
  de: Ponto;
  alvo: Ponto | null;
};

export function GrafoNeural({
  disparos,
  sinal,
  onConectar,
  onAdicionar,
  onChange,
}: Props) {
  const t = useT();
  const { id: idioma } = useIdioma();
  const [base, setBase] = useState<EstadoLigacoes | null>(null);
  const [layout, setLayout] = useState<Record<string, Posicao>>({});
  const [abertos, setAbertos] = useState<Record<string, boolean>>({});
  const [fio, setFio] = useState<FioTirado | null>(null);
  const [arraste, setArraste] = useState<Arraste | null>(null);
  const [erro, setErro] = useState<string | null>(null);
  const [zoom, setZoom] = useState(1);
  /// Enquanto `ajustada` é verdadeira, a lente acompanha o tamanho da moldura.
  const [ajustada, setAjustada] = useState(true);
  const [central, setCentral] = useState(0);

  const mundo = useRef<HTMLDivElement | null>(null);
  const quadro = useRef<HTMLDivElement | null>(null);
  const layoutAtual = useRef<Record<string, Posicao>>({});

  /// Escritas otimistas e releituras da política competem: a resposta de um
  /// `ligacoesAplicar` mais antigo pode chegar depois de um gesto mais novo e
  /// sobrescrever o estado otimista, divergindo do que o núcleo cobra. Cada
  /// pedido recebe um token crescente e só o mais recente escreve `base`.
  const pedido = useRef(0);

  const carregar = useCallback(async () => {
    const token = ++pedido.current;
    try {
      const estado = await api.ligacoesObter();
      if (token !== pedido.current) return;
      setBase(estado);
      const posicoes = Object.fromEntries(estado.posicoes.map((p) => [p.id, p] as const));
      layoutAtual.current = posicoes;
      setLayout(posicoes);
      setErro(null);
    } catch (cause) {
      if (token !== pedido.current) return;
      setErro(message(cause));
    }
  }, []);

  /// O grafo é lido do estado real no mount, na troca de idioma e sempre que um
  /// conector entra ou sai: ligar um servidor tem que mostrar as ferramentas
  /// dele sem pedir nenhum clique.
  useEffect(() => {
    void carregar();
  }, [carregar, idioma, sinal]);

  const fichas = useMemo(
    () => fichasDe(base?.nos ?? [], abertos),
    [base, abertos],
  );
  const mundoHoje = useMemo(() => mundoDe(fichas), [fichas]);

  const efetivas = useMemo(
    () => (base === null ? {} : lugares(fichas, layout, mundoHoje, arraste?.id ?? null)),
    [arraste, base, fichas, layout, mundoHoje],
  );

  const ladoDe = (p: Ponto): Lado =>
    p.x + LARGURA_NO / 2 < mundoHoje.hub.x + mundoHoje.hub.largura / 2
      ? "esquerda"
      : "direita";

  /** Grava e mostra o que o núcleo respondeu. Otimista: o fio aparece no
   *  instante do gesto e, se o núcleo recusar, a recusa volta para a tela e o
   *  desenho é relido do estado real. */
  const gravar = useCallback(
    async (novo: EstadoLigacoes, pastas: string[]) => {
      const token = ++pedido.current;
      try {
        const estado = await api.ligacoesAplicar(planoDe(novo, pastas));
        // Resposta fora de ordem: um gesto mais novo já assumiu o desenho, e
        // este estado velho não pode sobrescrevê-lo.
        if (token !== pedido.current) return;
        setBase(estado);
        setErro(null);
        onChange();
      } catch (cause) {
        if (token !== pedido.current) return;
        setErro(message(cause));
        void carregar();
      }
    },
    [carregar, onChange],
  );

  const alternar = (noId: string, porta: string, ligada: boolean) => {
    if (base === null) return;
    const novo: EstadoLigacoes = {
      ...base,
      nos: base.nos.map((no) =>
        no.id !== noId
          ? no
          : {
              ...no,
              portas: no.portas.map((p) => (p.nome === porta ? { ...p, ligada } : p)),
            },
      ),
    };
    setBase(novo);
    void gravar(novo, base.pastas.map((p) => p.caminho));
  };

  const todos = (no: NoLigacao) => {
    if (base === null || no.portas.length === 0) return;
    const todasLigadas = no.portas.every((p) => p.ligada);
    const novo: EstadoLigacoes = {
      ...base,
      nos: base.nos.map((entrada) =>
        entrada.id !== no.id
          ? entrada
          : {
              ...entrada,
              portas: entrada.portas.map((p) => ({ ...p, ligada: !todasLigadas })),
            },
      ),
    };
    setBase(novo);
    void gravar(novo, base.pastas.map((p) => p.caminho));
  };

  /// Abrir o cartão é o caminho de quem procura uma ferramenta que ainda não
  /// tem fio. Fechado, o nó mostra só as sinapses que o núcleo cobra — a lista
  /// inteira do cadastro é o que fazia o grafo parecer uma folha de especificação.
  const abrir = (ficha: Ficha) => {
    if (!ficha.dobra) return;
    setAbertos((atual) => ({ ...atual, [ficha.no.id]: !ficha.aberta }));
  };

  /// Devolver os nós ao padrão apaga o que foi arrastado. É o remédio de um
  /// grafo embaralhado: posição salva é responsabilidade da pessoa, não do
  /// desenho, e ela pode sobrepor o Aether.
  const reorganizar = () => {
    layoutAtual.current = {};
    setLayout({});
    void api.ligacoesLayout([]).catch((cause) => setErro(message(cause)));
  };

  const cortarPasta = (caminho: string) => {
    if (base === null) return;
    void gravar(
      base,
      base.pastas.filter((p) => p.caminho !== caminho).map((p) => p.caminho),
    );
  };

  /** Coordenada do mundo a partir do ponteiro. A caixa do mundo já vem
   *  transformada pelo zoom, então a razão entre ela e o desenho devolve a
   *  escala real — sem ela o fio nasceria ao lado do cursor em 60% ou 160%. */
  const ponto = (event: React.PointerEvent): Ponto => {
    const caixa = mundo.current?.getBoundingClientRect();
    if (!caixa || caixa.width === 0 || caixa.height === 0) return { x: 0, y: 0 };
    return {
      x: ((event.clientX - caixa.left) * LARGURA) / caixa.width,
      y: ((event.clientY - caixa.top) * mundoHoje.altura) / caixa.height,
    };
  };

  /// O cartão é arrastado pelo quadro, não pela barra que o segura: a mão sai
  /// do cabeçalho no primeiro centímetro e, sem a moldura por trás, o movimento
  /// morria no meio do caminho — o nó ficava torto e nada era gravado.
  const iniciarArraste = (ficha: Ficha) => (event: React.PointerEvent) => {
    const atual = efetivas[ficha.no.id];
    if (atual === undefined) return;
    event.preventDefault();
    quadro.current?.setPointerCapture(event.pointerId);
    const p = ponto(event);
    setArraste({ id: ficha.no.id, dx: p.x - atual.x, dy: p.y - atual.y });
  };

  const moverArraste = (event: React.PointerEvent) => {
    if (arraste === null) return;
    const ficha = fichas.find((f) => f.no.id === arraste.id);
    const atual = ficha === undefined ? undefined : efetivas[ficha.no.id];
    if (ficha === undefined || atual === undefined) return;
    const p = ponto(event);
    const proxima = apertar(
      ficha,
      { id: ficha.no.id, x: p.x - arraste.dx, y: p.y - arraste.dy },
      mundoHoje,
    );
    if (proxima.x === atual.x && proxima.y === atual.y) return;
    layoutAtual.current = { ...layoutAtual.current, [ficha.no.id]: proxima };
    setLayout({ ...layoutAtual.current });
  };

  const soltarArraste = (event: React.PointerEvent) => {
    if (arraste === null) return;
    const ficha = fichas.find((f) => f.no.id === arraste.id);
    setArraste(null);
    quadro.current?.releasePointerCapture(event.pointerId);
    if (ficha === undefined) return;
    // Largar um cartão sobre o Aether ou sobre outro cartão não é uma posição:
    // a escolha não vai para o disco e ele volta para o seu slot. A conta é a
    // mesma do desenho, com a mão fora dela.
    const pedida = layoutAtual.current[ficha.no.id];
    if (pedida !== undefined) {
      const aceita = lugares(fichas, layoutAtual.current, mundoHoje, null)[ficha.no.id];
      if (aceita === undefined || aceita.x !== pedida.x || aceita.y !== pedida.y) {
        delete layoutAtual.current[ficha.no.id];
        setLayout({ ...layoutAtual.current });
      }
    }
    void api.ligacoesLayout(Object.values(layoutAtual.current)).catch((cause) => {
      setErro(message(cause));
    });
  };

  // ── ligar por fio ──────────────────────────────────────────────────────────
  // O fio é puxado da porta e solto no Aether. Enquanto está no ar a ponta
  // segue o cursor e o destino acende. Com a captura ativa é o quadro que
  // recebe a soltura, então o alvo é medido pela conta do desenho — um teste de
  // acerto no DOM nunca veria o hub por trás do ponteiro capturado.

  const puxarFio =
    (ficha: Ficha, indice: number) => (event: React.PointerEvent) => {
      const porta = ficha.portas[indice];
      const p = efetivas[ficha.no.id];
      if (porta === undefined || p === undefined) return;
      event.preventDefault();
      quadro.current?.setPointerCapture(event.pointerId);
      setFio({
        noId: ficha.no.id,
        porta: porta.nome,
        de: portaDoNo(p, indice, ladoDe(p)),
        alvo: null,
      });
    };

  const moverFio = (event: React.PointerEvent) => {
    if (fio === null) return;
    const alvo = ponto(event);
    if (
      fio.alvo !== null &&
      Math.abs(alvo.x - fio.alvo.x) < 1 &&
      Math.abs(alvo.y - fio.alvo.y) < 1
    )
      return;
    setFio({ ...fio, alvo });
  };

  const largarFio = (event: React.PointerEvent) => {
    if (fio === null) return;
    quadro.current?.releasePointerCapture(event.pointerId);
    const alvo = ponto(event);
    const atual = fio;
    setFio(null);
    const entrada = base?.nos
      .find((no) => no.id === atual.noId)
      ?.portas.find((p) => p.nome === atual.porta);
    if (entrada === undefined) return;
    const puxou =
      Math.abs(alvo.x - atual.de.x) > DISTANCIA_TOQUE ||
      Math.abs(alvo.y - atual.de.y) > DISTANCIA_TOQUE;
    // Toque curto: liga ou desliga sem precisar de mira.
    if (!puxou) {
      alternar(atual.noId, atual.porta, !entrada.ligada);
      return;
    }
    if (dentroDe(alvo, mundoHoje.hub) && !entrada.ligada) {
      alternar(atual.noId, atual.porta, true);
    }
  };

  const cancelarFio = () => setFio(null);

  /// Os dois gestos vivem na moldura e se distinguem pelo que está no ar: um
  /// fio puxado de uma porta, ou um cartão segurado pelo cabeçalho.
  const noMovimento = (event: React.PointerEvent) => {
    moverFio(event);
    moverArraste(event);
  };

  const naSoltura = (event: React.PointerEvent) => {
    largarFio(event);
    soltarArraste(event);
  };

  /// Enter e Espaço ligam a porta em foco: nada pode obrigar a pessoa a acertar
  /// um círculo de doze pixels com o mouse para fazer a única coisa que o grafo
  /// faz.
  const peloTeclado =
    (ficha: Ficha, indice: number) => (event: React.KeyboardEvent) => {
      if (event.key !== "Enter" && event.key !== " ") return;
      event.preventDefault();
      const porta = ficha.portas[indice];
      if (porta === undefined) return;
      alternar(ficha.no.id, porta.nome, !porta.ligada);
    };

  /** Os fios nascem do que o núcleo permite, nunca do contrário: o que se vê é
   *  a allowlist que ele vai cobrar no próximo turno. O índice de cada fio na
   *  borda do Aether nasce aqui, depois de o grupo estar ordenado. */
  const fios = useMemo(() => {
    if (base === null) return [];
    type Origem = {
      chave: string;
      x: number;
      y: number;
      lado: Lado;
      efeito: ToolEffect | "pasta";
      rotulo: string;
      cortar: () => void;
    };
    const origens: Origem[] = [];
    for (const ficha of fichas) {
      const p = efetivas[ficha.no.id];
      if (p === undefined) continue;
      const lado = ladoDe(p);
      // O índice é o da lista desenhada: com o cartão fechado, a porta some do
      // nó e o fio que saísse do índice do cadastro nasceria no meio do ar.
      ficha.portas.forEach((porta, indice) => {
        if (!porta.ligada) return;
        const de = portaDoNo(p, indice, lado);
        origens.push({
          chave: porta.chave,
          x: de.x,
          y: de.y,
          lado,
          efeito: porta.efeito,
          rotulo: `${ficha.no.nome} · ${porta.titulo}`,
          cortar: () => alternar(ficha.no.id, porta.nome, false),
        });
      });
    }
    base.pastas.forEach((pasta, indice) => {
      const de = portaDaPasta(indice);
      origens.push({
        chave: `pasta__${pasta.caminho}`,
        x: de.x,
        y: de.y,
        lado: "topo",
        efeito: "pasta",
        rotulo: pasta.nome,
        cortar: () => cortarPasta(pasta.caminho),
      });
    });

    const saida: {
      chave: string;
      d: string;
      efeito: string;
      rotulo: string;
      cortar: () => void;
    }[] = [];
    for (const lado of ["esquerda", "direita", "topo"] as const) {
      const grupo = origens
        .filter((o) => o.lado === lado)
        .sort((a, b) => (lado === "topo" ? a.x - b.x : a.y - b.y));
      grupo.forEach((origem, k) => {
        saida.push({
          chave: origem.chave,
          d: caminhoFio(origem, portaDoAether(k, grupo.length, lado, mundoHoje.hub)),
          efeito: cor(origem.efeito),
          rotulo: origem.rotulo,
          cortar: origem.cortar,
        });
      });
    }
    return saida;
    // `alternar` e `cortarPasta` só leem `base`, que já está na conta.
  }, [base, fichas, efetivas, mundoHoje]);

  const pontaSolta = fio !== null && fio.alvo !== null ? fio.alvo : null;
  const mirando = pontaSolta !== null && dentroDe(pontaSolta, mundoHoje.hub);

  const vazio = base !== null && base.nos.length === 0;
  /// O número do topo é o que o núcleo vai poder chamar no próximo turno — não
  /// o que os servidores anunciam.
  const ligadas = fichas.reduce((total, ficha) => total + ficha.sinapses, 0);

  /// Quem aperta + ou − assumiu a lente: o ajuste automático para ali, porque
  /// quem reduziu de propósito não quer ser trazido de volta a cada fio.
  const ajustarZoom = (novo: number) => {
    setAjustada(false);
    setZoom(limitar(Math.round(novo * 100) / 100, ZOOM_MINIMO, ZOOM_MAXIMO));
  };

  /// Ajustar é encostar a lente na moldura: o mundo inteiro à vista, porque uma
  /// janela que corta metade dos cartões não mostra a ligação de nada. Para de
  /// valer no instante em que alguém mexe no zoom.
  useEffect(() => {
    if (!ajustada) return;
    const caixa = quadro.current;
    if (caixa === null) return;
    setZoom(
      limitar(
        Math.min(1, caixa.clientWidth / LARGURA, caixa.clientHeight / mundoHoje.altura),
        AJUSTE_MINIMO,
        ZOOM_MAXIMO,
      ),
    );
  }, [ajustada, central, mundoHoje]);

  /// Central o Aether na lente atual. A moldura não muda de tamanho com o zoom,
  /// mas a área rolável é a lente: por isso o mundo é multiplicado pela escala
  /// vigente, e não pelo tamanho do instante do clique.
  useEffect(() => {
    const caixa = quadro.current;
    if (caixa === null) return;
    caixa.scrollTo({
      left: Math.max(0, (LARGURA * zoom - caixa.clientWidth) / 2),
      top: limitar(
        mundoHoje.hub.y * zoom + (mundoHoje.hub.altura * zoom) / 2 - caixa.clientHeight / 2,
        0,
        Math.max(0, mundoHoje.altura * zoom - caixa.clientHeight),
      ),
    });
  }, [ajustada, central, zoom, mundoHoje]);

  const enquadrar = () => {
    setAjustada(true);
    setCentral((n) => n + 1);
  };

  return (
    <div className="grafo">
      <div className="grafo-topo">
        <span className="lig-conta">{t("ligacoes.ligadas", { n: ligadas })}</span>
        <span className="grafo-acoes">
          <button
            type="button"
            className="icon-only"
            onClick={onAdicionar}
            aria-label={t("servidores.adicionar")}
            title={t("servidores.adicionar")}
          >
            <Icon name="plug" />
          </button>
          <button
            type="button"
            className="ghost small"
            onClick={reorganizar}
            aria-label={t("ligacoes.reorganizar")}
            title={t("ligacoes.reorganizar")}
          >
            {t("ligacoes.reorganizar")}
          </button>
          <button
            type="button"
            className="icon-only"
            onClick={() => ajustarZoom(zoom - 0.2)}
            aria-label={t("ligacoes.reduzir")}
            title={t("ligacoes.reduzir")}
            disabled={zoom <= ZOOM_MINIMO}
          >
            <span className="grafo-minus" aria-hidden="true" />
          </button>
          <button
            type="button"
            className="ghost small"
            onClick={enquadrar}
            aria-label={t("ligacoes.enquadrar")}
            title={t("ligacoes.enquadrar")}
          >
            {Math.round(zoom * 100)}%
          </button>
          <button
            type="button"
            className="icon-only"
            onClick={() => ajustarZoom(zoom + 0.2)}
            aria-label={t("ligacoes.ampliar")}
            title={t("ligacoes.ampliar")}
            disabled={zoom >= ZOOM_MAXIMO}
          >
            <Icon name="plus" />
          </button>
          <button
            type="button"
            className="icon-only"
            onClick={() => void carregar()}
            aria-label={t("ligacoes.atualizar")}
            title={t("ligacoes.atualizar")}
          >
            <Icon name="refresh" />
          </button>
        </span>
      </div>

      {vazio && <p className="sheet-note">{t("ligacoes.vazio")}</p>}

      <div
        className="grafo-quadro"
        ref={quadro}
        onPointerMove={noMovimento}
        onPointerUp={naSoltura}
        onPointerCancel={(event) => {
          cancelarFio();
          soltarArraste(event);
        }}
      >
        <div
          className="grafo-lente"
          style={{ width: LARGURA * zoom, height: mundoHoje.altura * zoom }}
        >
          <div
            className="grafo-mundo"
            ref={mundo}
            style={{
              width: LARGURA,
              height: mundoHoje.altura,
              transform: `scale(${zoom})`,
              transformOrigin: "top left",
              ...variaveis,
            }}
          >
            <svg
              className="lig-wires"
              width={LARGURA}
              height={mundoHoje.altura}
              aria-hidden="true"
            >
              {fios.map((linha) => {
                const pulso = disparos[linha.chave];
                return (
                  <g key={linha.chave}>
                    <path
                      className="lig-fio-area"
                      d={linha.d}
                      onClick={linha.cortar}
                      role="presentation"
                    >
                      <title>{linha.rotulo}</title>
                    </path>
                    <path
                      key={`v${pulso?.n ?? 0}`}
                      className={`lig-fio lig-fio--${linha.efeito}${
                        pulso === undefined
                          ? ""
                          : pulso.emCurso
                            ? " disparando"
                            : pulso.ok
                              ? " disparada"
                              : " falhou"
                      }`}
                      d={linha.d}
                    >
                      <title>
                        {pulso === undefined
                          ? linha.rotulo
                          : `${linha.rotulo} · ${pulso.ms ?? 0} ms`}
                      </title>
                    </path>
                  </g>
                );
              })}
              {fio !== null && pontaSolta !== null && (
                <path
                  className={`lig-fio lig-fio--novo${mirando ? " alvo" : ""}`}
                  d={caminhoFio(fio.de, pontaSolta)}
                />
              )}
            </svg>

            <div
              className={`lig-no lig-no--aether${mirando ? " alvo" : ""}`}
              style={{
                left: mundoHoje.hub.x,
                top: mundoHoje.hub.y,
                width: mundoHoje.hub.largura,
                height: mundoHoje.hub.altura,
              }}
            >
              <span className="brand-mark" aria-hidden="true" />
              <span className="lig-aether-nome">{t("ligacoes.aether")}</span>
            </div>

            {base?.pastas.map((pasta, indice) => (
              <div
                key={pasta.caminho}
                className="lig-no lig-no--pasta"
                style={{
                  left: pastaX(indice),
                  top: pastaY(indice),
                  width: LARGURA_PASTA,
                  height: ALTURA_PASTA,
                }}
              >
                <span className="lig-ponto" />
                <span className="lig-pasta-nome" title={pasta.caminho}>
                  {pasta.nome}
                </span>
                <button
                  type="button"
                  className="lig-cortar"
                  onClick={() => cortarPasta(pasta.caminho)}
                  aria-label={t("ligacoes.cortar", { nome: pasta.nome })}
                >
                  <Icon name="cross" />
                </button>
              </div>
            ))}

            {fichas.map((ficha) => {
              const p = efetivas[ficha.no.id];
              if (p === undefined) return null;
              const no = ficha.no;
              const lado = ladoDe(p);
              // Com a fileira de dobra logo abaixo, o rodapé de dez pixels já
              // foi gasto: sem tirar da conta, o cartão estouraria a própria
              // borda.
              const rodape =
                ficha.dobra && ficha.portas.length > 0 ? { "--lig-portas-rodape": "0px" } : {};
              return (
                <div
                  key={no.id}
                  className={`lig-no${no.conectado ? " lig-no--on" : " lig-no--ocio"}`}
                  style={
                    {
                      left: p.x,
                      top: p.y,
                      width: LARGURA_NO,
                      ...rodape,
                    } as React.CSSProperties
                  }
                >
                  <div
                    className="lig-cabeca"
                    style={{ height: ALTURA_CABECA }}
                    onPointerDown={iniciarArraste(ficha)}
                  >
                    <span className={`lig-no-estado${no.conectado ? " on" : ""}`} />
                    <span className="lig-no-nome" title={no.descricao}>
                      {no.nome}
                    </span>
                    {ficha.sinapses > 0 && (
                      <span className="lig-no-conta" title={t("ligacoes.sinapses")}>
                        {ficha.sinapses}
                      </span>
                    )}
                    {no.conectado && (
                      <button
                        type="button"
                        className="lig-no-toggle"
                        onPointerDown={(event) => event.stopPropagation()}
                        onClick={() => todos(no)}
                        aria-label={t("ligacoes.todos", { nome: no.nome })}
                        title={t("ligacoes.todos", { nome: no.nome })}
                      >
                        <Icon name="plug" />
                      </button>
                    )}
                  </div>

                  {ficha.portas.length > 0 && (
                    <div className="lig-portas">
                      {ficha.portas.map((porta, indice) => {
                        const pulso = disparos[porta.chave];
                        /// O ponto faz uma coisa só por toque, e o nome dele é essa
                        /// coisa: ligado, ele corta; desligado, ele liga.
                        const gesto = t(
                          porta.ligada ? "ligacoes.cortar" : "ligacoes.ligar",
                          { nome: porta.titulo },
                        );
                        return (
                          <div
                            key={porta.nome}
                            className={`lig-porta lig-porta--${cor(porta.efeito)}${
                              lado === "esquerda" ? " lig-porta--saida" : " lig-porta--entrada"
                            }${porta.ligada ? " on" : ""}`}
                            style={{ height: ALTURA_PORTA }}
                            title={porta.descricao}
                          >
                            <span className="lig-porta-nome">{porta.titulo}</span>
                            <button
                              type="button"
                              className={`lig-ponto${
                                pulso !== undefined && pulso.emCurso ? " pulso" : ""
                              }`}
                              aria-label={gesto}
                              title={gesto}
                              onPointerDown={puxarFio(ficha, indice)}
                              onKeyDown={peloTeclado(ficha, indice)}
                            />
                          </div>
                        );
                      })}
                    </div>
                  )}

                  {!no.conectado && (
                    <button
                      type="button"
                      className="lig-conectar"
                      style={{ height: AVISO_ALTURA }}
                      onClick={() => onConectar(no.id)}
                    >
                      {t("ligacoes.conectar")}
                    </button>
                  )}

                  {no.conectado && no.portas.length === 0 && (
                    <p className="lig-advertencia" style={{ height: AVISO_ALTURA }}>
                      {t("ligacoes.semPortas")}
                    </p>
                  )}

                  {ficha.dobra && (
                    <button
                      type="button"
                      className="lig-dobra"
                      style={{ height: ALTURA_DOBRA }}
                      onClick={() => abrir(ficha)}
                      aria-expanded={ficha.aberta}
                    >
                      {t(ficha.aberta ? "ligacoes.esconder" : "ligacoes.mostrar", {
                        n: ficha.ocultas,
                      })}
                    </button>
                  )}
                </div>
              );
            })}
          </div>
        </div>
      </div>

      {erro !== null && (
        <p className="sheet-error" role="alert">
          <Icon name="warning" />
          {erro}
        </p>
      )}
    </div>
  );
}
