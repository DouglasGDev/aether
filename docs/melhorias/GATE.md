# GATE — o gate de permissão não confia em alegação de terceiros

Dono: maintainer. A frente mais crítica do lote: o gate é o mecanismo central de
segurança do produto ("ações destrutivas, acesso a arquivos sensíveis, execução
de código exigem aprovação"). Duas falhas o derrotavam.

## S2 — hint read-only de terceiros concedia execução sem prompt

**Defeito (lido).** `mcp/tool.rs::classify_effect` dava `ToolEffect::ReadOnly`
quando `annotations.read_only_hint == Some(true)`. `permissions/mod.rs::evaluate`
deixa `ReadOnly` passar sem prompt em **Plan** (`ReadOnly => Allow`) e
**AutoAccept** (`ReadOnly | Mutating => Allow`). Um servidor MCP malicioso anota
uma ferramenta de escrita/apagar como `read_only_hint=true` e executa sem prompt
nesses dois modos. O comentário do modo Default (`permissions/mod.rs:60-63`) já
admitia que o hint é não-confiável ("Letting a read-only claim skip approval
would let a server grant itself trust") — mas só o Default se protegia; Plan e
AutoAccept confiavam na mesma alegação que o Default recusava.

**Correção.** `classify_effect` agora tem teto `Mutating` para terceiros: honra
`destructive_hint` (que aumenta a cautela → `Destructive`), nunca honra
`read_only_hint` (que a diminui). O conector embutido continua confiável porque
usa `ToolInfo::local` com `ferramenta.efeito` declarado pelo app
(`mcp/manager.rs:101,106`), não `classify_effect` — só servidores de terceiros
passam por `from_rmcp` (`manager.rs:351`). Confirmado por grep: `classify_effect`
só é chamado em `from_rmcp` e nos testes.

**Consequência documentada (não é defeito, é a postura segura).** Em Plan, uma
ferramenta de terceiros passa a ser negada (`Mutating => Deny`): não dá para
verificar a alegação read-only, e Plan promete não mutar. Em AutoAccept, roda como
`Mutating => Allow` (o modo já consente com mutação). O rótulo de efeito exibido
para um terceiro que alega read-only passa a `Altera` — conservador e honesto,
porque não sabemos que é só-leitura. As ferramentas locais somente-leitura
(`ler_arquivo`, `buscar_memoria`, `info_sistema`, `ler_resultado`, leituras de
banco) seguem `Só lê` e seguem passando em Plan.

## S5 — grant de sessão pulava o prompt de destrutivo no Default

**Defeito (lido).** `permissions/mod.rs::evaluate`, modo Default:
`else if session_granted { Decision::Allow }` para qualquer efeito, inclusive
`Destructive`. Depois de um "confiar neste servidor", `apagar`/`bancos_apagar`
rodavam sem prompt. O Default (modo cauteloso) ficava MENOS seguro que o
AutoAccept, que pergunta para destrutivo (`AutoAccept: Destructive => Ask`).

**Correção.** Default pergunta para `Destructive` mesmo com grant da sessão
(`else if effect == ToolEffect::Destructive { Decision::Ask }` antes do
`session_granted`). Composto continua perguntando; não-destrutivo com grant
continua passando; sem grant continua perguntando. Auto/FullAccess seguem
`Allow` (são os modos explícitos de confiança total).

## Testes vermelho-primeiro

Escritos antes da correção, vistos vermelhos, depois verdes:

- `mcp::tool::tests::a_third_party_read_only_claim_does_not_grant_trust`
  — `classify_effect(read_only=Some(true)) == Mutating`.
  Vermelho: `left: ReadOnly, right: Mutating`. Verde após o teto.
- `permissions::tests::session_grant_does_not_skip_destructive_prompt`
  — `evaluate(Default, Destructive, false, true) == Ask`.
  Vermelho: `left: Allow, right: Ask`. Verde após o ramo destrutivo.

O teste antigo `read_only_hint_is_honoured` (afirmava `ReadOnly`, o comportamento
inseguro) foi removido — ele contradizia o contrato corrigido.

## Medida

`cd src-tauri && cargo test --lib` após a correção, árvore limpa (só esta frente):
**479 passaram, 0 falharam, 5 ignored** (linha de base era 478; +2 testes novos,
−1 antigo = 479). Log: `C:/Users/dougl/AppData/Local/Temp/aether-gate-gate-fix.log`.
Os dois testes novos passam dentro dos 479.

## Arquivos tocados

- `src-tauri/src/mcp/tool.rs` — `classify_effect` teto Mutating; teste novo;
  teste antigo inseguro removido.
- `src-tauri/src/permissions/mod.rs` — Default pergunta para Destructive com
  grant; teste novo.

Nenhuma chave de i18n nova: o rótulo `Altera` e o fluxo de aprovação já existem.

## Follow-up desta frente

- O modo AutoAccept ainda auto-aceita `Mutating` de terceiros (é o consentimento
  do modo). Se um dia quiser caution extra, um terceiro poderia ser `Ask` mesmo
  em AutoAccept — decisão de produto, não defeito.
- A tensão maior do lock do motor (A1) é da frente CONC, não desta.