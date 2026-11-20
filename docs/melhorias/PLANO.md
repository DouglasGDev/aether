# Lote de melhorias — análise e delegação

Análise de quatro frentes independentes sobre o código real (não sobre memória),
com os gates verdes como linha de base. Cada achado tem `arquivo:linha` e a
evidência lida. Este documento é o brief comum dos agents; cada frente grava o
seu próprio registro em `docs/melhorias/<frente>.md` antes de finalizar.

## Linha de base medida (2026-11-20)

- `cd src-tauri && cargo test --lib` → **478 passaram, 0 falharam, 5 ignored**
  (`C:/Users/dougl/AppData/Local/Temp/aether-gate-cargo.log`).
- `npm run build` → **OK**, único aviso: chunk JS de 620,84 kB > 500 kB
  (`C:/Users/dougl/AppData/Local/Temp/aether-gate-npm.log`).
- Nenhum `TODO/FIXME/unimplemented!/todo!` no código.
- Paridade de i18n conferida: 990 chaves em cada língua, zero chaves de um lado
  só, zero placeholders divergentes.

## Contrato que todo agent obedece (de CONTRIBUTING.md)

1. **Nada simulado.** Sem resultado inventado, placeholder ou fallback que finge
   sucesso. Capacidade que não rodou devolve o motivo real.
2. **Medida é o que foi lido.** Número em teste/doc vem de log ou medição desta
   máquina, com o comando. Nada de "deveria dar X".
3. **Teste vermelho primeiro.** Todo defeito corrigido ganha o teste que falhava
   antes da mudança; escreva-o, veja-o vermelho, então corrija.
4. **Um escritor por arquivo.** Cada frente só edita os arquivos listados abaixo.
   Arquivo de outro agente é intocável.
5. **i18n.** String visível nova vem de `locales/pt-BR.json` E `en-US.json` com a
   mesma chave e placeholder. **Agents NÃO editam locales** (arquivo compartilhado):
   reutilizam chave existente ou registram a chave necessária no seu doc; o
   maintainer adiciona todas ao final, num único escritor.
6. **Comentário `//!`/`///` em português, explicando o porquê**, não o quê.
7. **Não rodar `cargo`/`npm` durante o lote.** Os agents compartilham a árvore;
   compilar em paralelo pega arquivo pela metade. O gate central roda uma vez,
   pelo maintainer, depois que todas as frentes terminam. Agents verificam por
   leitura + teste escrito + raciocínio, e documentam.
8. **Sem gambiarra.** Se a correção não fica certa, desfaz; não deixa meia-solução.
9. **Lock de motor não é segurado durante geração** (regra do projeto). Frente
   CONC trata a tensão restante entre a regra e o código.

## Frentes e donos (arquivos disjointos)

> Este bloco é o brief como foi escrito antes do lote, com as linhas da árvore
> daquela hora. Não é lista de trabalho aberto: o que cada item virou, com as
> linhas de depois da correção e o vermelho-primeiro medido, está na
> **Resultado do lote** embaixo e em `docs/melhorias/<frente>.md`.

### GATE — maintainer (o mais crítico)
O gate de permissão é o mecanismo central de segurança do produto. Duas falhas
o derrotam:
- **S2 [HIGH]** `mcp/tool.rs:29` + `permissions/mod.rs:41,49`: `classify_effect`
  dá `ReadOnly` a partir do `read_only_hint` de um servidor de terceiros, e
  `evaluate` deixa `ReadOnly` passar sem prompt em **Plan** (`Allow`) e
  **AutoAccept** (`Allow`). Um servidor malicioso anota uma ferramenta de
  escrita/apagar como read-only e executa sem prompt. O comentário do modo
  Default (`permissions/mod.rs:60-63`) já admite que o hint é não-confiável —
  mas só o Default se protege. Evidência lida: `classify_effect` ramo
  `read_only_hint == Some(true) => ReadOnly`; `evaluate` Plan `ReadOnly=>Allow`.
  **Correção:** teto de efeito de terceiros = `Mutating` (honra `destructive_hint`
  que aumenta a cautela; não honra `read_only_hint` que a diminui). O conector
  embutido continua confiável via `ToolInfo::local` (`manager.rs:101,106`), então
  suas ferramentas somente-leitura seguem `ReadOnly`. Consequência documentada:
  em Plan, ferramenta de terceiros passa a ser negada (não dá para verificar a
  alegação read-only); em AutoAccept, roda como Mutating (o modo já consente).
- **S5 [MEDIUM]** `permissions/mod.rs:57`: no Default, `session_granted => Allow`
  para qualquer efeito, inclusive `Destructive`. Depois de um "confiar neste
  servidor", `apagar`/`bancos_apagar` rodam sem prompt — o Default fica MENOS
  seguro que o AutoAccept (que pergunta para destrutivo). **Correção:** Default
  pergunta para `Destructive` mesmo com grant da sessão.
- Arquivos: `src-tauri/src/permissions/mod.rs`, `src-tauri/src/mcp/tool.rs`.

### SEC — contenção e injeção
- **S1 [HIGH]** `local/arquivos.rs:239,289,293`: `arvore()` usa `item.metadata()`
  (no Windows não segue symlink), então symlink-para-arquivo entra como arquivo;
  `fs::metadata`/`fs::read` em `buscar()` seguem o link e leem fora da pasta
  concedida, sem passar por `pastas.resolver`. ReadOnly → sem aprovação no Plan.
  **Correção:** rejeitar entradas `file_type().is_symlink()` na travessia (e no
  ramo não-recursivo `:279`), e re-resolver cada candidato por `pastas.resolver`.
  Guardar também `mover` contra renomear a própria raiz concedida (`:492-504`,
  como `apagar` já faz em `:524`).
- **S3 [MEDIUM]** `mcp/execucao.rs:248-253`: detector de comando composto só olha
  `| ; && $( ` >`; deixa passar `&`, newline, `<`, `^`, `%`, derrotando o
  re-prompt do grant de sessão. **Correção:** incluir esses separadores.
- **S4 [MEDIUM]** `bancos/motor/mongo.rs:134-138` + `motor/mod.rs:107`: valor de
  filtro objeto com chave `$` (`$ne`, `$exists`, `$gt`…) vira operador Mongo
  sempre-verdadeiro em consultar/atualizar/apagar → update/delete em massa dentro
  da tabela autorizada. **Correção:** em `json_para_bson`, rejeitar chave que
  começa com `$`.
- **LOW** `bancos/ferramentas.rs:301`: `colunas` usa `fonte_exposta` sem o check
  `ler`, expondo esquema de tabela não-legível. **Correção:** exigir `fonte.ler`.
- Arquivos: `local/arquivos.rs`, `mcp/execucao.rs`, `bancos/motor/mongo.rs`,
  `bancos/ferramentas.rs`. (TLS de mssql/postgres fica como recomendação no doc —
  mudar padrão para verify quebraria conexões locais de dev; exige flag opt-in
  por conexão + UI, fora deste lote.)

### CORE — correção do núcleo de IA
- **A2 [MED]** `engine.rs:124,558` + `session.rs:508-510`: a rede de parada de
  ChatML descontrolado é inerte — `tokenizer.decode(tokens, true)` pula tokens
  especiais, então `text.contains("<|im_start|>")` nunca casa para tokenizers que
  marcam os marcadores como especiais (Granite/Qwen). Modelo repetindo turnos vai
  até max_tokens. **Correção:** detectar marcador por id de token cru, ou decodificar
  com `false` só para a detecção de parada.
- **A3 [MED]** `router.rs:195`: candidato sem ficha devolve `(3, true)`, violando o
  invariante `:168-169` ("None nunca deve ser lido como cabe"); modelo não-medido
  passa nos filtros `cabem` e pode vencer um medido que não cabe. **Correção:**
  `(3, false)` e ranquear não-medidos por último.
- **A4 [MED]** `alojamento.rs:256,303-304`: `kv_por_token` e `kv*janela`,
  `pesos+kv+FOLGA` multiplicam metadado u32 do GGUF sem checagem; header crafted
  estoura u64 → panic em debug / wrap em release → veredito `Inteira` falso e OOM.
  **Correção:** aritmética checked, recusar no overflow.
- **A5 [MED]** `engine.rs:302,316,328-331`: `fichar(...).ok()` /
  `capacidade(...).ok().flatten()` descartam o motivo real da falha; quando a
  sonda de VRAM falha, `despacho` cai em `CAMADAS_NA_GPU` (-1, tudo) e a UI mostra
  `tipo:"gpu", motivo:null` — offload total não-medido apresentado como normal.
  **Correção:** reter o erro da sonda e expô-lo (motivo/aviso), não default cego.
  Manter a mudança dentro de `engine.rs`; se precisar de `state.rs`, registrar no
  doc como follow-up (state.rs é do CONC).
- **A6 [MED]** `session.rs:664-679,690`: com pensamento ligado, corte MaxTokens
  dentro de bloco think não-fechado não tem continuação (`chamada_pendente` só vê
  tool call); `answer` vira string vazia e emite `Done` com texto vazio — sucesso
  vazio silencioso. **Correção:** continuar o bloco think como chamada truncada, ou
  encerrar expondo o motivo real como erro.
- **A7 [LOW]** `chat.rs:325-334`: `blocos_json` varre dentro de blocos XML já
  parseados; objeto JSON com `name` embutido num valor de parâmetro vira chamada
  extra (execução dupla pelo gate). **Correção:** pular intervalos já tomados pelas
  marcas XML.
- Arquivos: `ai/engine.rs`, `ai/router.rs`, `ai/alojamento.rs`, `ai/session.rs`,
  `ai/chat.rs`. (A1 lock, A8 poison, A9 dead code, A10 freio ficam para CONC/doc.)

### MEM — memória e persistência
- **M1 [HIGH]** `memoria.rs:1977,2079-2226`: `varrer_e_embutir` segura o guard
  `carregado` a varredura inteira, enquanto `estado()` faz `carregado.lock()` e é
  chamado pelo loop da tela (`commands.rs:1494`) → UI bloqueia a varredura toda
  (mesma classe do task #40 já corrigido). **Correção:** `estado()` lê
  `esta_indexando()`/info de placa em cache SEM tomar `carregado`; ou guard por
  arquivo. A correção fica dentro de `memoria.rs` (não tocar commands.rs).
- **M2 [HIGH]** `memoria.rs:1042,703`: `blob.len() != trechos.len()*dimensao*4`
  estoura em release; `dimensao` corrupta passa com blob vazio, e `remover` fatia
  `vetores[inicio..inicio+dimensao]` → panic OOB via `revogar`. **Correção:**
  cadeia `checked_mul`, recusar no overflow.
- **M4 [MED]** `memoria.rs:1896-1907`: conversa truncada por `LIMITE_TRECHOS` no
  meio do laço é salva com a prova COMPLETA → `conversa_vista` true para sempre,
  os trechos que faltam nunca voltam. **Correção:** marcar truncamento na
  `FichaConversa` ou pular a ficha quando `prontas.len() < lidas.len()`.
- **M5 [MED]** `memoria.rs:1736-1750,1828-1851`: `revogar` e `fundir_conversas`
  fazem clone→modify→save→swap sem lock de escrita compartilhado → revoke
  concorrente sobrescrito, ressuscitando trecho de arquivo revogado no índice.
  **Correção:** serializar escritores do índice sob um mutex.
- **M6 [MED]** `conversas.rs:199`: `ler_json::<Indice>().unwrap_or_default()` esvazia
  a lista inteira num `indice.json` corrupto; os `<id>.json` ficam órfãos.
  **Correção:** reconstruir o índice dos arquivos em disco quando o parse falha.
- **M7 [MED]** `conversas.rs:274-312`: `renomear`/`gravar` fazem read-modify-write
  do arquivo da conversa FORA do mutex (lock cobre só o índice) → rename durante
  save de turno perde mensagem/título. **Correção:** segurar o lock no arquivo+índice.
- **M3 [MED]** `download.rs:69-75,323-327`: sem `read_timeout` (só connect_timeout);
  cancel só é checado quando chega chunk → transferência travada pendura e *Parar*
  não responde. **Correção:** `.read_timeout(...)` e cancelar contra o stream.
- Arquivos: `ai/memoria.rs`, `conversas.rs`, `ai/download.rs`. (M8 sha256,
  catalog header-check, adaptadores DoRA honesty ficam como recomendação no doc.)

### CONC — concorrência do motor
- **A1 [HIGH]** `state.rs:390-394` + `commands.rs:1105-1172,806,830,851`: o mutex
  do motor É segurado a geração inteira (contradiz CONTRIBUTING:49-51). O deadlock
  original de status está mitigado (status lê `janela`/`acelerador` em cache), mas
  `conversa_nova/abrir/apagar` chamam `esquecer()` → `loaded.lock()` depois de um
  check `is_busy` TOCTOU; um turno que começa nesse gap congela a thread da UI o
  turno inteiro. **Correção mínima e segura:** tornar esses comandos de conversa
  atômicos quanto a busy (recusar/adiar quando há turno, sem bloquear no lock do
  motor). NÃO refatorar a posse do motor neste lote. **Documentar** a tensão
  regra-vs-código e a refatoração maior como follow-up, com a evidência lida.
- Arquivos: `state.rs`, `commands.rs`.

### FRONT — interface
- **F1 [HIGH]** `Chats.tsx:132-152`: botão abrir-conversa sem `disabled={ocupado}`,
  contradizendo o contrato do próprio arquivo (`:9-11`); trocar de conversa no meio
  do turno substitui `turns` (`App.tsx:480`) enquanto o handler de evento
  (`App.tsx:265-338`) segue anexando delta ao "último turno assistant" — agora de
  outra conversa. **Correção:** desabilitar `chats-abrir` quando `ocupado` e
  guardar `abrirConversa` em `talking`.
- **F2 [MED]** `GrafoNeural.tsx:454-484`: edição otimista da política corre —
  `setBase(novo)` então `gravar`, cuja resposta `setBase(estado)` pode chegar fora
  de ordem e sobrescrever estado mais novo, divergindo do núcleo. **Correção:**
  sequenciar escritas (contador de request, descartar resposta velha).
- **F3 [MED]** `App.tsx:109-150,365,442`: `refresh()` sem guarda de ordem; todo
  evento MCP dispara `void refresh()` concorrente → snapshot mais velho pode
  atropelar `pending`/`connected`/`chats` mais novo. **Correção:** token monotônico,
  descartar resultado velho.
- **F4 [MED]** `api.ts:215-217`: `message()` devolve `JSON.stringify(error)` para
  não-string; um `Error` vira `"{}"` exibido em toda faixa de falha. **Correção:**
  tratar `Error` (`error.message`) antes do stringify.
- **F5 [MED]** `MemoriaDialog.tsx:341,429`, `VozDialog.tsx:173`: rejeições de invoke
  engolidas (`void api.…()` sem catch) → unhandled rejection, sem estado de erro.
  **Correção:** rotear pelo caminho de erro existente.
- **F6 [MED]** `ServerDialog.tsx:147-148`: `mcpInstallCancel(...).catch(()=>false)`
  mapeia falha real para "nada para cancelar" — estado falso. **Correção:**
  distinguir rejeição (mostrar `message(cause)`) de `false`.
- Arquivos: `App.tsx`, `Chats.tsx`, `api.ts`, `GrafoNeural.tsx`,
  `MemoriaDialog.tsx`, `VozDialog.tsx`, `ServerDialog.tsx`. (F7 prosa na UI,
  F8-F16 lows e extração de componente compartilhado ficam como recomendação.)

## Recomendações documentadas (fora deste lote, com evidência)

- **TLS por conexão** (`bancos/motor/mssql.rs:204` trust_cert, `postgres.rs:151`
  NoTls): flag opt-in de verify/encrypt por conexão + UI; padrão verify quebraria
  dev local com cert auto-assinado. Não mudar padrão às cegas.
- **A8** recovery de mutex envenenado (`session.rs:276,559`, `ggml/ffi.rs:269-271`):
  `into_inner` ou erro real em vez de `expect` em produção.
- **A9** dead code `Motor::versao`/`Ggml::versao` (`ggml/mod.rs:412`, `ffi.rs:356`):
  remover ou ligar a diagnóstico.
- **M8** integridade do download só por tamanho (`download.rs:375-382,398-411`,
  antes do `rename` de 411): verificar sha256 (HF publica), não só a contagem.
- **#34 DoRA**: detectado mas NÃO aplicado (`adaptadores.rs:61-66` admite que o
  loader rejeita `.mag`); task #38 "completed" exagera. LoRA mount é real
  (`ffi.rs:335-337`, `state.rs:269,456`). Sem treino, por design.
- **#39 embedder**: hardcode `-1` (`ggml/mod.rs:515`); veredito de alojamento já
  dirige o `n_gpu_layers` do LLM (`engine.rs:386-393`), falta o embedder.
- **Chunk JS >500 kB**: code-splitting por rota/dialog (dynamic import) no Vite.
- **F16**: maquinaria de Job/progresso e slider duplicada em 4 dialogs; extrair
  componente compartilhado DEPOIS das correções individuais (sequenciado).

## Finalização

Cada frente grava `docs/melhorias/<frente>.md` com: o que mudou (arquivo:linha),
por quê, o teste vermelho-primeiro (nome + assertion), o que mediu/leu para
confirmar, e o que deixou como follow-up. O maintainer roda o gate central
(`cargo test --lib` + `npm run build`), adiciona as chaves de i18n registradas,
corrige qualquer fallout de compilação, e consolida o índice. Nada é dado como
pronto sem o gate verde lido do log.

## Resultado do lote (medido 2026-11-20)

Gate central após o lote inteiro fechado:
`cd src-tauri && cargo test --lib` → **509 passaram, 0 falharam, 7 ignored**;
`npm run build` → OK (o mesmo aviso pré-existente de chunk JS > 500 kB).
Baseline antes do lote: 478. No meio do lote, com só GATE/SEC/CONC/FRONT e
M4/M1/M2 na árvore, o gate media 499.

Registros por frente (cada um: o que mudou com `arquivo:linha`, por quê, o
vermelho-primeiro, o que foi medido e o que ficou pendente):

- `GATE.md` — S2, S5 (mantainer, o mais crítico). Verde, verificado.
- `SEC.md` — S1, S3, S4 e o `LOW`. Verde. O guard de `mover` foi reordenado para
  recusar renomear a raiz antes de validar o destino (causa do teste vermelho
  que a frente deixou).
- `CONC.md` — A1. Verde. Chave i18n nova `erros.motorOcupado` adicionada nos dois
  idiomas pelo maintainer.
- `FRONT.md` — F1 a F6. Verde.
- `CORE.md` — A2 a A7 todos na árvore. O agente de CORE parou no meio por
  estouro de limite de crédito (só A2 e A5 ficaram dele); o maintainer fechou
  A4 (`alojamento.rs:254,317`), A3 (`router.rs:199`), A6 (`session.rs:499,710`)
  e A7 (`chat.rs:330`), cada um com o vermelho-primeiro rodado antes do verde.
  Chaves i18n novas do lote: `erros.alojamentoEstouro`, `erros.pensamentoTruncado`,
  `erros.motorSemPlaca`, nos dois idiomas.
- `MEM.md` — M1, M2, M4, M5, M6, M7 e M3 todos na árvore. O agente de MEM parou
  por limite de crédito após M4; o maintainer fechou M1/M2 e depois M6
  (`conversas.rs:206,242`), M7 (`conversas.rs:311,346`), M5 (`memoria.rs:1568,1814`)
  e M3 (`download.rs:85,151,352`). Nenhum dos quatro precisou de chave i18n nova.

Os `arquivo:linha` do brief lá em cima são os da árvore antes do lote; os das
seções FEITO de cada doc são os da árvore depois da correção.

Pendências de próximo lote: nenhuma das quatro frentes do lote tem item aberto.
As recomendações fora de lote (TLS por conexão, A8/A9, M8, DoRA,
embedder, code-splitting, F16) seguem na seção acima.