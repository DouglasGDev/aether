# VISÃO — o que falta para o agente ver uma imagem

Task #119. Frente de pesquisa: a pessoa pediu "um harness pra deixar ele ter
visão e pesquisar na internet". Este doc **não** implementa nada: ele mede onde
está o bloqueio, porque a resposta curta mudou de "não dá" para "falta eu fazer
cinco coisas".

Nada aqui é estimado. Cada número vem de um arquivo lido ou de uma pedida de
rede cuja resposta foi conferida. O que não foi medido está dito como não
medido, na última seção.

## A resposta, antes da medição

**Não é bloqueio de upstream.** A revisão do llama.cpp que o Aether pinou —
`b12387`, a mesma dos cabeçalhos em `vendor/llama-cpp/include/` — já publica a
biblioteca multimodal (`mtmd.dll`, 2.245.120 bytes) no **mesmo pacote** de onde
saíram as 19 DLLs que eu já vendorizo, e ela não depende de nada que eu ainda
não tenha. O que não existe hoje é decisão minha: eu copiei 20 das 53 entradas
do pacote, e `mtmd.dll` está entre as 33 que ficaram de fora.

## 1. O FFI não tem o que bindar — medido nos binários

Pré-requisito para reproduzir: as DLLs não vêm no clone (`src-tauri/vendor/README.md`);
rode o instalador da tela *Runtime nativo*, ou copie o conteúdo de `lib/` do
`llama-b12387-bin-win-vulkan-x64.zip`.

Censo das exportações das 19 DLLs + `libomp.dll` em
`src-tauri/vendor/llama-cpp/lib/`, lidas pela tabela de exportações do próprio
PE (sem `dumpbin`, sem dependência de toolchain):

```
2.658 símbolos exportados no conjunto todo
  llama.dll      264   (238 começam com "llama_")
  ggml.dll        16
  ggml-vulkan.dll  9
  ggml-base.dll  689
  libomp.dll     756
  cada ggml-cpu-* 66
mtmd: 0 | "vision": 0 | "clip": 0
```

Zero, nos três nomes. O parser foi validado antes de eu publicar o zero: as
264 exportações de `llama.dll` incluem `llama_model_load_from_file` e
`llama_batch_ext_add_embd`, que são nomes que eu sei estarem lá — uma tabela lida
errada não produziria esses dois. E no lado Rust: `grep -rn "mtmd|visao|imagem|clip|mmproj"
src-tauri/src/ai/` não devolve binding nenhum (o único acerto é um teste de
`adaptadores.rs:614` sobre o nome de um tensor).

**Fato:** não há caminho de visão hoje. Não é "está desligado" — a biblioteca
que faz o trabalho não está na pasta, e nada no crate a nomeia.

## 2. O pacote pinado tem a biblioteca — lido do índice central do zip

Comando (não baixa os 38 MB: lê o EOCD e o índice central por *range request*):

```sh
curl -s https://api.github.com/repos/ggml-org/llama.cpp/releases/tags/b12387 \
  | grep -o '"name": "llama-b12387-bin-win-vulkan-x64.zip"[^}]*'
curl -sI <URL do asset>            # Content-Length: 38119681
curl -s -r 37857537-38119680 <URL>  # cauda com o índice central
```

`38.119.681` é exatamente o tamanho que `src-tauri/vendor/README.md:37` registra
para este asset. O índice central tem **53 entradas**; entre elas:

```
mtmd.dll                     2.245.120
llama-common.dll             8.523.264   (não serve à mtmd.dll — ver §4)
llama-mtmd-cli.exe             97.792
llama.dll                   3.877.376    (bate com o vendorizado)
ggml-vulkan.dll            42.164.224    (bate com o vendorizado)
```

O `SHA256SUMS.txt` versionado tem **20 linhas**. As 11 DLLs que ficaram de fora
são `mtmd.dll`, `llama-common.dll`, `ggml-rpc.dll` e as oito
`llama-*-impl.dll` das ferramentas de linha de comando.

### Integridade: o que está no meu disco É o que foi publicado

Conferi os 20 arquivos vendorizados um a um contra o CRC-32 e o tamanho
descompactado da entrada correspondente no zip:

```
conferidos byte a byte contra o pacote oficial: 20 de 20
```

E o `mtmd.dll` que extraí do pacote para ler por dentro: CRC publicado
`35d9a875`, CRC medido `35d9a875`, tamanho 2.245.120 nos dois lados. Sobre esses
mesmos bytes, a soma do arquivo é

```
c60207475143fc72e2eb0d7a87757fb86906a8f1ba0fc5e94d4cf85645683f76  mtmd.dll
```

(linha pronta para `vendor/llama-cpp/SHA256SUMS.txt`; **não** a inseri — inserir
é o passo 1 da lista abaixo, e exige rodar o instalador de verdade para conferir
a soma do servidor contra a minha.)

## 3. O cabeçalho existe na revisão pinada

```
https://raw.githubusercontent.com/ggml-org/llama.cpp/b12387/tools/mtmd/mtmd.h
→ 30.254 bytes, 82 funções MTMD_API
```

Das 82, **81 aparecem como exportação C pura** (`mtmd_...` sem mangling) dentro
do `mtmd.dll` do pacote — o arquivo exporta 1.421 símbolos, 115 deles com
prefixo `mtmd_`, 1.253 com mangling C++. A exceção é `mtmd_get_memory_usage`,
que o cabeçalho declara e a tabela de exportações não devolve com esse nome.

O cabeçalho abre com o aviso de que a API é experimental e muda — o que combina
com a regra que `src-tauri/vendor/README.md:31` já enuncia: DLL e cabeçalho têm
de andar juntos, porque o sintoma de desencontro é leitura de memória alheia, não
erro de compilação.

## 4. O que a biblioteca precisa já está aqui

Tabela de importações do `mtmd.dll` (lida do PE, não da documentação):

```
llama.dll       38 símbolos
ggml-base.dll  199 símbolos
ggml.dll         2 símbolos
KERNEL32 / MSVCP140 / VCRUNTIME140 / api-ms-win-crt-*  (o resto)
```

**`llama-common.dll` não aparece.** Ela é precisa para os executáveis
(`llama-mtmd-cli.exe`), não para a biblioteca — o que significa que o runtime do
Aether cresce de 19 para 20 DLLs, não de 19 para 30, e o `mtmd.dll` é o
acréscimo de 2,2 MB (não 8,5 MB).

MSVCP140 e VCRUNTIME140 já são pré-requisito do `llama.dll` atual (15 módulos
importados, os mesmos CRTs), então nada novo é exigido da máquina.

## 5. A porta de entrada das imagens já existe na llama.dll bindável

Para injetar os vetores de uma imagem no lote do modelo, o caminho é
`llama_batch_ext_add_embd` — e ele está entre as 264 exportações de
`llama.dll` medidas em §1. Não é preciso subir versão de nada para chegar nele.

## O que falta, na ordem, com o arquivo de cada passo

1. **`src-tauri/vendor/llama-cpp/SHA256SUMS.txt`** — uma linha com a soma da §2.
   Nada mais muda no instalador: `runtime.rs:154` guarda *todo* `.dll` listado
   nas somas, e o `build.rs:183` (`publica_ggml`) copia *todo* `.dll` da pasta
   para ao lado do executável. Os dois já tratam a nona-entrante como tratam as
   outras 19.
2. **`src-tauri/src/runtime.rs:508`** — a lista `obrigatorio` do teste
   `as_somas_cobrem_o_que_o_carregador_abre` passa a incluir `"mtmd.dll"`, para
   a ausência dela falhar em teste em vez de falhar no PC do usuário.
3. **`src-tauri/vendor/llama-cpp/include/mtmd.h`** — o cabeçalho da §3, na
   revisão pinada, ao lado dos três que já estão lá.
4. **`src-tauri/src/ai/ggml/ffi.rs`** — `Ggml::abrir` (356) hoje exige exatamente
   `["llama.dll", "ggml.dll", "ggml-base.dll"]` (359) e `Pino` (335) fixa esses
   três handles. Visão é um quarto handle nas duas listas, mais os `simbolo`
   (346) das ~12 funções do caminho mínimo:
   `mtmd_init_from_file`, `mtmd_free`, `mtmd_support_vision`,
   `mtmd_get_cap_from_file`, `mtmd_context_params_default`, `mtmd_tokenize`,
   `mtmd_input_chunks_init/_size/_get`, `mtmd_input_chunk_get_type/
   _get_n_tokens/_get_n_pos`, `mtmd_batch_init/_add_chunk/_encode`,
   `mtmd_get_output_embd`, `mtmd_default_marker`.
   Decisão que esse passo arrasta: a `Ggml` é *aberta* por todo motor; se
   `mtmd.dll` entrar na lista dura, toda máquina com runtime instalado de antes
   (20 arquivos, sem o novo) passa a falhar na abertura. Ou a visão é um handle
   opcional (abre se existir, o agente diz que não vê), ou o instalador precisa
   de um passo de atualização — e isso é escolha de produto, não de código.
5. **`src-tauri/src/ai/catalog.rs`** — um `mmproj` por modelo de visão, com
   `weights_bytes` e `lfs.oid` **lidos** da API (a mesma disciplina de M8, §1),
   porque sem soma publicada não há conferência de conteúdo. Nada do catálogo de
   hoje (Granite, Ornith, os dois codificadores) tem par de visão: isso é
   inventário novo, e o `mtmd_get_cap_from_file` da lista acima serve justamente
   para medir o que um arquivo baixado anuncia, em vez de eu afirmar pela etiqueta.
6. **Interface** — o composer anexar a imagem (o `@`/`/` de FRONT4 é vizinho, não
   é isto) e a conversa mostrar o anexo. Hoje não há campo de arquivo nenhum.
7. **Bancada** — um caso com veredito medido: uma imagem conhecida, uma pergunta
   sobre ela, e o critério sendo o texto da resposta, não "rodou sem erro".

O passo 1+2+3 são dados. O 4 é o FFI. O 5 é a única parte que exige baixar
qualquer coisa nova. Os passos 6 e 7 são o que transforma a biblioteca em
capacidade do agente.

## O que NÃO foi medido

- **Nenhum `mmproj` foi baixado nem aberto.** Não sei o tamanho real dos
  projetores dos modelos que entrariam no catálogo, nem se a placa dá conta
  deles — o que sei é o que o `alojamento.rs` mede para pesos de texto, e isto
  não é isso.
- **Nada rodou.** Não há verificação de que `mtmd_init_from_file` aceita o
  `llama.dll` b12387 que já está aqui; há apenas a prova de que os símbolos que
  ele importa existem naquele binário. É condição necessária, não resposta
  pronta.
- **Os 12 nomes do caminho mínimo não são a API inteira que eu vou usar** — são o
  que a leitura de `mtmd.h` sugere para um primeiro `ver(imgem) → resposta`.
  Vídeo, áudio em streaming e as funções `mtmd_gen_audio_*` estão fora do escopo.
- **A tabela de importações de `llama-common.dll` não foi lida.** Se ela for
  precisa para *algum* caminho de que eu não sei ainda, isso aparece na primeira
  execução, não antes.
- O `mtmd.dll` é do pacote **Vulkan**; não li nada sobre o pacote de CPU puro
  (`win-cpu-x64.zip`) ter o mesmo arquivo. Uma máquina sem placa pode ter a
  visão bloqueada por um motivo diferente do que este doc mede.
