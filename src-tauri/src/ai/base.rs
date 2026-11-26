//! A base de conhecimento: notas que o dono escreve — ou manda escrever — para
//! o agente saber de cabeça.
//!
//! O índice de [`crate::ai::memoria`] já sabia de duas origens: o texto de um
//! arquivo dentro de uma pasta concedida e a fala de uma conversa salva. Nenhuma
//! das duas é escolha. Um arquivo entra porque a pasta está na lista e sai quando
//! ela sai; uma conversa entra porque ela aconteceu. Falta a terceira, a que o
//! dono **decide**: "isto aqui é verdade, guarda". É isto que este arquivo é.
//!
//! ## Uma nota é um arquivo
//!
//! `<raiz de dados>/base/<id>.md`, primeira linha `# <título>`, o resto é o
//! corpo. Não há banco, não há índice lateral, não há chave em JSON nenhum:
//! abrir a pasta num editor e escrever um `.md` novo é criar uma nota, e apagá-lo
//! é apagá-la. [`listar`] adota o arquivo que chegou por fora — o título é o
//! primeiro `#` que ele tiver, e sendo ausente o nome do arquivo. É a mesma regra
//! que `conversas.rs` e `bancos/config.rs` seguem: o disco é a verdade e a
//! memória é uma cópia dele, nunca o contrário (`CONTRIBUTING.md:24`).
//!
//! Fixada é um marcador dentro do próprio arquivo (`FIXADA`), não um campo ao
//! lado. Uma chave `fixadas.json` seria a segunda verdade da nota: o dia que as
//! duas discordassem não haveria como dizer qual manda, e é exatamente isso que
//! o aplicativo proíbe. O marcador viaja com a nota quando ela é copiada para
//! outra máquina, e uma nota escrita à mão, sem marcador, entra livre — que é o
//! estado de quem ainda não decidiu.
//!
//! ## Nada de número chutado
//!
//! Toda função devolve o que mediu: os bytes do arquivo depois da escrita, os
//! caracteres e as linhas do corpo, quantas notas a pasta tem. Quem mostra um
//! número para a pessoa é um comando, e um comando só mostra o que este módulo
//! leu do disco.

use crate::ai::memoria::sem_acento;
use crate::idiomas::tr;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Nome da pasta. Ela nasce ao lado das escolhas de RAG (`rag.json`) e dos
/// adaptadores (`adaptadores.json`), na raiz de dados do aplicativo, e por uma
/// razão: a base é o material que o índice lê, então apagar `models/memoria`
/// para recomeçar a indexação não pode levar junto o que o dono escreveu.
pub const PASTA: &str = "base";

/// Extensão de uma nota. Uma só, sempre minúscula: é o que [`listar`] procura.
/// Pública porque o cartão da ferramenta e a tela citam o nome do arquivo — e o
/// nome do arquivo é `id` + esta extensão, decidido aqui, não repetido lá.
pub const EXTENSAO: &str = "md";

/// O marcador de nota fixada. Fica na linha logo abaixo do título e não é corpo:
/// [`ler_nota`] o retira antes de devolver o texto, e o índice nunca o vê. Um
/// arquivo escrito à mão sem esta linha é uma nota comum — fixar é decisão, não
/// efeito colateral de abrir o arquivo.
const FIXADA: &str = "<!-- aether:fixada -->";

/// Prefixo da linha de título em markdown.
const PREFIXO_TITULO: &str = "# ";

/// O corpo de uma nota não tem tamanho de arquivo nenhum: o teto é o da peça do
/// índice, e é ele que a busca corta, não a escrita. Este limite existe só para
/// uma nota de um megabyte por engano não travar a tela que lista a pasta.
const CORPO_MAX_BYTES: usize = 1_048_576;

/// Raiz da base a partir da raiz de dados do aplicativo.
pub fn raiz(dados_root: &Path) -> PathBuf {
    dados_root.join(PASTA)
}

/// Uma nota como o disco a guarda.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Nota {
    /// Nome do arquivo sem a extensão. É a identidade da nota: trocar o título
    /// não troca o `id`, e é por isso que o [`apagar`] de um id conhecido apaga
    /// sempre a mesma nota.
    pub id: String,
    /// A primeira linha do arquivo, sem o `# `. Ausente o `#`, é o próprio
    /// `id`, porque foi o disco que respondeu e não há o que inventar.
    pub titulo: String,
    /// O corpo, como está no arquivo, sem a linha de título e sem o marcador de
    /// fixada, aparado nos espaços das pontas — a fronteira do corpo é o
    /// arquivo, não o buffer de quem escreveu. Vazio é uma nota válida: o dono
    /// pode criar o título hoje e escrever amanhã.
    pub texto: String,
    /// Milissegundos do relógio Unix em que o arquivo nasceu. Zero é o disco
    /// dizendo que não sabe — `created` não é respondido em todo sistema de
    /// arquivos, e um zero é mais honesto que a data de hoje fingida.
    pub criado: u64,
    /// Milissegundos da última escrita no arquivo. É este número, e não o
    /// `criado`, que ordena as fixadas: a nota que o dono mexeu por último é a
    /// que ele quer na frente.
    pub atualizado: u64,
    /// `true` quando o arquivo traz o marcador de fixada.
    pub fixada: bool,
}

impl Nota {
    /// O texto que entra no índice: título e corpo juntos. O título vai porque é
    /// ele que a pessoa procura — uma nota chamada "política de reembolso"
    /// indexada sem essas três palavras só seria achada por quem acertasse o
    /// vocabulário do miolo dela.
    pub fn para_indexar(&self) -> String {
        if self.texto.trim().is_empty() {
            self.titulo.clone()
        } else {
            format!("{}\n{}", self.titulo, self.texto)
        }
    }
}

/// O que uma escrita devolve: a nota e as quatro medidas dela.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Registro {
    pub nota: Nota,
    /// Bytes do arquivo no disco, medidos depois da troca, não dos que foram
    /// pedidos antes dela.
    pub bytes: u64,
    /// Caracteres do corpo (de caracteres, não de bytes: acento conta um).
    pub caracteres: usize,
    /// Linhas do corpo.
    pub linhas: usize,
    /// Quantas notas a pasta tem agora, contadas no disco depois da escrita.
    pub notas: usize,
}

/// O que uma leitura da pasta devolve.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Lista {
    /// As notas, da mais mexida para a menos.
    pub itens: Vec<Nota>,
    /// Quantas notas a pasta tem, incluindo as que não puderam ser lidas.
    pub notas: usize,
    /// Soma dos bytes de arquivo lidos.
    pub bytes: u64,
    /// Quantas estão fixadas.
    pub fixadas: usize,
}

/// O que uma remoção devolve.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Removida {
    pub id: String,
    pub titulo: String,
    /// Bytes do arquivo que estava lá, lidos antes de ele sair.
    pub bytes: u64,
    /// Quantas notas restaram.
    pub notas: usize,
}

// ------------------------------------------------------------ como se nomeia --

/// O nome de arquivo que um título produz: minúsculo, sem acento, com hífen no
/// lugar de qualquer coisa que não é letra nem dígito. A mesma tabela
/// `SEM_ACENTO` da busca léxica manda nos acentos, de propósito: "Configuração"
/// e "configuracao" têm de dar o mesmo slug, senão a nota que o dono escreveu a
/// mão (`configuracao.md`) e a que o agente escreveria (`configuraccca~o.md`)
/// seriam duas notas para o mesmo assunto.
pub fn slug(titulo: &str) -> String {
    let mut saida = String::new();
    for letra in titulo.chars() {
        if letra.is_control() {
            continue;
        }
        let minuscula = letra.to_lowercase().next().unwrap_or(letra);
        if minuscula.is_alphanumeric() {
            saida.push(sem_acento(minuscula));
        } else if !saida.ends_with('-') {
            saida.push('-');
        }
    }
    let limpo = saida.trim_matches('-').to_string();
    if limpo.is_empty() {
        // Um título feito só de pontuação ainda é uma nota; o nome que ela
        // recebe não pode ser vazio, porque vazio não é nome de arquivo.
        return "nota".to_string();
    }
    limpo
}

/// O `n`-ésimo nome livre a partir de um slug. Colisão não sobrescreve nunca:
/// duas notas chamadas "Reunião" são duas notas, e fundir as duas por um
/// coincidência de nome seria apagar o texto de uma delas sem pedir.
fn nome_livre(raiz: &Path, base_slug: &str) -> String {
    if !caminho(raiz, base_slug).exists() {
        return base_slug.to_string();
    }
    let mut n = 2usize;
    loop {
        let candidato = format!("{base_slug}-{n}");
        if !caminho(raiz, &candidato).exists() {
            return candidato;
        }
        n += 1;
    }
}

/// O caminho de um id. A validação é curta porque o id vem do modelo: um
/// `../../windows/system32/drivers/etc/hosts` escrito como título de nota não
/// pode virar caminho, então só sobra o que é nome de arquivo de uma peça.
fn caminho(raiz: &Path, id: &str) -> PathBuf {
    raiz.join(format!("{id}.{EXTENSAO}"))
}

fn conferir_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id == "."
        || id == ".."
        || id.contains('/')
        || id.contains('\\')
        || id.contains('\0')
        || id.starts_with('.')
        || id.len() > 120
    {
        bail!(tr("base.erro.idInvalido", &[("id", id)]));
    }
    Ok(())
}

/// Milissegundos de um instante do disco. Ausente ou não suportado é zero, que é
/// o número que diz "o sistema de arquivos não respondeu" em vez de responder
/// pela pessoa.
fn milissegundos(de: SystemTime) -> u64 {
    de.duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn criado_de(meta: &std::fs::Metadata) -> u64 {
    meta.created().map(milissegundos).unwrap_or(0)
}

fn modificado_de(meta: &std::fs::Metadata) -> u64 {
    meta.modified().map(milissegundos).unwrap_or(0)
}

// ------------------------------------------------------- o arquivo e a nota --

/// O texto de uma nota, escrito na forma do disco: título, marcador quando ela é
/// fixada, corpo.
fn serializar(nota: &Nota) -> String {
    let mut saida = format!("{PREFIXO_TITULO}{}\n", nota.titulo);
    if nota.fixada {
        saida.push_str(FIXADA);
        saida.push('\n');
    }
    if !nota.texto.is_empty() {
        saida.push_str(&nota.texto);
        saida.push('\n');
    }
    saida
}

/// Lê um arquivo de nota e devolve a nota, com as datas que o disco respondeu.
///
/// Adotar é o ponto: o corpo é tudo o que vem depois do título (e do marcador,
/// quando ele está logo abaixo), e um `.md` que não tem `#` nenhum ganha o nome
/// do arquivo de título. Um arquivo que chegou por fora do aplicativo é uma nota
/// como qualquer outra no instante seguinte a ele aparecer.
fn nota_de(caminho: &Path, meta: &std::fs::Metadata, texto_bruto: &str) -> Nota {
    let id = caminho
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut linhas = texto_bruto.lines();
    let primeira = linhas.next().unwrap_or("").trim();
    let (titulo, fixada, corpo): (String, bool, Vec<&str>) = if let Some(t) =
        primeira.strip_prefix(PREFIXO_TITULO).or_else(|| primeira.strip_prefix('#').map(|r| r.trim_start()))
    {
        let titulo = t.trim().to_string();
        let resto: Vec<&str> = linhas.collect();
        let fixada = resto.first().is_some_and(|l| l.trim() == FIXADA);
        let corpo = if fixada { resto[1..].to_vec() } else { resto };
        (titulo, fixada, corpo)
    } else {
        // Sem linha de título: o arquivo inteiro é corpo, e o nome dele é o
        // título. Não é capricho de exibição — é o que o disco respondeu.
        (id.clone(), false, std::iter::once(primeira).chain(linhas).collect())
    };
    let texto = corpo.join("\n").trim_matches(['\n', '\r', ' ', '\t']).to_string();
    Nota {
        id,
        titulo,
        texto,
        criado: criado_de(meta),
        atualizado: modificado_de(meta),
        fixada,
    }
}

/// A nota de um id, quando ela existe.
pub fn ler(raiz: &Path, id: &str) -> Result<Option<Nota>> {
    conferir_id(id)?;
    let arquivo = caminho(raiz, id);
    let Ok(meta) = std::fs::metadata(&arquivo) else {
        return Ok(None);
    };
    let texto = std::fs::read_to_string(&arquivo)
        .with_context(|| tr("base.erro.leitura", &[("caminho", &arquivo.display().to_string())]))?;
    Ok(Some(nota_de(&arquivo, &meta, &texto)))
}

/// Os ids vivos da pasta: um por arquivo `.md`, sem ler conteúdo nenhum. É esta
/// lista que a busca consulta para responder `Fonte::Documento` — a nota ainda
/// existe? — do jeito que a lista de pastas concedidas responde a pergunta
/// equivalente para arquivo: lida na hora, no disco.
pub fn ids(raiz: &Path) -> Result<Vec<String>> {
    let Ok(entrada) = std::fs::read_dir(raiz) else {
        // Pasta que não existe é uma base vazia, não um erro: o aplicativo abre
        // numa máquina onde ninguém escreveu nota ainda.
        return Ok(Vec::new());
    };
    let mut vivos = Vec::new();
    for item in entrada {
        let Ok(item) = item else { continue };
        let nome = item.file_name().to_string_lossy().to_string();
        if !nome.to_lowercase().ends_with(&format!(".{EXTENSAO}")) {
            continue;
        }
        let id = nome[..nome.len() - EXTENSAO.len() - 1].to_string();
        if conferir_id(&id).is_ok() {
            vivos.push(id);
        }
    }
    vivos.sort();
    Ok(vivos)
}

/// As notas da pasta, da mais mexida para a menos.
///
/// Um arquivo ilegível não derruba a lista: ele é contado (está no disco) e
/// ignorado (não há o que mostrar dele). Silenciar os dois seria a tela prometer
/// menos notas do que a pasta tem.
pub fn listar(raiz: &Path) -> Result<Lista> {
    let Ok(entrada) = std::fs::read_dir(raiz) else {
        return Ok(Lista::default());
    };
    let mut itens: Vec<Nota> = Vec::new();
    let mut nota_viva = 0usize;
    let mut bytes = 0u64;
    for item in entrada {
        let Ok(item) = item else { continue };
        let arquivo = item.path();
        let Some(nome) = arquivo.file_name().map(|n| n.to_string_lossy().to_string()) else {
            continue;
        };
        if !nome.to_lowercase().ends_with(&format!(".{EXTENSAO}")) {
            continue;
        }
        let Ok(meta) = arquivo.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        nota_viva += 1;
        bytes += meta.len();
        let Ok(texto) = std::fs::read_to_string(&arquivo) else {
            continue;
        };
        itens.push(nota_de(&arquivo, &meta, &texto));
    }
    itens.sort_by(|a, b| {
        b.atualizado
            .cmp(&a.atualizado)
            .then_with(|| a.id.cmp(&b.id))
    });
    let fixadas = itens.iter().filter(|n| n.fixada).count();
    Ok(Lista { itens, notas: nota_viva, bytes, fixadas })
}

/// O censo da pasta: quantas notas, quantos bytes, quantas fixadas, lidos no
/// disco agora. [`listar`] mede os três; esta função existe para o comando que
/// quer o número sem o texto de cada nota.
pub fn censo(raiz: &Path) -> Result<(usize, u64, usize)> {
    let lista = listar(raiz)?;
    Ok((lista.notas, lista.bytes, lista.fixadas))
}

/// Escreve uma nota.
///
/// - `id` ausente: a nota é nova e o nome sai de [`slug`], com sufixo numérico
///   quando o nome já está tomado.
/// - `id` presente: reescreve aquela nota, **inclusive quando o título muda** —
///   o id é a identidade, e renomear o título de uma nota indexada criando um
///   arquivo novo deixaria a antiga no índice para sempre como órfã.
/// - `fixar` ausente: não mexe no marcador; numa nota nova, ela entra livre.
///   Quando a nota já estava fixada e o dono manda escrevê-la sem dizer nada, o
///   que ele tinha decidido continua.
///
/// A escrita é atômica do jeito de `local/web.rs:185` — `.md.tmp` e `rename`: um
/// corte no meio da escrita deixa a nota antiga inteira, não um markdown pela
/// metade que a busca indexaria como texto.
pub fn escrever(
    raiz: &Path,
    titulo: &str,
    texto: &str,
    id: Option<&str>,
    fixar: Option<bool>,
) -> Result<Registro> {
    std::fs::create_dir_all(raiz)
        .with_context(|| tr("base.erro.criarPasta", &[("caminho", &raiz.display().to_string())]))?;
    let titulo = titulo.trim();
    if titulo.is_empty() {
        bail!(tr("base.erro.tituloVazio", &[]));
    }
    if texto.len() > CORPO_MAX_BYTES {
        bail!(tr(
            "base.erro.corpoGrande",
            &[("bytes", &texto.len().to_string()), ("limite", &CORPO_MAX_BYTES.to_string())]
        ));
    }
    let (id, existente) = match id {
        Some(id) => {
            conferir_id(id)?;
            let antiga = ler(raiz, id)?;
            let Some(antiga) = antiga else {
                bail!(tr("base.erro.notaAusente", &[("id", id)]));
            };
            (id.to_string(), Some(antiga))
        }
        None => (nome_livre(raiz, &slug(titulo)), None),
    };
    let fixada = match (fixar, &existente) {
        (Some(v), _) => v,
        (None, Some(antiga)) => antiga.fixada,
        (None, None) => false,
    };
    let nota = Nota { id: id.clone(), titulo: titulo.to_string(), texto: texto.trim().to_string(), criado: 0, atualizado: 0, fixada };
    let arquivo = caminho(raiz, &id);
    gravar(&arquivo, &nota)
        .with_context(|| tr("base.erro.escrever", &[("caminho", &arquivo.display().to_string())]))?;
    // O que se devolve é o que o disco responde depois, não o que se pediu
    // antes: a data é a que ele gravou, os bytes são os que ele escreveu.
    let meta = arquivo
        .metadata()
        .with_context(|| tr("base.erro.medir", &[("caminho", &arquivo.display().to_string())]))?;
    let gravada = nota_de(&arquivo, &meta, &std::fs::read_to_string(&arquivo)?);
    Ok(Registro {
        bytes: meta.len(),
        caracteres: gravada.texto.chars().count(),
        linhas: gravada.texto.lines().count(),
        notas: ids(raiz)?.len(),
        nota: gravada,
    })
}

/// Grava uma nota no arquivo, atômico.
fn gravar(arquivo: &Path, nota: &Nota) -> Result<()> {
    let corpo = serializar(nota);
    let temporario = arquivo.with_extension(format!("{EXTENSAO}.tmp"));
    std::fs::write(&temporario, corpo).with_context(|| {
        tr("base.erro.escreverTemporario", &[("caminho", &temporario.display().to_string())])
    })?;
    std::fs::rename(&temporario, arquivo)?;
    Ok(())
}

/// Apaga uma nota. Ausente o arquivo, o erro diz qual id não estava lá — é a
/// resposta que a tela precisa mostrar em vez de um "falhou" sem dono.
pub fn apagar(raiz: &Path, id: &str) -> Result<Removida> {
    conferir_id(id)?;
    let arquivo = caminho(raiz, id);
    let Ok(meta) = arquivo.metadata() else {
        bail!(tr("base.erro.notaAusente", &[("id", id)]));
    };
    let titulo = ler(raiz, id)?.map(|n| n.titulo).unwrap_or_else(|| id.to_string());
    std::fs::remove_file(&arquivo)
        .with_context(|| tr("base.erro.apagar", &[("caminho", &arquivo.display().to_string())]))?;
    Ok(Removida { id: id.to_string(), titulo, bytes: meta.len(), notas: ids(raiz)?.len() })
}

/// Fixa ou solta uma nota, riscando ou pondo o marcador no arquivo dela, com a
/// mesma escrita atômica de [`escrever`].
pub fn fixar(raiz: &Path, id: &str, fixada: bool) -> Result<Registro> {
    let Some(mut nota) = ler(raiz, id)? else {
        bail!(tr("base.erro.notaAusente", &[("id", id)]));
    };
    if nota.fixada != fixada {
        nota.fixada = fixada;
        gravar(&caminho(raiz, id), &nota).with_context(|| {
            tr("base.erro.escrever", &[("caminho", &caminho(raiz, id).display().to_string())])
        })?;
    }
    let arquivo = caminho(raiz, id);
    let meta = arquivo.metadata()?;
    let gravada = nota_de(&arquivo, &meta, &std::fs::read_to_string(&arquivo)?);
    Ok(Registro {
        bytes: meta.len(),
        caracteres: gravada.texto.chars().count(),
        linhas: gravada.texto.lines().count(),
        notas: ids(raiz)?.len(),
        nota: gravada,
    })
}

#[cfg(test)]
mod testes {
    use super::*;

    fn pasta_temporaria(nome: &str) -> PathBuf {
        let raiz = std::env::temp_dir().join(format!(
            "aether-base-{}-{}-{}",
            nome,
            env!("CARGO_PKG_VERSION"),
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&raiz);
        std::fs::create_dir_all(&raiz).expect("pasta temporaria");
        raiz
    }

    #[test]
    fn o_slug_e_minusculo_sem_acento_e_com_hifens() {
        assert_eq!(slug("Configuração de Rede"), "configuracao-de-rede");
        assert_eq!(slug("  AMÊndoas, Nozes!  "), "amendoas-nozes");
        assert_eq!(slug("RAG & RAG"), "rag-rag");
    }

    #[test]
    fn um_slug_vazio_nao_produz_arquivo_sem_nome() {
        assert_eq!(slug("!!!"), "nota");
        assert_eq!(slug(""), "nota");
    }

    #[test]
    fn a_colisao_de_titulo_ganha_sufixo_e_nao_por_nada_no_lugar() {
        let raiz = pasta_temporaria("colisao");
        let primeira = escrever(&raiz, "Reunião", "primeira", None, None).unwrap();
        let segunda = escrever(&raiz, "Reunião", "segunda", None, None).unwrap();
        assert_eq!(primeira.nota.id, "reuniao");
        assert_eq!(segunda.nota.id, "reuniao-2");
        assert_eq!(primeira.notas, 1);
        assert_eq!(segunda.notas, 2);
        // As duas continuam existindo, com o texto de cada uma.
        let lista = listar(&raiz).unwrap();
        assert_eq!(lista.notas, 2);
        let texto_da_primeira =
            lista.itens.iter().find(|n| n.id == "reuniao").unwrap().texto.clone();
        assert_eq!(texto_da_primeira, "primeira");
        let _ = std::fs::remove_dir_all(&raiz);
    }

    #[test]
    fn um_md_largado_a_mao_na_pasta_e_adoptado_como_nota() {
        let raiz = pasta_temporaria("adocao");
        std::fs::create_dir_all(&raiz).unwrap();
        std::fs::write(raiz.join("receita-de-bolo.md"), "# Receita de Bolo\nduas xícaras\n")
            .unwrap();
        // Sem linha de título nenhuma: o nome do arquivo é o título.
        std::fs::write(raiz.join("solto.txt"), "isto nem entra").unwrap();
        std::fs::write(raiz.join("sem-titulo.md"), "só corpo, sem cerquilha\n").unwrap();
        let lista = listar(&raiz).unwrap();
        assert_eq!(lista.notas, 2, "o .txt não é nota");
        let bolo = lista.itens.iter().find(|n| n.id == "receita-de-bolo").unwrap();
        assert_eq!(bolo.titulo, "Receita de Bolo");
        assert_eq!(bolo.texto, "duas xícaras");
        assert!(!bolo.fixada, "arquivo sem marcador entra livre");
        let sem = lista.itens.iter().find(|n| n.id == "sem-titulo").unwrap();
        assert_eq!(sem.titulo, "sem-titulo", "sem `#` o título é o nome do arquivo");
        assert_eq!(sem.texto, "só corpo, sem cerquilha");
        let _ = std::fs::remove_dir_all(&raiz);
    }

    #[test]
    fn uma_nota_de_corpo_vazio_continua_na_lista() {
        let raiz = pasta_temporaria("vazia");
        let registro = escrever(&raiz, "Decisão de amanhã", "", None, None).unwrap();
        assert_eq!(registro.caracteres, 0);
        assert_eq!(registro.linhas, 0);
        assert_eq!(registro.nota.para_indexar(), "Decisão de amanhã");
        let lista = listar(&raiz).unwrap();
        assert_eq!(lista.notas, 1);
        assert_eq!(lista.itens[0].titulo, "Decisão de amanhã");
        let _ = std::fs::remove_dir_all(&raiz);
    }

    #[test]
    fn a_escrita_devolve_os_bytes_e_as_linhas_que_o_disco_respondeu() {
        let raiz = pasta_temporaria("medida");
        let registro = escrever(&raiz, "Guia", "uma\n duas\ntres", None, None).unwrap();
        assert_eq!(registro.linhas, 3);
        assert_eq!(registro.caracteres, "uma\n duas\ntres".chars().count());
        let no_disco = std::fs::read(raiz.join("guia.md")).unwrap();
        assert_eq!(registro.bytes, no_disco.len() as u64);
        let _ = std::fs::remove_dir_all(&raiz);
    }

    #[test]
    fn apagar_um_id_que_nao_existe_diz_qual_e_o_id() {
        let raiz = pasta_temporaria("ausente");
        let erro = apagar(&raiz, "nota-que-nao-existe").unwrap_err().to_string();
        assert_eq!(erro, tr("base.erro.notaAusente", &[("id", "nota-que-nao-existe")]));
        let _ = std::fs::remove_dir_all(&raiz);
    }

    #[test]
    fn titulo_vazio_nao_cria_arquivo() {
        let raiz = pasta_temporaria("sem-titulo-escrita");
        let antes = std::fs::read_dir(&raiz).map(|d| d.count()).unwrap_or(0);
        let erro = escrever(&raiz, "   ", "corpo", None, None).unwrap_err().to_string();
        assert_eq!(erro, tr("base.erro.tituloVazio", &[]));
        assert_eq!(std::fs::read_dir(&raiz).map(|d| d.count()).unwrap_or(0), antes);
        let _ = std::fs::remove_dir_all(&raiz);
    }

    #[test]
    fn um_id_do_modelo_nao_vira_caminho_de_fora_da_pasta() {
        let raiz = pasta_temporaria("travessia");
        for id in ["../fugir", "..\\fugir", "a/b", "a\\b", "", "..", ".escondida"] {
            let erro = escrever(&raiz, "Título", "corpo", Some(id), None).unwrap_err().to_string();
            assert_eq!(erro, tr("base.erro.idInvalido", &[("id", id)]));
        }
        // Nada foi escrito em lugar nenhum: a pasta continua sem um arquivo só.
        assert_eq!(std::fs::read_dir(&raiz).map(|d| d.count()).unwrap_or(0), 0);
        let _ = std::fs::remove_dir_all(&raiz);
    }

    #[test]
    fn fixar_e_solta_mora_no_arquivo_e_nao_em_lugar_nenhum_outro() {
        let raiz = pasta_temporaria("fixar");
        let registro = escrever(&raiz, "Regra da casa", "não abrir o app", None, Some(true)).unwrap();
        assert!(registro.nota.fixada);
        let bruto = std::fs::read_to_string(raiz.join("regra-da-casa.md")).unwrap();
        assert!(bruto.contains(FIXADA), "o marcador tem de estar no arquivo");
        assert!(!registro.nota.texto.contains(FIXADA), "o marcador não é corpo");
        // Ninguém pode fixar duas notas por acidente: a pasta só tem o arquivo.
        assert_eq!(std::fs::read_dir(&raiz).unwrap().count(), 1);
        let solta = fixar(&raiz, "regra-da-casa", false).unwrap();
        assert!(!solta.nota.fixada);
        assert!(!std::fs::read_to_string(raiz.join("regra-da-casa.md")).unwrap().contains(FIXADA));
        let _ = std::fs::remove_dir_all(&raiz);
    }

    #[test]
    fn escrever_de_novo_com_o_id_mantem_a_nota_e_troca_o_titulo() {
        let raiz = pasta_temporaria("reescrever");
        let primeira = escrever(&raiz, "Plano A", "velho", None, None).unwrap();
        let segunda = escrever(&raiz, "Plano B", "novo", Some(&primeira.nota.id), None).unwrap();
        assert_eq!(segunda.nota.id, primeira.nota.id);
        assert_eq!(segunda.nota.titulo, "Plano B");
        assert_eq!(lista_ids(&raiz), vec!["plano-a".to_string()]);
        assert_eq!(segunda.notas, 1, "renomear o título não cria segunda nota");
        let _ = std::fs::remove_dir_all(&raiz);
    }

    #[test]
    fn escrever_sem_dizer_de_fixar_nao_mexe_na_decisao() {
        let raiz = pasta_temporaria("decisao");
        escrever(&raiz, "Nota", "um", None, Some(true)).unwrap();
        let segunda = escrever(&raiz, "Nota", "dois", Some("nota"), None).unwrap();
        assert!(segunda.nota.fixada, "a decisão do dono continua valendo");
        let terceira = escrever(&raiz, "Nota", "tres", Some("nota"), Some(false)).unwrap();
        assert!(!terceira.nota.fixada);
        let _ = std::fs::remove_dir_all(&raiz);
    }

    #[test]
    fn a_lista_ve_primeiro_a_nota_mais_mexida() {
        let raiz = pasta_temporaria("ordem");
        escrever(&raiz, "Velha", "1", None, None).unwrap();
        std::fs::write(raiz.join("velha.md"), "# Velha\n1\n").unwrap();
        // `fixar` reescreve o arquivo, então é a forma de mudar a data dele sem
        // depender do relógio da máquina.
        fixar(&raiz, "velha", true).unwrap();
        escrever(&raiz, "Nova", "2", None, None).unwrap();
        let lista = listar(&raiz).unwrap();
        assert_eq!(lista.fixadas, 1);
        assert!(
            lista.itens[0].atualizado >= lista.itens[1].atualizado,
            "a lista tem de sair pela data de mexida"
        );
        let _ = std::fs::remove_dir_all(&raiz);
    }

    #[test]
    fn uma_pasta_que_nao_existe_e_uma_base_vazia_e_nao_um_erro() {
        let raiz = std::env::temp_dir().join(format!(
            "aether-base-inexistente-{}-{}",
            env!("CARGO_PKG_VERSION"),
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&raiz);
        let lista = listar(&raiz).unwrap();
        assert_eq!(lista.notas, 0);
        assert!(lista.itens.is_empty());
        assert_eq!(ids(&raiz).unwrap(), Vec::<String>::new());
        let (notas, bytes, fixadas) = censo(&raiz).unwrap();
        assert_eq!((notas, bytes, fixadas), (0, 0, 0));
    }

    fn lista_ids(raiz: &Path) -> Vec<String> {
        ids(raiz).unwrap()
    }
}
