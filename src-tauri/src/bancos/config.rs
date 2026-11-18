//! As conexões de banco de dados que o dono da máquina cadastrou.
//!
//! Uma conexão diz onde o banco está, quem entra nele e — a parte que é
//! política, não configuração de rede — quais tabelas estão expostas e o que
//! pode ser feito em cada uma. A senha é cifrada aqui, antes de o JSON tocar o
//! disco; o texto em claro só existe na memória, no momento de abrir a conexão.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use super::sigilo;

/// Hex do relógio mais um contador do processo: duas conexões criadas no mesmo
/// milissegundo não podem dividir id.
static SEQUENCIA: AtomicU64 = AtomicU64::new(0);

fn novo_id() -> String {
    let agora = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("bd-{agora:x}-{:x}", SEQUENCIA.fetch_add(1, Ordering::Relaxed))
}

/// As famílias que o conector fala. Cada tipo tem um motor próprio em `motor/`,
/// todos em Rust puro: nenhum cliente nativo precisa estar instalado, na mesma
/// regra dos motores de inferência. O Oracle entra pelo driver oficial da
/// Oracle em modo thin (protocolo TNS/TTC), que é Rust puro também.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tipo {
    Sqlite,
    Postgres,
    Mysql,
    Mssql,
    Mongo,
    Oracle,
}

impl Tipo {
    pub fn rotulo(&self) -> &'static str {
        match self {
            Self::Sqlite => "sqlite",
            Self::Postgres => "postgres",
            Self::Mysql => "mysql",
            Self::Mssql => "sqlserver",
            Self::Mongo => "mongodb",
            Self::Oracle => "oracle",
        }
    }

    pub fn de_rotulo(rotulo: &str) -> Option<Self> {
        let rotulo = rotulo.trim().to_ascii_lowercase();
        match rotulo.as_str() {
            "sqlite" => Some(Self::Sqlite),
            "postgres" | "postgresql" => Some(Self::Postgres),
            "mysql" | "mariadb" => Some(Self::Mysql),
            "sqlserver" | "mssql" => Some(Self::Mssql),
            "mongodb" | "mongo" => Some(Self::Mongo),
            "oracle" => Some(Self::Oracle),
            _ => None,
        }
    }
}

/// O que pode ser feito numa tabela (ou coleção). Nasce em só leitura: a
/// escrita é um ato do dono, ativado uma tabela por vez na tela.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Fonte {
    pub ler: bool,
    pub inserir: bool,
    pub atualizar: bool,
    pub apagar: bool,
}

impl Default for Fonte {
    fn default() -> Self {
        Self { ler: true, inserir: false, atualizar: false, apagar: false }
    }
}

impl Fonte {
    pub fn permite(&self, operacao: Operacao) -> bool {
        match operacao {
            Operacao::Ler => self.ler,
            Operacao::Inserir => self.inserir,
            Operacao::Atualizar => self.atualizar,
            Operacao::Apagar => self.apagar,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operacao {
    Ler,
    Inserir,
    Atualizar,
    Apagar,
}

impl Operacao {
    /// A palavra que o erro de política usa: o modelo lê o que foi negado.
    pub fn rotulo(&self) -> &'static str {
        match self {
            Self::Ler => "ler",
            Self::Inserir => "inserir",
            Self::Atualizar => "atualizar",
            Self::Apagar => "apagar",
        }
    }
}

/// Uma conexão cadastrada. O nome é a chave que o modelo usa nas ferramentas —
/// nomes têm de ser únicos, e é por ele que a política é achada na chamada.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conexao {
    pub id: String,
    pub nome: String,
    pub tipo: Tipo,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub porta: Option<u16>,
    #[serde(default)]
    pub usuario: String,
    #[serde(default)]
    pub banco: String,
    #[serde(default)]
    pub arquivo: String,
    /// Texto cifrado pelo DPAPI. O JSON do disco nunca carrega a senha em claro.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub senha_cifrada: Option<String>,
    /// Senha em claro recebida da tela, num só sentido: entra no `upsert`, vira
    /// cifra e morre. Não é serializada — o disco nunca a vê.
    #[serde(skip)]
    pub senha_clara: Option<String>,
    /// Teto de linhas que uma consulta devolve ao modelo, tabelas grandes
    /// incluídas. A consulta pede menos; nunca mais que isto.
    #[serde(default = "padrao_limite")]
    pub limite_linhas: u32,
    /// SQL escrito à mão pelo modelo. Desligado por padrão: ligar aqui é
    /// autorizar qualquer declaração na conexão, inclusive de escrita.
    #[serde(default)]
    pub sql_livre: bool,
    /// Tabelas (ou coleções) expostas e a política de cada uma. O que não está
    /// aqui o conector não toca, nem para descrever.
    #[serde(default)]
    pub fontes: BTreeMap<String, Fonte>,
}

fn padrao_limite() -> u32 {
    200
}

impl Conexao {
    /// A porta que vale quando o usuário não disse: a de fábrica de cada família.
    pub fn porta_efetiva(&self) -> u16 {
        self.porta.unwrap_or(match self.tipo {
            Tipo::Sqlite => 0,
            Tipo::Postgres => 5432,
            Tipo::Mysql => 3306,
            Tipo::Mssql => 1433,
            Tipo::Mongo => 27017,
            Tipo::Oracle => 1521,
        })
    }

    /// A senha em claro, aberta da cifra na memória. Devolve vazio quando a
    /// conexão não tem senha cadastrada — autenticação do usuário do Windows,
    /// por exemplo.
    pub fn senha(&self) -> Result<String> {
        match &self.senha_cifrada {
            None => Ok(String::new()),
            Some(texto) if texto.is_empty() => Ok(String::new()),
            Some(texto) => sigilo::decifrar(texto),
        }
    }
}

/// O cadastro inteiro, em `bancos.json` na pasta de configuração do aplicativo.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    #[serde(default)]
    pub conexoes: Vec<Conexao>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let caminho = path.display().to_string();
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)
                .with_context(|| crate::idiomas::tr("erros.configInvalida", &[("caminho", &caminho)])),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| crate::idiomas::tr("erros.falhaLerConfig", &[("caminho", &caminho)])),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let caminho = path.display().to_string();
        if let Some(pai) = path.parent() {
            std::fs::create_dir_all(pai).with_context(|| {
                crate::idiomas::tr("erros.falhaCriarPasta", &[("caminho", &pai.display().to_string())])
            })?;
        }
        let texto = serde_json::to_string_pretty(self)
            .context(crate::idiomas::tr("erros.falhaSalvarConfig", &[]))?;
        let temporario = path.with_extension("json.tmp");
        std::fs::write(&temporario, texto).with_context(|| {
            crate::idiomas::tr("erros.falhaEscreverConfig", &[("caminho", &temporario.display().to_string())])
        })?;
        std::fs::rename(&temporario, path)
            .with_context(|| crate::idiomas::tr("erros.falhaSubstituirConfig", &[("caminho", &caminho)]))?;
        Ok(())
    }

    /// Valida e instala uma conexão. A senha que chega aqui está em claro e
    /// nunca passa adiante: vira cifra DPAPI antes do save. Senha vazia numa
    /// edição significa "manter a que já está cifrada".
    pub fn upsert(&mut self, entrada: &mut Conexao) -> Result<()> {
        let nome = entrada.nome.trim();
        if nome.is_empty() {
            bail!(crate::idiomas::tr("erros.bancoNomeVazio", &[]));
        }
        let nome = nome.to_string();
        if let Some(outra) = self
            .conexoes
            .iter()
            .find(|c| c.nome.eq_ignore_ascii_case(&nome) && c.id != entrada.id)
        {
            bail!(crate::idiomas::tr(
                "erros.bancoNomeDuplicado",
                &[("nome", &outra.nome)]
            ));
        }
        match entrada.tipo {
            Tipo::Sqlite => {
                if entrada.arquivo.trim().is_empty() {
                    bail!(crate::idiomas::tr("erros.bancoArquivoFaltando", &[]));
                }
            }
            _ => {
                if entrada.host.trim().is_empty() {
                    bail!(crate::idiomas::tr("erros.bancoHostFaltando", &[]));
                }
                if entrada.banco.trim().is_empty() {
                    bail!(crate::idiomas::tr("erros.bancoNomeBancoFaltando", &[]));
                }
            }
        }
        entrada.nome = nome;
        entrada.limite_linhas = entrada.limite_linhas.clamp(1, 10_000);
        // A senha que a tela mandou em claro vira cifra DPAPI aqui e nunca mais
        // volta ao texto. Vazia numa conexão que já existe significa "manter a
        // que está guardada"; vazia numa nova significa "sem senha".
        let lugar = self.conexoes.iter().position(|c| c.id == entrada.id);
        match entrada.senha_clara.take() {
            Some(clara) if !clara.is_empty() => {
                entrada.senha_cifrada = Some(sigilo::cifrar(&clara)?);
            }
            _ => {
                if lugar.is_none() {
                    entrada.senha_cifrada = None;
                }
            }
        }
        if let Some(lugar) = lugar {
            if entrada.senha_cifrada.is_none() {
                entrada.senha_cifrada = self.conexoes[lugar].senha_cifrada.clone();
            }
            self.conexoes[lugar] = entrada.clone();
        } else {
            if entrada.id.trim().is_empty() {
                entrada.id = novo_id();
            }
            self.conexoes.push(entrada.clone());
        }
        Ok(())
    }

    pub fn remover(&mut self, id: &str) -> bool {
        let antes = self.conexoes.len();
        self.conexoes.retain(|c| c.id != id);
        self.conexoes.len() != antes
    }

    pub fn por_nome(&self, nome: &str) -> Option<&Conexao> {
        self.conexoes
            .iter()
            .find(|c| c.nome.eq_ignore_ascii_case(nome.trim()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conexao(nome: &str) -> Conexao {
        Conexao {
            id: format!("id-{nome}"),
            nome: nome.to_string(),
            tipo: Tipo::Sqlite,
            host: String::new(),
            porta: None,
            usuario: String::new(),
            banco: String::new(),
            arquivo: "C:/dados/estoque.db".into(),
            senha_cifrada: None,
            senha_clara: None,
            limite_linhas: 200,
            sql_livre: false,
            fontes: BTreeMap::new(),
        }
    }

    #[test]
    fn nasce_em_so_leitura_e_a_escrita_e_um_ato_do_dono() {
        let fonte = Fonte::default();
        assert!(fonte.permite(Operacao::Ler));
        assert!(!fonte.permite(Operacao::Inserir));
        assert!(!fonte.permite(Operacao::Atualizar));
        assert!(!fonte.permite(Operacao::Apagar));
    }

    #[test]
    fn nomes_sao_unicos_sem_case_dizer_o_contrario() {
        let mut config = Config::default();
        config.conexoes.push(conexao("Estoque"));
        let mut repetida = conexao("ESTOQUE ");
        repetida.id = "outro".into();
        assert!(config.upsert(&mut repetida).is_err());
    }

    #[test]
    fn editar_a_mesma_conexao_nao_e_duplicacao() {
        let mut config = Config::default();
        config.conexoes.push(conexao("Estoque"));
        let mut editada = conexao("Estoque");
        editada.arquivo = "C:/outro.db".into();
        config.upsert(&mut editada).expect("a mesma id edita");
        assert_eq!(config.conexoes.len(), 1);
        assert_eq!(config.conexoes[0].arquivo, "C:/outro.db");
    }

    #[test]
    fn a_busca_por_nome_ignora_case_e_espaco_sobra() {
        let mut config = Config::default();
        config.conexoes.push(conexao("Estoque"));
        assert!(config.por_nome(" estoque ").is_some());
        assert!(config.por_nome("vendas").is_none());
    }

    #[test]
    fn o_limite_de_linhas_tem_teto_e_piso() {
        let mut config = Config::default();
        let mut conexao = conexao("Estoque");
        conexao.limite_linhas = 99_999;
        config.upsert(&mut conexao).expect("valida");
        assert_eq!(config.conexoes[0].limite_linhas, 10_000);
        conexao.limite_linhas = 0;
        config.upsert(&mut conexao).expect("valida");
        assert_eq!(config.conexoes[0].limite_linhas, 1);
    }

    #[test]
    fn o_cadastro_roundtrips_pelo_disco() {
        let dir = std::env::temp_dir().join(format!("aether-bancos-{}", std::process::id()));
        let path = dir.join("bancos.json");
        let mut config = Config::default();
        let mut conexao = conexao("Estoque");
        conexao.tipo = Tipo::Postgres;
        conexao.host = "10.0.0.8".into();
        conexao.porta = Some(5433);
        conexao.usuario = "rosto".into();
        conexao.banco = "estoque".into();
        conexao.fontes.insert("clientes".into(), Fonte::default());
        config.conexoes.push(conexao);
        config.save(&path).expect("salva");

        let carregado = Config::load(&path).expect("carrega");
        assert_eq!(carregado.conexoes.len(), 1);
        let de_volta = &carregado.conexoes[0];
        assert_eq!(de_volta.tipo, Tipo::Postgres);
        assert_eq!(de_volta.porta_efetiva(), 5433, "a porta dita vale sobre a de fábrica");
        assert_eq!(de_volta.fontes.get("clientes").map(|f| f.ler), Some(true));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn arquivo_faltando_e_host_faltando_sao_recusados() {
        let mut config = Config::default();
        let mut sqlite = conexao("Arquivo");
        sqlite.arquivo = " ".into();
        assert!(config.upsert(&mut sqlite).is_err());

        let mut postgres = conexao("Rede");
        postgres.tipo = Tipo::Postgres;
        postgres.host = "10.0.0.8".into();
        postgres.banco = String::new();
        assert!(config.upsert(&mut postgres).is_err());
    }

    #[test]
    fn tipos_sao_lidos_do_rotulo_com_apelidos() {
        assert_eq!(Tipo::de_rotulo("PostgreSQL"), Some(Tipo::Postgres));
        assert_eq!(Tipo::de_rotulo("mariadb"), Some(Tipo::Mysql));
        assert_eq!(Tipo::de_rotulo("SQLServer"), Some(Tipo::Mssql));
        assert_eq!(Tipo::de_rotulo("mongodb"), Some(Tipo::Mongo));
        assert_eq!(Tipo::de_rotulo("oracle"), Some(Tipo::Oracle));
        assert_eq!(Tipo::de_rotulo("excel"), None);
    }

    #[test]
    fn remover_diz_se_havia_o_que_remover() {
        let mut config = Config::default();
        config.conexoes.push(conexao("Estoque"));
        assert!(config.remover("id-Estoque"));
        assert!(!config.remover("id-Estoque"));
    }
}
