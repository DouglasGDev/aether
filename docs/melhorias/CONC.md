# CONC — a troca de conversa não congela a UI no meio do turno

Dono: frente CONC. Arquivos exclusivos: `src-tauri/src/state.rs`,
`src-tauri/src/commands.rs`. Nada de refatorar a posse do motor neste lote.

## A1 — o defeito (lido no código antes da mudança)

O mutex do motor **é** segurado a geração inteira:

- `state.rs::acquire`, comentário "O guard é segurado durante a geração inteira"
  (linha 431 depois da mudança; ~390 antes). O `MutexGuard` devolvido por
  `acquire` vive até o fim de `run_chat_turn`.
- `commands.rs:1122` `let mut guard = ai.acquire(spec)` e `commands.rs:1181`
  `session::run_turn(&mut loaded.engine, …)` correm sob esse guard, incluindo o
  gate de permissão em `commands.rs:1173` `runtime.block_on(manager.call_tool(…))`.
  Isso contradiz `CONTRIBUTING.md:49-51` ("Lock de motor não é segurado durante a
  geração"). Ver follow-up abaixo — não foi tocado neste lote.

O deadlock **original** de status já estava mitigado: `ai_status` lê `janela` e
`acelerador` em cache (`state.rs:330,425-430`; `commands.rs:626-628`), não pelo
lock do motor. O risco **remanescente** e concreto era outro:

- `conversa_nova` / `conversa_abrir` / `conversa_apagar` eram comandos
  **síncronos** (correm na thread da janela) e faziam um check `is_busy`
  (`commands.rs:806,830,851` antes da mudança) seguido de `esquecer()`
  (`commands.rs:821,836,858`), que tomava `loaded.lock()` bloqueante
  (`state.rs:359-363` antes: `self.loaded.lock().expect(…)`).
- TOCTOU: o comando via `is_busy == false`, um `chat_send` começava nesse gap
  (`begin()` → `acquire()` segura o motor), e o `esquecer()` seguinte esperava o
  lock do motor **o turno inteiro** na thread da janela. A UI congelava; o card
  de aprovação da ferramenta que o turno espera nunca chegava.

## A correção mínima escolhida (opção a + b do brief)

Tornar a troca **atômica quanto ao início do turno**, reutilizando o próprio
portão de início de turno — o CAS `busy` de `begin()` — em vez de inventar um
lock novo:

1. `state.rs::troca_exclusiva` (364-379): reserva o lugar do turno com o mesmo
   `begin()` que `chat_send` usa. Se já há turno, recusa **na hora** com o motivo
   real (`erros.turnoEmAndamento`). Se não há, a reserva impede que um turno
   comece no meio da troca: o `begin()` do `chat_send` derrota no CAS e devolve
   antes de gerar. O `end()` fica num `Drop` (struct `Reserva`) para que um panic
   na troca não deixe o flag `busy` preso travando todos os turnos seguintes.
2. `state.rs::esquecer` (392-404): agora usa `try_lock`, **nunca** `lock()`. Motor
   ocupado devolve o motivo real (`erros.motorOcupado`) em vez de bloquear;
   envenenamento continua a panicar como o resto do arquivo. Dentro da reserva,
   o único ocupante possível do motor é uma carga (`preload`) ou aplicação de
   adaptadores — um turno já teria derrotado a reserva.
3. `commands.rs` `conversa_nova` (807-830), `conversa_abrir` (837-849),
   `conversa_apagar` (860-878): o corpo inteiro corre dentro de `troca_exclusiva`.
   O `esquecer()` vem **antes** de criar/abrir/apagar e de tocar no buffer vivo:
   se o motor recusar, nada foi mutado — recusa limpa, sem efeito pela metade.

**Por que não deadlock e não segura o lock na geração.** A reserva é um flag
atômico, não um lock: não cria aresta de ordenação. O `esquecer` usa `try_lock`
(nunca espera) e larga o guard antes de o corpo tomar `history`/`resumo`/
`conversas`. Nenhuma passagem nova segura o motor enquanto toma outro lock — a
ordem `loaded → history` que o turno usa continua a única, e a troca não a
percorre aninhada. O caminho de geração (`acquire`/`run_turn`/gate) fica
**intocado**: o guard continua segurado a geração inteira como antes. A correção
só garante que os comandos de conversa nunca **esperem** esse guard.

**Por que não a opção (c) (async).** Tornar os comandos async tiraria o trabalho
da thread da janela, mas não fecharia o TOCTOU nem a disputa de KV/histórico: a
troca ainda poderia correr no meio de um turno. A reserva atômica resolve a
causa, não só o sintoma, e mantém os comandos síncronos como o resto da tela.

## Testes vermelho-primeiro (`state.rs`, módulo `tests`)

- `o_esquecer_nao_espera_o_motor_no_meio_do_turno` (565): segura `loaded.lock()`
  na thread do teste (simula o turno com o motor preso) e chama `esquecer()`
  noutra thread. **Assertion:** `rx.recv_timeout(5s)` devolve um `Err` (motivo
  real), não expira. *Vermelho antes:* o `esquecer` antigo fazia `loaded.lock()`
  e devolvia `()` — bloqueava o turno inteiro, o timeout expirava (e o tipo `()`
  nem carregava uma recusa). *Verde agora:* `try_lock` → `WouldBlock` → `Err`.
- `a_troca_de_conversa_reserva_o_lugar_do_turno` (590): uma troca segura a
  reserva aberta; o teste tenta `ai.begin()` do lado de fora. **Assertion:**
  `!ai.begin()` enquanto a troca está dentro (um turno não começa no meio dela);
  depois de sair, `ai.begin()` volta a `true`. Prova a atomicidade check-and-act
  que fecha o TOCTOU.
- `a_troca_recusa_com_o_motivo_real_quando_ha_turno` (622): com `begin()` já
  tomado (turno em curso), `troca_exclusiva` recusa. **Assertion:** `Err` cujo
  texto contém "turno", e o corpo da troca **não** roda (`AtomicBool` fica
  false) — nada simulado, sem sucesso falso nem efeito pela metade.

Reprodução ao nível da lógica (flag + guard), sem hardware: o motor real não é
carregado; o guard preso simula a geração. Nenhum teste de hardware foi
necessário, então nada ficou `#[ignore]`.

## i18n

- **Reutilizada:** `erros.turnoEmAndamento` (pt-BR "já há um turno em andamento",
  en-US "a turn is already running") — a recusa quando há turno em curso. Já
  existia e já era usada por estes comandos.
- **Nova (registada para o maintainer; locales NÃO foram editados):**
  `erros.motorOcupado` — a recusa do `try_lock` quando o motor está ocupado por
  uma carga/aplicação (não um turno). Sem chave existente que diga isso com
  verdade; `tr` devolve a própria chave até o maintainer a adicionar (defeito
  visível, nunca texto inventado).
  - pt-BR: `"motorOcupado": "o motor está ocupado agora; espere a carga ou o turno terminar para trocar de conversa"`
  - en-US: `"motorOcupado": "the engine is busy right now; wait for the load or the turn to finish before switching conversations"`
  - Sem placeholders. Entra no objeto `erros` dos dois arquivos.

## Follow-up documentado — a tensão regra-vs-código (NÃO corrigido neste lote)

`CONTRIBUTING.md:49-51` diz "Lock de motor não é segurado durante a geração", mas
`state.rs::acquire` (431) + `commands.rs:1122,1181` seguram o guard a geração
inteira, inclusive sobre o `runtime.block_on` do gate (1173). Este lote removeu o
congelamento da UI nos comandos de conversa sem tocar esse modelo. Honrar a regra
exige uma decisão de design, com duas opções:

1. **Handle do motor possuído pelo turno.** `acquire` carregaria/devolveria um
   handle que o turno leva para dentro de `spawn_blocking` e larga ao terminar;
   o `Mutex` cobriria só a carga/troca de modelo, não a geração. `ai_status` e os
   comandos de conversa passariam a ler o estado sem disputar a geração. Custa
   refatorar a posse do motor (`Engine`/`LoadedModel` hoje vivem dentro do
   `Mutex<Option<…>>`) e re-ver o gate, que hoje corre sob o guard.
2. **Atualizar a regra.** Assumir que o guard é mesmo segurado a geração inteira
   (é o que serializa um turno por vez sobre o KV) e reescrever
   `CONTRIBUTING.md:49-51` para descrever o contrato real: o que não pode esperar
   o motor são os comandos síncronos da UI, que leem estado em cache
   (`janela`/`acelerador`/`loaded_id`) ou, como agora, recusam com motivo real.

A opção 1 é a que honra a regra como escrita; a opção 2 é a que descreve o código
como ele é. A escolha pertence ao maintainer — forçar a refatoração da posse do
motor dentro deste lote seria arriscado e está fora do escopo cirúrgico pedido.

## O que foi lido para confirmar

- `state.rs` inteiro (506→~660 linhas): `begin`/`end`/`is_busy` (CAS `busy`),
  `esquecer`, `acquire` (guard segurado), caches `janela`/`acelerador`/`loaded_id`.
- `commands.rs`: os três comandos de conversa, `chat_send` (`begin`→`spawn_blocking`
  →`run_chat_turn`→`end`), `run_chat_turn` (acquire/run_turn/gate sob o guard),
  `ai_status` (lê cache), `bancada_rodar` (mesmo portão `begin`).
- `conversas.rs:203,228,249,325`: `estado`/`criar`/`abrir`/`apagar` devolvem
  `anyhow::Result` (o `?` dentro da closure compõe com `troca_exclusiva`).
- `idiomas.rs:122-133`: `tr` cai na própria chave quando ausente — a chave nova
  não quebra a paridade nem o gate; `ggml`/`engine.rs:470` `reset(&mut self)`.
- grep: `AiRuntime::esquecer` só era chamado pelos três comandos (nenhum outro
  caller quebra com a nova assinatura `Result`).

`cargo`/`npm` **não** foram rodados (árvore compartilhada; gate central depois).
Verificação por leitura + testes escritos + raciocínio de ordenação de locks.