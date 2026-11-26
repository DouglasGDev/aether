# BANCOS — o canal conferido chega à tela

Frente: #95 (deixada pela metade pelo agente) e #99 (o buraco que a auditoria
do maintainer achou). Executada pelo maintainer na sessão de 2026-11-20.

## O que estava faltando

A flag `tls_verificado` já existia no cadastro (`src-tauri/src/bancos/config.rs:160`,
com `#[serde(default)]`, round-trip conferido pelos testes de
`config.rs:444/460/471`) e os motores já obedeciam:

| motor | onde lê a flag | o que faz com ela ligada |
|---|---|---|
| sqlserver | `motor/mssql.rs:209` (`confianca_da`) | passa a conferir o certificado em vez de aceitar qualquer um |
| mongo | `motor/mongo.rs:249` (`tls_da`) | `Tls::Enabled(TlsOptions::default())`, sem exceção de certificado |
| oracle | `motor/oracle.rs:176` (`string_de_conexao`) | prefixo `tcps://` na string Easy Connect |
| postgres | `motor/postgres.rs:160` (`ficha_de`) | `SslMode::Require` — e como este build só tem `NoTls`, a ligação **falha** em vez de deslizar para texto puro |
| mysql | — | **não havia leitura nenhuma**: a ligação ia adiante em claro |
| sqlite | — | arquivo local: não há canal a conferir |

O que faltava era a outra metade da promessa: nada, em `src/components/`,
`src/lib/api.ts` ou `locales/`, citava o campo. `ConexaoView`
(`src-tauri/src/commands.rs:475`) — a forma como a tela enxerga o cadastro —
não tinha o campo, então `bancos_listar` devolvia uma conexão sem dizer se o
canal era conferido, e `ConexaoRegistro` (`src/lib/types.ts:686`) não tinha para
onde levar a marcação do dono. O interruptor existia no disco e não existia na
interface: o motor obedecía a algo que ninguém podia ligar.

E havia o mysql: com a marca ligada, ele faria o que a doc do campo diz que não
faz — «o que não sabe se recusa a fingir».

## Vermelho medido

### 1. A vista não carregava o campo

Teste novo em `src-tauri/src/commands.rs` (`a_vista_carrega_o_canal_que_o_cadastro_marcou`),
escrito antes de mexer em `ConexaoView`. Serializa a vista e indexa o JSON, sem
tocar no tipo — por isso compila contra a árvore antiga e falha de verdade:

```
assertion `left == right` failed: a vista conta à tela o canal que o cadastro guarda:
  [{"id":"id-teste","nome":"teste","tipo":"postgres","host":"10.0.0.8","porta":null,
    "usuario":"rosto","banco":"estoque","arquivo":"C:/dados/estoque.db","temSenha":false,
    "limiteLinhas":200,"sqlLivre":false,"fontes":{}}]
  left: Null
 right: Bool(true)
test result: FAILED. 0 passed; 1 failed; ... 559 filtered out
```

A chave não existia no objeto — a tela não tinha como saber, nem como mostrar.

### 2. O mysql fingia

Teste novo em `src-tauri/src/bancos/motor/mysql.rs`
(`a_marca_de_canal_conferido_nao_vira_texto_puro`), chamando o `abrir` de
verdade com a marca ligada e host `127.0.0.1` — endereço que recusa na hora,
para que uma regressão de ordem falhe rápido em vez de esperar por uma rede:

```
a recusa tem de dizer por quê: não foi possível abrir a conexão com o banco:
Input/output error: Nenhuma conexão pôde ser feita porque a máquina de destino
as recusou ativamente. (os error 10061)
test result: FAILED. 0 passed; 1 failed; ... finished in 2.05s
```

Não é a recusa do canal: é o soquete. O `mysql_async` deste build foi compilado
com `features = ["minimal"]` (`src-tauri/Cargo.toml:60`), que não traz TLS
nenhum, então o `abrir` ignorava a flag e descia para o aperto de mão em texto
puro. O dono marcaria «canal conferido», a ligação pegaria, e nada disso seria
verdade.

O vermelho saiu como falha de asserção, não de compilação: a árvore estava
verde (548) antes de eu acrescentar os dois testes, então a falha é minha, não
herdada.

## O que mudou

**Núcleo**
- `src-tauri/src/commands.rs:489` — `ConexaoView` ganha `tls_verificado`, com o
  porquê escrito junto; preenchido em `commands.rs:509` a partir do cadastro.
  `bancos_listar`, `bancos_salvar` e `bancos_remover` passam pela mesma
  `vista_conexoes`, então os três devolvem o estado de uma vez.
- `src-tauri/src/bancos/motor/mysql.rs:484` — `conferir_canal`, chamado na
  primeira linha de `abrir` (`mysql.rs:178`), antes de qualquer rede: com a
  marca ligada, recusa com `erros.bancoSemTls` dizendo o tipo. Sem a marca,
  não nega nada (`mysql.rs:529`).

**Front**
- `src/lib/types.ts:680` e `:697` — `tlsVerificado` em `ConexaoBanco` (o que
  volta) e em `ConexaoRegistro` (o que vai). Obrigatório nos dois: `registro()`
  do dialog é checado pelo `tsc`, então não há como esquecer o campo em silêncio.
- `src/components/BancosDialog.tsx:402-418` — o toggle, no lugar de sempre
  (`field` + `field-label` + `toggle`/`toggle on` com `aria-pressed`), e só para
  tipo com servidor: SQLite é arquivo, e oferecer canal conferido para um
  arquivo seria um controle sem consequência.
- `src/components/BancosDialog.tsx:236` — a marca aparece na linha da conexão
  quando está ligada, do lado do chip de SQL livre (que fica em `aviso`, porque
  é risco; o TLS é proteção, então é chip comum).
- `src/components/BancosDialog.tsx:297` — trocar o tipo limpa a marca. Sem
  isso, escolher SQLite depois de marcar postgres deixaria salvo um estado que
  a tela não mostra mais.

**Idiomas** — `bancos.campoTls` (`locales/pt-BR.json:1127`,
`locales/en-US.json:1127`) e `erros.bancoSemTls` (`:1059` nos dois). Passam nos
testes de paridade (`todos_os_idiomas_tem_exatamente_as_mesmas_chaves`,
`toda_chave_pedida_pelo_codigo_existe_em_todo_idioma`).

## Verde medido

- `cargo test --lib` → **553 passed; 0 failed; 11 ignored; finished in 4.91 s**
  (eram 548 antes da frente: +1 em `commands`, +2 em `mysql`, +2 em `oracle`).
  Os novos, vistos por nome:
  ```
  test bancos::motor::mysql::tests::a_marca_de_canal_conferido_nao_vira_texto_puro ... ok
  test bancos::motor::mysql::tests::sem_a_marca_o_mysql_fala_como_sempre ... ok
  test bancos::motor::oracle::tests::a_marca_troca_so_o_protocolo_da_string ... ok
  test bancos::motor::oracle::tests::a_marca_nao_amassa_a_porta_dita ... ok
  test commands::tests::a_vista_carrega_o_canal_que_o_cadastro_marcou ... ok
  ```
- `npm run build` → `✓ built in 193 ms`, sem aviso de TypeScript e sem aviso de
  chunk; `BancosDialog` continua no próprio lazy chunk (`8.58 kB │ gzip: 2.33 kB`).

**ALEGADO, não medido:** o aperto de mão real com um servidor verdadeiro. Os
três testes de handshake dos motores (`postgres.rs:669`, `mssql.rs:683`,
`mongo.rs:564`) continuam `#[ignore]` — sem servidor não há negociação para
conferir, e um falso diria o contrário do que a rede faz. O que foi medido aqui
é a decisão (o que cada motor pede do driver) e o caminho até a tela, não o
byte na placa de rede.

**Cobertura que a auditoria achou faltando e fechou:** o oracle era o único
motor com a flag lida e nenhum teste — a linha da tabela acima era promessa do
código, não medição. Acrescentei `a_marca_troca_so_o_protocolo_da_string` e
`a_marca_nao_amassa_a_porta_dita` (`src-tauri/src/bancos/motor/oracle.rs`, no
fim do arquivo), que conferem a string dos dois lados (`10.0.0.8:1521/estoque`
× `tcps://10.0.0.8:1521/estoque`) e a porta dita (2484) com a marca ligada.
Verdes medidos, e o `Cargo.lock` confirma o resto da afirmação: a entrada de
`oracledb 26.0.0-beta.6` depende de `rustls` e `webpki-roots` sem feature
nenhuma a ligar.

## Consequência que fica aberta

O postgres está na mesa mas não entrega: `tokio-postgres` aqui (`Cargo.toml:59`)
só tem a feature `with-serde_json-1`, então o adaptador TLS é `NoTls` e a marca
ligada **impede** a conexão em vez de protegê-la. É honesto (nada roda em claro
achando que roda cifrado), mas não é o que o dono quer dizer quando liga o
interruptor. Fechar isso pede uma feature de TLS no driver — `rustls` via
`tokio-postgres-rustls`, em Rust puro, na mesma regra do resto, sem cliente
nativo — e um `MakeTlsConnect` próprio. Não é desta frente: muda dependências do
build e só teria verificação real com um Postgres de verdade na rede. Fica como
recomendação no `PLANO.md`.

---

# BANCOS — o caminho de leitura medido (2026-11-26)

Frente #115. Escrita começada por um agente que morreu no limite de turnos e
terminada pelo maintainer, que conferiu cada linha do diff antes de assumir o
que ele dizia. O recorte é o que a modelagem lê: `bancos_listar_fontes`,
`bancos_descrever_fonte`, `bancos_consultar`, `bancos_contar`. As escritas só
aparecem aqui porque passam pela mesma abertura de conexão.

## Os sete defeitos, cada um com a prova que o prende

Os números são os que aparecem nas asserções (`defeito N:`), porque um defeito
solto em prosa se resolve sozinho na memória de quem o consertou.

**1 — Cada chamada montava um runtime Tokio próprio.** `Runtime::new()` dentro de
`abrir` de cada motor assíncrono: a thread, o reactor e o timer eram pagos por
ferramenta, e o `block_on` de cada motor dependia do runtime que a própria
chamada criou. Hoje há um agendador para o processo inteiro
(`motor/mod.rs:86-111`, `OnceLock`, multi-thread, duas threads de trabalho, nome
`aether-bancos`), e os motores guardam `&'static Runtime`. A busca por runtime
próprio no diretório devolve uma linha só — o `Builder` de dentro de
`agendador()` — e as provas são `o_agendador_nasce_uma_vez_para_o_processo_inteiro`
(`motor/mod.rs:340`, com a régua `AGENDADORES_NASCIDOS` de `mod.rs:90`) e
`nenhum_motor_monta_o_proprio_agendador` (`motor/mysql.rs:549`). Dois threads de
propósito: o que mora nelas é tarefa de soquete que espera, e uma consulta por
vez é o que a ferramenta tem em voo.

**2 — A leitura pagava duas ou três idas ao catálogo.** Cada motor hoje tem um
`esquema_de` que devolve a tabela conferida **e** as colunas dela na mesma
viagem — `sqlite.rs:169` (um `sqlite_master × pragma_table_info`), `postgres.rs:234`,
`mysql.rs:293`, `mssql.rs:321`, `oracle.rs:239`. O que estava provado por
contador, não por opinião: `descrever_as_colunas_e_uma_viagem_s`
(`sqlite.rs:559`, `viagens == 1`), `a_leitura_faz_duas_viagens_e_devolve_as_colunas_da_fonte`
(`sqlite.rs:577`, `viagens == 2` — uma ao catálogo, uma ao dado), `a_contagem_faz_duas_viagens`
(`sqlite.rs:602`), e `a_ferramenta_de_leitura_faz_duas_viagens_no_arquivo`
(`ferramentas.rs:982`, que chama `executar("bancos_consultar", …)` e conta
declarações no arquivo de verdade). O contador é um `Cell` de thread local
pendurado no `preparar` do sqlite (`sqlite.rs:27`, zerado por `leitura_zerada`,
lido por `comandos_contados`), então o número é o que o driver executa, não o
que o autor do teste contou com o dedo. Nos motores de rede a mesma regra vira
teste de forma: `o_catalogo_de_uma_fonte_e_uma_ida_s` em `mssql.rs:641`,
`mysql.rs:586`, `oracle.rs:653`, `postgres.rs:666`.

**3 — `SELECT *`.** Onde não havia pedido de colunas, a leitura trazia o
asterisco — que pega o que a tabela ganhou depois da conferência e alarga a
pré-visualização sem que ninguém tenha pedido. `colunas_da_leitura`
(`sqlite.rs:197`) monta a lista explícita a partir do esquema conferido, e os
SQLs dos outros motores fazem o mesmo (`mysql.rs:193`, `postgres.rs:421`,
`mssql.rs:439`, `sqlite.rs:271`). A prova é literal: `sqlite.rs:591` exige que o
texto comece com `SELECT "id", "nome", "saldo" FROM "clientes"` e `sqlite.rs:595`
afirma que nenhuma das declarações da leitura contém `SELECT *`. Onde o
asterisco ainda aparece é no caminho do SQL livre do dono (`mysql.rs:484`), que
é o dono escrevendo, não a ferramenta.

**4 — Consulta pendurada segurava a conversa inteira.** Um prazo só,
`PRAZO_DE_LEITURA = 15 s` (`motor/mod.rs:23`), com `prazo_em_ms()` (`:25`) e
`prazo_de_espera() = prazo + 1 s` (`:49`) ao lado: quando os dois estão ligados,
o erro que chega primeiro é o do banco, que sabe o que expirou; o teto do
cliente é o que sobra para o servidor que nem responde ao cancelamento. Cada
motor aplica o que o próprio driver permite, e a diferença está escrita no
código, não disfarçada:

| motor | onde | o relógio |
|---|---|---|
| postgres | `postgres.rs:185` | viaja na ficha como `-c statement_timeout=15000` — morre no servidor |
| oracle | `oracle.rs:230` | `set_call_timeout(PRAZO_DE_LEITURA)` no motorista |
| mongo | `mongo.rs:411-414` + `:431` | `.max_time(PRAZO_DE_LEITURA)` na consulta — o `maxTimeMS` do servidor — **e** a espera de `prazo_de_espera()` cobrindo a busca e o despejo do cursor na mesma viagem |
| mysql | `mysql.rs:220-235` | só o prazo do cliente: o driver não expõe prazo de leitura e `MAX_EXECUTION_TIME` é do MySQL, não da MariaDB que este motor atende |
| sqlserver | `mssql.rs:264-279` | só o prazo do cliente: o `tiberius` 0.12 não tem `set_query_timeout` |
| sqlite | `sqlite.rs:18/67` | arquivo local: o que espera é lock (`busy_timeout` de 5 s), não rede |

A forma `esperar_prazo(runtime, prazo, futuro)` existe nos quatro motores
assíncronos (`mongo.rs:261`, `mssql.rs:264`, `mysql.rs:220`, `postgres.rs:193`) e
cria o `Sleep` **dentro** do `block_on` — fora do runtime o `tokio::time::timeout`
panica, e isso é medido por `a_espera_nao_panie_fora_do_agendador`
(`postgres.rs:685`). As outras provas: `uma_ida_que_passa_do_prazo_nao_vira_espera_infinita`
(`mongo.rs:555`, `mssql.rs:669`, `mysql.rs:629`), `a_espera_do_cliente_fica_acima_do_prazo`
(`mssql.rs:692`, `mysql.rs:655`), `a_ficha_leva_o_prazo_para_o_servidor`
(`postgres.rs:653`, confere a string `-c statement_timeout=15000`). O erro que o
modelo lê carrega o número: `erro_de_prazo` (`mod.rs:31`) publica
`erros.tempoEsgotado` com `{{segundos}}`, provado por
`o_prazo_aparece_na_mensagem_que_o_modelo_le` (`mod.rs:330`).

**5 — O corte era invisível.** Ler 200 linhas com um banco de 5 000 e dizer
`truncado: false` mentia sobre o que o modelo viu. O teto agora é
`limite + 1` (`teto_de_leitura`, `mod.rs:41`), a linha a mais é a prova de que
sobrou banco, `colher_ate` (`mod.rs:57`) drena o cursor assíncrono até o teto e
`aparar_no_teto` (`mod.rs:74`) aparando devolve `(linhas, truncado)`. Quem drena
recebe o «próximo» como fecho, não o cursor, para que a medida exista sem
servidor: `o_teto_de_leitura_deixa_uma_linha_de_barriga` (`mod.rs:309`, com o
`saturating_add` nos dois cantos), `a_leitura_pede_uma_linha_de_barriga`
(`mysql.rs:614`, o SQL termina em `LIMIT 21` para limite 20 e `LIMIT 1` para
limite 0), `a_leitura_para_no_teto_e_avisao_quando_sobra_linha` (`sqlite.rs:615`,
cortou-e-avisou e acabou-na-quinta-e-não-avisou), `a_leitura_para_no_teto…` no
mongo por `.limit(teto_de_leitura(limite))` (`mongo.rs:413`).

**6 — Serializar copiava a grelha duas vezes.** `json!({...})` materializava cada
`Vec<Value>` da consulta num `Value` novo para então serializar a cópia — e a
mesma grelha ainda era movida para a vista. Agora há um tipo que serializa por
referência, `ConsultaSerializada<'a>` (`ferramentas.rs:351-357`), usado por
`texto_da_consulta` (`ferramentas.rs:359`). A prova tem duas metades e as duas
importam: `a_saida_da_leitura_nao_copia_a_grelha` (`ferramentas.rs:951`)
constrói a saída antiga como referência e exige **texto igual byte a byte**
(nada do que o modelo lê mudou de forma) e **≤ 32 alocações** para uma grelha de
200×8. As alocações saem de uma régua `GlobalAlloc` local à linha
(`ferramentas.rs:900-945`), ligada só durante a medida — os outros testes correm
em paralelo no mesmo processo e nada neles tem a ver com o que está sendo
medido aqui; a régua delega tudo ao sistema e não toca em memória própria.

**7 — Ler escrevia.** A ferramenta de leitura abria a mesma conexão que a de
escrita. Hoje cada ferramenta pede a operação com que abre —
`motor::abrir_para(&conexao, Operacao::…)` (`mod.rs:277`), chamada com
`Operacao::Ler` em `ferramentas.rs:305`, `:390`, `:402` e com a operação devida
nas escritas (`:433`, `:457`, `:480`, `:511`). No sqlite a alça de leitura liga
`PRAGMA query_only` (`sqlite.rs:76`), e as duas provas não são a mesma:
`a_alca_de_leitura_nao_escreve_no_journal_padrao_nem_no_wal` (`sqlite.rs:516`)
confere os dois modos de journal — no `wal` a escrita recusada é outra e o
`query_only` precisa valer lá também — e
`a_leitura_da_ferramenta_nao_deixa_rastro_no_arquivo` (`ferramentas.rs:1008`)
roda a ferramenta e compara o arquivo antes e depois. A escrita pelo
`bancos_sql` continua onde o dono ligou o SQL livre: a trava é da alça, não do
banco.

## A conferência que continua valendo depois do `esquema_de`

Os três testes que conferem filtro e ordem na mesma viagem existem porque o prazo
e o teto só valem se o identificador continua vindo do esquema e o valor do
parâmetro: `a_fonte_e_a_coluna_volvem_na_caixa_do_banco` (`sqlite.rs:684`),
`o_filtro_vai_na_mesma_viagem_com_o_valor_ligado` (`sqlite.rs:705`),
`o_nulo_do_filtro_e_is_null_na_mesma_viagem` (`sqlite.rs:730`),
`o_nome_que_se_pede_ao_dicionario_e_cru` (`oracle.rs:678`) e
`o_catalogo_e_o_nome_estatico_dizem_a_mesma_coisa` (`ferramentas.rs:597`).
Nenhum deles é novo nesta frente; estão aqui porque a mudança de `esquema_de`
toca o caminho por onde passam.

## O que **não** foi medido

- **Nenhuma consulta contra servidor.** As provas de round-trip com contador
  existem só no sqlite, que é arquivo local aberto pelo teste. Para postgres,
  mysql, sqlserver, mongo e oracle o que foi medido é a **declaração** (o SQL
  montado, a string da ficha, o `LIMIT`/`TOP`, o `statement_timeout` dentro da
  ficha) e a **forma da espera** — nunca uma ida real. Um `TOP (201)` que o
  SQL Server não aceita como escrito apareceria só na rede.
- **Tempo de parede nenhum.** Não há antes/depois em milissegundos nesta frente:
  as magnitudes afirmadas são contagens (viagens, alocações) e a asserção de
  igualdade de texto. Dizer «a leitura ficou X vezes mais rápida» seria
  inventado.
- **O prazo do oracle está `#[ignore]`.** `a_conexao_abre_com_o_prazo_do_motor`
  (`oracle.rs:693`) precisa de um Oracle real para ler o `call_timeout` de volta;
  o que está provado é a chamada em `oracle.rs:230`, não o efeito nela.
- **Nenhuma tela.** A janela de leitura é o mesmo `BancosDialog` de antes; nada
  aqui mudou UI, e nada foi aberto.

## Verde medido (frente de leitura)

As três portas correram nesta árvore por último — depois de fechado o último
arquivo desta frente e de um comentário corrigido em `local/web.rs` pela frente
de rede. O `CONTRIBUTING.md` cobra `cargo test --lib` e `npm run build` (itens 2 e
3); o `cargo check --all-targets` sem advertência é o padrão da casa:

```text
$ cd "C:/Aether AI/src-tauri" && cargo test --lib
test bancos::motor::tests::o_agendador_nasce_uma_vez_para_o_processo_inteiro ... ok
test bancos::motor::mysql::tests::nenhum_motor_monta_o_proprio_agendador ... ok
test bancos::motor::sqlite::tests::descrever_as_colunas_e_uma_viagem_s ... ok
test bancos::motor::postgres::tests::a_espera_nao_panie_fora_do_agendador ... ok
test bancos::ferramentas::tests::a_saida_da_leitura_nao_copia_a_grelha ... ok
test bancos::ferramentas::tests::a_leitura_da_ferramenta_nao_deixa_rastro_no_arquivo ... ok

test result: ok. 685 passed; 0 failed; 12 ignored; 0 measured; 0 filtered out; finished in 7.36s

$ cd "C:/Aether AI/src-tauri" && cargo check --all-targets
    Checking aether v0.1.0 (C:\Aether AI\src-tauri)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.59s

$ cd "C:/Aether AI" && npm run build
vite v8.3.8 building client environment for production...
✓ 307 modules transformed.
✓ built in 171ms
```

Recortes, contados na mesma saída (não no dedo de quem escreve):

- `bancos::` = **71 casos anunciados → 67 verdes + 4 ignorados**. Os quatro
  ignorados são justamente as pontas que pedem servidor na rede:
  `postgres::com_a_flag_ligada_texto_puro_nunca_e_aceito`,
  `mssql::um_servidor_auto_assinado_e_recusado_com_a_flag_ligada`,
  `mongo::tls_conferido_so_falha_com_servidor_verdadeiro` e
  `oracle::a_conexao_abre_com_o_prazo_do_motor` — o mesmo oracle listado no
  «O que **não** foi medido» acima. sqlite, que é arquivo local, não tem
  ignorado nenhum: as dez asserções dele correm todas.
- por módulo, os anunciados: `bancos::motor::` **45** (41 dentro dos seis
  motores + 4 no módulo comum de agendador e prazo), `bancos::ferramentas::`
  **15**, `bancos::config::` **11** — os três somam os 71. Dos 41 dos seis
  motores, **4** estão ignorados (postgres, mssql, mongo, oracle); os verdes
  saem sqlite **10**, mysql **7**, postgres **6**, mssql **5**, mongo **5**,
  oracle **4**, com `ferramentas::` e `config::` inteiros (15 e 11) correndo.
  sqlite é o único dos seis sem ignorado nenhum: as dez asserções dele correm
  todas, porque arquivo local não aperta mão de TLS.
- `cargo check --all-targets` com **zero** linhas `warning`.

O `685 passed` acima é a suíte inteira, com os dois testes da frente de CPU
(`ai::ggml::ffi::testes::a_caminhada_le_a_sequencia_pelo_tamanho_de_cada_registro`,
`a_contagem_real_fecha_com_a_sequencia_da_api`) já dentro; o número próprio desta
frente não é esse, é o recorte `bancos::` logo acima — que é o que muda se um
destes sete defeitos voltar.
