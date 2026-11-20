# FRONT — interface (lote de melhorias)

Frente FRONT. Arquivos exclusivos tocados: `src/App.tsx`, `src/components/Chats.tsx`,
`src/lib/api.ts`, `src/components/GrafoNeural.tsx`, `src/components/MemoriaDialog.tsx`,
`src/components/VozDialog.tsx`, `src/components/ServerDialog.tsx`. Nenhum locale,
nenhum `.rs`, nenhum outro componente foi editado.

As linhas abaixo foram re-derivadas na passagem de 2026-11-20, depois das frentes
FRONT2/FRONT3 e da leva de contratos, que mexeram o `App.tsx` e os dialogs por
dentro. Cada âncora é lida no arquivo de hoje (`grep -n` do símbolo citado), não
copiada desta página.

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
  igual ao padrão já existente em `:82` (nova), `:193` (apagar) e `:213` (limpar).
  Honra o contrato do próprio arquivo (`:9-11`: "a tela não oferece um botão que ele
  não vai honrar").
- `src/App.tsx:643` — `abrirConversa` recusa com `if (talking) return;` antes de
  qualquer efeito. Cobre quem chamar o handler direto (a guarda não depende só do
  botão). `talking` é lido do closure do render; `onAbrir` é recriado a cada render
  (`App.tsx:865`), então não há closure velho.
- **Por quê:** o handler de chat-event (`App.tsx:335-442`) anexa cada delta ao
  "último turno assistant" do buffer vivo. `abrirConversa` substitui `turns`
  (`App.tsx:646`); trocar no meio do turno faria o handler escrever na história de
  OUTRA conversa. O núcleo já recusa a troca com o motor ocupado.
- **Verificação:** leitura do fluxo `ocupado={talking || calling}` (`App.tsx:863`) →
  Chats; `talking` ligado em `submitQuery` (`App.tsx:570`) e desligado no `finally`
  (`App.tsx:602`). A guarda usa `talking` (não `calling`): só o turno de chat emite
  deltas para `turns`; uma chamada direta de ferramenta (`calling`) não anexa ao
  histórico, então a troca nesse caso é honrada pelo núcleo. tsc OK.

### F2 [MED] Edição otimista da política corre contra a resposta do núcleo
- `src/components/GrafoNeural.tsx:419` — novo `const pedido = useRef(0)`, contador
  monotônico compartilhado por `carregar` e `gravar`.
- `carregar` (`:421-435`) e `gravar` (`:463-481`) recebem `const token = ++pedido.current`
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
- `src/App.tsx:182` — novo `const refreshPedido = useRef(0)`.
- `refresh` (`:184-249`) recebe `const token = ++refreshPedido.current`; depois do
  `Promise.all` (`:198`) e no catch (`:246`) só aplica o snapshot se o token ainda é
  o último.
- **Por quê:** todo evento MCP (`App.tsx:526`), todo lagged (`:532`) e todo fim de
  turno (`:603`) dispara `refresh()` concorrente. Um `Promise.all` mais antigo e mais
  lento podia sobrescrever `pending`/`connected`/`chats`/`capabilities` mais novos.
  A guarda no catch evita que a falha de um pedido já superado acenda a faixa de erro
  quando um refresh mais novo teve sucesso.
- **Verificação:** leitura de todos os chamadores (`void refresh()` e `await refresh()`,
  inclusive `RuntimeDialog onChange={refresh}` — `App.tsx:1095`). Todos passam pela
  mesma função guardada. `useCallback([])` inalterado (`App.tsx:249`); ref estável. tsc OK.

### F4 [MED] `message()` mostrava `"{}"` para um `Error`
- `src/lib/api.ts:210-216` — trata `string`, depois `Error` (`error.message`), depois
  o fallback `JSON.stringify(error)` para as outras formas.
- **Por quê:** `JSON.stringify(new Error(...))` devolve `"{}"`, exibido verbatim em
  toda faixa de falha. A mensagem real do `Error` é o que a pessoa precisa ler.
- **Verificação:** `error instanceof Error` narrow `unknown` para `Error`, `.message`
  é `string` — sem `any`, sem non-null assertion. Ramo string e fallback preservados.
  tsc OK.

### F5 [MED] Rejeições de invoke engolidas nos dialogs
- `src/components/MemoriaDialog.tsx:350` — `onCancelar` agora
  `void agir(() => api.modelDownloadCancel(modelo.id))`, igual a `onBaixar`/`onUsar`/`onExcluir`
  (`:349`, `:351`, `:352`).
- `src/components/MemoriaDialog.tsx:418` — parar varredura agora
  `aoCancelar={() => void agir(() => api.ragIndexarCancelar())}`, no `BarraDeTrabalho`
  que a frente FRONT3 extraiu; o `void agir` é o mesmo de antes da extração.
- `src/components/VozDialog.tsx:111-118` — nova `async function parar()` com
  `setErro(null)` + try/catch `setErro(message(cause))`, espelhando `prever`/`baixar`/
  `cancela`/`apagar`; botão em `:177` passa a `onClick={() => void parar()}`.
- **Por quê:** `void api.…()` sem catch gerava unhandled rejection e nenhum estado de
  erro. `agir` (MemoriaDialog) já faz `setOcupada`/try-catch/`ler`/`onChange`; VozDialog
  usa o padrão try-catch por ação. Uma falha agora aparece na faixa de erro existente.
- **Verificação:** leitura de `agir` (`MemoriaDialog:215-227`) e das funções irmãs de
  VozDialog. Nenhuma string nova; o caminho de erro já existe. tsc OK.

### F6 [MED] Cancelar instalação mapeava falha real para "nada para cancelar"
- `src/components/ServerDialog.tsx:147-158` — `cancelar` troca
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
`servidores.nadaParaCancelar` (`locales/pt-BR.json:197`), `conversas.abrir`,
`memoria.parar`, `voz.pararFala`. Nenhum locale foi editado (arquivo compartilhado).

## Follow-ups: o que esta passagem deixou, e onde cada um está hoje (lido 2026-11-20)

- **Prosa na UI (F7) — fechado nesta passagem: saiu a prosa e saiu o que ela
  usava.** `servidores.catalogoNota` foi cortada dos dois JSON ao mesmo tempo
  (`locales/pt-BR.json` e `locales/en-US.json`, mesma linha `:194` antes do corte)
  e `.sheet-hint` saiu de `src/styles.css` — as duas ausências medidas antes do
  corte: `grep -rn "sheet-hint" src` devolvia só a regra do próprio CSS, e
  `grep -rn catalogoNota src src-tauri/src` devolvia zero linhas. Paridade
  conferida depois: `idiomas.rs:150 todos_os_idiomas_tem_exatamente_as_mesmas_chaves`
  verde, e o total achatado passou de 1007 para **1006 chaves de cada lado**. Os
  `sheet-note` de estado vazio continuam legítimos e intocados: `App.tsx:920` é
  "carregando" e `App.tsx:973-977` escolhe entre `app.semServidor` e
  `app.semFerramenta` — estado, não explicação.
- **Marca hardcoded — fechado.** `App.tsx:874-879` renderiza `{t("ligacoes.aether")}`
  (`:878`), pela mesma chave que o hub do grafo usa (`GrafoNeural.tsx:983`).
- **Tooltip de 0 ms fabricado — fechado.** `GrafoNeural.tsx:930-932` só monta
  "N ms" quando `typeof pulso.ms === "number"`; sem medição o rótulo diz o que
  existe, não um zero inventado.
- **ResizeObserver ausente — fechado.** `GrafoNeural.tsx:799`
  (`new ResizeObserver(reenquadrar)`) reenquadra a lente quando a caixa muda; os
  `clientWidth` de `:790` e `:811` deixaram de ser a única fonte do enquadramento.
- **Poda do mapa `requests` — fechado.** A declaração é `App.tsx:117` e a poda
  mora no próprio `refresh` (`App.tsx:223-233`): recorta quem não voltou no
  snapshot e devolve o objeto atual quando nenhuma chave saiu, para não reacender
  render. No `refresh` e não no clique porque só ele conhece todas as formas de
  morrer de um pedido (resolvido, expirado, conector derrubado).
- **Exports mortos de `api.ts` — auditado, e resta um caso com dono.** O objeto
  `api` tem 78 métodos e os quatro candidatos desta página saíram: `toolCatalog`,
  `resolveIntent`, `chatHistory` e `connectorTools` não ocorrem em `src` nem em
  `src-tauri/src`. `bancosDescobrir` tem consumidor real
  (`BancosDialog.tsx:160`). Um ficou: `vozFalar` (`api.ts:114`) não é chamado pela
  interface, e o comando existe do outro lado (`commands.rs:1374`, registrado em
  `lib.rs:204`). Cortar o wrapper sem cortar o `pub fn` deixa um comando da
  interface pública sem dono; é frente Rust, com o `commands.rs` sob um único
  escritor — não uma linha a menos no `api.ts`.
- **Componente compartilhado (F16) — fechado.** `src/components/BarraDeTrabalho.tsx`
  traz a barra de Job (`BarraDeTrabalho`, `:43`) e a régua (`Regua`, `:139`, com o
  único `type="range"` do front em `:170`). Seis dialogs consomem a barra:
  `AdaptadoresDialog`, `GeracaoDialog`, `MemoriaDialog`, `ModelManager`,
  `RuntimeDialog`, `VozDialog`.