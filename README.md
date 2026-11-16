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
| GPU | llama.cpp pelo backend **Vulkan** | sempre que existe placa Vulkan registrada |
| CPU | implementação própria em candle (`src/ai/granite.rs`) | máquina sem GPU, ou quando a placa recusa a carga |

Vulkan é a porta única para NVIDIA, AMD e Intel — não há SDK de fabricante no
meio. Antes de carregar qualquer modelo, `src/ai/alojamento.rs` lê a VRAM livre
e decide entre carga inteira na placa, camada a camada dividida com a RAM, ou só
sistema; o veredito vira `n_gpu_layers` e é conferido contra o delta real de
VRAM que a carga produz.

Os binários do llama.cpp e do sherpa-onnx não estão no repositório. Veja
[`src-tauri/vendor/README.md`](src-tauri/vendor/README.md).

## O que ele faz

- **Catálogo de modelos** — 30 variantes Granite 4.2 GGUF e 2 quants do
  Qwen3-Embedding 0.6B, com download retomável, cancelamento e estado medido em
  disco. Um arquivo de apoio que chegou como ponteiro Git LFS é tratado como
  ausente, não como instalado.
- **Roteador** — classifica o pedido e escolhe o modelo pela capacidade real da
  placa; a escolha é exibida com o motivo auditável e os sinais que a produziram.
  Um modelo fixado nas Escalas passa na frente do roteador.
- **Laço de ferramentas** — o modelo chama ferramentas MCP e locais, o gate pede
  aprovação quando a ação muda algo, e o resultado volta ao modelo. As duas
  gramáticas que aparecem na prática são lidas: o XML declarado pelo
  `chat_template.jinja` do fabricante e o JSON em que modelos pequenos às vezes
  caem. Uma chamada cortada no meio pelo teto de tokens roda com o que tem
  inteiro; uma chamada com nome que não existe devolve a lista dos nomes reais.
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

Pré-requisitos: Windows 10+, Rust (stable), Node 20+, WebView2 Runtime, e as DLLs
vendorizadas no lugar (link acima).

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

- O modelo de conversa padrão tem 3 bilhões de parâmetros. Ele às vezes traduz o
  nome de uma ferramenta ao chamá-la, e o repositório não finge que isso é
  confiabilidade: o erro devolve os nomes reais e o laço para quando o modelo
  insiste, em vez de queimar a placa.
- Adaptadores LoRA/QLoRA por turno estão em construção; o que existe hoje é o
  catálogo GGUF, o roteador e a montagem de contexto.
- Janela e teto de tokens são escalas do usuário. Quando o pedido não cabe nem
  derrubando o histórico, o aplicativo diz quantos tokens têm o prompt fixo e o
  pedido, em vez de cortar em silêncio.

## Licença e contribuições

Apache-2.0 — veja [LICENSE](LICENSE). Para mexer no código, leia
[CONTRIBUTING.md](CONTRIBUTING.md); os binários vendorizados têm suas próprias
licenças em `src-tauri/vendor/`.
