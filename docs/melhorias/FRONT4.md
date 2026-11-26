# FRONT4 — a bancada vira atalho da barra (tarefa 117)

Frente FRONT, continuação de `FRONT3.md`. Arquivos tocados: `src/lib/atalhos.ts`
(novo), `src/components/IntentBar.tsx`, `src/components/Conversation.tsx`,
`src/App.tsx`, `src/styles.css` e este doc. **Nenhum `locales/*.json`, nenhum
`.rs`, nenhum outro componente.** `src/components/AppMenu.tsx` não foi preciso
tocar: `TelaAjuste` já era exportado (`AppMenu.tsx:7`) e é dele que o registro
nova. `ResultCard.tsx:17` (o Record de vistas) também não cresceu — nenhuma
`Vista` nova nasceu aqui.

Nada foi aberto: nenhum app, nenhuma GUI, nenhuma medição de tela. O que este
doc descreve é o que está na árvore, lido com `grep -n` nos arquivos de hoje.

## 1. O registro — `src/lib/atalhos.ts` (75 linhas, zero estado)

`listaDeAtalhos(maos)` (`atalhos.ts:35`) devolve seis portas que já existiam:

| `/` | chave do rótulo | ícone | o que aperta |
| --- | --- | --- | --- |
| `bancada` | `bancada.rodar` | `bancada` | `rodarBancada()` (`App.tsx:496`) |
| `parar` | `bancada.cancelar` | `stop` | `pararBancada()` (`App.tsx:508`) |
| `modelos` | `modelos.titulo` | `cube` | `abrirAjuste("modelos")` |
| `bancos` | `bancos.titulo` | `banco` | `abrirAjuste("bancos")` |
| `memoria` | `memoria.titulo` | `memoria` | `abrirAjuste("memoria")` |
| `grafo` | `app.grafo` | `nodes` | `setPalco("grafo")` |

O arquivo não conhece estado nenhum: `Mãos` (`atalhos.ts:28`) é injetado por
`App.tsx:873`, que passa as mesmas funções que o menu já usava. Por isso
`abrirAjuste` deixou de ser `function` e virou `useCallback` (`App.tsx:854`) — a
folha da barra precisa de uma lista que não muda a cada quadro.

Três resolvidores, todos puros:

- `buscaDeAtalho(texto)` (`:48`) — o que foi escrito depois de `/`; um espaço
  devolve `null` (a partir dali a barra voltou a ser texto).
- `resolverAtalho(lista, texto)` (`:56`) — nome exato, para o Enter.
- `buscaDeMencao(texto)` (`:69`) — `/@([^\s@]*)$/` no trecho antes do cursor,
  devolvendo `{ busca, desde }`.

## 2. A barra — `src/components/IntentBar.tsx`

A barra era um `<input>` e nada mais. Agora tem uma folha (`intent-folha`,
`styles.css:649`), aberta para cima porque a barra vive no pé do palco.

**`/`** — `sugestoes` (`IntentBar.tsx:73`) é um `useMemo`: se o pedido começa com
`/`, a lista é `filtrarAtalhos` por prefixo. Escolher um item faz
`onChange("")` e depois `atalho.onDisparar()` (`:81-85`): a barra fica vazia e a
ação acontece — **nenhum turno entra na conversa**. As setas ↑↓ movem, Enter/Tab
escolhem, Escape fecha, clique fora fecha.

**`@`** — a mesma folha, com outro vocabulário: `mencoes` vem do que a tela já
tem (`App.tsx:886`) — as pastas concedidas (`pasta.nome`, com `pasta.caminho` no
`title`) e o modelo em memória (`loaded`). Escolher escreve `@nome ` no lugar do
que foi digitado (`:96-106`) e devolve o cursor para depois do espaço. O backend
não ganha parser nenhum: `chat_send` recebe a menção como texto do pedido.

**Enter sem escolher na folha** — `App.submitQuery` (`App.tsx:563-571`) resolve o
texto digitado contra a mesma lista antes de mandar qualquer coisa: `/bancada`
escrito a mão dispara a bancada, nunca vira mensagem.

**Fechar** — o acordo do menu é o mesmo (`AppMenu.tsx:87-101`): `mousedown` fora
da raiz fecha, e o Escape passa por `useFecharComEscape` (`teclado.ts:9`), cujo
doc já pedia um autocompletar. O handler do campo chama `preventDefault()` antes
de fechar, então a tecla gasta não arrasta nenhuma tela modal junto — é exatamente
o que o comentário do gancho descreve. A folha dispensada de propósito não volta
enquanto o texto não encurtar (`dispensadoEm`, `IntentBar.tsx:64-66,110-112`).

O campo ganhou `aria-expanded` / `aria-autocomplete="list"` /
`aria-controls` / `aria-activedescendant` sobre uma `role="listbox"` de
`role="option"`.

## 3. A linha da rodada — `Conversation.tsx:112-188`

`LinhaDeRodada` é um botão de uma linha (mais um × de dispensar) com chips
`memoria-chip`, já existentes:

| chip | de onde vem |
| --- | --- |
| `Bancada` | `bancada.titulo` |
| `Rodando 3/9` (e `3/9` no fim) | `andando` + contagem de `resultados`; o 9 é `bancadaCasos.length` (`App.tsx:1023`) |
| nome do caso em curso | `rodada.atual`, do evento `caso` |
| `aprovados n` / `reprovados n` / `não rodaram n` | contagem sobre `resultados`, do evento `resultado` |
| `duração 4,2 s` | soma dos `ms` que os próprios eventos `resultado` carregam, formatada por `duracao()` |
| `title` com o modelo | `rodada.modelo`, do evento `iniciou` |

Clique na linha abre a **folha que já existia** (`onAbrirRodada` →
`setShowBancada(true)`); o × só esconde a linha (`rodadaOculta`, `App.tsx:143`) —
a medição continua na folha e o próximo `iniciou` faz a linha voltar
(`App.tsx:498`).

**Fonte de verdade: uma só.** A linha lê `BancadaRodada` do estado
`bancada` (`App.tsx:150`), alimentado apenas pelo ouvinte `onBancadaEvent` que já
existia (`App.tsx:467-491`). Nenhuma consulta nova, nenhum intervalo: `setRodada`
é uma cópia do mesmo objeto, derivada na renderização.

Duas honestidades medidas: (a) `event.total` do `iniciou` é descartado pelo
reducer que já estava lá e `BancadaRodada` não tem campo `total` — `types.ts` não
está na minha posse, então o denominador vem de `bancadaCasos`, a mesma lista que
a folha desenha; (b) o tempo mostrado é a **soma dos `ms` por caso** (cada um
medido em Rust com `inicio.elapsed()`, `bancada.rs:95-103`), não o relógio da
rodada — por isso o rótulo é "duração", e não "no motor".

A linha mora **fora do array `turns`**, no fim de `.talk` (`Conversation.tsx:405`).
Entrar como turno exigiria uma variante nova em `Turn` e tocaria a guarda
`last.role !== "assistant"` do ouvinte de chat (`App.tsx:370`), que hoje anexa
cada delta ao último turno: um turno de bancada no fim da lista engoliria o
streaming. Também por isso `Conversation` passou a ser desenhada com
`turns.length > 0 || rodada !== null` (`App.tsx:1018`) — com a conversa vazia, a
rodada ainda tem onde aparecer.

## 4. O que parou de ser despejado — `Conversation.tsx:282-331`

Antes: `tool.text` cru ia direto para `<pre className="talk-tool-out">`
(`HEAD:src/components/Conversation.tsx:209`), um muro por ferramenta de
terceiro. Agora o bloco é `<details>` fechado por padrão (`:318`) e a linha que
se vê é o cabeçalho que já existia + a medida do corpo (`conversa.caracteres`) +
a seta que gira (`styles.css:1919-1943`).

O que **não** foi escondido, porque a regra do dono é o contrário:

- o bloco de medidas do turno (`.talk-medidas`) está intacto (`:342-393`);
- o cartão de aprovação continua a faixa do palco (`App.tsx:991-995`);
- o cartão do derrame (`ToolCard` vista `resultado`, `ToolCard.tsx:335-355`) é
  uma vista — `cartao === null` é o único caminho que produz `<pre>` colapsável,
  então o derrame continua visível e o `ler_resultado` continua com o id à mão;
- toda vista estruturada continua desenhada aberta.

## 5. CSS

`styles.css`: `.intent { position: relative }` (única regra alheia tocada,
`styles.css:580`), `.intent-folha`/`.intent-item`/`.intent-item-gatilho`/
`.intent-item-nome` (`:649-712`, a mesma moldura do `.app-menu-painel`), e
`details.talk-tool > summary…`, `.talk-tool-medida`, `.talk-tool-seta`,
`.talk-rodada*` (`:1919-1993`).

## Chaves de locale — as sete foram inseridas pelo dono

`CONTRIBUTING.md:42-45` exige os dois arquivos e há o par do `idiomas.rs`
(`todos_os_idiomas_tem_exatamente_as_mesmas_chaves`), então **quem adiciona é o
dono**. Com a chave ausente a tela mostra a própria chave (`idiomas.ts:74`) —
defeito visível, não texto inventado.

| chave | pt-BR | en-US | onde |
| --- | --- | --- | --- |
| `atalhos.titulo` | `Atalhos` | `Shortcuts` | nome acessível da folha de `/` |
| `mencao.titulo` | `Mencionar` | `Mention` | nome acessível da folha de `@` |
| `bancada.aprovados` | `aprovados {n}` | `{n} passed` | chip da linha da rodada |
| `bancada.reprovados` | `reprovados {n}` | `{n} failed` | chip da linha da rodada |
| `bancada.naoRodados` | `não rodaram {n}` | `{n} did not run` | chip, só quando > 0 |
| `bancada.abrirDetalhe` | `Abrir a bancada` | `Open the bench` | `title` da linha da rodada |
| `conversa.caracteres` | `{n} caracteres` | `{n} characters` | medida do corpo cru colapsado |

O inglês inverte o número para a frase ler natural (`3 passed`), o português não:
o que os dois têm de igual é o conjunto de marcadores, e é só isso que o
`os_placeholders_de_uma_chave_sao_os_mesmos_em_todo_idioma` (`idiomas.rs:173`)
conferem. Inseridas em `locales/pt-BR.json` e `locales/en-US.json` como
`atalhos`/`mencao` (seções novas, antes de `compactacao`) e como três entradas de
`bancada` + uma de `conversa`.

Reutilizadas, sem nada novo: `bancada.titulo`, `bancada.rodar`,
`bancada.cancelar`, `bancada.executando`, `bancada.caso.*`, `modelos.titulo`,
`bancos.titulo`, `memoria.titulo`, `pastas.titulo`, `app.grafo`,
`app.pedido`, `cartao.duracao`, `geral.dispensar`, `geral.executando`,
`conversa.aguardando`. `conversa.mostrarTudo` / `conversa.recolher` continuam
livres para quem quiser um interruptor escrito em vez da seta.

## Gate

Rodados depois da última edição desta frente, verbatim:

```
$ cd "C:/Aether AI" && npm run build
vite v8.3.8 building client environment ...
✓ 307 modules transformed.
dist/assets/index-Di02SGls.css              68.18 kB │ gzip: 12.20 kB
dist/assets/index-_fHHy-AN.js              173.89 kB │ gzip: 55.73 kB
dist/assets/react-NRtzIFUv.js              210.53 kB │ gzip: 65.70 kB

✓ built in 193ms

(Trinta segundos antes, o mesmo comando dava `index-BVWkn9Qf.js 173.82 kB` em
`189ms`: o `index` carrega os dois `locales/*.json`, que outra frente está
editando agora — daí o hash e o tamanho derivem entre corridas.)
```

Zero erro TS (`tsc && vite build`), o chunk do React no mesmo ponto da linha de
base (`210.53 kB`) e nenhum dialog mudando de peso: registro, folha e linha da
rodada moram no chunk da abertura, que já carregava a barra. Antes da passagem o
mesmo comando dava `index-*.js 167.63 kB` e `index-*.css 66.36 kB` em `644ms`;
depois, **173.89 kB** e **68.18 kB** — os +6,3 kB são `IntentBar`, `atalhos` e a
folha, e os +1,8 kB de CSS são os blocos novos.

O gate de Rust passou **uma vez** nesta janela, com toda a frente da barra já
escrita na árvore (nenhum `.rs` foi tocado aqui, e a frente não tem como afetar a
compilação de Rust):

```
$ cd "C:/Aether AI/src-tauri" && cargo test --lib
test result: ok. 617 passed; 0 failed; 11 ignored; 0 measured; 0 filtered out; finished in 6.98s
```

Depois disso, duas corridas seguidas pararam **antes dos testes**, com 11 erros de
compilação em `src/bancos/ferramentas.rs`, `src/bancos/motor/mod.rs`,
`src/bancos/motor/sqlite.rs` e `src/commands.rs` (E0061 ×8, E0308, E0599 e um
`character literal may only contain one codepoint`):

```
$ cd "C:/Aether AI/src-tauri" && cargo test --lib
error: could not compile `aether` (lib test) due to 11 previous errors
```

A árvore Rust está sendo editada por outra frente ao mesmo tempo — `git status`
mostra `src-tauri/src/bancos/*`, `src-tauri/src/mcp/servidor.rs`,
`src-tauri/src/bancada.rs` e `src-tauri/src/local/web.rs` (novo) modificados depois
das 00:47, nenhum deles desta frente. O `617 passed; 0 failed; 11 ignored` acima é
o estado medido; quem fecha o backend precisa reconfirmá-lo sobre a própria
mudança. Esta frente não abriu, não corrigiu e não escreveu nada em `src-tauri/`.

**Atenção a quem lê este doc:** outra passagem está escrevendo `FRONT4.md` nesta
mesma janela (a seção de Gate foi reescrita duas vezes). Os valores acima são os
que os comandos desta frente imprimiram, na árvore desta frente.


## O que precisa de olho humano (nada aqui foi visto renderizado)

1. **Geometria da folha.** Ela abre para cima com `max-height: min(46vh, 320px)`;
   com o microfone ativo (`ouvindo`) a barra ganha a régua de nível — pode faltar
   ar entre a folha e a faixa de conversa.
2. **A linha da rodada no fim da conversa.** Ela é o estado vivo da bancada, não
   um marco no tempo: se a pessoa manda outro pedido depois, a linha continua por
   baixo do turno novo. Ancorar a rodada ao turno que a começou é trabalho de
   variante em `Turn` (ver §3) e não coube aqui.
3. **A linha sobrevive a trocar de conversa** — a medição é da máquina, não do
   papo; o × é o único jeito de limpá-la nesta passagem.
4. **Rodada interrompida.** Os chips só existem quando o número existe: parada no
   terceiro caso, a linha diz `3/9` e o que faltou só se vê na folha. Trocar o
   `3/9` por uma contagem de "ficaram de fora" é uma linha, mas é escolha de
   desenho que pede o olho do dono.
5. **Quebra de linha dos chips.** `flex-wrap: wrap` na linha: com os seis chips
   num palco estreito ela vira duas linhas. Pode ser aceitável; não foi visto.
6. **`<summary>` dentro do grid `.talk-tool`.** O alinhamento do cabeçalho
   colapsado com o dos cartões (`styles.css:2673` `.talk-tool > .card`) não foi
   conferido em pixel nenhum.
7. **Leitor de tela.** A lista `combobox`/`listbox` com `aria-activedescendant`
   nunca foi ouvida; só escrita.
8. **Transcrição por voz** regrava o campo inteiro sem passar pelo `onChange`,
   então a folha não se reavalia nesse caminho.
9. **Nome de pasta com espaço** entra como `@Meus Documentos` — texto legível
   para o agente, mas o campo não volta a sugerir dentro dele.
10. **`/bancada` com turno em curso**: o núcleo recusa com
    `erros.turnoEmAndamento` (`commands.rs:1741`) e a faixa de erro mostra o
    motivo; não foi exercido.
11. **As sete chaves** estão inseridas nos dois arquivos; o texto de cada chip
    é do dono, não do modelo, e nunca foi visto renderizado — em particular
    `{n} did not run`, que é mais longo que os vizinhos e pode ser o que quebra
    a linha no item 5.
