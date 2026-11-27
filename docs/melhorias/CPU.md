# CPU — o caminho sem placa, medido (2026-11-26)

A frente é a #116: "melhorar o desempenho apenas em hardware CPU sem GPU". O que
havia antes era uma escada de dois degraus — a llama.cpp na placa e, se ela não
abrisse, o candle. O candle é uma implementação própria que só conhece uma
arquitetura (granite), então numa máquina sem placa as outras famílias do catálogo
não carregavam. O degrau do meio que falta é a mesma llama.cpp contando no
processador: mesmo motor, mesma abertura de arquitetura, mesmo corte de prefixo de
cache. Este arquivo documenta as medidas que escolheram os números desse degrau.

A máquina que mediu é um Ryzen 7 5700X3D — 8 núcleos / 16 fios lógicos, 32 GiB, e
**com uma RX 9060 XT dentro**. Isso é a primeira coisa que este documento tem de
dizer de si mesmo: não existe aqui uma máquina sem placa, então o caminho de CPU
foi medido por sonda, que monta o contexto no device CPU da mesma biblioteca e não
pergunta à placa. Ver o item "O que NÃO foi medido".

## 1. A escada de agora

| degrau | o que roda | quando |
| --- | --- | --- |
| GPU | llama.cpp em Vulkan (`ggml::Destino::Placa { camadas_gpu }`) | a placa existe e a carga aceita |
| CPU | a **mesma** llama.cpp contando no processador (`ggml::Destino::Sistema`), com a escala lida da máquina | existe a `llama.dll` e a placa recusou a carga — ou não existe placa |
| CPU (degrau) | implementação própria em candle (`src-tauri/src/ai/granite.rs`), só granite | quando nem a llama.cpp abre no processador |

Os três pontos no fonte:

- o pedido de placa e o `Err(falha_gpu)` que cai no degrau de baixo:
  `src-tauri/src/ai/engine.rs:417` (carga na placa) e `:464`-`:469` — a alternativa
  imediata é `ggml::Escala::da_maquina()` com `Destino::Sistema`, não o candle;
- o `Destino` e o que cada braço faz: `src-tauri/src/ai/ggml/mod.rs:93`-`:96`, com o
  braço de sistema em `:233`-`:244` — device CPU do mesmo registro,
  `camadas_gpu = 0`, e o erro `erros.motorSemCpu` se o device CPU não estiver lá;
- o candle continua sendo o último degrau: `src-tauri/src/ai/engine.rs:749`
  (`carregar_na_cpu`), que abre os pesos por `granite::Model::from_gguf`.

Por que o degrau do meio vem antes do candle, e não depois: o candle corta a
arquitetura (só granite) e não faz o reaproveitamento de prefixo de cache que o
laço de ferramentas usa. Trocar "sem placa" por "só Granite" seria trocar uma
limitação de hardware por uma limitação de código.

## 2. Por que a escala tem dois números de thread

O llama.cpp tem duas contagens e elas servem a passos diferentes:

- `n_threads` é o que a biblioteca usa no passo de **um** token — a decodificação;
- `n_threads_batch` é o que ela usa no passo de **muitos** tokens — o prefill.

Antes desta frente o `Escala` tinha um número só, então era impossível dizer as
duas coisas. A medição mostra que elas querem números opostos no mesmo
processador. Corrida de debug, lote 512, atenção desligada, o `cpu:` variando só a
thread (`scratch/cpu-sweep.log`):

| threads | prefill | decode |
| --- | --- | --- |
| 8 | 33678 ms (60,8 t/s) | 6297 ms (10,16 t/s) |
| 16 | 27201 ms (75,3 t/s) | 7610 ms (8,41 t/s) |

O prefill quer os 16 fios; a decodificação quer os 8 núcleos. Um número só pagaria
19% de decodificação para ganhar 24% de prefill, ou o contrário. O `Escala` passou
a carregar os dois (`src-tauri/src/ai/ggml/ffi.rs:70`-`:74`), cada um vai para o
campo seu (`ffi.rs:648`-`:661`), e a prova de que eles não se amassam é o teste
`a_escala_cpu_chega_ao_contexto` (`ffi.rs:1075`), que pede 6 e 12 e confere os dois
campos — com um `threads = 0` de controle, para o zero continuar significando
"deixa com a biblioteca".

## 3. As corridas

O instrumento é `engine-probe cpu:<combos> <arquivo de prompt>`, que para cada
combo carrega os pesos, monta o contexto no device CPU com aquela escala, pré-fila
o mesmo prompt e decodifica os mesmos 64 passos. Janela 4096, decode 64. O rótulo de
cada linha é o combo que correu, na forma `threads[,threads_lote],lote,fa`.

O prompt é fixo e é um arquivo deste repositório, para que a corrida seja repetível
num clone limpo — os 5200 primeiros bytes de `docs/melhorias/M8.md`:

```text
$ head -c 5200 docs/melhorias/M8.md > scratch/cpu-prompt.txt
$ sha256sum scratch/cpu-prompt.txt
4716d965a4c9eb9477007d98a6bf97d8667e77949d394ec63936cc778f0aa64c
```

Cinquenta e dois centos de markdown viram **2048 tokens** no `tokenizer.json` do
granite, que é o número impresso no cabeçalho de cada corrida. Os `.log` citados
abaixo ficaram em `scratch/`, que não entra no repositório: o que vale como evidência
aqui é a transcrição, linha por linha, na ordem em que a sonda imprimiu.

### 3.1 Primeira rodada — profile de debug, forma antiga de três campos

```text
$ cd "C:/Aether AI/src-tauri" && ./target/debug/engine-probe.exe \
    ~/AppData/Local/ai.aether.core/models/granite-4.2-3b \
    granite-4.2-3b-Q4_K_M.gguf 4096 64 \
    "cpu:0,512,0;4,512,0;8,512,0;16,512,0;8,256,0;8,1024,0;16,256,0;16,1024,0;8,512,1;16,1024,1;0,512,0" \
    "C:/Aether AI/scratch/cpu-prompt.txt"
```

Saída (`scratch/cpu-sweep.log`, as 11 linhas na ordem em que saíram):

| combo | carga | prefill | decode |
| --- | --- | --- | --- |
| `0,512,0` | 1.81s | 45375ms (45.1 t/s) | 6267ms (10.21 t/s) |
| `4,512,0` | 1.67s | 45910ms (44.6 t/s) | 6458ms (9.91 t/s) |
| `8,512,0` | 1.88s | 33678ms (60.8 t/s) | 6297ms (10.16 t/s) |
| `16,512,0` | 1.69s | 27201ms (75.3 t/s) | 7610ms (8.41 t/s) |
| `8,256,0` | 1.69s | 30240ms (67.7 t/s) | 7189ms (8.90 t/s) |
| `8,1024,0` | 1.91s | 35000ms (58.5 t/s) | 6186ms (10.35 t/s) |
| `16,256,0` | 1.65s | 43153ms (47.5 t/s) | 8028ms (7.97 t/s) |
| `16,1024,0` | 1.69s | 30785ms (66.5 t/s) | 8774ms (7.29 t/s) |
| `8,512,1` | 1.68s | 27982ms (73.2 t/s) | 8170ms (7.83 t/s) |
| `16,1024,1` | 1.63s | 20793ms (98.5 t/s) | 7766ms (8.24 t/s) |
| `0,512,0` (repetido) | 1.74s | 40841ms (50.1 t/s) | 6211ms (10.31 t/s) |

O combo repetido no fim não foi acaso: ele é a medição do ruído da própria rodada.
Um mesmo `0,512,0` deu 45375ms e 40841ms — 11% de espalhamento dentro de uma
única corrida, com a decodificação parada em 10,21 e 10,31 t/s. O prefill é
instável; a decodificação não.

### 3.2 Segunda rodada — já com a escala dividida

```text
$ cd "C:/Aether AI/src-tauri" && ./target/debug/engine-probe.exe \
    ~/AppData/Local/ai.aether.core/models/granite-4.2-3b \
    granite-4.2-3b-Q4_K_M.gguf 4096 64 \
    "cpu:8,512,0;8,16,512,0;8,16,1024,0;8,16,1024,1;8,1024,1" \
    "C:/Aether AI/scratch/cpu-prompt.txt"
```

Saída (`scratch/cpu-sweep2.log`):

| combo | carga | prefill | decode |
| --- | --- | --- | --- |
| `8,512,0` | 2.09s | 25182ms (81.3 t/s) | 5862ms (10.92 t/s) |
| `8,16,512,0` | 1.69s | 22583ms (90.7 t/s) | 5811ms (11.01 t/s) |
| `8,16,1024,0` | 1.64s | 27679ms (74.0 t/s) | 5820ms (11.00 t/s) |
| `8,16,1024,1` | 1.65s | 19254ms (106.4 t/s) | 6542ms (9.78 t/s) |
| `8,1024,1` | 1.65s | 22853ms (89.6 t/s) | 6710ms (9.54 t/s) |

A escala dividida aparece: `8,512,0` (oitava linha da primeira rodada: 33678ms)
contra `8,16,512,0` (22583ms) é o mesmo decode com o lote em 16 fios. As duas
medidas, porém, são de rodadas diferentes, e a primeira linha desta tabela —
`8,512,0`, o mesmo combo que na rodada anterior deu 33678ms — deu 25182ms aqui.
**Um número de uma rodada não é comparável com o da outra**: 34% de diferença para
o mesmo combo, o mesmo binário e o mesmo perfil. Foi isso que mandou refazer a
escolha em release, entrelaçada, três vezes.

### 3.3 Terceira rodada — release, quatro combos × três repetições entrelaçadas

```text
$ cd "C:/Aether AI/src-tauri" && ./target/release/engine-probe.exe \
    ~/AppData/Local/ai.aether.core/models/granite-4.2-3b \
    granite-4.2-3b-Q4_K_M.gguf 4096 64 \
    "cpu:8,16,512,0;8,16,512,1;8,16,1024,0;8,16,1024,1;8,16,512,0;8,16,512,1;8,16,1024,0;8,16,1024,1;8,16,512,0;8,16,512,1;8,16,1024,0;8,16,1024,1" \
    "C:/Aether AI/scratch/cpu-prompt.txt"
```

Saída (`scratch/cpu-rel-sweep.log`, 12 linhas, três voltas dos mesmos quatro
combos — entrelaçadas de propósito para que a deriva de temperatura ou de cache
atrelesse os quatro braços, e não um só):

| # | combo | carga | prefill | decode |
| --- | --- | --- | --- | --- |
| 1 | `8,16,512,0` | 2.12s | 22791ms (89.9 t/s) | 5673ms (11.28 t/s) |
| 2 | `8,16,512,1` | 1.62s | 22408ms (91.4 t/s) | 6548ms (9.77 t/s) |
| 3 | `8,16,1024,0` | 1.63s | 24150ms (84.8 t/s) | 5560ms (11.51 t/s) |
| 4 | `8,16,1024,1` | 1.61s | 18900ms (108.4 t/s) | 6312ms (10.14 t/s) |
| 5 | `8,16,512,0` | 1.60s | 23615ms (86.7 t/s) | 5517ms (11.60 t/s) |
| 6 | `8,16,512,1` | 1.64s | 19110ms (107.2 t/s) | 6668ms (9.60 t/s) |
| 7 | `8,16,1024,0` | 1.61s | 24598ms (83.3 t/s) | 5661ms (11.30 t/s) |
| 8 | `8,16,1024,1` | 2.09s | 20786ms (98.5 t/s) | 6770ms (9.45 t/s) |
| 9 | `8,16,512,0` | 1.76s | 30193ms (67.8 t/s) | 5660ms (11.31 t/s) |
| 10 | `8,16,512,1` | 1.72s | 19462ms (105.2 t/s) | 6409ms (9.99 t/s) |
| 11 | `8,16,1024,0` | 1.68s | 23665ms (86.5 t/s) | 5508ms (11.62 t/s) |
| 12 | `8,16,1024,1` | 1.63s | 18092ms (113.2 t/s) | 6197ms (10.33 t/s) |

## 4. As medianas e o que elas decidem

Mediana das três repetições de cada combo (o número entre parênteses é o que
resulta da mediana dos milissegundos, não a mediana dos t/s publicados):

| combo | carga | prefill | decode | rodada completa (prefill + 64 tokens) |
| --- | --- | --- | --- | --- |
| `8,16,512,0` | 1.76s | 23615ms (86,7 t/s) | 5660ms (11,31 t/s) | 29,3s |
| `8,16,512,1` | 1.64s | 19462ms (105,2 t/s) | 6548ms (9,77 t/s) | 26,0s |
| `8,16,1024,0` | 1.63s | 24150ms (84,8 t/s) | 5560ms (11,51 t/s) | 29,7s |
| **`8,16,1024,1`** | **1.63s** | **18900ms (108,4 t/s)** | **6312ms (10,14 t/s)** | **25,2s** |

**A atenção rápida é a decisão que os dados sustentam sozinha.** Não há
sobreposição entre os conjuntos: com lote 512, as três medidas com `fa=1` foram
22408, 19110 e 19462 ms, e a melhor com `fa=0` foi 22791 ms; com lote 1024, as três
com `fa=1` foram 18900, 20786 e 18092 ms, e a melhor com `fa=0` foi 23665 ms. O
prefill ganha em seis de seis comparações por par. A decodificação perde, também
sem sobreposição, e na casa de 1,2 a 1,5 t/s.

Onde a perda de decodificação é paga com juros é no total da rodada: 2048 tokens
entram, 64 saem. Num laço de ferramentas isso é a forma normal do problema — o
`bancada.rs:8` cita os 29.777 tokens de prompt recalculados por rodada antes do
cache de prefixo, que é o motivo de o cache existir. Na CPU, o pré-fil é o
gargalo, e é ele que a atenção rápida resolve: 29,7s para 25,2s de rodada, 15%
mais rápido, com 12% a menos de tokens por segundo numa cauda dez vezes mais
curta que a entrada. Por isso `ATENCAO_RAPIDA_LIGADA = true`
(`src-tauri/src/ai/ggml/ffi.rs:228`).

**O lote, ao contrário, não separa.** 512 e 1024 se sobrepõem nos quatro pares
comparáveis — `fa=0` prefill: {22791, 23615, 30193} contra {23665, 24150, 24598};
`fa=1` prefill: {19110, 19462, 22408} contra {18092, 18900, 20786}; as duas
decodificações também. A mediana favorece 1024 em três dos quatro comparáveis, por
2 a 3%, que é menos que o espalhamento da própria tabela. Ficou
`LOTE_CPU = 1024` (`ffi.rs:222`) porque é o lote com a menor mediana de rodada
completa e porque, entre os dois, nenhum perdeu em algo que o outro ganhou por
margem. É a escolha mais fraca deste arquivo, e está escrita como tal: se outra
máquina discordar, o número que muda é este.

O que **não** se escolheu por palpite: `threads` e `threads_lote` não são
constantes, são lidos da máquina (§5). Uma placa de 96 threads não vai herdar 8.

E há como conferir sem carregar modelo nenhum: a sonda imprime a escala que o
aplicativo monta, antes de qualquer combo. Uma lista vazia corre só essa linha:

```text
$ cd "C:/Aether AI/src-tauri" && ./target/release/engine-probe.exe \
    ~/AppData/Local/ai.aether.core/models/granite-4.2-3b \
    granite-4.2-3b-Q4_K_M.gguf 4096 64 "cpu:" "C:/Aether AI/scratch/cpu-prompt.txt"
cpu: arquivo=granite-4.2-3b-Q4_K_M.gguf janela=4096 prompt=2048 tokens | decode=64 passos | threads lógicas=16 | escala da máquina=Escala { threads: 8, threads_lote: 16, lote: 1024, atencao_rapida: true } | combo=threads[,threads_lote],lote,fa | fixo para todos os combos
```

Os quatro números da linha são os quatro do combo que a tabela do §4 escolheu
(`8,16,1024,1`). O `Escala::da_maquina()` (`ffi.rs:206`) é o que o `Destino::Sistema`
recebe, então a linha acima é a ponte entre a varredura e o que roda.

## 5. Contar núcleos: a chamada que não respondia, o passo e a conta

`available_parallelism()` devolve 16 nesta máquina. O que a decodificação quer é 8.
Dividir o total por dois acerta aqui e erra em qualquer CPU sem SMT, em qualquer
máquina com núcleos heterogêneos e num convidado com topologia estranha — e erra
sem dizer. A contagem real vem de `GetLogicalProcessorInformationEx` com
`RelationProcessorCore` (`ffi.rs:87`).

A leitura antiga tinha dois defeitos, ambos silenciosos — e o segundo só estava
escondido pelo primeiro.

**A) A chamada nunca respondia, e ninguém percebia.** `ReturnLength` é um parâmetro
de entrada **e** de saída: entra com o tamanho do buffer que a API pode escrever e
sai com a quantidade que ela escreveu. O código o passava zerado na segunda chamada,
então a API tinha razão em recusar. A corrida viva, impressa por um teste de
diagnóstico temporário (já removido do arquivo) e depois repetida com o comprimento
certo:

```text
$ cd "C:/Aether AI/src-tauri" && cargo test --lib zzz_debug_nucleos -- --nocapture
chamada 1: Err(HRESULT(0x8007007A)) necessario=384    # esperada: ela devolve o tamanho
# com `let mut obtido: u32 = 0;`
chamada 2: Err(HRESULT(0x8007007A)) obtido=384 reserva=384
registro em 0: tipo=0 tamanho=0 grupos=0 mascara=0000000000000000 fios=0
logicos=16 caminhar=None nucleos_fisicos=None
# com `let mut obtido: u32 = necessario;`
chamada 2: Ok(()) obtido=384 reserva=384 size_of=80 d_tipo=0 d_tamanho=4 d_anonimo=8 d_grupos=30 d_mascara=32
registro em 0:   tipo=0 tamanho=48 grupos=1 mascara=0000000000000011 fios=2
registro em 48:  tipo=0 tamanho=48 grupos=1 mascara=0000000000001100 fios=2
registro em 96:  tipo=0 tamanho=48 grupos=1 mascara=0000000000110000 fios=2
registro em 144: tipo=0 tamanho=48 grupos=1 mascara=0000000011000000 fios=2
registro em 192: tipo=0 tamanho=48 grupos=1 mascara=0000001100000000 fios=2
registro em 240: tipo=0 tamanho=48 grupos=1 mascara=0000110000000000 fios=2
registro em 288: tipo=0 tamanho=48 grupos=1 mascara=0011000000000000 fios=2
registro em 336: tipo=0 tamanho=48 grupos=1 mascara=1100000000000000 fios=2
logicos=16 caminhar=Some((8, 16)) nucleos_fisicos=Some(8)
```

O valor de `nucleos_fisicos()` antes disto era `None` **em qualquer máquina**, e o
degrau de `None` é o total de fios. Ou seja: quando o aplicativo caísse na CPU, ele
montava `threads = 16` com `threads_lote = 16` — exatamente o número que a varredura
mostra ser o pior para a decodificação. Nas três tabelas do §3, as linhas com 16
threads de codificação dão **7,29 / 7,97 / 8,24 / 8,41 t/s**; as linhas com 8 núcleos
e a atenção desligada dão **8,90 a 11,62 t/s**, sem sobreposição entre os dois
conjuntos, em debug e em release. Com a atenção ligada — a forma como o degrau sai
agora — o par do mesmo perfil é `16,1024,1` a 8,24 t/s contra `8,16,1024,1` a 9,78
(rodada de debug) e 10,14 (mediana de release).

**B) O passo.** A leitura avançava de
`size_of::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX>()` em `size_of::<...>()`. Nesta
máquina o struct tem **80 bytes** — o maior membro da união é `GROUP_RELATIONSHIP`,
não `PROCESSOR_RELATIONSHIP` — e cada registro anunciado tem **48**. Com 384 bytes e
passo de 80 o laço visitaria quatro posições, a segunda no meio do primeiro
registro: os 48 reais nunca apareceriam na conta. Não é caso hipotético de máquina
grande, é esta máquina. E `PROCESSOR_RELATIONSHIP.GroupMask` é um `ANYSIZE_ARRAY`
declarado com uma casa (`GroupCount: u16` ao lado, e no crate windows 0.62
`GroupMask: [GROUP_AFFINITY; 1]`), então numa máquina com mais de 64 processadores
lógicos — dois grupos de afinidade — o registro vem maior ainda, com um
`GROUP_AFFINITY` por grupo.

**C) A conta.** Ela somava um núcleo por item encontrado, sem ler as máscaras. O
número estaria certo numa máquina comum e errado em qualquer outra, e não havia
nada na função que conferisse.

A função agora devolve a sequência crua para uma `caminhar(&[u8], logicos)`
(`ffi.rs:149`) que:

- anda pelo `Size` que **cada registro anuncia** — os 48 medidos acima — e não pelo
  tamanho do struct;
- soma `count_ones()` da máscara de afinidade de cada núcleo — o número de núcleos
  sai dos registros, o de fios sai das máscaras;
- recusa `GroupCount != 1` (a struct tem espaço para a primeira máscara, e contar
  meio núcleo é pior que não contar), `Size` menor que o cabeçalho (o laço não
  andaria) e sequência que não fecha no `obtido` que a API devolveu;
- confere a soma dos fios com `available_parallelism()` e devolve `None` se não
  bater — que faz quem chama cair no degrau de antes (o total de fios lógicos), o
  estado em que a biblioteca já estava, nunca um zero.

Os deslocamentos vêm de `offset_of!` sobre as próprias structs do crate — os
mesmos números que a corrida acima imprimiu (`d_tipo=0`, `d_tamanho=4`,
`d_anonimo=8`, `d_grupos=30`, `d_mascara=32`) —, então o caminhar não tem nenhum
número de layout escrito à mão.

Os dois testes novos:

- `a_caminhada_le_a_sequencia_pelo_tamanho_de_cada_registro` (`ffi.rs:1118`) monta
  a sequência em bytes: dois núcleos de dois fios (`0b0011`, `0b1100`) contam
  `(2, 4)`; os mesmos dois núcleos anunciados com `Size = 64` — o caso da máquina de
  dois grupos — contam `(2, 4)` também, o que a leitura por `size_of` não fazia; um
  registro `RelationCache` no meio, cheio de `0xff` onde um núcleo teria máscaras,
  é filtrado pelo tipo e não vira fio de ninguém; e os quatro casos de recusa
  (truncado, grupo duplo, `Size` menor que o cabeçalho, soma de fios diferente da
  máquina) devolvem `None`.
- `a_contagem_real_fecha_com_a_sequencia_da_api` (`ffi.rs:1198`) roda o caminho
  verdadeiro contra a API desta máquina e confere o invariante: a contagem divide
  o total de fios, e a escala publicada obedece a ela. **Este teste é o que achou o
  defeito (A):** ele nasceu vermelho — `a API de núcleos não respondeu nesta máquina
  Windows` — porque a conta em si estava certa e quem faltava era a chamada. Passou
  a verde com o comprimento certo no parâmetro de entrada e saída, e ficou na suíte
  justamente para a recusa silenciosa não voltar.

## 6. A llama.cpp de CPU contra o candle, na mesma máquina

O outro lado da frente é a pergunta "valeu a pena o degrau do meio?". A resposta é
uma medição direta: o mesmo GGUF, o mesmo prompt de 2048 tokens, os mesmos 64
passos, no mesmo processador, pelos dois caminhos. O candle é medido no profile
**release** — o ggml não precisa, porque os braços dele são a DLL vendorizada,
compilada fora deste projeto, e o que muda entre `debug` e `release` aqui é o
código em volta dela.

```text
$ cd "C:/Aether AI/src-tauri" && for i in 1 2 3; do ./target/release/engine-probe.exe \
    ~/AppData/Local/ai.aether.core/models/granite-4.2-3b \
    granite-4.2-3b-Q4_K_M.gguf 4096 64 \
    "prefill:C:/Aether AI/scratch/cpu-prompt.txt"; done
```

Saída (`scratch/cpu-candle.log` — o total de decodificação abaixo é a soma dos 64
`passo N (Xms)` que a sonda imprime, e os três passos de 64 rodaram inteiros, sem
parar em token especial):

| rep | prefill | decode 64 passos | rodada |
| --- | --- | --- | --- |
| 1 | 61362ms (33,4 t/s) | 14530ms (4,40 t/s) | 75,9s |
| 2 | 60264ms (34,0 t/s) | 14719ms (4,35 t/s) | 75,0s |
| 3 | 60811ms (33,7 t/s) | 14417ms (4,44 t/s) | 75,2s |
| **mediana** | **60811ms (33,7 t/s)** | **14530ms (4,40 t/s)** | **75,3s** |

Contra o melhor combo da tabela do §4 (`8,16,1024,1`: prefill 18900ms, decode
6312ms, rodada 25,2s) e contra o braço mais conservador (`8,16,512,0`: 23615 +
5660 = 29,3s):

| | candle | llama.cpp de CPU | diferença |
| --- | --- | --- | --- |
| prefill | 60811ms (33,7 t/s) | 18900ms (108,4 t/s) | **3,2× mais rápido** |
| decodificação | 4,40 t/s | 10,14 t/s | **2,3× mais rápido** |
| rodada (2048 + 64) | 75,3s | 25,2s | **3,0× mais rápido** |

A comparação não depende de escolher o melhor número da tabela: a pior linha das
doze — `8,16,512,0` da terceira volta, 30193 + 5660 = 35,9s — ainda é 2,1× mais
rápida que a mediana do candle. O caminho de CPU que o aplicativo vai usar quando
não houver placa é, nesta máquina, três vezes o caminho que ele usava.

Uma honestidade sobre o espalhamento: o candle repetiu apertado (60264 a 61362 ms,
1,8%), e a llama.cpp de CPU não (22791 a 30193 ms no mesmo combo, 32%). As duas
coisas são verdadeiras ao mesmo tempo, e a diferença de rodada entre os dois
caminhos é de 39 a 50 segundos — longe demais de qualquer um dos dois
espalhamentos para que a conclusão dependa de mediana.

O passo de número um merece uma linha. Na primeira tentativa desta corrida — o
processo foi interrompido no meio do rep 1 — o `passo 1` saiu em 3519ms contra os
220-237ms das três voltas publicadas. Não sei dizer o que a máquina estava fazendo
com a página daquele processo frio, então o que fica publicado é o processo único
por volta com o arquivo já em cache, do jeito que a corrida saiu.


## 7. Verde medido

As três portas correram por último sobre a árvore inteira deste commit — depois
de cada `.rs` tocado por esta frente e de um comentário corrigido em
`local/web.rs` —, e nenhum arquivo da árvore está mais novo que a suíte:

- `cargo test --lib` → **`test result: ok. 685 passed; 0 failed; 12 ignored; 0 measured; 0 filtered out; finished in 7.36s`**.
  Os 685 incluem os dois testes que esta frente escreveu, vistos por nome na saída:
  ```
  test ai::ggml::ffi::testes::a_caminhada_le_a_sequencia_pelo_tamanho_de_cada_registro ... ok
  test ai::ggml::ffi::testes::a_contagem_real_fecha_com_a_sequencia_da_api ... ok
  ```
  A corrida anterior dos mesmos dois era **`684 passed; 1 failed; 12 ignored`**, e a
  falha era o segundo — o que provou o defeito (A) do §5 nasceu vermelho.
- `cargo check --all-targets` → `Finished \`dev\` profile [unoptimized + debuginfo] target(s) in 3.59s`,
  com **zero** linhas `warning` no log — o padrão da casa. O tempo é curto porque
  só o lib mudou desde a re-leitura completa de 29.50 s da mesma árvore.
- `npm run build` → `tsc && vite build`, `✓ 307 modules transformed`,
  `✓ built in 171ms`, sem aviso de TypeScript.

## 8. O que NÃO foi medido

- **Uma máquina sem placa.** Esta tem uma RX 9060 XT. O degrau de CPU correu porque
  a sonda pede `Destino::Sistema`, não porque o `Engine::load` o escolheu: a
  cascada real (`engine.rs:456` → `:513`) não foi exercitada de ponta a ponta, e o
  caminho "a placa recusou, a CPU serviu" continua sem medição de verdade. O §9
  escreve a conta que este degrau ainda não tinha, e escreve sem carga de verdade:
  os pesos estão em disco, o aplicativo é que não foi aberto nesta frente.
- **Nenhuma janela aberta.** Por ordem do mantenedor, o aplicativo não foi executado
  nesta frente. Não existe captura de tela do caminho de CPU, nem verificação no app
  real do `Acelerador` que ele publica.
- **A máquina de dois grupos.** O registro menor que o struct é **medido aqui** — os
  48 anunciados contra os 80 de `size_of` estão na transcrição do §5, nesta máquina.
  O que continua CLAIMED é o caso de cima: uma CPU com mais de 64 processadores
  lógicos vem com um `GROUP_AFFINITY` por grupo, e não há aqui tal máquina. O que foi
  observado é o comportamento do `caminhar` contra bytes sintéticos que imitam esse
  registro maior (§5) — e a recusa de `GroupCount != 1`, que é o degrau conservador.
- **Outros modelos e outras quants.** As tabelas são granite 4.2 3B Q4_K_M, janela
  4096, prompt de 2048 tokens. `Escala::da_maquina()` não sabe que modelo vai
  carregar: um 9B, uma janela muito maior ou uma quant mais apertada podem inverter
  o lote, que já é a escolha mais fraca deste arquivo.
- **A kernel de atenção rápida em outras arquiteturas.** O ganho medido é do
  granite. Se uma arquitetura não tem kernel de FA em CPU, o `fa=1` vira o degrau
  de antes — não uma regressão medida, mas também não uma vitória medida.
- **Concorrência.** Cada linha das tabelas carregou o modelo e andou sozinha. Numa
  máquina sem placa atendendo duas conversas, a escala é a mesma e o gargalo não.

## 9. A RAM manda no degrau sem placa (2026-11-26, passada seguinte)

O §1 descreve a escada e o §2 a escala, mas até aqui o degrau de CPU carregava com
**qualquer janela**: `maior_janela_inteira` começava com `let cap = capacidade?`, e
sem placa não há capacidade — então `Acelerador.janela_inteira` saía `None` e a
escala de geração oferecia o máximo do arquivo. Numa máquina com placa isso é só um
teto faltando; sem placa é o buraco: pesos, KV e estado pousam todos na RAM, e uma
janela maior que a memória livre não é "um pouco mais lento" — é troca com disco a
cada token, que é o consumo de RAM variável que se reclamou, ou recusa do contexto.

O que entrou, com as linhas desta árvore:

- **Um só leitor de RAM.** `alojamento::medir_memoria_do_sistema`
  ([../src-tauri/src/ai/alojamento.rs:741](../src-tauri/src/ai/alojamento.rs)) é a
  única chamada a `GlobalMemoryStatusEx`; `memoria_do_sistema` (`:763`) é a mesma
  leitura como opção, e `router::measure_hardware` passou a ler daqui em vez de
  abrir a chamada por conta própria — duas leituras do mesmo sistema operacional
  podem discordar, e a frase que explica a escolha tem de citar o número que a
  conta dela usou.
- **A conta do sistema.** `caber_no_sistema` (`:788`) soma pesos + KV + estado e
  compara com a RAM livre, **sem** a folga de computação do §4 — aqueles 518 MiB de
  piso são o `sched_reserve` do grafo Vulkan medido nesta placa, e o kernel de CPU
  não tem nada medido parecido. Chutar um piso para o lado sem placa seria chamar
  de medida o que é transferência de outra máquina.
- **Uma varredura só.** `maior_potencia` (`:707`) é o laço que desce de `teto` pela
  metade; `maior_janela_inteira` e `maior_janela_no_sistema` (`:804`) são agora o
  mesmo passo com réguas diferentes, porque a monotonia é a mesma — o KV cresce com
  a janela.
- **A carga obedece.** No braço `Err(falha_gpu)` ([../src-tauri/src/ai/engine.rs:513](../src-tauri/src/ai/engine.rs))
  a janela pedida é conferida contra a RAM antes de `Motor::novo`: se não cabe e
  existe teto, a carga desce para `teto.min(context)` (`:546`) — nunca sobe — e a
  última linha do `motivo` diz quantos tokens foram pedidos, quanto custaria, quanta
  RAM havia e em quantos ficou (`erros.janelaCurtaSistema`, `:550`).
  `Acelerador.janela_inteira` sai medida com a mesma RAM que mandou na carga.
- **O alojamento do degrau sem placa.** `Alojado.cedido` passou a
  [`Option<u64>`](../src-tauri/src/ai/engine.rs) (`:340`) e o braço de CPU publica
  `alojado` (`:564`): o veredito é o `SoSistema` da conta com a **janela que foi
  carregada**, não a pedida, então o chip do gerenciador de modelos diz quantos
  bytes de pesos e de cache a carga ocupa no sistema — que é a informação que
  estava faltando — e o `cedido` vai `None`, porque nenhum byte de VRAM mudou de
  dono. O mesmo `None` cobre a placa que carregou sem que a sonda de depois
  respondesse (`:482`, `:489`): ali o veredito continua sendo conta feita com a
  leitura de antes, e só o delta é que não existe. Na tela,
  [`ModelManager.tsx:33`](../src/components/ModelManager.tsx) deixa o título do chip
  vazio em vez de escrever "0 MiB levados", e
  [`bancada.rs:242`](../src-tauri/src/bancada.rs) só empurra a medida
  `bancada.medida.cedido` quando ela foi medida.
- **A tela diz qual memória é.** `geracao.janelaInteira` ficou para a placa e
  entrou `geracao.janelaInteiraSistema` ("inteira na RAM até {n}") para o degrau
  sem placa; `inteiraMedida` leva o `tipo` do acelerador junto porque a mesma
  grandeza tem duas moedas.

A corrente fecha onde a compactação automática começa: `state.rs:737` grava a
janela que a biblioteca aceitou, e `session.rs:557` tira de `engine.context()` o
`limite` e o `limite_macio` que disparam a dobra. Reduzir a janela na carga, quando
a RAM manda, é o que faz o gatilho automático da compactação existir numa máquina
sem placa — antes ele disparava contra um teto que a RAM nunca deixou existir.

### 9.1 Medido nesta máquina

- A leitura, por `a_ram_do_sistema_responde_e_o_livre_nao_passa_do_total` rodado com
  `--nocapture`: **`RAM lida nesta máquina: 15.2 GiB livres de 31.9 GiB`**.
- O instrumento estranho concorda no total: `Get-CimInstance Win32_OperatingSystem`
  devolveu, minutos antes, `FreePhysicalMemory 17216072` e
  `TotalVisibleMemorySize 33471868` — 16,4 GiB livres de **31,92 GiB**, o mesmo
  total dígito por dígito; o livre difere porque são duas leituras em dois
  momentos, não duas contas.
- O teto do granite 4.2 3B Q4_K_M, calculado com a ficha do §5 e a RAM lida acima:
  `2136 MiB + 81920 B/tokens × 131072 = 12.376 MiB` (12,09 GiB) contra 15,2 GiB
  livres — cabe. Nesta máquina o teto é o `janela_maxima` que o arquivo declara, não
  a RAM, então **a redução não dispara aqui em janela nenhuma**. Ela só morde em
  máquina com menos memória, que é exatamente onde o buraco do §9 estava.
- Portas desta passada: `cargo test --lib` →
  **`test result: ok. 790 passed; 0 failed; 12 ignored; 0 measured; 0 filtered out; finished in 6.57s`**
  (sete testes novos: seis da conta de RAM em `alojamento::testes` e o do leitor
  acima), com zero linhas `warning` no `cargo check --all-targets`.
- Portas da passada seguinte, a do `cedido` opcional: `cargo test --lib` →
  **`test result: ok. 791 passed; 0 failed; 12 ignored; 0 measured; 0 filtered out; finished in 7.00s`**
  — o um a mais é `o_degrau_sem_placa_publica_o_sistema_sem_cedido_falso`, que vê o
  `SoSistema` serializado com `"cedido":null` e recusa `"cedido":0`.
  `cargo check --all-targets` → `Finished \`dev\` profile [unoptimized + debuginfo] target(s) in 7.08s`,
  zero linhas `warning`. `npm run build` → `tsc && vite build`, `✓ built in 207ms`,
  sem aviso de TypeScript.
- A consequence de publicar o veredito no degrau de CPU: as frases que já existiam
  e nunca haviam sido alcançadas por uma carga — `modelos.placaSistema` no chip do
  gerenciador e `bancada.previsao.sistema` ("só no sistema") na bancada — passam a
  ser o que a tela tem de mostrar quando não há placa. Estão nas duas locales e o
  `previsao()` já as formatava; o que faltava era o núcleo publicar o veredito.

### 9.2 O que continua não medido

- **Nenhuma carga real passou pela conta.** Os pesos estão em disco —
  `AppData\Local\ai.aether.core\models` tem `granite-4.2-3b-Q4_K_M.gguf` de
  2.244.011.552 B, `Ornith-1.5-9B-Q4_K_M.gguf` de 5.780.090.816 B e o codificador
  Qwen3 — o que não houve foi carga: o aplicativo não foi aberto nesta frente.
  Então o `motivo` de janela reduzida nunca foi impresso por uma carga de verdade,
  nem o RSS depois de um `Destino::Sistema` foi comparado com o `total` previsto.
  A aritmética está testada; a predição contra memória real de processo, não.
- **Nenhuma janela aberta.** O chip `SoSistema` do gerenciador de modelos e a linha
  da bancada existem desde esta passada só no papel: sem carga, sem aplicativo
  rodando, não há captura que mostre o `cedido` ausente no lugar certo. O que está
  provado aqui é a serialização (`"cedido":null`) e o tipo que a tela lê, não o
  pixel.
- **A placa que carregou sem responder depois.** Os dois ramos novos do lado de GPU
  (`:482`, `:489`) publicam o veredito com `cedido: None` quando a sonda de depois
  falhou. Nesta máquina a sonda responde, então esse caminho continua sendo conta
  escrita contra um caso que ainda não aconteceu — e é por isso que ele leva o
  motivo da sonda no `motivo` junto, para a tela nunca parecer que mediu.
- **O teto em outros modelos.** O número de 9.1 é do granite com a ficha do §5. Um
  9B Q4_K_M, um híbrido com estado recorrente (as camadas de atenção linear que um
  gemma-4 paga) ou uma quant maior mudam a conta — e em alguns casos mudam o lado
  que manda: o
  arquivo ou a RAM.

