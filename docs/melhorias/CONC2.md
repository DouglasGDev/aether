# CONC2 — a regra do lock do motor: qual caminho foi escolhido e o que ele custou

Dono: frente CONC (follow-up da task #94 de `CONC.md`, seção "Follow-up
documentado — a tensão regra-vs-código"). Arquivos tocados:
`src-tauri/src/state.rs`, `src-tauri/src/commands.rs`, `src-tauri/src/ai/session.rs`,
`CONTRIBUTING.md` e este doc. A8 (recovery de mutex envenenado) fecha aqui, na
parte que é destes arquivos.

## Decisão: opção 2 — reescrever a regra, e consertar o código que a desmentia

`CONC.md` deixou a escolha entre (1) handle do motor possuído pelo turno e
(2) descrever o contrato real em `CONTRIBUTING.md`. A instrução do maintainer foi
prefrir a opção que honra a regra como ela está escrita, **sem forçar uma
refatoração que não fique verde com testes**. Fico com a **opção 2**, por três
razões lidas, não por preferência de gosto:

1. **A opção 1 não cabe nos meus arquivos.** Ela muda a assinatura de `acquire`,
   que hoje devolve `MutexGuard<'_, Option<LoadedModel>>`. O terceiro tomador do
   motor não é meu: `bancada.rs::rodar` (`src-tauri/src/bancada.rs:990`) escreve o
   tipo explícito na variável —
   `let mut guard: MutexGuard<'_, Option<LoadedModel>> = match ai.acquire(escolhida)`
   — e usa `guard.as_mut()` em `bancada.rs:1002` durante a suíte inteira. Trocar o
   guard por handle obriga `bancada.rs` a mudar, e `bancada.rs` está na lista de
   arquivos intocáveis deste lote. Fazer o handle só para o turno de conversa,
   deixando a bancada com o guard, é exatamente o meio-caminho que a tarefa
   proíbe.
2. **O `Engine` mora dentro da trava por um motivo que a opção 1 não resolve.**
   `LoadedModel { engine, template, spec }` está em `Mutex<Option<…>>`
   (`state.rs:144`) e `run_turn` recebe `&mut loaded.engine`
   (`commands.rs:1189`) — um `&mut` emprestado do guard. Um handle possuído pelo
   turno teria de tirar o motor de dentro do `Option` e devolvê-lo no fim, o que
   abre uma janela em que `loaded` está vazio enquanto a placa está ocupada:
   `ai_status` mostraria "nenhum modelo", `aplicar_adaptadores` escreveria vazio
   em `aplicados`, `unload` acharia que não há o que descarregar, e um `preload`
   concorrente carregaria um segundo modelo sobre a mesma placa. Nada disso é
   teórico — são os quatro leitores que hoje se protegem justamente esperando o
   guard. A opção 1 exigiria re-ver os quatro, e a bancada, com teste próprio por
   caminho. Não é trabalho de um lote cirúrgico.
3. **A regra como escrita estava errada, não o código.** `acquire` devolver o
   guard "antes de gerar" (`CONTRIBUTING.md:49-51` antigo) é impossível sem um
   segundo mecanismo de posse: `run_turn` precisa do motor token a token. O que
   existe hoje é a serialização por trava, e ela é o que impede dois turnos de
   escreverem prefixos diferentes no mesmo KV cache — o mesmo motivo pelo qual
   `bancada_rodar` e `chat_send` disputam o CAS de `begin()`. A regra passou a
   descrever isso, com a parte que sempre foi verdade preservada: **o que nunca
   pode esperar o motor é a thread da janela.**

Escolher a opção 2 não é só mexer texto: a regra nova diz que comando síncrono da
UI não espera o motor, e havia **um sítio onde o código desmentia isso**. Ele foi
consertado, com vermelho-primeiro medido (R1 abaixo). Sem esse conserto a opção 2
seria a regra mentindo de outra forma.

## Leilão de risco: quem toma a trava do motor hoje, e quanto cada um espera

Lido em `state.rs`, `commands.rs`, `bancada.rs` na revisão deste lote (linha atual
do arquivo, não a do `CONC.md` — as linhas driftaram quando o lote anterior entrou).
`loaded` é `state.rs:144`.

| # | Tomador | Arquivo:fção:linha | O que espera | Thread |
|---|---------|--------------------|--------------|--------|
| 1 | Turno de conversa, guard da geração | `commands.rs:run_chat_turn:1129` → `session::run_turn` em `1189` | Do `acquire` ao último token, **incluindo** `runtime.block_on(manager.call_tool(…))` de `commands.rs:1180` — o card de aprovação | `spawn_blocking` (`chat_send` é `async`, `commands.rs:901`) |
| 2 | Roteador, carga do modelo inicial | `commands.rs:run_chat_turn:1085` | `count_tokens` + `router::hardware_agora` + `ai.candidatos` (lê cabeçalho GGUF de cada candidato em disco) + `choose`. Guard **largado no fim do bloco** (`1109`), antes do `acquire` de 1129 | `spawn_blocking` |
| 3 | Bancada | `bancada.rs:rodar:990`, usado em `1002` | A suíte inteira: `placa`, `esquecer`, `prefixo`, `parada`, `fluxo`, `dobrar` — várias gerações sob um só guard | `spawn_blocking` (`commands.rs:1760`) |
| 4 | `preload` ("Usar" do gerenciador) | `state.rs:preload:594` → `acquire` | `Engine::load` com os pesos na mão: segundos a minutos (mapeamento GGUF + reserva de KV) | `spawn_blocking` (`commands.rs:755`) |
| 5 | Aplicação de adaptadores | `state.rs:aplicar_adaptadores:313` (trava em `315`) | `usar_adaptadores` somando deltas nos pesos | `spawn_blocking`, recusando antes por `is_busy()` (`332`, `346`, `366`) |
| 6 | `unload` (apagar modelo / trocar janela) | `state.rs:unload:601` → `solta_motor:617` | **Antes deste lote:** `loaded.lock()` bloqueante — o turno inteiro ou uma carga inteira. **Agora:** `try_lock`, recusa imediata | `model_delete` é `async`; **`ai_ajustar` (`commands.rs:639`) e `model_solto` (`767`) são síncronos** |
| 7 | `esquecer` (troca de conversa) | `state.rs:esquecer:497` | `try_lock`, nunca espera (consertado no lote anterior, `CONC.md`) | comandos síncronos `conversa_nova`/`abrir`/`apagar` |
| 8 | `ai_status` | `commands.rs:ai_status:621` | Nada do motor: lê `loaded_id`, `acelerador`, `janela`, `ajustes`, `aplicados`, `is_busy` — caches gravados em `acquire` (`state.rs:573`, `582`, `586`, `589`) | síncrona |
| 9 | Interface que desenha o card | `chat_history` (`commands.rs:773`), `chat_reset` (`778`), `conversa_*` | Só a trava do histórico, tomada e largada por operação (`session::trava_historico`) | síncrona |

Leitura que decide: **os oito primeiros itens esperam o motor fora da thread da
janela, exceto o item 6 antes deste lote.** O único caminho síncrono que ainda
podia esperar o motor a geração inteira era `ai_ajustar` → `ajustar` → `unload` →
`loaded.lock()`. Ele estava escondido exatamente porque `CONC.md` tratou os
comandos de conversa e não a tela de geração. É o R1.

## O que mudou

### `CONTRIBUTING.md` (opção 2, a regra)

O bullet "Lock de motor não é segurado durante a geração" (linhas 49-51 do
arquivo antigo) virou três bullets: o guard é segurado a geração inteira e é isso
que serializa o KV; comando síncrono da UI nunca espera o motor — lê cache ou
recusa com motivo real; e a política de mutex envenenado (A8). A ordem de travas
(`geracao`/`adaptadores` antes do motor, caches depois) entrou escrita, porque é
ela que impede o deadlock clássico e estava só em comentário de `acquire`.

### `state.rs` — o código que faz a regra nova ser verdade

- `solta_motor` (`617`) é o corpo de `unload` com `try_lock`; `unload` (`601`)
  mantém o portão `is_busy` e passa a usar `solta_motor`. Nenhum caminho
  alcançável por comando síncrono espera a trava do motor.
- `ajustar` (`262`) solta o motor **antes** de gravar `geracao.json`, via
  `liberar_motor_para_janela` (`641`): recusa limpa, sem efeito pela metade — a
  mesma política do `esquecer` que abre o corpo de `conversa_nova`. Antes, a
  gravação vinha primeiro e o `unload` bloqueava depois.
- `reservar` (`410`) extrai o CAS de turno que `troca_exclusiva` fazia por
  dentro; `Reserva` (`166`) virou tipo com `Drop` no `end()`, reutilizado pela
  liberação de janela. É o que separa "há turno em curso" (a janela nova entra no
  próximo carregamento, como já entrava) de "o motor está nas mãos de uma carga"
  (recusa com motivo real).
- `trava_motor` (`462`) / `trava_motor_sem_esperar` (`474`): as duas formas de
  tomar a trava, cada uma com a thread onde roda escrita no nome e no doc.
  `acquire` (`548`) ganhou o doc que diz por que o guard é segurado a geração
  inteira, e a ordem fixa de travas.
- A8 na trava do motor: `descarta_motor_envenenado` (`445`) — põe `loaded` em
  `None`, limpa os quatro caches na ordem de sempre, **limpa a marca de
  envenenamento** (`clear_poison`, `459`) e devolve `erros.motorEnvenenado`. A
  marca é parte do contrato: sem ela o descarte não recupera nada, só adia o
  panic para sempre. Foi o que a primeira corrida verde pegou (ver R3/R4).

### `session.rs` e os arquivos da frente — A8

- `trava_historico` (`292`) e `trava_resumo` (`298`): devolvem o valor, não um
  panic. `push` (`275`) e a leitura da conversa viva em `run_turn` (`582`) passam
  por elas, e os 10 sítios de `histórico envenenado` / 6 de `resumo envenenado`
  em `commands.rs` também.
- `state::trava_estado` / `trava_leitura` / `trava_escrita` (`state.rs:30/36/42`):
  a mesma política para os dados de estado puro (`janela`, `acelerador`,
  `loaded_id`, `aplicados`, `geracao`, `adaptadores`, `downloads`, `fichas`,
  `config`, `pastas`, `runtime_job`).
- **Não** se recupera: a trava do motor. O payload sob ela é montado delta a
  delta (`Engine::load`, `usar_adaptadores`) e é segurado por gerações
  (`run_turn`, `bancada::rodar`): um panic ali deixa pesos pela metade, e
  `into_inner` seria continuar a conversa com um modelo que não existe. O motor é
  descartado e o motivo real volta.
- **Não** foi tocado: `gate` (`permission gate poisoned`, 6 sítios em
  `commands.rs`). É a peça de segurança, e os comandos que o leem
  (`permission_mode`, `session_grants`, `grant_session`) não têm `Result` na
  assinatura — devolver erro real mudaria o shape que a interface lê, e recuperar
  silenciosamente uma trava que protege uma allowlist é o oposto do que ela é.
  Fica como follow-up com a razão escrita.

## Vermelho-primeiro, com comando e as duas saídas

Comando dos quatro: `cd "/c/Aether AI/src-tauri" && cargo test --lib -- <filtro>`.

### R1 — `state::tests::a_troca_de_janela_nao_espera_o_motor_na_thread_da_janela`

É o sítio onde a regra nova seria mentira. Prende `loaded` como a geração faz,
chama `ajustar` com outra janela noutra thread e exige resposta em 500 ms, com o
motivo real e nada mutado.

*Vermelho (código do lote anterior, restaurado só para medir — a linha
`solta_motor` voltando a `trava_motor()`, que é o `loaded.lock()` bloqueante de
antes; depois disto a linha voltou a `trava_motor_sem_esperar()`, e o vermelho
abaixo não existe mais no árbol):*

```
COMANDO: cargo test --lib -- a_troca_de_janela
running 1 test
test state::tests::a_troca_de_janela_nao_espera_o_motor_na_thread_da_janela ... FAILED

failures:

---- state::tests::a_troca_de_janela_nao_espera_o_motor_na_thread_da_janela stdout ----

thread 'state::tests::a_troca_de_janela_nao_espera_o_motor_na_thread_da_janela' (24280) panicked at src\state.rs:765:32:
ajustar esperou o motor: um comando síncrono prendeu a thread da janela a geração inteira: Timeout
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 548 filtered out; finished in 0.51s
```

Falhou na barra de 500 ms (`Timeout`), que é exatamente o congelamento; o thread
`ajustar` continuou preso até o guard ser solto, e por isso o teste devolve o
controle em vez de enforcar a suíte.

*Verde:*

```
COMANDO: cargo test --lib -- a_troca_de_janela
running 1 test
test state::tests::a_troca_de_janela_nao_espera_o_motor_na_thread_da_janela ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 548 filtered out; finished in 0.01s
```

Os `0.01s` são o ponto: a recusa sai na hora, sem esperar o guard que ainda está
preso no teste. O que se afirma aqui é o tempo de resposta da thread da janela,
não o de uma geração real — ver "Pendente de medição real na placa".

### R2 — `ai::session::tests::o_historico_envenenado_devolve_a_conversa_em_vez_de_panicar`

Panicar a trava do histórico derrubaria `chat_history`/`ai_status`, que não têm
`Result` na assinatura. O teste envenena a trava num panic real de thread e exige
que `push` escreva na conversa recuperada. Vermelho medido **antes** da correção:

```
COMANDO: cargo test --lib -- envenen
test ai::session::tests::o_historico_envenenado_devolve_a_conversa_em_vez_de_panicar ... FAILED
---- ai::session::tests::o_historico_envenenado_devolve_a_conversa_em_vez_de_panicar stdout ----
thread '<unnamed>' (26308) panicked at src\ai\session.rs:809:13:
um panic segurando o histórico
thread 'ai::session::tests::o_historico_envenenado_devolve_a_conversa_em_vez_de_panicar' (26048) panicked at src\ai\session.rs:276:20:
histórico envenenado: PoisonError { .. }
```

*Verde:* R2 passou na primeira corrida verde (bloco final depois de R4).

### R3 — `state::tests::o_motor_envenenado_devolve_o_motivo_real_e_descarta_o_motor`

Vermelho medido **antes** da correção (o `expect` do `acquire`, `state.rs:442` do
arquivo de antes deste lote):

```
COMANDO: cargo test --lib -- envenen
test state::tests::o_motor_envenenado_devolve_o_motivo_real_e_descarta_o_motor ... FAILED
thread '<unnamed>' (30664) panicked at src\state.rs:645:13:
um panic segurando a trava do motor
thread 'state::tests::o_motor_envenenado_devolve_o_motivo_real_e_descarta_o_motor' (18016) panicked at src\state.rs:442:44:
motor envenenado: PoisonError { .. }
```

*Verde:* passou na segunda corrida (depois do `clear_poison` — ver nota abaixo de
R4, que é a mesma para R3 e R4).

### R4 — `state::tests::o_esquecer_devolve_o_motivo_real_quando_a_trava_do_motor_esta_envenenada`

O `esquecer` do lote anterior tinha `panic!("motor envenenado: {erro}")`
(`state.rs:398` do arquivo antes deste lote) — e ele é chamado por comando
síncrono. Vermelho medido:

```
COMANDO: cargo test --lib -- envenen
test state::tests::o_esquecer_devolve_o_motivo_real_quando_a_trava_do_motor_esta_envenenada ... FAILED
thread '<unnamed>' (30212) panicked at src\state.rs:679:13:
um panic segurando a trava do motor
thread 'state::tests::o_esquecer_devolve_o_motivo_real_quando_a_trava_do_motor_esta_envenenada' (29072) panicked at src\state.rs:398:61:
motor envenenado: poisoned lock: another task failed inside
```

**Verde dos três, com o tropeço honesto no meio.** A primeira corrida verde de
R3 e R4 **falhou**, e a falha é minha, não do árbol: eu descartei o payload e
esqueci a marca. `PoisonError::into_inner()` devolve a trava com o flag de
envenenamento ligado, então o `acquire`/`esquecer` seguinte ainda a achava
suspeita e a suíte inteira recusaria para sempre — o pior que o panic poderia
fazer, que é exatamente o que o doc de `descarta_motor_envenenado` promete
evitar. Faltava `self.loaded.clear_poison()` depois do `*guard = None`. Saída
medida desse meio-termo:

```
COMANDO: cargo test --lib -- envenen
running 4 tests
test ai::session::tests::o_historico_envenenado_devolve_a_conversa_em_vez_de_panicar ... ok
test state::tests::o_esquecer_devolve_o_motivo_real_quando_a_trava_do_motor_esta_envenenada ... FAILED
test state::tests::o_motor_envenenado_devolve_o_motivo_real_e_descarta_o_motor ... FAILED
test ai::ggml::ffi::testes::prender_dlls_devolve_o_motivo_e_nao_envenena_o_processo ... ok

---- state::tests::o_motor_envenenado_devolve_o_motivo_real_e_descarta_o_motor stdout ----
thread 'state::tests::o_motor_envenenado_devolve_o_motivo_real_e_descarta_o_motor' (28192) panicked at src\state.rs:813:9:
a trava do motor continuou envenenada

---- state::tests::o_esquecer_devolve_o_motivo_real_quando_a_trava_do_motor_esta_envenenada stdout ----
thread 'state::tests::o_esquecer_devolve_o_motivo_real_quando_a_trava_do_motor_esta_envenenada' (27976) panicked at src\state.rs:835:9:
a trava continuou envenenada

test result: FAILED. 2 passed; 2 failed; 0 ignored; 0 measured; 545 filtered out
```

Corrigido (`state.rs:459`, `clear_poison`), a corrida verde dos quatro:

```
COMANDO: cargo test --lib -- envenen
running 4 tests
test ai::session::tests::o_historico_envenenado_devolve_a_conversa_em_vez_de_panicar ... ok
test state::tests::o_motor_envenenado_devolve_o_motivo_real_e_descarta_o_motor ... ok
test state::tests::o_esquecer_devolve_o_motivo_real_quando_a_trava_do_motor_esta_envenenada ... ok
test ai::ggml::ffi::testes::prender_dlls_devolve_o_motivo_e_nao_envenena_o_processo ... ok

test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 545 filtered out
```

*(O quarto teste da família é `ai/ggml/ffi.rs`, a outra metade de A8 — arquivo que
não é deste lote, listado aqui só porque cai no mesmo filtro.)*

**Desvio declarado da técnica de referência.** `o_estado_nao_espera_o_motor_da_varredura`
(`ai/memoria.rs:2545`) usa `std::thread::scope`. Aqui o thread chamado **fica
preso de propósito** no estado vermelho, e o `Drop` do scope dá join nesse mesmo
thread: o teste enforcaria a suíte em vez de falhar. R1 usa `std::thread::spawn`
com `recv_timeout(500 ms)` e solta o guard antes do `assert`; o vermelho devolve o
controle e a barra de 500 ms é a da casa.

## Pendente de medição real na placa

Nada neste lote abriu o aplicativo, carregou pesos ou tocou a GPU (proibição
explícita). O que ficou como alegação não medida, e só vira veredito com um turno
real:

- **Quanto tempo um clique da tela de geração espera hoje.** R1 mede a recusa com
  a trava presa por um thread de teste; não mede a geração real. O número que
  falta é quanto dura `run_chat_turn` com o Granite 4.2 3B na placa — é o teto do
  que o `unload` antigo esperava. O veredito de que "500 ms é a barra certa para
  o usuário" continua sendo convenção da casa, não medição.
- **O caminho `ajustar` → liberação de janela com `preload` em curso** (segundos
  de mapeamento GGUF) não foi cronometrado com pesos reais; R1 o simula com a
  trava presa.
- Se a recusa de janela com carga em curso é percepivelmente rápida para quem
  clica, só um `npm run tauri dev` com modelo em disco diria — e abrir o
  aplicativo foi proibido neste lote.

## Chaves i18n

**Nova** (não editei `locales/*.json`; o código chama `tr` e a chave ausente volta
como texto visível, que é o defeito honesto previsto em `idiomas.rs`):

- `erros.motorEnvenenado`
  - pt-BR: `"a trava do motor ficou envenenada por um erro interno; o modelo em memória foi descartado e será carregado de novo no próximo pedido"`
  - en-US: `"the engine lock was poisoned by an internal error; the model in memory was dropped and will load again on the next request"`
  - Sem placeholders. Entra no objeto `erros` dos dois arquivos.

**Reutilizada, já existente nos dois arquivos** (verificado em
`locales/pt-BR.json:845` e `locales/en-US.json:845` — o maintainer já inseriu a
chave que `CONC.md` registrou):

- `erros.motorOcupado` — a recusa de `try_lock` em `esquecer`, `unload` e na
  liberação de janela. O texto atual ("…ocupado carregando ou trocando o modelo…")
  é exato nesses três casos porque a reserva de turno (`reservar`) garante que um
  turno nunca aparece como "carga em curso".
- `erros.turnoEmAndamento`, `erros.turnoEmAndamentoRemover` — sem mudança.

Nenhum evento ou comando mudou de shape: o que a interface lê continua o mesmo.
`aplicar_adaptadores` virou `Result` internamente (método de `AiRuntime`, chamado
só por `state.rs`), e o comando que o envolve já devolvia `Result<…, String>`.

## Follow-ups fora da minha posse

1. **`ai/ggml/ffi.rs:269-271`** — a outra metade de A8 (`expect` no `OnceLock` do
   pino das três DLLs). Não é meu arquivo, e **já foi fechada por outra frente
   enquanto este lote rodava**: `segurar_no_processo`/`pinar`
   (`ai/ggml/ffi.rs:270-318`) agora devolvem `Result<(), String>` com o erro do
   carregador, sem `expect` dentro do `OnceLock`, e há o teste
   `ai::ggml::ffi::testes::prender_dlls_devolve_o_motivo_e_nao_envenena_o_processo`
   — que aparece no filtro `envenen` deste doc. Registro aqui porque a lista de
   A8 em `PLANO.md:206` cobria os dois lugares, e um deles saiu da minha mão por
   posse, não por decisão.
2. **`gate` envenenado** (6 sítios em `commands.rs`) — precisa de decisão de
   design: ou os comandos ganham `Result` (muda o shape lido pela interface), ou
   a trava passa a `parking_lot`/`Mutex` não envenenável. Não é escolha deste lote.
3. **Janela nova com turno em curso continua esperando o próximo carregamento**
   (`liberar_motor_para_janela` devolve `Ok(false)` e `ajustar` grava). É o
   comportamento de antes, tornado visível: `ai_status.janela` mostra a que o
   backend aceitou. Se isso deve virar um aviso na tela ("a janela entra em vigor
   no próximo carregamento"), é decisão de frente de interface + chave nova.
4. **Opção 1 revisitável**, com o preço escrito acima: exige `bancada.rs` na posse
   de quem mexe em `acquire`, um slot de posse com `loaded` visivelmente "em
   geração" para os quatro leitores, e teste por caminho. Se um dia for feita, o
   bullet de `CONTRIBUTING.md` volta a mudar — e é ele que hoje descreve o código.

## Gate

`cd "/c/Aether AI/src-tauri" && cargo test --lib`

O agente parou no limite de turnos sem rodar esta conta. Rodou o maintainer, na
árvore que já tem os arquivos desta frente (`state.rs:266` chama a
`liberar_motor_para_janela` definida em `state.rs:645`), na quarta passagem:

```
test result: ok. 558 passed; 0 failed; 11 ignored; 0 measured; 0 filtered out; finished in 4.98s
```
