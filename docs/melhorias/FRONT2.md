# FRONT2 — a casca do front (segunda passagem)

Frente FRONT2. Arquivos tocados: `src/App.tsx`, `src/components/GrafoNeural.tsx`,
`src/components/ServerDialog.tsx`, `src/lib/api.ts`, `vite.config.ts` e este doc.
Nenhum `locales/*.json`, nenhum `README.md`, nenhum `.rs`, nenhum outro
componente foi editado. Nenhum app aberto, nenhuma GUI, nenhum engine-probe.

O contrato de trabalho tem sete itens. Seis são a lista de follow-ups de
`docs/melhorias/FRONT.md:114-132` (prosa, marca, tooltip, `ResizeObserver`,
poda de `requests`, exports mortos); o sétimo, o code-splitting, vem de
`PLANO.md:217` ("Chunk JS >500 kB") — a lista de `FRONT.md` não o contém, e o
sétimo item daquela lista (F16, `FRONT.md:133-136`, maquinaria de Job/slider
duplicada) não é desta frente. Todos os sete estão fechados abaixo; dois
ficaram com a metade que pertence a outro dono de arquivo pendente (FU1: a
chave de locale que ficou órfã; FU6: os comandos Rust que continuam expostos
sem chamada na frente).

## Verificação: o que é executável nesta árvore

- **Não há runner de teste no front.** Comando e saída reais:
  `node -e "console.log(Object.keys(require('./package.json').scripts).join(' '))"`
  → `dev build preview tauri`. `package.json:6-11` confirma: não existe `test`,
  nem vitest/jest/testing-library em `dependencies`/`devDependencies`. Conforme o
  brief, **não** acrescentei runner. O que executou de verdade nesta frente foi:
  `npm run build` (`tsc && vite build`) e greps contra `HEAD` e contra a árvore.
- **`npm run build` rodado três vezes**, na ordem abaixo, cada saída colada ao
  pé da letra em FU7: ANTES (baseline da árvore de `HEAD`), MEIO (só o import
  dinâmico, sem agrupamento) e DEPOIS (config final). O `tsc` faz parte do
  build (CONTRIBUTING:14 "tipo quebrado é teste quebrado") e passou nas três.
- Red-first do único item com gate numérico (FU7): o vermelho medido é o aviso
  `Some chunks are larger than 500 kB` com `index-*.js = 621,93 kB`; ele
  **persistiu** no passo do meio (`543,44 kB`, ainda com aviso) e só sumiu no
  passo final. Não há teste automatizado que eu pudesse ter escrito antes sem
  adicionar dependência — o que há é a medição do build, que é o que o item pede.
- Red-first dos demais itens: a falha está registrada pelo grep contra `HEAD`
  (colado em cada item) e a correção pelo mesmo grep contra a árvore. A
  verificação comportamental desses quatro itens (prosa, poda, tooltip,
  reenquadramento) foi **leitura de código + type-check, não execução** — sem
  runner e sem abrir o app, não há como executá-los, e eu não chamo leitura de
  teste.

---

## FU1 — prosa embaixo do campo (era F7, `FRONT.md:116-117`)

**Antes (medido):** `git grep -n "catalogoNota" HEAD -- src` →
`HEAD:src/components/ServerDialog.tsx:199: <p className="sheet-hint">{t("servidores.catalogoNota")}</p>`.

**Depois:** a linha saiu. `src/components/ServerDialog.tsx:198-199` agora é

```tsx
198        <p className="sheet-sep">{t("servidores.catalogo")}</p>
199        <ul className="server-rows">
```

**Por quê:** `catalogoNota` é frase explicativa
(`locales/pt-BR.json:190` "Instalados pelo Aether nesta máquina, sem serviço de
terceiro."), e o achado registrado diz exatamente isso — `FRONT.md:116-118`:
"é texto explicativo, não controle/estado. Avaliar remoção ou conversão em
estado", com a ressalva de que os `sheet-note` de estado vazio são estado
legítimo. O contrato deste lote é a lista de follow-ups, e nela a tela fica com
controles e estado; o caminho e o passo-a-passo vivem no README. A alegação já
está no README, que é o lugar dela: `README.md:3-5` — "não há chamada a serviço
de terceiros em nenhum caminho de execução". Remover a linha não perdeu
informação; removeu a duplicação dela no lugar errado. O `sheet-sep` de cima
ficou: é título de seção, é estrutura.

**Consequência registrada (fora do meu arquivo):** `servidores.catalogoNota`
passou a chave órfã nos dois locales, e `.sheet-hint` (`src/styles.css:1436`)
ficou sem uso — `grep -rn "sheet-hint" src` depois da edição só devolve o
arquivo de CSS. Não edito locale nem CSS: o maintainer decide se apaga a chave
ou se ela vira nota de README; o CSS é a única regra de `.sheet-hint` da casa.

## FU2 — marca hardcoded no cabeçalho (era `FRONT.md:119-121`)

**Depois:** `src/App.tsx:798-800`

```tsx
798            {/* A marca é uma string visível como qualquer outra: vem do locale,
799                pela mesma chave que o hub do grafo já usa. */}
800            {t("ligacoes.aether")}
```

**Por quê:** CONTRIBUTING:42-45 — "toda string visível vem de
`locales/pt-BR.json` e `locales/en-US.json`". O precedente citado no contrato é
`GrafoNeural.tsx:983` (`{t("ligacoes.aether")}`, linha re-derivada depois das
minhas edições naquele arquivo), que é exatamente o mesmo nome, medido nos dois
idiomas: `ligacoes.aether` = `"Aether"` em pt-BR **e** em en-US (lido dos
arquivos, não presumido). **Nenhuma chave nova** — reutilizar a existente é a
regra 5 do contrato (`PLANO.md:28-31`: "reutilizam chave existente ou registram
a chave necessária"), e a casa já pratica reuso entre seções: o grafo chama
`t("servidores.adicionar")` (`GrafoNeural.tsx:834`).

O `<title>Aether</title>` de `index.html:6` continua literal: janela do WebView,
não string de interface traduzida, e o arquivo não é meu.

## FU3 — `0 ms` inventado no tooltip do fio (era `FRONT.md:122-124`)

**Antes (medido):** `git grep -n "ms ?? 0" HEAD -- src` →
`HEAD:src/components/GrafoNeural.tsx:937: : \`${linha.rotulo} · ${pulso.ms ?? 0} ms\``.

**Depois:** `src/components/GrafoNeural.tsx:924-932` e `:956-960`

```tsx
924                /// O tempo do fio é o que o núcleo mediu e devolveu no
925                /// `toolResult`/`memoria`. Enquanto a sinapse dispara não há
926                /// número ainda — e o traço é o que a casa já usa para "não
927                /// medido" (RuntimeDialog, ToolCard). Um `0 ms` inventado
928                /// diria "instantâneo" justamente para a chamada em curso.
929                const medida =
930                  pulso === undefined || typeof pulso.ms !== "number"
931                    ? null
932                    : `${pulso.ms} ms`;
...
957                        {pulso === undefined
958                          ? linha.rotulo
959                          : `${linha.rotulo} · ${medida ?? "—"}`}
```

**Por quê:** `Disparo.ms` é opcional (`GrafoNeural.tsx:20`, `ms?: number`) e
`App.tsx:334` põe `ms: undefined` justamente no ramo `toolCall` — o `?? 0`
disparava enquanto a ferramenta estava em curso: a tela afirmava "0 ms" para uma
chamada que ainda não tem tempo. CONTRIBUTING:16-22 proíbe número inventado em
tela, e a regra do contrato é explícita (`FRONT.md:122-124`: "mostrar só o
rótulo quando não há medição real, em vez de um zero fabricado"). O traço é o
estado vazio; a unidade `ms` só aparece colada num número que o núcleo devolveu.

**Estado vazio explícito, não texto novo:** o traço `"—"` é a convenção já
medida da casa para "valor não medido". O grep depois da minha edição —
`grep -rn '"—"' src` — devolve 6 ocorrências, uma delas a minha:
`BarraDeTrabalho.tsx:56`, `GrafoNeural.tsx:959`, `RuntimeDialog.tsx:322`
(`${t("runtime.medido")}: ${arquivo.medido || "—"}`), `ToolCard.tsx:428`, mais
dois comentários que documentam a mesma escolha (`BarraDeTrabalho.tsx:40`,
`RuntimeDialog.tsx:292`). Numa leitura anterior, mais cedo no mesmo lote, a
mesma consulta devolvia também `MemoriaDialog.tsx:632`, `ModelManager.tsx:471`,
`VozDialog.tsx:457`: os números de linha desses arquivos são de outras frentes,
que estão editando a árvore agora, e mudam debaixo do doc — o que importa aqui é
o fato estável, o traço já é a resposta da casa para "não medido". Nenhuma chave
nova, nenhum idioma a quebrar.

**Verificação:** leitura e type-check — `typeof pulso.ms !== "number"` afunila
`number | undefined` sem cast. **Não houve execução**: sem runner no projeto e
sem abrir o app, não há como renderizar um `<title>` de SVG aqui. Registro isso
como o que é — verificação por leitura de código. Uma divergência do achado,
dita na cara: `FRONT.md:124` sugere "mostrar só o rótulo". Ficou rótulo +
traço, porque o traço é a convenção medida da casa para "não medido" e porque
esconder o segmento deixaria o fio em curso igual ao fio que nunca disparou.

## FU4 — reenquadramento da lente sem `ResizeObserver` (era `FRONT.md:125-127`)

**Antes (medido):** `git grep -c "ResizeObserver" HEAD -- src` → saída vazia
(zero ocorrências em todo o `src`).

**Depois:** `src/components/GrafoNeural.tsx:781-802` — o efeito de ajuste ganhou
a função `reenquadrar`, a chamada imediata (comportamento antigo preservado) e o
observador com `disconnect` na limpeza:

```tsx
781  useEffect(() => {
782    if (!ajustada) return;
783    const caixa = quadro.current;
784    if (caixa === null) return;
785    const reenquadrar = () => {
786      setZoom(
787        limitar(
788          Math.min(
789            1,
790            caixa.clientWidth / LARGURA,
791            caixa.clientHeight / mundoHoje.altura,
792          ),
793          AJUSTE_MINIMO,
794          ZOOM_MAXIMO,
795        ),
796      );
797    };
798    reenquadrar();
799    const observador = new ResizeObserver(reenquadrar);
800    observador.observe(caixa);
801    return () => observador.disconnect();
802  }, [ajustada, central, mundoHoje]);
```

**Por quê:** a moldura `.grafo-quadro` muda de tamanho sem gesto nenhum — a
janela do app foi arrastada, a lista de conversas abriu ao lado (`.app--chats`
troca a grade para `268px minmax(0, 1fr)`, `styles.css:94-96`). Antes, o
`clientWidth` só era relido quando `ajustada`/`central`/`mundoHoje` mudassem:
redimensionar deixava o grafo cortado até o próximo clique. O observador é a
única forma de ler esse evento, e o `disconnect` na limpeza é o que impede o
callback de sobreviver ao desmontar (o grafo é montado/desmontado a cada troca
de palco).

**Por que não loopa:** o `ResizeObserver` observa o *border box* da moldura; o
`setZoom` mexe na `.grafo-lente` dentro dela, não na moldura. A única
retroalimentação possível é barra de rolagem roubando altura útil, e ela é
decrescente (caixa menor → zoom menor → conteúdo menor), com piso em
`AJUSTE_MINIMO` (`:378`): converge, não oscila. **Isso é raciocínio sobre o
código, não medição** — mediria-se arrastando a janela do app, o que este lote
não faz.

## FU5 — poda do mapa `requests` (era `FRONT.md:128-129`)

**Depois:** `src/App.tsx:205-222` (dentro de `refresh`, logo depois de
`setPending(nextPending)`):

```tsx
212      setRequests((current) => {
213        const vivos = new Set(nextPending.map((entrada) => entrada.id));
214        const proximo: Record<string, Record<string, unknown>> = {};
215        for (const [id, args] of Object.entries(current)) {
216          if (vivos.has(id)) proximo[id] = args;
217        }
218        // Nenhuma chave fora: devolve o objeto atual para não reacender render.
219        return Object.keys(proximo).length === Object.keys(current).length
220          ? current
221          : proximo;
222      });
```

**Por quê:** `App.tsx:443-451` (`approvalRequested`) insere uma entrada por aprovação
e nada a removia — o mapa crescia uma chave por aprovação da vida do processo,
inclusive as resolvidas há horas. A poda fica no `refresh` porque é o único
lugar que conhece **todas** as formas de morrer do pedido: resolvido
(`resolveApproval` → `await refresh()`, `App.tsx:704` e `:717`), expirado no
núcleo, derrubado com o conector, e cada um dos pontos que já chamam
`void refresh()` / `await refresh()`: todo evento MCP (`App.tsx:453`,
`onMcpEvent`, evento `mcp-events`/`mcp-event` declarado em
`src/lib/api.ts:165`), o atraso de fila (`App.tsx:459`, `api.ts:171`) e o fim de
turno (`App.tsx:530`, no `finally` de `enviar`). Poda no clique de resolver cobriria um caso
só — seria duas políticas pela metade em vez de uma inteira.

**Corrida (lida no código dos dois lados):** o medo é um snapshot antigo apagar
os argumentos de um pedido recém-chegado — sobretudo porque o mesmo manipulador
que insere em `requests` chama `void refresh()` em seguida (`App.tsx:453`), e se
o `pending_approvals` daquele refresh ainda não trouxesse o id a entrada seria
podada no ato e o cartão nasceria sem argumentos. Não acontece, e isso tem
evidência lida do lado Rust: em `src-tauri/src/mcp/execucao.rs` o
`insert_pending` (linha 144) vem **antes** do `send(McpEvent::ApprovalRequested)`
(linha 156); o evento é a última coisa que acontece antes de a chamada ficar
suspensa, então qualquer snapshot pedido depois dele já contém o id. Somam-se a
isto o token monotônico herdado do F3 (`App.tsx:171` é o `useRef`, `:174` o
incrementa, `:200` e `:235` são as duas portas que descartam snapshot velho),
que impede que um refresh mais velho escreva qualquer estado, e o fato de o
cartão e a poda saírem do mesmo `nextPending` — nunca há `pending` vivo com
`requests` podado. **Nada disso foi executado**: sem runner e sem abrir o app, a
ordem acima é leitura das duas árvores, linha por linha.

**Retorno `current` quando nada mudou:** sem isto, cada refresh criaria um objeto
novo e reacenderia o `ApprovalCard` a cada evento MCP. É a mesma preocupação que
fez `App.tsx:445` usar `setRequests((current) => …)`.

## FU6 — exports mortos de `src/lib/api.ts` (era `FRONT.md:130-132`)

**Varredura (executada, não presumida):** script que lê `src/lib/api.ts`, extrai
as 91 chaves do objeto `api` e procura cada nome, com fronteira de palavra, nos
34 arquivos `.ts`/`.tsx` restantes de `src` (arquivo temporário fora do
repositório; nada foi adicionado à árvore). Saída literal:

```
ANTES (api.ts de HEAD):   chaves=91 | arquivos varridos (src sem api.ts)=34 | sem consumidor: connectorTools, toolCatalog, resolveIntent, chatHistory, vozFalar
DEPOIS (api.ts da árvore): chaves=87 | arquivos varridos (src sem api.ts)=34 | sem consumidor: vozFalar
```

E o grep simples, depois da edição:

- `grep -rn "toolCatalog\|resolveIntent\|chatHistory\|connectorTools" src` →
  **sem saída** (exit 1).
- `git grep -n "toolCatalog\|resolveIntent\|chatHistory\|connectorTools" HEAD -- src`
  → 4 ocorrências, todas em `src/lib/api.ts:53,54,55,80`.

**Removidos:** `connectorTools`, `toolCatalog`, `resolveIntent` (HEAD
`api.ts:53-56`) e `chatHistory` (HEAD `api.ts:80`). Na árvore atual,
`src/lib/api.ts:52-53` é `connectedConnectors` → `allTools`, colados.

**Vivo — ficou, como manda o contrato:** `bancosDescobrir`
(`src/lib/api.ts:107-108`) tem consumidora real:
`src/components/BancosDialog.tsx:154` — `await api.bancosDescobrir(...)`. Não
toquei.

**Achado extra da varredura, deixado de propósito:** `vozFalar`
(`src/lib/api.ts:114`) não tem nenhuma menção fora de `api.ts`. Não removi: está
fora dos cinco nomes do meu contrato e há outros quatro agents na árvore agora —
se algum estiver ligando um botão de fala neste lote, eu quebro o build deles
para cumprir uma linha de doc. Fica registrado com a evidência acima para o
próximo passo (ou para quem estiver editando `VozDialog.tsx` neste).

**Pendente do lado de fora (registrado, não executável por mim):** os quatro
comandos seguem registrados no Tauri — `src-tauri/src/lib.rs:153,154,155,191`
(`commands::connector_tools`, `commands::tool_catalog`,
`commands::resolve_intent`, `commands::chat_history`) com as implementações em
`src-tauri/src/commands.rs:106,111,116,773`. Retirar o invólucro da frente
removeu o último chamador, não a superfície de IPC: `chat_history`, por exemplo,
ainda responde a qualquer código que alcance `invoke`. `src-tauri/` é intocável
nesta frente. Quem cuida de dead code no Rust é a auditoria do núcleo — o item
que o plano registra hoje é A9 (`PLANO.md:208-209`), e ele trata de
`Motor::versao`/`Ggml::versao`, não destes quatro comandos: esses ainda não têm
item próprio no plano, e é isso que esta linha deixa como pendência.

## FU7 — code-splitting: os chunks medidos (era `PLANO.md:217`, "Chunk JS >500 kB")

### ANTES — `npm run build` na árvore de `HEAD`

```
vite v8.3.8 building client environment for production...
✓ 302 modules transformed.
dist/index.html                   0.39 kB │ gzip:   0.26 kB
dist/assets/index-C93aHfGe.css   66.25 kB │ gzip:  11.93 kB
dist/assets/index-BaktST_d.js   621.93 kB │ gzip: 185.17 kB

[plugin builtin:vite-reporter]
(!) Some chunks are larger than 500 kB after minification. Consider:
- Using dynamic import() to code-split the application
- Use build.rolldownOptions.output.codeSplitting to improve chunking
```

Tamanho lido no disco: `dist/assets/index-BaktST_d.js = 621.930 bytes` — um
único arquivo, e todo dialog e o grafo dentro do que o app baixava ao abrir.
(Este é o mesmo aviso que `PLANO.md:12-13` e `PLANO.md:234` já mediram; o número
acima é a minha leitura de agora, não a transcrição do plano.)

### MEIO — só o import dinâmico, `vite.config.ts` intocado

```
✓ 303 modules transformed.
dist/assets/IdiomaDialog-CbmhmMAp.js         1.16 kB │ gzip:   0.48 kB
dist/assets/PastasDialog-D3R527I4.js         2.19 kB │ gzip:   0.81 kB
dist/assets/BancadaDialog-DtfJ7y9O.js        2.89 kB │ gzip:   1.07 kB
dist/assets/AdaptadoresDialog-BpDQslT1.js    5.36 kB │ gzip:   1.59 kB
dist/assets/RuntimeDialog-BCaZDoPa.js        5.54 kB │ gzip:   1.78 kB
dist/assets/GeracaoDialog-DO6YEJ6I.js        6.22 kB │ gzip:   1.79 kB
dist/assets/BancosDialog-DQWRNczJ.js         7.81 kB │ gzip:   2.19 kB
dist/assets/ModelManager-BUPyODnR.js         8.14 kB │ gzip:   2.67 kB
dist/assets/VozDialog-BNiAkcM2.js            8.60 kB │ gzip:   2.41 kB
dist/assets/MemoriaDialog-BStn38I3.js       11.20 kB │ gzip:   3.19 kB
dist/assets/ServerDialog-Zwsq0XG2.js        11.20 kB │ gzip:   3.20 kB
dist/assets/GrafoNeural-q62p9h4m.js         13.20 kB │ gzip:   4.74 kB
dist/assets/index-Fjz8G6La.js              543.44 kB │ gzip: 168.79 kB

(!) Some chunks are larger than 500 kB after minification.
```

O passo do meio **ainda estava vermelho**: −78,49 kB no chunk de abertura, aviso
presente. É ele que prova que o import dinâmico sozinho não fecha o item — o
resto do arquivo não é código de dialog, é react-dom, as duas tabelas de idioma e
o markdown.

### DEPOIS — `vite.config.ts:15-50` (`rolldownOptions.output.codeSplitting.groups`)

```
✓ 303 modules transformed.
dist/index.html                              0.71 kB │ gzip:   0.34 kB
dist/assets/index-C93aHfGe.css              66.25 kB │ gzip:  11.93 kB
dist/assets/rolldown-runtime-XKyaHtl9.js     0.72 kB │ gzip:   0.42 kB
dist/assets/vendor-Dowfqfn-.js               1.14 kB │ gzip:   0.55 kB
dist/assets/IdiomaDialog-DueIFc9H.js         1.19 kB │ gzip:   0.50 kB
dist/assets/PastasDialog-DTjKpIrt.js         2.27 kB │ gzip:   0.85 kB
dist/assets/BancadaDialog-UIfnNwep.js        2.97 kB │ gzip:   1.11 kB
dist/assets/AdaptadoresDialog-C4J8K1yP.js    5.44 kB │ gzip:   1.63 kB
dist/assets/RuntimeDialog-BKdpsY6s.js        5.62 kB │ gzip:   1.81 kB
dist/assets/GeracaoDialog-BxSBa3Fe.js        6.30 kB │ gzip:   1.83 kB
dist/assets/BancosDialog-BEYT464R.js         7.89 kB │ gzip:   2.23 kB
dist/assets/ModelManager-BzrlKzzP.js         8.21 kB │ gzip:   2.70 kB
dist/assets/VozDialog-DS__M7l8.js            8.68 kB │ gzip:   2.45 kB
dist/assets/MemoriaDialog-isVXNUWn.js       11.28 kB │ gzip:   3.23 kB
dist/assets/ServerDialog-C_bDdPVa.js        11.28 kB │ gzip:   3.24 kB
dist/assets/GrafoNeural-j0ULWx_w.js         13.28 kB │ gzip:   4.77 kB
dist/assets/index-DaRHUwI-.js              159.08 kB │ gzip:  51.00 kB
dist/assets/markdown-aHERgz_x.js           172.30 kB │ gzip:  51.96 kB
dist/assets/react-NRtzIFUv.js              210.53 kB │ gzip:  65.70 kB

✓ built in 260ms
```

**Nenhum aviso.** Maior arquivo JS: 210,53 kB (react-dom), 58% abaixo do teto de
500 kB.

Bytes lidos do disco com `node` sobre `dist/assets` (script temporário, mesmo
do FU6):

```
aberturas (bytes): index-DaRHUwI-.js=159089 markdown-aHERgz_x.js=172306
                   react-NRtzIFUv.js=210533 rolldown-runtime-XKyaHtl9.js=726
                   vendor-Dowfqfn-.js=1142
total inicial = 543796
chunks sob demanda = 12 total = 84478
```

Fechamento das contas, sem arredondar para favor:

| | ANTES | DEPOIS |
|---|---|---|
| maior chunk JS | 621.930 B | 210.533 B |
| baixado ao abrir (JS) | 621.930 B (1 arquivo) | 543.796 B (5 arquivos) |
| não baixado até o clique | 0 B | 84.478 B (12 arquivos) |
| aviso `>500 kB` | presente | ausente |

Ou seja: **−78.134 B no caminho de abertura (−12,6%)** e 12 módulos que só
existem depois de um gesto. Os 84.478 B que saíram da abertura não somem —
viram `import()` sob demanda; a diferença entre os 84.478 B e os 78.134 B
economizados (6.344 B) é cola duplicada nos chunks novos, e é por isso que o
número da tabela não fecha redondo.

**O que mudou em código:**

- `src/App.tsx:36-75` — doze `lazy()` (11 dialogs + `GrafoNeural`). Os
  componentes são exports nomeados, então cada `lazy` recebe
  `.then((m) => ({ default: m.X }))`: é o adaptador que o `React.lazy` exige, não
  um invólucro decorativo — o chunk continua sendo o módulo próprio.
- `src/App.tsx:82-105` — `FronteiraDeCarregamento`: import dinâmico é leitura de
  disco, e um chunk que não vem produz throw no render; sem borda, o React
  desmonta a árvore e a tela fica branca no meio de um turno. A borda devolve
  `message(cause)` na `.fault-bar` que o app já usa (a mesma de `App.tsx:947`),
  sem inventar texto.
- `src/App.tsx:841-851` — o palco do grafo sob `Suspense`, com
  `fallback={<p className="sheet-note">{t("geral.carregando")}</p>}`: estado
  real, chave existente (`geral.carregando`), e a convenção já usada em
  `VozDialog.tsx:211`, `AdaptadoresDialog.tsx:148`, `MemoriaDialog.tsx:265`
  (`grep -rn "geral.carregando" src` → 9 ocorrências; além da minha em
  `App.tsx:842`, sete são `.sheet-note` exatamente assim e a oitava é
  `IntentBar.tsx:37`).
- `src/App.tsx:963-1033` — os onze dialogs sob um único `Suspense
  fallback={null}`: antes do clique não havia cartão na tela, e entre o clique e
  o módulo chegar a moldura vazia é o estado correto — não há meias-cartas para
  mostrar.
- `vite.config.ts:27-48` — três grupos por `test`/`priority` (markdown → react →
  resto de `node_modules`). **Honesto sobre o que este passo faz e o que não
  faz:** agrupar não baixa menos bytes, ele corta o mesmo caminho inicial em
  arquivos que o WebView lê em paralelo e que não se invalidam juntos. O ganho
  de bytes do item veio do `import()`; o ganho de "nenhum arquivo acima de
  500 kB" veio daqui. O `dist/index.html` (0,71 kB) ganhou quatro
  `modulepreload` exatamente por isso.

### Pendente do FU7 que **não** é meu para fechar

O caminho inicial ainda carrega 543 kB, e os dois maiores pedaços dele não
posso mover com os arquivos que tenho:

1. **`react-markdown` + `remark-gfm` + a família `micromark` (172.306 B lidos no
   disco)** só saem do caminho de abertura se `Conversation` for lazy.
   `App.tsx:9` importa `emptyTurn` e `turnsDe` do mesmo módulo
   `src/components/Conversation.tsx`, e as duas funções são chamadas em caminho
   síncrono (`App.tsx:494-495`, `:313`, `:573`). Enquanto as duas morarem lá,
   `Conversation` não pode virar chunk assíncrono. **Correção real:** mover
   `emptyTurn`/`turnsDe` para `src/lib/` e então
   `lazy(() => import("./components/Conversation"))`. `Conversation.tsx`
   é de outra frente — não toquei. Este é o item que mais derruba o número de
   abertura, e o próximo lote tem de ser dele.
2. **As duas tabelas de idioma (`locales/pt-BR.json` 56.992 B +
   `locales/en-US.json` 55.097 B = 112.089 B de JSON fonte, lidos com
   `node fs.statSync`)** entram todas no chunk inicial por
   `import.meta.glob(..., { eager: true })` em `src/lib/idiomas.ts:3-5`. Trocar
   por glob não-`eager` carregaria só o idioma ativo. **Não medi quanto disso
   sobra no arquivo minificado** — o que está medido aqui são os bytes de fonte,
   e é por isso que o número aparece como tamanho de arquivo, não como fatia do
   chunk. `idiomas.ts`
   pertence à frente de i18n (`docs/melhorias/I18N.md` está na árvore agora,
   escrita por outro agent) — não toquei.

---

## Chaves i18n

**Nenhuma chave nova.** Nada aqui precisa de inserção nos locales. As chaves
reutilizadas, conferidas nos dois arquivos:

| chave | pt-BR | en-US | onde entrou |
|---|---|---|---|
| `ligacoes.aether` | `Aether` | `Aether` | `App.tsx:800` (marca do cabeçalho) |
| `geral.carregando` | `Carregando` | `Loading` | `App.tsx:842` (fallback do grafo) |

Uma chave ficou **órfã** por causa do FU1: `servidores.catalogoNota`
(`locales/pt-BR.json:190`, `locales/en-US.json:190`). Não a apaguei — locale não
é meu. Se o maintainer preferir mantê-la viva, o lugar dela é o README (a
alegação já está em `README.md:3-5`).

## O que ficou como follow-up honesto

1. `servidores.catalogoNota` órfã nos dois locales + `.sheet-hint` órfão em
   `src/styles.css:1436` (FU1).
2. `connector_tools`, `tool_catalog`, `resolve_intent`, `chat_history` seguem
   registrados em `src-tauri/src/lib.rs:153,154,155,191` sem chamador na frente
   (FU6; é o terreno da frente A9/dead code no Rust).
3. `vozFalar` (`src/lib/api.ts:114`) é a única chave do `api` sem nenhuma menção
   fora de `api.ts` na árvore de hoje; deixei viva por risco de colisão com a
   frente que está em `VozDialog.tsx` agora (FU6).
4. Os dois blocos que ainda pesam na abertura — `Conversation` (markdown) e o
   glob `eager` de `idiomas.ts` (FU7, com o pré-requisito de cada um).
5. `index.html:6` mantém `<title>Aether</title>` literal — janela do WebView,
   fora do contrato de strings visíveis traduzidas e fora dos meus arquivos.
6. FU4 e FU5 não têm como serem executados sem runner nem sem abrir o app: a
   verificação deles foi leitura de código + `tsc`. Se o projeto aceitar um
   runner um dia, os dois são os primeiros casos de teste que valem a pena
   (reenquadrar com caixa mudada; podar `requests` com `pending` esvaziado).
7. A extração de maquinaria de Job/slider compartilhada (F16,
   `FRONT.md:133-136`) continua não feita, e um `FronteiraDeCarregamento` a mais
   não ajuda: se ela acontecer, a borda de chunk pode entrar junto.
