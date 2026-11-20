# CORE — correção do núcleo de IA

Frente com dono único dos arquivos de IA: engine.rs, router.rs, alojamento.rs,
session.rs e chat.rs. O agente delegou começou engine.rs (A2 e A5) e caiu no meio
por estouro de limite de crédito; o maintainer retomou e fechou o resto — A4
(alojamento.rs), A3 (router.rs), A6 (session.rs) e A7 (chat.rs). Toda a frente
está na árvore. O que segue é lido do diff desta árvore: cada item com o
defeito, a correção com `arquivo:linha` e o vermelho-primeiro que o prova.

## Linha de base medida neste lote

- cargo test --lib em src-tauri: 478 antes do lote; o gate central do maintainer
  marcava 499 / 0 / 7. Depois de A4 (+2), A3 (+1), A6 (+1) e A7 (+1) o número é
  lido do log no fim — cada teste novo abaixo é contado, cada item foi verde.
- npm run build: OK.

## FEITO — A2 [MED] parada de ChatML lida por token especial

O defeito: a rede de parada procurava o marcador de turno no texto já decodificado
com skip_special_tokens ativado. Nos tokenizers que marcam os marcadores como
especiais (Granite e Qwen publicam o marcador de turno como token adicionado), a
busca nunca casava: um modelo preso repetindo turnos ia até max_tokens em vez de
parar.

- engine.rs:131 bruto() abre um fluxo que decodifica com os especiais à vista, só
  para a vigia de parada enxergar o marcador. O que a pessoa lê continua saindo
  de new() (engine.rs:122), que pula os especiais.
- engine.rs:608 a vigia usa bruto(); a parada é procurada no texto bruto, não no
  texto limpo.
- engine.rs:167 tokens_antes_do_corte() recua token a token até o prefixo
  decodificado terminar antes do marcador, para devolver à pessoa exatamente o
  que precede a parada sem vazar o token especial.

Vermelho-primeiro (testes em engine.rs):
- marcador_especial_e_invisivel_no_stream_mas_nao_na_vigia (engine.rs:946): o
  marcador não aparece no stream da pessoa e aparece na vigia.

## FEITO — A5 [MED] motivo da sonda de VRAM é retido, não descartado

O defeito: fichar() e capacidade() eram chamados com ok() e .flatten(), jogando
fora o motivo real da falha. Quando a sonda de VRAM falhava, o despacho caía no
default CAMADAS_NA_GPU (tudo na placa) e a tela mostrava tipo gpu com motivo nulo
— um offload total não-medido apresentado como normal, contra a regra do produto
de placa primeiro e medido.

- engine.rs:301 motivo_de_carga() junta os avisos de ficha, sonda e medição
  depois da carga num único motivo.
- engine.rs:350/371/415: as três medições devolvem par (valor, erro); engine.rs:434
  engine.rs:457 o veredito carrega o motivo retido.
- engine.rs:426 motorSemPlaca: a placa que carregou não respondeu à medição de
  depois, e isso é dito, não engolido.

Chave i18n nova registrada e adicionada pelo maintainer: erros.motorSemPlaca (já
está nos dois idiomas).

## FEITO — A4 [MED] conta de alojamento não embrulha em cabeçalho crafted

O defeito: `fichar` multiplicava os metadados u32/u64 do cabeçalho GGUF sem
checagem (`kv_por_token = 2 * camadas * cabeças_kv * dimensão * 2`; a soma dos
pesos por `sum::<u64>()`), e `alojar` multiplicava `kv_por_token * janela` e
somava `pesos + kv + FOLGA` do mesmo jeito. Num build de release o produto
embrulha em u64, o KV parece pequeno e a placa jura `Inteira` para um modelo que
nunca caberia — o OOM vem depois, na carga real. Em debug o mesmo caminho panic.

- alojamento.rs:166 `bytes_do_tensor` agora acumula a forma e o tamanho com
  `saturating_mul`/`saturating_div_ceil`: um tensor fabricado satura em vez de
  embrulhar para um número pequeno.
- alojamento.rs:179 `por_camada` soma cada tensor com `saturating_add`; a
  `pesos_total` de `fichar` (:238) é um `fold` saturante — o teto honesto para
  "estes bytes não cabem em lugar nenhum".
- alojamento.rs:254 `kv_por_token` é uma cadeia `checked_mul` que recusa o
  arquivo no estouro com `erros.alojamentoEstouro` (motivo real, não zero).
- alojamento.rs:317 `alojar` é todo saturante (`saturating_mul`/`_add`/`_sub`):
  mesmo uma `Ficha` armada à mão não arranca um `Inteira` falso.

Vermelho-primeiro (testes em alojamento.rs):
- um_kv_que_estoura_a_conta_jamais_diz_que_cabe_inteira (sintoma, linha 323):
  ficha com `kv_por_token = 1<<63` e janela 2 → veredito nunca `Inteira`. Com a
  conta original embrulhada, `cargo test` panica em `attempt to multiply with
  overflow` nessa linha (medido).
- a_ficha_recusa_geometria_que_estoura_a_conta (fronteira, linha 264): um GGUF
  mínimo com `attention.head_count_kv = u32::MAX` é recusado por estouro. Com a
  cadeia `kv_por_token` original, o mesmo panic aparece dentro de `fichar`.

Chave i18n nova registrada e adicionada pelo maintainer: erros.alojamentoEstouro
(já está nos dois idiomas).

## FEITO — A3 [MED] candidato sem ficha não jura que cabe

O defeito: `posto` devolvia `(3, true)` para um candidato sem `ficha`. O
primeiro lado (colocação 3) já era o "entra por último", mas o segundo lado é o
freio `cabem` — e `true` dizia "a sobra cabe na RAM medida" de um arquivo que
nunca foi medido, violando o invariante escrito na struct (router.rs:168-169:
`None` é "não medimos", nunca "cabe"). Um arquivo pela metade passava no filtro
`filter(|c| cabem(c))` do `choose` como se tivesse sido conferido.

- router.rs:199 o ramo sem ficha devolve `(3, false)`: último na colocação e sem
  a alegação de caber. A escolha final ainda recorre ao último candidato
  (`pool.first()`), então "ainda entra" está preservado — só não mente.

Vermelho-primeiro (teste em router.rs):
- candidato_sem_ficha_nao_afirma_que_cabe (router.rs:603): `posto` de um
  candidato sem ficha é `(3, false)`. Com o `(3, true)` original o assert do
  segundo elemento falha. O teste existente `sem_ficha_a_escolha_acontece_e_diz
  _que_nao_mediu` continua verde: a escolha acontece pela última saída, não pela
  alegação de caber.

## FEITO — A6 [MED] corte em bloco de pensamento não vira sucesso vazio

O defeito: com pensamento ligado, quando o teto de tokens cortava a resposta
dentro de um bloco de pensamento que nunca fechou, o `answer` ficava vazio. A
retomada de `MaxTokens` só olhava `chamada_pendente` (corpo de ferramenta aberto)
— um bloco de pensamento aberto não é chamada, então não continuava — e o turno
caía no `Done { text: "" }`: um sucesso silencioso e vazio, contra a regra do
produto de devolvver o motivo real em vez de fingir que respondeu.

- session.rs:499 `corte_em_pensamento_aberto(raw, ligado, stop)`: verdadeiro só
  quando pensamento ligado + `MaxTokens` + bloco aberto (tem `THINK_OPEN`, não
  tem `THINK_CLOSE`). Prosagem truncada sem bloco (pensamento desligado) não
  entra: ali a resposta parcial é legítima.
- session.rs:710 `run_turn` recusa com `erros.pensamentoTruncado` nesse caso, em
  vez de emitir `Done` vazio. A retoma do bloco foi descartada de propósito: a
  continuação via `cabecilha` corre em `em_chamada`, que suprime a saída — o
  texto final nunca voltaria à tela. Melhor o motivo honesto que um mecanismo
  que esconde a resposta.

Vermelho-primeiro (teste em session.rs):
- corte_em_pensamento_aberto_e_falha_e_nao_sucesso_vazio (session.rs:850): a
  matriz das quatro combinações — bloco aberto com MaxTokens é o único `true`;
  bloco fechado, término por EOS e pensamento desligado são `false`.

Chave i18n nova registrada e adicionada pelo maintainer: erros.pensamentoTruncado
(já está nos dois idiomas).

## FEITO — A7 [LOW] JSON embutido na chamada XML não é segunda chamada

O defeito: `extrair` varre as chamadas em XML e depois, numa segunda passada,
todos os blocos JSON do texto (`blocos_json`) — sem conferir se o bloco já
estava dentro de uma chamada XML lida. Um valor de parâmetro escrito como JSON
que citasse outra ferramenta conectada (`{"name": "srv__n", ...}`) virava uma
chamada EXTRA: a mesma rodada executando duas ferramentas, uma que o modelo não
pediu.

- chat.rs:330 a varredura de JSON guarda os intervalos já tomados pela leitura
  XML (`tomadas`) e pula todo bloco contido num deles. JSON de nível alto, fora
  de qualquer XML, continua lido (o caso legítimo do `mixed_xml_and_json`).

Vermelho-primeiro (teste em chat.rs):
- json_dentro_da_chamada_xml_nao_e_segunda_chamada (chat.rs:842): um `tool_call`
  XML de `everything__echo` cujo parâmetro carrega `{"name": "srv__n"}` devolve
  UMA chamada, não duas. Sem o guarda, `chamadas_de_json` produzia a segunda. Os
  testes `mixed_xml_and_json_calls_are_both_read` e `array_of_json_calls_runs
  _them_all` continuam verdes: JSON fora do XML segue contando.

## Finalização desta frente

A2, A5, A4, A3, A6 e A7 estão na árvore, verdes no gate central, cada um com o
vermelho-primeiro acima. A frente CORE não tem item pendente. Três chaves i18n
foram adicionadas pelo maintainer nos dois idiomas: erros.alojamentoEstouro,
erros.pensamentoTruncado (e erros.motorSemPlaca, de A5).
