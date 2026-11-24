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
- **A3 [MED]** `router.rs:195` (de antes da correção; hoje o ramo sem ficha está
  em `:199`): candidato sem ficha devolve `(3, true)`, violando o
  invariante `:168-169` ("None nunca deve ser lido como cabe"); modelo não-medido
  passa nos filtros `cabem` e pode vencer um medido que não cabe. **Correção:**
  `(3, false)` e ranquear não-medidos por último.
- **A4 [MED]** `alojamento.rs:256,303-304` (de antes da correção; hoje a conta é
  `taxa_kv` em `:445` e `alojar` em `:626`): `kv_por_token` e `kv*janela`,
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
- Arquivos: `ai/memoria.rs`, `conversas.rs`, `ai/download.rs`. (M8 sha256 fechado
  em `M8.md`; a honestidade dos adaptadores DoRA fechada em `DORA.md`; o
  header-check do catálogo fechado — `catalog.rs:492` confere cada família
  declarada contra a `llama.dll` vendorizada.)

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

- ~~**TLS por conexão**~~ (`bancos/motor/mssql.rs:209` `confianca_da`,
  `postgres.rs:160` `ficha_de`) — **fechado na terceira passagem** (`BANCOS.md`): a flag opt-in existe
  desde a segunda leva, mas não tinha caminho até a tela; agora `ConexaoView`
  devolve `tlsVerificado` e o `BancosDialog` tem o toggle. O padrão continua
  desligado, e o mysql — único motor que a ignorava em silêncio — passou a
  recusar a ligação em vez de falar em claro.
- **TLS do postgres** (`Cargo.toml:59`, `tokio-postgres` só com
  `with-serde_json-1`): com a marca ligada, `SslMode::Require` sobre `NoTls`
  **impede** a conexão em vez de cifrá-la. Honestamente, mas o dono ligou o
  interruptor esperando o contrário. Fechar pede feature de TLS em Rust puro
  (`tokio-postgres-rustls` + `MakeTlsConnect` próprio) e só tem verificação real
  com um Postgres na rede. Fica fora deste lote por isso.
- **MySQL TLS** (`Cargo.toml:60`, `mysql_async` com `features = ["minimal"]`, que
  não traz TLS): hoje recusa com motivo. Ligaria de verdade com a feature `rustls`
  do crate — mesma decisão pendente do postgres, e o mesmo motivo para esperar:
  sem servidor, nada é medido.
- **A8** recovery de mutex envenenado — **fechado na produção, e é instrumento
  que diz isso, não conta de `grep`**: a regra passou a ser conferida crate a
  crate por `contrato::testes::trava_de_dado_puro_recupera_do_envenenamento`,
  que varre os **65** arquivos `.rs` de `src-tauri/src` (`find . -name "*.rs" | wc -l`),
  recorta todo bloco aberto depois de `#[cfg(test)]` e procura `.lock()` /
  `.read()` / `.write()` seguidos de `.expect(`/`.unwrap(`. Verdir com **zero**
  violações (`cargo test --lib contrato` → 2 passaram). Dos **18** literais
  `expect("… envenenad…")` que ainda ocorrem — `grep -rnE "expect\(\"[^\"]*envenenad"
  --include="*.rs" src-tauri/src | wc -l`, baixado de 100 na conta antiga — **16**
  estão dentro de `#[cfg(test)]` (é assim que o teste segura a trava de propósito:
  `state.rs` :678–:830, `memoria.rs` :2573–:2712, `session.rs` :837, `config.rs`
  :360, `conversas.rs` :1020, `idiomas.rs` :371, todos acima da linha do
  `cfg(test)` do próprio arquivo) e **2** são comentário de doc (`contrato.rs:5`,
  `session.rs:821`). O que a frente tinha como "fechar por arquivo, não em
  varredura" foi fechado assim, arquivo por arquivo, na leva CONTRATO.
- **A9** dead code `Motor::versao`/`Ggml::versao` — **fechado na segunda leva**
  (`FFI.md`): `grep -rn "fn versao" src/ai/` não devolve nada hoje, e a opção de
  ligar a diagnóstico ficou registrada como follow-up no doc deles (o lugar seria
  (`engine.rs:260`) + `InfoMotor`).
- ~~**M8** integridade do download só por tamanho~~ — **fechado na segunda leva**
  (`M8.md`): `download.rs:149` mede o sha256 do arquivo inteiro e o confere com o
  `lfs.oid` publicado antes do `rename`.
- **#34 DoRA**: detectado e rotulado na tela, **e o núcleo não consumia a
  informação** — a metade honesta **fechada** nesta passagem (`DORA.md`). LoRA
  mount é real (`ffi.rs:335-337`, `state.rs:269,456`). Sem treino, por design.
  Conferido de novo na sexta passagem, com instrumento:
  - a detecção era lida pela frente — `item.ficha.dora` vira o chip `aviso` em
    `AdaptadoresDialog.tsx:233-238` (a linha desta mesma seção dizia "nada fora
    de `adaptadores.rs` lê o campo": falsa hoje; `git grep -n dora -- src` devolve
    `types.ts:934` e o dialog).
  - no **crate**, `git grep -n dora -- 'src-tauri/src' ':!src-tauri/src/ai/adaptadores.rs' ':!src-tauri/src/voz'`
    devolve **zero linhas** (medido nesta passagem, e continua zero: quem lê o
    flag é o próprio módulo): a carga não olhava para ele, e um DoRA entrava pelo
    mesmo caminho de um LoRA comum sem que nada registrasse a diferença. É
    exatamente isso que fecha aqui — `LE_MAGNITUDE` (`ggml/mod.rs:159-172`)
    declara o que a biblioteca lê, com o próprio binário como instrumento
    (`ggml/mod.rs:1025-1069`), e `Item::magnitude_fora` (`adaptadores.rs:341-372`)
    grava por adaptador aplicado que a normalização ficou de fora. A linha da
    tela para de dizer "Na placa" a seco: diz "Na placa, sem magnitude".
  - o que a nota da tela alega sobre a biblioteca foi **medido no binário** dele,
    não recordado do upstream, em `vendor/llama-cpp/lib/llama.dll` (o build
    `b12387` do `vendor/README.md:37`):
    `grep -aoE "%s\.[a-z_.]{2,40}" llama.dll | sort -u | wc -l` → **203**
    modelos de chave, e `… | grep -c magnitude` → **1**, que é
    `%s.hyper_connection.magnitude` — outro mecanismo, não DoRA. A suíte de
    literais do adaptador também não aparece:
    `grep -haoE "\b(mag|mag_b|dora)\b" llama.dll ggml-base.dll ggml-vulkan.dll` →
    nada, enquanto `grep -haoE "\blora_[ab]\b" llama.dll` devolve `lora_a` (2) e
    `lora_b` (1). Lido daí: esta biblioteca casa pares de deltas e não tem o que
    ler de magnitude, e a nota da tela diz a verdade. **É evidência de string em
    binário, não execução** — provar o efeito de meio DoRA montado contra pesos
    exige adaptador, pesos e placa, e aí o aplicativo aberto volta a ser o
    portão.
  - o que **resta** do título é a outra metade: **montar** a magnitude. Não há
    API para isso na biblioteca — `llama_set_adapters_lora` soma deltas escaladas
    no grafo de computação, e a normalização por linha do DoRA pede o peso já
    fundido. Fazer certo é um conversor que funda o adaptador no GGUF com a
    magnitude aplicada, conferido contra um DoRA conhecido; é frente nova, com
    pesos na placa, não um número a mais na tela.
- ~~**#39 embedder**: hardcode `-1` (`ggml/mod.rs:515`)~~ — **fechado na segunda
  leva** (`FFI.md`): `despachar_codificador` (`ggml/mod.rs:482`) decide pelo
  veredito de alojamento, e o `-1` só aparece no ramo em que o cabeçalho não
  fechou — com o motivo real na mão, não em silêncio.
- ~~**Chunk JS >500 kB**~~ — **fechado na segunda leva** (`FRONT2.md`, FU7): onze
  dialogs e o grafo entram por `lazy()` (`App.tsx:36-75`) e o build não imprime
  mais aviso nenhum; o maior bloco é `react` a 210,53 kB.
- ~~**F16**: maquinaria de Job/progresso e slider duplicada em 4 dialogs~~ —
  **fechado** (`FRONT3.md`): `useJobs<F>()` em `src/lib/jobs.ts:40` é a única
  máquina de trabalho (`interface Job` restou 1 em `src`, `setJobs`/`setFaults`
  restaram 0 fora de `jobs.ts`) e `Regua` é o único cursor — `type="range"` há
  um só na frente toda (`BarraDeTrabalho.tsx:170`). A extração foi depois das
  correções individuais, como o sequenciamento pedia.

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
  A4 (`alojamento.rs:445,626`), A3 (`router.rs:199`), A6 (`session.rs:499,710`)
  e A7 (`chat.rs:330`), cada um com o vermelho-primeiro rodado antes do verde.
  Chaves i18n novas do lote: `erros.alojamentoEstouro`, `erros.pensamentoTruncado`,
  `erros.motorSemPlaca`, nos dois idiomas.
- `MEM.md` — M1, M2, M4, M5, M6, M7 e M3 todos na árvore. O agente de MEM parou
  por limite de crédito após M4; o maintainer fechou M1/M2 e depois M6
  (`conversas.rs:215,251`), M7 (`conversas.rs:320,355`), M5 (`memoria.rs:1579,1826`)
  e M3 (`download.rs:88,253,539`). Nenhum dos quatro precisou de chave i18n nova.

Os `arquivo:linha` do brief lá em cima são os da árvore antes do lote; os das
seções FEITO de cada doc são os da árvore depois da correção.

Pendências de próximo lote: nenhuma das quatro frentes do lote tem item aberto.
As recomendações fora de lote (TLS por conexão, A8/A9, M8, DoRA,
embedder, code-splitting, F16) seguem na seção acima.

## Passagem do maintainer sobre a segunda leva (medido 2026-11-20)

Os cinco agentes da segunda leva (#93 FFI, #94 CONC2, #95/#96/#97 FRONT) pararam
no limite de turnos e deixaram a árvore em pedaços: `ai/ggml/mod.rs` não
compilava, e os arquivos deles ficaram como estavam. O maintainer refez a árvore
e passou o gate — **544 passaram, 0 falharam, 11 ignored, finished in 4.82 s** na
reparação, **548** depois dos quatro testes novos de `MCP.md`. Depois fechou,
cada um com o vermelho-primeiro medido antes do verde, o que estava ao alcance
sem abrir o aplicativo:

- `MCP.md` — **M1** o `npm` que despeja stderr prendia o instalador (leitura do
  cano ao lado do laço de stdout; 10,01 s pendurado → 0,06 s);
  **M2** o recorte do grafo não valia na placa publicada: o servidor anunciava as
  21 ferramentas com o `computador` recortado para 2, e um `FullAccess` do cliente
  bastava para rodar a escrita desligada. O `Arc<RwLock<Registry>>` agora é
  dividido com a `Execucao`, e `catalogo_publicado` / `ferramentas_anunciadas` /
  `Estado.ferramentas` leem o mesmo recorte que o laço de conversa;
  **M3** duas escritas concorrentes caíam no mesmo `aether-tmp` e uma publicava o
  corpo da outra.
- `VOZ.md` — **V1** a barra de progresso da voz contava bytes que não vieram da
  rede (começava em 12.937.772 de 90.672.131) e o clamp a prendia nos 100 % com o
  download no meio; **V2** o medidor do microfone continuava congelado no último
  nível depois de a escuta fechar. V2 é **raciocinado, não medido**: exige
  microfone real, e a suíte não captura áudio.
- `I18N.md` — a verificação que estava pendente por causa da árvore quebrada foi
  rodado e registrada: **11 passaram** em `idiomas`, com as duas chaves que o
  guard achou (`erros.motorEnvenenado`, `erros.corpoTrocado`) já nos dois idiomas.

Registros dos agentes, como ficaram: `FFI.md` (#39, A9, A8 — com a ressalva
deles mesmos de que a carga real do codificador não rodou, porque abrir o
aplicativo estava proibido), `CONC2.md` (#94, posse do motor), `M8.md` (#92,
integridade do download), `FRONT2.md` e `FRONT3.md` (casca e dialogs). O gate acima
é a evidência de que os arquivos deles estão na árvore e verdes; o que cada um
alega além disso está nos próprios docs, com MEDIDO e ALEGADO separados.

Gates desta passagem: `cd src-tauri && cargo test --lib` → **548 passaram, 0
falharam, 11 ignored, finished in 4.60 s**; `npm run build` → **OK, sem aviso**
(grep por `warn|error|500` na saída não imprime nada: o aviso de chunk > 500 kB
que a linha de base tinha acabou com o code-splitting — maior bloco hoje é
`react` a 210,53 kB).

## Passagem do maintainer sobre a terceira leva (medido 2026-11-20)

Auditoria do que restava ao alcance, sem abrir o aplicativo. Fechou a frente que
o agente #95 deixou pela metade — não os erros de compilação que a tarefa citava
(a árvore já estava verde), e sim o buraco que a leitura do código mostrou:

- `BANCOS.md` — **o TLS por conexão existia só no disco**. A flag é lida por
  quatro motores e round-tripada pelo cadastro, mas `ConexaoView` não a
  devolvia e `ConexaoRegistro` não a levava: nenhum controle na tela e nenhuma
  chave de idioma. (O `bancos_listar` que o model chama continua sem a marca —
  é o cartão de política da conexão, e a postura do canal não muda o que ele
  pode pedir.) Vermelho medido
  com o JSON inteiro da vista impresso (`left: Null`, a chave ausente). Agora o
  toggle está no `BancosDialog`, com chip na linha da conexão e limpo quando o
  tipo troca.
  No mesmo caminho, dois buracos de honestidade: **mysql** ligava a flag e
  descia mesmo assim para o aperto de mão em texto puro (o vermelho mostrou
  `os error 10061` do soquete, não uma recusa — o driver deste build não tem
  TLS nenhum), agora recusado antes de tocar a rede; e **oracle** tinha a flag
  lida sem um único teste no arquivo, o que deixava a promessa do `tcps://`
  como alegado. Os dois testes novos do oracle passaram na primeira medição,
  então a promessa era verdadeira — só não era conferida.

Gates desta passagem: `cd src-tauri && cargo test --lib` → **553 passaram, 0
falharam, 11 ignored, finished in 4.91 s** (548 + 1 em `commands`, +2 em
`mysql`, +2 em `oracle`); `npm run build` → **`✓ built in 193 ms`**, sem aviso de
TypeScript e sem aviso de chunk, com `BancosDialog` no lazy chunk dele
(8,58 kB │ gzip 2,33 kB).

## Passagem do maintainer sobre a quarta leva (medido 2026-11-20)

Sem delegação: nada do que restava precisava de outro dono, e o critério foi o
mesmo das passagens anteriores — fechar o que dá de conferir sem abrir o
aplicativo. Quatro buracos de honestidade no Rust e quatro correções na casca.

### Casca (front), conferida na árvore nesta passagem

- **Chave dos turnos** (`Conversation.tsx:26-32`, contador em `:53-59`): a lista
  era desenhada com o índice do array por chave. Como o turno cresce durante o
  streaming e é recortado quando a compactação entra, o React emprestava o
  `<details>` já aberto de um turno para o outro — o resumo de um aparecia
  desenrolado dentro do de outro. Cada desenho nasce com a sua e a leva enquanto
  existir.
- **Títulos reais** (`RichText.tsx:144-149`): `h1`…`h6` saíam todos com o mesmo
  visual. Agora cada nível tem a sua classe (`rich-titulo-1`…`rich-titulo-6`), e
  o que o modelo escreve como cabeçalho aparece como cabeçalho.
- **Menu acessível** (`AppMenu.tsx:117`, `:142`): os botões de ícone só ganharam
  `aria-label` — sem ele, leitor de tela anuncia botão vazio num menu que é a
  porta de onze telas de ajuste.
- **Bancada sem estado próprio** (`BancadaDialog.tsx:6-13`): a folha era dona da
  medição e se inscrevia no evento por conta própria; fechar a tela perdia a
  rodada em curso. O estado subiu para o `App` (`BancadaRodada` vive em
  `src/lib/types.ts`, para o arquivo não fechar ciclo de import), porque o núcleo
  continua mandando resultado mesmo quando ninguém está olhando.

### Núcleo: o número que a tela mostra passou a ser o número que aconteceu

- **COMPACTAÇÃO** — `dobrar()` cortava a ponta mais antiga do trecho em silêncio
  e o turno dizia que tinha dobrado `ate - desde` mensagens: o aplicativo
  garantia uma memória que o modelo não leu. Vermelho medido, com a implementação
  ainda parada no corpo inteiro:
  `o_que_nao_cabe_fica_de_fora_e_o_numero_diz_o_que_foi_lido` →
  **`assertion left == right failed; left: 5, right: 3`** (cinco mensagens de 100
  tokens, régua de 300 → três foram lidas). `apara_ate_caber`
  (`compactacao.rs:98`) tokeniza, mede e solta a ponta mais antiga até caber, e
  devolve quantas sobraram; `dobrar` repassa o número (`compactacao.rs:123`,
  agora `(String, Generation, usize)`). A medição entra por callback, então a
  régua se confere sem placa (`medir_por_corpo`, nos testes de `:246`/`:256`/`:269`).
  Na tela: `Resumo` ganhou `descartadas` (`session.rs:90`, calculada em `:617`
  como `(ate - desde) - lidas`) e o cartão do turno mostra
  `conversa.resumoDescartadas` em aviso quando é maior que zero. O `ate` ficou
  como está, de propósito: recostá-lo no prefixo lido faria o `encaixe` girar
  pedindo o resumo de um corpo vazio.
- **MCP** — um servidor que nasce mudo (o script certo esperando uma entrada que
  nunca chega) pendurava `conectar` para sempre: a tela girava sem devolver erro
  nenhum. `PRAZO_CONEXAO` de 45 s (`manager.rs:721`) e `dentro_do_prazo`
  (`manager.rs:731`) cobrem o aperto de mão e a descoberta (`:354`, `:361`), e o
  erro diz **qual** servidor ficou calado. Vermelho medido antes do helper:
  **5,02 s** de espera contra a régua do teste (50 ms); depois, **0,06 s**.
  Cortar pelo meio é seguro nos dois transportes, e isso foi lido no fonte
  instalado, não presumido: a futura stdio carrega o `TokioChildProcess`, cujo
  `Drop` (`ChildWithCleanup`, `rmcp 3.10.0 transport/child_process.rs`) mata o
  filho; no HTTP não há processo. **A nota de reconhecimento que dizia que uma
  conexão fracassada vazava um `node.exe` vivo é falsa** — o defeito real era a
  falta de prazo, e fica registrada assim para ninguém caçar o fantasma. Na
  mesma passagem, `desligar` (`manager.rs:633`) derruba o servidor publicado e os
  conectores, e entra no gancho de saída (`lib.rs:242-247`) com 5 s de teto
  (`PRAZO_DESLIGAR`, `:253`) para um teimoso não segurar a janela fechada. Que o
  gancho dispara antes do `cleanup_before_exit()` (registro do cargo,
  `tauri-2.12.5/src/app.rs:1449-1452`) e que o
  runtime do `async_runtime` é do processo (daí o `block_on` na saída ser seguro)
  é lido do fonte das crates.
- **SONDAS** — `engine_probe` usava `unwrap_or` para ter número onde o motor não
  devolveu medida: a VRAM "cedida" saía de um palpite e a régua de recorte do KV
  idem. Agora `cedido` é `Option` (`bin/engine_probe.rs:321`, `src/bin/`, não
  `bin/`) e imprime `não medida`, e a rodada 3 **recusa** em vez de medir quando o
  motor não declara o cache ocupado (`:1434`). Uma régua inventada aceita o que
  não deveria.
- **VOZ** — o último aviso de `dead code` do crate era o `prefixo` que `remessas`
  calculava e somava por fora. Fechado na causa (`voz/baixar.rs:110`):
  `total += remessa.a_receber()` — o mesmo método que os testes já conferem — em
  vez de `#[allow(...)]`.

### Guards que trabalharam

O `cargo test --lib idiomas` pegou uma tradução que eu escrevi num bloco de
ferramenta malformado e que nunca chegou ao disco: **`en-US não traduz:
["conversa.resumoDescartadas"]`**, com **555 passaram / 1 falhou**; aplicado o
`en-US.json` de verdade, **556 / 0**. Ou seja: o guard do próprio repositório
provou a edição que eu achava ter feito — e é ele que tem de continuar verde.

### Gates desta passagem

`cd src-tauri && cargo test --lib` → **558 passaram, 0 falharam, 11 ignored,
finished in 4.98 s** (553 + 3 de compactação + 2 de prazo MCP, menos o reuso
dos contados antes). `npm run build` → **`✓ built in 220 ms`**, sem erro de
TypeScript e sem aviso de chunk (`BancadaDialog` no lazy chunk dele, 2,34 kB │
gzip 0,86 kB). `cargo check --all-targets` → **zero avisos**.

**ALEGADO, não medido nesta passagem**: o gancho de saída e o prazo de 45 s
contra um servidor realmente mudo só se provam fechando a janela do aplicativo,
o que estava proibido; os dois testes novos medem o helper com prazos curtos de
teste, não o `conectar` vivo. As sondas exigem placa e editor fechado. Os 11
`ignored` continuam os do aperto de mão TLS, que pedem servidor na rede.

### Recomendações novas (fora de lote, com evidência)

- **`call_tool` sem prazo é escolha, não buraco**: o prazo de uma chamada é o
  prazo do comando (`terminal` roda minutos por design). O que falta é
  cancelamento visível na tela, não um `timeout` que mataria o trabalho do
  usuário.
- ~~**`mcp/servidor.rs` `aceitar`** cria uma tarefa por soquete sem dono nenhum~~
  — **fechado** (`MCP.md`, M5): o `JoinSet` é do laço (`servidor.rs:424`), cada
  soquete entra por `conectar` (`:445`) e cai junto com o servidor porque a
  conexão também seleciona `parada.cancelled()` (`:489`); `drenar` (`:497`) espera
  com teto de 2 s (`:411`) antes de cortar o resto. A pendência tinha nota de "só
  verificável com o app aberto" — falsa: o teste novo usa o `publicador()` da
  suíte e viu o vermelho (1 falhou, 2,13 s) antes do verde.

## Passagem do maintainer sobre a quinta leva (medido 2026-11-20)

Duas frentes, ambas fechadas sem abrir o aplicativo, sem delegação e sem dono
novo de arquivo.

- **MCP M5** — `aceitar` lançava uma tarefa por soquete e não registrava ninguém:
  o `fechar()` esperava só o laço, devolvia "parado", e a conexão aberta antes
  dele continuava sendo atendida, autenticada pelo token que a tela já esqueceu.
  Agora o conjunto é do laço, cada conexão cai com o `CancellationToken` e o fecho
  drena com teto de 2 s dentro dos 5 s da saída (`MCP.md`, M5). O teste novo é
  auto-provado: exige resposta antes do fecho (prova a aceitação), exige a
  conexão de pé depois dela (prova que há o que drenar) e decide pelo que o
  cliente lê, não pelo que a tarefa devolve.
- **FFI #39, continuação** — o `Embutidor` calcula o `Despacho` desde o lote e
  nenhum byte dele chegava à tela: os dois `git grep -c` medidos em `FFI.md`
  (núcleo e frente, na HEAD `3a4dc4c`) não devolvem nenhuma linha. O chip de
  memória dizia "na placa" igual para a carga contada na VRAM livre e para a que
  subiu inteira porque o cabeçalho não fechou — tentativa apresentada como medida.
  O veredito agora atravessa o espelho, o `Estado`, o JSON e a tela, com a
  formulação do `ModelManager` extraída para `previsao()` em `display.ts` em vez
  de copiada (`FFI.md`, "#39 (continuação)"). Nenhuma chave de i18n nova; nenhum
  número inventado quando a conta não pôde ser feita — o chip mostra o motivo
  real do parser, na classe de aviso que o `AdaptadoresDialog` já usa.

Gates desta passagem: `cd src-tauri && cargo test --lib` → **560 passaram, 0
falharam, 11 ignored, finished in 4.28 s** (um teste por item);
`cargo test --lib ai::memoria` → **33, 0,41 s**; `cargo test --lib mcp::servidor`
→ **12, 0,12 s**; `cargo check --all-targets` → **zero avisos**;
`npm run build` → **OK, `✓ built in 182 ms`**, sem aviso de chunk.

O que continua fora do alcance, e não foi tocado: a carga real do codificador e
do LLM (pesos + placa + `llama.dll`), o pino das DLLs com as três bibliotecas em
disco, o `desligar` de 5 s contra um servidor teimoso e qualquer verificação no
aplicativo aberto. Os 11 `ignored` são os do aperto de mão TLS, que pedem servidor
na rede.

## Passagem do maintainer sobre a sexta leva (medido 2026-11-20)

Um item, `CONTRATO`, e ele não começou no código: começou na constatação de que a
regra mais repetida do `CONTRIBUTING.md` — a das travas, `:67-74` — era a única
sem nenhum instrumento. Dossiê completo em `CONTRATO.md`.

- **A regra virou teste.** `src-tauri/src/contrato.rs` não tem uma linha de
  produção: lê o fonte do crate, corta comentários e literais, corta os blocos
  `#[cfg(test)]` (onde o `expect` é instrumento do teste, não defeito), emenda as
  linhas para enxergar a cadeia que o `rustfmt` quebrou e falha o teste listando
  cada `arquivo:linha → fonte`. Vermelho da primeira corrida: **121 travas**. O
  guarda tem um caso que o confere (`o_guarda_ve_o_panic_na_trava_de_dado`) e o
  teste principal exige que a varredura tenha achado os arquivos, porque verde por
  fonte ausente é o falso verde que ninguém procura.
- **Os 121, por três escritores em arquivos disjuntos.** `ai/memoria.rs` (43)
  ficou comigo; o grupo MCP (40) e o grupo voz+conversas (25) e o grupo
  comandos/idiomas/config/lib (13) foram delegados, um escritor por arquivo, cada
  site com a pureza provada. O cache do Whisper é o caso que mais parece
  gambiarra e não é: atrás daquela trava há um **ponteiro cru do sherpa**, e ele
  recupera porque os dois caminhos que o tocam tiram o `Some` com `take` antes de
  chamar `destruir_reconhecedor` — não existe instrução que deixe ponteiro morto
  dentro do cache (`CONTRATO.md`, C2).
- **A exceção que virou correção.** O agente do MCP parou em `manager.rs:412` em
  vez de varrer, e estava certo: `flush_connector` mexe em duas coleções, e o
  meio-state de um panic entre as duas era o permissivo — conector ainda confiado
  com as aprovações apagadas, decidido `Allow` sem card depois de reconectar. A
  correção foi a ordem (`permissions/mod.rs:164`): a confiança cai primeiro, o
  meio que sobra passa a ser o restrito, e a trava volta a ser recuperável. O
  guarda fecha em **zero violações sem lista de exceção**.
- **Regras que este lote quebrou de propósito, e o que aconteceu.** A regra 7
  ("não rodar `cargo` durante o lote, a árvore é compartilhada") não foi
  obedecida: os três escritores rodaram o próprio `cargo check` na mesma árvore.
  Não houve compilação de arquivo pela metade porque os conjuntos de arquivos
  eram disjuntos e cada um conferiu depois de editar — mas é sorte, não projeto,
  e o gate final foi re-corrido por mim sobre a árvore inteira junta, que é o
  número que vale abaixo.

Gates desta passagem: `cd src-tauri && cargo test --lib` → **565 passaram, 0
falharam, 11 ignored, finished in 4.77 s** (eram 560; +2 do guarda, +3 de
recuperação em runtime); `cargo test --lib contrato` → **2, 0,12 s**;
`cargo test --lib conversas` → **27, 3,03 s**; `cargo test --lib voz` → **26 + 1
ignored, 0,25 s**; `cargo test --lib permissions` → **12, 0,00 s**;
`cargo check --all-targets` → **zero avisos, 4.38 s**; `npm run build` → **OK,
`✓ built in 196 ms`**.

Fora do alcance, e não tocado: nenhum envenenamento real foi observado no
aplicativo aberto — o que existe é a thread de teste que panica segurando a
trava. O `flush_connector` com processo morto de verdade, e qualquer crash real
no meio de uma carga de pesos, continuam na rota do motor (descartar e avisar),
que já tem teste próprio (`state.rs:792`).

## Passagem do maintainer sobre a sétima leva (medido 2026-11-20)

Um item, `DORA` (#98, filho da #34), e ele nasceu da medição da passagem
anterior: eu escrevi no `PLANO.md` que "a nota da tela diz a verdade" sem ter
lido a nota no binário. Li. Dossiê em `DORA.md`.

- **A nota mentia sobre o que acontece, e o estado não dizia nada.** A biblioteca
  vendorizada não recusa tensor de magnitude: ela não tem o que ler, monta as
  deltas e segue a vida. Um DoRA entrava pelo mesmo caminho de um LoRA comum e a
  linha da tela respondia "Na placa" para os dois — lido como "o modelo que o
  arquivo descreve está rodando", quando é metade dele. O `grep` de alcance no
  texto do produto (`grep -rn magnitude README.md locales/ CONTRIBUTING.md docs/`)
  devolve, fora as quatro linhas que este lote escreveu, **nada**: o README não
  fala de DoRA, e a frase errada vivia em dois lugares — a nota do chip e o
  comentário do tipo em `types.ts`.
- **A decisão entrou pelo estado, não pelo motor.** `LE_MAGNITUDE`
  (`ai/ggml/mod.rs:159-172`) declara o que a biblioteca lê, e `Item::magnitude_fora`
  (`ai/adaptadores.rs:341-372`) grava por adaptador **aplicado** que a
  normalização ficou de fora. O motor não mudou uma linha: `usar_adaptadores`
  responde `aplicada`/`erro`, e quem tem a ficha para interpretar isso é o
  `adaptadores`. O pin órfão passa pela mesma função, não por um `false` copiado.
- **A capacidade virou instrumento.** `ai/ggml/mod.rs:1025-1069` lê o
  `llama.dll` vendorizado byte a byte: os dois símbolos com que a ponte fala
  têm de estar no arquivo (sem eles a comparação seria um ponteiro LFS ou outra
  release), e os três nomes de tensor de magnitude não podem estar. Clone sem
  DLL diz "ausente: nada contra o que conferir", o mesmo acordo de
  `catalog.rs:492`.
- **Vermelho por mutação, porque não há DoRA para montar.** Não existe aqui um
  adaptador DoRA real contra pesos, então o que foi virado foi a hipótese: com
  `LE_MAGNITUDE = true` — exatamente "a biblioteca passou a ler magnitude" — os
  dois instrumentos caem, cada um com a frase que diz o que revisar. A constante
  voltou para `false`, e o diff do arquivo fecha em 61 linhas adicionadas e
  nenhuma alterada: a mutação não deixou rastro.
- **Frente delegada, um escritor por arquivo.** `types.ts`, o dialog e os dois
  JSON de locale foram para um agente com contrato fechado (`magnitudeFora` no
  JSON, `semMagnitude`/`semMagnitudeNota` nos dois idiomas); o Rust ficou comigo,
  e nenhum dos dois tocou o arquivo do outro.

Gates desta passagem: `cd src-tauri && cargo test --lib` → **567 passaram, 0
falharam, 11 ignored, finished in 4,03 s** (eram 565; os dois novos são o
instrumento do binário e o estado do DoRA); `cargo test --lib adaptadores` →
**13 passaram, 0 falharam, 0,03 s**; `cargo check --all-targets` →
**zero avisos**; `npm run build` → **OK, `✓ built in 216 ms`**, maior bloco
`react` a 210,53 kB, sem aviso de chunk. Paridade de locale medida nos dois
arquivos: **1007 chaves achatadas** de cada lado.

Fora do alcance, e registrado como tal: o efeito de um meio DoRA na resposta do
modelo não foi medido — é string de binário, não execução, e pesos na placa são
o portão. E a outra metade do título da #34 continua aberta: **montar** a
magnitude não tem API nesta biblioteca (`llama_set_adapters_lora` soma deltas
escaladas no grafo; a normalização por linha pede o peso já fundido), então
fechar isso é um conversor que funda adaptador em GGUF, conferido contra um DoRA
conhecido — frente nova, não um número a mais na tela.

## Passagem do maintainer sobre a oitava leva (medido 2026-11-20)

Sem frente delegada: dois restos que a leitura da sétima deixou apontados, e a
re-medição de uma recomendação que estava escrita com números de outra árvore.

- **A prosa que saiu da tela levou o que ela usava.** `servidores.catalogoNota`
  (dos dois JSON) e `.sheet-hint` (de `src/styles.css`) não tinham nenhum
  consumidor — `grep -rn catalogoNota src src-tauri/src` e
  `grep -rn "sheet-hint" src` devolviam, cada um, só o próprio arquivo. Cortados
  os dois lados do locale na mesma batida, a paridade medida passou de 1007 para
  **1006 chaves achatadas de cada lado**, e o guarda dela
  (`idiomas.rs:150 todos_os_idiomas_tem_exatamente_as_mesmas_chaves`) continua
  verde. Detalhado em `FRONT.md`, F7.
- **A8 deixou de ser conta e passou a ser instrumento.** A recomendação citava
  100 ocorrências em 14 arquivos, número da quarta passagem; re-medido hoje:
  **18** literais `expect("… envenenad…")` no crate, dos quais **16** dentro de
  `#[cfg(test)]` (o teste envenena a trava de propósito) e **2** comentário de
  doc. Em produção, **zero**: `contrato::testes::trava_de_dado_puro_recupera_do_envenenamento`
  varre os 65 `.rs` de `src-tauri/src`, corta o que é bloco de testes e exige que
  nenhuma aquisição de `.lock()`/`.read()`/`.write()` seja seguida de
  `expect`/`unwrap` — verdir com o crate inteiro sob os olhos, não com a minha
  conta de `grep`. A recomendação fechou por aí.
- **Um `grep` que quase entrou no doc por engano.** Ao medir as 18 linhas, a
  primeira passada contava 65 ocorrências de `envenenad` — porque
  `unwrap_or_else(|envenenado| …)`, a recuperação que a regra pede, usa a mesma
  palavra no nome da variável. A conta que importa é a do literal de `expect`, e
  o que decide produção x teste é o `#[cfg(test)]` do arquivo, lido um por um.

Gates desta passagem (árvore com os três cortes de arquivo): `cd src-tauri &&
cargo test --lib` → **567 passaram, 0 falharam, 11 ignored, finished in 4,79 s**;
`cargo test --lib contrato` → **2 passaram, 0 falharam**; `cargo check
--all-targets` → **Finished, zero avisos**; `npm run build` → **OK, `✓ built in
196 ms`**, maior bloco `react` a 210,53 kB, sem aviso de chunk. Nada do Rust
mudou: `git diff --stat` fecha em 3 arquivos e 8 linhas removidas.

## Passagem do maintainer sobre a nona leva (medido 2026-11-20)

Frente `CATALOGO` (#27), um escritor: `ai/catalog.rs`. Dossiê em
`CATALOGO.md`. É a frente que dava para fazer inteira sem abrir o app e sem
carregar peso na placa, porque o que ela precisava conferir estava em dois
lugares lidos por rede: a árvore de arquivos do HuggingFace e o cabeçalho GGUF
de cada arquivo.

- **Entraram nove variantes em duas famílias de 2026**: Qwen3.5 4B (Q4_K_M,
  Q5_K_M, Q6_K, Q8_0), Qwen3.5 9B (Q4_K_M, Q5_K_M, Q6_K) e Gemma 4 12B It
  (Q4_K_M, Q6_K). `CATALOG` 35 → **44**, `SHA256_PESOS` 4 → **7** repositórios,
  `SHA256_APOIO` 2 → **5**. Nenhum número veio de cartão de repositório: bytes e
  `lfs.oid` do `tree/main`, e arquitetura, licença, janela, EOS e template do
  corpo servido por `Range: bytes=0-16777215`.
- **A leitura que quase passou errada.** O primeiro parser devolvia um
  `general.architecture` de 100.663.296 bytes. No GGUF v3 o tipo do par
  chave → valor é `uint32`, não um byte; ler um byte desalinha três e o resto do
  cabeçalho vira lixo convincente — as primeiras chaves ainda saem certas.
- **O portão de entrada é o binário.** `every_entry_is_an_architecture_the_engine_runs`
  procura `"<família>\0"` na `llama.dll` vendorizada de 3.877.376 bytes (arquivo,
  não ponteiro LFS) e passou com `qwen35` e `gemma4` dentro. O que ficou de fora
  tem motivo medido: os GGUFs oficiais da Qwen e do GLM respondem `401` (gated)
  à API anônima, e o `GLM-5.3-Flash` vem em partes — `-00001-of-00006.gguf` —
  que o downloader de um arquivo por entrada não junta.
- **Instrumento novo, vermelho medido por mutação.** `every_size_has_a_routable_baseline`
  ganhou o laço que varre as séries do próprio `CATALOG`: série nova sem
  quant escolhível cai sozinha, sem depender de alguém lembrar de escrever o id
  na lista. Com as duas linhas do Gemma 4 marcadas como não-escolhíveis o teste
  caiu em `catalog.rs:664`; retirada a id da lista explícita, caiu em
  `catalog.rs:672`. Mutações revertidas; diff em 133 linhas adicionadas, 7
  removidas.
- **Cross-check que saiu de graça:** os 12.807.982 bytes de `tokenizer.json` de
  `Qwen/Qwen3.5-4B` têm o mesmo `lfs.oid` (`5f9e4d49…`) dos 12.807.982 bytes de
  `ornith-ai/Ornith-1.5-9B` — lidos um e outro na mesma passada. É a família
  mesma, conferida por dois caminhos.

Fora do alcance e registrado como tal, em `CATALOGO.md`: a carga dos pesos novos
na placa (a conta de `kv_por_token` fechou na passagem #106 — `camadas_de_cache`
e `taxa_kv` leem o array de geometria como o b12387; o que ali ficou como **teto**
eram as 24 camadas recorrentes do `qwen35`, e a passagem #107 as tirou do teto),
qualidade de texto e custo por rodada, e o que o laço de ferramentas entrega num
Gemma 4, cujo template usa marcadores próprios e não o XML que o parser lê.

Gates desta passagem: `cd src-tauri && cargo test --lib` → **567 passaram, 0
falharam, 11 ignored, finished in 5,92 s**; `cargo test --lib catalog` → **27
passaram, 0 falharam**; `cargo check --all-targets` → **Finished, zero avisos**;
`npm run build` → **OK, `✓ built in 250 ms`**, maior bloco `react` a 210,53 kB.
README: a linha do catálogo passou de "35 variantes em duas famílias" para o que
existe hoje.

## Passagem do maintainer sobre a décima leva (medido 2026-11-24)

Frente de conta, um escritor: `ai/alojamento.rs`, com o número novo atravessando
roteador, fio, tela e sonda. Dossiê em `CATALOGO.md`, seção #107. Nenhum peso
foi baixado, nenhuma DLL foi carregada, nenhum aplicativo foi aberto.

- **A fila virou outra frente por medição.** O item em espera era o downloader de
  GGUF em partes. Lido o fonte do b12387 vendorizado, a `llama.dll` já junta os
  splits sozinha — `llama_get_list_splits` em `llama-model-loader.cpp:84` e o
  ramo `n_split > 1` que abre o primeiro `-of-0000N` em `:604-635` —, então não
  faltava loader, faltava só o arquivo. E o único repositório em partes ao
  alcance anônimo, o `GLM-5.3-Flash` UD-Q4_K_XL, são 6 arquivos ≈ 200 GB: não
  cabem na placa de 16 GB nem na prova. O adiamento ficou registrado com o
  motivo medido, não como pendência aberta.
- **O defeito estava no `taxa_kv`.** Cobrando K e V de toda camada `propria`, um
  `qwen35moe` de 40 camadas pagava 81.920 por token quando o motor abre cache em
  dez: 20.480. São as 30 camadas de atenção linear, que no b12387 entram em
  `filter_attn = il < n_layer && !is_recr(il)` (`llama-model.cpp:3332-3338`) e
  por isso não chegam ao `llama_kv_cache`. O mesmo cabeçalho, lido por rede, deu
  a regra: `full_attention_interval = 4`, `ssm.conv_kernel` 4, `ssm.state_size`
  128, `ssm.group_count` 16, `ssm.inner_size` 4096 e **nenhuma** chave
  `attention.recurrent_layers` — então o período vem do intervalo, como em
  `qwen35.cpp:17-22`.
- **O que elas gastam em vez de cache também entrou.** `memoria_recorrente` lê as
  cinco chaves do gated delta net e devolve o mapa de recorrentes mais o custo de
  uma camada — `(conv-1) × (inner + 2 × grupo × estado) + estado × inner`, em
  f32 (`llama-hparams.cpp:229` e `:257`, tipos em `llama-model.cpp:3368-3370`) —
  e `n_rs_seq = 0` (`llama-context.cpp:4421`) é o que faz ser uma linha por
  sequência, fixa com a janela. O veredito agora desconta esse estado do orçamento
  da placa antes de empilhar camada, o roteador soma estado na conta de RAM do
  caso sem placa, e a sonda conta estado no esperado do delta de VRAM.
- **Os quatro números conferidos em teste:** A3B 20.480 por token e 65.863.680
  bytes (62,83 MiB) de estado; Qwen3.5-4B 32.768 e 52.690.944 (50,25 MiB);
  Granite 81.920 e estado 0, Gemma 4 344.064 e estado 0. Que os densos não tenham
  se movido é metade do teste — os 320,00 MiB do Granite em 4096 continuam
  batendo com o que a biblioteca escreveu na carga.
- **Vermelho medido por mutação, duas mutações revertidas.** Tirar
  `&& !c.recorrente` de `taxa_kv` derruba 4 testes e deixa 30 verdes; tirar
  `.saturating_add(estado)` do predicado de `Inteira` em `alojar` derruba 1 e
  deixa 33 verdes — é o teste que distingue o número que decide do número que
  enfeita.
- **As citações voltaram a apontar para a árvore.** Crescer `alojamento.rs`,
  `router.rs`, `engine_probe.rs`, `memoria.rs`, `ggml/mod.rs`, `types.ts` e
  `display.ts` moveu os `arquivo:linha` dos dossiês, e cada um foi re-lido na
  árvore de hoje: `CORE.md` tem `taxa_kv` em 445, `alojar` em 626,
  `bytes_do_tensor` em 486, `por_camada` em 507 e o teste do candidato sem ficha
  em 620; `MEM.md` tem `dois_escritores_do_indice_nao_se_atropelam` em 2672;
  `FFI.md` tem o teste do arquivo real em 967-1023, `Despacho` em `types.ts:240`,
  `despacho` em `types.ts:888` e `previsao()` em `display.ts:147`; `DORA.md` tem
  a capacidade lida do binário em 1025-1069 e `magnitudeFora` em `types.ts:951`;
  `BANCOS.md` tem `tlsVerificado` em `types.ts:680` e `:697`; e aqui mesmo
  `router.rs:195` passou a dizer onde o ramo está hoje (`:199`) e as sondas
  voltaram para `engine_probe.rs:321` e `:1434`.

Fora do alcance e registrado como tal, em `CATALOGO.md`: o estado recorrente
**em carga** — os 62,83 MiB do A3B saem da fórmula aplicada ao cabeçalho, e quem
os confere é `engine_probe alojar:` com o peso no disco, o que pede placa e
aplicativo aberto; a entrada do `Qwen3.5-35B-A3B` no catálogo, que ainda não tem
como dizer honestamente que uma `UD-Q4_K_XL` é a quant que o portão do binário
reconhece; `swa_full = false`; e o veredito de chamada de ferramenta por família.

Gates desta passagem: `cd src-tauri && cargo test --lib` → **582 passaram, 0
falharam, 11 ignored, finished in 4,91 s**; `cargo test --lib alojamento` → **34
passaram, 0 falharam**; `cargo check --all-targets` → **Finished em 3,57 s, zero
avisos**; `npm run build` → **OK, `✓ built in 173 ms`**, maior bloco `react` a
210,53 kB.
