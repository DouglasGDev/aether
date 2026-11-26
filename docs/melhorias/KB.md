# KB — a base de conhecimento dentro do app (tarefa 120)

Quatro frentes se encontraram aqui: uma loja de notas em disco, uma terceira
fonte no índice, o caminho que faz a nota presa entrar no turno sem ninguém
pedir, e a tela que mostra as três contas separadas — o que está na pasta, o que
o índice lembra, o que está preso.

Os arquivos que este doc descreve, com o censo de cada um medido nesta árvore
(`wc -l`, rodado na raiz do repositório):

| arquivo | linhas | o que é |
| --- | --- | --- |
| `src-tauri/src/ai/base.rs` | 695 | **novo** — a loja: pasta, id, `.md`, marcador de presa, censo |
| `src-tauri/src/local/base.rs` | 235 | **novo** — a ferramenta `escrever_na_base` e o censo de chaves |
| `src/components/BaseDialog.tsx` | 587 | **novo** — a tela |
| `src-tauri/src/ai/memoria.rs` | 4094 | a terceira fonte, a ficha, o bloco do turno |
| `src-tauri/src/commands.rs` | 1936 | cinco comandos + o ponto de injeção do turno |
| `src-tauri/src/local/mod.rs` | — | entrada de catálogo, despacho, censo do catálogo |
| `src-tauri/src/local/vista.rs` | — | `Vista::Base` (`vista.rs:169`) |
| `src-tauri/src/local/memoria.rs`, `ai/mod.rs`, `mcp/execucao.rs`, `mcp/servidor.rs`, `state.rs`, `lib.rs` | — | registro, estado, o braço que o `match` exaustivo obrigou |
| `src/App.tsx`, `components/AppMenu.tsx`, `components/ToolCard.tsx`, `components/ResultCard.tsx`, `lib/api.ts`, `lib/atalhos.ts`, `lib/display.ts`, `lib/types.ts`, `styles.css` | 5180 (css) | a cara do efeito |
| `locales/pt-BR.json`, `locales/en-US.json` | 1117 chaves cada | as 52 chaves da família |

Nenhuma dependência nova: `git diff --stat src-tauri/Cargo.toml` sai vazio. Medido
com a roda ainda na área de trabalho, antes de qualquer `git add`, o
`git diff --stat` dos arquivos rastreados é 22 arquivos, +1567/−52
(`git diff --stat | tail -1`); os quatro arquivos novos não entram nesse número
porque ainda não têm blob — `ai/base.rs`, `local/base.rs`, `BaseDialog.tsx` e
este doc. Eles estão no `wc -l` acima.

Nada foi aberto: nenhum app, nenhuma GUI. A injeção ao vivo **não foi medida** —
o item 9 lista o que ficou de fora, incluindo o que a suíte não alcança sem um
codificador instalado.

## 1. A loja: `base/` é a verdade, e o arquivo carrega a decisão

`ai/base.rs` não tem banco, não tem índice lateral, não tem arquivo de
metadados ao lado do `.md`. A nota É o arquivo: `PASTA = "base"` (`base.rs:45`),
`EXTENSAO = "md"` (`base.rs:50`), e o que decide se ela entra em todo turno é
uma linha dentro dele — `FIXADA = "<!-- aether:fixada -->"` (`base.rs:56`). Um
estado gravado em dois lugares é dois estados que um dia divergem; aqui a
divergência é impossível por construção, e `fixar_e_solta_mora_no_arquivo_e_nao_em_lugar_nenhum_outro`
(`base.rs:620`) confere isso lendo o bytes do disco, não a struct.

As regras que o fonte aplica, cada uma com o teste que a pinça:

| regra | onde | teste |
| --- | --- | --- |
| o id vem do título, sem acento, em minúsculo, com hífens | `slug` `base.rs:163` | `o_slug_e_minusculo_sem_acento_e_com_hifens` |
| título que não gera slug não gera arquivo sem nome | `slug` | `um_slug_vazio_nao_produz_arquivo_sem_nome` |
| o id dito pelo modelo não vira caminho fora da pasta | `conferir_id` `base.rs:209` | `um_id_do_modelo_nao_vira_caminho_de_fora_da_pasta` |
| pasta ausente é base vazia, não erro | `ids` `base.rs:309` | `uma_pasta_que_nao_existe_e_uma_base_vazia_e_nao_um_erro` |
| título em branco não escreve | `escrever` `base.rs:394` | `titulo_vazio_nao_cria_arquivo` |
| colisão de título ganha sufixo — e só quando colide | `escrever` | `a_colisao_de_titulo_ganha_sufixo_e_nao_por_nada_no_lugar` |
| reescrever pelo id é a mesma nota, com título novo | `escrever` | `escrever_de_novo_com_o_id_mantem_a_nota_e_troca_o_titulo` |
| um `.md` largado à mão na pasta é adotado como nota | `nota_de` `base.rs:262` | `um_md_largado_a_mao_na_pasta_e_adoptado_como_nota` |
| a lista ordena pela nota mais mexida | `listar` `base.rs:336` | `a_lista_ve_primeiro_a_nota_mais_mexida` |
| nota de corpo vazio continua na lista | `listar` | `uma_nota_de_corpo_vazio_continua_na_lista` |
| apagar um id que não existe diz qual id | `apagar` `base.rs:461` | `apagar_um_id_que_nao_existe_diz_qual_e_o_id` |
| escrever sem falar de presa não mexe na decisão | `escrever` | `escrever_sem_dizer_de_fixar_nao_mexe_na_decisao` |
| o que se devolve é o que o disco respondeu depois | `Registro` `base.rs:116` | `a_escrita_devolve_os_bytes_e_as_linhas_que_o_disco_respondeu` |

O corpo tem teto (`CORPO_MAX_BYTES = 1_048_576`, `base.rs:64`) e a escrita é
atômica no padrão da casa: `.md.tmp` e `rename` (`gravar`, `base.rs:449`), com
`base.erro.escreverTemporario` nomeando o meio do caminho que falhou.

## 2. A terceira fonte: a nota não empresta a política de ninguém

`Fonte::Documento` (`memoria.rs:282`) entrou ao lado de `Arquivo` e `Conversa`, e
`Origem::Fixada` (`memoria.rs:255`, `#[serde(rename_all = "lowercase")]` →
"fixada" no fio) ao lado de `vetorial`, `lexica` e `ambas`. O ponto do doc é
outro: **cada fonte tem a sua trava, e nenhuma delas manda nas outras duas**.

`Busca.documentos: &'a [String]` (`memoria.rs:559`) é a lista de ids que a
terceira fonte obedece, e quem a enche é `procurar` (`memoria.rs:2094`), lendo o
disco dentro da chamada (`base::ids` em `memoria.rs:2096`) — o mesmo desenho de
`raizes` para arquivos e `ignorar` para conversas. `buscar` (`memoria.rs:2322`)
passa `documentos: &[]` e o comentário no local diz por quê: quem responde pela
lista é `procurar`. A regra por fonte mora no `match` de `pode`
(`memoria.rs:1076-1090`):

```rust
Fonte::Arquivo => concedida(&trecho.arquivo, raizes),
Fonte::Conversa => {
    !busca.ignorar.iter().any(|id| *id == trecho.arquivo)
}
// A nota ainda existe? O `ignorar` do turno não passa por aqui:
// ele corta o que a conversa já tem no buffer, e uma nota curada
// não está em buffer nenhum.
Fonte::Documento => busca
    .documentos
    .iter()
    .any(|id| *id == trecho.arquivo),
```

Ter a lista lida no disco, e não guardada na memória, é o que faz apagar o `.md`
por fora tirar a nota da resposta na chamada seguinte — sem pedir nada a ninguém.
O índice continua com a peça (`apagar_o_md_por_fora_tira_a_nota_da_resposta_sem_tocar_no_indice`,
`memoria.rs:3911`): esconder da resposta não é apagar a memória.

A memória do que foi indexado é a `FichaDocumento{titulo, atualizada, fixada,
prova, truncada}`, uma por nota (`Indice::documentos` `memoria.rs:805`,
`fixadas` `:812`, `nota_vista` `:818`, `notas_pendentes` `:827`, `reter_notas`
`:842`, `substituir_documento` `:861`, `pecas_fixadas` `:899`,
`esquecer_documento` `:797`). A prova é `{bytes: nota.para_indexar().len(),
modificado: nota.atualizado}` (`prova_da_nota`, `memoria.rs:596`), e o título
está dentro do texto medido porque **o título também é indexado** —
`mudar_o_titulo_e_mudar_a_nota_porque_o_titulo_tambem_indexa` (`memoria.rs:3894`)
confere que a troca de título rejeita a prova antiga e manda a nota de volta para
a fila.

O reindexar apaga antes de inserir (`substituir_documento`, `memoria.rs:868`),
então a nota reescrita não deixa peça órfã no chão (`reescrever_a_nota_troca_as_pecas_delas_e_nao_deixa_orfa`,
`memoria.rs:3846`). E o que o teto de peças cortou fica marcado `truncada` e volta
para a fila na rodada seguinte — `uma_nota_cortada_pelo_teto_volta_para_a_fila`
(`memoria.rs:3879`) — porque perder o fim de uma nota em silêncio seria a
ferramenta sabendo de menos do que diz.

## 3. A presa: o bloco do turno e a chave que fecha as duas portas

`bloco_do_turno` (`memoria.rs:2407`) é o que a conversa injeta, e ele não é a
busca: a lista é `self.fixados().into_iter().chain(buscados)` (`memoria.rs:2434`),
com o teto somado — `TRECHOS_MAX` para o bloco inteiro, não por fonte — e o
repetido entrando uma vez só, na frente, com a origem que diz por que ele está
ali. Quem chama é `commands.rs:1044`, e chama só quando
`buscar_memoria` está ativa no grafo: a mesma chave que tira a ferramenta do
catalogo tira o bloco do prompt.

`fixados` (`memoria.rs:2377`) lê o índice e confere a pasta: `base::ids` no disco
(`memoria.rs:2379`) filtra peça por peça. Uma presa cujo `.md` saiu da pasta não
entra, e entra de novo se o arquivo voltar — sem sincronização, sem pedido.

Zero peça indexada não entra por nenhuma fresta: `pecas_fixadas` devolve zero
quando a nota nunca foi vetorada (`memoria.rs:899`), então uma nota presa que o
codificador nunca viu não aparece no cartão. Recusar isso seria escrever um
bloco sobre texto que ninguém mediu — a mesma porta que o resto da memória já
fecha para arquivos e conversas.

`ajustes.trechos == 0` desliga tudo: a busca e as presas. É uma chave só
(`memoria.rs:2409`), e a mutação provou que ela fecha por dois caminhos — o
retorno antecipado e o `teto` que o laço obedece (`memoria.rs:2435`) — porque
abrir um dos dois ainda deixa o bloco vazio. O item 6 tem a transcrição.

## 4. A ferramenta e o catálogo: 13 desligada, 14 ligada

Uma porta só: `escrever_na_base` (`local/mod.rs:243`, `Mutating`), despacho em
`local/mod.rs:360`, execução em `local/base.rs:23`. O efeito é `Mutating` e não
`Destructive` porque a nota nova não tira nada do que estava lá; reescrever pelo
id que o pedido diz é decisão do dono. O esquema tem quatro propriedades
(`titulo`, `corpo` obrigatórios; `nota`, `fixar` opcionais) e **nenhuma delas é
um caminho**: a nota cai em `base/`, ao lado de `rag.json`, no caminho que o
próprio custódio recebeu (`state.rs:72`, registrado em `lib.rs:236-240`).

Não há ferramenta de leitura na base de propósito: `buscar_memoria` já responde
por nota desde o item 2, e duas portas para a mesma busca são duas políticas que
um dia divergem. O que só esta porta faz é escrever.

`base_escrever` (`commands.rs:1648`) calcula `nova` lendo os ids ANTES da escrita
e sincroniza a pasta inteira depois; `base_apagar` (`commands.rs:1676`) sincroniza
pela diferença; `base_fixar` (`commands.rs:1694`) recebe `fixada: bool` e escreve
o marcador. Os censos: `o_catalogo_cobre_arquivos_memoria_base_resultados_terminal_e_sistema`
(`local/mod.rs:434`) e `escrever_na_base_e_o_dispatch_que_a_executa`
(`local/mod.rs:662`).

## 5. A tela mostra três números, e não um

`BaseDialog.tsx` é a janela da base: censo (`base.nota`/`base.notas`), a lista
ordenada pela mais mexida com presa, data e contagem de caracteres por linha, o
editor em linha, e os quatro pedidos — nova, salvar, prender/soltar, apagar —
com o eco dos dois lados (o que a pessoa pediu e o que o disco respondeu) em vez
de um "pronto". Enquanto o índice está varrendo, a tela relê a cada segundo em
vez de fingir que o número parou.

Os chips separam as três contas que o `EstadoBase` (`commands.rs:1592`) devolve
separadas — `base.naPasta` (o disco), `base.noIndice` (o índice), `base.pecas` e
`base.fixadas` / `base.fixadasIndice` (as presas, na pasta e no índice). Quando o
índice sabe de menos que a pasta, a tela diz por quê com a palavra do próprio
núcleo (`base.semCodificador`, `base.indexando`) — os mesmos estados que a tela
de memória já mostram, e o diálogo só tem o botão de ir até lá
(`base.irMemoria`, a cadeia `App.tsx:1150-1152`).

A entrada: `/base` no composer (`atalhos.ts:42`), item no menu (`AppMenu.tsx:63`)
e o cartão da ferramenta (`ToolCard.tsx:191-234`), com o chip de efeito vindo de
`display.ts` para a origem nova (`fixada` → `memoria.origemFixada` `:70`,
`documento` → `memoria.fonteDocumento` `:78`). O `ResultCard.tsx:24` declara
`base: true` para o cartão saber que existe vista nova.

## 6. As 52 chaves publicadas

A família saiu inteira nas duas pastas na mesma roda em que o texto foi escrito:
42 `base.*` (4 do texto que a ferramenta devolve + 28 da tela + 10 de recusa), 2 da ferramenta e
8 dos argumentos. Paridade medida e nenhum placeholder divergente entre pt-BR e
en-US.

O varredor de `idiomas.rs:307` (`chaves_pedidas`) acha o `tr("literal")` inclusive
quando o argumento quebra linha depois do parêntese, então as 14 chaves escritas
no fonte — as quatro do texto e as dez de recusa — já estavam cobradas. As 10
que caem fora são as montadas por `format!` dentro de `ferramenta()` e `campo()`:
`ferramentas.escreverNaBase.titulo/.descricao` e os oito `argumentos.*`. Para
essas o censo é o único censo que existe: `CHAVES` (`local/base.rs:202`) lista as
24 chaves do lado Rust e `toda_chave_da_base_tem_texto_em_todo_idioma` confere,
chave por chave, que `idiomas::tem_texto` responde verdadeiro nos dois idiomas.

Tabela espelho das pastas (`base.*` e o resto da família), lida dos dois JSON:

| chave | pt-BR | en-US |
| --- | --- | --- |
| `base.escrita` | Nota "{titulo}" (id {id}) gravada na base: {bytes} bytes, {notas} notas na pasta. | Note "{titulo}" (id {id}) written to the base: {bytes} bytes, {notas} notes in the folder. |
| `base.aprendeu` | O índice aprendeu {pecas} peças, {tokens} tokens vetorizados em {ms} ms. | The index learned {pecas} passages, {tokens} tokens vectorized in {ms} ms. |
| `base.semPeca` | Nenhuma peça entrou agora: as {notas} notas da pasta já estavam lembradas ou não há codificador carregado — a tela de memória diz qual dos dois. | No passage entered now: the {notas} notes in the folder were already remembered, or no encoder is loaded — the memory screen says which. |
| `base.preso` |  A nota ficou presa: entra no turno sem precisar ser buscada. |  The note is pinned: it enters the turn without being searched for. |
| `base.titulo` | Base de conhecimento | Knowledge base |
| `base.notas` | Notas na pasta | Notes in the folder |
| `base.vazia` | A pasta ainda não tem notas. | The folder has no notes yet. |
| `base.nova` | Nova nota | New note |
| `base.editando` | Editar nota | Edit note |
| `base.atualizar` | Reler a pasta | Re-read the folder |
| `base.salvando` | Salvando… | Saving… |
| `base.abrir` | Abrir {nome} | Open {nome} |
| `base.apagar` | Apagar {nome} | Delete {nome} |
| `base.prender` | Prender {nome} | Pin {nome} |
| `base.soltar` | Soltar {nome} | Unpin {nome} |
| `base.irMemoria` | Abrir a tela de memória | Open the memory screen |
| `base.naPasta` | {n} notas na pasta | {n} notes in the folder |
| `base.noIndice` | {n} lembradas pelo índice | {n} remembered by the index |
| `base.pecas` | {n} peças | {n} passages |
| `base.fixadas` | {n} presas | {n} pinned |
| `base.fixadasIndice` | o índice sabe de {n} | the index knows {n} |
| `base.semCodificador` | Sem codificador carregado | No encoder loaded |
| `base.indexando` | Varredura segurando o índice | The scan is holding the index |
| `base.caracteres` | {n} caracteres | {n} characters |
| `base.criada` | criada {quando} | created {quando} |
| `base.mexida` | mexida {quando} | touched {quando} |
| `base.novaNota` | Nota nova | Brand-new note |
| `base.reescrita` | Nota que já estava na pasta | Note that was already in the folder |
| `base.presa` | Presa | Pinned |
| `base.solta` | Solta | Unpinned |
| `base.removidas` | {n} peças saíram do índice. | {n} passages left the index. |
| `base.apagada` | Nota "{titulo}" (id {id}) apagada: {bytes} bytes fora da pasta, {notas} notas ficaram. | Note "{titulo}" (id {id}) deleted: {bytes} bytes out of the folder, {notas} notes left. |
| `base.erro.idInvalido` | Nome de nota impossível: "{id}" | Impossible note name: "{id}" |
| `base.erro.leitura` | Não consegui ler {caminho} | Could not read {caminho} |
| `base.erro.criarPasta` | Não consegui criar a pasta da base: {caminho} | Could not create the base folder: {caminho} |
| `base.erro.tituloVazio` | A nota precisa de um título | A note needs a title |
| `base.erro.corpoGrande` | Corpo de {bytes} bytes passa o teto de {limite} | Body of {bytes} bytes passes the {limite} limit |
| `base.erro.notaAusente` | Não há nota com o id "{id}" | There is no note with id "{id}" |
| `base.erro.escrever` | Não consegui escrever {caminho} | Could not write {caminho} |
| `base.erro.medir` | Não consegui medir {caminho} | Could not measure {caminho} |
| `base.erro.escreverTemporario` | Não consegui escrever o arquivo temporário {caminho} | Could not write the temporary file {caminho} |
| `base.erro.apagar` | Não consegui apagar {caminho} | Could not delete {caminho} |
| `ferramentas.escreverNaBase.titulo` | Escrever na base | Write to the base |
| `ferramentas.escreverNaBase.descricao` | Grava uma nota na base de conhecimento do aplicativo, na pasta base/ ao lado das escolhas de memória, e pôe o índice atrás dela na mesma chamada. Não recebe caminho: a nota nunca cai numa pasta concedida. O texto volta com o id, os bytes do arquivo e quantas peças o índice aprendeu — zero peça significa que a nota está no disco e ainda não foi vetorizada. | Stores a note in the app's knowledge base, in the base/ folder next to the memory settings, and sends the index after it in the same call. It takes no path: the note never lands in a granted folder. The reply carries the id, the file bytes and how many passages the index learned — zero passages means the note is on disk and not yet vectorized. |
| `argumentos.titulo` | Título | Title |
| `argumentos.tituloDescricao` | A primeira linha da nota, e o que a busca acha quando você procura pelo nome dela. | The first line of the note, and what the search finds when you look for it by name. |
| `argumentos.corpo` | Corpo | Body |
| `argumentos.corpoDescricao` | O texto da nota, em texto simples ou markdown. Teto de 1.048.576 bytes. | The text of the note, plain text or markdown. Limit 1,048,576 bytes. |
| `argumentos.nota` | Nota | Note |
| `argumentos.notaDescricao` | O id de uma nota já existente, para reescrevê-la em vez de criar outra. Ausente, a nota é nova e o id sai do título. | The id of an existing note, to rewrite it instead of creating another. Absent, the note is new and the id comes from the title. |
| `argumentos.fixar` | Fixar | Pin |
| `argumentos.fixarDescricao` | Verdadeiro prende a nota: ela entra em todo turno sem ser buscada. Ausente, a decisão anterior continua. | True pins the note: it enters every turn without being searched for. Absent, the previous decision stands. |

## 7. As mutações, e o vermelho com nome

Cinco mutações correram nesta árvore, cada uma aplicada, medida e desfeita antes
da seguinte. Os números de linha abaixo são do arquivo final — o que este doc
publica é o que saiu no dia em que ele foi escrito.

| mutação | vermelha nomeada | o que a asserção diz |
| --- | --- | --- |
| a cadeia do bloco sem as presas (`memoria.rs:2434` → `buscados.into_iter()`) | `o_bloco_do_turno_leva_a_presa_e_o_trechos_zero_fecha_as_duas_portas`, `memoria.rs:3992` | "a nota presa não depende da busca para entrar" |
| `pode` devolvendo verdadeiro para a terceira fonte (`memoria.rs:1086-1089`) | `apagar_o_md_por_fora_tira_a_nota_da_resposta_sem_tocar_no_indice`, `memoria.rs:3948` | a lista lida no disco é quem governa a fonte |
| inserir sem `remover` antes (`memoria.rs:868`) | `reescrever_a_nota_troca_as_pecas_delas_e_nao_deixa_orfa`, `memoria.rs:3855` | "a peça que saiu do texto saiu do índice" |
| `fixados` ignorando a pasta (`memoria.rs:2383` → `filter(\|_\| true)`) | `o_bloco_do_turno_…`, `memoria.rs:4008` | `assert!(…achados.is_empty())` depois do `remove_file` |
| as duas metades do desligamento abertas (`memoria.rs:2409` → `false`, `memoria.rs:2435` → `teto + 1`) | `o_bloco_do_turno_…`, `memoria.rs:3999` | o zero fecha a busca E a presa |

A saída como ela veio, linha por linha (o `:coluna` é o que o `assert!` marca):

```text
M1  panicked at src\ai\memoria.rs:3992:9:  assertion `left == right` failed:
    a nota presa não depende da busca para entrar      left: 0  right: 1
M2  panicked at src\ai\memoria.rs:3948:9:  (39 passed; 1 failed)
M3  panicked at src\ai\memoria.rs:3855:9:  assertion `left == right` failed:
    a peça que saiu do texto saiu do índice            left: 3  right: 1
M4  panicked at src\ai\memoria.rs:4008:9:  assertion failed:
    custodio.bloco_do_turno("o que vale aqui?", &[]).achados.is_empty()
M5  panicked at src\ai\memoria.rs:3999:9:  assertion failed:
    custodio.bloco_do_turno("o que vale aqui?", &[]).achados.is_empty()
```

E as metades da quinta, cada uma sozinha, com o teste nomeado respondendo
`test result: ok. 1 passed`:

```text
aberta só `memoria.rs:2409` (o retorno antecipado)  →  verde
aberta só `memoria.rs:2435` (o `teto` do laço)      →  verde
```

A quinta linha é a que ensinou algo: mutar só o retorno antecipado ficou verde, e
mutar só o teto do laço ficou verde. As duas metades se mascaram porque cada uma,
sozinha, já fecha a porta — o assert de `trechos == 0` só cai com as duas
abertas. Isso está escrito no doc-comment de `bloco_do_turno` (`memoria.rs:2390-2396`)
e no do teste (`memoria.rs:3955-3961`) para quem chegar depois não confundir
redundância com porta esquecida.

As 27 peças novas de teste: **14** em `ai::base::testes`, **5** em
`local::base::testes` (as quatro da porta + o censo de chaves), **7** em
`ai::memoria::testes` (`a_nota_presa_entra_na_frente_sem_ninguem_ter_perguntado`,
`reescrever_a_nota_troca_as_pecas_delas_e_nao_deixa_orfa`,
`a_nota_que_sai_da_pasta_sai_do_indice_e_da_ficha`,
`uma_nota_cortada_pelo_teto_volta_para_a_fila`,
`mudar_o_titulo_e_mudar_a_nota_porque_o_titulo_tambem_indexa`,
`apagar_o_md_por_fora_tira_a_nota_da_resposta_sem_tocar_no_indice`,
`o_bloco_do_turno_leva_a_presa_e_o_trechos_zero_fecha_as_duas_portas`) e **1** em
`local::tests` — o despacho. O censo do catálogo não somou peça: foi rebatizado
(`o_catalogo_cobre_arquivos_memoria_resultados_terminal_e_sistema` →
`..._memoria_base_...`), e é por isso que 14 + 5 + 7 + 1 fecha em 27 e não em 28.
Contados na saída:

```text
$ cd "C:/Aether AI/src-tauri" && cargo test --lib
test result: ok. 712 passed; 0 failed; 12 ignored; 0 measured; 0 filtered out; finished in 6.65s

$ grep -c '^test ai::base::testes::.* \.\.\. ok$' <log>          # 14
$ grep -c '^test local::base::testes::.* \.\.\. ok$' <log>       # 5
$ grep -c '^test ai::memoria::testes::.* \.\.\. ok$' <log>       # 40
$ grep -c '^test local::tests::.* \.\.\. ok$' <log>              # 16

$ cd "C:/Aether AI/src-tauri" && cargo check --all-targets
    Checking aether v0.1.0 (C:\Aether AI\src-tauri)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.99s

$ cd "C:/Aether AI" && npm run build
✓ 308 modules transformed.
✓ built in 178ms
```

Setecentos e vinte e quatro casos anunciados — 712 verdes, 12 ignorados. A roda
da REDE anunciou 697; os 27 novos são desta. Do lado da interface o `npm run build`
soma um módulo aos 307 da roda anterior — os 308 do censo acima são o
`BaseDialog` entrando como o lazy chunk que o `vite` já cortaria de qualquer
jeito, e o `dist/assets/BaseDialog-*.js` separado é a prova de que ele não subiu
no bundle inicial.

Os dois últimos `grep -c` são o total do módulo, não o acréscimo: `ai::memoria`
tinha 33 casos antes desta frente e tem 40, e `local::tests` tinha 15 e tem 16.
As duas linhas de cima são módulos novos, então ali o total é o acréscimo.

## 8. O que a suíte não alcança

- **Sem codificador instalado, o índice não aprende.** Os 19 testes da loja e da
  porta provam arquivo, id, marcador, recusa e censo — e nenhum deles vetoriza:
  `pecas()` exige `Tokenizer`, que só o codificador traz. É por isso que
  `a_nota_da_ferramenta_e_lida_pela_mesma_lista_do_estado` (`local/base.rs`)
  afirma `estado.notas == 0` com duas notas na pasta: a tela tem de dizer as duas
  coisas separadas, e o teste não finge a terceira.
- **A presa vive no índice.** Uma nota presa que nunca foi vetorada não entra em
  prompt nenhum (`memoria.rs:899`). Não existe caminho de texto puro que contorne
  o motor: a alternativa seria injetar no turno um conteúdo que nada mediu.
- **Nenhum pixel.** Nenhuma janela foi aberta. O que o `npm run build` prova é que
  o catálogo TypeScript fecha e o chunk do diálogo compila; o cartão com os três
  chips, o reler de um segundo durante a varredura e a cadeia `/base → memória`
  não foram vistos rodando.
- **A injeção ao vivo não foi medida.** `commands.rs:1044` é o ponto onde o bloco
  entra no prompt e o teste cobre a função, não a conversa. Um turno real com o
  diálogo aberto e pesos na placa — o `caso_base` na bancada — ficou de fora: sem
  codificador ele diria "não rodei" em vez de provar a presa no prompt, e
  levar o `Custodio` real para dentro de `bancada::rodar` é uma mudança de
  assinatura que merece a sua própria roda.

## 9. Fine-tuning: o que existe e o que não existe aqui

Esta frente entregou a parte de RAG do pedido. O lado de ajuste continua sendo o
que sempre foi documentado: **adaptadores montados, não pesos treinados.** O
aplicativo monta QLoRA/DoRA por turno sobre o GGUF (`ai::adaptadores`, roteiro em
`Retenção por adaptadores` — tarefa 38) e o teto é o que `DORA.md` mede: há um
leitor de adaptador e um veredito de alojamento, não um treinador. Ninguém aqui
gera gradiente, grava `.safetensors` ou escreve adaptador novo em disco; a base
de conhecimento melhora o agente pelo índice (itens 2 e 3), não pelo peso. Um
treinador local é tarefa própria, com placa medida antes — e enquanto ele não
existe, a palavra "fine-tuning" na interface é o nome da pasta de adaptadores,
nada mais.
