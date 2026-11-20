# FRONT — interface (lote de melhorias)

Frente FRONT. Arquivos exclusivos tocados: `src/App.tsx`, `src/components/Chats.tsx`,
`src/lib/api.ts`, `src/components/GrafoNeural.tsx`, `src/components/MemoriaDialog.tsx`,
`src/components/VozDialog.tsx`, `src/components/ServerDialog.tsx`. Nenhum locale,
nenhum `.rs`, nenhum outro componente foi editado.

## Verificação

- **Sem test runner no projeto.** `package.json` só tem `dev`/`build`/`preview`/`tauri`
  (sem vitest/jest/testing-library). Conforme o brief, NÃO adicionei runner. A
  verificação foi por leitura cuidadosa do fluxo real + type-check.
- **`npx tsc --noEmit` (raiz `C:/Aether AI`) → EXIT=0**, sem erros. Comando rodado
  depois de todas as edições; read-only, não escreve dist, não interfere nos agents Rust.
- O gate central (`cargo test --lib` + `npm run build`) NÃO foi rodado por esta frente
  (regra do lote). Nada aqui é dado como pronto sem o gate verde do maintainer.
- `git diff --stat` confirma que só os 7 arquivos da frente mudaram no frontend; os
  `.rs` modificados na árvore pertencem às frentes GATE/SEC paralelas.

## Mudanças (arquivo:linha, por quê, como verifiquei)

### F1 [HIGH] Troca de conversa no meio do turno corrompe história alheia
- `src/components/Chats.tsx:138` — botão `chats-abrir` ganha `disabled={ocupado}`,
  igual ao padrão já existente em `:82` (nova), `:193` (apagar) e `:212` (limpar).
  Honra o contrato do próprio arquivo (`:9-11`: "a tela não oferece um botão que ele
  não vai honrar").
- `src/App.tsx:493` — `abrirConversa` recusa com `if (talking) return;` antes de
  qualquer efeito. Cobre quem chamar o handler direto (a guarda não depende só do
  botão). `talking` é lido do closure do render; `onAbrir` é recriado a cada render
  (`App.tsx:705`), então não há closure velho.
- **Por quê:** o handler de chat-event (`App.tsx:276-349`) anexa cada delta ao
  "último turno assistant" do buffer vivo. `abrirConversa` substitui `turns`
  (`App.tsx:496`); trocar no meio do turno faria o handler escrever na história de
  OUTRA conversa. O núcleo já recusa a troca com o motor ocupado.
- **Verificação:** leitura do fluxo `ocupado={talking || calling}` (`App.tsx:703`) →
  Chats; `talking` ligado em `submitQuery` (`App.tsx:420`) e desligado no `finally`
  (`App.tsx:452`). A guarda usa `talking` (não `calling`): só o turno de chat emite
  deltas para `turns`; uma chamada direta de ferramenta (`calling`) não anexa ao
  histórico, então a troca nesse caso é honrada pelo núcleo. tsc OK.

### F2 [MED] Edição otimista da política corre contra a resposta do núcleo
- `src/components/GrafoNeural.tsx:419` — novo `const pedido = useRef(0)`, contador
  monotônico compartilhado por `carregar` e `gravar`.
- `carregar` (`:421-435`) e `gravar` (`:464-481`) recebem `const token = ++pedido.current`
  no início (síncrono, antes do primeiro await) e só escrevem `base`/`erro` se
  `token === pedido.current` depois do await — tanto no sucesso quanto no catch.
- **Por quê:** `alternar`/`todos` fazem `setBase(novo)` otimista e depois `gravar`;
  a resposta `setBase(estado)` de um pedido mais antigo podia chegar fora de ordem
  e sobrescrever o estado otimista mais novo, divergindo da allowlist que o núcleo
  cobra. Compartilhar o contador entre releitura e escrita também impede que um
  `carregar` (mount/idioma/sinal) atropele um gesto mais novo. No catch, `void carregar()`
  incrementa o token de novo: a releitura do estado real substitui a escrita que falhou.
- **Verificação:** leitura de `gravar`/`carregar`/`alternar`/`todos`/`cortarPasta`.
  O corpo de `gravar` roda síncrono até o primeiro await quando chamado via `void gravar(...)`,
  então o token é atribuído antes de qualquer resposta poder chegar. Dependências dos
  `useCallback` inalteradas (`carregar: []`, `gravar: [carregar, onChange]`); o ref é
  estável. tsc OK.

### F3 [MED] `refresh()` sem guarda de ordem atropela estado mais novo
- `src/App.tsx:112` — novo `const refreshPedido = useRef(0)`.
- `refresh` (`:114-161`) recebe `const token = ++refreshPedido.current`; depois do
  `Promise.all` (`:141`) e no catch (`:158`) só aplica o snapshot se o token ainda é
  o último.
- **Por quê:** todo evento MCP (`App.tsx:376`), todo lagged (`:382`) e todo fim de
  turno (`:453`) dispara `refresh()` concorrente. Um `Promise.all` mais antigo e mais
  lento podia sobrescrever `pending`/`connected`/`chats`/`capabilities` mais novos.
  A guarda no catch evita que a falha de um pedido já superado acenda a faixa de erro
  quando um refresh mais novo teve sucesso.
- **Verificação:** leitura de todos os chamadores (`void refresh()` e `await refresh()`,
  inclusive `RuntimeDialog onChange={refresh}`). Todos passam pela mesma função guardada.
  `useCallback([])` inalterado; ref estável. tsc OK.

### F4 [MED] `message()` mostrava `"{}"` para um `Error`
- `src/lib/api.ts:215-221` — trata `string`, depois `Error` (`error.message`), depois
  o fallback `JSON.stringify(error)` para as outras formas.
- **Por quê:** `JSON.stringify(new Error(...))` devolve `"{}"`, exibido verbatim em
  toda faixa de falha. A mensagem real do `Error` é o que a pessoa precisa ler.
- **Verificação:** `error instanceof Error` narrow `unknown` para `Error`, `.message`
  é `string` — sem `any`, sem non-null assertion. Ramo string e fallback preservados.
  tsc OK.

### F5 [MED] Rejeições de invoke engolidas nos dialogs
- `src/components/MemoriaDialog.tsx:341` — `onCancelar` agora
  `void agir(() => api.modelDownloadCancel(modelo.id))`, igual a `onBaixar`/`onUsar`/`onExcluir`.
- `src/components/MemoriaDialog.tsx:429` — parar varredura agora
  `void agir(() => api.ragIndexarCancelar())`.
- `src/components/VozDialog.tsx:129-138` — nova `async function parar()` com
  `setErro(null)` + try/catch `setErro(message(cause))`, espelhando `prever`/`baixar`/
  `cancela`/`apagar`; botão em `:184` passa a `onClick={() => void parar()}`.
- **Por quê:** `void api.…()` sem catch gerava unhandled rejection e nenhum estado de
  erro. `agir` (MemoriaDialog) já faz `setOcupada`/try-catch/`ler`/`onChange`; VozDialog
  usa o padrão try-catch por ação. Uma falha agora aparece na faixa de erro existente.
- **Verificação:** leitura de `agir` (`MemoriaDialog:220-232`) e das funções irmãs de
  VozDialog. Nenhuma string nova; o caminho de erro já existe. tsc OK.

### F6 [MED] Cancelar instalação mapeava falha real para "nada para cancelar"
- `src/components/ServerDialog.tsx:145-156` — `cancelar` troca
  `await api.mcpInstallCancel(...).catch(() => false)` por try/catch: `false` legítimo
  do núcleo → `t("servidores.nadaParaCancelar", { nome })`; rejeição →
  `setErro(message(cause))`.
- **Por quê:** `.catch(() => false)` transformava qualquer falha real num falso
  "não há nada em curso". Agora a rejeição mostra o motivo real e o `false` genuíno
  continua com a mensagem própria.
- **Verificação:** leitura do handler e do padrão `agir`/`alternar` do mesmo arquivo
  (try/catch com `setErro(message(cause))`). `message` já importado. tsc OK.

## i18n

**Nenhuma chave nova.** Todas as correções reutilizam o caminho de erro existente
(`message(cause)` → faixa de erro) e chaves já presentes:
`servidores.nadaParaCancelar` (`locales/pt-BR.json:190`), `conversas.abrir`,
`memoria.parar`, `voz.pararFala`. Nenhum locale foi editado (arquivo compartilhado).

## Follow-ups deixados (fora deste lote, com evidência lida)

- **Prosa na UI (F7):** `ServerDialog.tsx:199` `<p className="sheet-hint">{t("servidores.catalogoNota")}</p>`
  é texto explicativo, não controle/estado. Avaliar remoção ou conversão em estado.
  (Os `sheet-note` de estado vazio — ex. `App.tsx:810-813` — são estado legítimo, não prosa.)
- **Marca hardcoded:** `App.tsx:719-722` `<span className="brand">…Aether</span>` é
  literal, não vem de locale. O hub do grafo já usa `t("ligacoes.aether")`
  (`GrafoNeural.tsx:961`); a marca do cabeçalho deveria seguir o mesmo caminho.
- **Tooltip de 0 ms fabricado:** `GrafoNeural.tsx:937` `${pulso.ms ?? 0} ms` inventa
  "0 ms" quando `ms` é undefined (sinapse em curso / sem medição). Nada simulado:
  mostrar só o rótulo quando não há medição real, em vez de um zero fabricado.
- **ResizeObserver ausente:** o ajuste de lente do grafo (`GrafoNeural.tsx:778-789`)
  lê `clientWidth/clientHeight` só quando as deps mudam; não há `ResizeObserver` em
  todo o `src` (grep). Redimensionar a janela não reenquadra o grafo até outro gesto.
- **Poda do mapa `requests`:** `App.tsx:54,368` acumula uma entrada por
  `approvalRequested` e nunca remove ao resolver/apagar o pedido — cresce sem poda.
- **Exports mortos de `api.ts`:** auditar e remover/ligar a diagnóstico os exports sem
  consumidor (ex. candidatos `toolCatalog`, `resolveIntent`, `chatHistory`,
  `connectorTools`, `bancosDescobrir`); confirmar uso real antes de remover.
- **Extração de componente compartilhado (F16):** a maquinaria de Job/progresso e o
  slider estão duplicados em vários dialogs (MemoriaDialog, VozDialog, ModelManager,
  RuntimeDialog). Extrair DEPOIS das correções individuais, em lote sequenciado —
  não feito aqui para manter as mudanças cirúrgicas e um escritor por arquivo.