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
| `Qwen3.5-4B-Q4_K_M` | 32 (24 recorrentes, 8 de atenção) | 4 (escalar) | 256 | 131.072, como **teto** |

O `gemma4.attention.shared_kv_layers` do arquivo é `0` — as camadas que leem o
cache de outra ainda não aparecem nos pesos que entraram, e estão pinadas em
teste para o dia em que aparecerem. As 24 camadas recorrentes do `qwen35`
continham fora da conta: o estado delas existe e não é medido aqui, então o
número cobra-as como se fossem de atenção. Conservador por construção, como
estava registrado.

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

## O que esta frente NÃO mediu, e é exatamente por isso que está registrado

Nada aqui rodou. O que falta é placa, e placa é o portão que a instrução de
não abrir o app mantém fechado:

1. **Alojamento real.** Os `bytes` são o tamanho do arquivo, e o veredito de
   quantas camadas sobem sai de `alojamento::alojar` com a VRAM livre medida na
   hora. Nenhum dos GGUFs novos passou pela placa. O que a passagem #106 fechou
   foi a conta: `camadas_de_cache` (`alojamento.rs:287-324`) e `taxa_kv`
   (`:338-351`) agora leem `head_count_kv`, `sliding_window_pattern`,
   `key_length_swa` e `shared_kv_layers` como o b12387 os lê, e o 344.064 do
   Gemma 4 é aritmética conferida em teste, não medida em carga. O que continua
   **teto**, e por um motivo escrito no código: as 24 camadas recorrentes do
   `qwen35` pagam como se fossem de atenção, porque o estado de uma camada
   recorrente não é KV cache e não está conta nenhuma. Conferir os dois é
   `engine_probe alojar:` com o peso em disco — a sonda imprime a taxa por token
   (`engine_probe.rs:249`) e o previsto na placa ao lado do delta real de VRAM.
2. **A qualidade do texto e o custo por rodada** (tokens de entrada e saída,
   t/s) — medição de turno, placa, aplicativo aberto.
3. **Chamada de ferramenta no Gemma 4.** O laço do Aether não lê a gramática do
   template: ele define a sua, em `chat.rs:241-246` (abertura de chamada, nome
   de função e parâmetros em XML), e `inicio_de_chamada` (mesmo arquivo, `:254`)
   escreve essa gramática como continuação forçada — `session.rs:513` é o ponto
   que a injeta. O que o modelo viu no treino é a gramática do próprio template,
   e no Gemma 4 ela é diferente. Se a continuação forçada chega para governar o
   resto do bloco, é medição com a placa, não leitura de cabeçalho.

## Gates da frente do catálogo

Medidos antes da passagem #106; os números atuais estão no gates daquela
passagem, acima.

```
cd src-tauri && cargo test --lib        → 567 passaram, 0 falharam, 11 ignored, finished in 5,92 s
cd src-tauri && cargo test --lib catalog → 27 passaram, 0 falharam, finished in 4,70 s
cd src-tauri && cargo check --all-targets → Finished, zero avisos
npm run build                            → ✓ built in 250 ms, maior bloco react a 210,53 kB
```

O vermelho do instrumento novo foi medido por mutação, porque não há modelo
para rodar: com as duas linhas do Gemma 4 marcadas como não-escolhíveis,
`every_size_has_a_routable_baseline` caiu na lista explícita (`catalog.rs:664`)
e, retirada essa id da lista, caiu no laço novo que varre as séries do próprio
`CATALOG` (`catalog.rs:672`) — ou seja, série nova sem baseline é pega sem
ninguém precisar lembrar de escrever o nome dela. As duas mutações foram
revertidas; o diff do arquivo fecha em 133 linhas adicionadas e 7 removidas.

## Follow-ups

1. **Downloader de pesos em partes** (multi-shard) — sem ele, GLM-5.3-Flash e
   qualquer quant >48 GB ficam fora. É trabalho em `ai/download.rs`, com a
   conferência por sha256 de cada parte e a junção antes do `rename`.
2. **`swa_full` desligado, se um dia valer a pena** — #106 mediu que a chave de
   janela deslizante não reduz nada com o padrão da biblioteca
   (`swa_full = true`), e há 40 das 48 camadas do Gemma 4 nesse caso. Ligar o
   contrário (`p.swa_full = false` em `ggml/ffi.rs:454-459`) encurta o cache, mas
   muda o que a atenção vê fora da janela — é escolha de qualidade, e o que a
   decide é medição com a placa, não conta de cabeçalho.
3. **Veredito de ferramenta por família** — medir, com a placa, quais das
   famílias do catálogo conseguem entregar uma chamada no formato que o laço
   parseia, e dizer isso na tela em vez de deixar a pessoa descobrir no meio de
   um turno.
