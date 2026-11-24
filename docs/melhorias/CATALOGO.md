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
desta página — hoje o follow-up 5 — pedia um downloader de pesos em partes para
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

- `CamadaKv` ganhou `recorrente` (`alojamento.rs:289`) e `taxa_kv` (`:445`) soma
  só quem é `propria && !recorrente` — as 30 lineares saem do número por token.
- `memoria_recorrente` (`:325`) lê as chaves `ssm.*` e o padrão das camadas, com
  cada produto `checked`; `estado_recorrente` (`:370`) multiplica pelo número de
  camadas recorrentes. Um cabeçalho sem as quatro chaves do gated delta net não
  é híbrido e continua cobrando cache de tudo — a regra vem do arquivo, não de
  uma lista de arquiteturas.
- `Ficha.estado_recorrente` (`:93`) é preenchido por `fichar` e entra no veredito:
  `Alojamento` leva `estado` nos três casos (`:598-620`), `alojar` (`:626`)
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
  (`bin/engine_probe.rs:249-253`) e a contar o estado no esperado do delta de
  VRAM (`:309-313`), que é a metade que só a placa fecha.

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

O follow-up 2 desta página — o que esta passagem fechou — segurava a entrada do
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
foi rodada nos 49 arquivos que o catálogo oferece hoje mais 3 do A3B que ficam
fora, e **bate nos 52: 52 conformes, 0 divergentes**. É ela que fixa a numeração
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
| 9 | `q8_1` | 32 × 36 | | 23 | `iq4_xs` | 256 × 136 |
| 10 | `q2_k` | 256 × 84 | | 29 | `iq1_m` | 256 × 56 |
| 11 | `q3_k` | 256 × 110 | | 30 | `bf16` | 1 × 2 |
| 12 | `q4_k` | 256 × 144 | | 39 | `mxfp4` | não conferido |
| 13 | `q5_k` | 256 × 176 | | | | |

A linha do `q6_k` tem negrito porque foi onde o instrumento me pegou: com 208
bytes por bloco — o número que eu havia escrito de memória — o
`granite-4.2-3b-Q6_K.gguf` saía **163.840 bytes curto** e o
`granite-4.2-8b-Q6_K.gguf` **409.600**, contra os tamanhos publicados. Com 210
(`3 × QK_K/4 + QK_K/16 + 2`) os dois batem e os 52 fecham. O `alojamento.rs`
nunca esteve errado: ele usa `block_size()` e `type_size()` da própria
biblioteca, e é por isso que a tabela acima é a prova do *entendimento*, não do
código.

`mxfp4` (39) aparece no censo do `MXFP4_MOE` e é o único tipo lido cuja soma não
foi rodada — o arquivo não tem como entrar mesmo assim, e o motivo está logo
abaixo. É também a prova de que a numeração **não para no 30**: o 39 existe em
arquivo publicado, e um leitor que conhece só os números até 30 o trata como
desconhecido. Aqui a resposta dele é a certa, porque o candle também não conhece
o 39 — mas acertar por desconhecimento dos dois lados não é projeto, é sorte, e
é por isso que a linha está marcada como não conferida.

Das 23 linhas da tabela acima, **19 tiveram o tamanho conferido** contra um arquivo
publicado — são exatamente os tipos que aparecem nos 52 arquivos somados. Ficam
sem prova de arquivo `q8_1` (9), `iq4_nl` (20) e `iq1_m` (29), que nenhum dos 69
cabeçalhos lidos usa, e o `mxfp4` (39), que aparece em um arquivo lido mas cuja
soma não foi rodada: onde qualquer deles entrar em jogo, a conta tem de ser
conferida de novo. Pelo mesmo caminho, o
bits/elemento derivado do espaçamento **não existe para o `f32` em nenhum arquivo
lido** — os tensores de `f32` são os pequenos de normalização e de escala, todos
abaixo dos 8 milhões de elementos que o instrumento exigia — e faltou em quatro
arquivos para `q4_k`, `q4_1` e `q5_1` e em catorze para `q8_0`. Onde ele existiu,
concordou com o nome; onde não existiu, quem respondeu foi a soma de bytes.

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

O leitor de cabeçalho do aplicativo é o do candle, e ele aceita exatamente
`f32, f16, bf16, q4_0, q4_1, q5_0, q5_1, q8_0, q8_1, q2_k, q3_k, q4_k, q5_k,
q6_k, q8_k` (`GgmlDType::from_u32`, candle-core 0.11.1): os IQ e o MXFP4 caem no
ramo `_ => bail!("unknown dtype for tensor {u}")`, `Content::read` devolve erro,
`fichar` devolve erro e **não há veredito de alojamento**. Como o Aether não roda
o que não mediu antes (#36), a regra passou a ser o portão:

- `CENSO` (`catalog.rs:422`) — 49 linhas, uma por arquivo oferecido, com os tipos
  lidos do cabeçalho dele;
- `censo_do_arquivo` (`catalog.rs:476`) — a leitura, `None` para o arquivo que
  ninguém abriu;
- `censo_medido_de_todo_arquivo_oferecido_e_aberto_pelo_leitor`
  (`catalog.rs:779`) — cada arquivo do catálogo e dos codificadores tem de ter
  linha medida, nenhum nome pode se repetir (é a chave), e cada tipo da linha tem
  de estar na lista acima. Este teste substituiu o de etiquetas;
- `granites_sao_executaveis_pelo_caminho_de_cpu` (`catalog.rs:811`) — a parte que
  ainda é sobre o caminho de CPU, que só existe para o granite e escolhe pelo
  nome da quant, preservada;
- `tipo_fora_da_tabela_do_leitor_nao_vira_ficha_inventada`
  (`alojamento.rs:1122`) — o GGUF sintético de um tensor só: com o tipo 8 a ficha
  sai e conta os 68 bytes reais (`q8_0`, 64 elementos = 2 blocos de 34), com o
  tipo 16 `fichar` recusa citando `unknown dtype for tensor 16`. É o preço de
  entrada dos IQ, escrito como teste e não como intenção.

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
32.071.842.432, `UD-Q8_K_XL` 48.688.560.768. Dois deles merecem nota: o
`UD-Q4_K_XL` tem **o mesmo censo** do `Q4_K_M` e é maior (22.241.950.336 contra
22.016.023.168) — é o caso em que a etiqueta `XL` não compra tipo nenhum; e o
BF16 da série não é arquivo único: `BF16/Qwen3.5-35B-A3B-BF16-00001-of-00002.gguf`
49.826.698.560 + `…-00002-of-00002.gguf` 19.549.939.264, o que esbarra no
follow-up 5.

`routable: false` nos três, e a regra passou a estar escrita: `SO_PARA_BAIXAR`
(`catalog.rs:831`) é a lista de séries sem quant escolhível, cada uma com o
motivo, e `every_size_has_a_routable_baseline` confere os dois lados — série nova
sem escolhível precisa entrar lá com o motivo, e uma série listada que passou a
ter escolhível tem de sair. O motivo do A3B é o número: 22 GB de pesos não cabem
inteiros numa placa, e o roteador não escolhe sozinho o que só roda dividido.
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

## O que esta frente NÃO mediu, e é exatamente por isso que está registrado

Nada aqui rodou. O que falta é placa, e placa é o portão que a instrução de
não abrir o app mantém fechado:

1. **Alojamento real.** Os `bytes` são o tamanho do arquivo, e o veredito de
   quantas camadas sobem sai de `alojamento::alojar` (`:626`) com a VRAM livre
   medida na hora. Nenhum dos GGUFs novos passou pela placa — e agora três
   podem passar: o `qwen3.5-35b-a3b-Q4_K_M` é o maior arquivo do catálogo que a
   placa de 16 GiB desta máquina nunca leva inteiro, então o veredito esperado
   dele é `Dividida`, e a frase que a tela diz com `camadas`, `na_placa` e
   `em_ram` nunca foi vista saindo do aplicativo. É #39 que fecha isso. O que #106
   e #107 fecharam foi a conta: `camadas_de_cache` (`:386`) e `taxa_kv` (`:445`)
   leem `head_count_kv`, `sliding_window_pattern`, `key_length_swa` e
   `shared_kv_layers` como o b12387 os lê, `memoria_recorrente` (`:325`) e
   `estado_recorrente` (`:370`) leem o gated delta net, e os 344.064 do Gemma 4
   como os 20.480 do A3B são aritmética conferida em teste, não medida em carga.
   O que continua **não medido** é o estado recorrente em carga: os 62,83 MiB do
   A3B e os 50,25 MiB do 4B saem da fórmula do `llama_memory_recurrent` aplicada
   ao cabeçalho, e o que os confirma é `engine_probe alojar:` com o peso em disco
   — a sonda imprime os dois (`bin/engine_probe.rs:249-253`) e o delta real de
   VRAM ao lado do previsto (`:309-313`). Um estado que a carga abrir menor que
   isto não é erro de conta, é a biblioteca tendo mudado a regra: é ela que a
   sonda tem de pegar.
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
5. **Os tipos IQ e MXFP4 não têm conta.** O censo sabe quais tipos um arquivo
   usa; o que falta é o `block_size`/`type_size` deles no leitor de cabeçalho do
   aplicativo, que é o do candle — `GgmlDType::from_u32` conhece os números 0…15
   e o 30, e cai no `bail!` para qualquer outro. Sem
   isso não há ficha, e sem ficha o catálogo não oferece — é por isso que as
   onze quants com IQ ou MXFP4 do A3B ficam na fila, e que entre elas estão os
   oito arquivos cujos pesos caberiam nos 16 GiB de uma placa (a menor quant que
   o leitor abre mede 18,82 GiB de pesos). Todos os onze são arquivo único e têm
   `lfs.oid` medido no `tree/main` de 2026-11-24. A tabela de blocos acima **já tem** os números conferidos de seis deles
   (`iq2_xxs` 66, `iq2_xs` 74, `iq2_s` 82, `iq3_xxs` 98, `iq3_s` 110, `iq4_xs`
   136), porque a soma de bytes dos três arquivos do A3B bateu com o tamanho
   publicado — o que falta é o parser, não a medida.

## Gates da frente do catálogo

Medidos antes da passagem #106; os números atuais estão nos gates das passagens
acima (#107, #108 e #109).

```
cd src-tauri && cargo test --lib        → 567 passaram, 0 falharam, 11 ignored, finished in 5,92 s
cd src-tauri && cargo test --lib catalog → 27 passaram, 0 falharam, finished in 4,70 s
cd src-tauri && cargo check --all-targets → Finished, zero avisos
npm run build                            → ✓ built in 250 ms, maior bloco react a 210,53 kB
```

O vermelho do instrumento novo foi medido por mutação, porque não há modelo
para rodar: com as duas linhas do Gemma 4 marcadas como não-escolhíveis,
`every_size_has_a_routable_baseline` caiu na lista explícita (`catalog.rs:843`)
e, retirada essa id da lista, caiu no laço que varre as séries do próprio
`CATALOG` (`catalog.rs:859`) — ou seja, série nova sem baseline é pega sem
ninguém precisar lembrar de escrever o nome dela. As duas mutações foram
revertidas; o diff daquela passagem (#106) fechou em 133 linhas adicionadas e 7
removidas.

## Follow-ups

1. **Leitor de cabeçalho que conheça os tipos IQ e MXFP4.** É o portão que a
   passagem #109 escreveu e a entrada do `Qwen3.5-35B-A3B` deixou visível: onze
   dos 23 arquivos de pesos do repositório têm tipo que o candle recusa, e **oito
   deles são justamente os que caberiam nos 16 GiB de uma placa única** — os
   pesos medem 9,93 / 10,61 / 11,32 / 12,18 / 12,65 / 14,22 / 15,23 e 15,46 GiB,
   e o menor arquivo que o leitor abre tem 18,82 GiB. Os onze são arquivo único,
   cada um com `lfs.oid` medido no `tree/main` de 2026-11-24 e o censo lido
   tensor a tensor; a menor é a `UD-IQ2_XXS`, 10.656.955.008 bytes. O que falta é conta, não dado: os tamanhos de bloco já estão
   conferidos acima (`iq2_xxs` 66, `iq2_xs` 74, `iq2_s` 82, `iq3_xxs` 98,
   `iq3_s` 110, `iq4_xs` 136 — cada um batendo com o tamanho publicado do
   arquivo onde ele aparece), e o `mxfp4` (tipo 39) é o único sem soma conferida.
   O caminho é um leitor de cabeçalho próprio: hoje o aplicativo tem **um** jeito
   de ler os dados de um GGUF — `alojamento::fichar` (`alojamento.rs:531`), que é
   o `gguf_file::Content::read` do candle — e até o codificador passa por ele
   antes de carregar os pesos (`ggml/mod.rs:603`). Uma tabela de blocos nossa, ao
   lado do censo que já está escrito no catálogo, é o que devolve ficha a esses
   onze arquivos, com o `diff 0` dos 52 como instrumento de conferência.
   Enquanto isso não existir, a frase honesta na tela é "este arquivo tem tipo
   que não sei medir".
2. **`mxfp4` sem soma conferida.** É o tipo 39 do `Qwen3.5-35B-A3B-MXFP4_MOE.gguf`
   (21.587.638.912 bytes publicados, censo `q5_k×41`, `q6_k×1`, `mxfp4×78`): o
   único arquivo lido cujo censo tem um tipo que a tabela acima não conferiu
   contra tamanho publicado. Entra na mesma fila do item 1 e não tem pressa — com
   21 GB ele não cabe inteiro em placa única de 16 GiB nem dividido com folga.
3. **`swa_full` desligado, se um dia valer a pena** — #106 mediu que a chave de
   janela deslizante não reduz nada com o padrão da biblioteca
   (`swa_full = true`), e há 40 das 48 camadas do Gemma 4 nesse caso. Ligar o
   contrário (`p.swa_full = false` em `ggml/ffi.rs:454-459`) encurta o cache, mas
   muda o que a atenção vê fora da janela — é escolha de qualidade, e o que a
   decide é medição com a placa, não conta de cabeçalho.
4. **Veredito de ferramenta por família** — medir, com a placa, quais das
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
5. **Downloader de pesos em partes (multi-shard) — adiado com medida, não com
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
