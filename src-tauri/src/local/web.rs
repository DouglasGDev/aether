//! A busca na web: o único caminho do Aether para fora da máquina que não é
//! download de modelo.
//!
//! O contrato do produto (`README.md:4-8`) diz que não há chamada a serviço de
//! terceiros em nenhum caminho de execução, e ele é verdade: até este arquivo, a
//! única rede do aplicativo era o `ai/download.rs`, que traz pesos para o disco e
//! existe porque o modelo de conversa mora na máquina. Este módulo abre uma
//! segunda porta — e por isso ele é a parte do código que mais tem de provar que
//! não é uma terceira. O que sai daqui, literalmente:
//!
//! - o texto da consulta que o modelo pediu, percent-codificado dentro do
//!   parâmetro `q` (ou `gsrsearch`) de uma URL fixa;
//! - o cabeçalho `User-Agent` com a versão do aplicativo;
//! - o nome do host, que o TLS entrega.
//!
//! O que não sai: arquivo nenhum, conversa nenhuma, prompt nenhum, caminho
//! nenhum, chave nenhuma, modelo nenhum, e nenhum dado que a máquina tenha. A
//! ferramenta tem uma entrada só — `consultaRede` — e ela só aparece no corpo da
//! requisição; o resto da URL é constante deste arquivo. Um pedido que contenha
//! `?q=x&redirect=http://evil`, `/../../etc/passwd` ou `javascript:alert(1)` não
//! muda o host, o esquema nem o caminho: cai na barra do codificador e vira
//! texto a procurar, que é o que ele é. O teste `a_consulta_nao_abre_rota` é o
//! que fecha isso.
//!
//! Ligada por fora, ela não é: `rede.json` diz se existe, e `habilitada()` é o
//! que decide se a ferramenta aparece no catálogo e se ela executa. Desligada —
//! que é o estado de fábrica, e o estado de quem nunca abriu a pasta de
//! configuração — não há `buscar_na_web` para o modelo ver e chamá-la devolve o
//! motivo. E mesmo ligada, o efeito é `ToolEffect::Rede`, que nenhum modo
//! aprova sozinho (`permissions::evaluate`): cada saída tem um card. São três
//! travas independentes — o arquivo, o catálogo e o gate — porque uma porta de
//! rede que depende de uma só trava é uma porta aberta.
//!
//! ## As palavras deste módulo
//!
//! Toda frase visível sai por `frase()` e `erro()`, que montam a chave em tempo
//! de execução (`rede.motivo.host`, `erros.redeResposta`, …) em vez de escrevê-la
//! literal — a mesma saída que `ferramenta()` e `campo()` já usam para as
//! famílias compostas. Como uma chave montada por `format!` cai fora da varredura
//! de `idiomas.rs:307`, quem cobre a família inteira é o teste
//! `toda_chave_de_rede_tem_texto_em_todo_idioma`: ele atravessa a lista `CHAVES`
//! e exige texto nos dois catálogos, então renomear uma variante de `Motivo` sem
//! publicar a frase correspondente quebra aqui, com a chave na mensagem.
//!
//! ## Por que estes dois hosts
//!
//! A lista é hard-coded e não tem chave nem conta:
//!
//! - `html.duckduckgo.com` — a página HTML de resultados do DuckDuckGo. Não
//!   existe produto oficial ali (o `Instant Answer API` foi desligado e a API de
//!   busca séria é paga), então o que se usa é o HTML público, que dispensa
//!   chave. É um endpoint não versionado: ele pode mudar de marcação num
//!   trimestre e o parser devolver lista vazia. Ele devolve vazio, devolve a
//!   falha no cartão e não inventa resultado — que é o mínimo honesto contra um
//!   HTML que ninguém prometeu.
//! - `pt.wikipedia.org` — a API de ação do MediaWiki (`/w/api.php`), documentada,
//!   sem chave, com JSON e URL canônica no corpo. É o fornecedor que segura o
//!   resultado quando o outro mudar a marcação.
//!
//! Os dois são HTTPS e os dois são de terceiros: o que eles respondem não é
//! verificado por nada aqui, e é por isso que o cartão diz de onde os bytes
//! vieram em vez de apresentá-los como se fossem da máquina.

use crate::idiomas::tr;
use crate::local::vista::Par;
use crate::local::{Ferramenta, Saida, Vista, campo, inteiro, objeto, obrigatorio};
use crate::state::{trava_escrita, trava_leitura};
use anyhow::{Context, Result, anyhow, bail};
use reqwest::Url;
use serde::Deserialize;
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::path::Path;
use std::sync::{OnceLock, RwLock};
use std::time::Duration;

/// O nome sob o qual a ferramenta entra no catálogo e no dispatch.
pub const FERRAMENTA: &str = "buscar_na_web";
/// A chave do arquivo de escolha, no diretório de configuração do aplicativo.
pub const ARQUIVO: &str = "rede.json";

/// O teto de resultados que a ferramenta entrega, e o teto que o esquema declara.
pub const MAX_RESULTADOS: usize = 5;
/// 512 KiB por corpo: uma página de resultados cabe, um despejo não.
pub const LIMITE_CORPO: usize = 512 * 1024;
/// Redirecionamentos aceitos, cada um conferido antes de a conexão acontecer.
pub const MAX_PULOS: usize = 2;
/// Prazo de uma requisição só — inclusive o corpo.
const PRAZO_PEDIDO: Duration = Duration::from_secs(8);
/// Prazo de abrir a conexão.
const PRAZO_CONEXAO: Duration = Duration::from_secs(4);
/// O relógio de parede da chamada inteira, pulos e parsers incluídos. Ele é
/// menor que `PRAZO_PEDIDO * (MAX_PULOS + 1)` de propósito: um host que aceita a
/// conexão e silencia em cada pulo não pode somar os prazos até o infinito.
const PRAZO_GERAL: Duration = Duration::from_secs(15);

/// Um fornecedor da lista: host, caminho fixo e como montar o pedido. Nada aqui
/// vem do modelo — a URL é construída a partir destas constantes.
struct Fornecedor {
    /// Palavra de máquina, para o cartão dizer quem falhou. Não se traduz.
    nome: &'static str,
    host: &'static str,
    /// A URL exata pedida, menos o parâmetro da consulta.
    base: &'static str,
    /// O prefixo de caminho que este host pode ter. Um host da lista com um
    /// caminho fora daqui continua recusado: a lista é de pares (host, caminho).
    prefixo: &'static str,
    /// Nome do parâmetro onde a consulta entra.
    chave_consulta: &'static str,
    /// Nome do parâmetro de limite, quando o fornecedor tem um.
    chave_limite: Option<&'static str>,
    /// Como o corpo é lido.
    meio: Meio,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Meio {
    Html,
    Json,
}

/// A lista permitida. Acrescentar um item aqui é a única forma de um endereço
/// novo virar pedido — o modelo não acrescenta nada.
const FORNECEDORES: &[Fornecedor] = &[
    Fornecedor {
        nome: "duckduckgo",
        host: "html.duckduckgo.com",
        base: "https://html.duckduckgo.com/html/",
        prefixo: "/html/",
        chave_consulta: "q",
        // O `kl` fixa a região da resposta, e `t` marca que é um uso comum do
        // endpoint: nenhum dos dois é segredo nem escolha do modelo.
        chave_limite: None,
        meio: Meio::Html,
    },
    Fornecedor {
        nome: "wikipedia",
        host: "pt.wikipedia.org",
        base: "https://pt.wikipedia.org/w/api.php",
        prefixo: "/w/api.php",
        chave_consulta: "gsrsearch",
        chave_limite: Some("gsrlimit"),
        meio: Meio::Json,
    },
];

/// Os hosts da lista, para a tela e para o teste dizerem qual é a política.
pub fn hosts_permitidos() -> Vec<&'static str> {
    FORNECEDORES.iter().map(|f| f.host).collect()
}

// ---------------------------------------------------------------- a escolha --

/// O que a pessoa ligou em `rede.json`. Uma chave só, e ela é um botão de rede:
/// não há lista de hosts editável por aqui, porque uma allowlist que o usuário
/// edita deixa de ser allowlist.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rede {
    /// Desligado é o estado de fábrica: um arquivo ausente, ilegível ou vazio
    /// devolve `false`, nunca `true`. A escolha tem de ser escrita para existir.
    #[serde(default)]
    pub ativa: bool,
}

impl Rede {
    /// Lê a escolha. Arquivo que não existe é desligado; arquivo que existe e não
    /// é JSON legível é desligado **com aviso**, porque engolir um `rede.json`
    /// quebrado como "desligado" seria a tela prometer uma coisa que o disco não
    /// diz. O aviso vai no erro de quem escreveu; a decisão continua a mesma.
    pub fn de(caminho: &Path) -> (Self, Option<String>) {
        let Ok(texto) = std::fs::read_to_string(caminho) else {
            return (Self::default(), None);
        };
        match serde_json::from_str::<Self>(&texto) {
            Ok(rede) => (rede, None),
            Err(e) => (Self::default(), Some(e.to_string())),
        }
    }

    /// Grava a escolha no caminho temporário e troca no fim, a mesma política dos
    /// outros arquivos do aplicativo: um corte no meio da escrita deixa o
    /// arquivo antigo inteiro, não um JSON pela metade.
    pub fn gravar(&self, caminho: &Path) -> Result<()> {
        if let Some(pai) = caminho.parent() {
            std::fs::create_dir_all(pai).with_context(|| {
                tr("erros.falhaCriarPasta", &[("caminho", &pai.display().to_string())])
            })?;
        }
        let texto = serde_json::to_string_pretty(self)
            .map_err(|e| anyhow!(erro("Gravar", &[("detalhe", &e.to_string())])))?;
        let temporario = caminho.with_extension("json.tmp");
        std::fs::write(&temporario, texto).with_context(|| {
            tr("erros.falhaEscreverConfig", &[("caminho", &temporario.display().to_string())])
        })?;
        std::fs::rename(&temporario, caminho).with_context(|| {
            tr("erros.falhaSubstituirConfig", &[("caminho", &caminho.display().to_string())])
        })?;
        Ok(())
    }
}

/// A frase do cartão. Montada por `format!` de propósito: o varredor de
/// `idiomas.rs:307` só cobra texto de chamada com a chave escrita direto no
/// fonte, e nenhuma chave desta família é escrita direto: elas saem por aqui,
/// montadas em tempo de execução. O censo é a lista `CHAVES` abaixo, replicada
/// em `docs/melhorias/REDE.md`. Trocar o prefixo aqui troca o catálogo inteiro.
fn frase(resto: &str, args: &[(&str, &str)]) -> String {
    tr(&format!("rede.{resto}"), args)
}

/// O motivo de uma recusa, na família `erros.rede*`, pelo mesmo motivo e com o
/// mesmo destino de `frase`.
fn erro(nome: &str, args: &[(&str, &str)]) -> String {
    tr(&format!("erros.rede{nome}"), args)
}

/// A escolha em memória. É um processo inteiro porque o catálogo é montado sem
/// caminho nenhum — `local::catalogo()` é chamado pelo gerente, pelo servidor
/// publicado e pelo laço de conversa, e nenhum deles carrega a pasta de
/// configuração. O precedente é o idioma escolhido em `idiomas.rs:67`, que tem a
/// mesma forma e a mesma razão. Quem precisa da escolha como dado — o catálogo
/// filtrado, uma chamada de teste — lê `rede_atual()` e passa adiante, em vez de
/// cada função tomar a trava por conta.
fn memoria() -> &'static RwLock<Rede> {
    static REDE: OnceLock<RwLock<Rede>> = OnceLock::new();
    REDE.get_or_init(|| RwLock::new(Rede::default()))
}

/// A escolha valendo agora, copiada da memória.
pub fn rede_atual() -> Rede {
    trava_leitura(memoria()).clone()
}

/// A escolha valendo agora. Ausente o arquivo, desligado.
pub fn habilitada() -> bool {
    trava_leitura(memoria()).ativa
}

/// Põe a escolha do disco em memória. Chamado uma vez na abertura, antes de o
/// gerente conectar o conector embutido, e é ele que decide se `buscar_na_web`
/// existe no catálogo do aplicativo inteiro.
pub fn carregar(caminho: &Path) -> Rede {
    let (rede, aviso) = Rede::de(caminho);
    *trava_escrita(memoria()) = rede.clone();
    if let Some(detalhe) = aviso {
        eprintln!("{}: {}: {detalhe}", ARQUIVO, erro("ArquivoQuebrado", &[]));
    }
    rede
}

/// Liga ou desliga, gravando primeiro e mudando a memória depois. Uma escolha que
/// só vivesse em memória morreria no próximo clique e renasceria desligada no
/// próximo boot — o usuário tem de poder confiar que o que ele ligou continua.
pub fn definir(caminho: &Path, ativa: bool) -> Result<Rede> {
    let rede = Rede { ativa };
    rede.gravar(caminho)?;
    *trava_escrita(memoria()) = rede.clone();
    Ok(rede)
}

// ------------------------------------------------------------- o catálogo --

/// A ferramenta, no idioma da vez. Ela só é chamada por `local::catalogo_com_rede`
/// quando a escolha está ligada — desligada, ela não existe para o modelo.
pub fn ferramenta() -> Ferramenta {
    let mut limite = campo("integer", "maximoResultados");
    limite["minimum"] = json!(1);
    limite["maximum"] = json!(MAX_RESULTADOS);
    super::ferramenta(
        FERRAMENTA,
        "buscarNaWeb",
        crate::permissions::ToolEffect::Rede,
        objeto(
            json!({
                "consultaRede": campo("string", "consultaRede"),
                "maximoResultados": limite,
            }),
            &["consultaRede"],
        ),
    )
}

// ------------------------------------------------------------- o validador --

/// Por que um endereço foi recusado antes de sair da máquina. Cada variante tem
/// uma frase nos dois idiomas, e a recusa diz qual foi — "bloqueado" sem motivo
/// não deixa ninguém corrigir a allowlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motivo {
    /// `http://`, `file://`, `ftp://`: só HTTPS sai.
    Esquema,
    /// O nome não está na lista dos fornecedores.
    Host,
    /// Porta diferente de 443, inclusive a explícita `:8443`.
    Porta,
    /// Host da lista com um caminho que não é o do endpoint declarado.
    Caminho,
    /// `https://usuario@host/` ou `https://host:senha@`: credencial na URL.
    Usuario,
    /// Endereço literal privado, de retorno, link-local ou não roteável.
    Endereco,
    /// O pulo de um redirecionamento saiu da lista, ou a cadeia passou de dois.
    Pulo,
    /// O corpo passou dos 512 KiB.
    Corpo,
    /// O relógio de parede acabou.
    Prazo,
    /// O nome não devolveu nenhum endereço utilizável.
    SemEndereco,
}

impl Motivo {
    /// A palavra de máquina da recusa — o sufixo da chave de catálogo e o que o
    /// dossiê lista. Não se traduz: é nome de variante, não texto.
    pub fn nome(&self) -> &'static str {
        match self {
            Motivo::Esquema => "esquema",
            Motivo::Host => "host",
            Motivo::Porta => "porta",
            Motivo::Caminho => "caminho",
            Motivo::Usuario => "usuario",
            Motivo::Endereco => "endereco",
            Motivo::Pulo => "pulo",
            Motivo::Corpo => "corpo",
            Motivo::Prazo => "prazo",
            Motivo::SemEndereco => "semEndereco",
        }
    }

    pub fn texto(&self) -> String {
        frase(&format!("motivo.{}", self.nome()), &[])
    }

    /// O `Motivo` de uma recusa montada à mão, para o cartão dizer o defeito com
    /// a palavra de máquina junto — `rede.motivo.host` sem "host" ao lado não é
    /// rastreável num log.
    pub fn escrito(&self) -> String {
        format!("{} ({})", self.texto(), self.nome())
    }
}

/// A recusa vira texto de erro na boca de quem chama, e o nome da variante vai
/// junto: uma tela que mostre só a frase não permite reportar qual regra bateu.
impl std::fmt::Display for Motivo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.escrito())
    }
}

impl std::error::Error for Motivo {}

/// Um endereço é proibido quando não é um servidor público do outro lado do
/// mundo. A conferência é por faixa, não por lista de nomes: `127.0.0.0/8`
/// inteiro é retorno (só `.0.0.1` não bastaria), `169.254.0.0/16` é link-local —
/// que é donde fala o metadado de nuvem, o prêmio de qualquer SSRF —,
/// `100.64.0.0/10` é o CGNAT que uma operadora ou um VPN caseiro usa, e
/// `fc00::/7` e `fe80::/10` são os dois equivalentes em IPv6. As faixas são
/// conferidas no octeto porque `Ipv4Addr::is_documented`,
/// `Ipv4Addr::is_benchmarking` e `Ipv6Addr::is_private` são instáveis no
/// `rust-version` deste projeto (`Cargo.toml:6`, `1.88`), e uma lista que não
/// compila não protege nada — `fc00::/7` é conferido na máscara do primeiro
/// segmento, que é o que a função instável faz. Um IPv6 mapeado em IPv4 é
/// conferido nos dois lados: `::ffff:169.254.169.254` é o mesmo host escrito de
/// outro jeito. O `to_ipv4_mapped` (estável) é usado no lugar do `to_ipv4`
/// (deprecado) porque o compatível histórico `::/96` já cai na faixa explícita
/// abaixo, então nada escapa por escolher a versão menor.
pub fn endereco_proibido(ip: IpAddr) -> bool {
    if let IpAddr::V6(v6) = ip
        && let Some(v4) = v6.to_ipv4_mapped()
    {
        return endereco_proibido(IpAddr::V4(v4));
    }
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            v4.is_loopback()
                || v4.is_link_local()
                || v4.is_private()
                || v4.is_unspecified()
                || v4.is_multicast()
                || v4.is_broadcast()
                // 100.64.0.0/10 é o CGNAT das operadoras; 198.18.0.0/15 é banco
                // de prova de equipamento; 192.0.0.0/23 e 192.0.2.0/24 são
                // reservados e de documentação; 240.0.0.0/4 é o fim da tabela.
                || (a == 100 && (b & 0xc0) == 64)
                || (a == 198 && (b == 18 || b == 19))
                || (a == 192 && b <= 2)
                || a >= 240
        }
        IpAddr::V6(v6) => {
            let o = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // fc00::/7 — a rede local única do IPv6, o equivalente do 10/8.
                || (o[0] & 0xfe00) == 0xfc00
                // fe80::/10 é o link-local do IPv6, onde escuta o vizinho de rede.
                || (o[0] & 0xffc0) == 0xfe80
                // 2001:db8::/32 é a faixa de documentação: nunca um servidor.
                || (o[0] == 0x2001 && (o[1] & 0xff00) == 0xdb00)
                // ::/96 — o antigo IPv4-compatível, que é um IPv4 escrito à mão.
                || o[..6] == [0, 0, 0, 0, 0, 0]
        }
    }
}

/// O fornecedor de um nome, sem olhar o caminho: é o que separa "este host não
/// está na lista" de "este host está, mas este endpoint não".
fn fornecedor_do_host(host: &str) -> Option<&'static Fornecedor> {
    let normal = host.trim_end_matches('.').to_ascii_lowercase();
    FORNECEDORES.iter().find(|f| f.host == normal)
}

/// A conferência de um endereço antes de qualquer conexão. Pública porque o
/// teste a exercita ponta a ponta, e porque ela é a única porta de saída.
///
/// Escolhido conferir o **nome** aqui e o **endereço** logo depois, na resolução
/// (`enderecos_usaveis`), amarrando os dois no cliente com `resolve_to_addrs`:
/// conferir só o nome deixaria um `hosts` hostil apontar `pt.wikipedia.org` para
/// `127.0.0.1`, e conferir só o endereço deixaria o resolver responder outra
/// coisa na hora da conexão. Amarrar a lista conferida ao cliente fecha o vão.
pub fn validar(url: &Url) -> Result<(), Motivo> {
    if url.scheme() != "https" {
        return Err(Motivo::Esquema);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Motivo::Usuario);
    }
    if let Some(porta) = url.port()
        && porta != 443
    {
        return Err(Motivo::Porta);
    }
    let Some(host) = url.host_str() else {
        return Err(Motivo::Host);
    };
    // Um endereço literal nunca sai, mesmo que um dia entre na lista: a lista é
    // de nomes, e um `https://127.0.0.1/` escrito à mão não é um dos dois hosts.
    if let Some(literal) = endereco_literal(host)
        && endereco_proibido(literal)
    {
        return Err(Motivo::Endereco);
    }
    if endereco_literal(host).is_some() {
        // Passou pelo filtro de faixas mas ainda é um endereço literal: fora do
        // desenho da lista, e é o Host quem diz isso — a allowlist é de nomes.
        return Err(Motivo::Host);
    }
    if host == "localhost" || host.ends_with(".localhost") {
        return Err(Motivo::Endereco);
    }
    // Os dois motivos são diferentes porque a correção é diferente: um host fora
    // da lista é a política dizendo não; um host da lista com outro caminho é um
    // endpoint que ninguém declarou — e é assim que um host permitido deixaria de
    // ser um endpoint e viraria proxy para qualquer coisa naquela máquina.
    let Some(fornecedor) = fornecedor_do_host(host) else {
        return Err(Motivo::Host);
    };
    if !url.path().starts_with(fornecedor.prefixo) {
        return Err(Motivo::Caminho);
    }
    Ok(())
}

/// O host é um endereço escrito à mão? IPv6 vem entre colchetes do `url` crate,
/// IPv4 vem cru — e os dois têm de cair na conferência de faixas, porque um
/// `https://169.254.169.254/` não é um nome de host da lista nem de longe.
fn endereco_literal(host: &str) -> Option<IpAddr> {
    let desencolchetado = host
        .strip_prefix('[')
        .and_then(|resto| resto.strip_suffix(']'))
        .unwrap_or(host);
    desencolchetado.parse::<IpAddr>().ok()
}

/// A resolução antes da conexão: só endereços públicos, e a lista vazia é erro.
pub fn enderecos_usaveis(host: &str, porta: u16) -> Result<Vec<SocketAddr>, Motivo> {
    let todos = (host, porta)
        .to_socket_addrs()
        .map_err(|_| Motivo::SemEndereco)?
        .collect::<Vec<_>>();
    escolher_enderecos(todos)
}

/// O recorte puro, separado da chamada de sistema para poder ser conferido sem
/// DNS: um endereço proibido na lista é um endereço que não se conecta.
fn escolher_enderecos(todos: Vec<SocketAddr>) -> Result<Vec<SocketAddr>, Motivo> {
    let bons: Vec<SocketAddr> = todos
        .into_iter()
        .filter(|a| !endereco_proibido(a.ip()))
        .collect();
    if bons.is_empty() {
        return Err(Motivo::SemEndereco);
    }
    Ok(bons)
}

/// O próximo endereço de uma cadeia de redirecionamentos, conferido antes de a
/// conexão acontecer. É aqui que um `302` para `http://169.254.169.254` morre: o
/// cliente não segue nada sozinho (`Policy::none()`), e quem segue é este laço.
///
/// O `pulos` já dados entra na mesma função que a conferência de endereço de
/// propósito: contar e conferir são as duas metades da mesma política, e um
/// laço que esquecesse de chamar uma delas teria um teto de dois redireciona-
/// mentos que só existe quando o host é ruim — que é o caso que ninguém testa.
pub fn proximo_pulo(atual: &Url, localizacao: &str, pulos: usize) -> Result<Url, Motivo> {
    if pulos >= MAX_PULOS {
        return Err(Motivo::Pulo);
    }
    let novo = atual.join(localizacao).map_err(|_| Motivo::Pulo)?;
    validar(&novo)?;
    Ok(novo)
}

/// O corpo, medido peça a peça. Estourou o teto, a resposta inteira é recusada:
/// HTML cortado no meio produce um resultado pela metade, e resultado pela
/// metade apresentado como resultado é a ferramenta mentindo duas vezes — sobre
/// o que veio e sobre o que falta.
struct Medidor {
    teto: usize,
    corpo: Vec<u8>,
}

impl Medidor {
    fn novo(teto: usize) -> Self {
        Self { teto, corpo: Vec::new() }
    }

    fn acrescentar(&mut self, peça: &[u8]) -> Result<(), Motivo> {
        if self.corpo.len().saturating_add(peça.len()) > self.teto {
            return Err(Motivo::Corpo);
        }
        self.corpo.extend_from_slice(peça);
        Ok(())
    }

    fn concluir(self) -> Vec<u8> {
        self.corpo
    }
}

// ------------------------------------------------------------ a montagens --

/// O pedido de um fornecedor: a URL já montada e conferida. Sem DNS e sem
/// conexão aqui, o que dá para testar os dois sem rede.
struct Alvo {
    fornecedor: &'static Fornecedor,
    url: Url,
}

/// Monta o pedido. A consulta entra como valor de um parâmetro e nada mais: o
/// host, o esquema, o caminho e os parâmetros fixos são deste arquivo.
fn montar(fornecedor: &'static Fornecedor, consulta: &str, maximo: usize) -> Result<Alvo, Motivo> {
    let mut url = Url::parse(fornecedor.base).map_err(|_| Motivo::Host)?;
    {
        let mut pares = url.query_pairs_mut();
        // Os parâmetros fixos vão antes da consulta, que é o que um servidor
        // espera e o que impede um valor de consulta de reescrever um deles:
        // `query_pairs_mut` codifica, então um `&gsrnamespace=2` escrito dentro
        // da consulta é um `&` literal no valor, não um parâmetro novo.
        match fornecedor.nome {
            "duckduckgo" => {
                pares.append_pair("kl", "br-pt");
                pares.append_pair("t", "canonical");
            }
            "wikipedia" => {
                pares.append_pair("action", "query");
                pares.append_pair("format", "json");
                pares.append_pair("formatversion", "2");
                pares.append_pair("generator", "search");
                pares.append_pair("gsrnamespace", "0");
                pares.append_pair("prop", "info|extracts");
                pares.append_pair("exintro", "1");
                pares.append_pair("explaintext", "1");
                pares.append_pair("redirects", "");
            }
            outro => unreachable!("fornecedor fora da lista: {outro}"),
        }
        pares.append_pair(fornecedor.chave_consulta, consulta);
        if let Some(chave) = fornecedor.chave_limite {
            pares.append_pair(chave, &maximo.to_string());
        }
    }
    validar(&url)?;
    Ok(Alvo { fornecedor, url })
}

/// Um resultado, com a origem escrita junto. O `host` é o que o cartão exibe: um
/// título e um resumo sem o de onde vieram seriam texto da máquina.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resultado {
    pub titulo: String,
    pub url: String,
    pub resumo: String,
    pub host: &'static str,
    pub fornecedor: &'static str,
}

/// O que um fornecedor respondeu, para o cartão dizer o que aconteceu em vez de
/// mostrar uma lista mais curta sem explicação.
enum Resposta {
    Lista(Vec<Resultado>),
    Recusada(Motivo),
    Falha(String),
}

// ----------------------------------------------------------------- a busca --

/// Executa a ferramenta com a escolha do disco. Síncrona como as outras do
/// catálogo: quem chama roda isto numa tarefa bloqueante, e o runtime abaixo é o
/// mesmo truque dos motores de banco (`bancos/motor/mysql.rs:180`) — uma thread
/// só para dirigir o cliente HTTPS assíncrono que já está no aplicativo.
pub fn buscar(args: &Map<String, Value>) -> Result<Saida> {
    buscar_com(args, &rede_atual())
}

/// A mesma execução com a escolha escrita à mão. Ela existe separada porque a
/// escolha global é uma só por processo, e um teste que a ligasse mudaria a
/// resposta de todo o resto da suíte — que roda em threads paralelas.
pub fn buscar_com(args: &Map<String, Value>, rede: &Rede) -> Result<Saida> {
    if !rede.ativa {
        bail!("{}", erro("Desligada", &[]));
    }
    let consulta = obrigatorio(args, "consultaRede")?;
    let pedido = inteiro(args, "maximoResultados");
    // O teto é do código, não do pedido: um modelo que peça 100000 resultados
    // recebe cinco, e a recusa não é um erro — é a política da ferramenta.
    let maximo = pedido.unwrap_or(MAX_RESULTADOS).clamp(1, MAX_RESULTADOS);

    let mut alvos: Vec<Alvo> = Vec::new();
    // Recusar aqui só acontece se uma constante deste arquivo for inválida, e o
    // caminho é o mesmo de sempre: o cartão diz, a conexão não acontece.
    let mut falhas: Vec<(&'static str, String)> = Vec::new();
    for fornecedor in FORNECEDORES {
        match montar(fornecedor, &consulta, maximo) {
            Ok(alvo) => alvos.push(alvo),
            Err(motivo) => falhas.push((fornecedor.nome, motivo.escrito())),
        }
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context(erro("Motor", &[]))?;

    let trabalho = runtime.block_on(tokio::time::timeout(PRAZO_GERAL, colecionar(&alvos, maximo)));
    let respostas = match trabalho {
        Err(_) => bail!("{}: {}", frase("motivo.prazo", &[]), PRAZO_GERAL.as_secs()),
        Ok(r) => r,
    };

    let mut resultados: Vec<Resultado> = Vec::new();
    for (nome, resposta) in respostas {
        match resposta {
            Resposta::Lista(mut itens) => resultados.append(&mut itens),
            Resposta::Recusada(motivo) => falhas.push((nome, motivo.escrito())),
            Resposta::Falha(detalhe) => falhas.push((nome, detalhe)),
        }
    }
    // O teto é o que a ferramenta prometeu ao modelo, e ele é cortado depois de
    // juntar os fornecedores, não entre um e outro: cinco pedidos podem voltar um
    // do DuckDuckGo e quatro da Wikipédia, e o quinto não pode sumir porque o
    // primeiro lote já tinha cinco.
    resultados.truncate(maximo);

    if resultados.is_empty() && !alvos.is_empty() && falhas.len() >= alvos.len() {
        let motivos = falhas
            .iter()
            .map(|(f, m)| format!("{f}: {m}"))
            .collect::<Vec<_>>()
            .join(" · ");
        bail!("{}", erro("TodosFalharam", &[("detalhe", &motivos)]));
    }

    Ok(saida(&consulta, &resultados, &falhas))
}

async fn colecionar(alvos: &[Alvo], maximo: usize) -> Vec<(&'static str, Resposta)> {
    let mut saída = Vec::new();
    for alvo in alvos {
        saída.push((alvo.fornecedor.nome, pedir(alvo, maximo).await));
    }
    saída
}

/// Um fornecedor, do pedido ao corpo medido. Nenhum erro de rede é pânico: ele
/// vira uma linha no cartão, porque metade dos resultados com a outra metade
/// explicada é mais honesta que uma exceção sem contexto.
async fn pedir(alvo: &Alvo, maximo: usize) -> Resposta {
    let porta = alvo.url.port().unwrap_or(443);
    let enderecos = match enderecos_usaveis(alvo.url.host_str().unwrap_or(""), porta) {
        Ok(e) => e,
        Err(m) => return Resposta::Recusada(m),
    };
    let cliente = match reqwest::Client::builder()
        .user_agent(concat!("Aether/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(PRAZO_CONEXAO)
        .timeout(PRAZO_PEDIDO)
        // O cliente não segue nada: cada pulo volta aqui e é conferido.
        .redirect(reqwest::redirect::Policy::none())
        // Os endereços conferidos são os endereços conectados — sem deixar que o
        // resolvedor responda outra coisa entre a conferência e a conexão.
        .resolve_to_addrs(alvo.fornecedor.host, &enderecos)
        .build()
    {
        Ok(c) => c,
        Err(e) => return Resposta::Falha(e.to_string()),
    };

    // Declarado vazio de propósito: o único caminho que chega ao leitor abaixo é
    // o `break` logo depois de o medidor fechar, então nunca há um corpo pela
    // metade para ser parsedo.
    let corpo: Vec<u8>;
    let mut atual = alvo.url.clone();
    let mut pulos = 0usize;
    loop {
        let resposta = match cliente.get(atual.clone()).send().await {
            Ok(r) => r,
            Err(e) => return Resposta::Falha(erro("Falhou", &[("detalhe", &e.to_string())])),
        };
        let status = resposta.status();
        if status.is_redirection() {
            let Some(local) = resposta
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
            else {
                return Resposta::Falha(erro(
                    "SemDestino",
                    &[("codigo", &status.as_u16().to_string())],
                ));
            };
            match proximo_pulo(&atual, local, pulos) {
                Ok(novo) => {
                    atual = novo;
                    pulos += 1;
                    continue;
                }
                Err(m) => return Resposta::Recusada(m),
            }
        }
        if !status.is_success() {
            return Resposta::Falha(erro("Resposta", &[("codigo", &status.as_u16().to_string())]));
        }
        // O `Content-Length` mentiroso não engana o teto — o Medidor corta por
        // peça — mas uma resposta que anuncia 40 MiB já chegou grande o bastante
        // para não ser lida: a conferência antecipada economiza o corpo inteiro.
        if resposta.content_length().is_some_and(|n| n > LIMITE_CORPO as u64) {
            return Resposta::Recusada(Motivo::Corpo);
        }
        let mut medidor = Medidor::novo(LIMITE_CORPO);
        let mut resposta = resposta;
        loop {
            match tokio::time::timeout(PRAZO_PEDIDO, resposta.chunk()).await {
                Err(_) => return Resposta::Recusada(Motivo::Prazo),
                Ok(Err(e)) => {
                    return Resposta::Falha(erro("Falhou", &[("detalhe", &e.to_string())]));
                }
                Ok(Ok(None)) => break,
                Ok(Ok(Some(peça))) => {
                    if let Err(m) = medidor.acrescentar(&peça) {
                        return Resposta::Recusada(m);
                    }
                }
            }
        }
        corpo = medidor.concluir();
        break;
    }

    let texto = String::from_utf8_lossy(&corpo).into_owned();
    let mut resultados = match alvo.fornecedor.meio {
        Meio::Html => ler_html(&texto, maximo),
        Meio::Json => ler_json(&texto, maximo),
    };
    for r in resultados.iter_mut() {
        r.fornecedor = alvo.fornecedor.nome;
    }
    resultados.truncate(maximo);
    Resposta::Lista(resultados)
}

// ------------------------------------------------------------------ os ler --

/// As âncoras `<a ...>...</a>` de um HTML, na ordem do documento. Um ler de
/// HTML de verdade custaria uma dependência nova; o que se precisa aqui são os
/// links marcados com uma classe, e um scanner de âncoras os acha sem prometer
/// o que não cumpre: uma página malformada devolve menos itens, nunca um item
/// inventado.
fn ancoras(html: &str) -> Vec<(String, String)> {
    let mut saída = Vec::new();
    let mut resto = html;
    while let Some(inicio) = find_case_insensitive(resto, "<a ") {
        let depois = &resto[inicio + 3..];
        let Some(fim_abertura) = depois.find('>') else { break };
        let atributos = depois[..fim_abertura].to_string();
        let corpo = &depois[fim_abertura + 1..];
        let Some(fim_fecho) = find_case_insensitive(corpo, "</a") else { break };
        saída.push((atributos, corpo[..fim_fecho].to_string()));
        resto = &corpo[fim_fecho..];
    }
    saída
}

fn find_case_insensitive(texto: &str, agulha: &str) -> Option<usize> {
    let minusculo = texto.to_ascii_lowercase();
    minusculo.find(&agulha.to_ascii_lowercase())
}

/// Tira as etiquetas que um servidor pôs no trecho (`<b>` em volta do termo
/// buscado) e desfaz as entidades que ele escapou. Sem isto o modelo recebe
/// `<b>Brasil</b>` como se fosse o texto.
fn texto_limpo(html: &str) -> String {
    let mut saída = String::with_capacity(html.len());
    let mut dentro = false;
    for c in html.chars() {
        match c {
            '<' => dentro = true,
            '>' => dentro = false,
            _ if dentro => {}
            c => saída.push(c),
        }
    }
    entidades(&saída)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// As entidades que um HTML de resultados usa. Uma lista fechada, e o que não
/// está nela fica como está — desmaiar num `&foo;` desconhecido seria esconder
/// o que o servidor escreveu.
pub fn entidades(texto: &str) -> String {
    let mut saída = texto.replace("&nbsp;", " ");
    for (nome, valor) in [
        ("&amp;", "&"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&#39;", "'"),
        ("&#x27;", "'"),
        ("&apos;", "'"),
    ] {
        saída = saída.replace(nome, valor);
    }
    // `&#NN;` numérico, que é como um servidor escapa o que não tem nome.
    while let Some(inicio) = saída.find("&#") {
        let depois = &saída[inicio + 2..];
        let hex = depois.starts_with('x');
        let corpo = if hex { &depois[1..] } else { depois };
        let Some(fim) = corpo.find(';') else { break };
        let numero = &corpo[..fim];
        let base = if hex { 16 } else { 10 };
        let Ok(valor) = u32::from_str_radix(numero, base) else { break };
        let Some(letra) = char::from_u32(valor) else { break };
        saída.replace_range(inicio..inicio + 3 + fim + if hex { 1 } else { 0 }, &letra.to_string());
    }
    saída
}

/// A página de resultados do DuckDuckGo: um `<a class="result__a">` por título e
/// um `<a class="result__snippet">` por resumo, e o `href` do título é um
/// involucro do DuckDuckGo (`//duckduckgo.com/l/?uddg=<codificado>`), não o
/// endereço do resultado. Desembrulhar é o que faz o cartão mostrar a fonte em
/// vez de mostrar o intermediário.
pub fn ler_html(html: &str, maximo: usize) -> Vec<Resultado> {
    let mut saída: Vec<Resultado> = Vec::new();
    for (atributos, interior) in ancoras(html) {
        let classe = atributo(&atributos, "class").unwrap_or_default();
        let texto = texto_limpo(&interior);
        if classe.contains("result__a") {
            let Some(href) = atributo(&atributos, "href") else { continue };
            let Some(url) = desenrolar(&href) else { continue };
            saída.push(Resultado {
                titulo: texto,
                url,
                resumo: String::new(),
                host: "html.duckduckgo.com",
                fornecedor: "duckduckgo",
            });
        } else if classe.contains("result__snippet")
            && let Some(último) = saída.last_mut()
            && último.resumo.is_empty()
        {
            último.resumo = texto;
        }
        if saída.len() >= maximo {
            break;
        }
    }
    saída.retain(|r| !r.titulo.is_empty() || !r.resumo.is_empty());
    saída.into_iter().take(maximo).collect()
}

/// O valor de um atributo de etiqueta, entre aspas ou sem aspas.
fn atributo(atributos: &str, nome: &str) -> Option<String> {
    let procura = format!("{nome}=");
    let inicio = atributos.find(&procura)? + procura.len();
    let resto = atributos[inicio..].trim_start();
    if let Some(sem_aspas) = resto.strip_prefix('"') {
        return sem_aspas.split('"').next().map(str::to_string);
    }
    if let Some(sem_aspas) = resto.strip_prefix('\'') {
        return sem_aspas.split('\'').next().map(str::to_string);
    }
    resto.split_whitespace().next().map(str::to_string)
}

/// Tira o endereço do involucro do DuckDuckGo. Só HTTP e HTTPS saem: um
/// `uddg=javascript:...` ou `uddg=file:...` devolve `None` e o item desaparece,
/// porque um link que o cartão mostra é um link que alguém vai abrir.
fn desenrolar(href: &str) -> Option<String> {
    let href = href.trim();
    if href.is_empty() || href.starts_with('#') {
        return None;
    }
    let candidado = if href.starts_with("//") {
        Url::parse(&format!("https:{href}")).ok()?
    } else {
        Url::parse(href).ok()?
    };
    if let Some(uddg) =
        candidado.query_pairs().find(|(chave, _)| chave == "uddg").map(|(_, valor)| valor.into_owned())
    {
        let real = Url::parse(&uddg).ok()?;
        if matches!(real.scheme(), "http" | "https") {
            return Some(real.to_string());
        }
        return None;
    }
    matches!(candidado.scheme(), "http" | "https").then(|| candidado.to_string())
}

#[derive(Deserialize)]
struct Pagina {
    #[serde(default)]
    index: Option<u32>,
    #[serde(default)]
    title: String,
    #[serde(rename = "canonicalurl", default)]
    url: String,
    #[serde(default)]
    extract: Option<String>,
}

#[derive(Deserialize)]
struct Consulta {
    #[serde(default)]
    pages: Vec<Pagina>,
}

#[derive(Deserialize)]
struct RespostaWikipedia {
    #[serde(default)]
    query: Option<Consulta>,
}

/// O JSON do MediaWiki: `query.pages` chega sem ordem garantida e com o posto da
/// busca em `index` — que é o que ordena a lista, não a ordem do vetor. Páginas
/// sem `canonicalurl` (um redirect quebrado, uma página movida) ficam de fora:
/// título sem endereço não é uma fonte citável.
pub fn ler_json(corpo: &str, maximo: usize) -> Vec<Resultado> {
    let Ok(lida) = serde_json::from_str::<RespostaWikipedia>(corpo) else {
        return Vec::new();
    };
    let Some(query) = lida.query else { return Vec::new() };
    let mut páginas = query.pages;
    páginas.sort_by_key(|p| p.index.unwrap_or(u32::MAX));
    páginas
        .into_iter()
        .filter(|p| p.url.starts_with("https://"))
        .take(maximo)
        .map(|p| Resultado {
            titulo: p.title,
            url: p.url,
            resumo: p.extract.unwrap_or_default().trim().to_string(),
            host: "pt.wikipedia.org",
            fornecedor: "wikipedia",
        })
        .collect()
}

// ------------------------------------------------------------- o cartão --

/// O cartão e o texto do modelo. Os dois dizem a mesma coisa e os dois dizem de
/// onde veio: a primeira linha do cartão é a origem externa, e as seguintes são
/// título, resumo e endereço de cada resultado.
fn saida(
    consulta: &str,
    resultados: &[Resultado],
    falhas: &[(&'static str, String)],
) -> Saida {
    let aviso = frase("aviso", &[]);
    let mut linhas = vec![linha_proveniencia()];
    linhas.push(Par::texto(frase("consulta", &[]), consulta));
    for (n, r) in resultados.iter().enumerate() {
        // O host vai no rótulo, não só no endereço: o cartão tem de dizer de onde
        // é cada linha antes de a pessoa clicar em qualquer coisa.
        linhas.push(Par::texto(
            format!("{}. {} · {}", n + 1, r.titulo, r.host),
            format!("{} — {}", r.resumo, r.url),
        ));
    }
    for (fornecedor, motivo) in falhas {
        linhas.push(Par::texto(frase("falha", &[("fornecedor", fornecedor)]), motivo));
    }
    if resultados.is_empty() {
        linhas.push(Par::texto(frase("vazio", &[]), ""));
    }

    // O texto do modelo abre com o aviso: uma citação que ele não sabe que veio
    // de fora é uma citação que ele vai apresentar como se a máquina soubesse.
    let mut texto = format!("{aviso}\n");
    if resultados.is_empty() {
        texto.push_str(&frase("vazio", &[]));
        texto.push('\n');
    }
    for (n, r) in resultados.iter().enumerate() {
        texto.push_str(&format!(
            "{}. {} [{}]\n   {}\n   {}\n",
            n + 1,
            r.titulo,
            r.host,
            r.resumo,
            r.url
        ));
    }
    for (fornecedor, motivo) in falhas {
        texto.push_str(&format!("{}: {motivo}\n", frase("falha", &[("fornecedor", fornecedor)])));
    }
    Saida::nova(texto, Vista::Sistema { linhas })
}

/// A linha de proveniência, montada a partir dos hosts reais da lista: se um dia
/// a allowlist muda, o cartão muda junto, e ninguém precisa lembrar de editar a
/// string de lugar nenhum.
fn linha_proveniencia() -> Par {
    let hosts: Vec<String> = hosts_permitidos().into_iter().map(str::to_string).collect();
    Par::texto(
        frase("origem", &[]),
        format!("{} · {}", frase("foraDaMaquina", &[]), hosts.join(", ")),
    )
}

#[cfg(test)]
mod testes {
    use super::*;

    /// Uma página de resultados do DuckDuckGo, congelada do formato público: o
    /// involucro `l/?uddg=`, o `<b>` em volta do termo, a entidade escapada e o
    /// snippet na âncora seguinte. Nada aqui veio da rede durante o teste.
    const FIXTURE_DDG: &str = r#"
    <div class="results">
      <div class="result">
        <a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fpt.wikipedia.org%2Fwiki%2FAmazonas&rut=abc">
          O rio <b>Amazonas</b> &amp; a bacia
        </a>
        <a class="result__snippet" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fpt.wikipedia.org%2Fwiki%2FAmazonas">
          O <b>Amazonas</b> é o maior rio em volume de água &#39;do mundo&#39;.
        </a>
      </div>
      <div class="result">
        <a rel="nofollow" class="result__a" href="https://exemplo.org/outra">Outra p&#225;gina</a>
        <a class="result__snippet" href="https://exemplo.org/outra">Resumo da outra.</a>
      </div>
      <a class="result__a" href="//duckduckgo.com/l/?uddg=javascript%3Aalert(1)">Enxerto</a>
    </div>"#;

    /// A mesma resposta que o MediaWiki devolve para `generator=search`, com as
    /// páginas fora de ordem e um item sem endereço canônico.
    const FIXTURE_WIKIPEDIA: &str = r#"
    {"continue":{"gsroffset":"2"},"query":{"pages":[
      {"ns":0,"index":2,"title":"Rio Negro","canonicalurl":"https://pt.wikipedia.org/wiki/Rio_Negro","extract":"Afluente do Amazonas."},
      {"ns":0,"index":1,"title":"Amazonas","canonicalurl":"https://pt.wikipedia.org/wiki/Amazonas","extract":"Estado do norte do Brasil. Mais de 3 milhões de habitantes."},
      {"ns":0,"index":3,"title":"Pagina sem url"}
    ]}}"#;

    fn url(texto: &str) -> Url {
        Url::parse(texto).expect("url de teste é parseável")
    }

    // -- o validador: cada recusa tem o nome do defeito que a produz ---------

    #[test]
    fn um_esquema_que_nao_e_https_nao_sai() {
        assert_eq!(validar(&url("http://html.duckduckgo.com/html/")), Err(Motivo::Esquema));
        assert_eq!(validar(&url("file:///C:/windows/win.ini")), Err(Motivo::Esquema));
        assert_eq!(validar(&url("ftp://pt.wikipedia.org/")), Err(Motivo::Esquema));
        // O `javascript:` nem é uma URL de rede: sem o esquema https ele morre
        // na primeira conferência, antes de qualquer parsedor de host.
        assert_eq!(validar(&url("javascript:alert(1)")), Err(Motivo::Esquema));
        assert!(validar(&url("https://html.duckduckgo.com/html/?q=x")).is_ok());
    }

    #[test]
    fn um_host_fora_da_lista_nao_sai() {
        assert_eq!(validar(&url("https://exemplo.org/")), Err(Motivo::Host));
        // O sufixo mentiroso é o clássico: a conferência é de nome inteiro, não
        // de "termina com".
        assert_eq!(
            validar(&url("https://pt.wikipedia.org.evil.example/")),
            Err(Motivo::Host)
        );
        assert_eq!(validar(&url("https://duckduckgo.com/html/")), Err(Motivo::Host));
    }

    #[test]
    fn um_endereco_literal_proibido_nao_sai() {
        // O metadado de nuvem é a razão de existir desta lista.
        assert_eq!(
            validar(&url("https://169.254.169.254/latest/meta-data/")),
            Err(Motivo::Endereco)
        );
        assert_eq!(validar(&url("https://127.0.0.1/admin")), Err(Motivo::Endereco));
        assert_eq!(validar(&url("https://[::1]/")), Err(Motivo::Endereco));
        assert_eq!(validar(&url("https://10.0.0.5/")), Err(Motivo::Endereco));
        assert_eq!(validar(&url("https://0.0.0.0/")), Err(Motivo::Endereco));
        // IPv6 que é IPv4 escrito de outro jeito cai na mesma faixa.
        assert_eq!(
            validar(&url("https://[::ffff:169.254.169.254]/")),
            Err(Motivo::Endereco)
        );
        // O nome de retorno, que não é um endereço e ainda assim é a mesma casa.
        assert_eq!(validar(&url("https://localhost/")), Err(Motivo::Endereco));
        assert_eq!(validar(&url("https://x.localhost/")), Err(Motivo::Endereco));
        // Um endereço público literal continua fora da lista: ela é de nomes.
        assert_eq!(validar(&url("https://8.8.8.8/")), Err(Motivo::Host));
    }

    #[test]
    fn as_faixas_proibidas_sao_as_faixas_de_casa() {
        for texto in [
            "127.0.0.1",
            "127.5.5.5",
            "169.254.169.254",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.0.1",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "::1",
            "fe80::1",
            "fc00::1",
            "::ffff:127.0.0.1",
        ] {
            let ip: IpAddr = texto.parse().expect("endereço de teste");
            assert!(endereco_proibido(ip), "{texto} passou como seguro");
        }
        for texto in ["8.8.8.8", "151.101.1.140", "2001:4860:4860::8888", "1.1.1.1"] {
            let ip: IpAddr = texto.parse().expect("endereço de teste");
            assert!(!endereco_proibido(ip), "{texto} passou como proibido");
        }
    }

    #[test]
    fn a_lista_e_de_pares_host_e_caminho() {
        // Host permitido com um caminho que não é o endpoint dele: recusado com o
        // nome do defeito, que é diferente de "host fora da lista" justamente
        // porque a correção é diferente. É o que impede um host da lista de
        // virar proxy para qualquer coisa naquela máquina.
        assert_eq!(
            validar(&url("https://pt.wikipedia.org/wiki/Usu%C3%A1rio:Root")),
            Err(Motivo::Caminho)
        );
        assert_eq!(validar(&url("https://html.duckduckgo.com/")), Err(Motivo::Caminho));
        assert_eq!(
            validar(&url("https://html.duckduckgo.com/../../etc/passwd")),
            Err(Motivo::Caminho)
        );
        assert!(validar(&url("https://pt.wikipedia.org/w/api.php?action=query")).is_ok());
    }

    #[test]
    fn credencial_e_porta_na_url_sao_recusadas() {
        assert_eq!(validar(&url("https://usuario:segredo@pt.wikipedia.org/w/api.php")), Err(Motivo::Usuario));
        assert_eq!(validar(&url("https://pt.wikipedia.org:8443/w/api.php")), Err(Motivo::Porta));
        // A porta explícita que é a de sempre não muda nada.
        assert!(validar(&url("https://pt.wikipedia.org:443/w/api.php")).is_ok());
    }

    #[test]
    fn um_pulo_para_fora_da_lista_e_recusado_antes_de_conectar() {
        let atual = url("https://html.duckduckgo.com/html/?q=amazonas");
        // Para a placa de retorno, com esquema trocado no caminho do pulo.
        assert_eq!(
            proximo_pulo(&atual, "http://169.254.169.254/latest/meta-data/", 0).unwrap_err(),
            Motivo::Esquema
        );
        assert_eq!(
            proximo_pulo(&atual, "https://169.254.169.254/latest/meta-data/", 0).unwrap_err(),
            Motivo::Endereco
        );
        assert_eq!(
            proximo_pulo(&atual, "https://exemplo.org/roubo", 0).unwrap_err(),
            Motivo::Host
        );
        // Relativo dentro do próprio endpoint: aceito, e é o único pulo legítimo.
        assert_eq!(
            proximo_pulo(&atual, "/html/?q=amazonas-2", 0).unwrap(),
            url("https://html.duckduckgo.com/html/?q=amazonas-2")
        );
    }

    /// O teto de dois pulos é conferido pela mesma função que confere o endereço,
    /// e o que o teste exige é o caso que um laço esqueceria: um destino perfeito,
    /// dentro da lista, pedido na terceira viagem. Sem o contador dentro de
    /// `proximo_pulo`, esta asserção não teria onde morar.
    #[test]
    fn a_cadeia_para_no_segundo_pulo_mesmo_com_destino_bom() {
        let atual = url("https://html.duckduckgo.com/html/?q=x");
        let destino = "/html/?q=y";
        assert!(proximo_pulo(&atual, destino, 0).is_ok());
        assert!(proximo_pulo(&atual, destino, MAX_PULOS - 1).is_ok());
        assert_eq!(proximo_pulo(&atual, destino, MAX_PULOS).unwrap_err(), Motivo::Pulo);
        assert_eq!(proximo_pulo(&atual, destino, MAX_PULOS + 5).unwrap_err(), Motivo::Pulo);
        // E o teto é o número que o cartão promete, não um número solto: dois.
        assert_eq!(MAX_PULOS, 2);
    }

    #[test]
    fn o_pulo_nunca_reinventa_a_lista() {
        // `//host/` é um protocolo relativo: herda o esquema, não a confiança.
        let atual = url("https://html.duckduckgo.com/html/?q=x");
        assert_eq!(proximo_pulo(&atual, "//exemplo.org/", 0).unwrap_err(), Motivo::Host);
        assert_eq!(proximo_pulo(&atual, "javascript:alert(1)", 0).unwrap_err(), Motivo::Esquema);
    }

    // -- a montagem do pedido: a consulta é texto, nunca rota ----------------

    #[test]
    fn a_consulta_nao_abre_rota() {
        let ddg = FORNECEDORES.iter().find(|f| f.nome == "duckduckgo").unwrap();
        let wiki = FORNECEDORES.iter().find(|f| f.nome == "wikipedia").unwrap();
        for ataque in [
            "?q=x&gsrnamespace=2",
            "http://169.254.169.254/latest/meta-data/",
            "../../../../etc/passwd",
            "javascript:alert(1)",
            "https://exemplo.org/",
            "@exemplo.org",
        ] {
            for fornecedor in [ddg, wiki] {
                let alvo = montar(fornecedor, ataque, MAX_RESULTADOS).expect("montou");
                let url = alvo.url.as_str();
                // O host, o esquema e o caminho continuam os da lista.
                assert_eq!(alvo.url.scheme(), "https", "{url}");
                assert_eq!(alvo.url.host_str().unwrap(), fornecedor.host, "{url}");
                assert!(alvo.url.path().starts_with(fornecedor.prefixo), "{url}");
                assert!(validar(&alvo.url).is_ok(), "{url}");
                // A consulta chegou inteira, como valor de um par só.
                let pares: Vec<(String, String)> =
                    alvo.url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
                let (chave, valor) = pares.iter().find(|(k, _)| k == fornecedor.chave_consulta).expect("a consulta está no pedido");
                assert_eq!(chave, fornecedor.chave_consulta);
                assert_eq!(valor, ataque, "a consulta foi alterada ou virou parâmetro");
                // Nada do ataque virou parâmetro extra: o número de pares é o da
                // montagem fixa, mais a consulta e o limite.
                let fixos = match fornecedor.nome {
                    "duckduckgo" => 2,
                    // action, format, formatversion, generator, gsrnamespace,
                    // prop, exintro, explaintext, redirects. Contadas à mão: se a
                    // montagem ganhar um parâmetro, este número tem de mudar com
                    // ela, e é assim que o teste percebe.
                    _ => 9,
                };
                let esperados = fixos + 1 + usize::from(fornecedor.chave_limite.is_some());
                assert_eq!(pares.len(), esperados, "{url} ganhou parâmetro do ataque");
            }
        }
    }

    #[test]
    fn o_limite_do_pedido_e_o_teto_da_ferramenta() {
        let wiki = FORNECEDORES.iter().find(|f| f.nome == "wikipedia").unwrap();
        let alvo = montar(wiki, "amazonas", 3).unwrap();
        let limite = alvo.url.query_pairs().find(|(k, _)| k == "gsrlimit").unwrap().1;
        assert_eq!(limite, "3");
    }

    // -- o teto de corpo -----------------------------------------------------

    #[test]
    fn o_corpo_para_no_teto_e_nao_devolve_pela_metade() {
        let peça = vec![b'x'; LIMITE_CORPO / 2];
        let mut medidor = Medidor::novo(LIMITE_CORPO);
        medidor.acrescentar(&peça).expect("metade do teto coube");
        medidor.acrescentar(&peça).expect("o teto exato cabe");
        assert_eq!(medidor.corpo.len(), LIMITE_CORPO);

        let mut estourado = Medidor::novo(LIMITE_CORPO);
        estourado.acrescentar(&vec![b'x'; LIMITE_CORPO]).unwrap();
        assert_eq!(
            estourado.acrescentar(b"um byte a mais").unwrap_err(),
            Motivo::Corpo,
            "um byte acima do teto foi aceito"
        );
        // Recusado é recusado inteiro: o corpo parcial não sai daqui para o
        // parsedor produzir um resultado pela metade.
        let mut outro = Medidor::novo(16);
        assert_eq!(outro.acrescentar(&vec![b'x'; 40]).unwrap_err(), Motivo::Corpo);
        assert_eq!(outro.corpo.len(), 0, "a peça recusada entrou no corpo");
    }

    // -- os parsedores, contra corpo congelado -------------------------------

    #[test]
    fn o_html_do_duckduckgo_vira_titulo_endereco_e_resumo() {
        let achados = ler_html(FIXTURE_DDG, MAX_RESULTADOS);
        assert_eq!(achados.len(), 2, "o involucro e o javascript: entraram: {achados:?}");
        assert_eq!(achados[0].titulo, "O rio Amazonas & a bacia");
        // O `uddg` desembrulhado: o cartão mostra a fonte, não o intermediário.
        assert_eq!(achados[0].url, "https://pt.wikipedia.org/wiki/Amazonas");
        assert_eq!(achados[0].resumo, "O Amazonas é o maior rio em volume de água 'do mundo'.");
        assert_eq!(achados[0].host, "html.duckduckgo.com");
        assert_eq!(achados[1].url, "https://exemplo.org/outra");
        // O recorde de `maximo` é respeitado antes de qualquer outra coisa.
        assert_eq!(ler_html(FIXTURE_DDG, 1).len(), 1);
    }

    #[test]
    fn o_involucro_javascript_nao_vira_link() {
        let achados = ler_html(FIXTURE_DDG, MAX_RESULTADOS);
        assert!(
            achados.iter().all(|a| a.url.starts_with("http")),
            "um link não-HTTP chegou ao cartão: {:?}",
            achados.iter().map(|a| &a.url).collect::<Vec<_>>()
        );
        assert!(achados.iter().all(|a| !a.titulo.contains("Enxerto")));
    }

    #[test]
    fn o_json_do_mediawiki_vem_na_ordem_que_o_servidor_disse() {
        let achados = ler_json(FIXTURE_WIKIPEDIA, MAX_RESULTADOS);
        assert_eq!(achados.len(), 2, "a página sem endereço entrou: {achados:?}");
        // `index` é a posição da busca; o vetor veio fora de ordem de propósito.
        assert_eq!(achados[0].titulo, "Amazonas");
        assert_eq!(achados[0].url, "https://pt.wikipedia.org/wiki/Amazonas");
        assert_eq!(achados[1].titulo, "Rio Negro");
        assert_eq!(achados[0].fornecedor, "wikipedia");
        assert_eq!(ler_json(FIXTURE_WIKIPEDIA, 1).len(), 1);
    }

    #[test]
    fn um_corpo_que_nao_e_o_esperado_devolve_nada_em_vez_de_inventar() {
        assert!(ler_json("<html>429 Too Many Requests</html>", MAX_RESULTADOS).is_empty());
        assert!(ler_json("{}", MAX_RESULTADOS).is_empty());
        assert!(ler_html("<div>nada aqui</div>", MAX_RESULTADOS).is_empty());
    }

    #[test]
    fn as_entidades_de_um_servidor_sao_texto() {
        assert_eq!(entidades("a &amp; b &lt;c&gt; &#39;d&#39; &#x27;e&#39; &nbsp;f"), "a & b <c> 'd' 'e'  f");
        assert_eq!(entidades("p&#225;gina"), "página");
        // O que não tem nome conhecido fica como está, visível.
        assert_eq!(entidades("&foo;"), "&foo;");
    }

    #[test]
    fn as_etiquetas_do_servidor_nao_chegam_ao_modelo() {
        assert_eq!(texto_limpo("O <b>Amazonas</b> é grande"), "O Amazonas é grande");
        assert_eq!(texto_limpo("  muitos\n espaços \t aqui "), "muitos espaços aqui");
    }

    // -- o endereço conferido é o endereço conectado -------------------------

    #[test]
    fn um_nome_que_s6_resolve_em_endereco_proibido_e_recusado() {
        let casa = vec!["127.0.0.1:443".parse().unwrap(), "[::1]:443".parse().unwrap()];
        assert_eq!(escolher_enderecos(casa).unwrap_err(), Motivo::SemEndereco);

        let metadado = vec!["169.254.169.254:443".parse().unwrap()];
        assert_eq!(escolher_enderecos(metadado).unwrap_err(), Motivo::SemEndereco);

        // Público e privado juntos: o privado sai da lista, o público conecta.
        let misto = vec!["127.0.0.1:443".parse().unwrap(), "91.198.174.192:443".parse().unwrap()];
        let bons = escolher_enderecos(misto).expect("sobrou o público");
        assert_eq!(bons.len(), 1);
        assert_eq!(bons[0].ip().to_string(), "91.198.174.192");
    }

    // -- desligado é o estado de fábrica -------------------------------------

    #[test]
    fn sem_arquivo_a_rede_esta_desligada() {
        let caminho = std::env::temp_dir().join(format!(
            "aether-rede-inexistente-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&caminho);
        let (rede, aviso) = Rede::de(&caminho);
        assert!(!rede.ativa, "sem arquivo a rede ligou sozinha");
        assert_eq!(aviso, None, "ausência de arquivo não é defeito de leitura");
    }

    #[test]
    fn um_rede_json_quebrado_devolve_desligado_e_o_motivo() {
        let caminho = std::env::temp_dir().join(format!(
            "aether-rede-quebrado-{}.json",
            std::process::id()
        ));
        std::fs::write(&caminho, "{ isso não é json ").unwrap();
        let (rede, aviso) = Rede::de(&caminho);
        assert!(!rede.ativa, "um arquivo ilegível ligou a rede");
        assert!(aviso.is_some(), "o arquivo ilegível passou sem aviso");
        let _ = std::fs::remove_file(&caminho);
    }

    #[test]
    fn ligar_e_desligar_fica_no_disco() {
        let caminho = std::env::temp_dir().join(format!(
            "aether-rede-grava-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&caminho);
        Rede { ativa: true }.gravar(&caminho).expect("gravei");
        let (lido, aviso) = Rede::de(&caminho);
        assert!(lido.ativa, "liguei e o disco diz o contrário");
        assert_eq!(aviso, None);
        assert!(std::fs::read_to_string(&caminho).unwrap().contains("ativa"));
        // O temporário não fica sobrando ao lado do arquivo.
        assert!(!caminho.with_extension("json.tmp").exists());
        let _ = std::fs::remove_file(&caminho);
    }

    #[test]
    fn a_escolha_do_arquivo_e_a_escolha_da_ferramenta() {
        // A chave gravada é a mesma que o struct lê: um `{"ativa":true}` escrito
        // à mão na pasta de configuração tem de ligar a rede, e é isto que o
        // dono tem em mãos antes de existir tela para isso.
        assert_eq!(serde_json::to_string(&Rede { ativa: true }).unwrap(), r#"{"ativa":true}"#);
        assert!(!serde_json::from_str::<Rede>("{}").unwrap().ativa);
        assert!(serde_json::from_str::<Rede>(r#"{"ativa":true}"#).unwrap().ativa);
    }

    // -- o catálogo e o efeito ------------------------------------------------

    #[test]
    fn a_ferramenta_declara_o_efeito_de_rede_e_nao_aceita_endereco() {
        let ferramenta = ferramenta();
        assert_eq!(ferramenta.nome, FERRAMENTA);
        assert_eq!(ferramenta.efeito, crate::permissions::ToolEffect::Rede);
        let propriedades = ferramenta.esquema["properties"].as_object().unwrap();
        assert_eq!(
            propriedades.keys().cloned().collect::<Vec<_>>(),
            vec!["consultaRede".to_string(), "maximoResultados".to_string()]
        );
        // O ponto inteiro da política: não existe campo de endereço. O modelo
        // escolhe o que procurar, nunca onde bater.
        assert!(!propriedades.contains_key("url"));
        assert!(!propriedades.contains_key("endereco"));
        assert_eq!(ferramenta.esquema["additionalProperties"], false);
        assert_eq!(ferramenta.esquema["required"].as_array().unwrap(), &vec![json!("consultaRede")]);
        assert_eq!(ferramenta.esquema["properties"]["maximoResultados"]["maximum"], MAX_RESULTADOS);
    }

    #[test]
    fn a_lista_de_hosts_e_a_que_o_codigo_usa() {
        // O cartão mostra estes nomes; se a allowlist crescer e a lista do cartão
        // ficar velha, é aqui que aparece.
        assert_eq!(hosts_permitidos(), vec!["html.duckduckgo.com", "pt.wikipedia.org"]);
        assert!(FORNECEDORES.iter().all(|f| f.base.starts_with("https://")));
        for fornecedor in FORNECEDORES {
            let alvo = montar(fornecedor, "amazonas", 5).expect("o endpoint declarado é válido");
            assert_eq!(alvo.url.host_str().unwrap(), fornecedor.host);
        }
    }

    #[test]
    fn a_rede_desligada_recusa_a_ferramenta() {
        // Desligado é o estado de fábrica do processo inteiro: `carregar` nunca
        // foi chamado com `ativa: true` aqui. A recusa acontece antes de qualquer
        // leitura de argumento e antes de qualquer soquete — o teste passa numa
        // máquina sem cabo de rede.
        assert!(!habilitada(), "outro teste ligou a rede neste processo");
        let recusa = buscar(&mapa(json!({"consultaRede": "amazonas"}))).unwrap_err().to_string();
        assert_eq!(recusa, erro("Desligada", &[]));
        // A mesma chamada com a escolha escrita à mão recusa pelo mesmo motivo: a
        // porta é o `Rede`, não a memória global.
        let desligada = buscar_com(&mapa(json!({"consultaRede": "amazonas"})), &Rede::default())
            .unwrap_err()
            .to_string();
        assert_eq!(desligada, erro("Desligada", &[]));
        // Um argumento que falta não é a porta de entrada da rede: a recusa de
        // estar desligada vem antes da validação do pedido, senão um modelo sem
        // rede descobr hosts sondando argumentos.
        assert_eq!(
            buscar_com(&mapa(json!({})), &Rede::default()).unwrap_err().to_string(),
            erro("Desligada", &[])
        );
    }

    /// As chaves que este módulo pede a `tr`. Escritas aqui como dados, não como
    /// chamada com a chave no fonte, de propósito: `idiomas.rs:307` cobra texto
    /// de todo literal do fonte, e as frases deste arquivo saem por `frase()` e
    /// `erro()`, que é a mesma saída que `campo()` e `ferramenta()` já usam para
    /// famílias compostas. Com o catálogo publicado, este caso é a guarda: se
    /// alguém renomear uma variante de `Motivo`, ou pedir uma chave que não
    /// entrou, ele diz qual — do lado do que `ai/cabecalho.rs:1067` já faz para
    /// as recusas do leitor de GGUF.
    const CHAVES: &[&str] = &[
        "rede.aviso",
        "rede.consulta",
        "rede.origem",
        "rede.foraDaMaquina",
        "rede.vazio",
        "rede.falha",
        "rede.motivo.esquema",
        "rede.motivo.host",
        "rede.motivo.porta",
        "rede.motivo.caminho",
        "rede.motivo.usuario",
        "rede.motivo.endereco",
        "rede.motivo.pulo",
        "rede.motivo.corpo",
        "rede.motivo.prazo",
        "rede.motivo.semEndereco",
        "erros.redeDesligada",
        "erros.redeMotor",
        "erros.redeTodosFalharam",
        "erros.redeResposta",
        "erros.redeSemDestino",
        "erros.redeFalhou",
        "erros.redeArquivoQuebrado",
        "erros.redeGravar",
        "ferramentas.buscarNaWeb.titulo",
        "ferramentas.buscarNaWeb.descricao",
        "argumentos.consultaRede",
        "argumentos.consultaRedeDescricao",
        "argumentos.maximoResultados",
        "argumentos.maximoResultadosDescricao",
    ];

    #[test]
    fn toda_chave_de_rede_tem_texto_em_todo_idioma() {
        for chave in CHAVES {
            assert!(crate::idiomas::tem_texto(chave), "{chave} apareceria na tela como chave");
        }
    }

    /// O teto de corpo e o prazo de parede não são decoração: eles são o que
    /// falta para um host lento ou gordo segurar a chamada para sempre. Medido
    /// aqui, no relógio do teste, sem rede: a soma dos prazos por requisição é
    /// maior que o relógio da chamada inteira, então o que corta primeiro é o
    /// relógio — e o corte é `Recusada(Prazo)`/`Err`, não um `await` suspenso.
    #[test]
    fn os_prazos_declarados_sao_menores_que_a_soma_dos_pulos() {
        // O relógio de parede tem de cortar antes que os prazos por requisição se
        // somem até o infinito: sem isto, um host que silencia em cada pulo
        // seguraria a chamada por `PRAZO_PEDIDO * (MAX_PULOS + 1)`.
        let soma = PRAZO_PEDIDO * (MAX_PULOS as u32 + 1);
        assert!(PRAZO_GERAL < soma, "{PRAZO_GERAL:?} não corta {soma:?}");
        assert!(PRAZO_CONEXAO < PRAZO_PEDIDO);
    }

    fn mapa(valor: Value) -> Map<String, Value> {
        valor.as_object().unwrap().clone()
    }
}
