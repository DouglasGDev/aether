//! Conversas salvas: o histórico deixa de morrer com a janela.
//!
//! Até aqui havia uma conversa só, em memória, e fechá-la era perdê-la. Este
//! módulo dá a cada conversa um arquivo próprio em `conversas/<id>.json` e um
//! índice leve ao lado — a tela lista títulos e datas sem ler histórico nenhum,
//! e um turno grava só a conversa que mudou.
//!
//! Três decisões que não são cosméticas:
//!
//! - **O título vem da conversa.** Uma conversa nova não tem título: ele nasce
//!   da primeira linha do primeiro pedido, e um título que a pessoa escreveu não
//!   é sobrescrito pelo automático. Nada aqui inventa um nome de exemplo.
//! - **O id não vira caminho sem conferência.** Ele chega da interface, então é
//!   aceito só no formato que este módulo gera (hex e hífen). Um `../` num id
//!   seria escrita fora da pasta das conversas.
//! - **Trocar de conversa esquece o motor.** O buffer vivo é o da conversa
//!   ativa; quem troca carrega o histórico do arquivo e manda o motor esquecer
//!   o KV, porque o cache da conversa anterior não é prefixo desta.

use crate::ai::ChatMessage;
use crate::idiomas::tr;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// O que a lista mostra. Não carrega mensagens: é o que cabe num índice que a
/// tela relê a cada turno sem custar o histórico inteiro.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ficha {
    pub id: String,
    /// Vazio até a primeira mensagem — ou até a pessoa nomear.
    pub titulo: String,
    pub criada: u64,
    pub atualizada: u64,
    pub mensagens: usize,
}

/// A conversa inteira, como fica no disco.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversa {
    pub id: String,
    pub titulo: String,
    /// Alguém nomeou esta conversa. Um nome escrito não é sobrescrito pelo
    /// título automático — nem quando o histórico é limpo.
    #[serde(default)]
    pub nomeada: bool,
    pub criada: u64,
    pub atualizada: u64,
    pub mensagens: Vec<ChatMessage>,
}

impl Conversa {
    fn ficha(&self) -> Ficha {
        Ficha {
            id: self.id.clone(),
            titulo: self.titulo.clone(),
            criada: self.criada,
            atualizada: self.atualizada,
            mensagens: self.mensagens.len(),
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Indice {
    ativa: Option<String>,
    conversas: Vec<Ficha>,
}

/// O que a tela recebe de uma vez: a lista e qual delas está no buffer vivo.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Estado {
    pub ativa: Option<String>,
    pub conversas: Vec<Ficha>,
}

pub struct Conversas {
    raiz: PathBuf,
    indice: Mutex<Indice>,
}

fn agora() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Hex do relógio mais um contador do processo: duas conversas criadas no mesmo
/// milissegundo não podem receber o mesmo arquivo.
static SEQUENCIA: AtomicU64 = AtomicU64::new(0);

fn novo_id() -> String {
    format!("{:x}-{:x}", agora(), SEQUENCIA.fetch_add(1, Ordering::Relaxed))
}

/// Um id deste módulo é hex e hífen. Isto é fronteira: o valor vem da interface,
/// e um id com separador de pasta escreveria fora da raiz das conversas.
fn id_valido(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

fn arquivo(raiz: &Path, id: &str) -> PathBuf {
    raiz.join(format!("{id}.json"))
}

/// Escrita atômica: ou a conversa inteira está no disco, ou fica a anterior.
/// Um turno interrompido no meio da gravação não pode deixar meia conversa.
fn gravar_json<T: Serialize>(caminho: &Path, valor: &T) -> Result<()> {
    if let Some(pai) = caminho.parent() {
        std::fs::create_dir_all(pai).map_err(|e| {
            anyhow::anyhow!(tr(
                "erros.falhaCriarPasta",
                &[("caminho", &pai.display().to_string()), ("detalhe", &e.to_string())]
            ))
        })?;
    }
    let texto = serde_json::to_string_pretty(valor).map_err(|e| {
        anyhow::anyhow!(tr(
            "erros.falhaSerializar",
            &[("caminho", &caminho.display().to_string()), ("detalhe", &e.to_string())]
        ))
    })?;
    let temporario = caminho.with_extension("json.tmp");
    std::fs::write(&temporario, &texto).map_err(|e| {
        anyhow::anyhow!(tr(
            "erros.falhaEscrever",
            &[("caminho", &temporario.display().to_string()), ("detalhe", &e.to_string())]
        ))
    })?;
    std::fs::rename(&temporario, caminho).map_err(|e| {
        anyhow::anyhow!(tr(
            "erros.falhaMover",
            &[("caminho", &caminho.display().to_string()), ("detalhe", &e.to_string())]
        ))
    })?;
    Ok(())
}

fn ler_json<T: for<'de> Deserialize<'de>>(caminho: &Path) -> Result<T> {
    let texto = std::fs::read_to_string(caminho).map_err(|e| {
        anyhow::anyhow!(tr(
            "erros.falhaLer",
            &[("caminho", &caminho.display().to_string()), ("detalhe", &e.to_string())]
        ))
    })?;
    serde_json::from_str(&texto).map_err(|e| {
        anyhow::anyhow!(tr(
            "erros.jsonInvalidoEm",
            &[("caminho", &caminho.display().to_string()), ("detalhe", &e.to_string())]
        ))
    })
}

/// O título automático: a primeira linha do primeiro pedido, curta. É o que a
/// pessoa escreveu, não um rótulo nosso — e uma conversa sem pedido fica sem
/// título, que é a verdade sobre ela.
fn titulo_de(mensagens: &[ChatMessage]) -> String {
    const LIMITE: usize = 48;
    let pedido = mensagens
        .iter()
        .find(|m| m.role == "user")
        .map(|m| m.content.as_str())
        .unwrap_or("");
    let linha = pedido.lines().next().unwrap_or("").trim();
    let mut titulo: String = linha.chars().take(LIMITE).collect();
    if linha.chars().count() > LIMITE {
        titulo.push('…');
    }
    titulo
}

impl Conversas {
    pub fn novo(raiz: PathBuf) -> Self {
        let indice = ler_json::<Indice>(&raiz.join("indice.json")).unwrap_or_default();
        Self { raiz, indice: Mutex::new(indice) }
    }

    pub fn estado(&self) -> Estado {
        let indice = self.indice.lock().expect("conversas envenenadas");
        let mut conversas = indice.conversas.clone();
        conversas.sort_by(|a, b| b.atualizada.cmp(&a.atualizada).then(b.id.cmp(&a.id)));
        Estado { ativa: indice.ativa.clone(), conversas }
    }

    fn indice_path(&self) -> PathBuf {
        self.raiz.join("indice.json")
    }

    fn salvar_indice(&self, indice: &Indice) -> Result<()> {
        gravar_json(&self.indice_path(), indice)
    }

    /// A conversa ativa, criando uma se não houver: o primeiro pedido que a
    /// pessoa escreve já tem onde ficar, sem um clique antes.
    pub fn garantir(&self) -> Result<String> {
        let ativa = self.indice.lock().expect("conversas envenenadas").ativa.clone();
        match ativa {
            Some(id) => Ok(id),
            None => self.criar().map(|ficha| ficha.id),
        }
    }

    pub fn criar(&self) -> Result<Ficha> {
        let conversa = Conversa {
            id: novo_id(),
            titulo: String::new(),
            nomeada: false,
            criada: agora(),
            atualizada: agora(),
            mensagens: Vec::new(),
        };
        gravar_json(&arquivo(&self.raiz, &conversa.id), &conversa)?;
        let ficha = conversa.ficha();
        let mut indice = self.indice.lock().expect("conversas envenenadas");
        indice.conversas.push(ficha.clone());
        indice.ativa = Some(conversa.id);
        self.salvar_indice(&indice)?;
        Ok(ficha)
    }

    /// Carrega as mensagens da conversa e a torna ativa. Quem chama põe o
    /// resultado no buffer vivo e manda o motor esquecer o KV anterior.
    pub fn abrir(&self, id: &str) -> Result<Vec<ChatMessage>> {
        if !id_valido(id) {
            bail!(tr("erros.conversaIdInvalido", &[("id", id)]));
        }
        let indice = self.indice.lock().expect("conversas envenenadas");
        if !indice.conversas.iter().any(|f| f.id == id) {
            bail!(tr("erros.conversaDesconhecida", &[("id", id)]));
        }
        drop(indice);
        let conversa: Conversa = ler_json(&arquivo(&self.raiz, id))?;
        let mut indice = self.indice.lock().expect("conversas envenenadas");
        indice.ativa = Some(id.to_string());
        self.salvar_indice(&indice)?;
        Ok(conversa.mensagens)
    }

    pub fn renomear(&self, id: &str, titulo: &str) -> Result<Ficha> {
        if !id_valido(id) {
            bail!(tr("erros.conversaIdInvalido", &[("id", id)]));
        }
        let titulo = titulo.trim();
        if titulo.is_empty() {
            bail!(tr("erros.conversaTituloVazio", &[]));
        }
        let caminho = arquivo(&self.raiz, id);
        let mut conversa: Conversa = ler_json(&caminho)?;
        conversa.titulo = titulo.to_string();
        conversa.nomeada = true;
        gravar_json(&caminho, &conversa)?;
        let ficha = conversa.ficha();
        let mut indice = self.indice.lock().expect("conversas envenenadas");
        match indice.conversas.iter_mut().find(|f| f.id == id) {
            Some(entrada) => *entrada = ficha.clone(),
            None => bail!(tr("erros.conversaDesconhecida", &[("id", id)])),
        }
        self.salvar_indice(&indice)?;
        Ok(ficha)
    }

    /// Grava o buffer vivo na conversa. O título automático acompanha o primeiro
    /// pedido enquanto ninguém nomeou a conversa: um nome escrito não é
    /// sobrescrito por turno, e um histórico limpo devolve o título ao vazio.
    pub fn gravar(&self, id: &str, mensagens: &[ChatMessage]) -> Result<Ficha> {
        if !id_valido(id) {
            bail!(tr("erros.conversaIdInvalido", &[("id", id)]));
        }
        let caminho = arquivo(&self.raiz, id);
        let mut conversa: Conversa = ler_json(&caminho)?;
        if !conversa.nomeada {
            conversa.titulo = titulo_de(mensagens);
        }
        conversa.mensagens = mensagens.to_vec();
        conversa.atualizada = agora();
        gravar_json(&caminho, &conversa)?;
        let ficha = conversa.ficha();
        let mut indice = self.indice.lock().expect("conversas envenenadas");
        match indice.conversas.iter_mut().find(|f| f.id == id) {
            Some(entrada) => *entrada = ficha.clone(),
            None => bail!(tr("erros.conversaDesconhecida", &[("id", id)])),
        }
        self.salvar_indice(&indice)?;
        Ok(ficha)
    }

    /// Apaga a conversa e o arquivo dela. Se era a ativa, não sobra conversa
    /// ativa: o buffer vivo é limpo por quem chama, e o próximo pedido cria uma.
    pub fn apagar(&self, id: &str) -> Result<()> {
        if !id_valido(id) {
            bail!(tr("erros.conversaIdInvalido", &[("id", id)]));
        }
        let caminho = arquivo(&self.raiz, id);
        if caminho.is_file() {
            std::fs::remove_file(&caminho).map_err(|e| {
                anyhow::anyhow!(tr(
                    "erros.falhaApagar",
                    &[("caminho", &caminho.display().to_string()), ("detalhe", &e.to_string())]
                ))
            })?;
        }
        let mut indice = self.indice.lock().expect("conversas envenenadas");
        let antes = indice.conversas.len();
        indice.conversas.retain(|f| f.id != id);
        if indice.conversas.len() == antes {
            bail!(tr("erros.conversaDesconhecida", &[("id", id)]));
        }
        if indice.ativa.as_deref() == Some(id) {
            indice.ativa = None;
        }
        self.salvar_indice(&indice)?;
        Ok(())
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    fn temporaria(nome: &str) -> Conversas {
        let raiz = std::env::temp_dir().join(format!("aether-conversas-{nome}"));
        let _ = std::fs::remove_dir_all(&raiz);
        Conversas::novo(raiz)
    }

    fn pedido(texto: &str) -> Vec<ChatMessage> {
        vec![
            ChatMessage::new("system", "prompt"),
            ChatMessage::new("user", texto),
            ChatMessage::new("assistant", "resposta"),
        ]
    }

    #[test]
    fn uma_conversa_nova_nao_tem_titulo_nem_mensagens() {
        let conversas = temporaria("nova");
        let ficha = conversas.criar().unwrap();
        assert_eq!(ficha.titulo, "");
        assert_eq!(ficha.mensagens, 0);
        assert_eq!(conversas.estado().ativa.as_deref(), Some(ficha.id.as_str()));
        assert!(arquivo(&conversas.raiz, &ficha.id).is_file());
        let _ = std::fs::remove_dir_all(&conversas.raiz);
    }

    #[test]
    fn o_titulo_nasce_do_primeiro_pedido() {
        let conversas = temporaria("titulo");
        let ficha = conversas.criar().unwrap();
        let gravada = conversas.gravar(&ficha.id, &pedido("crie uma página animada")).unwrap();
        assert_eq!(gravada.titulo, "crie uma página animada");
        assert_eq!(gravada.mensagens, 3);
        let _ = std::fs::remove_dir_all(&conversas.raiz);
    }

    #[test]
    fn o_titulo_automatico_e_curto_e_so_da_primeira_linha() {
        let conversas = temporaria("titulo-longo");
        let ficha = conversas.criar().unwrap();
        let longo = format!("{}\nsegunda linha", "a".repeat(120));
        let gravada = conversas.gravar(&ficha.id, &pedido(&longo)).unwrap();
        assert_eq!(gravada.titulo.chars().count(), 49);
        assert!(gravada.titulo.ends_with('…'));
        assert!(!gravada.titulo.contains('\n'));
        let _ = std::fs::remove_dir_all(&conversas.raiz);
    }

    #[test]
    fn um_nome_escrito_nao_e_sobrescrito_pelo_turno() {
        let conversas = temporaria("nome");
        let ficha = conversas.criar().unwrap();
        conversas.renomear(&ficha.id, "  Landing do cliente  ").unwrap();
        let gravada = conversas.gravar(&ficha.id, &pedido("outro pedido")).unwrap();
        assert_eq!(gravada.titulo, "Landing do cliente");
        let _ = std::fs::remove_dir_all(&conversas.raiz);
    }

    #[test]
    fn limpar_o_historico_devolve_o_titulo_automatico_ao_vazio() {
        let conversas = temporaria("limpar");
        let ficha = conversas.criar().unwrap();
        conversas.gravar(&ficha.id, &pedido("pedido que nomeou")).unwrap();
        let limpa = conversas.gravar(&ficha.id, &[]).unwrap();
        assert_eq!(limpa.titulo, "");
        assert_eq!(limpa.mensagens, 0);
        // O nome escrito sobrevive à limpeza: ele é da pessoa, não do histórico.
        conversas.renomear(&ficha.id, "Landing").unwrap();
        conversas.gravar(&ficha.id, &pedido("outro")).unwrap();
        assert_eq!(conversas.gravar(&ficha.id, &[]).unwrap().titulo, "Landing");
        let _ = std::fs::remove_dir_all(&conversas.raiz);
    }

    #[test]
    fn abrir_devolve_o_que_foi_gravado_inclusive_as_chamadas() {
        let conversas = temporaria("roundtrip");
        let ficha = conversas.criar().unwrap();
        let mut mensagens = pedido("escreva o arquivo");
        mensagens.push(ChatMessage {
            role: "assistant".to_string(),
            content: String::new(),
            tool_calls: Some(vec![crate::ai::ToolCall {
                name: "local__escrever_arquivo".to_string(),
                arguments: serde_json::json!({"caminho": "E:\\teste\\a.txt", "conteudo": "oi"}),
            }]),
        });
        mensagens.push(ChatMessage::new("tool", "escrito"));
        conversas.gravar(&ficha.id, &mensagens).unwrap();

        // Outro dono, lendo do disco: é o que acontece ao reabrir o aplicativo.
        let reaberto = Conversas::novo(conversas.raiz.clone());
        let lidas = reaberto.abrir(&ficha.id).unwrap();
        assert_eq!(lidas.len(), mensagens.len());
        let chamadas = lidas[3].tool_calls.as_ref().unwrap();
        assert_eq!(chamadas.len(), 1);
        assert_eq!(chamadas[0].name, "local__escrever_arquivo");
        assert_eq!(lidas[4].content, "escrito");
        assert_eq!(reaberto.estado().ativa.as_deref(), Some(ficha.id.as_str()));
        let _ = std::fs::remove_dir_all(&conversas.raiz);
    }

    #[test]
    fn duas_conversas_nao_se_misturam() {
        let conversas = temporaria("duas");
        let a = conversas.criar().unwrap();
        conversas.gravar(&a.id, &pedido("pedido da primeira")).unwrap();
        let b = conversas.criar().unwrap();
        conversas.gravar(&b.id, &pedido("pedido da segunda")).unwrap();

        assert_eq!(conversas.abrir(&a.id).unwrap()[1].content, "pedido da primeira");
        assert_eq!(conversas.abrir(&b.id).unwrap()[1].content, "pedido da segunda");
        let estado = conversas.estado();
        assert_eq!(estado.conversas.len(), 2);
        assert_eq!(estado.ativa.as_deref(), Some(b.id.as_str()));
        let _ = std::fs::remove_dir_all(&conversas.raiz);
    }

    #[test]
    fn apagar_leva_o_arquivo_e_a_entrada() {
        let conversas = temporaria("apagar");
        let a = conversas.criar().unwrap();
        conversas.gravar(&a.id, &pedido("pedido")).unwrap();
        let caminho = arquivo(&conversas.raiz, &a.id);
        assert!(caminho.is_file());
        conversas.apagar(&a.id).unwrap();
        assert!(!caminho.is_file());
        let estado = conversas.estado();
        assert!(estado.conversas.is_empty());
        assert_eq!(estado.ativa, None);
        // Apagar de novo é erro honesto, não um sucesso silencioso.
        assert!(conversas.apagar(&a.id).is_err());
        let _ = std::fs::remove_dir_all(&conversas.raiz);
    }

    #[test]
    fn garantir_cria_uma_vez_so() {
        let conversas = temporaria("garantir");
        let primeiro = conversas.garantir().unwrap();
        let segundo = conversas.garantir().unwrap();
        assert_eq!(primeiro, segundo);
        assert_eq!(conversas.estado().conversas.len(), 1);
        let _ = std::fs::remove_dir_all(&conversas.raiz);
    }

    /// O id chega da interface: um separador de pasta nele escreveria fora da
    /// raiz das conversas, e apagar com ele apagaria outro arquivo.
    #[test]
    fn um_id_com_caminho_e_recusado() {
        let conversas = temporaria("id-invalido");
        for id in ["../indice", "..\\indice", "a/b", "", "indice.json"] {
            assert!(conversas.abrir(id).is_err(), "{id}");
            assert!(conversas.apagar(id).is_err(), "{id}");
            assert!(conversas.gravar(id, &[]).is_err(), "{id}");
            assert!(conversas.renomear(id, "x").is_err(), "{id}");
        }
        let _ = std::fs::remove_dir_all(&conversas.raiz);
    }

    #[test]
    fn a_lista_vem_da_mais_recente_para_a_mais_antiga() {
        let conversas = temporaria("ordem");
        let velha = conversas.criar().unwrap();
        conversas.gravar(&velha.id, &pedido("velha")).unwrap();
        // O relógio tem resolução de milissegundo: sem esta pausa as duas
        // conversas nasceriam no mesmo instante e a ordem seria a do id.
        std::thread::sleep(std::time::Duration::from_millis(5));
        let nova = conversas.criar().unwrap();
        conversas.gravar(&nova.id, &pedido("nova")).unwrap();
        let estado = conversas.estado();
        assert_eq!(estado.conversas[0].id, nova.id);
        assert_eq!(estado.conversas[1].id, velha.id);
        let _ = std::fs::remove_dir_all(&conversas.raiz);
    }

    #[test]
    fn o_indice_sobrevive_a_reabrir_o_aplicativo() {
        let conversas = temporaria("indice");
        let ficha = conversas.criar().unwrap();
        conversas.gravar(&ficha.id, &pedido("pedido")).unwrap();
        let reaberto = Conversas::novo(conversas.raiz.clone());
        let estado = reaberto.estado();
        assert_eq!(estado.conversas.len(), 1);
        assert_eq!(estado.conversas[0].titulo, "pedido");
        assert_eq!(estado.conversas[0].mensagens, 3);
        assert_eq!(estado.ativa.as_deref(), Some(ficha.id.as_str()));
        let _ = std::fs::remove_dir_all(&conversas.raiz);
    }

    #[test]
    fn uma_pasta_sem_indice_nao_tem_conversas() {
        let conversas = temporaria("vazio");
        let estado = conversas.estado();
        assert!(estado.conversas.is_empty());
        assert_eq!(estado.ativa, None);
        let _ = std::fs::remove_dir_all(&conversas.raiz);
    }
}