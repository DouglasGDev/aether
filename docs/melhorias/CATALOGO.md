# CATALOGO — mais modelos (#27), com cada número lido do arquivo que roda

Frente única, um escritor: `src-tauri/src/ai/catalog.rs`. Nenhuma DLL foi
carregada, nenhum peso foi baixado, nenhum modelo foi executado — o que este doc
registra é o que se pode medir sem placa: o cabeçalho GGUF servido pelo
HuggingFace e a tabela de arquiteturas da `llama.dll` vendorizada.

## Linha de base medida antes da frente

```
cd src-tauri && cargo test --lib        → 567 passaram, 0 falharam, 11 ignored
npm run build                           → ✓ built in 250 ms
```

## O que entrou

Nove variantes novas em três séries, todas de 2026, todas cabendo na placa de 16
GB da máquina (as maiores em Q4_K_M; as Q6_K ficam apertadas e o roteador não as
escolhe sozinho):

| série | quant | bytes lidos do `tree/main` | janela do cabeçalho |
| --- | --- | --- | --- |
| Qwen3.5 4B | Q4_K_M | 2.740.937.888 | 262.144 |
| Qwen3.5 4B | Q5_K_M | 3.143.656.608 | 262.144 |
| Qwen3.5 4B | Q6_K | 3.525.956.768 | 262.144 |
| Qwen3.5 4B | Q8_0 | 4.482.403.488 | 262.144 |
| Qwen3.5 9B | Q4_K_M | 5.680.522.464 | 262.144 |
| Qwen3.5 9B | Q5_K_M | 6.577.841.376 | 262.144 |
| Qwen3.5 9B | Q6_K | 7.458.301.152 | 262.144 |
| Gemma 4 12B It | Q4_K_M | 7.121.861.440 | 262.144 |
| Gemma 4 12B It | Q6_K | 9.786.022.720 | 262.144 |

`CATALOG` passou de 35 para **44** entradas; `SHA256_PESOS` de 4 para **7**
repositórios; `SHA256_APOIO` de 2 para **5**. As somas são o `lfs.oid` do
arquivo, lido um por um — sem elas o M8 voltaria a conferir download por
tamanho.

## Como cada fato foi lido

Dois endpoints, e nada de cartão de repositório:

- `https://huggingface.co/api/models/<repo>/tree/main?recursive=true` → `size` e
  `lfs.oid` de cada `.gguf`. Os 9 pesos acima saíram daqui.
- `https://huggingface.co/<repo>/resolve/main/<arquivo>` com
  `Range: bytes=0-16777215` → os primeiros 16 MB do arquivo, que é onde mora o
  cabeçalho GGUF. Destas bytes saíram `general.architecture`,
  `general.size_label`, `general.license`, `general.name`,
  `<arquitetura>.context_length`, o `eos_token_id` e o `tokenizer.chat_template`
  — o que o modelo declara sobre si, não o que alguém escreveu sobre ele.

Medidas pelo segundo caminho, na frente desta sessão:

```
Qwen3.5-4B-Q4_K_M.gguf   arquitetura qwen35   licença apache-2.0  janela 262144  eos 248046  template 7816 chars
Qwen3.5-9B-Q4_K_M.gguf   arquitetura qwen35   licença apache-2.0  janela 262144  eos 248046  template 7816 chars
gemma-4-12b-it-Q4_K_M.gguf  arquitetura gemma4  licença apache-2.0  janela 262144  eos 106  template 18922 chars
```

### A leitura que quase passou errada

O primeiro parser devolveu um `general.architecture` de 100.663.296 bytes. O
motivo é a suposição, não o arquivo: **no GGUF v3 o tipo de um par chave → valor
é escrito como `uint32`**, não como um byte, e o mesmo vale para o tipo dos
elementos de um array. Ler um byte e seguir desalinha três bytes para trás e
todo o resto do cabeçalho vira lixo — lixo convincente, porque as primeiras
chaves ainda saem certas. O `Content-Range` da resposta (`bytes 0-63/2740937888`)
e o `lfs.oid` bateram com o arquivo inteiro, então o corpo estava certo: quem
estava errado era o parser.

## Por que estas duas famílias, e não as outras que apareceram

O portão de entrada é o que o motor executa hoje, medido no binário vendorizado
de 3.877.376 bytes (não é ponteiro LFS): `qwen35` e `gemma4` estão na tabela de
arquiteturas, e é o próprio teste do catálogo que confere —
`every_entry_is_an_architecture_the_engine_runs` procura `"<família>\0"` na
`llama.dll` e hoje passa com as duas novas.

- **Qwen3.5** é o parente direto do Ornith 1.5 que já estava dentro: mesma
  arquitetura, mesma janela, e o mesmo tokenizador — os 12.807.982 bytes de
  `tokenizer.json` de `Qwen/Qwen3.5-4B` têm o `lfs.oid`
  `5f9e4d49…` idêntico aos 12.807.982 bytes de `ornith-ai/Ornith-1.5-9B`, e as
  duas somas foram lidas cada uma do próprio repositório. É a melhor evidência
  disponível de que a família é a mesma. Os 7.816 caracteres do template dele
  carregam o bloco de ferramentas em XML — mesmas palavras que a gramática do
  laço, lidas como nomes de tag, sem byte comum entre os dois textos.
- **Gemma 4 12B** entra pela arquitetura registrada e pelo `general.license`
  lido do arquivo (`apache-2.0`), mas **não** entra como modelo de ferramentas
  verificado: os 18.922 caracteres do template declaram a chamada de ferramenta
  com os marcadores próprios do modelo, e o laço do Aether escreve e lê o XML da
  família do Qwen. O que ele faz numa rodada de ferramentas não foi medido —
  vê abaixo.
- **GLM-5.3-Flash ficou de fora por um motivo concreto**: os quants do
  repositório vêm em partes (`GLM-5.3-Flash-UD-Q4_K_XL-00001-of-00006.gguf`), e
  o downloader do Aether baixa um arquivo por entrada. Um modelo dividido em
  seis partes entraria no catálogo como um download que nunca fica completo.
- **Os repositórios oficiais dos pesos grandes também ficaram de fora por
  acesso**: `Qwen/Qwen3.5-4B-GGUF` e `zai-org/GLM-5.3-Flash-GGUF` respondem
  `401` (gated) à API anônima, e o Aether não pede token do usuário. Os
  repositórios que entraram como `weights_repo` respondem `200` com
  `gated: false`.

## FEITO na passagem seguinte — o Gemma 4 voltou a ter número de placa (#106)

O catálogo de 2026 entrou com famílias que o `fichar` não sabia ler, e o problema
não era um número ruim: era a ausência dele. `gemma4.attention.head_count_kv` é
um array de 48 inteiros, e `numero()` tratava array como metadado ausente — a
ficha falhava, `engine.rs:350` degradava para "não medido", e a tela mostrava o
Gemma 4 sem veredito. Mesmo lido o array, a conta antiga multiplicava um único
par `(cabeças_kv, dimensão)` por todas as camadas, e no Gemma 4 as camadas não
são iguais.

A conta agora é a do motor, camada a camada, e cada regra foi lida no fonte do
b12387 vendorizado antes de virar código:

| o que a conta faz | onde o motor faz o mesmo |
| --- | --- |
| número vale para todas as camadas; array precisa de uma entrada por camada, senão o arquivo é recusado | `llama-model-loader.cpp:463-501` (`get_key_or_arr`) |
| `head_count_kv` ausente = `head_count` | `llama-model.cpp:1573-1576` |
| dimensão de cabeça ausente = `embedding_length / head_count[0]` | `llama-model.cpp:1621-1625` |
| camada de janela deslizante paga `key_length_swa`/`value_length_swa`; ausentes, paga a cheia | `llama-model.cpp:1645-1648`, `llama-hparams.cpp:140-154` |
| `is_swa` vem do array de bools; sem array, do período `il % n < n - 1` | `gemma4.cpp:6`, `llama-hparams.cpp:8-18` |
| as últimas `shared_kv_layers` camadas não abrem cache próprio | `gemma4.cpp:11-18`, `llama-hparams.cpp:321-323` (`has_kv`) |
| a janela é arredondada para cima em múltiplos de 256 antes de dimensionar o cache | `llama-context.cpp:409-415` |

E o fato que derrubou a premissa desta frente: **a janela deslizante não
barateia nada aqui.** `llama_context_default_params` (`llama-context.cpp:4415`)
escreve `swa_full = true`, e com ela `size_swa = size_base`
(`llama-kv-cache-iswa.cpp:73-80`) — a camada deslizante reserva a janela inteira.
O `params_contexto` do Aether (`ggml/ffi.rs:454-459`) só toca em `n_ctx` e
`flash_attn_type`, então o padrão da biblioteca governa. O follow-up pedia
"alojamento com janela deslizante" esperando um número menor; o que faltava era a
geometria por camada, e o que ela muda é a dimensão de cabeça e quantas cabeças —
não o comprimento do cache. Baixar `swa_full` corta KV e muda o comportamento
fora da janela: é decisão de produto, não de conta, e não foi tomada aqui.

Medido nos cabeçalhos reais, lidos por `Range` do `resolve/main` nesta passagem:

| arquivo | camadas | cabeças de KV | dimensão K/V | `kv_por_token` |
| --- | --- | --- | --- | --- |
| `gemma-4-12b-it-Q4_K_M` | 48 (40 deslizantes, 8 cheias) | array: 8 naquelas, 1 nestas | 256/256 e 512/512 | 344.064 |
| `granite-4.2-3b-Q4_K_M` | 40 | 8 (escalar) | 64, pelo padrão 2560/40 | 81.920 |
| `Qwen3.5-4B-Q4_K_M` | 32 (24 recorrentes, 8 de atenção) | 4 (escalar) | 256 | 32.768, lido depois da correção #107 |

O `gemma4.attention.shared_kv_layers` do arquivo é `0` — as camadas que leem o
cache de outra ainda não aparecem nos pesos que entraram, e estão pinadas em
teste para o dia em que aparecerem.

> **O que esta linha escreveu e #107 desfez.** O registro original fechava o
> `Qwen3.5-4B` em 131.072 "como teto", porque as 24 camadas recorrentes pagavam
> cache como se fossem de atenção. Elas não pagam: `llama-memory-hybrid.cpp`
> entrega ao `llama_kv_cache` o `filter_attn = !is_recr(il)`, e o que aquelas
> camadas abrem é um estado recorrente fixo. O número real é 32.768 por token
> mais 52.690.944 bytes de estado — a passagem #107, abaixo, é quem mede isso.

## FEITO — #107: a camada de atenção linear não paga cache; paga estado

A correção nasceu de uma frente que ia ser outra. O item que estava à frente
desta página — hoje o follow-up 4 — pedia um downloader de pesos em partes para
baixar um MoE de 2026 que coubesse na placa; medido o repositório, as partes do `unsloth/GLM-5.3-Flash-GGUF` somam
≈200 GB e a `llama.dll` b12387 já abre split sozinha (`llama-model-loader.cpp:84`
`llama_get_list_splits`, `:604-635`, com as strings `of-%05d` e `invalid split
count` presentes nos 3.877.376 bytes do binário). Sem valor presente, a frente
virou o que estava no caminho: o `unsloth/Qwen3.5-35B-A3B-GGUF`, que é de arquivo
único, tem as quants cabendo em disco — e é híbrido.

Medido no cabeçalho dele (`Range` do `resolve/main`, 52 pares, 733 tensores):

```
general.architecture = qwen35moe        qwen35moe.block_count = 40
qwen35moe.attention.head_count = 16     qwen35moe.attention.head_count_kv = 2
qwen35moe.attention.key_length = 256    qwen35moe.attention.value_length = 256
qwen35moe.full_attention_interval = 4   qwen35moe.embedding_length = 2048
qwen35moe.ssm.conv_kernel = 4           qwen35moe.ssm.state_size = 128
qwen35moe.ssm.group_count = 16          qwen35moe.ssm.time_step_rank = 32
qwen35moe.ssm.inner_size = 4096         qwen35moe.context_length = 262144
```

e no censo de tensores do mesmo arquivo: `attn_k|q|v(.norm)` em 10 camadas,
`ssm_conv1d|ssm_a|ssm_beta…` em 30. As 40 nunca tiveram K e V; 30 delas não têm
nem camada de atenção.

A regra, lida do b12387 antes de virar conta:

| fato | onde |
| --- | --- |
| recorrente vem de `attention.recurrent_layers`; sem a chave, `(il + 1) % intervalo != 0` com intervalo 4 por padrão | `qwen35.cpp:17-22`, `qwen35moe.cpp:19-25` |
| o gated delta net é obrigatório nas cinco chaves `ssm.*` — sem elas a arquitetura não é híbrida | `qwen35.cpp:9-13`, `qwen35moe.cpp:11-15` |
| o cache de KV recebe `filter_attn = il < n_layer && !is_recr(il)` | `llama-model.cpp:3332-3338`, `llama-memory-hybrid.cpp` |
| o estado por camada recorrente é `(conv-1) × (inner + 2 × grupo × estado) + estado × inner`, em f32, uma linha por sequência | `llama-hparams.cpp:229` e `:257`, tipos em `llama-model.cpp:3368-3370`, `n_rs_seq = 0` em `llama-context.cpp:4421` |

O que mudou em código:

- `CamadaKv` ganhou `recorrente` (`alojamento.rs:323`) e `taxa_kv` (`:479`) soma
  só quem é `propria && !recorrente` — as 30 lineares saem do número por token.
- `memoria_recorrente` (`:327`) lê as chaves `ssm.*` e o padrão das camadas, com
  cada produto `checked`; `estado_recorrente` (`:372`) multiplica pelo número de
  camadas recorrentes. Um cabeçalho sem as quatro chaves do gated delta net não
  é híbrido e continua cobrando cache de tudo — a regra vem do arquivo, não de
  uma lista de arquiteturas.
- `Ficha.estado_recorrente` (`:95`) é preenchido por `fichar` e entra no veredito:
  `Alojamento` leva `estado` nos três casos (`:569-592`), `alojar` (`:597`)
  desconta o estado do orçamento da placa antes de empilhar camadas, e o roteador
  soma o estado na conta de RAM do caso sem placa (`router.rs:209`).
- O fio ganhou `estado` nas três formas, pinado byte a byte em
  `as_tres_formas_do_veredito_sao_camelcase_no_wire`, e o `types.ts` espelha.
- Quatro chaves i18n novas, nos dois idiomas, só para o caso em que o estado
  existe: `modelos.placaInteiraEstado`, `modelos.placaSistemaEstado`,
  `roteador.naPlacaEstado`, `roteador.noSistemaEstado`. Um arquivo denso segue
  lendo a frase de sempre — número zero não é medida, é ruído. Na frase nova do
  caso sem placa o cache também entrou: o `Alojamento::SoSistema` já devolviam
  `kv` e a tela de modelos nunca o mostrou, nem no híbrido nem no denso. A densa
  era a omissão anterior a esta passagem, e foi fechada junto: `modelos.placaSistema`
  agora diz os dois números, a mesma conta que `roteador.noSistema` sempre escreveu.
- `engine_probe alojar:` passou a imprimir os dois juntos
  (`bin/engine_probe.rs:268-272`) e a contar o estado no esperado do delta de
  VRAM (`:309-314`), que é a metade que só a placa fecha.

Os números que a conta agora devolve, cada um conferido em teste:

| arquivo | cache por token | estado por sequência |
| --- | --- | --- |
| `Qwen3.5-35B-A3B` (40 camadas, 10 de atenção) | 20.480 — eram 81.920, **4×** | 65.863.680 bytes = 62,83 MiB (30 × 2.195.456) |
| `Qwen3.5-4B` (32 camadas, 8 de atenção) | 32.768 — eram 131.072, **4×** | 52.690.944 bytes = 50,25 MiB (24 × 2.195.456) |
| `granite-4.2-3b` (denso) | 81.920, inalterado | 0 |
| `gemma-4-12b-it` (deslizante, não híbrido) | 344.064, inalterado | 0 |

Que o denso não se moveu é metade do teste: os 320,00 MiB do Granite na janela de
4096 continuam saindo iguais aos que a biblioteca escreveu na carga.

Vermelho medido por mutação, duas mutações, cada uma revertida:

- tirando `&& !c.recorrente` de `taxa_kv` — ou seja, voltando ao comportamento de
  antes — caem `as_camadas_lineares_nao_pagam_cache_e_o_estado_ocupa_o_lugar_delas`,
  `a_um_hibrido_de_32_camadas_o_estado_sobe_com_elas`,
  `a_chave_de_camadas_recorrentes_manda_no_periodo` e
  `a_ficha_hibrida_sai_do_cabecalho_escrito_e_entra_na_conta` (4 falhas, 30
  verdes: nenhum teste de arquivo denso ou deslizante se moveu).
- tirando `.saturating_add(estado)` do predicado de `Inteira` em `alojar` cai
  `o_estado_recorrente_nao_cresce_com_a_janela_e_muda_o_veredito` (1 falha, 33
  verdes) — é o teste que distingue o número que entra na decisão do número que
  só enfeita a tela.

Gates desta passagem: `cd src-tauri && cargo test --lib` → **582 passaram, 0
falharam, 11 ignored, finished in 4,91 s** (eram 575; 7 testes novos em
`alojamento.rs`, que passou a 34 — `cargo test --lib alojamento` → **34 passaram,
0 falharam**); `cargo check --all-targets` → **Finished em 3,57 s, zero avisos**;
`npm run build` → **OK, `✓ built in 173 ms`**, maior bloco `react` a 210,53 kB.


Os dois números que fecham a prova:

- **Gemma 4, janela de 4096**: 344.064 × 4096 = **1.344 MiB**. A conta antiga,
  que tomava o primeiro número do array (8 cabeças) e a dimensão cheia (512)
  para as 48 camadas, daria 786.432 × 4096 = 3.072 MiB — **2,28×** sobre o real,
  e o veredito mandaria para a RAM camadas que a placa segura.
- **Granite, janela de 4096**: 81.920 × 4096 = 335.544.320 bytes = **320,00
  MiB**, igual ao `llama_kv_cache: size = 320.00 MiB (4096 cells, 40 layers)` que
  a biblioteca escreveu na carga. A soma por camada reproduz o número do modelo
  uniforme sem alterá-lo — é assim que se sabe que a conta nova não inventou.

Vermelho medido por mutação, porque o peso não está em disco: relendo o array
como metadado ausente — o comportamento que o catálogo de 2026 encontrou — caem
`o_gemma4_paga_cada_camada_pelo_que_a_camada_aloja`,
`a_ficha_le_os_array_do_gemma4_sem_recusar_o_arquivo` e
`quem_le_o_cache_de_outra_camada_nao_paga_cache`; retirando a guarda de
comprimento, o array de 47 entradas para 48 camadas estoura o índice dentro de
`camadas_de_cache` (`um_array_que_nao_bate_com_as_camadas_e_recusado_pelo_nome`).
As duas mutações foram revertidas.

Gates desta passagem: `cd src-tauri && cargo test --lib` → **575 passaram, 0
falharam, 11 ignored, finished in 4,92 s** (eram 567; 8 testes novos, todos em
`alojamento.rs`); `cargo test --lib alojamento` → **27 passaram, 0 falharam**;
`cargo check --all-targets` → **Finished, zero avisos**; `npm run build` → **OK,
`✓ built in 227 ms`**, maior bloco `react` a 210,53 kB.

Chave i18n nova, registrada nos dois idiomas: `erros.alojamentoArray` — array de
geometria com comprimento que não bate com `block_count`, citando a chave, o
esperado e o recebido.

## FEITO — #109: a etiqueta não é um tipo, e o catálogo passou a dizer o censo

O item 2 da lista desta página, que esta passagem fechou, segurava a entrada do
`Qwen3.5-35B-A3B` por um motivo honesto: `UD-*` e `IQ*` são nomes de editor, não
tipos ggml, e o portão
`quants_are_all_implemented_by_the_engine` comparava a etiqueta do arquivo com
uma lista fechada de 16 nomes. Entrar exigia medir o conteúdo. Medido, e a
entrada aconteceu com a lista fechada substituída pelo censo.

### Como foi lido

Um arquivo por vez, `Range: bytes=0-16777215` no `resolve/main`, e o cabeçalho
GGUF v3 percorrido na ordem em que ele é escrito: magic e versão, `n_tensors` e
`n_kv` como `uint64`, os `n_kv` pares chave → valor (o tipo do valor é `uint32`,
não byte — foi a lição de #106), e depois as `n_tensors` entradas de tensor:
nome, `n_dims` (`uint32`), as dimensões (`uint64` cada), **o tipo ggml
(`uint32`)** e o `offset` (`uint64`). Dali saem duas coisas:

1. **o censo** — o conjunto de tipos que aquele arquivo usa, com quantos tensores
   de cada;
2. **a prova do número** — dois caminhos independentes, e onde os dois estavam
   disponíveis bateram:
   - o bits/elemento de um tensor grande, `(8 × (offset_do_próximo −
     offset_dele)) / elementos`, que é o espaçamento que o arquivo mesmo deixou;
   - a soma de bytes do arquivo inteiro: com a tabela de blocos do ggml
     (`elementos_por_bloco`, `bytes_por_bloco`), `div_ceil(elementos, bloco) ×
     bytes` sobre todos os tensores, mais os pesos fora de camada e o
     alinhamento (`general.alignment`, 32 por padrão), tem de devolver
     exatamente o `size` que o HuggingFace publica no `tree/main`.

Os dois rodaram sobre os 46 arquivos que o catálogo oferecia (2026-11-20) e os
23 arquivos de pesos do repositório do A3B (2026-11-24). A conferência de bytes
cobriu os 49 arquivos que o catálogo oferece hoje, os 3 do A3B que ficam fora e,
na segunda rodada, os 69 inteiros: **69 conformes, 0 divergentes**, com 34.829
vizinhanças offset-a-offset conferidas e 0 violações. É ela que fixa a numeração
dos tipos — a conta saiu do arquivo, não da minha memória nem de uma tabela
copiada:

| número | nome | blocos × bytes | | número | nome | blocos × bytes |
| --- | --- | --- | --- | --- | --- | --- |
| 0 | `f32` | 1 × 4 | | 14 | `q6_k` | 256 × **210** |
| 1 | `f16` | 1 × 2 | | 16 | `iq2_xxs` | 256 × 66 |
| 2 | `q4_0` | 32 × 18 | | 17 | `iq2_xs` | 256 × 74 |
| 3 | `q4_1` | 32 × 20 | | 18 | `iq3_xxs` | 256 × 98 |
| 6 | `q5_0` | 32 × 22 | | 20 | `iq4_nl` | 32 × 18 |
| 7 | `q5_1` | 32 × 24 | | 21 | `iq3_s` | 256 × 110 |
| 8 | `q8_0` | 32 × 34 | | 22 | `iq2_s` | 256 × 82 |
| 9 | `q8_1` | sem arquivo | | 23 | `iq4_xs` | 256 × 136 |
| 10 | `q2_k` | 256 × 84 | | 29 | `iq1_m` | 256 × 56 |
| 11 | `q3_k` | 256 × 110 | | 30 | `bf16` | 1 × 2 |
| 12 | `q4_k` | 256 × 144 | | 39 | `mxfp4` | 32 × 17 |
| 13 | `q5_k` | 256 × 176 | | 19 | `iq1_s` | 256 × **50** |

A linha do `q6_k` tem negrito porque foi onde o instrumento me pegou da primeira
vez: com 208 bytes por bloco — o número que eu havia escrito de memória — o
`granite-4.2-3b-Q6_K.gguf` saía **163.840 bytes curto** e o
`granite-4.2-8b-Q6_K.gguf` **409.600**, contra os tamanhos publicados; com 210 os
dois batem. Da segunda vez o instrumento me pegou em duas linhas que eu havia
copiado da minha própria tabela de instrumento: `iq1_s` **não é 256 × 52, é 256 ×
50**, e `mxfp4` **é 4,25 bits por elemento, não 4,5** (32 × 17 — o mesmo
bytes/elemento do `iq4_xs`, por coincidência). O curioso do `iq1_s` é que a soma
de bytes **não vê o erro**: em `Qwen3-30B-A3B-UD-IQ1_S.gguf` nenhum tensor `iq1_s`
é o último em offset, então as duas linhas devolvem o mesmo tamanho publicado, e a
linha errada teria passado para sempre se a conferência fosse só de soma. O que
reprovou foram 58 janelas offset-a-offset — em `blk.0.ffn_gate_exps.weight`, 201.326.592
elementos ocupam 39.321.600 bytes, que é exatamente 786.432 blocos de 50; a linha
52 pediria 40.894.464 e o arquivo não tem essa janela. O bits/elemento que eu já
havia medido para o tipo (1,5625) também pedia 50: a conta estava certa no censo
e errada na tabela de blocos, e ninguém tinha cruzado as duas.

Um limite honesto, e é o mais fino deste doc: a janela offset-a-offset fixa
**bytes por elemento exatamente**; a granularidade do bloco não é observável por
ela. Para o `mxfp4`, `32 × 17`, `64 × 34`, `256 × 136` e `2048 × 1088` fecham o
mesmo tamanho publicado, porque os comprimentos de tensor são todos múltiplos
compatíveis — no caso dos IQ, por potências de dois. O que está medido é o
bits/elemento; a escrita em `elementos × bytes` segue a convenção do ggml e é
assim que ela deve ser lida.

Das 24 linhas da tabela, 21 têm o tamanho conferido contra um dos 69
arquivos. Duas precisaram de prova fora da conta, e é justo nomeá-las:
`iq1_s` (19) e `iq1_m` (29) não aparecem em nenhum dos 69 arquivos lidos, então
foi baixado um sétimo arquivo como sonda — `Qwen3-30B-A3B-UD-IQ1_S.gguf`,
9.043.300.928 bytes publicados, censo com dez tipos — e os dois fecharam lá, com
os mesmos 1,5625 e 1,75 bits por elemento do espaçamento. O `q8_1` (9) continua
**sem arquivo**, e o `q8_k` (15) nem entrou na tabela: nenhum dos 74 cabeçalhos
lidos usa esses dois números. E o
`mxfp4` (39) fecha o `MXFP4_MOE` sozinho, o que também responde uma dúvida que
estava aberta: a numeração não para no 30, mas acima do 39 nada apareceu nestes
74 cabeçalhos — os tipos 133, 139 e 140 medidos de passagem pertencem a um fork
(`ik_llama.cpp`) que não é o motor que o Aether carrega, e ficam registrados como
fronteira do que foi lido, não como catálogo.

### O que a etiqueta esconde

| arquivo | etiqueta diz | `general.file_type` | o que os tensores são |
| --- | --- | --- | --- |
| `granite-4.2-3b-Q3_K_S.gguf` | Q3_K_S | 11 | `f32×81`, `q3_k×281`, `q6_k×1` |
| `Qwen3.5-35B-A3B-Q3_K_S.gguf` | Q3_K_S | 11 | `f32×301`, `q8_0×310`, `iq3_xxs×80`, `iq3_s×40`, `q6_k×2` |
| `Qwen3.5-35B-A3B-Q4_K_M.gguf` | Q4_K_M | 15 | `f32×301`, `q8_0×311`, `q4_k×80`, `q5_k×40`, `q6_k×1` |
| `Qwen3.5-35B-A3B-UD-Q2_K_XL.gguf` | UD-Q2_K_XL | 10 | `f32×301`, `q8_0×61`, `q4_k×1`, `q5_k×177`, `q6_k×73`, `iq2_xs×78`, `iq3_xxs×41`, `iq4_xs×1` |
| `Qwen3.5-35B-A3B-MXFP4_MOE.gguf` | MXFP4_MOE | **15** | `f32×301`, `q8_0×312`, `q5_k×41`, `q6_k×1`, `mxfp4×78` |

Os dois primeiros têm a mesma etiqueta e o mesmo número declarado, e um deles não
tem **um tensor `q3_k` nenhum**. O último declara 15 — o número do `Q4_K_M` da
própria série — e é um arquivo MXFP4. Uma lista de etiquetas não distingue
nenhum desses casos; o censo distingue.

### O portão, agora lido do conteúdo

> **Lido em 2026-11-24 e já trocado uma passagem depois.** Esta seção é a da
> #109, e o portão que ela escreveu mudou de natureza na #111: a lista de 15
> tipos copiada do candle saiu, o leitor passou a ser do aplicativo, e os onze
> arquivos que ela deixava de fora entraram. Veja
> `## FEITO — #110 e #111`. As linhas abaixo ficaram como foram medidos, com os
> apontadores re-derivados para a árvore de hoje.

O leitor de cabeçalho do aplicativo era o do candle, e ele aceitava exatamente
`f32, f16, bf16, q4_0, q4_1, q5_0, q5_1, q8_0, q8_1, q2_k, q3_k, q4_k, q5_k,
q6_k, q8_k` (`GgmlDType::from_u32`, candle-core 0.11.1): os IQ e o MXFP4 caem no
ramo `_ => bail!("unknown dtype for tensor {u}")`, `Content::read` devolve erro,
`fichar` devolve erro e **não há veredito de alojamento**. Como o Aether não roda
o que não mediu antes (#36), a regra passou a ser o portão:

- `CENSO` (`catalog.rs:434`) — 49 linhas então, **60 hoje**, uma por arquivo
  oferecido, com os tipos lidos do cabeçalho dele;
- `censo_do_arquivo` (`catalog.rs:499`) — a leitura, `None` para o arquivo que
  ninguém abriu;
- `censo_medido_de_todo_arquivo_oferecido_e_aberto_pelo_leitor`
  (`catalog.rs:814`) — cada arquivo do catálogo e dos codificadores tem de ter
  linha medida, nenhum nome pode se repetir (é a chave), e cada tipo da linha tem
  de ter conta de bloco no leitor. Este teste substituiu o de etiquetas;
- `granites_sao_executaveis_pelo_caminho_de_cpu` (`catalog.rs:849`) — a parte que
  é sobre o degrau mais estreito do caminho de CPU: sem placa a llama.cpp conta
  no processador e abre qualquer arquitetura que ela abra, e o candle só é
  alcançado quando essa carga também falha. O degrau de baixo existe só para o
  granite e escolhe pelo nome da quant, preservada (redescrita na frente #116,
  que acrescentou o degrau do meio);
- `tipo_sem_bloco_medido_nao_vira_ficha_inventada` (`alojamento.rs:1155`) — o
  GGUF sintético de um tensor só, escrito na #109 como `tipo_fora_da_tabela_do_leitor_nao_vira_ficha_inventada`
  e reescrito na #111: com o tipo 8 a ficha sai e conta os 68 bytes reais
  (`q8_0`, 64 elementos = 2 blocos de 34), com o tipo 16 conta 66 (`iq2_xxs` —
  antes recusava o arquivo inteiro com `unknown dtype for tensor 16`), e com o
  tipo 9 recusa nomeando o tensor e o número lido. O preço de entrada continua
  sendo a recusa com motivo, só que agora ele pega um tipo que nenhum arquivo
  dos 74 lidos carrega, e não um que dezesseis deles carregam.

Uma frase sobre o `general.file_type`, porque é a primeira coisa que alguém
olha: ele é um número que o quantizador escreve, **não** a conta de conteúdo, e
a medição mostra os dois casos. Os arquivos lidos declaram —
`granite-4.2-3b-Q3_K_S` 11, `Qwen3.5-35B-A3B-Q3_K_S` 11, `UD-Q2_K_XL` 10,
`Q4_K_M` 15, `Q5_K_M` 17, `Q6_K` 18 e `MXFP4_MOE` 15. Dois arquivos com o mesmo
11 têm censos diferentes, e um MXFP4 declara o mesmo 15 do `Q4_K_M` da série. O
que cada número quer dizer é matéria do fonte da llama.cpp, e isto não foi lido
dele: o que está provado aqui é que o número não serve de prova de conteúdo, e
é por isso que o portão lê o censo.

### O que entrou: `Qwen3.5 35B A3B`, três quants, arquivo único

`CATALOG` passou de 44 para **47** entradas; `SHA256_PESOS` de 7 para **8**
repositórios; `SHA256_APOIO` de 5 para **6**. A tabela de censos nasceu com as
**49** linhas dos arquivos oferecidos (47 de conversa + 2 de embeddings).

Lido do cabeçalho em 2026-11-24 (`Qwen3.5-35B-A3B-Q4_K_M.gguf`, 733 tensores, 52
pares): arquitetura `qwen35moe`, `general.name` e `general.size_label` =
`35B-A3B`, licença `apache-2.0`, janela `262144`, 40 camadas, `embedding_length`
2048, 16 cabeças com 2 de KV, `key_length` e `value_length` 256, 256 peritos com
8 usados, `full_attention_interval` 4 (o híbrido que #107 já cobra: 20.480 por
token e 62,83 MiB de estado), e `tokenizer.chat_template` dentro do arquivo.

| id | bytes publicados | `lfs.oid` (sha256) | censo |
| --- | --- | --- | --- |
| `qwen3.5-35b-a3b-Q4_K_M` | 22.016.023.168 | `3b46d106…de494375` | `f32, q4_k, q5_k, q6_k, q8_0` |
| `qwen3.5-35b-a3b-Q5_K_M` | 26.249.607.808 | `93198787…fa315d89` | `f32, q5_k, q6_k, q8_0` |
| `qwen3.5-35b-a3b-Q6_K` | 28.852.861.568 | `56ddab06…80e8280a` | `f32, q6_k, q8_0` |

As outras 20 quants do repositório ficaram fora, por dois motivos distintos e
medidos: **11 têm tipo que o portão recusa** (os IQ acima, mais o MXFP4) e 9 são
lidas mas não escolhíveis — `Q4_K_S` 20.673.845.888, `Q5_K_S` 24.823.544.448,
`Q8_0` 36.903.139.968, `UD-Q4_K_L` 20.205.632.160, `UD-Q4_K_XL` 22.241.950.336,
`UD-Q5_K_XL` 26.385.922.688, `UD-Q6_K_S` 28.515.105.440, `UD-Q6_K_XL`
32.071.842.432, `UD-Q8_K_XL` 48.688.560.768. Os onze do primeiro grupo entraram
na passagem #111, com o portão trocado por uma conta medida arquivo por arquivo;
o `## FEITO — #110 e #111` abaixo é quem vale para eles hoje. Dois dos nove do
segundo grupo merecem nota: o
`UD-Q4_K_XL` tem **o mesmo censo** do `Q4_K_M` e é maior (22.241.950.336 contra
22.016.023.168) — é o caso em que a etiqueta `XL` não compra tipo nenhum; e o
BF16 da série não é arquivo único: `BF16/Qwen3.5-35B-A3B-BF16-00001-of-00002.gguf`
49.826.698.560 + `…-00002-of-00002.gguf` 19.549.939.264, o que esbarra no
follow-up 4.

`routable: false` nos três, e a regra passou a estar escrita: `SO_PARA_BAIXAR`
(`catalog.rs:866`) é a lista de séries sem quant escolhível, cada uma com o
motivo, e `every_size_has_a_routable_baseline` confere os dois lados — série nova
sem escolhível precisa entrar lá com o motivo, e uma série listada que passou a
ter escolhível tem de sair. O motivo escrito então foi o número: 22 GB de pesos
não cabem inteiros numa placa. Ele foi trocado na #111, quando a série passou a
ter catorze arquivos e o mais leve deles (10.656.955.008) cabe — o que falta não
é mais conta, é corrida, e a frase hoje dita pelo teste está na seção abaixo.
`support_repo` é o `Qwen/Qwen3.5-35B-A3B` que o próprio cabeçalho declara em
`general.base_model.0.repo_url`, e o `tokenizer.json` dele tem `lfs.oid`
`5f9e4d49…` com 12.807.982 bytes — **a mesma soma do Ornith 1.5 9B, do Qwen3.5 4B
e do 9B**, medida arquivo por arquivo. O comentário de `SHA256_APOIO` dizia
"aparece duas vezes"; eram três, e agora são quatro, e foi corrigido na fonte em
vez de ficar torto.

Uma frase que o arquivo declara e o aplicativo ainda não usa:
`general.tags = ["unsloth","image-text-to-text"]`, com `mmproj-BF16/F16/F32` ao
lado no repositório. O Aether não tem caminho de visão, então ele entra como
modelo de texto — o que é escolha nossa, registrada, não capacidade medida.

### Vermelho medido por mutação

As mensagens abaixo são as que foram impressas naquela passagem. Três coisas
mudaram na #111: o texto do gate do item 1 hoje é *"…carrega iq3_s, que não tem
conta de bloco medida — sem ficha não há veredito a mostrar"*; o comprimento do
item 3 é `[; 60]`, com o mesmo mecanismo; e a mutação do item 5 deixou de existir
— o tipo 16 agora pesa, e o fixture correspondente está reescrito no fim da seção
`## FEITO — #110 e #111`.

Cinco mutações, cada uma revertida, todas com a mensagem certa:

1. um `iq3_s` inventado na linha do `granite-4.2-3b-Q4_K_M.gguf` → cai o gate do
   censo: *"granite-4.2-3b-Q4_K_M.gguf carrega iq3_s, que o leitor de cabeçalho
   recusa"*;
2. uma linha trocada por um nome que nenhum arquivo do catálogo tem → cai o mesmo
   gate: *"Qwen3.5-35B-A3B-Q5_K_M.gguf não tem censo medido: o catálogo oferece o
   que não leu"*;
3. apagar uma linha da tabela → o comprimento `[; 49]` não compila, então nem
   chega a teste;
4. `qwen35moe!("Q4_K_M", …, true)` mantendo a série em `SO_PARA_BAIXAR` → cai
   *"está em SO_PARA_BAIXAR mas passou a ter quant escolhível"*; e esvaziando a
   lista, cai *"não tem nenhuma quant escolhível — entre em SO_PARA_BAIXAR com o
   motivo, ou dê-lhe uma"*. As duas direções, portanto;
5. no fixture de `alojamento.rs`, trocar o tipo 16 pelo 14 (`q5_k`, conhecido) →
   cai *"iq2_xxs não está na tabela do leitor de cabeçalho"*, porque a ficha
   voltou a sair: o vermelho é sobre o número, não sobre o fixture.

Gates desta passagem: `cd src-tauri && cargo test --lib` → **584 passaram, 0
falharam, 11 ignored, finished in 5,78 s** (eram 582; dois a mais no total porque
o gate velho de etiquetas virou dois — `cargo test --lib catalog` → **28
passaram, 0 falharam, finished in 4,64 s**, eram 27); `cargo test --lib
alojamento` → **35 passaram, 0 falharam, finished in 0,02 s** (eram 34); `cargo
check --all-targets` → **Finished em 3,35 s, zero avisos**; `npm run build` →
**OK, `✓ built in 180 ms`**, maior bloco `react` a 210,53 kB. Nenhum `.tsx` foi
tocado: as três entradas novas chegam à tela pelos dados do catálogo, e a frase
do veredito dividido já existia.

## FEITO — #110 e #111: a tabela de blocos medida, e o leitor que passou a ser nosso

Duas passagens, um só motivo. O portão da #109 era uma lista de 15 nomes copiada
do parser do candle, e copiar a lista de outro programa é aceitar o que ele não
conhece como limite do que o Aether sabe medir: foi isso que deixou **onze**
arquivos de fora quando o censo deles já estava lido e os blocos já podiam ser
pesados. #110 mediu os blocos; #111 trocou o leitor e abriu a porta.

### #110 — `cabecalho::BLOCOS`: 23 linhas, cada uma com uma conta de arquivo

A tabela está em `cabecalho.rs:106` — `(número do tipo, Bloco { nome, elementos,
bytes })` — e é a conta de `ggml`, não a tabela de um doc:

| tipo | nome | elementos × bytes | bits por elemento |
| --- | --- | --- | --- |
| 0 | `f32` | 1 × 4 | 32 |
| 1 | `f16` | 1 × 2 | 16 |
| 2 | `q4_0` | 32 × 18 | 4,5 |
| 3 | `q4_1` | 32 × 20 | 5 |
| 6 | `q5_0` | 32 × 22 | 5,5 |
| 7 | `q5_1` | 32 × 24 | 6 |
| 8 | `q8_0` | 32 × 34 | 8,5 |
| 10 | `q2_k` | 256 × 84 | 2,625 |
| 11 | `q3_k` | 256 × 110 | 3,4375 |
| 12 | `q4_k` | 256 × 144 | 4,5 |
| 13 | `q5_k` | 256 × 176 | 5,5 |
| 14 | `q6_k` | 256 × **210** | 6,5625 |
| 16 | `iq2_xxs` | 256 × 66 | 2,0625 |
| 17 | `iq2_xs` | 256 × 74 | 2,3125 |
| 18 | `iq3_xxs` | 256 × 98 | 3,0625 |
| 19 | `iq1_s` | 256 × **50** | 1,5625 |
| 20 | `iq4_nl` | 32 × 18 | 4,5 |
| 21 | `iq3_s` | 256 × 110 | 3,4375 |
| 22 | `iq2_s` | 256 × 82 | 2,5625 |
| 23 | `iq4_xs` | 256 × 136 | 4,25 |
| 29 | `iq1_m` | 256 × 56 | 1,75 |
| 30 | `bf16` | 1 × 2 | 16 |
| 39 | `mxfp4` | 32 × 17 | 4,25 |

**Como cada linha foi medida.** Com `Range: bytes=…` sobre a URL LFS resolvida, o
cabeçalho de cada arquivo foi lido sem baixar o arquivo, e sobre as fichas dos
tensores rodaram três contas juntas: o espaçamento `offset[i+1] − offset[i]`
fixa os bytes por elemento de um tensor que não é o último do seu bloco de tipo;
`div_ceil(elementos, bloco) × bytes` somado sobre todos os tensores, mais
`início_dos_dados` (`info_fim` arredondado para cima no alinhamento declarado),
tem de devolver o `size` publicado; e cada vizinhança de ficha tem de cair no
próximo múltiplo do alinhamento. **74 arquivos no cache desta rodada, todos com
`diff 0` e zero violações de vizinhança**; os onze IQ/MXFP4 foram re-rodados
nesta passagem, cada um com 733 fichas conferidas tensor a tensor.

O que a conta não observa é a granularidade do bloco: `32 × 17` e `256 × 136`
dão o mesmo bits por elemento. A coluna `elementos × bytes` é a convenção do
ggml sobre um bpw medido; o bpw é o que a evidência fixa.

**Duas linhas erradas saíram da primeira tabela, e as duas foram pegas por
arquivo, não por doc.** `iq1_s` estava em 52 (copiado de tabela alheia) — em
`Qwen3-30B-A3B-UD-IQ1_S.gguf` nenhum tensor `iq1_s` é o último, então a soma não
distingue 50 de 52; foram **58 janelas offset-a-offset** que reprovaram a 52 (em
`blk.0.ffn_gate_exps.weight`, 786.432 blocos ocupam 39.321.600 bytes e a linha
errada pediria 40.894.464). `q6_k` estava em 208 e com 210 batem os arquivos IBM
que estavam saindo curtos em 163.840 e 409.600 bytes. As duas correções hoje têm
tripwire em `a_tabela_de_blocos_e_a_medida_e_nao_uma_lista_de_desejos`
(`cabecalho.rs:601`), que também confere os bpw dos quatro tipos que o candle
recusava (`iq2_xxs` 2,0625, `iq3_xxs` 3,0625, `iq4_xs` e `mxfp4` 4,25).

**O que ficou de fora, e por quê.** Um tipo só entra com bloco medido em arquivo
real. Não entraram: 4, 5, **9 (`q8_1`)**, **15 (`q8_k`)**, 24–28 (`i8`, `i16`,
`i32`, `i64`, `f64`), 31–38 e 40+. Os dois primeiros nomes são o caso interessante:
o parser do candle os conhece e calcularia um peso para eles, mas **nenhum dos 74
arquivos lidos carrega um tensor daqueles** — pesar por tabela emprestada seria
prometer um número que ninguém mediu. Está escrito em
`tipo_sem_arquivo_lido_nao_tem_conta` (`cabecalho.rs:621`), que inclui os números
133, 139, 145 e 154 vistos em sondas de fork: não são do motor que o aplicativo
carrega.

### #111 — o leitor

`ai/cabecalho.rs` é o leitor: `ler` (`:248`) e `ler_de` (`:269`) percorrem magic
(ou byte-reversal), versão, contagens, os 13 tipos de valor do GGUF e as fichas
de tensor; `Tensor::bytes` (`:176`) faz `div_ceil(elementos, bloco) × bytes` com
produto saturante; `Cabecalho::bytes_por_tensor` (`:221`) recusa o arquivo no
primeiro tipo sem conta, nomeando o tensor e o número lido — texto em
`cabecalhoTipoSemConta`, nos dois idiomas — e `pesos` (`:235`) é a dobra dessa
mesma conta. `ALINHAMENTO_PADRAO` é 32 (`:60`). O arquivo não precisa estar
inteiro na mão de ninguém: nada aqui lê a região de dados.

O alojamento passou a usar isto: `fichar` (`alojamento.rs:546`) abre o cabeçalho
por `cabecalho::ler` (`:515`) e `por_camada` (`:490`) recebe o `Cabecalho` do
leitor. O candle continua carregando os pesos do granite — é outra coisa, medir não
é executar — mas desde a #116 ele não é mais o caminho de CPU: sem placa a
llama.cpp conta no processador, e o candle é o degrau abaixo dela.

**A troca não muda um número de arquivo que já tinha veredito.** O candle não
expõe `GgmlDType::from_u32` (é `pub(crate)`), mas `block_size()` e `type_size()`
são `pub`, e a régua chama os dois: `a_conta_nova_bate_com_a_do_parser_antigo`
(`cabecalho.rs:1129`) percorre as 23 linhas e, para os **13 tipos que os dois
leitores conhecem**, confere `bloco.elementos == block_size`, `bloco.bytes ==
type_size` e `Tensor::bytes() ==` a conta antiga, em **9 formas** (1, 31, 32, 33,
255, 256, 257, 1024 e 4096×4096 — as que não fecham bloco, inclusive). **117
acordos byte a byte**, e o `assert_eq!(conferidos, 13 × 9)` é o que avisa se um
dos lados encolheu. Os dez que só este leitor pesa estão fixos no teste como
`[16, 17, 18, 19, 20, 21, 22, 23, 29, 39]`; os dois que o parser antigo nomeava e
aqui continuam recusa (9 e 15) estão logo abaixo, com a afirmação de que a conta
antiga daria um número — e é justamente por isso que não dá para confiar nela.

**A mesma identidade, agora no disco.** `o_peso_lido_no_disco_bate_com_o_tamanho_do_arquivo`
(`cabecalho.rs:1186`) varre os 60 specs do catálogo, abre o que está em
`%LOCALAPPDATA%\ai.aether.core\models` **com o tamanho publicado inteiro** (um
`.part` não fecha por definição, então não entra) e confere
`inicio_dos_dados + Σ fichas == tamanho do arquivo` e `pesos() == Σ`. Rodado
nesta máquina: **4 de 60** — `Qwen3-Embedding-0.6B-Q8_0.gguf`,
`Qwen3-Embedding-0.6B-f16.gguf`, `granite-4.2-3b-Q4_K_M.gguf` e
`Ornith-1.5-9B-Q4_K_M.gguf`. O teste imprime a contagem em voz alta porque um
"ok" com zero arquivos na mão não é evidência. Em `cabecalho.rs:939` a mesma
identidade está fixada para o `UD-IQ2_XXS` (as 733 fichas por tipo, `início`
10.989.696, soma batendo com 10.656.955.008) e em `cabecalho.rs:990` as 23
linhas da tabela são lidas pelo parser de verdade, uma ficha por tipo.

### Os onze arquivos que voltaram

Proveniência de cada número: `GET /api/models/unsloth/Qwen3.5-35B-A3B-GGUF/tree/main?blobs=true`
em 2026-11-24 — o `lfs.oid` é o sha256 de 64 hex do arquivo inteiro (o `oid`
liso é o sha de 40 hex do blob git e não serve para o catálogo).

| arquivo | bytes publicados | censo | sha256 |
| --- | --- | --- | --- |
| `…-MXFP4_MOE.gguf` | 21.587.638.912 | `f32, mxfp4, q5_k, q6_k, q8_0` | `0f135a59…23cd770` |
| `…-Q3_K_M.gguf` | 16.356.375.168 | `f32, iq3_xxs, iq4_xs, q6_k, q8_0` | `5607c8fc…d5ab1b2d` |
| `…-Q3_K_S.gguf` | 15.265.856.128 | `f32, iq3_s, iq3_xxs, q6_k, q8_0` | `315cce21…c08e625d` |
| `…-UD-IQ2_M.gguf` | 11.391.613.568 | `f32, iq2_xxs, iq3_xxs, q4_k, q5_k, q6_k, q8_0` | `187b5bbb…e15ace0e` |
| `…-UD-IQ2_XXS.gguf` | 10.656.955.008 | `f32, iq2_s, iq2_xxs, q4_k, q5_k, q6_k, q8_0` | `2a809de3…af84718b` |
| `…-UD-IQ3_S.gguf` | 13.583.383.168 | `f32, iq2_s, iq3_s, q6_k, q8_0` | `ca6e9542…898c061fb6` |
| `…-UD-IQ3_XXS.gguf` | 13.080.066.688 | `f32, iq2_s, iq3_xxs, q6_k, q8_0` | `9f7f5928…f5dd96cd` |
| `…-UD-IQ4_NL.gguf` | 17.821.719.168 | `f32, iq3_s, iq4_nl, q6_k, q8_0` | `6aec31ed…50e3e444` |
| `…-UD-IQ4_XS.gguf` | 17.486.174.848 | `f32, iq3_s, iq4_xs, q6_k, q8_0` | `745def08…bf1be123` |
| `…-UD-Q2_K_XL.gguf` | 12.156.754.560 | `f32, iq2_xs, iq3_xxs, iq4_xs, q4_k, q5_k, q6_k, q8_0` | `855b8d11…db3e77270` |
| `…-UD-Q3_K_XL.gguf` | 16.601.176.704 | `f32, iq3_xxs, iq4_xs, q5_k, q6_k, q8_0` | `fef37f41…300fc945` |

Os onze têm 733 fichas, `info_fim` 10.989.695 e alinhamento 32 (região de dados
começando em 10.989.696), e cada um fecha com `diff 0` — a série completa de 14
arquivos oferecidos (os três de antes mais estes) vai de 10.656.955.008 a
28.852.861.568 bytes.

**Os fatos de cabeçalho foram lidos arquivo por arquivo**, com um `Range` de 1 MiB
e saída antecipada assim que o conjunto que o catálogo usa está completo:
`GGUF v3`, 733 tensores, 52 pares, 37 pares percorridos, fim do bloco de KV em
1877, sem corte. Em todos os onze: arquitetura `qwen35moe`, `general.name`
`Qwen3.5-35B-A3B`, `size_label` `35B-A3B`, licença `apache-2.0`, 40 camadas,
janela 262.144, `embedding_length` 2048, 256 peritos com 8 usados,
`full_attention_interval` 4. Não é um transferido para os outros — é a mesma
leitura feita onze vezes, e ela é o que autoriza as linhas do catálogo.

No código, o que mudou: `SHA256_QWEN35_35B_A3B_GGUF` passou de 3 para **14**
linhas (`catalog.rs:325`), `CENSO` de 49 para **60** (`:434`), `CATALOG` de 47
para **58** (`:511`) com os onze `qwen35moe!` (`:193`), e a constante `DA_FICHA`
— os 15 nomes copiados do candle — **saiu**: o gate (`catalog.rs:834`) agora
pergunta `cabecalho::numero_do_nome(tipo)`, que é a tabela medida, não a lista de
outro programa. O motivo de `routable: false` na série também foi reescrito em
`SO_PARA_BAIXAR` (`catalog.rs:866`): não é mais "22 GB não cabem numa placa",
porque o `UD-IQ2_XXS` tem 9,93 GiB de pesos — oito dos onze medem menos de 16
GiB (9,93; 10,61; 11,32; 12,18; 12,65; 14,22; 15,23; 15,46 GiB). É que nenhum
arquivo da série rodou com a placa medida, e o que falta agora é corrida, não
conta. Peso de arquivo não é veredito: o alojamento soma o KV da janela pedida e
a folga da biblioteca por cima, e esse número continua saindo da conta sem
placa.

### Vermelho medido por mutação (desta passagem)

Quatro mutações, cada uma revertida e conferida no diff:

1. `q6_k` a **209** bytes por bloco (o número que a primeira tabela tinha) → 5
   vermelhos, e o que importa é o que não é tripwire:
   `o_peso_lido_no_disco_bate_com_o_tamanho_do_arquivo` cai em `cabecalho.rs:1211`
   com *"…granite-4.2-3b-Q4_K_M.gguf: as 363 fichas não fecham o tamanho do
   arquivo"*. A soma de um arquivo real no disco reprova a linha errada sem
   ninguém ter de saber que ela estava errada. Caem junto `:986` (o censo do
   IQ2_XXS), `:1032` (a ficha lida pelo parser), `:1139` (a régua do candle) e
   `:617` (o tripwire que nomeia `q6_k`).
2. apagar a linha do `bf16` → 4 vermelhos: o comprimento 22 ≠ 23 (`:602`),
   *"são 13 os tipos que os dois leitores conhecem"* (`:1152`), o desembrulho da
   linha na leitura real (`:1024`, que com a linha ausente vira `None` e panic) e
   *"granite-4.2-3b-bf16.gguf carrega bf16, que não tem conta de bloco medida"*
   (`:1048`). A conferência em disco **não** caiu — nenhum dos quatro arquivos
   baixados é bf16, e é por isso que a contagem dela é impressa.
3. um `q8_1` inventado na linha do censo do `UD-IQ2_XXS` → o gate do catálogo cai
   em `catalog.rs:833` com *"…carrega q8_1, que não tem conta de bloco medida —
   sem ficha não há veredito a mostrar"* e o espelho dele em `cabecalho.rs:1048`.
   Os dois lados do contrato, porque um lê a lista do outro.
4. trocar a chave do censo do `UD-Q3_K_XL` por um nome que nenhum arquivo tem →
   `catalog.rs:831`: *"Qwen3.5-35B-A3B-UD-Q3_K_XL.gguf não tem censo medido: o
   catálogo oferece o que não leu"* — é o caso que a oferta sem leitura pega, e
   ele só existe porque o arquivo passou a ser oferecido.

Gates desta passagem: `cd src-tauri && cargo test --lib` → **616 passaram, 0
falharam, 11 ignored, finished in 6,77 s**; recortado por módulo, `--lib
cabecalho` → **34 passaram, 0 falharam, 1,60 s**, `--lib catalog` → **29
passaram, 0 falharam, 5,53 s**, `--lib alojamento` → **36 passaram, 0 falharam,
0,02 s**; `cargo check --all-targets` → **Finished em 4,16 s, zero avisos**;
`npm run build` → **OK, `✓ built in 188 ms`**, maior bloco `react` a 210,53 kB.
Nenhum `.tsx` foi tocado, e o aplicativo não foi aberto: as onze entradas novas
chegam à tela pelos dados do catálogo, pelo mesmo caminho das outras.

## FEITO — #112: a folga do grafo saiu do `sched_reserve`, e o degrau que a substituiria foi retirado

### O que estava cobrando a menos

`const FOLGA_DE_COMPUTACAO: u64 = 220 * 1024 * 1024;` era um número só para toda
arquitetura e toda janela, e ele era — isso está no histórico — o
`sched_reserve: Vulkan0 compute buffer size` de **uma** carga: granito 4.2-3B
Q4_K_M em 4096, onde a biblioteca escreveu 220,02 MiB. Na mesma janela, o
ornith-1.5-9B registrou 521,02 MiB. Cobrar 220 onde a placa guardou 521 é a
direção de erro que este módulo não pode cometer: o veredito promete uma placa
mais gorda do que ela é, e a pessoa descobre a verdade no OOM, não na tela.

### As treze cargas que viraram a conta

Duas arquiteturas, sete janelas, cada número lido do
`sched_reserve:    Vulkan0 compute buffer size` que a própria biblioteca escreve
ao carregar, com a placa ligada (RX 9060 XT, driver AMD, flash attention):

| janela | granite-4.2-3b Q4_K_M | ornith-1.5-9b Q4_K_M |
| --- | --- | --- |
| 4.096 | 220,02 MiB | 521,02 MiB |
| 32.768 | 248,02 MiB | 549,02 MiB |
| 49.152 | 264,02 MiB | 565,02 MiB |
| 61.440 | 276,02 MiB | 577,02 MiB |
| 65.536 | 280,02 MiB | 581,02 MiB |
| 131.072 | 344,02 MiB | 645,02 MiB |
| 262.144 | — | 773,02 MiB |

A segunda linha de cada carga (`sched_reserve: Vulkan_Host compute buffer size`:
24,02 no granito de 4096, 36,03 no ornith, 84,02 e 288,03 nas janelas grandes)
não é a placa: `placas:` mostra o dispositivo host como `tipo=0` com
`total=32687 MiB`, que é a RAM da máquina, não os 16.304 MiB da placa. Ela fica
fora da conta por ser RAM, não por descuido.

Os dois crescem **exatos 1.024 bytes por célula de janela**: `216,02 + janela /
1024` fecha as seis linhas do granito e `517,02 + janela / 1024` fecha as sete do
ornith, com diferença zero nos treze pontos. Ou seja: a parte proporcional está
medida, não estimada. O piso, não — ele é do grafo de cada arquitetura (a híbrida
paga o dobro, porque as camadas recorrentes atravessam o grafo com estados que a
densa não tem), e nada no cabeçalho autoriza adivinhar o piso de um arquivo que
ainda não rodou.

### O que `folga_de_computacao` cobra

`PISO_DE_COMPUTACAO = 518 MiB` — o **maior piso medido, arredondado para cima**,
não o piso daquele arquivo — mais `CRESCIMENTO_POR_CELULA = 1024` bytes vezes
`celulas_da_janela`, o mesmo arredondamento em múltiplos de 256 que o KV paga.
A função está em `alojamento.rs:75` e entra nos dois lados do veredito: o
predicado de `Inteira` (`:644-649`) e o `orcamento` que decide quantas camadas
sobem (`:657-659`). Cobrir o piso menor deixa folga sobrando; cobrir o maior a
menos seria a promessa falsa de cima.

O gate que prende isso é `a_folga_cobre_cada_buffer_de_computacao_medido_na_placa`
(`alojamento.rs:1766`), sobre a tabela `COMPUTACAO_MEDIDA` (`:1749`) com os
treze pontos. Ele aperta as duas pontas: `cobrada > medida` (cobrar a menos é o
erro proibido) **e** `cobrada − medida ≤ 302` MiB — a diferença entre os dois
pisos medidos (517,02 − 216,02 = 301) mais o MiB do arredondamento, porque
folga que passa disso virou imposto e encolhe a janela que a pessoa podia ter. A
segunda metade do teste varre os trinta e seis pares possíveis entre as treze
cargas da mesma arquitetura e exige que a diferença registrada seja exatamente
Δcélulas / 1024.

**Vermelho medido por mutação**, cada uma revertida e conferida no diff:

1. `PISO_DE_COMPUTACAO` 518 → 220 (o número velho): o gate cai em
   *`ornith-1.5-9b em 4096: a conta cobrou 224 MiB e a placa registrou 521`*. É
   exatamente o defeito que existia antes desta passagem, agora pego por teste.
2. `CRESCIMENTO_POR_CELULA` 1024 → 512: cai em
   *`ornith-1.5-9b em 4096: a conta cobrou 520 MiB e a placa registrou 521`* —
   um MiB a menos, e é para pegar um MiB que o `cobrada > medida` existe. A
   sobra dessa mutação cresce com a janela: em 262.144 a conta seria 646 contra
   os 773 registrados.

### O que a mesma carga confirmou sem mudar uma linha de conta

- **KV exato nas duas arquiteturas.** Previsto 320,00 MiB em 4096 →
  `llama_kv_cache: size = 320.00 MiB ( 4096 cells, 40 layers, 1/1 seqs)`; previsto
  128,00 → `128.00 MiB ( 4096 cells, 8 layers)`; o granito em 65.536 prevê
  5.120,00 e a biblioteca escreve `Vulkan0 KV buffer size = 5120.00 MiB`; o
  ornith em 131.072 prevê 4.096 e a biblioteca escreve `4096.00 MiB (131072
  cells, 8 layers)`. Quatro cargas conferidas, célula a célula, duas por
  arquitetura.
- **O estado recorrente estava certo.** O previsto da fórmula era 52,34 MiB; a
  carga do ornith escreveu `llama_memory_recurrent: size = 50.25 MiB ( 1 cells,
  32 layers, 1 seqs 0 rs_seq), R (f32) 2.25, S (f32) 48.00, P 0.00`. Cobramos
  2,09 MiB a mais — a direção segura — e a composição (R + S, P zero, 32 camadas,
  uma célula) é a que o módulo assume.
- **Pesos: um exato, um com sobra.** Granito: `1.998,84 (Vulkan0) + 137,81
  (CPU_Mapped) = 2.136,65`, o mesmo que a ficha prevê. Ornith: a ficha prevê
  5.501,86 e a biblioteca nomeia `4.812,25 + 545,62 = 5.357,87` — 143,99 MiB
  cobrados a mais, causa não medida. É conservador, e por isso ficou registrado
  em vez de ser arredondado para baixo.
- **A biblioteca confere o próprio buffer.** No fim de cada carga:
  `~llama_context: Vulkan0 compute buffer size is 220.0156 MiB, matches
  expectation of 220.0156 MiB` (e 521,0215 no ornith). O número da descarga é o
  da carga, e o do granito é exatamente o que a constante cobrava de **todos**
  os arquivos — inclusive dos que registram 521, 581 e 773 MiB.

### O degrau foi retirado por medida, não por gosto

A passagem anterior tinha trocado a folga por um "degrau" de ~4,3 GiB, e o
argumento era o delta de `VRAM cedida`. Ele não se sustenta. Para **uma** carga
viva de granito em 65536 — a biblioteca nomeando 1.998,84 + 5.120,00 + 280,02 =
**7.398,86 MiB** na placa —, o `livre` que a ggml respondeu foi:

| quem leu | quando | `livre` |
| --- | --- | --- |
| o processo que carregava | logo depois da carga | 3.710,22 MiB |
| outro processo carregando o mesmo arquivo | mesma janela, outra corrida | 3.808,06 MiB |
| um segundo processo, amostra 1 | dentro da vida da carga | 15.445 MiB |
| um segundo processo, amostra 2 | 1,2 s depois | 11.231 MiB |
| um segundo processo, amostra 3 | 1,2 s depois | 15.445 MiB |

Cinco números para um estado só da placa, e dois deles são exatamente o piso da
placa ociosa (15.445,62 MiB). As três amostras têm carimbo de relógio e caem
dentro da janela de vida do processo que carregava, então não é amostra feita com
a placa já vazia. Consequência direta: **o delta de `VRAM cedida` não é
atribuível**, e nada que se construa sobre ele — inclusive um degrau de 4,3 GiB —
pode ser chamado de medida. A sonda continua imprimindo o delta, mas ao lado das
duas leituras que o produzem (`livre {} → {}`), para que ele se descreva sozinho
como o que é: uma diferença entre duas contas do estado da placa inteira, e não
uma medição do nosso processo. Está registrado no doc-comment de `alojamento()`
(`bin/engine_probe.rs:234-247`).

Há um corolário que ainda não tem instrumento: o `livre` que o **aplicativo** vê
antes de carregar é essa mesma leitura, feita por um processo que ainda não alocou
nada. Ele vale para o nosso orçamento, não para a placa compartilhada — um
terceiro programa usando 8 GiB não aparece no veredito. Isto é #114.

### O que esta passagem NÃO fechou: o grafo de uma carga dividida

Seis cargas com `despacho` forçado pela sonda (`alojar:262144|20` — o número
depois da barra impõe as camadas sem tocar no veredito, único jeito de ver um
dividido numa placa onde ele não dividiria sozinho):

| arquivo | janela | camadas forçadas | `Vulkan0 compute buffer` | `splits` |
| --- | --- | --- | --- | --- |
| ornith-1.5-9b | 262.144 | 20 | 1.858,00 MiB | 239 / 24 |
| ornith-1.5-9b | 262.144 | 10 | 1.858,00 MiB | 404 / 38 |
| ornith-1.5-9b | 262.144 | 0 | 1.858,00 MiB | 540 / 49 |
| ornith-1.5-9b | 32.768 | 10 | 517,00 MiB | 404 / 38 |
| granite-4.2-3b | 65.536 | 20 | 222,00 MiB | 233 / 2 |
| granite-4.2-3b | 65.536 | 0 | 416,98 MiB | 444 / 1 |

Não há regra aqui: o ornith de 262.144 registra o mesmo buffer com 20, 10 ou
zero camadas na placa, o granito de 65.536 registra **menos** dividido (222,00)
que inteiro (280,02) e **mais** com tudo na RAM (416,98) que com as camadas na
placa, e a coluna que se move junto é `splits` — a biblioteca dimensiona o grafo
pelo recorte entre dispositivos, não pela janela. `max_nodes` é 13.664 em cada
carga do ornith e 2.904 em cada carga do granito — os dezenove logs da sonda,
inteira ou dividida —, então ele também não é a variável que falta.

Isso fica de fora da conta nova, e o custo é dito com o número: um ornith
dividido em 262.144 registra 1.858 MiB de grafo enquanto
`folga_de_computacao` cobra 774 MiB para as 262.144 células daquela janela
(518 + 256) — **1.084 MiB cobrados a menos**, a direção errada. Não virou regra
porque seis pontos não fazem uma, e porque a coluna do `splits` mostra que a
variável que falta não está no cabeçalho. É #113, e a chave para medi-lo já
existe na sonda.

### Gates desta passagem

`cd src-tauri && cargo test --lib` → **617 passaram, 0 falharam, 11 ignored,
finished in 6,34 s**; recortado, `--lib alojamento` → **37 passaram, 0 falharam,
0,01 s**, `--lib router` → **21 passaram, 0 falharam, 0,01 s**; `cargo check
--all-targets` → **Finished em 3,38 s, zero avisos**; `npm run build` → **OK, `✓
built in 171 ms`**, maior bloco `react` a 210,53 kB. Nenhum `.tsx` foi tocado e o
aplicativo não foi aberto: tudo que esta passagem mudou sai de uma conta e de uma
sonda de linha de comando. As cargas que produzem os treze pontos são da sonda,
rodadas serialmente com o aplicativo fechado.

O único vermelho que a passagem produziu no caminho foi honesto, e é por isso que
vale registrar: `router.rs` tinha um fixture com 700 MiB livres na placa para
dizer "sem orçamento para o modelo grande". Com a folga real de 522 MiB em 4096,
aquele número não descrevia mais nenhuma placa possível — o teste passou a
reprovar o próprio fixture. Ele foi re-derivado para 1024 MiB, e a conta está no
comentário: 1024 − 320 de KV − 522 de folga = 182 MiB de orçamento, e nove das
quarenta postas de 20 MiB do 3B cabem ali; o 8B paga 640 MiB de KV e a mesma
folga antes da primeira camada, e não sobra nenhuma. A asserção `(1, true)` /
`(2, true)` não mudou.

## O que esta frente NÃO mediu, e é exatamente por isso que está registrado

Nada da **frente do catálogo** rodou; o que rodou desde então é a carga de
medição da #112, e onde ela mudou o veredito sobre um item, o item foi reescrito
e diz o que ainda falta. O portão continua sendo o mesmo: placa com aplicativo
aberto, que a instrução de não abrir o app mantém fechado.

1. **Alojamento real dos arquivos novos.** Os `bytes` são o tamanho do arquivo,
   e o veredito de quantas camadas sobem sai de `alojamento::alojar` (`:629`) com
   a VRAM livre medida na hora. Nenhum dos GGUFs novos passou pela placa, e a
   série agora tem catorze arquivos oferecidos: oito têm pesos abaixo dos 16 GiB
   da placa desta máquina (9,93 a 15,46 GiB), e para esses `Inteira` é uma
   possibilidade que depende da janela pedida e da VRAM livre medida na hora —
   nenhum número saiu do aplicativo. Os outros seis começam acima do teto da
   placa só de pesos (16,29 a 26,87 GiB), então o veredito esperado deles é
   `Dividida`, e a frase que a tela diz com `camadas`, `na_placa` e `em_ram`
   nunca foi vista saindo do aplicativo. É #39 que fecha isso. O que #106 e #107
   fecharam foi a conta: `camadas_de_cache` (`:420`) e `taxa_kv` (`:479`) leem
   `head_count_kv`, `sliding_window_pattern`, `key_length_swa` e
   `shared_kv_layers` como o b12387 os lê, `memoria_recorrente` (`:359`) e
   `estado_recorrente` (`:404`) leem o gated delta net, e os 344.064 do Gemma 4
   como os 20.480 do A3B são aritmética conferida em teste, não medida em carga.
   **O estado recorrente em carga deixou de ser um deles:** a #112 carregou o
   híbrido que existe em disco e a biblioteca escreveu
   `llama_memory_recurrent: size = 50.25 MiB ( 1 cells, 32 layers, 1 seqs 0
   rs_seq), R (f32) 2.25, S (f32) 48.00, P 0.00` contra os 52,34 MiB previstos —
   a composição, as camadas e a célula batem, e a conta cobra 2,09 MiB acima. O
   que continua não medido é o estado dos arquivos que não estão em disco (os 62,83
   MiB previstos do A3B) e o de uma carga dividida, onde a biblioteca reparte o
   RS entre `CPU` e `Vulkan0` (`37,69 + 12,56` no ornith de 32.768 com 10
   camadas) e o veredito hoje não modela esse corte. A sonda imprime previsto e
   registrado lado a lado (`bin/engine_probe.rs:268-272` e `:293-315`).
2. **A qualidade do texto e o custo por rodada** (tokens de entrada e saída,
   t/s) — medição de turno, placa, aplicativo aberto.
3. **Chamada de ferramenta no Gemma 4.** O laço do Aether não lê a gramática do
   template: ele define a sua, em `chat.rs:242-247` (abertura de chamada, nome
   de função e parâmetros em XML), e `inicio_de_chamada` (mesmo arquivo, `:255`)
   escreve essa gramática como continuação forçada — `session.rs:513` é o ponto
   que a injeta. O que o modelo viu no treino é a gramática do próprio template,
   e no Gemma 4 ela é diferente. Se a continuação forçada chega para governar o
   resto do bloco, é medição com a placa, não leitura de cabeçalho.
4. **A gramática do 35B A3B, lida mas não corrida.** Diferente do Gemma 4: o
   `tokenizer.chat_template` do próprio arquivo abre a chamada com a tag
   `tool_call`, nomeia a função em `function=` e fecha cada valor em
   `parameter=` — que é, byte a byte, o alfabeto de `CALL_OPEN`, `FN_OPEN` e
   `PARAM_OPEN` (`ai/chat.rs:242-247`). Isso é fato do arquivo, medido aqui. O
   que o modelo *entrega* nesse laço — se ele abre a chamada e fecha os
   parâmetros quando o laço pede — continua sendo turno com placa, e vale para as
   cinco famílias de conversa do catálogo, não só para esta.
5. **Os tipos IQ e MXFP4 não têm conta.** *Fechado na #110/#111, e este parágrafo
   fica como foi medido.* O censo sabia quais tipos um arquivo
   usa; o que faltava era o `block_size`/`type_size` deles no leitor de cabeçalho do
   aplicativo, que era o do candle — `GgmlDType::from_u32` conhece os números 0…15
   e o 30, e caía no `bail!` para qualquer outro, e é por isso que as
   onze quants com IQ ou MXFP4 do A3B ficavam na fila, entre elas os
   oito arquivos cujos pesos caberiam nos 16 GiB de uma placa (a menor quant que
   o leitor abria media 18,82 GiB de pesos). Todos os onze são arquivo único e têm
   `lfs.oid` medido no `tree/main` de 2026-11-24. A tabela de blocos agora tem a
   conta de todos eles em `cabecalho::BLOCOS`, conferida arquivo por arquivo contra
   o tamanho publicado — `iq2_xxs` 66, `iq2_xs` 74, `iq2_s` 82, `iq3_xxs` 98,
   `iq3_s` 110, `iq4_nl` 18 por 32 elementos, `iq4_xs` 136 e `mxfp4` 17 por 32 —
   e os dois `iq1` (50 e 56) vieram da sonda fora dos onze, já que nenhum arquivo
   daqui os usa. O que faltava era o parser, não a medida: os onze estão oferecidos,
   com ficha e veredito. Ver `## FEITO — #110 e #111`.
6. **O grafo de uma carga dividida não tem regra.** Seis cargas forçadas pela
   sonda (`alojar:<janela>|<camadas>`) registram 1.858,00 MiB para o híbrido em
   262.144 com 20, 10 ou zero camadas na placa, 517,00 MiB para o mesmo híbrido em
   32.768 com 10, e 222,00 / 416,98 MiB para o granito em 65.536 com 20 / zero —
   contra 280,02 da mesma carga inteira. A coluna que acompanha é `splits`, não a
   janela. Enquanto isso, `folga_de_computacao` cobra 774 MiB naquele híbrido de
   262.144: **1.084 MiB a menos**, a direção proibida. É #113.
7. **A folga que o veredito vê é a do nosso processo.** O `livre` que
   `capacidade()` devolve é a leitura que a ggml faz antes de o nosso processo
   alocar qualquer coisa, e a medida da #112 mostra que ela não enxerga o resto da
   máquina: com 7.398,86 MiB nomeados por uma carga viva, um segundo processo
   amostrou 15.445, 11.231 e 15.445 MiB livres — duas vezes o piso da placa
   ociosa. Num desktop com navegador e jogo abertos, o veredito `Inteira` quer
   dizer "cabe no orçamento que a placa nos dá", não "cabe no que sobrou para
   todos". Isto é #114, e enquanto não houver instrumento ele vale para toda tela
   que mostra VRAM livre.

## Gates da frente do catálogo

Medidos antes da passagem #106; os números atuais estão nos gates das passagens
acima (#107, #108, #109, #110/#111 e #112).

```
cd src-tauri && cargo test --lib        → 567 passaram, 0 falharam, 11 ignored, finished in 5,92 s
cd src-tauri && cargo test --lib catalog → 27 passaram, 0 falharam, finished in 4,70 s
cd src-tauri && cargo check --all-targets → Finished, zero avisos
npm run build                            → ✓ built in 250 ms, maior bloco react a 210,53 kB
```

O vermelho do instrumento novo foi medido por mutação, porque não há modelo
para rodar: com as duas linhas do Gemma 4 marcadas como não-escolhíveis,
`every_size_has_a_routable_baseline` caiu na lista explícita (`catalog.rs:886`)
e, retirada essa id da lista, caiu no laço que varre as séries do próprio
`CATALOG` (`catalog.rs:901`) — ou seja, série nova sem baseline é pega sem
ninguém precisar lembrar de escrever o nome dela. As duas mutações foram
revertidas; o diff daquela passagem (#106) fechou em 133 linhas adicionadas e 7
removidas.

## Follow-ups

1. **Leitor de cabeçalho que conheça os tipos IQ e MXFP4 — FECHADO na #110/#111.**
   Era o portão que a #109 escreveu: onze dos 23 arquivos de pesos do repositório
   tinham tipo que o candle recusava, e **oito deles eram justamente os que
   caberiam nos 16 GiB de uma placa única** — pesos de 9,93 / 10,61 / 11,32 /
   12,18 / 12,65 / 14,22 / 15,23 e 15,46 GiB, contra 18,82 GiB da menor quant que
   o leitor abria. Os onze são arquivo único, cada um com `lfs.oid` medido no
   `tree/main` de 2026-11-24 e o censo lido tensor a tensor; a menor é a
   `UD-IQ2_XXS`, 10.656.955.008 bytes. O que faltava era conta, não dado, e a
   conta foi medida: `cabecalho::BLOCOS` tem os blocos de todos eles (`iq2_xxs`
   66, `iq2_xs` 74, `iq2_s` 82, `iq3_xxs` 98, `iq3_s` 110, `iq4_xs` 136, `mxfp4`
   17 por 32 elementos), cada linha batendo com o tamanho publicado do arquivo
   onde ela aparece — o `mxfp4` deixou de ser exceção nesta rodada, o
   `MXFP4_MOE` fecha com `diff 0`. O aplicativo passou a ter um leitor próprio
   (`ai/cabecalho.rs`), e o que era "um jeito só de ler um GGUF" — o
   `gguf_file::Content::read` do candle atrás de `alojamento::fichar`
   (`alojamento.rs:546`) — hoje é o caminho de medição nosso; o codificador
   continua passando por `fichar` antes de carregar pesos (`ggml/mod.rs:603`),
   então ele também passou a ler os tipos IQ. A frase honesta na tela deixou de
   ser hipotética: `cabecalhoTipoSemConta` nomeia o tensor e o número lido, e é
   dela que os tipos 9 e 15 ainda se servem.
2. **`swa_full` desligado, se um dia valer a pena** — #106 mediu que a chave de
   janela deslizante não reduz nada com o padrão da biblioteca
   (`swa_full = true`), e há 40 das 48 camadas do Gemma 4 nesse caso. Ligar o
   contrário (`p.swa_full = false` em `ggml/ffi.rs:454-459`) encurta o cache, mas
   muda o que a atenção vê fora da janela — é escolha de qualidade, e o que a
   decide é medição com a placa, não conta de cabeçalho.
3. **Veredito de ferramenta por família** — medir, com a placa, quais das
   famílias de conversa do catálogo conseguem entregar uma chamada no formato que
   o laço parseia, e dizer isso na tela em vez de deixar a pessoa descobrir no
   meio de um turno. A leitura de cabeçalho de #109 já separou o problema e deu
   um fato novo: percorrido o `tokenizer.chat_template` de um arquivo por
   arquitetura, **quatro das quatro** declaram o alfabeto que o laço escreve
   (`granite` 9.013 caracteres, `qwen35` 7.816 no 4B e no 9B e 7.828 no Ornith
   1.5 9B, `qwen35moe` 7.817 no A3B), e o `gemma4` é o único em que as três
   aberturas — de chamada, de função e de parâmetro — não aparecem nos 18.922
   caracteres do template dele. O que falta deixou de ser "qual família fala a
   língua": é se cada uma *entrega* o bloco fechado quando o laço pede, e isso só
   a corrida diz.
4. **Downloader de pesos em partes (multi-shard) — adiado com medida, não com
   palpite.** O registro anterior dizia "sem ele, GLM-5.3-Flash e qualquer quant
   >48 GB ficam fora". Medido na passagem #107: a `llama.dll` b12387 já abre
   split sozinha (`llama-model-loader.cpp:84`, `:604-635`), então o que falta é
   só o download das partes — mas as seis partes do `unsloth/GLM-5.3-Flash-GGUF`
   UD-Q4_K_XL somam ≈200 GB, e as quants do `Qwen3.5-35B-A3B` oferecidas são
   arquivo único. A #109 mediu mais um caso concreto do mesmo tipo: o BF16 do
   A3B está em duas partes, `BF16/Qwen3.5-35B-A3B-BF16-00001-of-00002.gguf`
   49.826.698.560 e `…-00002-of-00002.gguf` 19.549.939.264. Baixar 69 GB para
   rodar o que não cabe em 16 GiB de placa nem nesta máquina não é trabalho
   pendente, é trabalho sem valor presente. Volta a ter valor quando houver um
   repositório dividido cujo arquivo caiba, e o caminho é `ai/download.rs`: a
   conferência por sha256 de cada parte antes da junção, e o `rename` só depois
   de todas elas estarem conferidas.
