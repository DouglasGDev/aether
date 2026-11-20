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
canal era conferido, e `ConexaoRegistro` (`src/lib/types.ts:661`) não tinha para
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
- `src/lib/types.ts:655` e `:672` — `tlsVerificado` em `ConexaoBanco` (o que
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
