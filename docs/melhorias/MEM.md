# MEM — memória e persistência

Frente delegada a um agente, dono único de ai/memoria.rs, conversas.rs e
ai/download.rs. O agente entregou M4 em memoria.rs e parou por estouro de limite
de crédito. O maintainer fechou em cima o resto — os dois HIGH de memoria.rs
(M1, M2) e depois M6, M7, M5 e M3 — cada um com vermelho-primeiro medido e, no
caso dos quatro últimos, o vermelho confirmado contra o código de produção antes
de verdir. O que segue é lido do git diff desta árvore.

Os `arquivo:linha` abaixo foram re-derivados na passagem de 2026-11-20, depois das
frentes M8 (download.rs), CONC2 e CONTRATO (os `lock()` viraram `trava_estado`,
`state.rs:30`), que mexeram os três arquivos. Cada âncora é a linha do símbolo no
arquivo de hoje, conferida por `grep -n`.

## Linha de base medida neste lote

- cargo test --lib em src-tauri: 499 passaram, 0 falharam, 7 ignored (gate
  central do maintainer no ponto em que M4, M1 e M2 estavam na árvore, depois da
  correção de compilação).
- npm run build: OK.
- O número final, depois de M6, M7, M5 e M3, está na Finalização embaixo.

## FEITO — M4 [MED] conversa truncada deixa de ser fingida como vista

O defeito: uma conversa cortada pelo teto LIMITE_TRECHOS no meio do laço era
salva no índice com a prova COMPLETA. conversa_vista então a marcava como lida
para sempre, e os trechos que faltaram nunca voltavam a ser indexados.

- memoria.rs:770 substituir_conversa ganhou o quarto parâmetro truncada: bool (:775); a
  FichaConversa guarda truncada (memoria.rs:411, com #[serde(default)] em :410) para o
  índice antigo no disco continuar lendo, e a escrita da ficha está em :793.
- O scan de custódia (memoria.rs:2018, com `total_lidas` em :2003) passa truncada = prontas.len()
  < total_lidas: só o que coube inteiro é marcado visto.
- O caminho de leitura agora exige !truncada junto da prova, então a conversa
  truncada volta a ser trabalho no próximo turno.

Reparações de teste: os cinco sítios de teste que ainda chamavam
substituir_conversa com três argumentos foram completados com false (são
conversas completas) para destravar a compilação do gate central.

## FEITO — M2 [HIGH] a conta de dimensão não estoura mais

O defeito: no carregar, a checagem blob.len() == trechos.len() * dimensao * 4 era
multiplicação sem checagem. Em release um dimensao danificado no cabeçalho fazia
o produto embrulhar até casar com um blob curto, o índice carregava torto, e a
fatia vetores[inicio..inicio+dimensao] (memoria.rs:717, 855) entrava em pânico
OOB mais tarde, por exemplo via revogar.

- memoria.rs:1073 a conta virou uma cadeia checked_mul; overflow ou tamanho que
  não bate devolve None — o mesmo caminho honesto de um índice que não bate
  consigo mesmo (indexar de novo).
- Vermelho-primeiro: uma_dimensao_que_estoura_a_conta_nao_carrega
  (memoria.rs:2527) — grava um índice de três peças, mente uma dimensão gigante
  (1 << 62) com blob vazio (a colisão que embrulhava para zero) e exige None.

## FEITO — M1 [HIGH] estado() não congela a UI durante a varredura

O defeito: varrer_e_embutir (memoria.rs, o guard de carregado é tomado em
memoria.rs:2191 e seguro do primeiro ao último arquivo) e estado() pediam o mesmo
lock carregado. Como a tela chama estado() no seu loop, cada leitura de estado
esperava a varredura inteira — a mesma classe da UI congelada do task #40, agora
pelo lado da memória.

- memoria.rs:1543 InfoMotor é um espelho pequeno e estável dos fatos do motor
  carregado (dispositivo, na_placa, agrupamento, dimensao, lote), guardado em
  info_motor: RwLock (memoria.rs:1569, init em 1609).
- memoria.rs:1757 o espelho é escrito dentro de carregado() — o único lugar onde
  um codificador entra no motor — sob o mesmo guard. Ordem de lock carregado antes
  de info_motor; como estado() só lê info_motor, não há ciclo.
- memoria.rs:1634 (dentro de `ajustar`, :1627) e :1699 (dentro de `soltar`, :1689) —
  os dois pontos de descarga (troca de codificador e
  soltar) limpam o espelho junto, para ele nunca mentir.
- memoria.rs:2085 estado() lê o espelho (trava_leitura em :2091) e nunca encosta em carregado.

Vermelho-primeiro medido: o_estado_nao_espera_o_motor_da_varredura
(memoria.rs:2558) segura o lock do motor como a varredura faz e exige que estado()
devolva em até 500 ms. Confirmado vermelho com o lock do motor de volta em
estado() (panico programado por recv_timeout) e verde com o espelho. O gate
central rodou 499/0 depois de retirar a injeção de prova.

## Segunda passagem do maintainer — o que o limite de crédito deixou

M5, M6, M7 e M3 sobraram para o maintainer e estão fechados abaixo, cada um com
o vermelho-primeiro rodado contra o código de produção antes de verdir.

## FEITO — M6 [MED] um índice corrompido não apaga as conversas

O defeito: `Conversas::novo` lia o sumário com `ler_json::<Indice>().unwrap_or_default()`.
Um `indice.json` truncado virava lista vazia e cada `<id>.json` — que existe
sozinho, com mensagens, título e resumo — ficava órfão: a pessoa abre o app e
some o histórico inteiro, sem erro nenhum.

- conversas.rs:215 `reconstruir_indice` percorre a pasta, lê cada conversa e
  refaz a ficha (título, datas, ordem por `atualizada`).
- conversas.rs:251 o parse do sumário virou `match`: `Ok` usa o índice (:252), `Err`
  reconstrói do disco (:253). O sumário é cache, não fonte da verdade — quem manda é o
  arquivo da conversa.

Vermelho-primeiro: `um_sumario_corrompido_nao_apaga_as_conversas`
(conversas.rs:924) grava uma conversa, pisa só no `indice.json` com `b"{"` e
exige que a reabertura liste a conversa, com o título certo, e que `abrir`
devolva o histórico do arquivo.

## FEITO — M7 [MED] o lock cobre o arquivo e o índice juntos

O defeito: `renomear` e `gravar` faziam ler→mudar→gravar do arquivo da conversa
fora do mutex; o lock protegia só o update do sumário depois. Um renomear durante
um save de turno lia o mesmo estado que o save e escrevia o arquivo inteiro por
cima — o último a escrever apagava o título escrito à mão ou as mensagens do
turno.

- conversas.rs:320 `renomear` (lock em :334) e conversas.rs:355 `gravar` (lock em :372) tomam
  `trava_estado(&self.indice)` antes de ler o arquivo e só soltam depois de
  `gravar_json` + update da ficha + `salvar_indice`. (A frente CONTRATO trocou o
  `.lock()` daquela hora por `trava_estado`, que recupera de lock envenenado.)
- Escrito no `gravar` e comprovado ao rodar: a escrita é atômica contra um
  leitor (`.tmp` + rename), não contra uma segunda escrita que começou lendo um
  estado anterior. As duas escritas ainda disputam o mesmo `<nome>.json.tmp`
  determinístico.

Vermelho-primeiro: `renomear_e_gravar_concorrentes_nao_se_atropelam`
(conversas.rs:935) — 150 rodadas, cada uma semeando a conversa e soltando um
`renomear` e um `gravar` no mesmo id por `Barrier`, exigindo os dois efeitos no
arquivo final. Verde: 0 perdidas. Vermelho medido com o lock de volta só no
sumário: as 150 rodadas perderam efeito, e o sintoma foi pior que o previsto —
`JSON inválido em <id>.json: trailing characters`, as duas escritas no mesmo
`.tmp`. O lock também protege o arquivo temporário.

## FEITO — M5 [MED] escritores do índice serializados

O defeito: `revogar` e a revalidação de conversas de um turno (`fundir_conversas`)
faziam ler-cópia → mudar → gravar-disco → publicar sem trava compartilhada. Dois
escritores concorrentes liam a mesma cópia e o último publicar ressuscitava no
disco e na memória o trecho que o primeiro acabou de apagar — inclusive o de uma
pasta revogada.

- memoria.rs:1579 `escritores: Mutex<()>` no `Custodio`.
- memoria.rs:1826 `escrever_indice` é o único caminho de escrita fora da
  varredura: trava (:1830) → lê → muda → grava → publica.
- memoria.rs:1846 `revogar` passou a usar o helper; memoria.rs:1907
  `fundir_conversas` segura a mesma trava do ler-cópia ao publicar (:1917).
- Ordem declarada: `escritores → carregado → indice`. A varredura não pega
  `escritores` — ela já é exclusiva por `ocupado` e segura `carregado` o tempo
  todo; pegá-la sob esse guard fecharia um ciclo contra quem escreve. É por isso
  que a correção certa é a trava nova, não alargar a existente.

Vermelho-primeiro: `dois_escritores_do_indice_nao_se_atropelam` (memoria.rs:2671)
semeia três arquivos, e por 150 rodadas solta dois `escrever_indice` concorrentes
apagando arquivos diferentes, exigindo que sobre só o terceiro. Verde: 0
perdidas. Vermelho medido com a trava retirada (sozinha, sem ela): 150 de 150
rodadas com o arquivo apagado de volta no índice — `left: 150, right: 0`.

## FEITO — M3 [MED] corpo silencioso e *Parar* durante o silêncio

O defeito: o cliente só tinha `connect_timeout`. Um servidor que aceita, manda os
cabeçalhos e então silencia no meio do corpo pendurava `stream.next()` para
sempre — o `.part` congelado no ar, sem falha e sem retentativa. E o cancelamento
era checado apenas quando um chunk chegava, então o botão *Parar* não fazia nada
durante a pendura.

- download.rs:72 `client()` chama `client_com_leitura(Duration::from_secs(30))`
  — a cadeia real de produção ganhou `.read_timeout` (a função em download.rs:78, a
  chamada em :88). É por leitura, não pelo corpo inteiro: um servidor lento mas vivo segue mandando
  peças; o que cai é o silêncio, e ele vira falha retentável na fila de
  tentativas. O prazo é parâmetro só para o teste encurtá-lo, sem duplicar a
  cadeia — o que se prova é a configuração da produção.
- download.rs:253 `vigiar_cancelamento` e download.rs:539: a espera por chunk
  virou `tokio::select!` contra o cancelamento, que devolve `Cancelled` no meio
  da espera por bytes (:541, com o `stream.next()` ao lado em :540).
- download.rs:945 `Pedida::Silenciar { bytes }` no servidor de teste: anuncia o
  corpo inteiro, entrega `bytes` e então silencia com a conexão aberta, sem
  fechar. É o corpo que para depois que o `connect_timeout` já passou.

Vermelho-primeiro medido (os dois verdes acima, cada um com o guard retirado):
- `parar_responde_durante_um_corpo_que_silenciou` (download.rs:1280): sem o
  `select!`, o download ficou pendurado e o veredito de 5 s do próprio teste
  disparou — "o *Parar* não respondeu durante o silêncio".
- `um_corpo_que_silencia_vira_falha_retentavel_pelo_read_timeout`
  (download.rs:1314): sem o `.read_timeout` da produção, o download ficou
  pendurado e o veredito de 10 s disparou. Com ele, cada silêncio vira uma
  pedida e a retentativa para no número de tentativas, sem publicar nada.

Os dois vermelhos foram rodados contra o código de produção (a linha do
`read_timeout` foi retirada de `client_com_leitura`, não do teste).

## Finalização desta frente

M1, M2, M4, M5, M6, M7 e M3 estão na árvore. Gate central medido após o lote:
`cargo test --lib` com 509 passaram, 0 falharam, 7 ignored; `npm run build` OK.
memoria.rs, conversas.rs e download.rs continuam sob um único dono.
