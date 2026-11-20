# CONTRATO — a regra de trava vira teste, e os 121 panics somem

Dono: maintainer, com um agente por grupo de arquivo (três escritores, arquivos
disjuntos). O item é o `CONTRIBUTING.md:67-74`: trava de dado puro recupera do
envenenamento com `into_inner`; só a trava do motor devolve erro. A regra estava
escrita há tempos e nada a cobrava.

## C1 — uma frase em arquivo de regras não segura um `.expect` novo

**Defeito (lido).** Um `Mutex` envenenado continua envenenado para sempre: o
panic de uma thread de fundo marca a trava, e todo comando que chegar depois
dela panica na própria thread. Como os comandos de leitura são **síncronos** e
correm na thread da janela — `ai_status`, `conversas_listar`, `voz_estado`,
`rag_estado`, todos em `commands.rs` sem `async` —, o que um panic ali faz é
fechar o aplicativo inteiro com a conversa intacta escrita no disco ao lado: o
pior desfecho possível para um defeito que o usuário nem provocou.

**A contagem, por dois instrumentos.** Na HEAD anterior à varredura
(`b6cd568`):

```
git grep -E -o "\.(lock|read|write)\(\)\.(expect|unwrap)\(" b6cd568 -- 'src-tauri/src' | wc -l
→ 100
```

Cem é o piso, não o censo: o `rustfmt` quebra boa parte das cadeias em três
linhas (`self.gate` / `.lock()` / `.expect("…")`) e uma busca linha-a-linha
perde todas elas — e ainda conta por cima os `expect` que os testes escrevem de
propósito. O censo é o do guarda, que emenda as linhas de produção antes de
buscar e corta os blocos de teste: na primeira corrida vermelha contra esta
árvore ele imprimiu **121**, cada um como `arquivo:linha → fonte`. A lista abaixo
é a soma dos próprios 121, agrupada pelo guarda.

| arquivo | aquisições que panicavam |
|---|---|
| `ai/memoria.rs` | 43 |
| `mcp/manager.rs` | 21 |
| `voz/escuta.rs` | 8 |
| `conversas.rs` | 8 |
| `commands.rs` | 6 |
| `mcp/execucao.rs` | 6 |
| `voz/mod.rs` | 6 |
| `mcp/servidor.rs` | 5 |
| `mcp/approval.rs` | 4 |
| `mcp/instalador.rs` | 4 |
| `config.rs` | 3 |
| `idiomas.rs` | 3 |
| `voz/fala.rs` | 3 |
| `lib.rs` | 1 |
| **total** | **121** |

Os números por arquivo batem com as três delegações (43 + 40 + 25 + 13 = 121),
cada uma contada pelo próprio agente no arquivo que lhe pertencia.

## C2 — o guarda: `src-tauri/src/contrato.rs`

Um módulo sem uma linha de código de produção, que lê o próprio fonte do crate e
falha o teste quando acha o panic proibido. Colocar a regra no `CONTRIBUTING.md`
não impediu nada: cada frente nova pôde escrever um `.expect("... envenenado")`
sem que nada reclamasse. O guarda é o que transforma a alegação em algo que quebra
o build.

Quatro armadilhas ele tem de acertar para não virar ruído — e cada uma está
escrita no arquivo com a razão:

- **comentário e literal não são código.** `somente_codigo` (`contrato.rs:48`)
  troca strings, `char`, `//` e `/* */` (aninhado) por espaço, **conservando os
  offsets**, para a linha de cada achado continuar sendo a linha do arquivo. Sem
  isso, um `format!("{{{}}}")` dentro de um doc desloca a borda do bloco de
  testes e o guarda passa a ver o que está do lado de lá.
- **o apóstrofo é ambíguo.** `'a` numa assinatura é lifetime, `'x'` é literal de
  `char`; `entre_chars` (`contrato.rs:35`) decide olhando dois caracteres adiante.
  O que ele não reconhece (`'\u{1F600}'`) fica como está e é inofensivo, porque as
  chaves daquele literal são balanceadas.
- **dentro de `#[cfg(test)]` o `expect` é instrumento.** É assim que um teste
  segura o motor de propósito para provar que a UI não espera por ele. `producao`
  (`contrato.rs:166`) conta chaves e corta o bloco inteiro — e a data de
  nascimento do bloco é a linha do `mod`, não a do atributo (o flag `entrou`,
  `contrato.rs:188-197`).
- **o que não é violação.** `.await` no meio da cadeia é trava de `tokio::sync`,
  que não envenena, e `unwrap_or_else(` é justamente a recuperação que a regra
  pede — nem um nem outro casam com a busca por `.expect(`/`.unwrap(`.

E o guarda do guarda: `o_guarda_ve_o_panic_na_trava_de_dado` (`contrato.rs:249`)
dá ao rastreador um fonte em que a aquisição proibida aparece **depois** do bloco
de testes e **dentro** dele, e exige que conte uma só. Se o rastreio de chaves
errar, o teste falha em vez de ficar mudo — um guarda que não vê o arquivo não é
um guarda, é um enfeite.

O teste principal varre `env!("CARGO_MANIFEST_DIR")/src` e exige primeiro que a
varredura achou os arquivos (`arquivos.len() > 30`, `contrato.rs:268`): verde
por ausência de fonte é a forma mais perigosa de falso verde que existe. A
falha imprime cada violação como `arquivo:linha → fonte` e diz o que fazer — a
lista de 121 veio dela.

## C3 — a varredura, grupo por grupo

A política nunca foi duplicada: os três ajudantes de `state.rs:30,36,42`
(`trava_estado`, `trava_leitura`, `trava_escrita`) já existiam e já eram usados
em `state.rs` (29) e `commands.rs` (17) — medidos na HEAD `b6cd568` com
`git grep -c -E "trava_(estado|leitura|escrita)\(" b6cd568 -- 'src-tauri/src'`. O
histórico tem os irmãos dele, `session::trava_historico` e `trava_resumo`, que o
`CONTRIBUTING.md:69` já citava. O trabalho foi levar os 121 sites até eles. Na
árvore de hoje o mesmo comando, sem o commit e somando as colunas
(`… | awk -F: '{s+=$NF} END {print s}'`), dá **167 aquisições em 15 arquivos**.

Cada site convertido teve de ter a pureza provada, não presumida. Os três casos
que pareciam construídos delta a delta e viraram doc no próprio campo:

- `voz/escuta.rs:101` — o cache do Whisper guarda um **ponteiro cru do sherpa**,
  a coisa que menos parece dado puro do crate. Ele recupera porque os dois
  caminhos que o tocam — `na_cache` (`escuta.rs:223`) e `largar`
  (`escuta.rs:197`) — tiram o `Some` com `take` **antes** de chamar
  `destruir_reconhecedor`: não há instrução entre a trava e o valor que deixe um
  ponteiro morto dentro do cache, então o que um panic devolve é `None` ou uma
  instância viva — nunca a destruída, que é a que uma decodificação seguiria
  usando.
- `conversas.rs:104` — o índice troca dois campos em `apagar` (`retain` da lista
  e depois `ativa = None`). Não há valor partido possível: o `índice.json` só é
  escrito depois dos dois, e todo leitor revalida o `ativa` antes de usar.
- `voz/fala.rs` — `pedidos: Mutex<Sender<Pedido>>` não é `Vec` nem `HashMap`, mas
  nunca é reescrito depois de `novo`: atrás da trava envenenada está exatamente o
  mesmo `Sender`, e `send` ou põe o pedido inteiro ou devolve o canal fechado.

Nada de semântica mudou nesses 121: `try_lock` continua `try_lock`, o guard
continua do tamanho que tinha, e nenhuma cadeia ganhou `unwrap` onde tinha
`expect`. O que sumiu foi o panic — e com ele as mensagens em inglês que
`mcp/execucao.rs` e `mcp/manager.rs` ainda carregavam (`"registry lock poisoned"`,
`"permission gate poisoned"`).

## C4 — a única trava que precisou mudar de ordem

O agente que varreu o MCP parou no site que o guarda apontava como
`mcp/manager.rs:412`, em vez de converter, e estava certo: sob aquela guarda,
`flush_connector` mexe em **dois** conjuntos, na ordem errada: removia as
aprovações pendentes do conector uma a uma e só depois retirava a confiança de
`session_grants`. Um panic entre os dois deixava o meio-state perigoso — conector
**ainda confiado** com as aprovações apagadas — e reconectado ele passaria a
decidir `Allow` sem card nenhum. Recuperar a trava ali seria consagrar o meio.

A correção foi trocar a ordem (`permissions/mod.rs:164`): a confiança cai
primeiro. O mesmo panic possível passa a deixar o meio **restrito** — aprovação
órfã de um conector que não tem mais confiança não autoriza chamada nenhuma, e
ela morre sozinha no `expire_pending`. Com a ordem certa, o payload volta a ser
recuperável e o site virou `trava_estado(&self.execucao.gate())` como o resto
(`mcp/manager.rs:410`). O guarda fecha em **zero violações sem lista de
exceção**, que era o objetivo: um contrato com escapamento escrito à mão é o
contrato que ninguém lê.

O caso existe porque `disconnect()` derruba o conector; o teste
`flushing_a_connector_revokes_grant_and_pending` (`permissions/mod.rs:314`) já
exigia o `Ask` depois do flush e continua verde com a ordem nova — a asserção não
diz em que instante cada coleção caiu, e foi isso que deixou a troca livre.

## O que continua panicando, de propósito

- **A trava do motor.** Sob ela os pesos são montados delta a delta, então o
  payload pode estar pela metade: `state.rs:484` trata `TryLockError::Poisoned`
  descartando o motor e devolvendo o motivo real, e não `into_inner`. É o que o
  `CONTRIBUTING.md:72-74` manda e o guarda não pode cobrar — exigir recuperação
  onde a recuperação é errada seria pior que a regra.
- **Os `expect` dentro de `#[cfg(test)]`.** Medido com
  `git grep -n "envenenad" -- 'src-tauri/src' | grep -vE ":[0-9]+: *(//|///)"`:
  **53 ocorrências em 6 arquivos** (`state.rs` 26, `ai/session.rs` 8, `idiomas.rs` 5,
  `conversas.rs` 5, `ai/memoria.rs` 5, `config.rs` 4). São os instrumentos —
  a thread `envenenador` que panica segurando a trava, o nome de cada teste de
  recuperação e as asserções que exigem que a marca continue envenenada — mais a
  recuperação de produção em si: os seis `unwrap_or_else` dos ajudantes — três em
  `state.rs`, três em `ai/session.rs` — e o `descarta_motor_envenenado`. O guarda
  as ignora pelo rastreador de blocos, não por uma lista de arquivos.

Uma consequência lateral da varredura: `git grep -ni poisoned -- 'src-tauri/src'`
não devolve mais nenhum texto de panic em inglês em produção. O que sobra é o
nome da API do `std` (`TryLockError::Poisoned`, `state.rs:484`) e as asserções
`is_poisoned()` dos testes novos.

## Testes novos (5)

| teste | o que prova |
|---|---|
| `contrato::testes::o_guarda_ve_o_panic_na_trava_de_dado` | o próprio guarda: conta a aquisição de produção e ignora a de teste |
| `contrato::testes::trava_de_dado_puro_recupera_do_envenenamento` | o crate inteiro, arquivo por arquivo; vermelho lista `arquivo:linha → fonte` |
| `conversas::testes::o_indice_envenenado_devolve_as_conversas_em_vez_de_panicar` | trava envenenada de verdade (thread que panica segurando o guard): `estado` devolve a conversa e `gravar` continua escrevendo; a **marca** fica envenenada, porque `trava_estado` devolve o valor, não lava a memória |
| `config::tests::os_segredos_envenenados_devolvem_o_token_em_vez_de_panicar` | idem para o `HashMap` de segredos, onde mora o token do servidor MCP publicado |
| `idiomas::tests::o_idioma_envenenado_devolve_a_escolha_em_vez_de_panicar` | idem para o `RwLock<String>` do idioma, lido por `tr` até dentro de comando síncrono da tela |

## Gates

`cd src-tauri && cargo test --lib` → **565 passaram, 0 falharam, 11 ignored,
finished in 4.77 s** (eram 560 na quinta leva; os 5 novos estão na tabela
acima). As famílias tocadas: `cargo test --lib contrato` → **2, 0,12 s**;
`conversas` → **27, 3,03 s**; `voz` → **26 + 1 ignored, 0,25 s**;
`permissions` → **12, 0,00 s**. `cargo check --all-targets` → **zero avisos,
finished in 4.38 s**. `npm run build` → **OK, `✓ built in 196 ms`**.

## Fora do alcance

Nada aqui foi verificado no aplicativo aberto: o envenenamento é provado por
thread que panica segurando a trava, dentro do teste, e não por um crash real do
motor sob carga. O que acontece se um panic de verdade pegar a carga de pesos no
meio continua sendo a rota do próprio motor — descartar e devolver o motivo real
(`state.rs:792`), que é exatamente o caso que o guarda deixa panicar.
