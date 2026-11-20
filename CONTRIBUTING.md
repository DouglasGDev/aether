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
- **O guard do motor é segurado a geração inteira, e é assim de propósito.**
  Um turno escreve no KV cache do modelo em memória célula a célula; dois turnos
  sobre o mesmo motor não são dois turnos mais rápidos, são os tokens de um
  pedido misturados com os do outro. `acquire` devolve o guard, `run_chat_turn`
  o larga depois do último token — inclusive sobre a espera do card de
  aprovação, que é o turno pedindo licença no meio da geração. Quem precisa do
  motor espera **fora** da thread da janela: turno, `preload`, bancada e
  aplicação de adaptadores rodam em `spawn_blocking`. A ordem das travas é uma
  só — `geracao`/`adaptadores` lidos antes de tomar o motor, os caches depois.
- **Comando síncrono da UI nunca espera o motor** — nem a geração, nem uma carga.
  Ou ele lê os caches gravados no carregamento (`janela`, `acelerador`,
  `loaded_id`, `aplicados`, `ajustes`), ou tenta a trava e recusa com o motivo
  real (`erros.motorOcupado`, `erros.turnoEmAndamento`,
  `erros.turnoEmAndamentoRemover`): `try_lock`, nunca `lock()`. `ai_status` já
  travou a interface por ler a janela pelo lock do motor, e o que espera o motor
  na thread da janela segura também a aprovação que o próprio turno espera — a
  placa a 0% e ninguém clicando. Um comando novo que precise do motor durante a
  geração é `async` com `spawn_blocking`, ou lê cache; nunca bloqueia aqui.
- **Mutex envenenado não derruba a janela.** Trava de dado puro — `Vec`,
  `Option`, `HashMap`, struct escrito por atribuição inteira — recupera com
  `into_inner` (`state::trava_estado`, `session::trava_historico`): o que um
  panic pode deixar ali é o valor de antes ou o de depois, nunca um valor
  partido, e panicar na thread que desenha fecha a aplicação com a conversa
  intacta no disco. A trava do motor não recupera nada: sob ela os pesos são
  montados delta a delta, então o payload pode estar pela metade — descarta-se o
  motor e devolve-se o motivo real (`state::AiRuntime::trava_motor`).
  **Esta regra é conferida no próprio fonte**: `src-tauri/src/contrato.rs` varre
  os arquivos de produção e falha `cargo test --lib` em qualquer `.lock()` /
  `.read()` / `.write()` seguido de `expect`/`unwrap` fora de `#[cfg(test)]`. O
  erro imprime `arquivo:linha → fonte` para cada um, com a política a tomar. Uma
  trava que não pode recuperar sem deixar um payload pela metade tem de mudar de
  ordem ou de construção — não de regra; ver `docs/melhorias/CONTRATO.md`, C4.
- Ferramenta que o modelo pediu passa pelo gate. Não existe atalho de execução
  para o conector local: ele entra no mesmo cadastro que um servidor MCP.
- Entrada nova no catálogo de modelos entra medida, não estimada: os bytes vêm do
  `tree/main` do HuggingFace arquivo por arquivo, a janela e a licença vêm do
  cabeçalho GGUF, e a arquitetura tem de estar registrada na `llama.dll`
  vendorizada — `every_entry_is_an_architecture_the_engine_runs` confere isso
  lendo o binário. O que diz "Apache-2.0" ou "5,3 GB" sem ter lido a fonte é
  propaganda dentro de código.
- O prompt do sistema é curto porque foi medido: o Granite 4.2 3B passa a
  ignorar a pergunta quando o prompt é longo ou em outra língua. Acrescentar uma
  frase aqui exige rodar a conversa de verdade depois, não só o `cargo test`.

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
