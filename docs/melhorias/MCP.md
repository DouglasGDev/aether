# MCP — as duas portas do mesmo cadastro

Dono: maintainer. Frente sobre o servidor MCP que o próprio aplicativo publica e
sobre o cliente que instala os servidores do catálogo. Três defeitos, cada um
visto vermelho antes da correção.

## M1 — um npm que só fala em stderr prendia o instalador para sempre

**Defeito (lido).** `mcp/instalador.rs` lançava o `npm` com stdout e stderr em
canos, lia as linhas do stdout até o fim e **só depois** chamava
`stderr.read_to_string`. O cano de stderr cabe alguns kilobytes: quando o
processo despeja mais que isso, ele para de escrever até alguém ler — e quem
deveria ler estava esperando o `wait()`, que nunca voltava. Um pacote que emite
avisos suficientes travava a instalação indefinidamente, sem cancelamento
possível pelo caminho do `wait`.

**Correção.** A leitura do despejo passa a correr ao lado do laço de stdout
(`instalador.rs:311`, tarefa `tokio::spawn` sobre o `stderr` tomado antes de o
filho ser esperado), e o texto é colhido depois do `wait()` com
`derrame.await.unwrap_or_default()` (`instalador.rs:345`). O cano fecha quando o
processo morre, então a espera termina sozinha; uma tarefa que estourou devolve
stderr vazio e a mensagem volta a cair na saída padrão, o caminho de antes quando
não havia nada para ler. No ramal de cancelamento a tarefa é abortada e o filho
morto antes do `bail!` (`instalador.rs:320-325`).

**Medido.** Vermelho (leitura depois do `wait()`, filho despejando ~1 MB de
stderr): o teste ficou **10,01 s** pendurado e a asserção acusou
`o instalador ficou 10 s pendurado num processo que só despejou stderr` — o teto
de 10 s do próprio teste, não o tempo do instalador. Verde, hoje, com o mesmo
teste: `cd src-tauri && cargo test --lib mcp::instalador` → **19 passaram, 0
falharam, 0 ignored, finished in 0.06 s**.
Teste: `mcp::instalador::testes::um_filho_que_inunda_o_stderr_nao_prende_o_instalador`.

## M2 — o que o grafo desliga continuava vivo na placa publicada

**Defeito (lido).** As ferramentas locais têm duas portas: o laço de conversa
(`computador__ler_arquivo`) e o servidor MCP publicado (`aether__ler_arquivo`).
A allowlist desenhada na tela do grafo vive no cadastro do conector embutido e
era conferida só pelo laço, em `mcp/manager.rs::permitidas` e
`ToolMeta::resolve`. `mcp/servidor.rs` resolvia o efeito e montava o anúncio a
partir de `catalogo_publicado()`, que não consultava cadastro nenhum: com
`computador` recortado para duas ferramentas, o servidor anunciava as 21 e
aceitava a escrita desligada. Não era um segundo gate — era a ausência dele: um
`FullAccess` do cliente bastava para rodar o que o usuário fechou.

**Correção.** O cadastro passa a ser um só objeto dividido com a execução:
`McpManager` guarda `Arc<RwLock<Registry>>` (`manager.rs:80`) e o entrega à
`Execucao` no construtor (`manager.rs:138`), campo declarado em
`execucao.rs:30`. A leitura é `Execucao::permite_local` (`execucao.rs:112`), que
confere a allowlist do conector embutido e responde `false` quando o cadastro não
tem o conector — a mesma resposta de `manager::permitidas`, para o mesmo ato não
ter duas. O servidor usa isso nos três lugares onde fala do catálogo:

- `catalogo_publicado(&execucao)` filtra (`servidor.rs:358`);
- `ferramentas_anunciadas(&execucao)` anuncia o recorte (`servidor.rs:368`);
- `call_tool` resolve o efeito no recorte (`servidor.rs:313`): nome desligado cai
  no ramo que já existia — `erros.ferramentaInexistente`, com a lista das
  ferramentas realmente ligadas. Nenhuma chave de i18n nova, nenhum caminho novo,
  e a lista que devolve o nome errado não entrega capacidade desligada.

A contagem que a tela lê (`Estado.ferramentas`) sai do mesmo recorte
(`servidor.rs:125`, usado em `abrir` e no estado inativo), porque um número que
não bate com o `tools/list` diria "ligada" uma ferramenta que o grafo fechou.

**Medido.** Vermelho, com o cadastro recortado para `["ler_arquivo",
"info_sistema"]`:

```
desligada no grafo não é anúncio: ["ler_arquivo", "listar_pasta", "buscar_texto",
"buscar_memoria", "ler_resultado", "escrever_arquivo", "editar_arquivo",
"criar_pasta", "mover", "apagar", "executar_comando", "info_sistema",
"bancos_listar", "bancos_tabelas", "bancos_colunas", "bancos_consultar",
"bancos_contar", "bancos_inserir", "bancos_atualizar", "bancos_apagar",
"bancos_sql"]
```

21 anunciadas onde o grafo deixou 2, e a escrita chegava ao disco. Verde, hoje:
a mesma asserção passa, o `call_tool` de `escrever_arquivo` responde
`is_error = true` e o arquivo não existe. Testes novo:
`o_recorte_do_grafo_vale_na_placa_publicada` (pelo protocolo, cliente real na
placa de retorno) e `a_contagem_do_estado_e_o_recorte_do_grafo` na frente do
servidor, mais `a_allowlist_do_grafo_decide_o_que_a_execucao_aceita` e
`sem_cadastro_nao_ha_permicao` na execução.
`cd src-tauri && cargo test --lib mcp::servidor` → **11 passaram, 0 falharam,
finished in 0.03 s**; `cargo test --lib mcp::execucao` → **10 passaram, 0
falharam, finished in 0.01 s**.

## M3 — duas escritas concorrentes podiam publicar o corpo uma da outra

**Defeito (lido).** `local/arquivos.rs::escrever_atomico` gravava
`<tronco>.aether-tmp` ao lado do alvo e renomeava. O tronco é o nome sem
extensão, então `LEIA-ME.md` e `LEIA-ME.txt` escreviam no **mesmo** temporário —
e o chat e um cliente MCP na placa de retorno chamam a mesma ferramenta, ao mesmo
tempo. Quem renomeasse primeiro publicaria o corpo da outra.

**Correção.** `temporario_de` (`arquivos.rs:361`) mantém o nome inteiro do alvo —
para um `aether-tmp` perdido na pasta dizer de que arquivo ele era — e acrescenta
pid e um contador estático (`arquivos.rs:355`, `AtomicU64`), único por escrita
dentro do processo. O nome continua derivado do alvo, nunca sorteado às cegas, e
um alvo sem extensão também tem temporário ao lado.

**Medido.** Vermelho: `assert_eq!` dos dois caminhos, e os dois eram literalmente
`p/LEIA-ME.md.aether-tmp`. Verde, hoje:
`cd src-tauri && cargo test --lib local::arquivos` → **32 passaram, 0 falharam,
finished in 0.04 s**. Testes:
`dois_alvos_com_o_mesmo_tronco_tem_temporarios_diferentes` (inclui duas escritas
no mesmo caminho) e `um_alvo_sem_extensao_tem_temporario_ao_lado`; o teste antigo
que conferia um nome fixo virou uma varredura da pasta atrás de qualquer
`aether-tmp` sobrando.

## M4 — um servidor mudo pendurava o botão de conectar para sempre

**Defeito (lido).** `conectar` esperava o aperto de mão e a descoberta de
ferramentas sem relógio nenhum. O laço do rmcp delimita essa espera só pelo
`CancellationToken` (`serve_client_with_lifecycle_and_ct` não tem prazo embutido),
então um servidor que nasce sem falar — o script certo lendo uma entrada que nunca
chega, o binário que abre o socket e fica quieto — deixava a tela girando para
sempre, sem erro nenhum. Não é hipótese: é o caminho que um `npx` mal configurado
faz por padrão.

A nota de reconhecimento que vinha com esta frente alegava outra coisa — que uma
conexão fracassada **vazava um `node.exe` vivo**. **Falso**, lido no fonte
instalado (rmcp 3.10.0, `transport/child_process.rs`): a futura stdio carrega o
`TokioChildProcess`, e o `Drop` dele (`ChildWithCleanup`) mata o filho. Registrar
assim para ninguém "consertar" o que não existe; o defeito real era a falta de
prazo.

**Correção.** `PRAZO_CONEXAO` de 45 s (`manager.rs:721`) — folgado o bastante
para um `npx` que precisa baixar o pacote, apertado o bastante para não ser a
tela girando. `dentro_do_prazo` (`manager.rs:731`) recebe o prazo como parâmetro
para a conta poder ser conferida em teste sem esperar 45 s reais, e o erro diz
**qual** servidor ficou calado (`erros.servidorSemResposta`, nos dois idiomas).
Coberto o `dial` (`manager.rs:354`) e a descoberta (`:361`); se a descoberta
estoura, o cliente é cancelado antes de devolver o erro. Na saída do aplicativo,
`desligar` (`manager.rs:633`) derruba o servidor publicado e cada conector, e
entra no gancho de `RunEvent::Exit` com teto próprio de 5 s (`lib.rs:241-252`)
para um servidor teimoso não segurar a janela aberta.

Que o gancho é chamado antes do `cleanup_before_exit()` é lido do fonte do
tauri 2.12.5 (`src/app.rs:1449`), e o `async_runtime` guarda o runtime do processo
numa `OnceLock` — por isso o `block_on` na saída é seguro, não um acaso.

**Medido.** Vermelho, com o helper ainda parado num `Ok(()) { sleep(5s) }`:
**5,02 s** de espera contra a régua de 50 ms do teste, exatamente a tela girando.
Verde, hoje: `cargo test --lib mcp::manager` → **os dois testes novos passam**,
e a medição do mesmo teste caiu para **0,06 s**. Testes:
`o_servidor_que_nao_responde_devolve_um_erro_em_vezes_de_ficar_pendurado` (o texto
precisa nomear o servidor *e* dizer que não respondeu, para o erro não virar
"falhou" genérico) e `o_servidor_que_responde_passa_pela_regua_inteiro` (a régua
não pode cortar quem fala a tempo).

**ALEGADO, não medido aqui:** o gancho de saída e os 45 s contra um servidor
realmente mudo exigem fechar a janela do aplicativo, o que estava proibido nesta
passagem. O que está medido é o `dentro_do_prazo` com prazos de teste; o valor
`45` é escolha documentada, não algo que a suíte espera.

## Gates

`cd src-tauri && cargo test --lib` → **548 passaram, 0 falharam, 11 ignored,
finished in 4.60 s** (eram 544; os quatro novos são os testes de M2).
`npm run build` → **OK**, sem novos avisos.

Quarta passagem (medido 2026-11-20, com M4 na árvore):
`cd src-tauri && cargo test --lib` → **558 passaram, 0 falharam, 11 ignored,
finished in 4.98 s**; o recorte `mcp::manager` são **4 testes, 0,06 s**.
`cargo check --all-targets` → **zero avisos**.
