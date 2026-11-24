# FFI — camada do motor llama.cpp (task #93)

Itens: **#39** (despacho do embutidor medido, não prescrito), **A9** (dead code
`Motor::versao` / `Ggml::versao`), **A8** (o `expect` do pino das DLLs).

Dono dos arquivos do lote: só `src-tauri/src/ai/ggml/`. Nada fora deste diretório
foi editado pelo lote; o que precisava de edição fora está em *Follow-ups* no fim
— e a continuação do maintainer, logo abaixo do #39, é justamente essa edição
fora, feita depois do lote.

## Linha de base medida antes do lote

- `cd src-tauri && cargo test --lib ggml` → 10 passed, 506 filtered out (516 no
  total da suíte). Lido em árvore limpa, com `state.rs` de outro agente quebrado
  em um dos ciclos — ver *O que eu não pude medir*.
- Árvore no início: `git status --short` vazio (HEAD `b02a397`).

## #39 — o `-1` do codificador saiu do código e entrou na conta

### O defeito, relido

`Embutidor::novo` chamava `ggml.params_modelo(ffi::CAMADAS_NA_GPU)` — o sentinela
do llama.cpp para "todas as camadas" — com o comentário de que "o embutidor vai
inteiro para a placa: são dezenas de megabytes, e a conta de dividir é do LLM,
não dele" (mod.rs:511-514 na HEAD). O veredito medido já existia para o LLM
(`engine.rs:391-394` usa `alojamento::despacho`, com `CAMADAS_NA_GPU` apenas como
saída de "não houve medida"). O codificador não passava por nada disso: o número
era uma prescrição do arquivo, e a frase "dezenas de megabytes" nunca foi lida do
GGUF que está em disco — medido aqui, ele tem 604 MiB de pesos (ver *Medição do
arquivo real*), dez vezes mais do que o comentário sugeria.

### O que mudou

- `src-tauri/src/ai/ggml/mod.rs:446-461` — `pub struct Despacho { camadas_gpu,
  previsto, motivo }`. É o espelho do par `alojado` + `motivo` do
  `Acelerador` do caminho de conversa: o número, o veredito que o contou e,
  quando não houve veredito, o motivo real.
- `src-tauri/src/ai/ggml/mod.rs:463-516` — `fn despachar_codificador(ficha,
  capacidade, erro_da_ficha, janela) -> Despacho`. Chamada pura: `alojamento::
  fichar` já fez o disco, `alojamento::alojar` + `alojamento::despacho` já existem
  para o LLM. As três saídas:
  - ficha + placa → `despacho(alojar(ficha, Some(placa), janela))`. `-1` volta
    **quando o veredito diz `Inteira`**, um número volta quando diz `Dividida`,
    `motivo` é `None` nos dois casos porque a conta aconteceu;
  - placa, sem ficha → `-1` **com o erro real de `fichar` no `motivo`** (a mesma
    decisão documentada de `engine.rs:386-394`: recusar a carga porque o parser
    não fechou deixaria a máquina sem memória nenhuma; o que não pode é calar);
  - sem placa → `0`, o mesmo `despacho` que `SoSistema` já devolve para o LLM. O
    device escolhido por `device_de_embutir` é o de CPU e não há placa para
    receber camada nenhuma; o `na_placa: false` existente é quem diz isso à tela.
- `src-tauri/src/ai/ggml/mod.rs:560-583` — `Embutidor` ganhou `pub despacho:
  Despacho`.
- `src-tauri/src/ai/ggml/mod.rs:600-627` — `Embutidor::novo`: lê o cabeçalho com
  `alojamento::fichar`, monta a `Capacidade` com o **mesmo** `ggml::escolher` que
  escolheu o device da carga (uma placa só, uma opinião só sobre a mesma
  memória), chama `despachar_codificador` e entrega `despacho.camadas_gpu` a
  `ggml.params_modelo`. O `-1` fixo saiu deste arquivo.
- `src-tauri/src/ai/ggml/ffi.rs:50-58` — o comentário de `CAMADAS_NA_GPU`, que
  afirmava "o embutidor, dezenas de megabytes, vai inteiro", foi reescrito: a
  constante agora documenta os dois caminhos medidos e a única situação em que o
  sentinela é escolha (sem veredito possível, com o motivo junto).

Leitura do disco: `fichar` pára no cabeçalho e nas fichas de tensor. Nenhum peso
é movido, nenhum contexto é criado, a placa não é tocada. Nada aqui rodou carga
real — ver *O que eu não pude medir*.

### Vermelho-primeiro

Os quatro testes de decisão chamam `despachar_codificador`, que não existia antes
deste lote; contra a árvore da HEAD eles não compilam. A falha medida abaixo é o
comportamento antigo reproduzido no corpo da função (`-1` sempre, veredito `None`,
motivo `None`) — exatamente o que `Embutidor::novo` passava antes — para que o
assert falhe com o sintoma real, não com um erro de digitação.

Comandos e saídas: ver *Medições do lote* abaixo.

### Medição do arquivo real

Teste `o_arquivo_real_do_codificador_sobe_inteiro_por_conta` (mod.rs:967-1023):
lê o cabeçalho do GGUF do catálogo que está em disco
(`%LOCALAPPDATA%\ai.aether.core\models\qwen3-embedding-0.6b\Qwen3-Embedding-0.6B-Q8_0.gguf`,
caminho montado por `catalog::find_embeddings` + `catalog::model_dir`, não
escrito à mão), procura **na conta** o menor `livre` que ainda dá `Inteira` (a
busca é binária porque `alojar` é monotônico no livre; o número que ela devolve
inclui a `FOLGA_DE_COMPUTACAO` que é privada a `alojamento.rs`, então nada aqui é
aritmética de comentário) e confere os dois lados do corte. Saída medida:

PENDENTE — colar `cargo test --lib ggml::tests::o_arquivo_real -- --nocapture`
depois que a árvore-compilar (ver *O que eu não pude medir*).

## A9 — `Motor::versao` e `Ggml::versao` removidos

Prova de que eram dead code, lida da HEAD antes de mexer:

```
$ cd "C:/Aether AI" && git grep -n "\.versao()" HEAD -- src-tauri/src
HEAD:src-tauri/src/ai/ggml/mod.rs:413:        self.ggml.versao()
```

O único chamador de `.versao()` no crate inteiro era o corpo de `Motor::versao`,
que é o outro lado do par. `llama_version` aparecia só no registro do símbolo
(`ffi.rs:309` na HEAD) e no cabeçalho vendorizado. Nada em `engine.rs`,
`state.rs`, `commands.rs`, `bancada.rs`, `memoria.rs` ou `src/bin/engine_probe.rs`
lê a versão da biblioteca; a interface (`src/`) não chama Rust diretamente.

Liguei a um consumidor? Não, e o motivo é de posse, não de gosto: o lugar onde o
usuário vê a carga do motor é `Acelerador` (`engine.rs:258-281`) no caminho de
conversa e `InfoMotor` → `Estado` (`memoria.rs:1537-1543`, `memoria.rs:1483-1516`)
no caminho de memória. Os três arquivos estão fora da minha posse neste lote, e a
regra do task é não tocar neles. Sem um consumidor dentro de `ai/ggml/`, a
alternativa honesta era remover. Feito:

- `ffi.rs`: campo `versao: unsafe extern "C" fn() -> *const c_char` da `struct
  Ggml`, a linha `versao: simbolo(&llama, "llama_version")?,` em `Ggml::abrir` e
  o método `Ggml::versao` — os três fora.
- `mod.rs`: `Motor::versao` fora.

Prova depois:

```
$ grep -rn "versao" src-tauri/src/ai/ggml/
(sem resposta)
```

Consequência colateral honesta: `Ggml::abrir` deixou de exigir `llama_version` da
DLL. Se a versão da biblioteca um dia aparecer na tela, o caminho está em
*Follow-ups* — e ele volta com consumidor, não como função solta.

## A8 — o pino das DLLs devolve o erro do carregador

O `expect` de `ffi.rs:269-271` na HEAD (`"llama.dll já abriu nesta chamada"`) não
estava protegendo um `Mutex`: estava dentro do fechamento de um
`OnceLock::get_or_init`. Isso piora o defeito em vez de atenuá-lo:

1. o `expect` derruba o processo no meio de `Ggml::abrir`, que devolve
   `Result<_, String>` e tinha um erro real em mãos para devolver; e
2. um pânico dentro de `get_or_init` **envenena** o `OnceLock` (`std`): a partir
   dali todo `Ggml::abrir` do processo panica na mensagem do padrão
   ("OnceLock instance failed to initialize"), e a falha transitória do
   carregador vira um motor quebrado até o fim da sessão.

O que o guard protege, lido do próprio módulo (ffi.rs:246-260): manter os três
mapeamentos acima de zero para que o `set_terminate` pendurado pela
`ggml-base.dll` da release b12387 nunca aponte para código descarregado
(ggml-org/llama.cpp#15752, `0xC0000409`). Estado envenenado não existe aqui — o
estado é o *contador de referências do carregador*, e ele só precisa não cair a
zero. Logo, `into_inner` não tem o que consertar: o que a realidade permite é
(a) devolver o erro real e (b) não deixar o descarte recriar o ponteiro pendurado.

Feito:

- `ffi.rs:262-301` — `segurar_no_processo` devolve `Result<(), String>`. Sai de
  dentro de `get_or_init`: abre as três bibliotecas, devolve o erro com a chave
  existente `erros.motorDllNaoAbriu` (mesmo texto que `Ggml::abrir` já usa para o
  mesmo evento, com `pino de {rotulo}: {erro}` no `{detalhe}`) e só faz
  `PIN.set(pin)` no fim. `set` não panic, então o `OnceLock` nunca entra em
  estado de inicialização falhada. Atalho no topo (`PIN.get().is_some()`) preserva
  o comportamento de antes: prendido uma vez, nada reabre.
- `ffi.rs:329-341` — em `Ggml::abrir`, se o pino falhar os três handles da própria
  chamada são esquecidos (`std::mem::forget`) antes do `return Err(motivo)`. Sem
  isso, o caminho de erro que eu mesmo acabei de criar descarregaria os módulos
  que o pino existe para nunca descarregar: o `Drop` dos três `Library` levaria o
  contador a zero, o handler continuaria pendurado, e a próxima abertura abortaria
  com `0xC0000409` — o defeito original reintrozuído pela correção. O erro real
  vai para a tela e o estado segura continua seguro.

Vermelho-primeiro: `prender_dlls_devolve_o_motivo_e_nao_envenena_o_processo`
(`ffi.rs:748-789`). Rodado contra o corpo antigo (mesma assinatura, mesmo
`get_or_init` + `expect` de antes) ele falha com o pânico medido; contra o corpo
novo passa, e a segunda chamada devolve o erro do carregador de novo em vez de
panicar. Comandos e saídas em *Medições do lote*.

Sem chave i18n nova: o motivo reusa `erros.motorDllNaoAbriu`, que já está nos dois
idiomas.

## #39 (continuação, maintainer) — o veredito do codificador chega à tela

O item 1 dos *Follow-ups* abaixo, fechado. Não é edição deste diretório: o
`Despacho` nasce aqui, mas quem o escondeia é `ai/memoria.rs` e a frente.

**Defeito (lido).** #39 pôs no `Embutidor` o `despacho` com as três coisas que
importam — o `n_gpu_layers` entregue à biblioteca, o veredito que o contou e o
motivo de ele não ter sido contado. Nada disso saía do processo: baseline medida
antes desta passagem, `git grep -c "despacho" HEAD -- src-tauri/src/ai/memoria.rs`
e `git grep -c "despacho\|camadasGpu" HEAD -- src/lib src/components src/App.tsx`
não devolvem **nenhuma linha**. O espelho que a tela de memória lê (`InfoMotor`)
copiava cinco campos e não o sexto, então o chip dizia "na placa" do mesmo jeito
para duas cargas que não significam a mesma coisa: a que dividiu as camadas por
conta feita na VRAM livre, e a que jogou todas para cima porque o cabeçalho do
GGUF não fechou — o caso em que `engine.rs:386` decide "placa primeiro, com o
motivo na mão", e o motivo era perdido exatamente aqui.

**Correção.** O veredito atravessa os três degraus que faltavam, sem vocabulário
novo:

- `Despacho` entra para o serde com o mesmo camelCase do resto (`mod.rs:452`);
- `InfoMotor` ganha `despacho: Despacho` (`memoria.rs:1552`), copiado de
  `c.embutidor.despacho` no mesmo bloco que já copia o dispositivo, no instante
  da carga (`memoria.rs:1762`) — a ordem `carregado` → `info_motor` do task #40
  está preservada, porque a cópia continua dentro do `if let Some(c)`;
- `Estado` ganha `despacho: Option<Despacho>` (`memoria.rs:1514`), lido do
  espelho (`memoria.rs:2112`) e nunca do lock do motor — a lição de #40 continua
  valendo: `estado()` não espera a varredura;
- a frente espelha o contrato: `Despacho` em `types.ts:240`, `despacho` em
  `EstadoMemoria` (`types.ts:888`);
- a **formulação** do veredito sai de dentro do `ModelManager` e vira
  `previsao()` em `display.ts:147`. Era uma função local com as três frases
  (`modelos.placaInteira` / `placaDividida` / `placaSistema`); duplicá-la na tela
  de memória seria ter duas versões para a mesma conta — o mesmo motivo pelo qual
  `useJobs` e `Regua` existem. `ModelManager.tsx:24-33` agora só acrescenta a
  `medida` (o `vramCedida`, que é do LLM e não do codificador);
- `MemoriaDialog.tsx:296-300` mostra o chip ao lado do de dispositivo: veredito
  lido → a frase de sempre; veredito ausente → o motivo do núcleo, com o ícone de
  aviso e a classe `memoria-chip aviso` que o `AdaptadoresDialog.tsx:235` já usa
  para "aparece, mas não se faz de capacidade" (`styles.css:4135`).

Nenhuma chave de i18n nova, nenhum texto inventado: quando a conta não pôde ser
feita a tela repete o erro real que `alojamento::fichar` devolveu, já traduzido
pelas chaves do próprio parser.

**Medido.** Vermelho desta vez é compilação, não asserção: sem o campo não havia
o que conferir, e o teste novo (`o_despacho_do_codificador_aparece_no_estado`,
`memoria.rs:2591`) não existia antes dele. O que o teste confere, agora:

- carga nenhuma → `despacho` é `None`, e a tela não tem o que inventar;
- o espelho escrito com um `Dividida` de 10 de 24 camadas → o `estado()` devolve
  o mesmo `Despacho`, byte a byte (`assert_eq!`), e o JSON traz `camadasGpu`,
  `previsto.tipo = "dividida"` e `previsto.emRam` — os nomes que `types.ts`
  declara, pinados aqui porque o chip escolhe a palavra por eles;
- `previsto: None` com o sentinela `CAMADAS_NA_GPU` e um motivo → o motivo chega
  junto. É a asserção que impede a regressão que abriu este item: perder o motivo
  é o que faria uma tentativa parecer medida.

Gates: `cd src-tauri && cargo test --lib ai::memoria` → **33 passaram, 0
falharam, finished in 0.41 s**; `cargo test --lib` → **560 passaram, 0 falharam,
11 ignored, finished in 4.28 s**; `cargo check --all-targets` → **zero avisos**;
`npm run build` → **OK, `✓ built in 182 ms`**, sem aviso de chunk.

**ALEGADO, não medido aqui:** (1) a linha que copia o despacho na carga
(`memoria.rs:1762`) só roda quando um codificador entra na memória — pesos,
`llama.dll` e placa, tudo proibido nesta passagem; o que está provado é o trecho
espelho → estado → JSON, e a cópia é leitura de código. (2) O chip nunca foi visto
num navegador: sem abrir o aplicativo não há render. O que se sabe dele é o CSS
citado e o type-check; a contagem de como ele quebra em `memoria-chips` (que tem
`flex-wrap`, `styles.css:4106`) com um motivo comprido é inferida do estilo, não
observada.

## O que eu não pude medir, e por quê

- **A carga real do codificador não rodou.** O task proíbe abrir o aplicativo,
  rodar `engine-probe`, carregar pesos e tocar a placa (a RX 9060 XT está em uso).
  Portanto: o `n_gpu_layers` que a `llama.dll` recebeu em uma carga de verdade, o
  delta de VRAM que ele produziu e o tempo que o codificador dividido leva na CPU
  ficaram esperando uma corrida real do maintainer. O que está provado aqui é a
  decisão (a função pura, o arquivo real no disco) e a ligação dela ao argumento
  de `params_modelo` — lida no código, não executada.
- **A VRAM livre deste instante não foi lida.** `alojamento::capacidade` abre as
  DLLs e chama `llama_backend_init`, que enumera as placas Vulkan: é tocar a GPU,
  então nenhum teste deste lote a chama. As `Capacidade` dos testes de decisão são
  entradas injetadas, marcadas como tal, e o `total: 16 GiB` é o da máquina de
  referência já usada nos testes existentes deste arquivo (mod.rs:634 na HEAD).
- **O caminho de erro do pino com DLLs reais** (`mem::forget` em
  `ffi.rs:329-341`) não é exercitável sem as três bibliotecas em disco: o teste
  usa um caminho que não existe, que é como se obtém o erro real do
  `LoadLibraryW` sem mapear módulo nenhum. A ramificação do esquecimento em si
  continua não executada — ela exige que o carregador recuse um módulo que acabou
  de mapear, o que não se fabrica sem mexer na pasta de runtime.
- **O ciclo de compilação ficou bloqueado por arquivos de outros agentes**
  (`state.rs`, depois `src/mcp/instalador.rs`, com erros de tipo e de
  tempo-de-vida). Nada no crate compila enquanto um deles estiver quebrado,
  inclusive os meus testes. Onde há saída colada, a árvore estava verde; onde está
  marcado como pendente, não estava.

## Chaves i18n

Nenhuma chave nova. `erros.motorDllNaoAbriu` (existente, nos dois idiomas) cobre o
motivo do pino; o motivo do codificador sem ficha é o texto que o próprio
`alojamento::fichar` já devolve (`erros.alojamentoGguf`, `erros.alojamentoSemArquitetura`,
`erros.alojamentoSemCamadas`, `erros.alojamentoEstouro`).

## Follow-ups fora da minha posse

1. ~~**`src/ai/memoria.rs:1537` (`struct InfoMotor`) e `1483` (`struct Estado`)**~~
   — **fechado pelo maintainer**, na seção "#39 (continuação)" acima. Os campos
   vieram como um `Despacho` inteiro (`Option<Despacho>` no `Estado`), não como
   `previsto` + `motivo` soltos: é a mesma estrutura que o núcleo já calcula, e a
   frente não precisa de duas declarações para uma coisa só. Nenhuma chave nova.
2. ~~**`src/ai/memoria.rs:1723`** — o `erro` do `Estado` como canal para o motivo
   de um despacho não medido~~ — **não usado**: com o item 1 fechado o motivo tem
   campo próprio, e o `erro` da tela de memória continua sendo só o da varredura.
   Misturar os dois faria uma recusa de placa parecer falha de índice.
3. **Versão da biblioteca na tela** — se a opção ligada do A9 for desejada, o
   lugar é `Acelerador` (engine.rs:258) e `InfoMotor` (memoria.rs:1537), com o
   `llama_version` voltando como campo de `Ggml`. Eu removi porque, sem esses dois
   edits, ele seria dead code de novo.
4. **`engine.rs:391-394`** — o caminho do LLM continua escolhendo o device com
   `ggml::escolher` dentro de `Motor::novo` e a `Capacidade` com
   `alojamento::capacidade(dlls)` antes; são a mesma placa por construção, mas por
   duas leituras do registro. Se um dia as duas precisarem concordar por tipo e
   não por coincidência, o `Capacidade` deveria vir do mesmo `dispositivos` que
   elegeu o device — mudança em `engine.rs`, arquivo que não é meu.
