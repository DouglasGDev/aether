//! Catálogo de servidores MCP prontos e o instalador que roda de verdade.
//!
//! A lista não é enfeite: cada entrada é um pacote npm com versão fixa, o
//! instalador chama `npm install` no diretório do aplicativo, e o que volta de
//! lá é exatamente o comando que o gerente vai executar. Nada é simulado e nada
//! depende de serviço de terceiro em tempo de execução: depois de instalado, o
//! servidor é um processo local na máquina do usuário.

use crate::connectors::{Connector, ConnectorCategory, ConnectorKind, McpAuth, McpEntry};
use crate::idiomas::tr;
use crate::state::trava_estado;
use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;

/// Todos os servidores prontos vêm deste repositório público; a URL é a
/// referência que o usuário pode abrir para conferir o que instalou.
pub const REPOSITORIO: &str = "https://github.com/modelcontextprotocol/servers";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Escopo {
    /// O servidor não toca no sistema de arquivos do usuário.
    Nenhum,
    /// O servidor recebe como argumentos as pastas concedidas: a concessão de
    /// pastas do Aether é o que limita o servidor, não uma rota à parte.
    PastasConcedidas,
}

#[derive(Debug, Clone, Copy)]
pub struct ServidorPronto {
    pub id: &'static str,
    pub pacote: &'static str,
    /// Versão fixa. Instalar `latest` faria o comportamento do aplicativo mudar
    /// sozinho entre duas execuções.
    pub versao: &'static str,
    pub categoria: ConnectorCategory,
    pub escopo: Escopo,
    pub rotas: &'static [&'static str],
}

pub const PRONTOS: &[ServidorPronto] = &[
    ServidorPronto {
        id: "filesystem",
        pacote: "@modelcontextprotocol/server-filesystem",
        versao: "2.3.0",
        categoria: ConnectorCategory::Files,
        escopo: Escopo::PastasConcedidas,
        rotas: &["arquivos", "pastas", "files", "filesystem"],
    },
    ServidorPronto {
        id: "memory",
        pacote: "@modelcontextprotocol/server-memory",
        versao: "2.1.1",
        categoria: ConnectorCategory::Data,
        escopo: Escopo::Nenhum,
        rotas: &["memoria", "lembrar", "knowledge", "graph"],
    },
    ServidorPronto {
        id: "thinking",
        pacote: "@modelcontextprotocol/server-sequential-thinking",
        versao: "2.1.1",
        categoria: ConnectorCategory::Productivity,
        escopo: Escopo::Nenhum,
        rotas: &["raciocinio", "etapas", "pensar", "thinking"],
    },
    ServidorPronto {
        id: "everything",
        pacote: "@modelcontextprotocol/server-everything",
        versao: "2.3.0",
        categoria: ConnectorCategory::Development,
        escopo: Escopo::Nenhum,
        rotas: &["referencia", "teste", "everything"],
    },
];

pub fn pronto(id: &str) -> Option<&'static ServidorPronto> {
    PRONTOS.iter().find(|p| p.id == id)
}

pub fn catalogado(id: &str) -> Result<&'static ServidorPronto> {
    pronto(id).ok_or_else(|| anyhow!(tr("erros.servidorNaoCatalogado", &[("id", id)])))
}

/// Nome legível do servidor no idioma atual. O arquivo `mcp.json` guarda o id;
/// o nome é sempre derivado daqui, senão trocar de idioma deixaria o servidor
/// instalado falando a língua antiga.
pub fn nome(pronto: &ServidorPronto) -> String {
    tr(&format!("servidores.pronto.{}.nome", pronto.id), &[])
}

pub fn descricao(pronto: &ServidorPronto) -> String {
    tr(&format!("servidores.pronto.{}.descricao", pronto.id), &[])
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ServidorEvent {
    /// `instalando` separa a escrita nova da remoção: a barra e o rótulo da
    /// linha não devem adivinhar o que o aplicativo está fazendo.
    Iniciou { id: String, instalando: bool },
    Linha { id: String, texto: String },
    Concluiu { id: String, versao: String },
    Removeu { id: String },
    Falhou { id: String, erro: String },
    Cancelou { id: String },
}

pub fn node_modules(raiz: &Path) -> PathBuf {
    raiz.join("node_modules")
}

fn caminho_do_pacote(raiz: &Path, pacote: &str) -> PathBuf {
    empilhar(&node_modules(raiz), pacote)
}

/// `push` acrescenta a barra da frente e deixa as de dentro como vieram: um
/// pacote com escopo (`@org/nome`) ou um `bin` relativo (`dist/index.js`) sairia
/// do cadastro com separadores mistos. Empilhando um componente por vez, o
/// caminho inteiro fica na convenção do sistema.
fn empilhar(base: &Path, trecho: &str) -> PathBuf {
    trecho
        .split(['/', '\\'])
        .filter(|parte| !parte.is_empty())
        .fold(base.to_path_buf(), |mut acc, parte| {
            acc.push(parte);
            acc
        })
}

/// Versão que de fato está em disco, ou None quando o pacote não foi instalado.
pub fn versao_instalada(raiz: &Path, pacote: &str) -> Option<String> {
    let arquivo = caminho_do_pacote(raiz, pacote).join("package.json");
    let texto = std::fs::read_to_string(arquivo).ok()?;
    let json: serde_json::Value = serde_json::from_str(&texto).ok()?;
    json.get("version")?.as_str().map(|v| v.to_string())
}

pub fn esta_instalado(raiz: &Path, pronto: &ServidorPronto) -> bool {
    versao_instalada(raiz, pronto.pacote).as_deref() == Some(pronto.versao)
}

/// O `bin` do pacote. Um único bin é tomado direto; com vários, o nome do
/// pacote precisa bater — adivinhar qual script rodar executaria código errado.
fn roteiro_de_bin<'a>(bin: &'a serde_json::Value, pacote: &str) -> Result<&'a str> {
    let ultimo = pacote.rsplit('/').next().unwrap_or(pacote);
    match bin {
        serde_json::Value::String(texto) => Ok(texto.as_str()),
        serde_json::Value::Object(mapa) if mapa.len() == 1 => {
            Ok(mapa.values().next().and_then(|v| v.as_str()).ok_or_else(|| {
                anyhow!(tr("erros.binInvalido", &[("pacote", pacote)]))
            })?)
        }
        serde_json::Value::Object(mapa) => mapa
            .get(ultimo)
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!(tr("erros.binAmbiguo", &[("pacote", pacote)]))),
        _ => Err(anyhow!(tr("erros.binAusente", &[("pacote", pacote)]))),
    }
}

/// Caminho absoluto do script que abre o servidor MCP.
pub fn entrada_do_pacote(raiz: &Path, pacote: &str) -> Result<PathBuf> {
    let dir = caminho_do_pacote(raiz, pacote);
    let manifesto = dir.join("package.json");
    let texto = std::fs::read_to_string(&manifesto)
        .map_err(|_| anyhow!(tr("erros.pacoteNaoInstalado", &[("pacote", pacote)])))?;
    let json: serde_json::Value = serde_json::from_str(&texto).map_err(|e| {
        anyhow!(tr("erros.jsonInvalidoEm", &[
            ("caminho", &manifesto.display().to_string()),
            ("detalhe", &e.to_string())
        ]))
    })?;
    let bin = json
        .get("bin")
        .ok_or_else(|| anyhow!(tr("erros.binAusente", &[("pacote", pacote)])))?;
    let relativo = roteiro_de_bin(bin, pacote)?;
    let roteiro = empilhar(&dir, relativo);
    if !roteiro.is_file() {
        return Err(anyhow!(tr("erros.entradaAusente", &[
            ("arquivo", &roteiro.display().to_string())
        ])));
    }
    Ok(roteiro)
}

fn candidatos_de_node() -> Vec<PathBuf> {
    let mut lista = Vec::new();
    for variavel in ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"] {
        if let Some(pasta) = std::env::var_os(variavel) {
            lista.push(Path::new(&pasta).join("nodejs").join("node.exe"));
        }
    }
    #[cfg(unix)]
    if let Some(caminho) = std::env::var_os("PATH") {
        for pasta in std::env::split_paths(&caminho) {
            lista.push(pasta.join("node"));
        }
    }
    #[cfg(windows)]
    if let Some(caminho) = std::env::var_os("PATH") {
        for pasta in std::env::split_paths(&caminho) {
            lista.push(pasta.join("node.exe"));
        }
    }
    lista
}

/// Onde está o Node. A variável de ambiente do aplicativo não é consultada com
/// shell: o caminho é resolvido aqui e vai como argumento do processo filho.
pub fn localizar_node() -> Result<PathBuf> {
    candidatos_de_node()
        .into_iter()
        .find(|p| p.is_file())
        .ok_or_else(|| anyhow!(tr("erros.nodeNaoEncontrado", &[])))
}

/// O arquivo `npm-cli.js` que vem com o próprio Node. Invocar `npm.cmd` pediria
/// um shell; chamar o script com o binário que já localizamos não pede.
pub fn npm_cli(node: &Path) -> Result<PathBuf> {
    let pasta = node.parent().ok_or_else(|| {
        anyhow!(tr("erros.npmNaoEncontrado", &[("detalhe", &node.display().to_string())]))
    })?;
    for relativo in [
        "node_modules/npm/bin/npm-cli.js",
        "../lib/node_modules/npm/bin/npm-cli.js",
    ] {
        let candidato = pasta.join(relativo);
        if candidato.is_file() {
            return Ok(candidato);
        }
    }
    Err(anyhow!(tr("erros.npmNaoEncontrado", &[
        ("detalhe", &pasta.display().to_string())
    ])))
}

fn argumentos_de_instalacao(raiz: &Path, pronto: &ServidorPronto) -> Vec<String> {
    vec![
        "install".to_string(),
        "--global=false".to_string(),
        "--prefix".to_string(),
        raiz.display().to_string(),
        "--no-audit".to_string(),
        "--no-fund".to_string(),
        "--no-update-notifier".to_string(),
        "--loglevel=http".to_string(),
        format!("{}@{}", pronto.pacote, pronto.versao),
    ]
}

fn argumentos_de_remocao(raiz: &Path, pronto: &ServidorPronto) -> Vec<String> {
    vec![
        "uninstall".to_string(),
        "--global=false".to_string(),
        "--prefix".to_string(),
        raiz.display().to_string(),
        "--no-audit".to_string(),
        "--no-fund".to_string(),
        pronto.pacote.to_string(),
    ]
}

/// A linha de comando do processo do npm, na ordem. O `npm-cli.js` precisa vir
/// antes de "install": sem ele o Node tentaria executar a palavra como script.
fn linha_do_npm(node: &Path, cli: &Path, argumentos: &[String]) -> Vec<std::ffi::OsString> {
    let mut linha = vec![node.as_os_str().to_os_string(), cli.as_os_str().to_os_string()];
    linha.extend(argumentos.iter().map(std::ffi::OsString::from));
    linha
}

/// Roda `npm` e entrega cada linha que sai, cancelando pelo sinal do usuário.
async fn rodar_npm(
    node: &Path,
    cli: &Path,
    argumentos: &[String],
    cancel: &AtomicBool,
    mut on_line: impl FnMut(&str),
) -> Result<String> {
    let linha = linha_do_npm(node, cli, argumentos);
    let mut cmd = Command::new(&linha[0]);
    cmd.args(&linha[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: nada de console na tela.

    let mut filho = cmd
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            anyhow!(tr("erros.falhaIniciarInstalador", &[("detalhe", &e.to_string())]))
        })?;

    let stdout = filho.stdout.take().ok_or_else(|| {
        anyhow!(tr("erros.falhaIniciarInstalador", &[("detalhe", "stdout")]))
    })?;
    let mut stderr = filho.stderr.take().ok_or_else(|| {
        anyhow!(tr("erros.falhaIniciarInstalador", &[("detalhe", "stderr")]))
    })?;

    // O cano de stderr cabe alguns kilobytes: quem escreve mais que isso para
    // até alguém ler. Ler o despejo depois do `wait()` era esperar que um
    // processo bloqueado terminasse sozinho — o instalador pendurava para
    // sempre num npm que só fala pelos warnings. A leitura corre ao lado.
    let derrame = tokio::spawn(async move {
        let mut texto = String::new();
        let _ = stderr.read_to_string(&mut texto).await;
        texto
    });

    let mut saida = String::new();
    let mut linhas = BufReader::new(stdout).lines();
    loop {
        if cancel.load(Ordering::Relaxed) {
            derrame.abort();
            let _ = filho.start_kill();
            let _ = filho.wait().await;
            bail!(tr("erros.instalacaoCancelada", &[]));
        }
        // O intervalo curto existe para o cancelamento valer mesmo se o npm
        // emudecer: sem ele ficaríamos presos na leitura da próxima linha.
        tokio::select! {
            l = linhas.next_line() => match l? {
                Some(texto) => {
                    saida.push_str(&texto);
                    saida.push('\n');
                    on_line(texto.trim_end());
                }
                None => break,
            },
            _ = tokio::time::sleep(std::time::Duration::from_millis(200)) => {}
        }
    }

    let status = filho.wait().await.context(tr("erros.instalacaoSemSaida", &[]))?;
    // O cano fecha quando o processo morre, então esta espera termina. Uma
    // tarefa que estourou deixa o stderr vazio, e a mensagem cai na saída
    // padrão — o mesmo caminho de antes, quando não havia nada para ler.
    let erro = derrame.await.unwrap_or_default();
    if !status.success() {
        let cauda = erro.trim();
        return Err(anyhow!(tr("erros.instaladorFalhou", &[
            ("codigo", &status.code().unwrap_or(-1).to_string()),
            ("detalhe", if cauda.is_empty() { saida.trim_end() } else { cauda })
        ])));
    }
    Ok(saida)
}

/// Instala o pacote pinçado no diretório do aplicativo.
pub async fn instalar(
    pronto: &ServidorPronto,
    raiz: &Path,
    cancel: &AtomicBool,
    on_line: impl FnMut(&str),
) -> Result<()> {
    std::fs::create_dir_all(raiz)
        .with_context(|| tr("erros.falhaCriarPasta", &[("caminho", &raiz.display().to_string())]))?;
    // Reinstalar o que já está exatamente na versão pinçada não é refazer o
    // trabalho da rede: é o caminho do "Adicionar" de um servidor baixado antes.
    if esta_instalado(raiz, pronto) {
        // A versão no manifesto sozinha não prova nada: sem o script em disco
        // não há o que executar, e isso é um erro de verdade.
        entrada_do_pacote(raiz, pronto.pacote)?;
        return Ok(());
    }
    let node = localizar_node()?;
    let cli = npm_cli(&node)?;
    let argumentos = argumentos_de_instalacao(raiz, pronto);
    rodar_npm(&node, &cli, &argumentos, cancel, on_line).await?;

    // A prova de que instalou é o que se executa: o script do servidor precisa
    // estar em disco antes de dizermos ao usuário que está pronto.
    let roteiro = entrada_do_pacote(raiz, pronto.pacote)?;
    if versao_instalada(raiz, pronto.pacote).as_deref() != Some(pronto.versao) {
        bail!(tr("erros.versaoDivergente", &[
            ("pacote", pronto.pacote),
            ("esperada", pronto.versao),
            ("medida", &versao_instalada(raiz, pronto.pacote).unwrap_or_default()),
            ("arquivo", &roteiro.display().to_string())
        ]));
    }
    Ok(())
}

pub async fn desinstalar(
    pronto: &ServidorPronto,
    raiz: &Path,
    cancel: &AtomicBool,
    on_line: impl FnMut(&str),
) -> Result<()> {
    let node = localizar_node()?;
    let cli = npm_cli(&node)?;
    let argumentos = argumentos_de_remocao(raiz, pronto);
    rodar_npm(&node, &cli, &argumentos, cancel, on_line).await?;
    // O npm pode sair com código zero e deixar a árvore no lugar; o que vale é
    // o manifesto ter sumido.
    if let Some(resto) = versao_instalada(raiz, pronto.pacote) {
        bail!(tr("erros.remocaoIncompleta", &[
            ("pacote", pronto.pacote),
            ("versao", &resto)
        ]));
    }
    Ok(())
}

/// O transporte stdio do servidor instalado: `node <script>`.
///
/// As pastas não entram aqui. O que vai no `mcp.json` é o servidor; a concessão
/// de pastas é lida no momento de conectar, senão uma pasta revogada continuaria
/// acessível até o usuário reinstalar o pacote.
pub fn transporte(pronto: &ServidorPronto, raiz: &Path) -> Result<McpEntry> {
    let node = localizar_node()?;
    transporte_com(&node, pronto, raiz)
}

fn transporte_com(node: &Path, pronto: &ServidorPronto, raiz: &Path) -> Result<McpEntry> {
    let roteiro = entrada_do_pacote(raiz, pronto.pacote)?;
    Ok(McpEntry::Stdio {
        command: node.display().to_string(),
        args: vec![roteiro.display().to_string()],
        env: Default::default(),
    })
}

/// Injeta as pastas concedidas no servidor que só enxerga o que recebe nos
/// argumentos. Sem nenhuma concessão ele nem deve subir: do contrário rodaria
/// com acesso a todo o disco.
pub fn com_pastas(pronto: &ServidorPronto, entry: McpEntry, pastas: &[String]) -> Result<McpEntry> {
    if pronto.escopo != Escopo::PastasConcedidas {
        return Ok(entry);
    }
    if pastas.is_empty() {
        bail!(tr("erros.pastasNecessarias", &[("servidor", &nome(pronto))]));
    }
    let McpEntry::Stdio { command, mut args, env } = entry else {
        // Um endereço de rede não recebe pastas: se o arquivo de configuração foi
        // editado à mão assim, o servidor não tem como ser contido.
        bail!(tr("erros.escopoSemStdio", &[("servidor", &nome(pronto))]));
    };
    args.extend(pastas.iter().cloned());
    Ok(McpEntry::Stdio { command, args, env })
}

/// O conector que vai para o cadastro. Nome e descrição são derivados do idioma
/// atual; `config::registry` refaz essa derivação a cada leitura, então trocar de
/// idioma retraduz o servidor instalado sem reescrever o arquivo.
pub fn conector(pronto: &ServidorPronto, raiz: &Path) -> Result<Connector> {
    conector_com(&localizar_node()?, pronto, raiz)
}

fn conector_com(node: &Path, pronto: &ServidorPronto, raiz: &Path) -> Result<Connector> {
    Ok(Connector {
        id: pronto.id.to_string(),
        name: nome(pronto),
        description: descricao(pronto),
        kind: ConnectorKind::Mcp,
        category: pronto.categoria,
        docs_url: REPOSITORIO.to_string(),
        routes: pronto.rotas.iter().map(|r| r.to_string()).collect(),
        auth: McpAuth::None,
        entry: Some(transporte_com(node, pronto, raiz)?),
        local: HashMap::new(),
        fields: vec![],
        env: vec![],
        tool_allowlist: None,
    })
}

/// O que a interface precisa para desenhar uma linha de catálogo.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Ficha {
    pub id: String,
    pub nome: String,
    pub descricao: String,
    pub categoria: ConnectorCategory,
    pub pacote: String,
    pub versao: String,
    /// A versão que de fato está em disco: None quando nunca foi instalado,
    /// e um valor diferente de `versao` quando há uma instalação ultrapassada.
    pub instalada: Option<String>,
    pub requer_pastas: bool,
    pub rotas: Vec<String>,
    pub docs_url: String,
}

pub fn fichas(raiz: &Path) -> Vec<Ficha> {
    PRONTOS
        .iter()
        .map(|p| Ficha {
            id: p.id.to_string(),
            nome: nome(p),
            descricao: descricao(p),
            categoria: p.categoria,
            pacote: p.pacote.to_string(),
            versao: p.versao.to_string(),
            instalada: versao_instalada(raiz, p.pacote),
            requer_pastas: p.escopo == Escopo::PastasConcedidas,
            rotas: p.rotas.iter().map(|r| r.to_string()).collect(),
            docs_url: REPOSITORIO.to_string(),
        })
        .collect()
}

/// Um cancelador por instalação em curso. Duas tarefas no mesmo pacote
/// escreveriam na mesma árvore de `node_modules`.
#[derive(Default)]
pub struct Instalacoes {
    jobs: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

impl Instalacoes {
    pub fn novo() -> Self {
        Self::default()
    }

    /// None quando já há uma tarefa para este servidor.
    pub fn iniciar(&self, id: &str) -> Option<Arc<AtomicBool>> {
        let mut jobs = trava_estado(&self.jobs);
        if jobs.contains_key(id) {
            return None;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        jobs.insert(id.to_string(), Arc::clone(&cancel));
        Some(cancel)
    }

    pub fn encerrar(&self, id: &str) {
        trava_estado(&self.jobs).remove(id);
    }

    pub fn cancelar(&self, id: &str) -> bool {
        match trava_estado(&self.jobs).get(id) {
            Some(cancel) => {
                cancel.store(true, Ordering::Relaxed);
                true
            }
            None => false,
        }
    }

    pub fn em_curso(&self) -> Vec<String> {
        let mut ids: Vec<String> = trava_estado(&self.jobs).keys().cloned().collect();
        ids.sort();
        ids
    }
}

#[cfg(test)]
mod testes {
    use super::*;
    use std::fs;

    fn pasta_temporaria(nome: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aether-mcp-{}-{nome}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("pasta de teste");
        dir
    }

    /// O cano de stderr de um processo tem poucos kilobytes. Quem escreve mais
    /// que isso para de escrever até alguém ler, e a leitura do stderr estava
    /// depois do `wait()`: quem deveria ler está esperando o processo terminar.
    /// Um npm que despeja warnings vira um instalador que nunca volta, e a única
    /// saída era o *Parar*. Este teste é o travamento, medido pelo prazo.
    #[tokio::test]
    async fn um_filho_que_inunda_o_stderr_nao_prende_o_instalador() {
        let dir = pasta_temporaria("stderr-cheio");
        let arquivo = dir.join("despejo.txt");
        fs::write(&arquivo, "x".repeat(200_000)).expect("arquivo de despejo");

        let cancel = AtomicBool::new(false);
        let despejo = [format!("type {} 1>&2", arquivo.display())];
        let corrida = rodar_npm(
            Path::new("cmd"),
            Path::new("/C"),
            &despejo,
            &cancel,
            |_| {},
        );
        let prazo = tokio::time::timeout(std::time::Duration::from_secs(10), corrida).await;
        let _ = fs::remove_dir_all(&dir);

        let terminou = prazo.unwrap_or_else(|_| {
            panic!("o instalador ficou 10 s pendurado num processo que só despejou stderr")
        });
        terminou.expect("o despejo de stderr saiu com erro");
    }

    /// Cria o esqueleto de um pacote npm instalado de verdade: manifesto e o
    /// script que ele declara, porque sem o arquivo em disco o instalador não
    /// garante nada.
    fn pacote(raiz: &Path, nome: &str, manifesto: &str) -> PathBuf {
        let dir = empilhar(&node_modules(raiz), nome);
        fs::create_dir_all(dir.join("dist")).expect("estrutura do pacote");
        fs::write(dir.join("package.json"), manifesto).expect("manifesto");
        fs::write(dir.join("dist").join("index.js"), "#!/usr/bin/env node\n")
            .expect("script do servidor");
        dir
    }

    #[test]
    fn ids_sao_unicos_e_as_versoes_pinadas() {
        let mut vistos = std::collections::HashSet::new();
        for p in PRONTOS {
            assert!(vistos.insert(p.id), "id repetido: {}", p.id);
            assert!(!p.pacote.is_empty());
            assert!(
                p.versao.chars().next().is_some_and(|c| c.is_ascii_digit()),
                "{} não pinça uma versão numérica",
                p.id
            );
            assert!(!p.versao.contains('*'), "{} usa coringa", p.versao);
            assert!(!p.rotas.is_empty(), "{} não tem rota de busca", p.id);
        }
    }

    #[test]
    fn todo_servidor_pronto_tem_nome_e_descricao_em_todo_idioma() {
        for p in PRONTOS {
            // Chave ausente devolve a própria chave: é o defeito visível que o
            // teste procura, não um texto inventado pelo aplicativo.
            let nome = nome(p);
            let descricao = descricao(p);
            assert!(!nome.starts_with("servidores."), "{} sem nome traduzido", p.id);
            assert!(!descricao.starts_with("servidores."), "{} sem descrição", p.id);
        }
    }

    #[test]
    fn um_bin_unico_e_aceito_sem_conversar() {
        let raiz = pasta_temporaria("bin-unico");
        pacote(&raiz, "@modelcontextprotocol/server-memory", r#"{"bin":"dist/index.js"}"#);
        let achado = entrada_do_pacote(&raiz, "@modelcontextprotocol/server-memory").unwrap();
        assert!(achado.ends_with("index.js"));
        fs::remove_dir_all(&raiz).ok();
    }

    #[test]
    fn bin_em_mapa_de_uma_chave_pegado_direto() {
        let raiz = pasta_temporaria("bin-mapa");
        pacote(
            &raiz,
            "@modelcontextprotocol/server-filesystem",
            r#"{"bin":{"mcp-server-filesystem":"dist/index.js"}}"#,
        );
        assert!(
            entrada_do_pacote(&raiz, "@modelcontextprotocol/server-filesystem")
                .unwrap()
                .is_file()
        );
        fs::remove_dir_all(&raiz).ok();
    }

    #[test]
    fn o_caminho_grafado_nao_mistura_separadores() {
        let raiz = pasta_temporaria("separadores");
        pacote(
            &raiz,
            "@modelcontextprotocol/server-filesystem",
            r#"{"bin":{"mcp-server-filesystem":"dist/index.js"}}"#,
        );
        let achado = entrada_do_pacote(&raiz, "@modelcontextprotocol/server-filesystem")
            .unwrap()
            .to_string_lossy()
            .into_owned();
        // O pacote vem com escopo e o bin com barra: se forem empilhados de uma
        // vez, o cadastro guarda `...\@org\nome\dist/index.js` e a linha de
        // comando do filho fica com as duas convenções no mesmo argumento.
        if cfg!(windows) {
            assert!(!achado.contains('/'), "separador misto em {achado}");
        }
        assert!(achado.ends_with("index.js"));
        fs::remove_dir_all(&raiz).ok();
    }

    #[test]
    fn dois_bins_sem_correspondencia_recusados_em_vez_de_escolher() {
        let raiz = pasta_temporaria("bin-ambiguo");
        pacote(
            &raiz,
            "coisa",
            r#"{"bin":{"um":"dist/um.js","dois":"dist/dois.js"}}"#,
        );
        let erro = entrada_do_pacote(&raiz, "coisa").unwrap_err().to_string();
        assert!(erro.contains("coisa"), "erro deveria citar o pacote: {erro}");
        fs::remove_dir_all(&raiz).ok();
    }

    #[test]
    fn script_declorado_mas_ausente_de_disco_e_erro() {
        let raiz = pasta_temporaria("sem-script");
        pacote(&raiz, "fantasma", r#"{"bin":"dist/nao-existe.js"}"#);
        let erro = entrada_do_pacote(&raiz, "fantasma").unwrap_err().to_string();
        assert!(!erro.is_empty());
        fs::remove_dir_all(&raiz).ok();
    }

    #[test]
    fn pacote_que_nao_foi_instalado_diz_isso() {
        let raiz = pasta_temporaria("vazio");
        let erro = entrada_do_pacote(&raiz, "ausente").unwrap_err().to_string();
        assert!(!erro.starts_with("erros."));
        assert_eq!(versao_instalada(&raiz, "ausente"), None);
        fs::remove_dir_all(&raiz).ok();
    }

    #[test]
    fn versao_instalada_lida_o_manifesto() {
        let raiz = pasta_temporaria("versao");
        pacote(&raiz, "coisa", r#"{"version":"1.2.3","bin":"dist/index.js"}"#);
        fs::write(node_modules(&raiz).join("coisa").join("dist").join("index.js"), "")
            .unwrap();
        assert_eq!(versao_instalada(&raiz, "coisa").as_deref(), Some("1.2.3"));
        let p = ServidorPronto {
            id: "coisa",
            pacote: "coisa",
            versao: "1.2.3",
            categoria: ConnectorCategory::Data,
            escopo: Escopo::Nenhum,
            rotas: &["coisa"],
        };
        assert!(esta_instalado(&raiz, &p));
        fs::remove_dir_all(&raiz).ok();
    }

    #[test]
    fn instalacao_e_remocao_nao_passam_por_shell() {
        let p = *PRONTOS.first().unwrap();
        let args = argumentos_de_instalacao(Path::new("C:/srv"), &p);
        let joined = args.join(" ");
        assert!(!joined.contains("&&") && !joined.contains('|'), "{joined}");
        assert!(args.iter().any(|a| a == &format!("{}@{}", p.pacote, p.versao)));
        let remove = argumentos_de_remocao(Path::new("C:/srv"), &p);
        assert_eq!(remove.first().map(String::as_str), Some("uninstall"));
        // Pacote com escopo começa em "@": o que não pode haver é uma versão
        // colada, porque remover "pkg@1.2.3" deixa o resto da árvore no disco.
        let alvo = remove.last().map(String::as_str).unwrap();
        assert_eq!(alvo, p.pacote);
        assert!(!alvo.ends_with(&format!("@{}", p.versao)), "{alvo} pede versão");
    }

    #[test]
    fn comando_npm_precede_o_cli_com_o_node_localizado() {
        let node = Path::new("C:/arquivos/nodejs/node.exe");
        let cli = Path::new("C:/arquivos/nodejs/node_modules/npm/bin/npm-cli.js");
        let args = vec!["install".to_string(), "--no-audit".to_string()];
        let linha = linha_do_npm(node, cli, &args);
        assert_eq!(linha.len(), 4, "node, cli e os dois argumentos");
        assert_eq!(linha[0], node.as_os_str());
        assert_eq!(linha[1], cli.as_os_str());
        assert_eq!(linha[2], "install");
        assert_eq!(linha[3], "--no-audit");
    }

    fn node_ficticio() -> &'static Path {
        Path::new("C:/arquivos/nodejs/node.exe")
    }

    #[test]
    fn o_que_fica_no_cadastro_nao_carrega_pastas() {
        // Se a concessão fosse gravada aqui, revogar uma pasta só pegaria na
        // próxima reinstalação. O cadastro guarda o servidor; a pasta entra na
        // hora de conectar.
        let raiz = pasta_temporaria("transporte");
        let p = *PRONTOS.first().unwrap();
        pacote(&raiz, p.pacote, r#"{"bin":"dist/index.js"}"#);
        let McpEntry::Stdio { command, args, .. } =
            transporte_com(node_ficticio(), &p, &raiz).unwrap()
        else {
            panic!("stdio");
        };
        assert_eq!(command, node_ficticio().display().to_string());
        assert_eq!(args.len(), 1, "somente o script do servidor");
        fs::remove_dir_all(&raiz).ok();
    }

    #[test]
    fn escopo_de_pastas_recebe_as_pastas_depois_do_script() {
        let raiz = pasta_temporaria("com-pastas");
        let p = *PRONTOS.first().unwrap();
        pacote(&raiz, p.pacote, r#"{"bin":"dist/index.js"}"#);
        let base = transporte_com(node_ficticio(), &p, &raiz).unwrap();

        // Sem pasta concedida o servidor de arquivos nem deve subir.
        let erro = com_pastas(&p, base.clone(), &[]).unwrap_err().to_string();
        assert!(!erro.starts_with("erros."), "chave de idioma não traduzida: {erro}");

        let McpEntry::Stdio { args, .. } =
            com_pastas(&p, base, &["C:/concedido".to_string(), "D:/outra".to_string()]).unwrap()
        else {
            panic!("stdio");
        };
        assert_eq!(args.len(), 3, "script + as duas pastas");
        assert_eq!(args[1], "C:/concedido");
        assert_eq!(args[2], "D:/outra");
        fs::remove_dir_all(&raiz).ok();
    }

    #[test]
    fn servidor_sem_escopo_nao_recebe_pastas() {
        let raiz = pasta_temporaria("sem-escopo");
        let p = PRONTOS[1];
        pacote(&raiz, p.pacote, r#"{"bin":"dist/index.js"}"#);
        let base = transporte_com(node_ficticio(), &p, &raiz).unwrap();
        let McpEntry::Stdio { args, .. } =
            com_pastas(&p, base, &["C:/x".to_string()]).unwrap()
        else {
            panic!("stdio");
        };
        assert_eq!(args.len(), 1, "pastas não pertencem a este servidor");
        fs::remove_dir_all(&raiz).ok();
    }

    #[test]
    fn um_http_na_escopo_de_pastas_e_recusado() {
        // Edições à mão do mcp.json não podem transformar o servidor de
        // arquivos em um endereço sem contenção.
        let p = *PRONTOS.first().unwrap();
        let http = McpEntry::Http {
            url: "http://127.0.0.1:9/mcp".to_string(),
            headers: HashMap::new(),
        };
        let erro = com_pastas(&p, http, &["C:/x".to_string()]).unwrap_err().to_string();
        assert!(!erro.starts_with("erros."), "{erro}");
    }

    #[test]
    fn o_conector_montado_e_um_mcp_valido_para_a_registry() {
        use crate::connectors::Registry;
        let raiz = pasta_temporaria("conector");
        let p = PRONTOS[1];
        pacote(&raiz, p.pacote, r#"{"bin":"dist/index.js"}"#);
        let conector = conector_com(node_ficticio(), &p, &raiz).unwrap();
        assert_eq!(conector.id, p.id);
        assert_eq!(conector.kind, ConnectorKind::Mcp);
        assert_eq!(conector.category, p.categoria);
        assert_eq!(conector.auth, McpAuth::None);
        assert!(!conector.name.starts_with("servidores."));
        assert!(!conector.description.starts_with("servidores."));
        assert_eq!(conector.docs_url, REPOSITORIO);
        assert_eq!(conector.routes, p.rotas.to_vec());
        assert!(conector.has_entry());
        // Um conector montado assim passa na validação da registry.
        Registry::new(vec![conector]).expect("conector pronto deve ser registrável");
        fs::remove_dir_all(&raiz).ok();
    }

    #[test]
    fn a_ficha_diz_o_que_esta_em_disco_agora() {
        let raiz = pasta_temporaria("ficha");
        let p = *PRONTOS.first().unwrap();
        assert_eq!(fichas(&raiz)[0].instalada, None, "nada instalado ainda");

        pacote(&raiz, p.pacote, &format!(r#"{{"version":"{}","bin":"dist/index.js"}}"#, p.versao));
        let ficha = fichas(&raiz).into_iter().find(|f| f.id == p.id).unwrap();
        assert_eq!(ficha.instalada.as_deref(), Some(p.versao));
        assert_eq!(ficha.versao, p.versao);
        assert!(ficha.requer_pastas, "o servidor de arquivos é contido por pastas");
        assert!(!ficha.nome.starts_with("servidores."));
        fs::remove_dir_all(&raiz).ok();
    }

    #[test]
    fn duas_instalacoes_do_mesmo_servidor_nao_convivem() {
        let jobs = Instalacoes::novo();
        let primeira = jobs.iniciar("filesystem").expect("primeira instalação entra");
        assert!(jobs.iniciar("filesystem").is_none(), "segunda escrita no mesmo node_modules");
        assert!(jobs.iniciar("memory").is_some(), "servidores diferentes não se atrapalham");
        assert_eq!(jobs.em_curso(), vec!["filesystem".to_string(), "memory".to_string()]);

        assert!(jobs.cancelar("filesystem"));
        assert!(primeira.load(Ordering::Relaxed), "o cancelador é o mesmo flag");
        jobs.encerrar("filesystem");
        assert!(!jobs.cancelar("filesystem"), "nada a cancelar depois de encerrar");
        assert_eq!(jobs.em_curso(), vec!["memory".to_string()]);
    }
}
