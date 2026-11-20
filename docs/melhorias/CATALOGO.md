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
  `5f9e4d49…` idêntico ao do `ornith-ai/Ornith-1.5-9B`. Isso foi lido dos dois
  lados, não copiado de um para o outro, e é a melhor evidência disponível de
  que a família é a mesma. O template declara o bloco `<tools>` e o marcador de
  chamada de ferramenta: a gramática de ferramenta que a sessão já sabe ler.
- **Gemma 4 12B** entra pela arquitetura registrada e pelo `general.license`
  lido do arquivo (`apache-2.0`), mas **não** entra como modelo de ferramentas
  verificado: o template dele usa marcadores próprios (`<tool_call`, `name`),
  não o XML que o laço do Aether escreve e parseia. O que ele faz no laço de
  ferramentas não foi medido — vê abaixo.
- **GLM-5.3-Flash ficou de fora por um motivo concreto**: os quants do
  repositório vêm em partes (`GLM-5.3-Flash-UD-Q4_K_XL-00001-of-00006.gguf`), e
  o downloader do Aether baixa um arquivo por entrada. Um modelo dividido em
  seis partes entraria no catálogo como um download que nunca fica completo.
- **Os repositórios oficiais dos pesos grandes também ficaram de fora por
  acesso**: `Qwen/Qwen3.5-4B-GGUF` e `zai-org/GLM-5.3-Flash-GGUF` respondem
  `401` (gated) à API anônima, e o Aether não pede token do usuário. Os
  repositórios que entraram como `weights_repo` respondem `200` com
  `gated: false`.

## O que esta frente NÃO mediu, e é exatamente por isso que está registrado

Nada aqui rodou. O que falta é placa, e placa é o portão que a instrução de
não abrir o app mantém fechado:

1. **Alojamento real.** Os `bytes` são o tamanho do arquivo, e o veredito de
   quantas camadas sobem sai de `alojamento::alojar` com a VRAM livre medida na
   hora. Os novos GGUFs nunca passaram por ali. Em particular, o `kv_por_token`
   do `alojamento.rs:254-269` é declarado **teto**, não conta exata, em
   arquitetura híbrida — o `qwen35` já estava nessa condição com o Ornith, e o
   `gemma4` declara `attention.sliding_window`, `shared_kv_layers` e
   `sliding_window_pattern` no cabeçalho, que a conta atual não modela. O
   número da tela pode ser conservador (menos camadas na placa do que caberiam).
   Conferir é `engine_probe alojar:` com o peso em disco.
2. **A qualidade do texto e o custo por rodada** (tokens de entrada e saída,
   t/s) — medição de turno, placa, aplicativo aberto.
3. **Chamada de ferramenta no Gemma 4.** O catálogo declara o modelo como de
   conversa, e o roteador pode escolhê-lo; se o laço pedir ferramentas a um
   modelo cujo template não usa a gramática que o parser lê, a rodada devolve
   texto em vez de chamada. Isso é verificação de produto, com a placa.

## Gates desta frente

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
2. **Alojamento com janela deslizante e KV compartilhado** — as chaves
   `gemma4.attention.sliding_window`, `shared_kv_layers` e
   `sliding_window_pattern` estão no cabeçalho e hoje não entram na conta.
3. **Veredito de ferramenta por família** — medir, com a placa, quais das
   famílias do catálogo conseguem entregar uma chamada no formato que o laço
   parseia, e dizer isso na tela em vez de deixar a pessoa descobrir no meio de
   um turno.
