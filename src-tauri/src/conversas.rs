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
use crate::ai::compactacao::Resumo;
use crate::ai::memoria::{CONVERSAS_MEMORIAIS, CONVERSA_MAX_BYTES, ConversaViva};
use crate::idiomas::tr;
use crate::state::trava_estado;
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
    /// O que o modelo escreveu para não ter que ler: o resumo dos turnos que
    /// saíram do prompt e o índice até onde ele cobre. `None` é a conversa lida
    /// inteira. Guarda porque recompôr a cada turno seria cobrar da placa um
    /// texto que ela já escreveu.
    #[serde(default)]
    pub resumo: Option<Resumo>,
}

/// O que abre uma conversa: o buffer vivo do motor e o resumo que pertence a
/// ele. Trocar uma sem o outro é deixar o modelo lendo o começo de uma conversa
/// que não é aquela.
#[derive(Debug, Clone)]
pub struct Aberta {
    pub mensagens: Vec<ChatMessage>,
    pub resumo: Option<Resumo>,
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
    /// O sumário das conversas em memória. Dado puro no sentido da regra de
    /// `CONTRIBUTING.md`: cada mudança é um `push`, um `retain` ou uma
    /// atribuição, e ela acontece inteira. O único trecho que toca dois campos
    /// — `apagar` tira a ficha e depois limpa a `ativa` — não deixa valor
    /// partido, só uma `ativa` apontando para quem já não está na lista, e todo
    /// leitor revalida antes de usar. `estado` é comando síncrono da janela:
    /// panicar ali fecharia o aplicativo com as conversas intactas no disco, que
    /// é o pior resultado possível para um sumário em memória.
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
pub(crate) fn id_valido(id: &str) -> bool {
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

/// Reconstrói o índice dos arquivos de conversa em disco. É o plano B honesto
/// quando o `indice.json` está ilegível: as conversas têm arquivo próprio, o que
/// se perdeu foi só o sumário — reler a pasta as traz de volta em vez de deixar
/// órfãs. Uma conversa que também não abre fica de fora, em silêncio: não se
/// recupera o que não se lê, do mesmo jeito que `para_memoria` faz.
///
/// O `ativa` não volta: nada no arquivo diz qual estava no buffer, e escolher
/// uma por conta seria inventar estado. A lista é o que importa, e a próxima
/// conversa ativa nasce do primeiro pedido de quem voltou.
fn reconstruir_indice(raiz: &Path) -> Indice {
    let mut indice = Indice::default();
    let Ok(entradas) = std::fs::read_dir(raiz) else {
        return indice;
    };
    for entrada in entradas.flatten() {
        let caminho = entrada.path();
        if caminho.file_name().map(|n| n == "indice.json").unwrap_or(false) {
            continue;
        }
        if caminho.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        // O arquivo é `<id>.json`; um nome que não é id deste módulo (um `.tmp`
        // de gravação interrompida, por exemplo) não entra.
        let Some(id) = caminho.file_stem().and_then(|s| s.to_str()) else { continue };
        if !id_valido(id) {
            continue;
        }
        let Ok(conversa) = ler_json::<Conversa>(&caminho) else { continue };
        let mut ficha = conversa.ficha();
        // O identificador canônico é o nome do arquivo, que é como `arquivo()`
        // resolve uma abertura depois — o `id` de dentro é redundante.
        ficha.id = id.to_string();
        indice.conversas.push(ficha);
    }
    indice
        .conversas
        .sort_by(|a, b| b.atualizada.cmp(&a.atualizada).then(b.id.cmp(&a.id)));
    indice
}

impl Conversas {
    pub fn novo(raiz: PathBuf) -> Self {
        // Um `indice.json` corrompido não apaga as conversas: elas existem em
        // arquivos próprios. Se o sumário não abre, a lista é refeita do disco.
        let indice = match ler_json::<Indice>(&raiz.join("indice.json")) {
            Ok(indice) => indice,
            Err(_) => reconstruir_indice(&raiz),
        };
        Self { raiz, indice: Mutex::new(indice) }
    }

    pub fn estado(&self) -> Estado {
        let indice = trava_estado(&self.indice);
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
        let ativa = trava_estado(&self.indice).ativa.clone();
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
            resumo: None,
        };
        gravar_json(&arquivo(&self.raiz, &conversa.id), &conversa)?;
        let ficha = conversa.ficha();
        let mut indice = trava_estado(&self.indice);
        indice.conversas.push(ficha.clone());
        indice.ativa = Some(conversa.id);
        self.salvar_indice(&indice)?;
        Ok(ficha)
    }

    /// Carrega a conversa e a torna ativa. Quem chama põe as mensagens no buffer
    /// vivo, o resumo no dele, e manda o motor esquecer o KV anterior.
    pub fn abrir(&self, id: &str) -> Result<Aberta> {
        if !id_valido(id) {
            bail!(tr("erros.conversaIdInvalido", &[("id", id)]));
        }
        let indice = trava_estado(&self.indice);
        if !indice.conversas.iter().any(|f| f.id == id) {
            bail!(tr("erros.conversaDesconhecida", &[("id", id)]));
        }
        drop(indice);
        let conversa: Conversa = ler_json(&arquivo(&self.raiz, id))?;
        let mut indice = trava_estado(&self.indice);
        indice.ativa = Some(id.to_string());
        self.salvar_indice(&indice)?;
        Ok(Aberta { mensagens: conversa.mensagens, resumo: conversa.resumo })
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
        // O lock cobre o ler→mudar→gravar do arquivo E o update do índice. Sem
        // isso, um renomear e um gravar de turno no mesmo id leem o mesmo estado
        // e cada um escreve o arquivo inteiro: o último apagou o efeito do
        // outro — o título, ou as mensagens do turno. Gravar é atômico no disco,
        // mas não contra uma segunda escrita que começou antes desta.
        let mut indice = trava_estado(&self.indice);
        let Some(pos) = indice.conversas.iter().position(|f| f.id == id) else {
            bail!(tr("erros.conversaDesconhecida", &[("id", id)]));
        };
        let mut conversa: Conversa = ler_json(&caminho)?;
        conversa.titulo = titulo.to_string();
        conversa.nomeada = true;
        gravar_json(&caminho, &conversa)?;
        let ficha = conversa.ficha();
        indice.conversas[pos] = ficha.clone();
        self.salvar_indice(&indice)?;
        Ok(ficha)
    }

    /// Grava o buffer vivo na conversa. O título automático acompanha o primeiro
    /// pedido enquanto ninguém nomeou a conversa: um nome escrito não é
    /// sobrescrito por turno, e um histórico limpo devolve o título ao vazio.
    ///
    /// O resumo só fica quando ainda diz alguma verdade sobre estas mensagens:
    /// um índice depois do fim do histórico esconderia do modelo uma conversa
    /// que ele deveria ler inteira. Um histórico limpo leva o resumo junto.
    pub fn gravar(
        &self,
        id: &str,
        mensagens: &[ChatMessage],
        resumo: Option<&Resumo>,
    ) -> Result<Ficha> {
        if !id_valido(id) {
            bail!(tr("erros.conversaIdInvalido", &[("id", id)]));
        }
        let caminho = arquivo(&self.raiz, id);
        // O lock cobre o ler→mudar→gravar do arquivo E o update do índice, pelo
        // mesmo motivo de renomear: sem ele, um gravar de turno e um renomear no
        // mesmo id leem o mesmo estado e cada um escreve o arquivo inteiro por
        // cima — o último apaga o título (ou as mensagens) que o outro acabou de
        // pôr, e as duas escritas ainda se atropelam no mesmo arquivo .tmp.
        // Gravar é atômico no disco contra um leitor, não contra uma segunda
        // escrita que começou lendo um estado anterior.
        let mut indice = trava_estado(&self.indice);
        let Some(pos) = indice.conversas.iter().position(|f| f.id == id) else {
            bail!(tr("erros.conversaDesconhecida", &[("id", id)]));
        };
        let mut conversa: Conversa = ler_json(&caminho)?;
        if !conversa.nomeada {
            conversa.titulo = titulo_de(mensagens);
        }
        conversa.mensagens = mensagens.to_vec();
        conversa.resumo = resumo.filter(|r| r.ate <= mensagens.len()).cloned();
        conversa.atualizada = agora();
        gravar_json(&caminho, &conversa)?;
        let ficha = conversa.ficha();
        indice.conversas[pos] = ficha.clone();
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
        let mut indice = trava_estado(&self.indice);
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

    /// O que a memória lê do histórico: as conversas mais novas, cada uma com a
    /// ficha que o cartão mostra e o texto do que foi dito. É a janela de
    /// `CONVERSAS_MEMORIAIS` — o que ficou para trás continua na tela, só não
    /// está no índice.
    ///
    /// A conversa ativa entra, e não de propósito para o turno reler a si mesmo:
    /// quem decide isso é a busca, com `ignorar`. Aqui a lista é o histórico, e a
    /// memória de uma conversa longa é o que a janela do modelo já não cabe.
    ///
    /// Arquivo que não abre fica de fora, em silêncio: uma conversa ilegível não
    /// pode custar o turno inteiro, e a memória não promete o que não leu.
    pub fn para_memoria(&self) -> Vec<ConversaViva> {
        let estado = self.estado();
        let mut vivas = Vec::new();
        for ficha in estado.conversas.into_iter().take(CONVERSAS_MEMORIAIS) {
            let Ok(conversa) = ler_json::<Conversa>(&arquivo(&self.raiz, &ficha.id)) else {
                continue;
            };
            let falas = transcricao(&conversa.mensagens);
            let Some((texto, a_partir_de)) = cauda(&falas, CONVERSA_MAX_BYTES) else {
                // Nada dito: uma conversa vazia não tem o que lembrar, e ficha
                // dela no índice diria que tem.
                continue;
            };
            vivas.push(ConversaViva {
                id: ficha.id,
                titulo: if ficha.titulo.is_empty() {
                    tr("memoria.conversaSemTitulo", &[])
                } else {
                    ficha.titulo
                },
                atualizada: ficha.atualizada,
                mensagens: ficha.mensagens,
                texto,
                a_partir_de,
            });
        }
        vivas
    }
}

/// O texto lembrado de uma conversa: uma fala por linha, com o rótulo de quem
/// falou. Papéis que não são fala (`system`, `tool`) e a linha de uma chamada de
/// ferramenta ficam de fora de propósito — o índice existe para responder o que
/// as duas partes disseram, e o JSON de uma chamada responderia como se fosse
/// conversa.
///
/// Uma fala por linha é o que faz a linha citada no cartão ser a fala: o índice
/// numera as linhas do texto que recebeu.
fn transcricao(mensagens: &[ChatMessage]) -> Vec<String> {
    mensagens
        .iter()
        .filter_map(|mensagem| {
            let quem = match mensagem.role.as_str() {
                "user" => tr("memoria.falaUsuario", &[]),
                "assistant" => tr("memoria.falaAssistente", &[]),
                _ => return None,
            };
            let texto = mensagem.content.split_whitespace().collect::<Vec<_>>().join(" ");
            (!texto.is_empty()).then(|| format!("{quem}: {texto}"))
        })
        .collect()
}

/// A cauda do histórico, do limite para trás. O corte é em fim de linha porque
/// metade de uma fala no índice não é lembrança — é texto picado; e a linha só
/// sai inteira, ainda que uma fala sozinha passe do teto (o fatiador dela cuida).
///
/// Devolve o texto e quantas falas ficaram para trás, que é o que devolve ao
/// cartão o número verdadeiro da fala citada. `None` é histórico sem fala.
fn cauda(falas: &[String], limite: usize) -> Option<(String, usize)> {
    let mut bytes = 0usize;
    let mut inicio = falas.len();
    while inicio > 0 {
        let custo = falas[inicio - 1].len() + 1;
        if inicio < falas.len() && bytes + custo > limite {
            break;
        }
        bytes += custo;
        inicio -= 1;
    }
    (!falas.is_empty()).then(|| (falas[inicio..].join("\n"), inicio))
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
        let gravada = conversas.gravar(&ficha.id, &pedido("crie uma página animada"), None).unwrap();
        assert_eq!(gravada.titulo, "crie uma página animada");
        assert_eq!(gravada.mensagens, 3);
        let _ = std::fs::remove_dir_all(&conversas.raiz);
    }

    #[test]
    fn o_titulo_automatico_e_curto_e_so_da_primeira_linha() {
        let conversas = temporaria("titulo-longo");
        let ficha = conversas.criar().unwrap();
        let longo = format!("{}\nsegunda linha", "a".repeat(120));
        let gravada = conversas.gravar(&ficha.id, &pedido(&longo), None).unwrap();
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
        let gravada = conversas.gravar(&ficha.id, &pedido("outro pedido"), None).unwrap();
        assert_eq!(gravada.titulo, "Landing do cliente");
        let _ = std::fs::remove_dir_all(&conversas.raiz);
    }

    #[test]
    fn limpar_o_historico_devolve_o_titulo_automatico_ao_vazio() {
        let conversas = temporaria("limpar");
        let ficha = conversas.criar().unwrap();
        conversas.gravar(&ficha.id, &pedido("pedido que nomeou"), None).unwrap();
        let limpa = conversas.gravar(&ficha.id, &[], None).unwrap();
        assert_eq!(limpa.titulo, "");
        assert_eq!(limpa.mensagens, 0);
        // O nome escrito sobrevive à limpeza: ele é da pessoa, não do histórico.
        conversas.renomear(&ficha.id, "Landing").unwrap();
        conversas.gravar(&ficha.id, &pedido("outro"), None).unwrap();
        assert_eq!(conversas.gravar(&ficha.id, &[], None).unwrap().titulo, "Landing");
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
        conversas.gravar(&ficha.id, &mensagens, None).unwrap();

        // Outro dono, lendo do disco: é o que acontece ao reabrir o aplicativo.
        let reaberto = Conversas::novo(conversas.raiz.clone());
        let lidas = reaberto.abrir(&ficha.id).unwrap().mensagens;
        assert_eq!(lidas.len(), mensagens.len());
        let chamadas = lidas[3].tool_calls.as_ref().unwrap();
        assert_eq!(chamadas.len(), 1);
        assert_eq!(chamadas[0].name, "local__escrever_arquivo");
        assert_eq!(lidas[4].content, "escrito");
        assert_eq!(reaberto.estado().ativa.as_deref(), Some(ficha.id.as_str()));
        let _ = std::fs::remove_dir_all(&conversas.raiz);
    }

    /// O resumo é o texto que o modelo escreveu para não ter que reler os
    /// turnos antigos. Perder isto no disco é cobrar da placa, turno após turno,
    /// o resumo de uma conversa de vinte turnos.
    #[test]
    fn o_resumo_sobrevive_a_reabrir_a_conversa() {
        let conversas = temporaria("resumo");
        let ficha = conversas.criar().unwrap();
        let resumo = Resumo { texto: "a pessoa pediu a página e ela foi criada".into(), ate: 2 };
        conversas.gravar(&ficha.id, &pedido("crie a página"), Some(&resumo)).unwrap();

        let reaberta = Conversas::novo(conversas.raiz.clone()).abrir(&ficha.id).unwrap();
        assert_eq!(reaberta.resumo.as_ref(), Some(&resumo));
        assert_eq!(reaberta.mensagens.len(), 3);
        let _ = std::fs::remove_dir_all(&conversas.raiz);
    }

    /// Índice depois do fim do histórico é resumo de mensagem que não existe:
    /// ele esconderia do modelo a conversa que ele deveria ler inteira.
    #[test]
    fn um_resumo_fora_do_historico_nao_e_guardado() {
        let conversas = temporaria("resumo-fora");
        let ficha = conversas.criar().unwrap();
        let fora = Resumo { texto: "cobria dez turnos".into(), ate: 9 };
        conversas.gravar(&ficha.id, &pedido("oi"), Some(&fora)).unwrap();
        assert_eq!(Conversas::novo(conversas.raiz.clone()).abrir(&ficha.id).unwrap().resumo, None);
        let _ = std::fs::remove_dir_all(&conversas.raiz);
    }

    /// Limpar o histórico é limpar o que ele cobria. Uma conversa vazia com
    /// resumo na mão seria o modelo começando uma conversa nova lendo um texto
    /// que ninguém escreveu nela.
    #[test]
    fn limpar_a_conversa_esquece_o_resumo() {
        let conversas = temporaria("resumo-limpo");
        let ficha = conversas.criar().unwrap();
        let resumo = Resumo { texto: "o que aconteceu antes".into(), ate: 1 };
        conversas.gravar(&ficha.id, &pedido("oi"), Some(&resumo)).unwrap();
        conversas.gravar(&ficha.id, &[], None).unwrap();
        let limpa = Conversas::novo(conversas.raiz.clone()).abrir(&ficha.id).unwrap();
        assert_eq!(limpa.resumo, None);
        assert!(limpa.mensagens.is_empty());
        let _ = std::fs::remove_dir_all(&conversas.raiz);
    }

    /// As conversas gravadas antes da compactação não têm a chave. Elas têm de
    /// abrir como conversa lida inteira — não como arquivo inválido.
    #[test]
    fn uma_conversa_sem_a_chave_do_resumo_abre_normal() {
        let conversas = temporaria("resumo-antiga");
        let ficha = conversas.criar().unwrap();
        conversas.gravar(&ficha.id, &pedido("oi"), None).unwrap();
        let caminho = arquivo(&conversas.raiz, &ficha.id);
        let velho: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&caminho).unwrap()).unwrap();
        let mut sem_resumo = velho.as_object().unwrap().clone();
        sem_resumo.remove("resumo");
        std::fs::write(&caminho, serde_json::to_string(&sem_resumo).unwrap()).unwrap();

        let aberta = Conversas::novo(conversas.raiz.clone()).abrir(&ficha.id).unwrap();
        assert_eq!(aberta.resumo, None);
        assert_eq!(aberta.mensagens.len(), 3);
        let _ = std::fs::remove_dir_all(&conversas.raiz);
    }

    #[test]
    fn duas_conversas_nao_se_misturam() {
        let conversas = temporaria("duas");
        let a = conversas.criar().unwrap();
        conversas.gravar(&a.id, &pedido("pedido da primeira"), None).unwrap();
        let b = conversas.criar().unwrap();
        conversas.gravar(&b.id, &pedido("pedido da segunda"), None).unwrap();

        assert_eq!(conversas.abrir(&a.id).unwrap().mensagens[1].content, "pedido da primeira");
        assert_eq!(conversas.abrir(&b.id).unwrap().mensagens[1].content, "pedido da segunda");
        let estado = conversas.estado();
        assert_eq!(estado.conversas.len(), 2);
        assert_eq!(estado.ativa.as_deref(), Some(b.id.as_str()));
        let _ = std::fs::remove_dir_all(&conversas.raiz);
    }

    #[test]
    fn apagar_leva_o_arquivo_e_a_entrada() {
        let conversas = temporaria("apagar");
        let a = conversas.criar().unwrap();
        conversas.gravar(&a.id, &pedido("pedido"), None).unwrap();
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
            assert!(conversas.gravar(id, &[], None).is_err(), "{id}");
            assert!(conversas.renomear(id, "x").is_err(), "{id}");
        }
        let _ = std::fs::remove_dir_all(&conversas.raiz);
    }

    #[test]
    fn a_lista_vem_da_mais_recente_para_a_mais_antiga() {
        let conversas = temporaria("ordem");
        let velha = conversas.criar().unwrap();
        conversas.gravar(&velha.id, &pedido("velha"), None).unwrap();
        // O relógio tem resolução de milissegundo: sem esta pausa as duas
        // conversas nasceriam no mesmo instante e a ordem seria a do id.
        std::thread::sleep(std::time::Duration::from_millis(5));
        let nova = conversas.criar().unwrap();
        conversas.gravar(&nova.id, &pedido("nova"), None).unwrap();
        let estado = conversas.estado();
        assert_eq!(estado.conversas[0].id, nova.id);
        assert_eq!(estado.conversas[1].id, velha.id);
        let _ = std::fs::remove_dir_all(&conversas.raiz);
    }

    #[test]
    fn o_indice_sobrevive_a_reabrir_o_aplicativo() {
        let conversas = temporaria("indice");
        let ficha = conversas.criar().unwrap();
        conversas.gravar(&ficha.id, &pedido("pedido"), None).unwrap();
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

    /// A memória lê o histórico pela mesma janela que a tela mostra, e o que ela
    /// recebe é o que as duas partes disseram: nem o prompt de sistema, nem o
    /// JSON de uma chamada de ferramenta — que responderia a uma pergunta como se
    /// fosse alguém tendo falado.
    #[test]
    fn a_memoria_recebe_as_falas_e_nao_as_chamadas() {
        let conversas = temporaria("memoria-falas");
        let ficha = conversas.criar().unwrap();
        let mut mensagens = pedido("qual a vazão da bomba?");
        mensagens.push(ChatMessage {
            role: "assistant".to_string(),
            content: String::new(),
            tool_calls: Some(vec![crate::ai::ToolCall {
                name: "local__ler_arquivo".to_string(),
                arguments: serde_json::json!({"caminho": "E:\\teste\\bomba.txt"}),
            }]),
        });
        mensagens.push(ChatMessage::new("tool", "4,2 m³/h"));
        conversas.gravar(&ficha.id, &mensagens, None).unwrap();

        let vivas = conversas.para_memoria();
        assert_eq!(vivas.len(), 1);
        let viva = &vivas[0];
        assert_eq!(viva.id, ficha.id);
        assert_eq!(viva.titulo, "qual a vazão da bomba?");
        assert_eq!(viva.atualizada, conversas.estado().conversas[0].atualizada);
        // A ficha diz quantas mensagens o histórico tem; o texto diz o que foi
        // dito. Os dois números não precisam bater — e é assim que tem de ser.
        assert_eq!(viva.mensagens, 5);
        assert_eq!(viva.a_partir_de, 0);
        let falas: Vec<&str> = viva.texto.split('\n').collect();
        assert_eq!(falas.len(), 2);
        assert_eq!(falas[0], format!("{}: qual a vazão da bomba?", tr("memoria.falaUsuario", &[])));
        assert_eq!(falas[1], format!("{}: resposta", tr("memoria.falaAssistente", &[])));
        assert!(!viva.texto.contains("tool"));
        assert!(!viva.texto.contains("local__ler_arquivo"));
        let _ = std::fs::remove_dir_all(&conversas.raiz);
    }

    /// Conversa sem fala lembrada não vira ficha: um índice que promete o que não
    /// leu faria a tela dizer que a memória está em dia com um histórico vazio.
    #[test]
    fn a_conversa_recem_criada_nao_entra_na_memoria() {
        let conversas = temporaria("memoria-vazia");
        let ficha = conversas.criar().unwrap();
        assert!(conversas.para_memoria().is_empty());
        // Nem a que ficou sem texto por causa de uma limpeza.
        conversas.gravar(&ficha.id, &pedido("algo"), None).unwrap();
        assert_eq!(conversas.para_memoria().len(), 1);
        conversas.gravar(&ficha.id, &[], None).unwrap();
        assert!(conversas.para_memoria().is_empty());
        let _ = std::fs::remove_dir_all(&conversas.raiz);
    }

    /// A cauda corta em fim de fala, nunca no meio de uma, e diz quantas ficaram
    /// para trás — é este número que devolve ao cartão a fala verdadeira.
    #[test]
    fn a_cauda_corta_na_fala_e_conta_o_que_deixou_para_tras() {
        let falas: Vec<String> = (0..10).map(|i| "x".repeat(100) + &format!("-{i}")).collect();
        // O teto cabe exatamente três falas de 104 bytes (com o \n): 312.
        let (texto, a_partir_de) = cauda(&falas, 3 * 104).unwrap();
        let linhas: Vec<&str> = texto.split('\n').collect();
        assert_eq!(linhas.len(), 3);
        assert_eq!(a_partir_de, 7);
        assert_eq!(linhas[0], format!("{}-7", "x".repeat(100)));
        assert_eq!(linhas[2], format!("{}-9", "x".repeat(100)));
        // A última fala entra mesmo sem caber sozinha: o fatiador cuida dela, e
        // uma conversa sem lembrança nenhuma é o pior resultado possível.
        let (texto, a_partir_de) = cauda(&falas, 10).unwrap();
        assert_eq!(a_partir_de, 9);
        assert_eq!(texto, format!("{}-9", "x".repeat(100)));
        // Sem limite, entra tudo.
        let (texto, a_partir_de) = cauda(&falas, usize::MAX).unwrap();
        assert_eq!(a_partir_de, 0);
        assert_eq!(texto.split('\n').count(), 10);
        assert_eq!(cauda(&[], 100), None);
    }

    /// O teto por conversa é medido no texto que sai, não no que foi escrito: uma
    /// conversa de um megabyte entra no índice do tamanho da janela, e o resto
    /// fica onde a tela ainda pode ler.
    #[test]
    fn a_memoria_recebe_no_maximo_a_cauda_de_cada_conversa() {
        let conversas = temporaria("memoria-cauda");
        let ficha = conversas.criar().unwrap();
        let longo = "a".repeat(4000);
        let mut mensagens = Vec::new();
        for _ in 0..20 {
            mensagens.push(ChatMessage::new("user", &longo));
            mensagens.push(ChatMessage::new("assistant", &longo));
        }
        conversas.gravar(&ficha.id, &mensagens, None).unwrap();
        let vivas = conversas.para_memoria();
        assert_eq!(vivas.len(), 1);
        let viva = &vivas[0];
        let falas = viva.texto.split('\n').count();
        // Aqui toda mensagem é uma fala: as lembradas são as que não ficaram
        // para trás.
        assert_eq!(falas, viva.mensagens - viva.a_partir_de);
        assert!(falas > 1 && viva.a_partir_de > 0);
        // O custo com que a cauda é medida é fala + o \n: é assim que ela cabe.
        assert!(
            viva.texto.len() + falas <= CONVERSA_MAX_BYTES,
            "passou do teto: {} bytes",
            viva.texto.len()
        );
        // Toda fala lembrada é fala inteira, até a última — que é a última de
        // verdade do histórico.
        assert!(viva.texto.split('\n').all(|fala| fala.ends_with(&longo)));
        let _ = std::fs::remove_dir_all(&conversas.raiz);
    }

    /// A janela tem um teto de conversas: o que ficou para trás continua no
    /// histórico e na tela, só não vira vetor no turno.
    #[test]
    fn a_memoria_cobre_a_janela_e_nao_o_historico_inteiro() {
        let conversas = temporaria("memoria-janela");
        let sobra = CONVERSAS_MEMORIAIS + 3;
        for i in 0..sobra {
            let ficha = conversas.criar().unwrap();
            conversas.gravar(&ficha.id, &pedido(&format!("pedido {i}")), None).unwrap();
        }
        assert_eq!(conversas.estado().conversas.len(), sobra);
        let vivas = conversas.para_memoria();
        assert_eq!(vivas.len(), CONVERSAS_MEMORIAIS);
        // A lista vem da mais recente para a mais antiga: o que a janela deixa
        // fora é o fim do histórico, não o começo.
        assert_eq!(vivas.first().unwrap().titulo, format!("pedido {}", sobra - 1));
        assert_eq!(
            vivas.last().unwrap().titulo,
            format!("pedido {}", sobra - CONVERSAS_MEMORIAIS)
        );
        let _ = std::fs::remove_dir_all(&conversas.raiz);
    }

    /// A memória conta as falas pelo que o histórico tem agora. É este número,
    /// e não o texto, que a ficha prova contra o índice: ganhou fala, o trabalho
    /// volta a existir.
    #[test]
    fn a_memoria_recebe_a_conversa_com_o_estado_novo() {
        let conversas = temporaria("memoria-contagem");
        let ficha = conversas.criar().unwrap();
        conversas.gravar(&ficha.id, &pedido("primeira"), None).unwrap();
        let primeira = conversas.para_memoria().remove(0);
        assert_eq!(primeira.mensagens, 3);
        assert_eq!(primeira.texto.split('\n').count(), 2);

        let mut mais = pedido("segunda");
        mais.push(ChatMessage::new("assistant", "e o filtro também"));
        conversas.gravar(&ficha.id, &mais, None).unwrap();
        let segunda = conversas.para_memoria().remove(0);
        assert_eq!(segunda.mensagens, 4);
        assert_eq!(segunda.texto.split('\n').count(), 3);
        assert!(segunda.texto.ends_with(&format!("{}: e o filtro também", tr("memoria.falaAssistente", &[]))));
        let _ = std::fs::remove_dir_all(&conversas.raiz);
    }

    #[test]
    fn um_sumario_corrompido_nao_apaga_as_conversas() {
        let conversas = temporaria("sumario-corrompido");
        let ficha = conversas.criar().unwrap();
        conversas.gravar(&ficha.id, &pedido("crie uma página animada"), None).unwrap();
        let raiz = conversas.raiz.clone();
        drop(conversas);
        // Só o sumário é destruído; o arquivo da conversa continua inteiro.
        std::fs::write(raiz.join("indice.json"), b"{").unwrap();
        let reaberto = Conversas::novo(raiz.clone());
        let estado = reaberto.estado();
        assert_eq!(estado.conversas.len(), 1, "a conversa sobrevive ao sumário perdido");
        assert_eq!(estado.conversas[0].id, ficha.id);
        assert_eq!(estado.conversas[0].titulo, "crie uma página animada");
        // Recuperada de verdade: dá para abrir e o histórico vem do arquivo.
        let aberta = reaberto.abrir(&ficha.id).unwrap();
        assert_eq!(aberta.mensagens.len(), 3);
        let _ = std::fs::remove_dir_all(&raiz);
    }

    #[test]
    fn renomear_e_gravar_concorrentes_nao_se_atropelam() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Barrier;

        let conversas = temporaria("renomeia-grava-race");
        let ficha = conversas.criar().unwrap();
        let id = ficha.id.clone();
        let caminho = arquivo(&conversas.raiz, &id);
        let titular = "nome escrito a mao";
        let mensagens = pedido("assunto razoavelmente longo para o turno de hoje");
        let perdido = AtomicUsize::new(0);

        for _ in 0..150 {
            // Cada rodada recomeça do mesmo estado: sem nome e sem mensagens.
            let base = Conversa {
                id: id.clone(),
                titulo: String::new(),
                nomeada: false,
                criada: agora(),
                atualizada: agora(),
                mensagens: Vec::new(),
                resumo: None,
            };
            gravar_json(&caminho, &base).unwrap();

            let conv = &conversas;
            let id_ref = &id;
            let msgs_ref = &mensagens;
            let barreira = Barrier::new(3);
            std::thread::scope(|cena| {
                let r1 = &barreira;
                cena.spawn(move || {
                    r1.wait();
                    let _ = conv.renomear(id_ref, titular);
                });
                let r2 = &barreira;
                cena.spawn(move || {
                    r2.wait();
                    let _ = conv.gravar(id_ref, msgs_ref, None);
                });
                barreira.wait();
            });

            // Dos dois efeitos nenhum pode sumir: o nome dado à mão e o histórico
            // do turno. Um ler→escrever sem lock faz o último apagar o outro.
            let final_: Conversa = ler_json(&caminho).unwrap();
            if !(final_.nomeada
                && final_.titulo == titular
                && final_.mensagens.len() == mensagens.len())
            {
                perdido.fetch_add(1, Ordering::Relaxed);
            }
        }

        assert_eq!(
            perdido.load(Ordering::Relaxed),
            0,
            "renomear e gravar no mesmo id se atropelaram: um efeito sumiu do arquivo"
        );
        let _ = std::fs::remove_dir_all(&conversas.raiz);
    }

    /// Vermelho-primeiro (CONTRATO): o `expect` na trava do índice derrubava a
    /// thread que lesse a lista, e `estado` é comando síncrono da tela — um panic
    /// ali fecha o aplicativo com as conversas intactas no disco, que é o pior
    /// resultado possível para um sumário em memória. O índice é dado puro: cada
    /// campo muda inteiro por vez, então atrás de uma trava envenenada está a
    /// lista de sempre, e a política certa é `into_inner`.
    #[test]
    fn o_indice_envenenado_devolve_as_conversas_em_vez_de_panicar() {
        use std::sync::Arc;
        let conversas = Arc::new(temporaria("indice-envenenado"));
        let ficha = conversas.criar().unwrap();

        let dentro = Arc::clone(&conversas);
        let envenenador = std::thread::spawn(move || {
            let _guard = dentro.indice.lock().expect("índice envenenado");
            panic!("um panic segurando a trava do índice");
        });
        let _ = envenenador.join();
        assert!(
            conversas.indice.is_poisoned(),
            "o teste não envenenou a trava do índice"
        );

        let estado = conversas.estado();
        assert_eq!(
            estado.conversas.len(),
            1,
            "a lista perdeu uma conversa no envenenamento"
        );
        assert_eq!(estado.ativa.as_deref(), Some(ficha.id.as_str()));
        // Recuperação de verdade é ler e escrever: o próximo turno grava-se por
        // cima do índice envenenado em vez de panicar na thread da janela.
        conversas
            .gravar(&ficha.id, &pedido("pedido depois do panic"), None)
            .unwrap();
        let depois = conversas.estado();
        assert_eq!(depois.conversas[0].mensagens, 3);
        // A marca de que um panic aconteceu fica onde ela pode ser lida: o que
        // `trava_estado` devolve é o estado de verdade, não uma lavagem da
        // memória de que a thread caiu.
        assert!(conversas.indice.is_poisoned());
        let _ = std::fs::remove_dir_all(&conversas.raiz);
    }
}