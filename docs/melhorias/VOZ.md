# VOZ — a barra de progresso diz o que a rede traz

Dono: maintainer. Duas telas da frente de voz mentiam sobre o estado real: a
barra de download dos modelos e o medidor do microfone.

## V1 — a barra começava adiantada e travava nos 100 % antes da hora

**Defeito (lido).** `voz/baixar.rs` somava no denominador o **tamanho
publicado** de cada arquivo que faltava e emitia no numerador os **bytes em
disco** que `on_progress` reportava — que incluem o prefixo do `.part` e, no
primeiro evento, o que já estava baixado de chamadas anteriores. As duas
contas não eram a mesma grandeza:

- um `.part` pela metade entrava no numerador inteiro e no denominador só pelo
  que faltava, então a barra começava adiantada;
- quando o numerador ultrapassava o denominador por essa soma torta, um
  `min(denominador)` prendia o progresso em 100 % enquanto ainda havia byte
  saindo da rede — a barra cheia e o download longe do fim;
- um arquivo já completo continuava na lista e no total, então "quanto falta"
  incluía o que não faltava.

**Correção.** A conta passa a ser uma grandeza só — **o que a rede traz nesta
chamada**:

- `Remessa` (`baixar.rs:57`) carrega `tamanho` e `prefixo`, e
  `a_receber()` / `na_rede()` (`baixar.rs:68,76`) são o denominador e o
  numerador da mesma coisa;
- `remessas()` (`baixar.rs:86`) monta a lista: arquivo no tamanho certo **sai
  da lista** (a classe de defeito deixa de ser representável), e um `.part`
  maior que o arquivo publicado entra com `prefixo = 0`, porque `download`
  deita aquele lixo fora e pede tudo de novo — descontar um prefixo que não
  será retomado mentiria de novo;
- `Iniciou { total_bytes }` (`baixar.rs:122`) e cada `Progresso` usam o mesmo
  `total`, e o `min(denominador)` foi apagado. Um recomeço do zero faz
  `em_disco` voltar a caminhar do começo, e a barra **atrasa** um pouco — nunca
  adianta, que é a direção errada de um progresso.

**Medido.** Vermelho, com o primeiro arquivo de `whisper-tiny` já em disco:
`a barra começou em 12937772 de 90672131` — 14 % antes de um único byte da rede,
e com um denominador que era a soma dos que faltavam (103.609.903 − 12.937.772),
não o que a rede traria. Verde, hoje:
`cd src-tauri && cargo test --lib voz::baixar` → **7 passaram, 0 falharam,
finished in 0.19 s**. Os números saem do catálogo real de `whisper-tiny`
(12.937.772 + 89.855.401 + 816.730 = 103.609.903):

| teste | o que confere |
|---|---|
| `um_modelo_inteiro_nao_tem_nada_a_receber` | lista vazia e `total == 0` |
| `a_barra_soma_o_que_falta_nao_o_tamanho_do_modelo` | 2 remessas, `total == 89.855.401 + 816.730` |
| `um_arquivo_que_ja_esta_em_disco_nao_engorda_a_barra` | numerador `0` |
| `um_part_parcial_nao_conta_como_rede` | numerador `0`, denominador `103.609.903 − 8.000.000` |
| `um_part_maior_que_o_arquivo_promete_o_arquivo_inteiro` | `prefixo == 0`, `a_receber() == tamanho` |
| `a_barra_promete_o_que_a_rede_trara_de_um_disco_misto` | `total == Σ a_receber()` |

## V2 — o medidor continuava "gravando" depois de o microfone fechar

**Defeito (lido).** `voz/escuta.rs` escreve o nível do último bloco em
`estado.nivel`, e a tela pinta a barra a partir dele quando `ouvindo` está
ligado. Quando a escuta terminava — pelo *Parar* ou pelo teto de 60 s — o
`nivel` ficava com o valor do último bloco lido. O resultado visível era o app
parado com a barra congelada no meio de uma sílaba, dizendo "ainda te ouvindo"
quando o `Stream` já tinha caído.

**Correção.** `drop(stream)` seguido de `estado.nivel.store(0)` no fim da
coleta (`escuta.rs:354`): fechou o microfone, o medidor volta ao zero. O que
fica no buffer continua sendo aproveitado pelo laço de coleta antes disso, então
a transcrição não perde nada.

**O que não mudou, de propósito.** O `ouvindo` não é limpo no teto de 60 s. Os
amostrados só são entregues quando `transcrever` faz `join()` na tarefa de
escuta; limpar a marca antes disso faria o segundo clique do microfone chamar
`ouvir()` de novo e esbarrar em `erros.vozEscutaEmCurso`, deixando o áudio já
falado sem dono. A barra vai a zero; a conversa continua esperando o Whisper.

**Verificação.** **Raciocinado, não medido**: nenhum teste cobre este ponto
porque a asserção exige microfone real (`cpal` abrindo o dispositivo de
entrada), e a suíte do aplicativo não captura áudio. O que foi medido é que nada
mais quebrou: `cd src-tauri && cargo test --lib` → **548 passaram, 0 falharam,
11 ignored**.

## Gates

`cargo test --lib` → 548 / 0 / 11 ignored, 4.60 s. `npm run build` → OK.
