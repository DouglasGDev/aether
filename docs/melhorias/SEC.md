# SEC — contenção e injeção (registro da frente)

Lote de 2026-11-20, brief em `PLANO.md`, regras de `CONTRIBUTING.md`.
Arquivos desta frente (um escritor por arquivo): `src-tauri/src/local/arquivos.rs`,
`src-tauri/src/mcp/execucao.rs`, `src-tauri/src/bancos/motor/mongo.rs`,
`src-tauri/src/bancos/ferramentas.rs`. Nenhum outro arquivo de código foi tocado.

**Verificação:** por leitura + testes escritos + raciocínio de tipos/borrow, como
manda a regra 7 do PLANO (agents não rodam `cargo`/`npm`). Em cada arquivo os
testes foram escritos ANTES da correção (estado vermelho estabelecido pela
assertiva que o código antigo viola; o gate central confirma). Nada simulado:
toda recusa nova devolve erro real com chave de i18n existente.

---

## S1 [HIGH] Escape por symlink em `buscar`/`arvore` + raiz renomeável em `mover`

### O que foi lido para confirmar o defeito

- `arquivos.rs:239` (antes): `arvore()` usava `item.metadata()`. No Windows o
  `DirEntry::metadata` NÃO segue symlink: um link-para-arquivo entrava como
  arquivo na lista de candidatos.
- `arquivos.rs:279` (antes): ramo não-recursivo filtrava com `p.is_file()`, que
  SEGUE o link — o mesmo escape pela outra porta.
- `arquivos.rs:289,293` (antes): `fs::metadata`/`fs::read` no laço de leitura
  seguem o link e liam o alvo FORA da pasta concedida, sem nunca passar por
  `pastas.resolver`. `buscar_texto` é ReadOnly (`local/mod.rs`), então no Plan
  roda sem aprovação: exfiltração silenciosa.
- `local/pastas.rs:127-174`: `resolver` canonicaliza o ancestral existente
  (seguindo links), remove o prefixo verbatim `\\?\` e exige
  `alvo.starts_with(raiz)` — re-resolver um candidato é exatamente a contenção
  que faltava. `raizes` guarda caminhos reais canonicalizados sem verbatim
  (`conceder`, `pastas.rs:95-117`).
- `arquivos.rs:524` (antes): `apagar` já recusava a raiz concedida
  (`pastas.raizes.contains`); `mover` (`:492-504` antes) não — renomear a raiz
  revogaria a concessão por dentro.

### O que mudou

- `arquivos.rs:239-246` — `arvore()` consulta `item.file_type()` (vê o link em
  si, sem seguir) e pula entradas `is_symlink()` antes de qualquer metadata.
  No Windows isso também barra junctions/mount points (reparse points).
- `arquivos.rs:287-289` — ramo não-recursivo troca `p.is_file()` por
  `p.symlink_metadata().map(|m| m.is_file())`: o link em si não é arquivo.
- `arquivos.rs:299-302` — cada candidato é re-resolvido por
  `pastas.resolver(&arquivo.to_string_lossy())` antes de `fs::metadata`/`fs::read`.
  Fecha a janela TOCTOU entre listar e ler: link que aponta para fora é
  canonicalizado e recusado (`erros.foraDasPastas`); link para dentro da
  concessão resolve para o alvo contido e segue legítimo. Como `alvo` já vem
  canonicalizado do resolver/`raiz_de_trabalho`, o caminho exibido nos achados
  não muda para arquivos normais.
- `arquivos.rs:508-516` — `mover` recusa `origem` que seja raiz concedida
  (`pastas.raizes.contains(&origem)`), com a mesma chave `erros.raizProtegida`
  de `apagar`. O destino já não pode existir (`:518`), então raiz como destino
  já era recusada.

### Testes vermelho-primeiro (escritos antes da correção)

- `buscar_nao_le_link_simbolico_que_aponta_para_fora` (`arquivos.rs:974`):
  pasta concedida + `segredo.txt` FORA com "SEGREDOFORA"; symlink de arquivo
  dentro da raiz apontando para ele. Assertiva: para `recursivo` true E false,
  `total == 0`, `achados.is_empty()` e o texto não contém "SEGREDOFORA".
  Vermelho antes: nos dois ramos o link era lido e o termo achado (Windows:
  `item.metadata` não segue → entra como arquivo; Unix: segue → entra como
  arquivo; `is_file()` segue nos dois).
- `buscar_nao_desce_em_link_de_pasta_que_aponta_para_fora` (`arquivos.rs:1015`):
  symlink de PASTA para fora, com `sub/segredo.txt`. Assertiva: `total == 0` e
  texto sem "SEGREDOFORA". Vermelho antes no Unix (a travessia descia pelo link);
  no Windows o código antigo já não lia (metadata do link não é dir → entra como
  arquivo → `fs::read` de diretório falha), mas o teste vigia a regressão.
- `mover_a_propria_raiz_concedida_e_recusado` (`arquivos.rs:1046`): assertiva
  `mover(origem=raiz, destino=fora)` é `Err` contendo "pasta concedida", a raiz
  continua existindo e o destino não nasce. Vermelho antes: o rename acontecia.
- Guarda de privilégio: `criar_link` (`arquivos.rs:949`) usa
  `std::os::windows::fs::symlink_file/symlink_dir` (Unix: `symlink`). Se a
  criação falhar por falta de privilégio (Windows sem modo desenvolvedor), o
  teste NÃO finge sucesso: ainda afirma que `pastas.resolver` recusa o alvo do
  link (a mesma regra de contenção) e retorna — mesmo padrão de
  `pastas.rs:323 link_simbolico_para_fora_e_recusado`. Os links são encenados
  sobre `pastas.raizes[0]` (caminho real sem `\\?\`), não sobre o canonicalize
  verbatim que `cenario()` devolve.

### i18n

Nenhuma chave nova. Reuso: `erros.foraDasPastas` (recusa do resolver, já
existente no caminho) e `erros.raizProtegida` no `mover`.

---

## S3 [MED] Detector de comando composto ignorava separadores do cmd.exe

### O que foi lido para confirmar o defeito

- `execucao.rs:248-253` (antes): o scan só flagrava `| ; && $( ` >`.
  `cmd /C "echo ok & payload"` passava como não-composto; `argumentos_contem_composto`
  alimenta `gate.decide(..., compound)` (`execucao.rs:121-126`), e o compound é
  o que revoga o benefício do grant de sessão (`permissions/mod.rs:54-55`, lido
  sem editar). Faltavam `&` simples, `\n`, `\r`, `<`, `^` (escape do cmd) e `%`
  (expansão `%VAR%`).

### O que mudou

- `execucao.rs:245-263` — o scan agora flagra também `&` (que cobre `&&`, por
  isso o teste literal `"&&"` foi substituído pelo caractere simples), `<`,
  `\n`, `\r`, `^`, `%`. `$(` e crase continuam. Comentário acima da função
  explica o porquê (separadores do cmd.exe, não só do sh). Comando único sem
  separadores continua simples — o detector é conservador por desenho: um
  `%` legítimo (ex.: `echo 100%`) passa a reperguntar, nunca a executar sem
  prompt; esse é o lado seguro da troca.

### Testes vermelho-primeiro

- `separadores_do_cmd_tambem_tornam_composto` (`execucao.rs:327`): para cada um
  de `"echo ok & payload"`, `"echo ok\npayload"`, `"echo ok\rpayload"`,
  `"type saida < entrada.txt"`, `"echo ^escapado"`, `"echo %PATH%"`, assertiva
  `argumentos_contem_composto(...) == true`. Vermelho antes: todos passavam
  como simples.
- `comando_unico_sem_separadores_continua_simples` (`execucao.rs:344`):
  `"git status --short"` continua não-composto (não quebra comando legítimo).
- Os testes antigos do arquivo (`pipe_...`, `substituicao_...`,
  `a_deteccao_..._listas_e_objetos`, `argumentos_simples_...`) foram relidos e
  permanecem verdadeiros com o conjunto novo (`"a && b"` casa via `&`).
  `permissions/mod.rs` recebe `compound: bool` direto nos testes — nenhum
  fixture de outro arquivo depende do scan antigo (grep conferido).

### i18n

Nenhuma chave nova (o detector não emite string; o re-prompt usa o fluxo existente).

---

## S4 [MED] Injeção de operador Mongo por valor de filtro com chave `$`

### O que foi lido para confirmar o defeito

- `mongo.rs:120-141` (antes): `json_para_bson` convertia QUALQUER objeto em
  subdocumento BSON, inclusive chaves `$`. Um valor `{"$ne": null, "x": 1}`
  virava documento de operadores → consulta sempre-verdadeira em
  consultar/atualizar/apagar → update/delete em massa dentro da tabela autorizada.
- `motor/mod.rs:85-116` (lido, NÃO editado — arquivo de ninguém desta frente):
  `filtro_de` só recusa objeto de UMA chave sem `"op"`; objeto com 2+ chaves
  (`{"$ne":null,"x":1}`) cai no ramo `outro => (Igual, outro.clone())` e chega
  inteiro ao motor. Portanto a correção tinha de estar no motor, e
  `json_para_bson` (mongo.rs, desta frente) é o ponto por onde TODOS os valores
  passam (filtro, inserir, atualizar). Nenhuma mudança em mod.rs é necessária.
- `sqlite.rs:418` (lido): os demais motores ligam objeto como texto JSON
  parametrizado (`outro.to_string()`), sem semântica de operador — o defeito é
  específico do Mongo; nada a fazer nos outros motores.
- `conferir_campo` (`mongo.rs:36-42`) já recusava `$` no NÍVEL DE CIMA com
  `erros.bancoColunaInexistente`; a recusa nova segue o mesmo padrão de erro.

### O que mudou

- `mongo.rs:120-150` — `json_para_bson` agora devolve `Result<Bson>` e recusa
  chave com `$` inicial em QUALQUER profundidade (objetos aninhados e dentro de
  arrays), com erro real nomeando a chave (`erros.bancoColunaInexistente`,
  mesma chave que `conferir_campo` já usa para `$`). Operadores legítimos que o
  próprio motor constrói (`$ne`, `$gt`, `$regex` em `filtro_bson`) não passam
  por `json_para_bson` — continuam funcionando.
- Propagação do `?` em todos os call sites: `filtro_bson` (`:85,:92,:111`),
  ramo Array (`:131`, `collect::<Result<Vec<_>>>()?`), `documento_de` (`:171`)
  e `atualizar` (`:440`). Grep conferiu que não sobrou chamada sem `?`.

### Testes vermelho-primeiro (módulo `tests` novo — o arquivo não tinha)

- `filtro_com_operador_embutido_nao_vira_consulta` (`mongo.rs:519`): o teste
  que compila contra o código ANTIGO e falha na assertiva — `filtro_de` +
  `filtro_bson` com `{"campo":{"$ne":null,"x":1}}` e com
  `{"campo":{"op":"igual","valor":{"$ne":null}}}`; assertiva `is_err()`.
  Vermelho antes: `filtro_bson` devolvia `Ok` com o documento de operadores.
- `chave_de_operador_no_valor_e_recusada_em_qualquer_profundidade`
  (`mongo.rs:506`): `json_para_bson({"$ne":null})` é `Err` contendo "$ne";
  aninhado `{"a":{"b":{"$gt":1}}}` e em array `[{"$exists":true}]` são `Err`;
  `documento_de({"campo":{"$ne":null,"x":1}})` é `Err`. (Contra a assinatura
  antiga este teste nem compila — `unwrap_err` em `Bson`; o vermelho-primeiro
  compilável é o teste acima, do `filtro_bson`.)
- `objeto_sem_operador_continua_entrando_como_subdocumento` (`mongo.rs:529`):
  `{"a":1,"b":{"c":"x"}}` continua `Ok` (Int64 conferido) e filtro com valor
  objeto sem `$` continua `Ok` — a recusa é do `$`, não de valor composto.

### i18n

Nenhuma chave nova. Reuso: `erros.bancoColunaInexistente` ("a tabela não tem a
coluna {coluna}"), o mesmo padrão que `conferir_campo` já aplica a nomes `$`.
A redação não é perfeita para um valor aninhado (ver follow-up opcional abaixo).

---

## LOW `colunas` divulgava esquema de tabela não-legível

### O que foi lido para confirmar o defeito

- `ferramentas.rs:301` (antes): `colunas` usava `fonte_exposta` (só existência
  no cadastro), enquanto `consultar`/`contar` (`:367,:384` antes) usam
  `fonte_pedida(..., Operacao::Ler)`, que cobra `fonte.ler` via
  `Fonte::permite` (`config.rs:87-94`). Tabela exposta com `ler=false` tinha o
  esquema descrito mesmo assim.

### O que mudou

- `ferramentas.rs:299-304` — `colunas` passa a usar
  `fonte_pedida(&conexao, args, Operacao::Ler)?`, exatamente o padrão das
  demais leituras. Recusa antes de o motor abrir, com
  `erros.bancoOperacaoNegada` existente. `fonte_exposta` continua em uso por
  `fonte_pedida` (sem dead code).

### Testes vermelho-primeiro

- `colunas_de_fonte_nao_legivel_sao_recusadas` (`ferramentas.rs:801`): bancada
  SQLite real (mesmo fixture dos testes vizinhos), `fonte.ler=false` salvo no
  `bancos.json`; assertiva: `executar("bancos_colunas", ...)` é `Err` contendo
  "ler" e "clientes". Vermelho antes: descrevia o esquema (`expect_err`
  panicava). O caminho positivo segue coberto pelo teste existente
  `colunas_da_fonte_exposta_vem_do_banco_real` (Fonte::default tem ler=true).

### i18n

Nenhuma chave nova. Reuso: `erros.bancoOperacaoNegada` (via `fonte_pedida`).

---

## Chaves de i18n para o maintainer

**Nenhuma chave nova é obrigatória.** Todas as recusas reutilizam chaves
existentes com paridade pt-BR/en-US já conferida.

Opcional (redação, não bloqueia o gate): as duas reusadas têm texto calibrado
para outro contexto —
- `erros.raizProtegida` diz "não pode ser apagada por dentro"; agora também
  cobre o `mover`. Se o maintainer quiser texto próprio:
  chave `erros.raizProtegidaMover`,
  pt-BR: `"{caminho} é uma pasta concedida e não pode ser renomeada por dentro"`,
  en-US: `"{caminho} is a granted folder and can't be renamed from within"`.
- `erros.bancoColunaInexistente` diz "a tabela não tem a coluna {coluna}";
  agora também nomeia chave `$` recusada em valor. Se quiser texto próprio:
  chave `erros.bancoOperadorProibido`,
  pt-BR: `"chave iniciada por $ é operador do banco e não entra como valor: {coluna}"`,
  en-US: `"a key starting with $ is a database operator and is not accepted as a value: {coluna}"`.
Sem essas chaves o comportamento já é correto e a recusa é real; ficam como
sugestão de clareza, não como dependência.

## Follow-ups deixados

1. **`motor/mod.rs` `filtro_de`** (arquivo fora desta frente): objeto de uma
   chave sem `"op"` já é recusado lá; o de 2+ chaves chega como igualdade e
   agora é contido pelo `json_para_bson`. Uma recusa genérica de chave `$` no
   `filtro_de` daria defesa em profundidade para todos os motores, mas não é
   necessária para fechar este defeito (os outros motores parametrizam objeto
   como texto — `sqlite.rs:418` lido). Registrado como recomendação.
2. **Redação das chaves reusadas** (opcional, acima).
3. **TLS de mssql/postgres**: fica como recomendação do PLANO (flag opt-in por
   conexão + UI); nada mudado nesta frente.
4. `listar` continua exibindo entradas symlink como itens (metadata do link,
   sem seguir, sem ler conteúdo) — não há leitura de conteúdo fora da concessão
   por ele; não tocado para não refatorar código fora do defeito.

## Estado

Quatro defeitos corrigidos nos quatro arquivos da frente, cada um com teste
escrito antes da correção. Gate central (`cargo test --lib` + `npm run build`)
NÃO foi rodado por esta frente, como manda a regra 7 do PLANO — nada aqui é
dado como verde sem o log do maintainer.