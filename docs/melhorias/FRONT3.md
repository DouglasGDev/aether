# FRONT3 — um Job, uma régua (F16)

Frente F16, a que `docs/melhorias/FRONT.md:133-136` deixou expressamente para um
lote depois das correções individuais. Arquivos tocados: `src/lib/jobs.ts`
(novo), `src/components/BarraDeTrabalho.tsx`, `AdaptadoresDialog.tsx`,
`GeracaoDialog.tsx`, `MemoriaDialog.tsx`, `ModelManager.tsx`,
`RuntimeDialog.tsx`, `VozDialog.tsx` e este doc. Nenhum `locales/*.json`,
nenhum `.rs`, nenhum `README.md`. Nenhum app aberto, nenhuma GUI, nenhum
engine-probe, nenhum byte escrito em GPU.

## O que estava duplicado (medido em `HEAD`)

```
$ git grep -n "interface Job" HEAD -- src
HEAD:src/components/MemoriaDialog.tsx:24:interface Job {
HEAD:src/components/ModelManager.tsx:15:interface Job {
HEAD:src/components/RuntimeDialog.tsx:19:interface Job {
HEAD:src/components/VozDialog.tsx:17:interface Job {

$ git grep -c "setJobs\|setFaults" HEAD -- src
HEAD:src/components/MemoriaDialog.tsx:4
HEAD:src/components/ModelManager.tsx:10
HEAD:src/components/RuntimeDialog.tsx:8
HEAD:src/components/VozDialog.tsx:10

$ git grep -c 'type="range"' HEAD -- src
HEAD:src/components/AdaptadoresDialog.tsx:1
HEAD:src/components/BarraDeTrabalho.tsx:1
HEAD:src/components/GeracaoDialog.tsx:2
```

Quatro máquinas de Job, 32 chamadas de `setJobs`/`setFaults`, quatro cursores
de `<input type="range">` escritos à mão fora da régua. Depois:

```
$ grep -rn "interface Job" src
src/lib/jobs.ts:14:export interface Job<F extends string = string> {
$ grep -rn 'type="range"' src
src/components/BarraDeTrabalho.tsx:170       (dentro de `Regua`)
$ grep -rn "<Regua" src | wc -l
5                                              (4 arquivos)
```

## J1 — a máquina de Job vira `src/lib/jobs.ts`

`useJobs<F extends string>()` (`src/lib/jobs.ts:40`) devolve `{ jobs, faltas,
aplicar, limparFalta }`. O evento normalizado (`EventoJob`,
`src/lib/jobs.ts:25`) tem quatro casos — `iniciou`, `bytes`, `fase`,
`terminou` — e a política é uma só:

| evento | o que faz |
| --- | --- |
| `iniciou` | limpa a falta do id e escreve `fase` + `total`, com a medição zerada |
| `bytes` | regrava a medição; a fase vem junto quando o próprio progresso é uma fase (`pesos`) |
| `fase` | troca a fase **e mantém a medição** |
| `terminou` | tira o trabalho da tela; com `erro`, escreve a falta |

`F` é o vocabulário de quem chama, não uma enumeração global: `runtime` sabe de
três fases (`RuntimeDialog.tsx:20`), `voz` de duas (`VozDialog.tsx:19`), o
gerenciador tem a sua e ainda registra o nome do arquivo
(`ModelManager.tsx:18`), a memória só tem `pesos` (`MemoriaDialog.tsx:68`).
Juntar as quatro num enum só mentiria para duas delas; apagar o tipo deixaria
de pegar o nome de fase digitado errado — que o `tsc` ainda pega.

**A única divergência real entre as quatro cópias** estava no `case "extraindo"`
da voz (`HEAD:src/components/VozDialog.tsx`): a runtime espalhava o job
anterior e mantinha os bytes; a voz zerava `done/total/speed`. As duas regras
não podiam coexistir numa máquina só, e a escolha foi a da runtime (manter),
porque a tela que decide o que mostrar já tem `contavel`: tanto
`RuntimeDialog.tsx:272` quanto `VozDialog.tsx:439` passam
`contavel={job.fase === "baixando"}`, e `BarraDeTrabalho.tsx:54` faz
`medido = contavel && total > 0`. Na extração, `medido` é falso — o byte
guardado não vira barra nem percentual. A divergência era invisível porque as
duas telas já escondiam o número do mesmo jeito.

**E havia um vazamento real nessa escolha**, que só apareceu quando a medição
passou a morar num lugar só: `BarraDeTrabalho.tsx:62` publicava a velocidade com
`velocidade > 0`, sem olhar `medido`. Na extração do pacote Kokoro a ponta, que
antes dizia só `—`, passaria a dizer `— · 8,4 MB/s` — uma taxa de rede lida de
outra fase, exatamente o defeito que a leva anterior combateu. Agora a taxa
sobe do mesmo `medido` do percentual. (Em `HEAD` isso nunca aparecia porque a
voz zerava `speed` na troca de fase; o bug estava latente na componente e a
unificação o expunha.)

Os efeitos de cada dialog viraram um `switch` de tradução — o evento da frente
mapeia no evento da máquina — e mantiveram os seus próprios efeitos colaterais
onde eles sempre estiveram: os vereditos obsoletos depois do `concluiu`
(runtime), o `refresh()` do gerenciador, o `onRefresh()` da voz, o
`setErro(event.error)` da memória (`MemoriaDialog.tsx:178-182`, que continua
dono da mensagem de erro da própria linha e por isso não escreve na `faltas`).
`limparFalta` entrou nos dois lugares onde a cópia antiga fazia
`setFaults(… "")` antes de pedir o download: `VozDialog.tsx:122` e
`ModelManager.tsx:154`.

## J2 — a régua vira uma

`Regua` (`src/components/BarraDeTrabalho.tsx:139`) já existia para voz e
memória. Ganhou três portas para caber nos quatro cursores que faltavam, sem
mudar o desenho de nenhum:

- `classe` (`:147`, padrão `"field"`) — o invólucro dos adaptadores é
  `adaptador-forca`, o dos demais é `field`. O CSS dos dois está intacto.
- `rotulo` (`:157`) — o `aria-label` quando precisa ser mais específico que o
  rótulo visível: a força de um adaptador entre dez na lista carrega o nome do
  adaptador (`AdaptadoresDialog.tsx:258`, mesmo texto de antes).
- `mostrar` (`:162`) — o número na linha do rótulo quando o cursor não anda
  sobre ele. A janela de contexto sobe por degraus: o `value` do cursor é o
  índice, o que a pessoa lê é `ajustes.contexto`
  (`GeracaoDialog.tsx:262-264`, `mostrar={ajustes.contexto}` com
  `valor={degrauAtual}`).
- `extra` (`:124`, usado em `GeracaoDialog.tsx:270`) — o chip "janela inteira"
  e o botão que a usa. **Com `extra` o invólucro deixa de ser `<label>` e passa
  a `<div className={classe}>`** (`:182-191`): um `<button>` dentro de um
  `<label>` ativaria o `<input>` do lado, e o HTML antigo da geração já era um
  `div` por esse motivo. O campo sempre passa um fragmento (mesmo quando
  `inteiraDoModelo === null`), então o tipo do elemento não alterna entre
  `label` e `div` a cada resposta do núcleo — alternar seria desmontar o cursor
  e perder o arraste em curso.

Os sete campos de `GeracaoDialog` passaram pelo mesmo `campo` (`:204`), que agora
chama `Regua`; a política de escrita continua a mesma e continua sendo a única:
`onPointerUp`/`onBlur` gravam, o arraste só mexe na tela.

## Gates desta passagem

Medidos nesta árvore, depois de tudo:

```
$ cd src-tauri && cargo test --lib
test result: ok. 558 passed; 0 failed; 11 ignored; 0 measured; 0 filtered out; finished in 6.37s

$ npm run build
npm notice run tsc && vite build
✓ 306 modules transformed.
✓ built in 184ms
```

558 é o mesmo total da leva anterior — nenhum teste nascido, nenhum morto. O
`npm run build` é o único gate do front: não há runner de teste em
`package.json`, e o `tsc` que ele roda tem `noUnusedLocals` e
`noUnusedParameters` ligados (`tsconfig.json:9-10`), o que pagou a conta da
poda (`useState`/`setJobs`/`numero`/`inteiro` que sobraram teriam sido erro).
Nenhum erro de tipo sobrou.

O chunk novo aparece no build: `dist/assets/jobs-*.js = 0,88 kB`, e as quatro
telas seguem em chunks separados (`ModelManager`, `MemoriaDialog`,
`RuntimeDialog`, `VozDialog`), cada um importando a máquina em comum.

**Um coverage que mudou de lugar, registrado porque é perda:** o guarda
`idiomas::toda_chave_pedida_pelo_codigo_existe_em_todo_idioma` varre o código
por chamadas estáticas `t("…")`. Em `HEAD`, `MemoriaDialog` tinha
`t("modelos.pesos")` estática; agora as três fases do gerenciador e a da memória
são lidas por template (`t(\`modelos.${job.fase}\`)`), que o guarda não alcança.
Conferi à mão, e as chaves existem nos dois idiomas: `modelos.preparando`,
`modelos.pesos`, `modelos.apoio` em `locales/pt-BR.json:256-258` e
`locales/en-US.json:256-258`; `voz.baixando`/`extraindo` em
`locales/pt-BR.json:86-87` e `en-US:86-87`; `runtime.baixando`/`extraindo`/
`conferindo` em `locales/pt-BR.json:675-677` e `en-US:675-677`. O guarda passou
mesmo assim, porque as chaves continuam todas presentes.

## O que não foi medido

ALEGADO, e assim fica: nada daqui foi visto num navegador nem no app. A
verificação foi `tsc` + leitura dos diffs contra `HEAD`, linha por linha, com a
marcação explícita de que o HTML de saída não mudou de classe, de estrutura ou
de texto em nenhum dos cinco campos de régua e nas quatro barras. Se a ponta da
extração do Kokoro, o chip da janela inteira ou o `aria-label` de um adaptador
estiverem diferentes na tela, o que falta é abrir o app — não o código desta
passagem, que não toca CSS nem locales.
