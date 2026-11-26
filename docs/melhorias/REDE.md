# REDE — a porta de saída do catálogo (tarefa 118)

Frente da ferramenta de busca. Lado do Rust, os arquivos tocados são
`src-tauri/src/local/web.rs` (novo, 1561 linhas — `wc -l src-tauri/src/local/web.rs`),
`src-tauri/src/local/mod.rs`, `src-tauri/src/permissions/mod.rs`, `src-tauri/src/lib.rs`,
`src-tauri/src/state.rs` — mais o braço único em `mcp/servidor.rs` que o `match`
exaustivo obrigou (abaixo) — e este doc. Nenhuma dependência nova:
`git diff --stat src-tauri/Cargo.toml` sai vazio,
`reqwest = { version = "0.13", features = ["stream"] }` já estava lá
(`Cargo.toml:47`), e `tokio` já era `full` (`Cargo.toml:24`). O outro lado da
mesma chave — os 30 textos em `locales/*.json` e os quatro arquivos em `src/` que
dão cara ao efeito novo — está listado no item 8 e é do agente da frente de tela,
não deste arquivo.

Uma exceção à lista de posse, forçada pelo compilador: `ToolEffect` é um enum
exaustivo e `mcp/servidor.rs:373-379` faz `match` nele, então a variante nova
precisou de um braço lá. O braço é `ToolEffect::Rede => {}` — nenhuma anotação
MCP é afirmada, porque `read_only_hint` mentiria justamente sobre o egresso.
Nenhuma outra linha desse arquivo foi tocada.

Nada foi aberto: nenhum app, nenhuma GUI. A busca ao vivo **não foi medida** — o
item 11 lista o que ficou de fora.

## 1. O efeito novo e como `evaluate` o recusa

`ToolEffect::Rede` (`permissions/mod.rs:19-30`) não é um degrau acima de
`Destructive`: é outro eixo. O que um `Destructive` faz no disco admite
arrependimento; o texto que sai pela rede já está do outro lado antes de o
cartão aparecer, e não tem volta. Por isso ele não se graduou com os três de
sempre e por isso `evaluate` (`permissions/mod.rs:40-93`) o trata à parte em
cada braço:

| modo | `ReadOnly` | `Mutating` | `Destructive` | **`Rede`** |
| --- | --- | --- | --- | --- |
| `Plan` | Allow | Deny | Deny | **Deny** (`:47-53`) |
| `Default` | Ask/Allow por grant | Ask/Allow | Ask, sem grant | **Ask, mesmo com grant** (`:76-81`) |
| `AutoAccept` | Allow | Allow | Ask | **Ask** (`:69`) |
| `Auto` | Allow | Allow | Allow | **Ask** (`:60-66`) |
| `FullAccess` | Allow | Allow | Allow | **Ask** (`:60-66`) |

Duas regras importam e as duas têm teste:

- Nenhum modo devolve `Allow` para `Rede`, nem com comando composto, nem com a
  confiança da sessão. A régua percorre os cinco modos × composto × concedido
  (`nenhum_modo_aprova_a_rede`, `permissions/mod.rs:273-303`), então um modo novo
  que alguém acrescentar à lista de `AgentMode` cai dentro do teste no dia em
  que nascer.
- No `Default` o `Rede` é conferido **antes** do `session_granted` (`:76-83`),
  junto com o `Destructive`: um grant não pode tornar o modo padrão menos seguro
  do que o `AutoAccept`, que já pergunta. `a_confianca_da_sessao_nao_compra_a_
  porta_de_saida` (`:305-323`) confere isso pelo `PermissionGate` real e ainda
  exige que o `Mutating` do mesmo conector continue `Allow` — sem essa segunda
  metade o teste não provaria que foi a rede que mudou a resposta.

O efeito viaja para a interface como texto: `serde(rename_all = "kebab-case")`
devolve `"rede"` (`o_efeito_da_rede_serdeja_como_a_palavra_rede`, `:325-333`).

## 2. A ferramenta e o seu esquema

Uma ferramenta só, no catálogo: `buscar_na_web` (`web.rs:79`, `FERRAMENTA`). O
esquema (`web::ferramenta`, `web.rs:267-283`) tem exatamente duas propriedades e
nenhuma delas é um endereço:

```json
{
  "type": "object",
  "properties": {
    "consultaRede": { "type": "string" },
    "maximoResultados": { "type": "integer", "minimum": 1, "maximum": 5 }
  },
  "required": ["consultaRede"],
  "additionalProperties": false
}
```

`a_ferramenta_declara_o_efeito_de_rede_e_nao_aceita_endereco` (`web.rs:1441`)
pinha o nome, o efeito `Rede`, as duas chaves na ordem, a ausência de `url` e de
`endereco`, `additionalProperties: false` e o teto 5. O teto também é aplicado no
código, não só declarado: `maximo.clamp(1, MAX_RESULTADOS)` (`web.rs:630`).

## 3. Os dois hosts, e por que estes

A allowlist é uma constante do arquivo (`FORNECEDORES`, `web.rs:125-146`) e é de
pares (host, prefixo de caminho) — não de hosts. Não há chave, não há conta, não
há endpoint configurável por arquivo: `rede.json` tem uma chave só, porque uma
allowlist que o usuário edita deixa de ser allowlist.

- `html.duckduckgo.com` / `/html/` — a página pública de resultados. É o que
  existe sem chave: o Instant Answer API foi desligado e a API de busca séria do
  DuckDuckGo é paga. Endpoint não versionado: pode mudar de marcação e o parser
  devolver lista vazia — devolve vazio e diz no cartão, não inventa.
- `pt.wikipedia.org` / `/w/api.php` — a API de ação do MediaWiki, documentada,
  sem chave, JSON com `canonicalurl` no corpo. É o fornecedor que segura o
  resultado quando o outro trocar a marcação.

Os dois são terceiros e nada aqui verifica o que eles respondem; por isso o
cartão diz de onde os bytes vieram (item 7). `a_lista_de_hosts_e_a_que_o_codigo_
usa` (`web.rs:1460`) amarra a lista exibida no cartão à lista usada no código, e
`o_limite_do_pedido_e_o_teto_da_ferramenta` (`web.rs:1276`) confere que o limite
pedido vira `gsrlimit` no pedido.

## 4. O validador: cada regra, o teste que a pinça

`validar` (`web.rs:424-465`) roda antes de qualquer soquete, e `montar`
(`web.rs:555`, chamando-a em `:586`) a chama sobre a URL que o próprio arquivo
construiu — o modelo não
escolhe para onde ir, escolhe só o que procurar.

| regra | recusa | teste | mutação que provou o vermelho |
| --- | --- | --- | --- |
| só `https` | `Motivo::Esquema` | `um_esquema_que_nao_e_https_nao_sai` | aceitar `http` no `if` do esquema |
| host inteiro, sem sufixo mentiroso | `Motivo::Host` | `um_host_fora_da_lista_nao_sai` | trocar `f.host == normal` por `normal.contains(f.host)` |
| endereço literal proibido | `Motivo::Endereco` | `um_endereco_literal_proibido_nao_sai` | desligar o `&& endereco_proibido(literal)` |
| faixas: 127/8, 169.254/16, 10/8, 172.16/12, 192.168/16, 100.64/10, 198.18/15, 192.0.0/23, 192.0.2/24, 240/4, `::1`, `fc00::/7`, `fe80::/10`, `2001:db8::/32`, `::/96`, IPv4 mapeado em IPv6 | — | `as_faixas_proibidas_sao_as_faixas_de_casa` | tirar `v4.is_link_local()` (169.254.169.254 passou) |
| host da lista **com o caminho do endpoint** | `Motivo::Caminho` | `a_lista_e_de_pares_host_e_caminho` | `starts_with(fornecedor.prefixo)` → `starts_with("")` |
| credencial na URL, porta ≠ 443 | `Motivo::Usuario`, `Motivo::Porta` | `credencial_e_porta_na_url_sao_recusadas` | folgar as duas conferências de userinfo |
| `localhost` e `*.localhost` | `Motivo::Endereco` | `um_endereco_literal_proibido_nao_sai` | idem à linha do endereço literal |
| pulo para fora da lista ou para outro esquema | `Motivo::Pulo`/`Host`/`Esquema` | `um_pulo_para_fora_da_lista_e_recusado_antes_de_conectar`, `o_pulo_nunca_reinventa_a_lista` | tirar o `validar(&novo)?` de `proximo_pulo` |
| a cadeia para em 2 pulos **mesmo com destino bom** | `Motivo::Pulo` | `a_cadeia_para_no_segundo_pulo_mesmo_com_destino_bom` | desligar o `if pulos >= MAX_PULOS` |
| a consulta nunca abre rota | — | `a_consulta_nao_abre_rota` | montar a query por concatenação crua, sem o codificador |
| o nome conferido é o nome conectado | `Motivo::SemEndereco` | `um_nome_que_s6_resolve_em_endereco_proibido_e_recusado` | tirar o filtro de `escolher_enderecos` |

Escolha defendível, e ela está no doc-comment de `validar` (`web.rs:416-423`): o
cliente roda com `redirect::Policy::none()` (`web.rs:702`) e **este laço** segue
cada pulo, conferindo antes de conectar; e a resolução é feita à mão
(`enderecos_usaveis`, `web.rs:479`) e amarrada ao cliente com
`resolve_to_addrs` (`web.rs:705`). Conferir só o nome deixaria um `/etc/hosts`
hostil apontar `pt.wikipedia.org` para `127.0.0.1`; conferir só o endereço
deixaria o resolvedor responder outra coisa na hora da conexão. As duas metades
juntas fecham o vão do rebinding.

## 5. O teto, os prazos e o corpo

- `LIMITE_CORPO = 512 * 1024` (`web.rs:86`). O `Content-Length` é conferido, mas
  quem manda é o `Medidor` (`web.rs:521-542`), que conta peça a peça e recusa a
  resposta **inteira** ao passar do teto: HTML cortado no meio produziria um
  resultado pela metade, e resultado pela metade apresentado como resultado é a
  ferramenta mentindo duas vezes. Prova: `o_corpo_para_no_teto_e_nao_devolve_
  pela_metade` (mutação: `> self.teto` → `> self.teto * 2`).
- `PRAZO_PEDIDO = 8 s`, `PRAZO_CONEXAO = 4 s`, `PRAZO_GERAL = 15 s`
  (`web.rs:90-96`). O de parede é **menor** que a soma dos prazos por requisição
  de propósito: `os_prazos_declarados_sao_menores_que_a_soma_dos_pulos` (mutação:
  15 → 30 s) confere `PRAZO_GERAL < PRAZO_PEDIDO * (MAX_PULOS + 1)`, que é o que
  impede um host que silencia em cada pulo de somar oito segundos três vezes.
- `MAX_RESULTADOS = 5`, `MAX_PULOS = 2`.

## 6. `rede.json`: desligado é o estado de fábrica

O struct é uma chave só (`Rede { ativa }`, `web.rs:160-165`) e o arquivo vive no
mesmo diretório de `pastas.json` / `geracao.json` / `adaptadores.json`
(`lib.rs:38-46`, agora `:44-45` com o `rede_path`). `de()` (`web.rs:172`) lê;
`gravar()` (`web.rs:185`) escreve em `.json.tmp` e troca no fim, a política de
todos os outros arquivos do aplicativo.

- Arquivo ausente → `ativa: false`, sem aviso. Arquivo ilegível → `false` **com
  aviso no stderr**, porque engolir um `rede.json` quebrado como "desligado"
  seria a tela prometer o que o disco não diz. Provas:
  `sem_arquivo_a_rede_esta_desligada`, `um_rede_json_quebrado_devolve_desligado_
  e_o_motivo`, `a_escolha_do_arquivo_e_a_escolha_da_ferramenta`,
  `ligar_e_desligar_fica_no_disco` (que também confere que o `.tmp` não sobra).
- `carregar` roda uma vez na abertura (`lib.rs:44-45`), antes de
  `manager.connect(local::CONECTOR)`, porque é a escolha que decide se a
  ferramenta entra no catálogo que o gerente publica. O caminho fica no estado
  (`state.rs:62-67`) para a tela que um dia liga o botão escrever no mesmo
  arquivo que o catálogo leu.
- Três travas independentes: o arquivo, o catálogo (`catalogo()` →
  `catalogo_com_rede`, `local/mod.rs:95-109`) e a execução (`buscar_com`
  recusa antes de ler argumento, `web.rs:622-625`). `a_rede_desligada_recusa_a_
  ferramenta` e `chamar_a_rede_pelo_nome_com_a_porta_desligada_devolve_a_recusa`
  conferem as duas últimas; `a_rede_ligada_e_a_unica_ferramenta_que_entre_e_saia_
  do_catalogo` confere 12 → 13 e que a diferença é uma ferramenta só.

## 7. O cartão diz de onde os bytes vieram

`saida()` (`web.rs:990-1034`) abre a vista com a linha de proveniência
(`rede.origem` / `rede.foraDaMaquina` + a lista real de hosts, `web.rs:1042-1043`) e o
texto que o modelo recebe abre com `rede.aviso`. Cada linha do resultado carrega
o host junto do título. Um nome de host velho no cartão é impossível: a linha é
montada de `hosts_permitidos()`.

## 8. As trinta chaves publicadas da porta de saída

`CONTRIBUTING.md:42-45` exige paridade, e `idiomas.rs:307` varre o fonte atrás de
`tr("literal")` e cobra texto em todo idioma. Toda frase visível desta frente sai
por `frase()` / `erro()` (`web.rs:209-217`), que montam a chave em tempo de
execução — a mesma saída que `campo()` e `ferramenta()` já usam para famílias
compostas — e as trinta chaves abaixo foram publicadas nas duas pastas na mesma
roda em que o texto foi escrito. A tabela é o espelho do catálogo, não um dossiê
do que falta: `local/web.rs` tem a lista em `CHAVES` (`web.rs:1503`) e o teste
`toda_chave_de_rede_tem_texto_em_todo_idioma` (`web.rs:1537`) roda e confere,
chave por chave, que `idiomas::tem_texto` responde verdadeiro nos dois idiomas.
Uma chave montada por `format!` cai fora da varredura de `idiomas.rs:307`, então
`CHAVES` é o único censo que existe para esta família — e o teste é o que impede
o censo de ficar velho. A guarda anterior, que proibia chave literal no fonte,
foi retirada de propósito: publicada a tabela, uma chave literal é um pedido que
o varredor do catálogo já resolve, não um defeito.

`rede.*` (colunas: chave · pt-BR · en-US; placeholders iguais nos dois, como
cobra `idiomas.rs:173`):

| chave | pt-BR | en-US |
| --- | --- | --- |
| `rede.aviso` | Resultado obtido fora desta máquina, em servidor de terceiros. Nada aqui foi verificado localmente. | Result obtained outside this machine, from a third-party server. Nothing here was verified locally. |
| `rede.consulta` | Consulta enviada | Query sent |
| `rede.origem` | Origem dos bytes | Where the bytes came from |
| `rede.foraDaMaquina` | Fora da máquina | Outside this machine |
| `rede.vazio` | Nenhum resultado chegou | No result arrived |
| `rede.falha` | O fornecedor {fornecedor} não respondeu | Provider {fornecedor} did not answer |
| `rede.motivo.esquema` | Esquema recusado: só https sai desta máquina | Scheme refused: only https leaves this machine |
| `rede.motivo.host` | Host fora da lista permitida | Host outside the allowlist |
| `rede.motivo.porta` | Porta diferente de 443 | Port other than 443 |
| `rede.motivo.caminho` | Caminho fora do endpoint declarado para este host | Path outside the endpoint declared for this host |
| `rede.motivo.usuario` | Credencial na URL | Credential in the url |
| `rede.motivo.endereco` | Endereço de retorno, privado ou link-local | Loopback, private or link-local address |
| `rede.motivo.pulo` | Redirecionamento fora da lista, ou pulos demais | Redirect outside the allowlist, or too many hops |
| `rede.motivo.corpo` | Corpo acima de 512 KiB | Body above 512 KiB |
| `rede.motivo.prazo` | Prazo esgotado | Deadline exceeded |
| `rede.motivo.semEndereco` | O nome não resolveu em endereço público | The name did not resolve to a public address |
| `erros.redeDesligada` | A busca na web está desligada: ligue em rede.json | Web search is off: turn it on in rede.json |
| `erros.redeMotor` | Não foi possível abrir o motor de rede | Could not open the network engine |
| `erros.redeTodosFalharam` | Nenhum fornecedor respondeu: {detalhe} | No provider answered: {detalhe} |
| `erros.redeResposta` | O servidor respondeu com erro {codigo} | The server replied with error {codigo} |
| `erros.redeSemDestino` | Resposta {codigo} sem endereço para onde ir | Response {codigo} with no place to go |
| `erros.redeFalhou` | O pedido falhou: {detalhe} | The request failed: {detalhe} |
| `erros.redeArquivoQuebrado` | rede.json ilegível; a rede ficou desligada | rede.json unreadable; the network stayed off |
| `erros.redeGravar` | Não foi possível gravar rede.json: {detalhe} | Could not write rede.json: {detalhe} |
| `ferramentas.buscarNaWeb.titulo` | Pesquisar na internet | Search the web |
| `ferramentas.buscarNaWeb.descricao` | Procura em html.duckduckgo.com e pt.wikipedia.org e devolve título, endereço e resumo. Só existe com a rede ligada em rede.json, e cada saída pede aprovação da pessoa. O texto da consulta é o único dado que sai da máquina. | Searches html.duckduckgo.com and pt.wikipedia.org and returns title, url and snippet. It exists only when the network is on in rede.json, and every call needs the user's approval. The query text is the only data that leaves the machine. |
| `argumentos.consultaRede` | Consulta | Query |
| `argumentos.consultaRedeDescricao` | O texto a procurar. Ele entra como valor de um parâmetro de uma URL fixa: endereço, host e caminho não são escolha do modelo. | The text to look for. It goes in as the value of one parameter of a fixed url: address, host and path are not the model's choices. |
| `argumentos.maximoResultados` | Máximo de resultados | Maximum results |
| `argumentos.maximoResultadosDescricao` | Entre 1 e 5; mais que isto o código corta. | Between 1 and 5; the code cuts anything above that. |

Trinta chaves. Uma a mais, do lado da tela, e ela também está publicada:
`efeito.saiDaMaquina` · "Sai da máquina" · "Leaves the machine". A tela conhecia
três efeitos; hoje `src/lib/types.ts:48` declara `ToolEffect = "read-only" |
"mutating" | "destructive" | "rede"`, `src/lib/display.ts:28-39` aponta
`EFFECT_KEY.rede` para essa chave e `EFFECT_ICON.rede` para o globo, e
`src/components/GrafoNeural.tsx:371` dá ao fio a cor `--egress` (`src/styles.css:14`,
com `.lig-fio--rede` em `:3500` e `.lig-porta--rede` em `:3822`) em vez de deixá-lo
cair no cinza de pasta. Sem o membro novo o cartão chegava com
rótulo vazio; o `npm run build` desta roda é verde com ele.

## 9. As mutações, e o vermelho com nome

Trinta e quatro testes correm desta frente, contados na saída da suíte inteira:
**28** em `local::web::testes` (`grep -c '^test local::web::.* \.\.\. ok$'` sobre o
log verde fecha 28, nenhum ignorado), **3** novos em `permissions::tests`
(`nenhum_modo_aprova_a_rede`, `a_confianca_da_sessao_nao_compra_a_porta_de_saida`,
`o_efeito_da_rede_serdeja_como_a_palavra_rede`) e **3** novos em `local::tests`
(`a_rede_ligada_e_a_unica_ferramenta_que_entre_e_saia_do_catalogo`,
`a_ferramenta_de_rede_tem_o_efeito_que_o_gate_nao_deixa_passar`,
`chamar_a_rede_pelo_nome_com_a_porta_desligada_devolve_a_recusa`), mais três testes
existentes estendidos com uma asserção de `Rede` cada
(`plan_mode_is_read_only`, `auto_and_full_access_bypass_prompts`,
`auto_accept_still_asks_for_destructive`). Eles foram quebrados em dois lotes com
todas as mutações ativas de uma vez, e cada mutação tem o seu nome:

| mutação | vermelho nomeado |
| --- | --- |
| esquema aceita `http` | `um_esquema_que_nao_e_https_nao_sai` |
| host por `contains` | `um_host_fora_da_lista_nao_sai` |
| endereço literal sem conferência de faixa | `um_endereco_literal_proibido_nao_sai` |
| sem `is_link_local` | `as_faixas_proibidas_sao_as_faixas_de_casa` |
| caminho sem o prefixo do endpoint | `a_lista_e_de_pares_host_e_caminho` |
| userinfo folgada | `credencial_e_porta_na_url_sao_recusadas` |
| pulo sem `validar` | `um_pulo_para_fora_da_lista_e_recusado_antes_de_conectar`, `o_pulo_nunca_reinventa_a_lista` |
| contador de pulos desligado | `a_cadeia_para_no_segundo_pulo_mesmo_com_destino_bom` |
| query montada por concatenação crua | `a_consulta_nao_abre_rota` |
| `gsrlimit` fixo no teto | `o_limite_do_pedido_e_o_teto_da_ferramenta` |
| teto do corpo ×2 | `o_corpo_para_no_teto_e_nao_devolve_pela_metade` |
| `uddg` sem filtro de esquema | `o_involucro_javascript_nao_vira_link` |
| página sem `canonicalurl` aceita | `o_json_do_mediawiki_vem_na_ordem_que_o_servidor_disse` |
| JSON inválido devolve item inventado | `um_corpo_que_nao_e_o_esperado_devolve_nada_em_vez_de_inventar` |
| `&amp;` não desescapado | `as_entidades_de_um_servidor_sao_texto` |
| `texto_limpo` começando dentro de etiqueta | `as_etiquetas_do_servidor_nao_chegam_ao_modelo` |
| snippet sempareado | `o_html_do_duckduckgo_vira_titulo_endereco_e_resumo` |
| filtro de endereço resolvido removido | `um_nome_que_s6_resolve_em_endereco_proibido_e_recusado` |
| arquivo ausente liga a rede | `sem_arquivo_a_rede_esta_desligada` |
| `rede.json` quebrado sem aviso | `um_rede_json_quebrado_devolve_desligado_e_o_motivo` |
| gravar sem o `rename` | `ligar_e_desligar_fica_no_disco` |
| chave serde trocada | `a_escolha_do_arquivo_e_a_escolha_da_ferramenta` |
| porta de execução desligada | `a_rede_desligada_recusa_a_ferramenta` |
| efeito `ReadOnly` na ferramenta | `a_ferramenta_declara_o_efeito_de_rede_e_nao_aceita_endereco`, `a_ferramenta_de_rede_tem_o_efeito_que_o_gate_nao_deixa_passar` |
| `hosts_permitidos` pela metade | `a_lista_de_hosts_e_a_que_o_codigo_usa` |
| `PRAZO_GERAL = 30 s` | `os_prazos_declarados_sao_menores_que_a_soma_dos_pulos` |
| catálogo sempre com a rede | `o_catalogo_cobre_arquivos_memoria_resultados_terminal_e_sistema`, `a_rede_ligada_e_a_unica_ferramenta_que_entre_e_saia_do_catalogo` |
| despacho sem o braço `buscar_na_web` | `chamar_a_rede_pelo_nome_com_a_porta_desligada_devolve_a_recusa` |
| `Plan` liberando a rede | `plan_mode_is_read_only` |
| `Auto`/`FullAccess` liberando a rede | `auto_and_full_access_bypass_prompts`, `nenhum_modo_aprova_a_rede` |
| `AutoAccept` liberando a rede | `auto_accept_still_asks_for_destructive` |
| `Default` deixando o grant comprar a rede | `a_confianca_da_sessao_nao_compra_a_porta_de_saida` |
| `#[serde(rename = "network")]` em `Rede` | `o_efeito_da_rede_serdeja_como_a_palavra_rede` |
| `tr("rede.inexistente")` escrito no fonte | `idiomas::tests::toda_chave_pedida_pelo_codigo_existe_em_todo_idioma` |

O último par não foi uma mutação artificial: ele aconteceu de verdade durante a
frente, quando dois comentários deste arquivo continham a forma `tr("literal")`
e o varredor de `idiomas.rs:307` (`chaves_pedidas`) os leu como pedido de chave. A prova de que a
paridade alcança esta frente está no vermelho que ela já causou. Com a tabela
publicada, a guarda que existia para segurar a chave até o catálogo chegar foi
retirada — e o que a substitui é mais forte, não menos: o varredor do catálogo
continua cobrando toda chave literal, só que agora ele tem texto para cobrar.

A cobrança foi re-medida nesta roda, com a chave escrita de propósito dentro de
`linha_proveniencia` (`web.rs:1039`) — uma função de produção, não um teste:

```text
$ cd "C:/Aether AI/src-tauri" && cargo test --lib idiomas
test idiomas::tests::toda_chave_pedida_pelo_codigo_existe_em_todo_idioma ... FAILED
thread 'idiomas::tests::toda_chave_pedida_pelo_codigo_existe_em_todo_idioma' (30928) panicked at src\idiomas.rs:277:9:
chaves pedidas pelo código sem texto em algum idioma:
C:\Aether AI\src-tauri\src\local\web.rs → rede.inexistente

test result: FAILED. 11 passed; 1 failed; 0 ignored; 0 measured; 685 filtered out; finished in 0.02s
```

O vermelho nomeia o arquivo e a chave, e a própria linha de resultado fecha o
censo por outro caminho: 11 + 1 + 685 = 697 casos anunciados, o número do item
11, obtido sem contar linhas. A injeção foi retirada antes das três portas
verdes, e nada dela sobrou no fonte.

Todos os parsedores foram conferidos contra corpo congelado — `FIXTURE_DDG` e
`FIXTURE_WIKIPEDIA` são `const &str` no módulo de teste (`web.rs:1054`, `:1073`),
com o involucro `l/?uddg=`, o `<b>` em volta do termo, a entidade escapada, o
`javascript:` no `uddg` e as páginas da Wikipédia fora de ordem de propósito.

## 10. O que NÃO foi medido

- **A busca ao vivo.** Nenhuma requisição real foi feita: nada mede latência, HTTP
  429 do DuckDuckGo, mudança de marcação, `Content-Length` mentiroso nem o que o
  MediaWiki devolve hoje. O que está provado é o parsedor contra corpo
  congelado e o validador contra URL — o transporte em si não foi exercido.
- **O `resolve_to_addrs` em produção.** A pinça de endereço é exercitada por
  `escolher_enderecos` (pura) e pelo validador; o efeito da amarração no cliente
  `reqwest` depende de DNS real e não foi medida.
- **Nenhuma tela aberta.** O app continua fechado por ordem do mantenedor: o
  `rede.json` é lido na abertura e não há UI para ligá-lo ainda — quem liga hoje é
  o arquivo, escrito à mão. O lado de tela desta roda tem o membro `rede` no tipo,
  no rótulo, no ícone e na cor do fio, e o `npm run build` verde prova que o
  catálogo TypeScript fecha; o que ele não prova é o pixel. Nenhuma janela foi
  aberta para ver o cartão com o globo.
- **O `mcp/servidor.rs` anunciado.** O braço novo existe porque o `match` é
  exaustivo; o que um cliente MCP vê de um efeito sem hint não foi medido.

## 11. As portas de corrida

Rodadas depois da última edição de fonte desta frente — um comentário em
`web.rs:205-208` que dizia o contrário do §8 —, com a saída tal qual ela saiu. O
`cargo check` não precisou de `touch`: a própria edição do arquivo forçou a
re-leitura do alvo, e as corridas de mutação provam que o compilador imprime as
advertências quando existem (uma delas imprimiu `warning: unused variable:
maximo`). No item 12 está o que falhou nas janelas em que o vizinho recompilava.

```text
$ cd "C:/Aether AI/src-tauri" && cargo test --lib
test local::tests::a_rede_ligada_e_a_unica_ferramenta_que_entre_e_saia_do_catalogo ... ok
test local::web::testes::um_host_fora_da_lista_nao_sai ... ok
test permissions::tests::o_efeito_da_rede_serdeja_como_a_palavra_rede ... ok

test result: ok. 685 passed; 0 failed; 12 ignored; 0 measured; 0 filtered out; finished in 7.36s

$ cd "C:/Aether AI/src-tauri" && cargo check --all-targets
    Checking aether v0.1.0 (C:\Aether AI\src-tauri)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.59s

$ cd "C:/Aether AI" && npm run build
✓ 307 modules transformed.
✓ built in 171ms
```

Zero advertência nas três portas — `grep -c warning` sobre o log do `check`
devolve `0`. As linhas são o fim da saída, não a suíte inteira. A contagem, com
os dois comandos que a reproduzem no log:

```text
$ grep -cE '^test .* \.\.\. ok$' scratch/gates-test3.log
685
$ grep -cE '^test .* \.\.\. (ok|FAILED|ignored)' scratch/gates-test3.log
697
```

Seiscentas e noventa e sete casos anunciados — 685 verdes, 12 ignorados. Os doze,
nomes conferidos no mesmo log (o arquivo fica em `scratch/`, fora do repositório;
o que viaja é o comando, e cada bloco acima é a saída literal): 4 de `ai::download` (baixam do HuggingFace), 2 de
`ai::engine` (modelo real em disco; DLLs vendorizadas e placa), 4 de `bancos::motor`
(postgres, mssql, mongo e oracle com servidor verdadeiro), 1 de `runtime` (46 MB dos
releases oficiais) e 1 de `voz::testes` (Kokoro falando e Whisper ouvindo). Um `grep -c
"^test "` devolve 698, não 697: a linha `test result:` também começa com `test `, e
contar linhas por prefixo não é contar casos.

Referência: a linha de base citada na tarefa é 617 passed / 0 failed / 11 ignored,
e a corrida desta frente fechou **671 / 0 / 12** na janela em que ela terminou. O
`685` publicado acima é a árvore no commit, depois das frentes de CPU
(`ai::ggml::ffi::testes`, +2) e de leitura de bancos (`bancos/**`), que também
acrescentaram testes na mesma janela. O que esta frente prova continua sendo o
recorte dela: 34 casos, todos verdes, e o vermelho nomeado de cada mutação do §9.

## 12. O que cruzou a janela desta frente

- `src/bin/engine_probe.rs` — chegou a falhar desta frente com
  `error[E0308]: mismatched types` (`let Some([t, l, f]) = campo.as_slice()`
  sobre `&[&str]`), o que barrava `cargo check --all-targets`. Na janela em que o
  arquivo era de outro trabalho o defeito foi registrado, não tocado. O arquivo é
  agora desta sessão: a forma de quatro campos (`threads,threads_lote,lote,fa`)
  foi escrita aqui, e o braço `prefixo:` deixou de inferir que era ggml — ele lê
  `engine.usa_ggml()` e sai com código 2 dizendo que o candle prefila inteiro, em
  vez de medir o recorte de cache sobre um motor que não o tem.
- `src-tauri/src/bancos/**` — em duas janelas desta frente a suíte fallou com
  `bancos::motor::sqlite::tests::o_filtro_vai_na_mesma_viagem_com_o_valor_ligado`
  e `o_nulo_do_filtro_e_is_null_na_mesma_viagem`, e o lib não compilou com
  `cannot find value AGENDADORES_NASCIDOS` e com `unresolved import
  super::prazo_de_espera` (`bancos/motor/postgres.rs`). Nada disso foi corrigido
  por esta frente enquanto o vizinho escrevia; o trabalho de leitura passou para
  as mãos do maintainer depois, e o que ele prova está em
  `docs/melhorias/BANCOS.md`.
