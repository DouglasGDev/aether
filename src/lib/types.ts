export type ConnectorKind = "builtin" | "mcp";

export type ConnectorCategory =
  | "system"
  | "files"
  | "web"
  | "development"
  | "data"
  | "productivity";

export type McpAuth = "none" | "token" | "key" | "oauth" | "oauth-client";

export type DesktopOs = "windows" | "macos" | "linux";

export type McpEntry =
  | {
      transport: "stdio";
      command: string;
      args: string[];
      env: Record<string, string>;
    }
  | { transport: "http"; url: string; headers: Record<string, string> };

export interface ConnectorField {
  key: string;
  label: string;
  secret: boolean;
  required: boolean;
  placeholder?: string | null;
}

export interface Connector {
  id: string;
  name: string;
  description: string;
  kind: ConnectorKind;
  category: ConnectorCategory;
  docsUrl: string;
  routes: string[];
  auth: McpAuth;
  entry?: McpEntry | null;
  local: Partial<Record<DesktopOs, McpEntry>>;
  fields: ConnectorField[];
  env: string[];
  toolAllowlist?: string[] | null;
}

export type ToolEffect = "read-only" | "mutating" | "destructive";

export interface ToolInfo {
  qualifiedName: string;
  connectorId: string;
  name: string;
  /// Nome legível no idioma atual, só das ferramentas do computador.
  titulo?: string;
  description: string;
  effect: ToolEffect;
  inputSchema: unknown;
}

export interface GrantedFolder {
  caminho: string;
  nome: string;
}

/** Uma ferramenta visível no grafo: a porta de um nó. */
export interface Porta {
  nome: string;
  /// O nome qualificado que o núcleo cobra em cada chamada
  /// (`conector__ferramenta`). É a identidade do fio e a chave com que o evento
  /// de turno avisa qual sinapse disparou.
  chave: string;
  titulo: string;
  descricao: string;
  efeito: ToolEffect;
  ligada: boolean;
}

/** Um conector no grafo, com tudo o que ele anunciou ao núcleo. */
export interface NoLigacao {
  id: string;
  nome: string;
  descricao: string;
  conectado: boolean;
  portas: Porta[];
}

export interface Posicao {
  id: string;
  x: number;
  y: number;
}

/** O desenho que a tela manda para o núcleo compilar na política real. */
export interface Plano {
  conectores: { id: string; ferramentas: string[] }[];
  pastas: string[];
}

export interface EstadoLigacoes {
  nos: NoLigacao[];
  pastas: GrantedFolder[];
  posicoes: Posicao[];
}

/** Uma linha do catálogo de servidores locais, com o estado real que o
 * aplicativo mede em disco e no gerente de conexões. */
export interface ServidorCatalogo {
  id: string;
  nome: string;
  descricao: string;
  categoria: ConnectorCategory;
  pacote: string;
  versao: string;
  instalada: string | null;
  requerPastas: boolean;
  rotas: string[];
  docsUrl: string;
  instalando: boolean;
  cadastrado: boolean;
  conectado: boolean;
  ferramentas: number;
}

/** O servidor MCP que o próprio aplicativo publica, lido do `Publicador`. */
export interface ServidorMcpEstado {
  ativa: boolean;
  url: string;
  porta: number;
  ferramentas: number;
}

export type ServerEvent =
  | { type: "iniciou"; id: string; instalando: boolean }
  | { type: "linha"; id: string; texto: string }
  | { type: "concluiu"; id: string; versao: string }
  | { type: "removeu"; id: string }
  | { type: "falhou"; id: string; erro: string }
  | { type: "cancelou"; id: string };

export interface ApprovalRequest {
  id: string;
  connectorId: string;
  tool: string;
  arguments: unknown;
}

export interface PendingApproval {
  id: string;
  connectorId: string;
  tool: string;
  effect: ToolEffect;
  compound: boolean;
  ageSeconds: number;
}

export type AgentMode =
  | "plan"
  | "default"
  | "auto-accept"
  | "auto"
  | "full-access";

export type ContentBlock =
  | { type: "text"; text: string }
  | { type: "image"; data: string; mimeType: string }
  | { type: "audio"; data: string; mimeType: string }
  | { type: "resource"; resource: unknown }
  | { type: "resource_link"; uri: string; name?: string };

export interface CallToolResult {
  resultType?: string;
  content: ContentBlock[];
  structuredContent?: unknown;
  isError?: boolean;
}

export type McpEvent =
  | { type: "toolsDiscovered"; connectorId: string; tools: ToolInfo[] }
  | { type: "approvalRequested"; request: ApprovalRequest }
  | { type: "approvalSettled"; id: string; approved: boolean }
  | { type: "disconnected"; connectorId: string };

export type Tier = "leve" | "pesado";

export type Complexity = "trivial" | "media" | "alta" | "critica";

export type StopReason =
  | "eos"
  | "stopSequence"
  | "maxTokens"
  | "cancelled"
  | "contextFull";

export interface ModelStatus {
  id: string;
  display: string;
  series: string;
  tier: Tier;
  parametersB: number;
  license: string;
  quant: string;
  routable: boolean;
  contextLimit: number;
  weightsBytes: number;
  downloadedBytes: number;
  partialBytes: number;
  complete: boolean;
  missingSupport: string[];
  dir: string;
}

/// Onde os bytes do modelo ficam, calculado com o cabeçalho do GGUF e a VRAM
/// livre medida antes da carga. Os nomes vêm do serde do núcleo, que pineia as
/// três formas em `alojamento.rs`.
export type Alojamento =
  | { tipo: "inteira"; pesos: number; kv: number; livre: number }
  | {
      tipo: "dividida";
      camadas: number;
      total: number;
      naPlaca: number;
      emRam: number;
      livre: number;
    }
  | { tipo: "soSistema"; pesos: number; kv: number };

export interface Alojado {
  previsto: Alojamento;
  /// Bytes de VRAM que sumiram do livre entre antes e depois da carga.
  cedido: number;
}

export interface Acelerador {
  /** "gpu" ou "cpu" — o que foi usado, não o que foi pedido. */
  tipo: "gpu" | "cpu";
  /** Nome que a placa se deu no registro Vulkan; vazio na CPU. */
  dispositivo: string;
  /** Por que a placa não serviu, quando ela não serviu. */
  motivo: string | null;
  /** Onde os bytes ficaram; `null` só na CPU, onde não há placa. */
  alojado: Alojado | null;
  /**
   * A maior janela de potência de dois em que o modelo ainda iria inteiro para a
   * placa, medida com a VRAM livre desta máquina. `null` é a CPU, ou o modelo
   * que não cabe inteiro em janela nenhuma.
   */
  janelaInteira: number | null;
}

/// As escalas que o dono da máquina escolheu em `geracao.json`. Cada uma delas
/// é lida pelo núcleo no momento em que o turno é montado.
export interface AjustesGeracao {
  /// Id do modelo fixado; vazio é o roteador quem decide.
  modelo: string;
  /// 0 é ganoso: sem sorteio.
  temperatura: number;
  /// 0 desliga o corte por top-k.
  topK: number;
  /// 1 desliga o top-p.
  topP: number;
  /// 1 desliga a penalidade de repetição.
  penalidade: number;
  janelaRepeticao: number;
  maxTokens: number;
  /// 0 sorteia a semente a cada turno.
  semente: number;
  contexto: number;
  pensamento: boolean;
  rodadas: number;
}

/// Um pacote do runtime nativo — as DLLs de que o motor e a voz dependem — nas
/// pastas que o carregador procura. `completo` exige UMA pasta com o conjunto
/// inteiro: a ggml.dll abre os próprios backends da pasta de onde foi carregada.
export interface RuntimePacote {
  id: string;
  nome: string;
  completo: boolean;
  /// A pasta que atendeu ao conjunto, ou `null` quando nenhuma atendeu.
  pasta: string | null;
  faltando: string[];
  arquivos: string[];
  /// Tamanho exato do pacote oficial, conferido no fim do download.
  bytesPacote: number;
}

/// Um arquivo conferido contra o SHA256SUMS.txt versionado. Presença não é
/// integridade: `bateu` só é verdadeiro quando o hash medido é o esperado.
export interface RuntimeConferido {
  nome: string;
  presente: boolean;
  bateu: boolean;
  medido: string;
  esperado: string;
}

export interface RuntimeVeredito {
  id: string;
  conferido: boolean;
  arquivos: RuntimeConferido[];
}

export type RuntimeEvento =
  | { type: "iniciou"; id: string; totalBytes: number }
  | {
      type: "progresso";
      id: string;
      doneBytes: number;
      totalBytes: number;
      bytesPorSegundo: number;
    }
  | { type: "extraindo"; id: string }
  | { type: "conferindo"; id: string }
  | { type: "concluiu"; id: string }
  | { type: "falhou"; id: string; erro: string }
  | { type: "cancelou"; id: string };

/// O veredito de um caso da bancada. `naoRodado` não é meio aprovado: é o caso
/// que não tinha como rodar nesta máquina.
export type BancadaEstado = "aprovado" | "reprovado" | "naoRodado";

export interface BancadaMedida {
  /// Chave de i18n: o motor devolve o valor, a tela traduz o rótulo.
  rotulo: string;
  valor: string;
}

export interface BancadaResultado {
  id: string;
  estado: BancadaEstado;
  medidas: BancadaMedida[];
  detalhe: string | null;
  ms: number;
}

export type BancadaEvento =
  | { type: "iniciou"; total: number; modelo: string | null }
  | { type: "caso"; id: string }
  | ({ type: "resultado" } & BancadaResultado);

export interface AiStatus {
  models: ModelStatus[];
  loaded: string | null;
  acelerador: Acelerador | null;
  ajustes: AjustesGeracao;
  /// A janela que o backend aceitou para o modelo em memória; `null` sem modelo.
  janela: number | null;
  busy: boolean;
  turns: number;
  downloading: string[];
  runtime: RuntimePacote[];
}

export type ModelEvent =
  | { type: "started"; id: string; totalBytes: number }
  | { type: "support"; id: string; file: string }
  | {
      type: "progress";
      id: string;
      doneBytes: number;
      totalBytes: number;
      bytesPerSecond: number;
    }
  | { type: "done"; id: string }
  | { type: "failed"; id: string; error: string }
  | { type: "cancelled"; id: string };

export interface Choice {
  modelId: string;
  display: string;
  complexity: Complexity;
  reason: string;
  signals: string[];
}

export interface ToolCall {
  name: string;
  arguments: unknown;
}

/// O Rust serializa ChatMessage sem rename_all: tool_calls fica em snake_case.
export interface ChatMessage {
  role: string;
  content: string;
  tool_calls?: ToolCall[];
}

/// A ficha de uma conversa salva: o que a lista mostra. O histórico fica no
/// arquivo dela e só entra na tela quando a conversa é aberta.
export interface FichaConversa {
  id: string;
  /// Vazio até o primeiro pedido — ou até a pessoa nomear.
  titulo: string;
  criada: number;
  atualizada: number;
  mensagens: number;
}

export interface EstadoConversas {
  ativa: string | null;
  conversas: FichaConversa[];
}

/// O que o turno mediu. Vem do motor, soma das rodadas.
export interface Medicao {
  promptTokens: number;
  /// Tokens do prompt que o KV cache já tinha: não passaram pela placa nesta
  /// rodada. Zero é um prefill inteiro.
  reusedTokens: number;
  generatedTokens: number;
  promptMs: number;
  decodeMs: number;
  /// Tokens por segundo na decodificação: o ritmo que a pessoa percebe.
  tokensPorSegundo: number;
  /// Tokens por segundo na leitura do prompt.
  prefillTokensPorSegundo: number;
  stop: StopReason;
  /// Mensagens que entraram no prompt como resumo em vez de como texto. Zero é
  /// a conversa inteira ao pé da letra.
  dobradas: number;
}

/// O resumo que um turno deixou para a conversa, e até onde ele cobre.
export interface Resumo {
  texto: string;
  ate: number;
}

export interface TurnOutcome {
  text: string;
  thinking: string;
  rounds: number;
  medicao: Medicao;
  resumo: Resumo | null;
}

export type ChatEvent =
  | { type: "choice"; choice: Choice }
  | { type: "loading"; model: string }
  | { type: "thinking"; text: string }
  | { type: "delta"; text: string }
  | { type: "replace"; text: string }
  | {
      /// O modelo dobrou os turnos antigos no resumo para caber na janela. Não é
      /// rodapé: é o assistente dizendo que parou de ler aquilo ao pé da letra.
      type: "resumo";
      texto: string;
      mensagens: number;
      tokens: number;
    }
  | { type: "toolCall"; name: string; arguments: unknown }
  | {
      type: "toolResult";
      name: string;
      ok: boolean;
      text: string;
      vista: Vista | null;
      /// Milissegundos de execução da ferramenta: o tempo do fio no grafo.
      ms: number;
    }
  | {
      /// O bloco de memória que entrou no prompt antes do primeiro token. Não
      /// é uma ferramenta que o modelo pediu: é o que a casa sabia.
      type: "memoria";
      /// O nome qualificado da sinapse que autorizou a injeção — a mesma
      /// identidade que o fio tem no desenho do grafo.
      ferramenta: string;
      consulta: string;
      achados: TrechoMemoria[];
      /// O modo que ordenou esta lista, em palavra de máquina.
      modo: ModoMemoria;
      /// Quantos trechos entraram na ordem que produziu a lista, quando o `k`
      /// cortou o resto.
      total: number;
      ms: number;
    }
  | { type: "done"; text: string; rounds: number; medicao: Medicao };

export interface VistaEntrada {
  nome: string;
  pasta: boolean;
  bytes: number;
  /// Milissegundos desde 1970; formatado na data e no idioma do usuário.
  modificado: number | null;
}

export interface VistaAchado {
  caminho: string;
  nome: string;
  linha: number;
  texto: string;
}

/// Um trecho que a memória devolveu, com a medida que o classificou. É o mesmo
/// dado que sai da ferramenta `buscar_memoria` e do bloco injetado no prompt.
export interface TrechoMemoria {
  /// Caminho completo, como está no índice.
  arquivo: string;
  nome: string;
  linha: number;
  texto: string;
  /// Cosseno medido entre o vetor da consulta e o do trecho, de 0 a 1. É a
  /// mesma medida em qualquer modo: o que o modo muda é a ordem, não o número.
  pontuacao: number;
  /// O que trouxe o trecho: o vetor, a palavra escrita ou os dois.
  origem: OrigemMemoria;
  /// De onde ele saiu: arquivo concedido ou conversa salva.
  fonte: FonteMemoria;
  /// Instante da última fala da conversa, em milissegundos. Ausente no arquivo:
  /// a data que importa ali é a do trecho, e o índice não a tem.
  quando?: number;
}

export interface VistaTroca {
  antes: string;
  depois: string;
  linha: number;
}

export interface VistaPar {
  rotulo: string;
  valor: string;
  /// Quando a linha é uma medida, o número cru chega junto para a interface
  /// formatar com o separador decimal do idioma escolhido.
  numero?: number;
  unidade?: string;
}

/// Um trecho do cartão de memória: mesmo conteúdo do `TrechoMemoria` do
/// índice, com o caminho no nome que o protocolo dos cartões usa.
export interface VistaMemoria {
  caminho: string;
  nome: string;
  linha: number;
  texto: string;
  pontuacao: number;
  /// O que trouxe o trecho: o vetor, a palavra escrita ou os dois.
  origem: OrigemMemoria;
  /// Arquivo concedido ou conversa salva: muda como o cartão cita a origem.
  fonte: FonteMemoria;
  /// Instante da última fala da conversa, em milissegundos.
  quando?: number;
}

/// O cartão que o computador desenha no lugar de um despejo técnico. Vem sempre
/// do resultado real da ferramenta; uma ferramenta de servidor não tem vista.
export type Vista =
  | {
      type: "arquivo";
      caminho: string;
      nome: string;
      linhas: number;
      bytes: number;
      previa: string;
      linguagem: string;
      inicio: number;
      truncado: boolean;
    }
  | {
      type: "lista";
      caminho: string;
      entradas: VistaEntrada[];
      total: number;
      truncado: boolean;
    }
  | {
      type: "busca";
      termo: string;
      caminho: string;
      achados: VistaAchado[];
      total: number;
      truncado: boolean;
    }
  | {
      type: "memoria";
      consulta: string;
      achados: VistaMemoria[];
      /// O modo que ordenou a lista, em palavra de máquina.
      modo: ModoMemoria;
      /// Quantos trechos entraram na ordem; é o que permite escrever "e mais N".
      total: number;
      truncado: boolean;
    }
  | {
      type: "escrita";
      caminho: string;
      nome: string;
      bytes: number;
      linhas: number;
      criado: boolean;
    }
  | { type: "edicao"; caminho: string; nome: string; trocas: VistaTroca[]; aplicadas: number }
  | { type: "pasta"; caminho: string; nome: string }
  | { type: "movido"; origem: string; destino: string; nome: string }
  | { type: "apagado"; caminho: string; nome: string; pasta: boolean }
  | {
      type: "comando";
      comando: string;
      pasta: string;
      codigo: number | null;
      saida: string;
      erro: string;
      duracaoMs: number;
      estourou: boolean;
      truncado: boolean;
    }
  | { type: "sistema"; linhas: VistaPar[] }
  | { type: "bancoConexoes"; itens: ConexaoCartao[] }
  | { type: "bancoFontes"; conexao: string; itens: FonteCartao[] }
  | { type: "bancoEsquema"; conexao: string; fonte: string; colunas: ColunaCartao[] }
  | {
      type: "bancoConsulta";
      conexao: string;
      fonte: string;
      colunas: string[];
      linhas: unknown[][];
      total: number;
      truncado: boolean;
      duracaoMs: number;
    }
  | {
      type: "bancoLinhas";
      conexao: string;
      fonte?: string;
      operacao: string;
      linhas: number;
      duracaoMs: number;
    };

/** Um conector de banco cadastrado, como o núcleo devolve: a cifra nunca
 * atravessa a fronteira de volta, só o aviso de que existe. */
export interface ConexaoBanco {
  id: string;
  nome: string;
  tipo: string;
  host: string;
  porta: number | null;
  usuario: string;
  banco: string;
  arquivo: string;
  temSenha: boolean;
  limiteLinhas: number;
  sqlLivre: boolean;
  fontes: Record<string, FontePolitica>;
}

/** O que atravessa para o núcleo ao salvar: a senha vai num campo à parte e
 * morre na cifra DPAPI. */
export interface ConexaoRegistro {
  id: string;
  nome: string;
  tipo: string;
  host: string;
  porta: number | null;
  usuario: string;
  banco: string;
  arquivo: string;
  limiteLinhas: number;
  sqlLivre: boolean;
  fontes: Record<string, FontePolitica>;
}

export interface FontePolitica {
  ler: boolean;
  inserir: boolean;
  atualizar: boolean;
  apagar: boolean;
}

export interface ConexaoCartao {
  nome: string;
  tipo: string;
  destino: string;
  fontes: number;
  limiteLinhas: number;
  sqlLivre: boolean;
}

export interface FonteCartao {
  nome: string;
  ler: boolean;
  inserir: boolean;
  atualizar: boolean;
  apagar: boolean;
}

export interface ColunaCartao {
  nome: string;
  tipo: string;
}

/** Para que serve um modelo de voz: fala (texto → áudio) ou escuta (microfone
 * → texto). Vem do catálogo Rust, que é quem sabe de cada entrada. */
export type ServicoVoz = "fala" | "escuta";

/// O alto-falante que o Kokoro anuncia, com o gênero já classificado por ele.
export type GeneroVoz = "feminina" | "masculina";

export interface VozInfo {
  sid: number;
  nome: string;
  genero: GeneroVoz;
}

export interface StatusVoz {
  id: string;
  nome: string;
  servico: ServicoVoz;
  bytes: number;
  baixado: boolean;
  faltando: string[];
}

export interface AjustesVoz {
  autoFalar: boolean;
  modeloFala: string;
  sid: number;
  velocidade: number;
  modeloEscuta: string;
  entrada: string;
}

/// Os aparelhos que o sistema abriu quando o aplicativo ligou a voz.
export interface DispositivosVoz {
  saida: string | null;
  entrada: string | null;
  entradas: string[];
}

export interface EstadoVoz {
  disponivel: boolean;
  motivo: string | null;
  ajustes: AjustesVoz;
  modelos: StatusVoz[];
  vozes: VozInfo[];
  falando: boolean;
  ouvindo: boolean;
  nivel: number;
  erro: string | null;
  dispositivos: DispositivosVoz;
  baixando: string[];
}

/// O pulso lido a cada quadro: três atômicos do motor, nenhum acesso a disco.
export interface MedidorVoz {
  falando: boolean;
  ouvindo: boolean;
  nivel: number;
}

export type VozEvento =
  | { type: "iniciou"; id: string; totalBytes: number }
  | {
      type: "progresso";
      id: string;
      doneBytes: number;
      totalBytes: number;
      bytesPorSegundo: number;
    }
  | { type: "extraindo"; id: string }
  | { type: "concluiu"; id: string }
  | { type: "falhou"; id: string; erro: string }
  | { type: "cancelou"; id: string };

/// Como a busca ordena os trechos. Palavra de máquina, como o núcleo a escreve
/// em `rag.json`; a tela traduz.
export type ModoMemoria = "vetorial" | "hibrido" | "lexico";

/// O que trouxe um achado: a proximidade entre vetores, a palavra escrita ou os
/// dois caminhos na fusão.
export type OrigemMemoria = "vetorial" | "lexica" | "ambas";

/// De onde saiu um trecho: um arquivo das pastas concedidas ou uma conversa
/// salva. As duas fontes se citam de jeitos diferentes — caminho e linha contra
/// conversa e fala — e a tela não pode usar uma só etiqueta para as duas.
export type FonteMemoria = "arquivo" | "conversa";

/// O que a tela da memória edita, e que vale em `rag.json`.
export interface AjustesMemoria {
  /// Id do codificador; vazio é o primeiro completo em disco.
  modelo: string;
  /// Quantos trechos entram no prompt a cada pedido. 0 desliga a injeção.
  trechos: number;
  /// Tokens-alvo de uma peça do índice.
  peca: number;
  /// Tokens da peça anterior que reabrem a seguinte. O teto é metade da peça.
  sobreposicao: number;
  /// Piso de similaridade de cosseno, de 0 a 1, abaixo do qual o trecho fica fora.
  piso: number;
  /// Como a busca ordena.
  modo: ModoMemoria;
}

/// O andamento da varredura, medido arquivo a arquivo.
export interface ProgressoMemoria {
  concluidos: number;
  total: number;
  arquivo: string;
  trechos: number;
}

/// Por que a injeção automática está ou não valendo. Máquina: a tela traduz.
export type AutoMemoria =
  | "semFio"
  | "trechosZero"
  | "semModelo"
  | "indiceVazio"
  | "fatiamentoNovo"
  | "ligada";

export interface ResumoMemoria {
  arquivos: number;
  relidos: number;
  trechos: number;
  /// Conversas que ficaram lembradas nesta varredura.
  conversas: number;
  ms: number;
  truncado: boolean;
  cancelado: boolean;
}

/// Tudo que o núcleo sabe sobre a memória agora. Cada número aqui é lido do
/// motor ou do disco no instante da chamada.
export interface EstadoMemoria {
  escolhido: string;
  modelo: string;
  instalado: boolean;
  carregado: boolean;
  dispositivo: string;
  naPlaca: boolean;
  agrupamento: string;
  dimensao: number;
  /// Tokens que cabem numa peça só — o teto real do lote.
  lote: number;
  peca: number;
  sobreposicao: number;
  trechos: number;
  piso: number;
  /// O modo que a busca está usando agora.
  modo: ModoMemoria;
  indexados: number;
  arquivos: number;
  /// Conversas salvas que hoje têm trechos no índice.
  conversas: number;
  /// Dos `indexados`, quantos saíram de conversa e não de arquivo.
  trechosConversas: number;
  bytes: number;
  indexando: boolean;
  progresso: ProgressoMemoria;
  erro: string | null;
  concluidoEm: number | null;
  auto: AutoMemoria;
  /// As quants do codificador, com o estado medido em disco.
  codificadores: ModelStatus[];
}

/// O que uma busca devolve: os melhores trechos na ordem do modo escolhido e
/// quantos entraram nas listas — o segundo número é o que diz se a lista parou
/// por acabar ou por cortar no `k`.
export interface RespostaMemoria {
  achados: TrechoMemoria[];
  alcancados: number;
  /// O modo que produziu esta lista.
  modo: ModoMemoria;
}

export type MemoriaEvento =
  | {
      type: "progresso";
      arquivo: string;
      concluidos: number;
      total: number;
      trechos: number;
    }
  | { type: "concluido"; resumo: ResumoMemoria }
  | { type: "cancelado"; resumo: ResumoMemoria }
  | { type: "falhou"; erro: string };

/// O que um arquivo de adaptador declara no próprio cabeçalho GGUF. Nada aqui é
/// catálogo: nome, tipo, base e contagem de deltas saem do arquivo, e o tamanho
/// é o que o disco respondeu.
export interface FichaAdaptador {
  caminho: string;
  nome: string;
  tipo: string;
  base: string;
  arquitetura: string;
  tensores: number;
  /// `true` quando o arquivo traz magnitudes DoRA junto dos deltas. É o que o
  /// cabeçalho dele declara — não uma promessa da biblioteca, que recusa tensor
  /// que não seja `.lora_a`/`.lora_b`.
  dora: boolean;
  /// `true` quando o arquivo declara tokens de invocação (a chave alora do
  /// cabeçalho): especialista que só age onde o token dele aparece.
  invocado: boolean;
  bytes: number;
}

/// A linha da tela: o arquivo, a escolha do dono e o que o motor fez com ela.
/// As três coisas são diferentes e a interface mostra cada uma no seu lugar.
export interface ItemAdaptador {
  ficha: FichaAdaptador;
  ativo: boolean;
  forca: number;
  aplicada: boolean;
  erro: string | null;
}

/// Um pin de `adaptadores.json`.
export interface PinoAdaptador {
  caminho: string;
  forca: number;
  ativo: boolean;
}

export interface AjustesAdaptadores {
  pinos: PinoAdaptador[];
}

export interface EstadoAdaptadores {
  pasta: string;
  /// O modelo em memória, sobre o qual os deltas agem. Sem ele, nada vale ainda.
  modelo: string | null;
  naPlaca: boolean;
  itens: ItemAdaptador[];
}