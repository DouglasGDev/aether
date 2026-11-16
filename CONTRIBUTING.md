# Contribuir

Obrigado por ler isto antes de abrir uma pull request. As regras abaixo não são
gosto pessoal: cada uma vem de um defeito que este projeto já teve, e a maioria
tem um teste que a vigia.

## Antes de codar

1. `npm install` e coloque as DLLs vendorizadas no lugar
   ([`src-tauri/vendor/README.md`](src-tauri/vendor/README.md)).
2. `cd src-tauri && cargo test --lib` — tem de estar verde antes de você começar.
   Se algo já está vermelho, diga na issue; não conserte de carona em um PR de
   outro assunto.
3. `npm run build` — o `tsc` faz parte do build. Tipo quebrado é teste quebrado.

## O que este projeto não aceita

**Nada simulado.** Nenhum resultado inventado, nenhum *placeholder*, nenhum
fallback que finge sucesso. Se uma capacidade não rodou, a resposta é o motivo
real dela — "a placa recusou a carga", "não existe a ferramenta X", "a janela não
comporta este pedido". Um caminho que devolve texto pronto para a tela ficar
bonita é um defeito, não uma entrega.

**Uma política, um lugar.** O que autoriza uma chamada é `mcp.json` (allowlists)
e `pastas.json` (concessões), cobrados no núcleo. A tela de grafo edita essa
política; ela não tem interruptor próprio. Se você precisa de um segundo botão
para ligar ou desligar algo, o primeiro botão está no lugar errado.

**Nada de binário no repositório.** As DLLs de terceiros ficam fora do controle
de versão de propósito. Se um novo dependente traz binário, o passo de instalação
entra em `src-tauri/vendor/README.md` com a licença que o acompanha.

**Medida é o que foi lido.** Número em comentário, em teste ou em documentação
tem de vir de um log ou de uma medição desta máquina, com o comando que o produziu.
"Deveria dar cerca de X" não entra.

## Convenções que valem

- Comentários de documentação (`//!` e `///`) em português, explicando **por que**
  a linha existe — não o que ela faz. Uma linha que só repete o código some na
  próxima revisão.
- Toda string visível vem de `locales/pt-BR.json` e `locales/en-US.json`, com as
  mesmas chaves e os mesmos placeholders nos dois arquivos. `idiomas.rs` tem dois
  testes que conferem isso; adicionar a chave em uma língua e esquecer a outra
  quebra a suíte.
- Estado do pedido (memória recuperada, pastas concedidas agora) entra na
  renderização do prompt e nunca no histórico guardado. É assim que revogar uma
  pasta muda o turno seguinte sem reescrever a conversa.
- Lock de motor não é segurado durante a geração. `ai_status` já travou a
  interface por ler a janela pelo lock do motor; qualquer comando novo que
  precisar do engine usa `acquire` e devolve antes de gerar.
- Ferramenta que o modelo pediu passa pelo gate. Não existe atalho de execução
  para o conector local: ele entra no mesmo cadastro que um servidor MCP.

## Ciclo de mudança

1. Abra uma issue descrevendo o comportamento observado, com o log ou o passo a
   passo. Comportamento esperado e medido separadamente, sem "acho que".
2. Branch a partir da `main`: `feat/…`, `fix/…`, `docs/…`.
3. Mudança com teste. Corrigir um defeito pede o teste que falhava antes de
   você mexer — se ele não existia, escreva-o primeiro e veja-o ficar vermelho.
4. `cargo test --lib` e `npm run build` verdes. Rode o aplicativo real quando a
   mudança aparece na tela: type-check não prova que a interface funciona.
5. PR pequeno, um assunto por PR. Descreva o que mudou e o que você mediu depois.

Commits: imperativo, uma frase, dizendo o porquê. `git log` do projeto é o
histórico das decisões, então "corrige janela" vale menos que "reserva tokens de
saída antes de cortar o histórico".

## Reportando vulnerabilidade

Não abra issue pública. Descreva o caso por e-mail ao mantenedor do repositório
e dê prazo antes de divulgar. Credenciais, chaves e tokens de qualquer tipo não
entram em código, teste ou fixture — nem em formato falso que pareça real, porque
os varredores de segredo derrubam o push inteiro e o fixture é o problema, não o
varredor.

## Licença de contribuição

Ao enviar uma mudança, você declara que pode licenciá-la sob a Apache-2.0 do
repositório e que ela chega sem exigência de CLA separada.
