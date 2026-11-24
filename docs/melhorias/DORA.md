# DORA — a metade montada deixou de ser silêncio

Task #98, derivada da #34. Dono do Rust: maintainer. Frente TS/locales: um
agente com dono único de `src/lib/types.ts`, `src/components/AdaptadoresDialog.tsx`
e dos dois JSON de locale. Este doc é lido do diff desta árvore e de medições
rodadas nela; o que não foi rodado está marcado como tal.

## O defeito (lido)

A ficha de um adaptador já dizia a verdade: `Ficha.dora`
(`src-tauri/src/ai/adaptadores.rs:71`) é `true` quando o cabeçalho GGUF traz um
tensor cujo nome termina em `mag`, e isso é teste de arquivo desde a quarta leva
(`a_ficha_ve_magnitude_e_invocacao_onde_o_arquivo_escreve_elas`).

O que faltava estava do lado de cá do arquivo. A biblioteca vendorizada casa os
pares de deltas e não tem o que ler de magnitude, então um DoRA **não falha** ao
montar: ele entra pela metade. E nada no estado dizia isso —

- `git grep -n dora -- 'src-tauri/src' ':!src-tauri/src/ai/adaptadores.rs' ':!src-tauri/src/voz'`
  devolvia **zero linhas**: nenhum caminho de carga olhava para o flag;
- a linha do adaptador aplicado (`AdaptadoresDialog.tsx:272-279`) respondia
  "Na placa" para o DoRA e para o LoRA comum com as mesmas palavras, e
  "Na placa" é lido como "o modelo que o arquivo descreve está rodando";
- a nota da tela jurava o contrário do que acontece: dizia que a biblioteca
  "recusa tensor que não seja delta". Ela não recusa nada — recusa seria
  barulho, e o que havia era silêncio.

## Medido no binário, antes de escrever qualquer frase

`llama.dll` vendorizada = `b12387` (`vendor/README.md:37`, e a mesma revisão que
`runtime.rs:74` baixa). Duas leituras, uma por `grep` e outra por byte exato, no
arquivo de 3.877.376 bytes:

```
grep -aoE "%s\.[a-z_.]{2,40}" llama.dll | sort -u | wc -l      → 203
grep -aoE "%s\.[a-z_.]{2,40}" llama.dll | sort -u | grep -c magnitude → 1
      (o único é `%s.hyper_connection.magnitude` — outro mecanismo, não DoRA)
grep -haoE "\blora_[ab]\b" llama.dll | sort | uniq -c          → lora_a 2, lora_b 1
grep -haoE "\b(mag|mag_b|dora)\b" llama.dll ggml-base.dll ggml-vulkan.dll → nada
```

A leitura por byte, que é a que o teste usa, é mais seletiva que o `grep` de
palavra: o `lora_a` que aparece três vezes inclui o `adapters_lora_are_same`, e
o único `.mag` do binário está no meio de `.magnitude`. Daí as agulhas
exatas, cada uma conferida nesta máquina:

| agulha | ocorrências no `llama.dll` | o que prova |
|---|---|---|
| `llama_adapter_lora_init` | 5 | a ponte fala com este binário |
| `llama_set_adapters_lora` | 2 | idem, pelo outro lado da chamada |
| `".mag\0"` | 0 | nenhum nome de tensor de magnitude é construído aqui |
| `mag_b` | 0 | idem, pela variante de quem separa as metades |
| `dora` | 0 | idem, pelo nome do mecanismo |

**É evidência de string em binário, não execução.** O que ela fundamenta é a
frase "esta biblioteca não lê magnitude"; o que ela não prova é o efeito do DoRA
meio montado contra pesos reais — isso exige placa, pesos e um adaptador DoRA,
e o aplicativo aberto volta a ser o portão (pendência 1).

## O que mudou

### Núcleo

- `ai/ggml/mod.rs:159-172` — `LE_MAGNITUDE: bool = false`, a capacidade declarada
  da biblioteca, com a medição acima no doc e a razão de não ser memória de
  changelog. É `false` e o nome diz o que é: um DoRA montado aqui monta deltas.
- `ai/adaptadores.rs:341-372` — `Item::magnitude_fora` e a função que o decide.
  A regra é uma linha (`:371`): `aplicada && ficha.dora && !LE_MAGNITUDE`. As
  três condições têm motivo: `aplicada` porque nada montado não tem metade
  montada; `ficha.dora` porque a declaração vem do arquivo; `!LE_MAGNITUDE`
  porque a decisão é sobre a biblioteca, não sobre o adaptador.
- `ai/adaptadores.rs:394` e `:437` — as duas ligações em `itens()`: o arquivo
  descoberto no disco e o pin órfão. O órfão passa pelo mesmo `magnitude_fora`,
  com a ficha que não tem cabeçalho lido, em vez de um `false` copiado.
- O motor não mudou uma linha: `usar_adaptadores` continua respondendo o que a
  biblioteca respondeu (`aplicada`, `erro`). Ele não tem do que decidir — a
  decisão é sobre o que a resposta *significa*, e quem tem a ficha é o
  `adaptadores`.

### Instrumento, não frase

- `ai/ggml/mod.rs:1025-1069` — `a_dll_vendorizada_e_a_que_esta_capacidade_descreve`
  lê o `llama.dll` e confronta o binário com `LE_MAGNITUDE`. Os dois símbolos da
  ponte precisam estar no arquivo (sem eles a comparação não valeria nada: seria
  um ponteiro LFS ou outra release); os três nomes de tensor de magnitude não
  podem estar. Clone recém-feito, sem DLL em disco, diz `ausente: nada contra o
  que conferir` em vez de passar fingindo — o mesmo acordo de
  `catalog.rs:783 every_entry_is_an_architecture_the_engine_runs`.
- `ai/adaptadores.rs:748-822` — `um_dora_aplicado_registra_que_a_magnitude_ficou_fora`:
  escreve dois GGUF reais byte a byte (um com os `mag` nos nomes, um sem), exige
  que as fichas digam o que o teste presume, e confere o estado nos três casos:
  DoRA aplicado (`true`), LoRA comum aplicado (`false`), e o DoRA sem motor e
  com o motor recusando (`false`, `:803`). Pina também o nome no JSON
  (`magnitudeFora`) — um `camelCase` que escapar aqui escapa invisível, o chip
  simplesmente some.

### Frente

- `src/lib/types.ts:951` — `magnitudeFora: boolean` no `ItemAdaptador`, e o
  comentário do `dora` da ficha (`:931-933`) corrigido: a biblioteca não recusa
  tensor nenhum, ela monta os pares e deixa a magnitude de fora.
- `src/components/AdaptadoresDialog.tsx:273-289` — a linha de estado: aplicado
  com magnitude fora deixa de dizer "Na placa" e diz **"Na placa, sem
  magnitude"**, com ícone de aviso e a nota no `title`. Aplicado limpo continua
  `check` + "Na placa"; não aplicado continua "Fora da placa". Nenhuma classe
  nova.
- `locales/pt-BR.json:376-377` e `locales/en-US.json:376-377` — `semMagnitude` e
  `semMagnitudeNota` nos dois idiomas, na mesma posição dos dois arquivos
  (paridade conferida chave a chave: 1009 chaves achatadas de cada lado).

## Vermelho, medido por mutação

Não há como escrever um DoRA "montado de verdade" nesta árvore, então o
instrumento foi virado pelo único ponto que pode virá-lo: `LE_MAGNITUDE` trocado
para `true`, que é exatamente a hipótese "a biblioteca passou a ler magnitude".
Os dois testes caíram, cada um com a frase que explica o que fazer:

```
---- ai::ggml::tests::a_dll_vendorizada_e_a_que_esta_capacidade_descreve
assertion `left == right` failed: a biblioteca vendorizada e o que `LE_MAGNITUDE`
declara divergiram. …  left: true  right: false

---- ai::adaptadores::tests::um_dora_aplicado_registra_que_a_magnitude_ficou_fora
panicked at src\ai\adaptadores.rs:790: as deltas entraram sem a normalização
```

A constante voltou a `false` antes do verde, e o `git diff` do arquivo fecha em
61 linhas adicionadas e nenhuma alterada — a mutação não deixou rastro.

## Pendências

1. **Alegado, não medido: o efeito de meio DoRA nos pesos.** O que está provado
   aqui é a montagem (delta entra, magnitude não) e o registro honesto disso. O
   que a resposta do modelo perde com a normalização de fora não foi medido —
   precisa de pesos, placa e um adaptador DoRA real, e roda fora do alcance com
   o aplicativo fechado.
2. **Montar a magnitude de verdade é outra frente.** Não há API na biblioteca
   para isso: `llama_set_adapters_lora` soma deltas escaladas no grafo de
   computação, e a normalização por linha do DoRA precisa do peso já fundido.
   Fazer certo é fundir o adaptador no GGUF com a magnitude aplicada — um
   conversor nosso, com teste contra um DoRA conhecido — não um número na tela.
3. **Sinal cromático do "sem magnitude".** A linha reusa `adaptador-estado
   ligada` (acentro de aplicada) e só muda o ícone e o texto. Se o aviso precisar
   de cor própria, é classe nova em `styles.css`, e isso é escolha de design, não
   defeito.

## Gates lidos nesta árvore

```
cd src-tauri && cargo test --lib          → 567 passaram, 0 falharam, 11 ignored (4,03 s)
cd src-tauri && cargo check --all-targets → Finished, zero avisos
npm run build                             → ✓ built in 216 ms, maior chunk react a 210,53 kB
```

Eram 565 antes da frente; os dois novos são `a_dll_vendorizada_e_a_que_esta_capacidade_descreve`
e `um_dora_aplicado_registra_que_a_magnitude_ficou_fora`.
