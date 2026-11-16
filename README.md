# Aether

Assistente de IA que roda inteiro no computador de quem usa. O modelo, o
tokenizador, a memória vetorial e a voz estão na máquina local; não há chamada a
serviço de terceiros em nenhum caminho de execução. O que ele faz no sistema de
arquivos e no terminal passa por um gate de permissão que a pessoa controla.

**Ele não é** um front-end para LM Studio, Ollama ou uma API de nuvem: o motor de
inferência é parte deste repositório, e o aplicativo é o dono da placa.

## Como o modelo roda

| Caminho | O que executa | Quando |
| --- | --- | --- |
| GPU | llama.cpp pelo backend **Vulkan** — qualquer arquitetura que a `llama.dll` vendorizada registre | sempre que existe placa Vulkan registrada |
| CPU | implementação própria em candle (`src-tauri/src/ai/granite.rs`), só granite | máquina sem GPU, ou quando a placa recusa a carga |

Vulkan é a porta única para NVIDIA, AMD e Intel — não há SDK de fabricante no
meio. Antes de carregar qualquer modelo, `src-tauri/src/ai/alojamento.rs` lê a VRAM livre
e decide entre carga inteira na placa, camada a camada dividida com a RAM, ou só
sistema; o veredito vira `n_gpu_layers` e é conferido contra o delta real de
VRAM que a carga produz.

Os binários do llama.cpp e do sherpa-onnx não estão no repositório. Veja
[`src-tauri/vendor/README.md`](src-tauri/vendor/README.md).

## O que ele faz

- **Catálogo de modelos** — 35 variantes GGUF em duas famílias: Granite 4.2
  (3B e 8B, Apache-2.0) e Ornith 1.5 9B (MIT, janela de 262 mil, o modelo de
  programação), mais 2 quants do Qwen3-Embedding 0.6B. Cada tamanho é lido do
  `tree/main` do HuggingFace, arquivo por arquivo, e cada família é conferida
  contra a tabela de arquiteturas da `llama.dll` vendorizada. Download
  retomável, cancelamento e estado medido em disco. Um arquivo de apoio que
  chegou como ponteiro Git LFS é tratado como ausente, não como instalado.
- **Roteador** — classifica o pedido e escolhe o modelo pela capacidade real da
  placa; a escolha é exibida com o motivo auditável e os sinais que a produziram.
  Um modelo fixado nas Escalas passa na frente do roteador.
- **Laço de ferramentas** — o modelo chama ferramentas MCP e locais, o gate pede
  aprovação quando a ação muda algo, e o resultado volta ao modelo. As duas
  gramáticas que aparecem na prática são lidas: o XML declarado pelo template
  que o próprio GGUF publica — é ele que o modelo viu no treino, e o
  `chat_template.jinja` do repositório de pesos pode ser outra versão — e o JSON
  em que modelos pequenos às vezes
  caem. Uma chamada cortada no meio pelo teto de tokens é retomada de onde o
  modelo parou, tantas vezes quantas a janela permitir — o resto do corpo é ele
  quem escreve, e o app não inventa linha;
  se ainda assim faltar um argumento obrigatório, a ferramenta recusa e diz o
  que faltou, em vez de criar um arquivo vazio com a cara de sucesso; uma
  chamada com nome que não existe devolve a lista dos nomes reais.
- **Conector local** — 12 ferramentas de arquivo, terminal, código, memória e
  sistema, dentro das pastas que a pessoa concedeu. Ele entra no mesmo cadastro,
  na mesma allowlist e no mesmo gate que um servidor MCP de terceiros.
- **Memória (RAG)** — índice das pastas concedidas com busca híbrida (BM25 +
  vetorial com fusão), peças com sobreposição e piso de similaridade. A injeção
  automática e a ferramenta `buscar_memoria` obedecem ao mesmo fio do grafo.
- **Voz** — Kokoro para fala, Whisper para escuta, ambos locais.
- **Grafo** — a tela de ligações é a forma de editar a política (quais
  conectores, quais ferramentas, quais pastas). O desenho não é a política: o
  núcleo cobra `mcp.json` e `pastas.json` em cada chamada.
- **Medidas visíveis** — tokens de entrada e saída, tokens/s de prefill e decode,
  tempo no motor, motivo de parada e quantas mensagens ficaram fora da janela.

Todo texto da interface vem de `locales/pt-BR.json` e `locales/en-US.json`, e um
teste de suíte exige que as duas línguas tenham exatamente as mesmas chaves e os
mesmos placeholders.

## Rodar

Pré-requisitos: Windows 10+, Rust (stable), Node 20+ e WebView2 Runtime. As DLLs
vendorizadas (link acima) não são pré-requisito para compilar — sem elas o build
avisa e segue; elas são pré-requisito para a inferência rodar na placa e para o
instalador levá-las.

```sh
npm install
npm run tauri dev
```

A janela de desenvolvimento serve a interface em `http://localhost:5173` (Vite).
Para o pacote:

```sh
npm run build
npm run tauri build
```

## Testar

```sh
cd src-tauri && cargo test --lib     # 355 testes
npm run build                        # tsc + vite
```

Os testes que dependem de rede real (download do HuggingFace) e os que dependem
de placa estão marcados com `#[ignore]` e rodam sob demanda:

```sh
cd src-tauri && cargo test --lib -- --ignored
```

Uma sonda separada carrega pesos reais e mede o motor:

```sh
cd src-tauri && cargo run --bin engine-probe -- <pasta-do-modelo> <arquivo.gguf> [contexto] [max_tokens] [prompt]
```

## Estrutura

```
src/                 interface React + TypeScript
  components/        conversa, catálogo de modelos, conectores, grafo, memória, voz
src-tauri/src/       núcleo Rust
  ai/                motor, catálogo, download, alojamento, roteador, sessão, memória
  mcp/               cliente MCP, catálogo instalável, gate de aprovação
  local/             ferramentas locais e contenção de pastas
  voz/               fala (Kokoro) e escuta (Whisper)
locales/             pt-BR e en-US, a única fonte de texto do produto
src-tauri/vendor/    cabeçalhos, licenças e somas dos binários de terceiros
```

## Estado em disco

O aplicativo grava em `%APPDATA%\ai.aether.core` (ajustes, conectores,
allowlists, pastas concedidas, grafo, voz, RAG) e em
`%LOCALAPPDATA%\ai.aether.core\models` (pesos e índice). Nenhum desses arquivos
sai da máquina.

## Limites conhecidos

- O modelo que o roteador escolhe para uma tarefa média tem 3 bilhões de
  parâmetros. Ele às vezes traduz o nome de uma ferramenta ao chamá-la, e o
  repositório não finge que isso é confiabilidade: o erro devolve os nomes reais
  e o laço para quando o modelo insiste, em vez de queimar a placa. Para
  programação existe o Ornith 1.5 9B no gerenciador de modelos — ele não vem
  instalado, são 5,4 GiB de pesos, e o roteador só o escolhe depois de baixado.
- O caminho de CPU executa só granite, porque é uma implementação própria em
  candle. Numa máquina sem GPU, as outras famílias do catálogo não carregam, e o
  aplicativo diz qual arquitetura pediu o quê em vez de falhar mudo.
- Adaptadores LoRA/QLoRA por turno estão em construção; o que existe hoje é o
  catálogo GGUF, o roteador e a montagem de contexto.
- Janela e teto de tokens são escalas do usuário. Quando o pedido não cabe nem
  derrubando o histórico, o aplicativo diz quantos tokens têm o prompt fixo e o
  pedido, em vez de cortar em silêncio.

## Licença e contribuições

Apache-2.0 — veja [LICENSE](LICENSE). Para mexer no código, leia
[CONTRIBUTING.md](CONTRIBUTING.md); os binários vendorizados têm suas próprias
licenças em `src-tauri/vendor/`.
