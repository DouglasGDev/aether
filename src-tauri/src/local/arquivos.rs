//! Ferramentas de arquivo: ler, listar, buscar, escrever, editar, criar, mover
//! e apagar. Todas restritas às pastas concedidas.

use super::pastas::Pastas;
use super::vista::{self, Achado, Entrada, Troca, Vista};
use super::{Saida, booleano, inteiro, obrigatorio, texto};
use crate::ai::catalog;
use crate::idiomas::tr;
use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::UNIX_EPOCH;

/// Teto do que uma leitura devolve. Acima dele o modelo precisa pedir um trecho:
/// um arquivo inteiro de megabytes não cabe na janela de contexto.
pub const LIMITE_LEITURA: u64 = 256 * 1024;
/// Teto do que o cartão mostra. O texto completo continua indo ao modelo.
const LIMITE_PREVIA: usize = 4000;
const LIMITE_ENTRADAS: usize = 400;
const LIMITE_ACHADOS: usize = 200;
const LIMITE_ARQUIVOS_BUSCA: usize = 5000;
const LIMITE_LINHAS: usize = 200_000;
const LIMITE_PROFUNDIDADE: usize = 16;

fn caminho(args: &Map<String, Value>, pastas: &Pastas) -> Result<PathBuf> {
    pastas.resolver(&obrigatorio(args, "caminho")?)
}

fn falha_leitura(caminho: &Path, erro: std::io::Error) -> anyhow::Error {
    if erro.kind() == std::io::ErrorKind::InvalidData {
        return anyhow!(tr(
            "erros.binario",
            &[("caminho", &caminho.display().to_string())]
        ));
    }
    anyhow!(tr(
        "erros.falhaLer",
        &[("caminho", &caminho.display().to_string()), ("detalhe", &erro.to_string())]
    ))
}

fn epoch_ms(tempo: std::time::SystemTime) -> Option<u64> {
    tempo.duration_since(UNIX_EPOCH).ok().map(|d| d.as_millis() as u64)
}

fn contar_linhas(texto: &str) -> usize {
    if texto.is_empty() { 0 } else { texto.lines().count() }
}

fn previa_de(texto: &str) -> String {
    texto.chars().take(LIMITE_PREVIA).collect()
}

/// Casa um padrão com `*` e `?` contra um nome. Sem expressões regulares: o
/// modelo escreve `*.rs`, não sintaxe de regex.
pub fn casa(padrao: &str, nome: &str) -> bool {
    let padrao: Vec<char> = padrao.chars().collect();
    let nome: Vec<char> = nome.chars().collect();
    let (mut p, mut n) = (0usize, 0usize);
    let (mut estrela, mut marca) = (usize::MAX, 0usize);
    while n < nome.len() {
        if p < padrao.len() && (padrao[p] == '?' || padrao[p].eq_ignore_ascii_case(&nome[n])) {
            p += 1;
            n += 1;
        } else if p < padrao.len() && padrao[p] == '*' {
            estrela = p;
            marca = n;
            p += 1;
        } else if estrela != usize::MAX {
            p = estrela + 1;
            marca += 1;
            n = marca;
        } else {
            return false;
        }
    }
    while p < padrao.len() && padrao[p] == '*' {
        p += 1;
    }
    p == padrao.len()
}

pub fn ler(args: &Map<String, Value>, pastas: &Pastas) -> Result<Saida> {
    let alvo = caminho(args, pastas)?;
    let meta = fs::metadata(&alvo)
        .map_err(|e| falha_leitura(&alvo, e))?;
    if !meta.is_file() {
        bail!(tr("erros.naoEArquivo", &[("caminho", &alvo.display().to_string())]));
    }
    let bytes = meta.len();
    let inicio = inteiro(args, "inicio").unwrap_or(1).max(1);
    let fim = inteiro(args, "fim").unwrap_or(usize::MAX);
    let com_faixa = args.contains_key("inicio") || args.contains_key("fim");
    if bytes > LIMITE_LEITURA && !com_faixa {
        bail!(tr(
            "erros.arquivoGrande",
            &[
                ("caminho", &alvo.display().to_string()),
                ("bytes", &bytes.to_string()),
                ("limite", &LIMITE_LEITURA.to_string()),
            ]
        ));
    }

    let mut arquivo = File::open(&alvo).map_err(|e| falha_leitura(&alvo, e))?;
    let mut amostra = [0u8; 8192];
    let lidos = arquivo.read(&mut amostra).map_err(|e| falha_leitura(&alvo, e))?;
    if amostra[..lidos].contains(&0) {
        bail!(tr("erros.binario", &[("caminho", &alvo.display().to_string())]));
    }
    arquivo.seek(SeekFrom::Start(0)).map_err(|e| falha_leitura(&alvo, e))?;

    let mut leitor = BufReader::new(arquivo);
    let mut linha = String::new();
    let mut numero = 0usize;
    let mut escolhidas: Vec<String> = Vec::new();
    let mut tamanho_escolhido = 0u64;
    let mut truncado = false;
    loop {
        linha.clear();
        let lidos = leitor.read_line(&mut linha).map_err(|e| falha_leitura(&alvo, e))?;
        if lidos == 0 {
            break;
        }
        numero += 1;
        if numero >= inicio && numero <= fim && tamanho_escolhido < LIMITE_LEITURA {
            let sem_quebra = linha.trim_end_matches(['\n', '\r']).to_string();
            tamanho_escolhido += sem_quebra.len() as u64 + 1;
            escolhidas.push(sem_quebra);
        }
        if numero >= LIMITE_LINHAS {
            truncado = true;
            break;
        }
    }
    if tamanho_escolhido >= LIMITE_LEITURA {
        truncado = true;
    }

    let conteudo = escolhidas.join("\n");
    let mut para_modelo = conteudo.clone();
    if truncado {
        para_modelo.push_str(&format!("\n… {}", tr("geral.truncado", &[])));
    }
    Ok(Saida::nova(
        para_modelo,
        Vista::Arquivo {
            caminho: alvo.display().to_string(),
            nome: vista::nome(&alvo),
            linhas: numero,
            bytes,
            previa: previa_de(&conteudo),
            linguagem: vista::linguagem(&alvo),
            inicio,
            truncado,
        },
    ))
}

pub fn listar(args: &Map<String, Value>, pastas: &Pastas) -> Result<Saida> {
    let alvo = caminho(args, pastas)?;
    if !alvo.is_dir() {
        bail!(tr("erros.naoEPasta", &[("caminho", &alvo.display().to_string())]));
    }
    let padrao = texto(args, "padrao");
    let leitura = fs::read_dir(&alvo).map_err(|e| {
        anyhow!(tr(
            "erros.falhaListar",
            &[("caminho", &alvo.display().to_string()), ("detalhe", &e.to_string())]
        ))
    })?;

    let mut entradas: Vec<Entrada> = Vec::new();
    let mut total = 0usize;
    let mut truncado = false;
    for item in leitura {
        let item = item.map_err(|e| {
            anyhow!(tr(
                "erros.falhaListar",
                &[("caminho", &alvo.display().to_string()), ("detalhe", &e.to_string())]
            ))
        })?;
        let nome = item.file_name().to_string_lossy().into_owned();
        if let Some(padrao) = &padrao && !casa(padrao, &nome) {
            continue;
        }
        total += 1;
        if entradas.len() >= LIMITE_ENTRADAS {
            truncado = true;
            continue;
        }
        let meta = item.metadata().ok();
        let pasta = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
        entradas.push(Entrada {
            nome,
            pasta,
            bytes: meta.as_ref().map(|m| m.len()).unwrap_or(0),
            modificado: meta.and_then(|m| m.modified().ok()).and_then(epoch_ms),
        });
    }
    entradas.sort_by(|a, b| {
        b.pasta.cmp(&a.pasta).then_with(|| a.nome.to_lowercase().cmp(&b.nome.to_lowercase()))
    });

    let mut para_modelo = String::new();
    for entrada in &entradas {
        para_modelo.push_str(if entrada.pasta { "[pasta] " } else { "" });
        para_modelo.push_str(&entrada.nome);
        if !entrada.pasta {
            para_modelo.push_str(&format!(" ({} bytes)", entrada.bytes));
        }
        para_modelo.push('\n');
    }
    if para_modelo.is_empty() {
        para_modelo = tr("cartao.pastaVazia", &[]);
    }
    if truncado {
        para_modelo.push_str(&format!("… {}", tr("geral.truncado", &[])));
    }
    Ok(Saida::nova(
        para_modelo,
        Vista::Lista { caminho: alvo.display().to_string(), entradas, total, truncado },
    ))
}

/// Percorre a árvore com teto de arquivos e de profundidade, sem recursão: uma
/// pasta profunda não pode estourar a pilha do aplicativo.
fn arvore(raiz: &Path, profundidade_maxima: usize) -> Vec<PathBuf> {
    let mut achados = Vec::new();
    let mut pilha: Vec<(PathBuf, usize)> = vec![(raiz.to_path_buf(), 0)];
    let mut visitados = 0usize;
    while let Some((pasta, profundidade)) = pilha.pop() {
        let Ok(leitura) = fs::read_dir(&pasta) else { continue };
        let mut itens: Vec<_> = leitura.filter_map(|e| e.ok()).collect();
        itens.sort_by_key(|e| e.file_name());
        for item in itens {
            let caminho = item.path();
            // No Windows `DirEntry::metadata` não segue o link: um symlink para
            // arquivo entraria como arquivo e a leitura depois seguiria o link
            // para fora da concessão. O tipo da entrada vê o link em si, então
            // a recusa acontece aqui, antes de qualquer leitura.
            let Ok(tipo) = item.file_type() else { continue };
            if tipo.is_symlink() {
                continue;
            }
            let Ok(meta) = item.metadata() else { continue };
            if meta.is_dir() {
                if profundidade + 1 < profundidade_maxima {
                    pilha.push((caminho, profundidade + 1));
                }
            } else {
                visitados += 1;
                if visitados > LIMITE_ARQUIVOS_BUSCA {
                    return achados;
                }
                achados.push(caminho);
            }
        }
    }
    achados
}

pub fn buscar(args: &Map<String, Value>, pastas: &Pastas) -> Result<Saida> {
    let termo = obrigatorio(args, "termo")?;
    let alvo = match texto(args, "caminho") {
        Some(pedido) => pastas.resolver(&pedido)?,
        None => super::raiz_de_trabalho(pastas)
            .ok_or_else(|| anyhow!(tr("erros.semPastas", &[])))?,
    };
    if !alvo.exists() {
        bail!(tr("erros.caminhoNaoExiste", &[("caminho", &alvo.display().to_string())]));
    }
    let recursivo = booleano(args, "recursivo", true);
    let needle = termo.to_lowercase();

    let arquivos = if alvo.is_file() {
        vec![alvo.clone()]
    } else if recursivo {
        arvore(&alvo, LIMITE_PROFUNDIDADE)
    } else {
        fs::read_dir(&alvo)
            .map(|leitura| {
                leitura
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    // `is_file` segue o link e aprovaria um symlink para fora;
                    // `symlink_metadata` vê o link em si, que não é arquivo.
                    .filter(|p| p.symlink_metadata().map(|m| m.is_file()).unwrap_or(false))
                    .collect()
            })
            .unwrap_or_default()
    };

    let mut achados: Vec<Achado> = Vec::new();
    let mut total = 0usize;
    let mut truncado = false;
    'arquivos: for arquivo in arquivos {
        // Re-resolver cada candidato fecha a janela entre listar e ler: um link
        // que apareceu nesse meio tempo é canonicalizado pelo resolver e, se
        // aponta para fora da concessão, recusado antes de qualquer leitura.
        let Ok(arquivo) = pastas.resolver(&arquivo.to_string_lossy()) else { continue };
        // O texto vem do cache assinado: uma busca sobre a mesma árvore com
        // outro termo volta a varrer os mesmos arquivos, e o que custava era a
        // leitura de cada um — não a comparação de linhas.
        let Some(texto) = texto_para_busca(&arquivo) else { continue };
        for (indice, linha) in texto.lines().enumerate() {
            if !linha.to_lowercase().contains(&needle) {
                continue;
            }
            total += 1;
            if achados.len() >= LIMITE_ACHADOS {
                truncado = true;
                break 'arquivos;
            }
            achados.push(Achado {
                caminho: arquivo.display().to_string(),
                nome: vista::nome(&arquivo),
                linha: indice + 1,
                texto: linha.trim().chars().take(300).collect(),
            });
        }
    }

    let mut para_modelo = String::new();
    for achado in &achados {
        para_modelo.push_str(&format!("{}:{}: {}\n", achado.caminho, achado.linha, achado.texto));
    }
    if para_modelo.is_empty() {
        para_modelo = tr("cartao.nadaEncontrado", &[]);
    }
    if truncado {
        para_modelo.push_str(&format!("… {}", tr("geral.truncado", &[])));
    }
    Ok(Saida::nova(
        para_modelo,
        Vista::Busca {
            termo,
            caminho: alvo.display().to_string(),
            achados,
            total,
            truncado,
        },
    ))
}

// ---------------------------------------------------------------------------
// Cache de texto assinado pelo disco
// ---------------------------------------------------------------------------

/// O texto de um arquivo para a busca, com a assinatura do momento em que foi
/// lido, e o custo que ele ocupa em RAM.
struct TextoGuardado {
    assinatura: catalog::Assinatura,
    /// `None` é "este arquivo não é texto": byte nulo no meio ou UTF-8
    /// inválido. É veredito de conteúdo, não falha de leitura, e por isso vale
    /// guardar — a próxima busca não precisa reler 200 KB para descobrir de
    /// novo que um `.png` não é texto.
    texto: Option<String>,
    carimbo: u64,
}

/// Arquivos residentes na busca.
///
/// O teto é de bytes, não de arquivos, porque é o que a máquina cobra: uma
/// entrada é no máximo `LIMITE_LEITURA` (256 KiB), então 8 MiB guardam de
/// verdade entre 32 arquivos grandes e centenas dos normais — o tamanho de uma
/// árvore de código que se revolve mais de uma vez numa conversa. Oito
/// megabytes é menos de um por cento do que um modelo de 3B pede em RAM;
/// passar disso é a busca começando a competir com a carga.
/// `TETO_DE_ARQUIVOS` (512) é o segundo cinto: muitos arquivos pequenos
/// também ocupam tabela, e a tabela tem de caber no número documentado.
/// Sai o menos recente; o preço de errar é reler um arquivo, não mentir.
const TETO_TOTAL_DE_BYTES: usize = 8 * 1024 * 1024;
const TETO_DE_ARQUIVOS: usize = 512;

static TEXTOS: OnceLock<Mutex<HashMap<PathBuf, TextoGuardado>>> = OnceLock::new();
static TEXTO_CARIMBO: AtomicU64 = AtomicU64::new(0);

fn textos() -> &'static Mutex<HashMap<PathBuf, TextoGuardado>> {
    TEXTOS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// O texto de um arquivo, pelo cache assinado.
///
/// `None` tem o mesmo significado que tinha no laço antigo: este arquivo não
/// entra na busca (não existe, não pode ser lido, é maior que o teto, não é
/// texto). O que não entra no cache é a falha de leitura — no Windows um
/// arquivo aberto por outro processo sem compartilhamento devolve erro agora e
/// devolve conteúdo depois, e gravar "não tem texto" nesse caso faria a busca
/// seguinte continuar cega para sempre.
///
/// A assinatura é a mesma do catálogo (tamanho + datas): trocado o arquivo, a
/// entrada cai sozinha. `descartar_texto` é o cinto nos pontos de escrita.
fn texto_para_busca(caminho: &Path) -> Option<String> {
    let assinatura = catalog::Assinatura::de_arquivo(caminho);
    if assinatura == catalog::Assinatura::Ausente {
        descartar_texto(caminho);
        return None;
    }
    let bytes = assinatura.bytes();
    if bytes > LIMITE_LEITURA {
        descartar_texto(caminho);
        return None;
    }
    {
        let mut cache = crate::state::trava_estado(textos());
        if let Some(entrada) = cache.get_mut(caminho) {
            if entrada.assinatura == assinatura {
                entrada.carimbo = TEXTO_CARIMBO.fetch_add(1, Ordering::Relaxed);
                return entrada.texto.clone();
            }
            cache.remove(caminho);
        }
    }
    registrar_leitura(caminho);
    let conteudo = match fs::read(caminho) {
        Ok(c) => c,
        Err(_) => return None,
    };
    let texto = if conteudo.contains(&0) {
        None
    } else {
        String::from_utf8(conteudo).ok()
    };
    {
        let mut cache = crate::state::trava_estado(textos());
        cache.insert(
            caminho.to_path_buf(),
            TextoGuardado { assinatura, texto: texto.clone(), carimbo: TEXTO_CARIMBO.fetch_add(1, Ordering::Relaxed) },
        );
        while cache.len() > TETO_DE_ARQUIVOS
            || cache
                .values()
                .map(|e| e.texto.as_ref().map(|t| t.len()).unwrap_or(0))
                .sum::<usize>()
                > TETO_TOTAL_DE_BYTES
        {
            let mais_velha =
                cache.iter().min_by_key(|(_, e)| e.carimbo).map(|(k, _)| k.clone());
            match mais_velha {
                Some(chave) => {
                    cache.remove(&chave);
                }
                None => break,
            }
        }
    }
    texto
}

/// Esquece o texto de um arquivo. Chamada em toda escrita desta módulo: o
/// renomear apaga a origem e publica o destino, o apagar tira o alvo e a
/// árvore inteira dele.
fn descartar_texto(caminho: &Path) {
    crate::state::trava_estado(textos()).remove(caminho);
}

/// Esquece tudo que mora sob uma pasta: é o que um `mover` ou um `apagar` de
/// árvore muda de uma vez, sem percorrer o cache arquivo por arquivo.
fn descartar_textos_sob(pasta: &Path) {
    let mut cache = crate::state::trava_estado(textos());
    cache.retain(|caminho, _| !caminho.starts_with(pasta));
}

/// Contador de leituras de disco por caminho, só em teste: prova que a segunda
/// busca não voltou ao arquivo. Global não serviria — os testes rodam em
/// threads do mesmo processo.
#[cfg(test)]
mod medidor {
    use crate::state::trava_estado;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, OnceLock};

    static LEITURAS: OnceLock<Mutex<HashMap<PathBuf, u64>>> = OnceLock::new();

    fn registro() -> &'static Mutex<HashMap<PathBuf, u64>> {
        LEITURAS.get_or_init(|| Mutex::new(HashMap::new()))
    }

    pub fn marcar(caminho: &Path) {
        *trava_estado(registro()).entry(caminho.to_path_buf()).or_insert(0) += 1;
    }

    pub fn lidas(caminho: &Path) -> u64 {
        trava_estado(registro()).get(caminho).copied().unwrap_or(0)
    }
}

#[cfg(test)]
#[inline]
fn registrar_leitura(caminho: &Path) {
    medidor::marcar(caminho);
}

#[cfg(not(test))]
#[inline]
fn registrar_leitura(_caminho: &Path) {}

#[cfg(test)]
pub fn leituras_de(caminho: &Path) -> u64 {
    medidor::lidas(caminho)
}

#[cfg(test)]
pub fn textos_em_cache() -> usize {
    crate::state::trava_estado(textos()).len()
}

/// Uma escrita pode cair ao lado de outra: o chat e um cliente MCP loopback
/// chamam a mesma ferramenta. É o que separa os temporários delas.
static ESCRITAS: AtomicU64 = AtomicU64::new(0);

/// Nome do temporário que vira o alvo: o nome inteiro do alvo, para um
/// `aether-tmp` perdido na pasta dizer de que arquivo ele era, mais um sufixo
/// único por escrita. Sem o sufixo, `LEIA-ME.md` e `LEIA-ME.txt` caem no mesmo
/// nome, e a escrita que renomeia primeiro publica o corpo da outra.
fn temporario_de(alvo: &Path) -> PathBuf {
    let nome = alvo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let vez = ESCRITAS.fetch_add(1, Ordering::Relaxed);
    alvo.with_file_name(format!("{nome}.aether-tmp.{}.{}", std::process::id(), vez))
}

/// Escreve por arquivo temporário e renomeia: uma queda no meio não deixa o
/// arquivo original pela metade.
fn escrever_atomico(alvo: &Path, conteudo: &str) -> Result<()> {
    if let Some(pai) = alvo.parent() {
        fs::create_dir_all(pai).map_err(|e| {
            anyhow!(tr(
                "erros.falhaCriarPasta",
                &[("caminho", &pai.display().to_string()), ("detalhe", &e.to_string())]
            ))
        })?;
    }
    let temporario = temporario_de(alvo);
    let resultado = (|| -> std::io::Result<()> {
        let mut arquivo = OpenOptions::new().write(true).create(true).truncate(true).open(&temporario)?;
        arquivo.write_all(conteudo.as_bytes())?;
        arquivo.flush()?;
        Ok(())
    })();
    if let Err(e) = resultado {
        fs::remove_file(&temporario).ok();
        return Err(anyhow!(tr(
            "erros.falhaEscrever",
            &[("caminho", &alvo.display().to_string()), ("detalhe", &e.to_string())]
        )));
    }
    fs::rename(&temporario, alvo).map_err(|e| {
        fs::remove_file(&temporario).ok();
        anyhow!(tr(
            "erros.falhaEscrever",
            &[("caminho", &alvo.display().to_string()), ("detalhe", &e.to_string())]
        ))
    })?;
    // O rename publicou um conteúdo novo neste caminho — é aqui, e não em quem
    // chama, que a busca tem de esquecer o texto velho. Um `editar` que troca
    // uma palavra por outra de mesmo tamanho deixa o arquivo do mesmo tamanho, e
    // no Windows a data pode cair no mesmo tique: a assinatura sozinha não jura.
    descartar_texto(alvo);
    Ok(())
}

pub fn escrever(args: &Map<String, Value>, pastas: &Pastas) -> Result<Saida> {
    let alvo = caminho(args, pastas)?;
    // O corpo é obrigatório no esquema, e é obrigatório aqui pelo mesmo motivo:
    // uma chamada cortada no teto de tokens chega só com o `caminho`, e um
    // `String::new()` nesse ponto criaria um arquivo vazio dizendo "Criado".
    // Esvaziar um arquivo é legítimo, mas quem decide é quem pede — o modelo
    // escreve `conteudo: ""`, não omite o argumento.
    let conteudo = match args.get("conteudo") {
        Some(Value::String(texto)) => texto.clone(),
        _ => bail!(tr("erros.argumentoFaltando", &[("nome", "conteudo")])),
    };
    if alvo.exists() && !alvo.is_file() {
        bail!(tr("erros.naoEArquivo", &[("caminho", &alvo.display().to_string())]));
    }
    let criado = !alvo.exists();
    escrever_atomico(&alvo, &conteudo)?;
    let bytes = fs::metadata(&alvo).map(|m| m.len()).unwrap_or(conteudo.len() as u64);
    let linhas = contar_linhas(&conteudo);
    let resumo = format!(
        "{}: {} ({} bytes, {} linhas)",
        if criado { tr("cartao.criado", &[]) } else { tr("cartao.atualizado", &[]) },
        alvo.display(),
        bytes,
        linhas
    );
    Ok(Saida::nova(
        resumo,
        Vista::Escrita {
            caminho: alvo.display().to_string(),
            nome: vista::nome(&alvo),
            bytes,
            linhas,
            criado,
        },
    ))
}

pub fn editar(args: &Map<String, Value>, pastas: &Pastas) -> Result<Saida> {
    let alvo = caminho(args, pastas)?;
    if !alvo.is_file() {
        bail!(tr("erros.naoEArquivo", &[("caminho", &alvo.display().to_string())]));
    }
    let trocas = match args.get("trocas") {
        Some(Value::Array(lista)) => lista.clone(),
        _ => bail!(tr("erros.argumentoFaltando", &[("nome", "trocas")])),
    };
    if trocas.is_empty() {
        bail!(tr("erros.argumentoFaltando", &[("nome", "trocas")]));
    }
    let original = fs::read_to_string(&alvo).map_err(|e| falha_leitura(&alvo, e))?;

    // Tudo ou nada: o arquivo só é tocado depois que todas as trocas casaram.
    let mut atual = original.clone();
    let mut aplicadas: Vec<Troca> = Vec::new();
    for troca in &trocas {
        let de = troca["de"].as_str().unwrap_or("");
        let para = troca["para"].as_str().unwrap_or("");
        if de.is_empty() {
            bail!(tr("erros.argumentoFaltando", &[("nome", "de")]));
        }
        let vezes = atual.match_indices(de).count();
        if vezes == 0 {
            bail!(tr(
                "erros.trocaNaoEncontrada",
                &[
                    ("caminho", &alvo.display().to_string()),
                    ("trecho", &de.chars().take(120).collect::<String>()),
                ]
            ));
        }
        if vezes > 1 {
            bail!(tr(
                "erros.trocaAmbigua",
                &[("caminho", &alvo.display().to_string()), ("n", &vezes.to_string())]
            ));
        }
        let posicao = atual.find(de).expect("acabei de contar uma ocorrência");
        let linha = atual[..posicao].lines().count().max(1);
        atual = atual.replacen(de, para, 1);
        aplicadas.push(Troca {
            antes: de.chars().take(600).collect(),
            depois: para.chars().take(600).collect(),
            linha,
        });
    }
    escrever_atomico(&alvo, &atual)?;
    let resumo = format!(
        "{}: {} ({} trocas)",
        tr("cartao.edicao", &[]),
        alvo.display(),
        aplicadas.len()
    );
    Ok(Saida::nova(
        resumo,
        Vista::Edicao {
            caminho: alvo.display().to_string(),
            nome: vista::nome(&alvo),
            aplicadas: aplicadas.len(),
            trocas: aplicadas,
        },
    ))
}

pub fn criar_pasta(args: &Map<String, Value>, pastas: &Pastas) -> Result<Saida> {
    let alvo = caminho(args, pastas)?;
    fs::create_dir_all(&alvo).map_err(|e| {
        anyhow!(tr(
            "erros.falhaCriarPasta",
            &[("caminho", &alvo.display().to_string()), ("detalhe", &e.to_string())]
        ))
    })?;
    Ok(Saida::nova(
        format!("{}: {}", tr("cartao.pasta", &[]), alvo.display()),
        Vista::Pasta { caminho: alvo.display().to_string(), nome: vista::nome(&alvo) },
    ))
}

pub fn mover(args: &Map<String, Value>, pastas: &Pastas) -> Result<Saida> {
    let origem = pastas.resolver(&obrigatorio(args, "origem")?)?;
    // Renomear a raiz concedida revogaria por dentro a permissão que o usuário
    // deu — o mesmo motivo de `apagar` recusar: o gate autorizou o conteúdo da
    // pasta, não a concessão em si. A recusa vem antes do destino, senão um
    // destino fora da concessão mascararia o verdadeiro motivo.
    if pastas.raizes.contains(&origem) {
        bail!(tr(
            "erros.raizProtegida",
            &[("caminho", &origem.display().to_string())]
        ));
    }
    let destino = pastas.resolver(&obrigatorio(args, "destino")?)?;
    if !origem.exists() {
        bail!(tr("erros.caminhoNaoExiste", &[("caminho", &origem.display().to_string())]));
    }
    if destino.exists() {
        bail!(tr("erros.destinoExiste", &[("caminho", &destino.display().to_string())]));
    }
    if let Some(pai) = destino.parent() {
        fs::create_dir_all(pai).ok();
    }
    fs::rename(&origem, &destino).map_err(|e| {
        anyhow!(tr(
            "erros.falhaMover",
            &[("caminho", &origem.display().to_string()), ("detalhe", &e.to_string())]
        ))
    })?;
    // O caminho de origem passou a não existir e o de destino passou a existir
    // com o conteúdo dela: os dois lados são descartados por prefixo porque um
    // `mover` de pasta leva a árvore inteira de arquivos.
    descartar_textos_sob(&origem);
    descartar_textos_sob(&destino);
    Ok(Saida::nova(
        format!("{} → {}", origem.display(), destino.display()),
        Vista::Movido {
            origem: origem.display().to_string(),
            destino: destino.display().to_string(),
            nome: vista::nome(&destino),
        },
    ))
}

pub fn apagar(args: &Map<String, Value>, pastas: &Pastas) -> Result<Saida> {
    let alvo = caminho(args, pastas)?;
    // Uma raiz concedida nunca é apagada por dentro: isso revogaria a permissão
    // que o usuário deu, e o gate não foi feito para autorizar isso.
    if pastas.raizes.contains(&alvo) {
        bail!(tr(
            "erros.raizProtegida",
            &[("caminho", &alvo.display().to_string())]
        ));
    }
    if !alvo.exists() {
        bail!(tr("erros.caminhoNaoExiste", &[("caminho", &alvo.display().to_string())]));
    }
    let pasta = alvo.is_dir();
    let resultado = if pasta { fs::remove_dir_all(&alvo) } else { fs::remove_file(&alvo) };
    resultado.map_err(|e| {
        anyhow!(tr(
            "erros.falhaApagar",
            &[("caminho", &alvo.display().to_string()), ("detalhe", &e.to_string())]
        ))
    })?;
    // Apagada a árvore, o texto que ela continha tem de sair do cache junto:
    // sem isto a busca seguinte ainda acharia linha de arquivo que não existe.
    descartar_textos_sob(&alvo);
    Ok(Saida::nova(
        format!("{}: {}", tr("cartao.apagado", &[]), alvo.display()),
        Vista::Apagado {
            caminho: alvo.display().to_string(),
            nome: vista::nome(&alvo),
            pasta,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cenario(nome: &str) -> (PathBuf, Pastas) {
        let raiz = std::env::temp_dir().join(format!("aether-arq-{nome}-{}", std::process::id()));
        fs::create_dir_all(&raiz).unwrap();
        let raiz = fs::canonicalize(&raiz).unwrap();
        let mut pastas = Pastas::default();
        pastas.conceder(&raiz.to_string_lossy()).unwrap();
        (raiz, pastas)
    }

    fn args(valor: Value) -> Map<String, Value> {
        valor.as_object().expect("objeto").clone()
    }

    fn limpar(raiz: &Path) {
        fs::remove_dir_all(raiz).ok();
    }

    #[test]
    fn ler_devolve_o_conteudo_real_e_a_contagem_de_linhas() {
        let (raiz, pastas) = cenario("ler");
        fs::write(raiz.join("nota.txt"), "primeira\nsegunda\nterceira\n").unwrap();
        let saida = ler(&args(json!({"caminho": raiz.join("nota.txt")})), &pastas).unwrap();
        assert_eq!(saida.texto, "primeira\nsegunda\nterceira");
        let Vista::Arquivo { linhas, bytes, previa, .. } = saida.vista else {
            panic!("vista errada");
        };
        assert_eq!(linhas, 3);
        assert_eq!(bytes, 26);
        assert_eq!(previa, "primeira\nsegunda\nterceira");
        limpar(&raiz);
    }

    #[test]
    fn ler_um_trecho_respeita_inicio_e_fim() {
        let (raiz, pastas) = cenario("trecho");
        fs::write(raiz.join("nota.txt"), "a\nb\nc\nd\n").unwrap();
        let saida =
            ler(&args(json!({"caminho": raiz.join("nota.txt"), "inicio": 2, "fim": 3})), &pastas)
                .unwrap();
        assert_eq!(saida.texto, "b\nc");
        let Vista::Arquivo { linhas, inicio, .. } = saida.vista else { panic!("vista errada") };
        assert_eq!(linhas, 4);
        assert_eq!(inicio, 2);
        limpar(&raiz);
    }

    #[test]
    fn arquivo_grande_demais_recusa_em_vez_de_estourar_o_contexto() {
        let (raiz, pastas) = cenario("grande");
        let alvo = raiz.join("grande.txt");
        let linha = "x".repeat(1000);
        let grandes = vec![linha.as_str(); (LIMITE_LEITURA as usize / 1000) + 10].join("\n");
        fs::write(&alvo, &grandes).unwrap();
        let erro = ler(&args(json!({"caminho": alvo})), &pastas).unwrap_err().to_string();
        assert!(erro.contains("acima do limite"), "{erro}");
        // Com faixa, o mesmo arquivo é lido sem carregar tudo.
        let saida = ler(&args(json!({"caminho": alvo, "inicio": 2, "fim": 3})), &pastas).unwrap();
        assert_eq!(saida.texto, format!("{linha}\n{linha}"));
        let Vista::Arquivo { linhas, .. } = saida.vista else { panic!("vista errada") };
        assert_eq!(linhas, (LIMITE_LEITURA as usize / 1000) + 10);
        limpar(&raiz);
    }

    #[test]
    fn arquivo_binario_e_recusado_como_binario() {
        let (raiz, pastas) = cenario("binario");
        let alvo = raiz.join("dados.bin");
        fs::write(&alvo, [0u8, 1, 2, 3, 0, 5]).unwrap();
        let erro = ler(&args(json!({"caminho": alvo})), &pastas).unwrap_err().to_string();
        assert!(erro.contains("não é um arquivo de texto"), "{erro}");
        limpar(&raiz);
    }

    #[test]
    fn ler_pasta_em_vez_de_arquivo_e_recusado() {
        let (raiz, pastas) = cenario("ler-pasta");
        let erro = ler(&args(json!({"caminho": raiz})), &pastas).unwrap_err().to_string();
        assert!(erro.contains("não é um arquivo"), "{erro}");
        limpar(&raiz);
    }

    #[test]
    fn listar_mostra_pastas_primeiro_e_o_tamanho_real() {
        let (raiz, pastas) = cenario("listar");
        fs::create_dir_all(raiz.join("zeta")).unwrap();
        fs::write(raiz.join("alfa.txt"), "12345").unwrap();
        let saida = listar(&args(json!({"caminho": raiz})), &pastas).unwrap();
        let Vista::Lista { entradas, total, .. } = saida.vista else { panic!("vista errada") };
        assert_eq!(total, 2);
        assert_eq!(entradas[0].nome, "zeta");
        assert!(entradas[0].pasta);
        assert_eq!(entradas[1].nome, "alfa.txt");
        assert_eq!(entradas[1].bytes, 5);
        assert!(entradas[1].modificado.is_some());
        limpar(&raiz);
    }

    #[test]
    fn listar_filtra_pelo_padrao_pedido() {
        let (raiz, pastas) = cenario("padrao");
        fs::write(raiz.join("um.rs"), "x").unwrap();
        fs::write(raiz.join("dois.txt"), "x").unwrap();
        let saida = listar(&args(json!({"caminho": raiz, "padrao": "*.rs"})), &pastas).unwrap();
        let Vista::Lista { entradas, total, .. } = saida.vista else { panic!("vista errada") };
        assert_eq!(total, 1);
        assert_eq!(entradas[0].nome, "um.rs");
        limpar(&raiz);
    }

    #[test]
    fn pasta_vazia_e_dita_como_vazia() {
        let (raiz, pastas) = cenario("vazia");
        let saida = listar(&args(json!({"caminho": raiz})), &pastas).unwrap();
        assert_eq!(saida.texto, tr("cartao.pastaVazia", &[]));
        limpar(&raiz);
    }

    #[test]
    fn buscar_acha_o_termo_com_linha_e_sem_diferenciar_caixa() {
        let (raiz, pastas) = cenario("buscar");
        fs::create_dir_all(raiz.join("sub")).unwrap();
        fs::write(raiz.join("sub").join("codigo.rs"), "fn main() {}\n// ACHADO aqui\n").unwrap();
        fs::write(raiz.join("outro.txt"), "nada\n").unwrap();
        let saida = buscar(&args(json!({"termo": "achado"})), &pastas).unwrap();
        let Vista::Busca { achados, total, .. } = saida.vista else { panic!("vista errada") };
        assert_eq!(total, 1);
        assert_eq!(achados[0].nome, "codigo.rs");
        assert_eq!(achados[0].linha, 2);
        assert!(achados[0].texto.contains("ACHADO"));
        limpar(&raiz);
    }

    #[test]
    fn buscar_sem_recursao_nao_desce_nas_subpastas() {
        let (raiz, pastas) = cenario("buscar-raso");
        fs::create_dir_all(raiz.join("sub")).unwrap();
        fs::write(raiz.join("sub").join("fundo.txt"), "alvo\n").unwrap();
        fs::write(raiz.join("raso.txt"), "alvo\n").unwrap();
        let saida = buscar(&args(json!({"termo": "alvo", "recursivo": false})), &pastas).unwrap();
        let Vista::Busca { total, .. } = saida.vista else { panic!("vista errada") };
        assert_eq!(total, 1);
        limpar(&raiz);
    }

    #[test]
    fn buscar_termo_inexistente_diz_que_nao_achou() {
        let (raiz, pastas) = cenario("buscar-nada");
        fs::write(raiz.join("a.txt"), "conteúdo\n").unwrap();
        let saida = buscar(&args(json!({"termo": "zzzz"})), &pastas).unwrap();
        assert_eq!(saida.texto, tr("cartao.nadaEncontrado", &[]));
        limpar(&raiz);
    }

    #[test]
    fn escrever_cria_o_arquivo_com_o_conteudo_pedido() {
        let (raiz, pastas) = cenario("escrever");
        let alvo = raiz.join("novo").join("codigo.rs");
        let saida = escrever(
            &args(json!({"caminho": alvo, "conteudo": "fn main() {}\n"})),
            &pastas,
        )
        .unwrap();
        assert_eq!(fs::read_to_string(&alvo).unwrap(), "fn main() {}\n");
        let Vista::Escrita { criado, linhas, bytes, .. } = saida.vista else {
            panic!("vista errada");
        };
        assert!(criado);
        assert_eq!(linhas, 1);
        assert_eq!(bytes, 13);
        let sobras: Vec<String> = fs::read_dir(alvo.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|nome| nome.contains("aether-tmp"))
            .collect();
        assert!(sobras.is_empty(), "a escrita deixou {sobras:?} na pasta");
        limpar(&raiz);
    }

    /// Duas escritas na mesma pasta não podem disputar um mesmo temporário: se
    /// `LEIA-ME.md` e `LEIA-ME.txt` caem no mesmo nome, quem renomeia primeiro
    /// publica o corpo do outro.
    #[test]
    fn dois_alvos_com_o_mesmo_tronco_tem_temporarios_diferentes() {
        let um = temporario_de(Path::new("p/LEIA-ME.md"));
        let dois = temporario_de(Path::new("p/LEIA-ME.txt"));
        assert_ne!(um, dois, "{} e {} escreveriam no mesmo arquivo", um.display(), dois.display());
        let tres = temporario_de(Path::new("p/LEIA-ME.md"));
        assert_ne!(um, tres, "duas escritas do mesmo arquivo não podem se atropelar");
    }

    /// O temporário é um arquivo ao lado do alvo, não uma mudança de extensão:
    /// um alvo sem extensão (`Makefile`) continua tendo onde escrever.
    #[test]
    fn um_alvo_sem_extensao_tem_temporario_ao_lado() {
        let alvo = Path::new("p/Makefile");
        let temporario = temporario_de(alvo);
        assert_eq!(temporario.parent(), alvo.parent());
        assert!(
            temporario
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("Makefile"),
            "{}",
            temporario.display()
        );
    }

    #[test]
    fn escrever_sobre_um_existente_substitui_e_nao_diz_que_criou() {
        let (raiz, pastas) = cenario("sobrescrever");
        let alvo = raiz.join("a.txt");
        fs::write(&alvo, "velho").unwrap();
        let saida = escrever(&args(json!({"caminho": alvo, "conteudo": "novo"})), &pastas).unwrap();
        let Vista::Escrita { criado, .. } = saida.vista else { panic!("vista errada") };
        assert!(!criado);
        assert_eq!(fs::read_to_string(&alvo).unwrap(), "novo");
        limpar(&raiz);
    }

    /// A chamada cortada no teto de tokens chega só com o caminho. Sem o corpo
    /// não há arquivo a criar: o que estava acontecendo era um zero bytes
    /// anunciado como "Criado", e o modelo sem nada para corrigir.
    #[test]
    fn escrever_sem_corpo_nao_toca_o_disco() {
        let (raiz, pastas) = cenario("escrever-sem-corpo");
        let alvo = raiz.join("pagina.html");
        let erro = escrever(&args(json!({"caminho": alvo})), &pastas)
            .err()
            .expect("sem conteúdo a ferramenta tem de recusar");
        assert!(erro.to_string().contains("conteudo"), "{erro}");
        assert!(!alvo.exists(), "recusar não é criar arquivo vazio");
        limpar(&raiz);
    }

    /// Esvaziar é um pedido legítimo, e ele vem dito: `conteudo` presente e
    /// vazio. O que a recusa protege é a ausência, não a string vazia.
    #[test]
    fn conteudo_vazio_explicito_e_permitido() {
        let (raiz, pastas) = cenario("escrever-vazio");
        let alvo = raiz.join("a.txt");
        fs::write(&alvo, "velho").unwrap();
        escrever(&args(json!({"caminho": alvo, "conteudo": ""})), &pastas).unwrap();
        assert_eq!(fs::read_to_string(&alvo).unwrap(), "");
        limpar(&raiz);
    }

    #[test]
    fn editar_troca_o_trecho_exato_e_diz_a_linha() {
        let (raiz, pastas) = cenario("editar");
        let alvo = raiz.join("codigo.rs");
        fs::write(&alvo, "fn main() {\n    println!(\"oi\");\n}\n").unwrap();
        let saida = editar(
            &args(json!({
                "caminho": alvo,
                "trocas": [{"de": "println!(\"oi\")", "para": "println!(\"olá\")"}]
            })),
            &pastas,
        )
        .unwrap();
        assert!(fs::read_to_string(&alvo).unwrap().contains("println!(\"olá\")"));
        let Vista::Edicao { trocas, aplicadas, .. } = saida.vista else { panic!("vista errada") };
        assert_eq!(aplicadas, 1);
        assert_eq!(trocas[0].linha, 2);
        assert_eq!(trocas[0].antes, "println!(\"oi\")");
        limpar(&raiz);
    }

    #[test]
    fn editar_trecho_ausente_nao_toca_no_arquivo() {
        let (raiz, pastas) = cenario("editar-ausente");
        let alvo = raiz.join("a.txt");
        fs::write(&alvo, "conteúdo original").unwrap();
        let erro = editar(
            &args(json!({"caminho": alvo, "trocas": [{"de": "não existe", "para": "x"}]})),
            &pastas,
        )
        .unwrap_err()
        .to_string();
        assert!(erro.contains("não encontrei"), "{erro}");
        assert_eq!(fs::read_to_string(&alvo).unwrap(), "conteúdo original");
        limpar(&raiz);
    }

    #[test]
    fn editar_trecho_ambiguo_recusa_em_vez_de_escolher_um() {
        let (raiz, pastas) = cenario("editar-ambiguo");
        let alvo = raiz.join("a.txt");
        fs::write(&alvo, "x\nx\n").unwrap();
        let erro = editar(
            &args(json!({"caminho": alvo, "trocas": [{"de": "x", "para": "y"}]})),
            &pastas,
        )
        .unwrap_err()
        .to_string();
        assert!(erro.contains("2 vezes"), "{erro}");
        assert_eq!(fs::read_to_string(&alvo).unwrap(), "x\nx\n");
        limpar(&raiz);
    }

    #[test]
    fn editar_duas_trocas_so_escreve_se_as_duas_casarem() {
        let (raiz, pastas) = cenario("editar-tudo-ou-nada");
        let alvo = raiz.join("a.txt");
        fs::write(&alvo, "um\ndois\n").unwrap();
        let erro = editar(
            &args(json!({
                "caminho": alvo,
                "trocas": [{"de": "um", "para": "UM"}, {"de": "tres", "para": "TRES"}]
            })),
            &pastas,
        )
        .unwrap_err();
        assert!(!erro.to_string().is_empty());
        assert_eq!(fs::read_to_string(&alvo).unwrap(), "um\ndois\n");
        limpar(&raiz);
    }

    #[test]
    fn criar_pasta_cria_as_intermediarias() {
        let (raiz, pastas) = cenario("criar");
        let alvo = raiz.join("a").join("b").join("c");
        criar_pasta(&args(json!({"caminho": alvo})), &pastas).unwrap();
        assert!(alvo.is_dir());
        limpar(&raiz);
    }

    #[test]
    fn mover_renomeia_e_preserva_o_conteudo() {
        let (raiz, pastas) = cenario("mover");
        let origem = raiz.join("a.txt");
        let destino = raiz.join("b.txt");
        fs::write(&origem, "conteúdo").unwrap();
        mover(&args(json!({"origem": origem, "destino": destino})), &pastas).unwrap();
        assert!(!origem.exists());
        assert_eq!(fs::read_to_string(&destino).unwrap(), "conteúdo");
        limpar(&raiz);
    }

    #[test]
    fn mover_sobre_algo_existente_e_recusado() {
        let (raiz, pastas) = cenario("mover-existe");
        fs::write(raiz.join("a.txt"), "a").unwrap();
        fs::write(raiz.join("b.txt"), "b").unwrap();
        assert!(
            mover(&args(json!({"origem": raiz.join("a.txt"), "destino": raiz.join("b.txt")})), &pastas)
                .is_err()
        );
        assert_eq!(fs::read_to_string(raiz.join("b.txt")).unwrap(), "b");
        limpar(&raiz);
    }

    #[test]
    fn apagar_arquivo_remove_so_ele() {
        let (raiz, pastas) = cenario("apagar");
        let alvo = raiz.join("a.txt");
        fs::write(&alvo, "x").unwrap();
        fs::write(raiz.join("b.txt"), "y").unwrap();
        let saida = apagar(&args(json!({"caminho": alvo})), &pastas).unwrap();
        let Vista::Apagado { pasta, .. } = saida.vista else { panic!("vista errada") };
        assert!(!pasta);
        assert!(!alvo.exists());
        assert!(raiz.join("b.txt").exists());
        limpar(&raiz);
    }

    #[test]
    fn apagar_pasta_remove_a_arvore_inteira() {
        let (raiz, pastas) = cenario("apagar-pasta");
        let alvo = raiz.join("arvore");
        fs::create_dir_all(alvo.join("sub")).unwrap();
        fs::write(alvo.join("sub").join("a.txt"), "x").unwrap();
        let saida = apagar(&args(json!({"caminho": alvo})), &pastas).unwrap();
        let Vista::Apagado { pasta, .. } = saida.vista else { panic!("vista errada") };
        assert!(pasta);
        assert!(!alvo.exists());
        limpar(&raiz);
    }

    #[test]
    fn apagar_a_propria_raiz_concedida_e_recusado() {
        let (raiz, pastas) = cenario("apagar-raiz");
        assert!(apagar(&args(json!({"caminho": raiz})), &pastas).is_err());
        assert!(raiz.exists());
        limpar(&raiz);
    }

    #[test]
    fn nenhuma_ferramenta_escapa_da_pasta_concedida() {
        let (raiz, pastas) = cenario("contencao");
        let fora = raiz.parent().unwrap().join("aether-fora.txt");
        for pedido in [
            json!({"caminho": fora}),
            json!({"caminho": raiz.join("..").join("aether-fora.txt")}),
        ] {
            assert!(escrever(&args(pedido.clone()), &pastas).is_err());
            assert!(apagar(&args(pedido), &pastas).is_err());
        }
        assert!(!fora.exists());
        limpar(&raiz);
    }

    /// Link simbólico de teste. Sem privilégio (comum no Windows fora do modo
    /// desenvolvedor) devolve false: o teste então prova a contenção pelo
    /// resolver, que é a mesma regra que barra o alvo do link.
    fn criar_link(alvo: &Path, link: &Path, pasta: bool) -> bool {
        #[cfg(windows)]
        {
            if pasta {
                std::os::windows::fs::symlink_dir(alvo, link).is_ok()
            } else {
                std::os::windows::fs::symlink_file(alvo, link).is_ok()
            }
        }
        #[cfg(unix)]
        {
            let _ = pasta;
            std::os::unix::fs::symlink(alvo, link).is_ok()
        }
        #[cfg(not(any(windows, unix)))]
        {
            let _ = (alvo, link, pasta);
            false
        }
    }

    /// `buscar` é ReadOnly e roda sem aprovação no Plan: um link dentro da
    /// pasta concedida apontando para fora não pode virar leitura do alvo,
    /// ou a concessão de uma pasta vira acesso a qualquer arquivo do disco.
    #[test]
    fn buscar_nao_le_link_simbolico_que_aponta_para_fora() {
        let (_, pastas) = cenario("buscar-link");
        // O link é encenado sobre a raiz concedida, que é o caminho real sem o
        // prefixo verbatim do Windows: criar symlink com `\\?\` no alvo é
        // terreno incerto, e a concessão é exatamente esta pasta.
        let raiz = pastas.raizes[0].clone();
        let fora = raiz
            .parent()
            .unwrap()
            .join(format!("aether-arq-link-fora-{}", std::process::id()));
        fs::create_dir_all(&fora).unwrap();
        let segredo = fora.join("segredo.txt");
        fs::write(&segredo, "SEGREDOFORA na primeira linha\n").unwrap();
        fs::write(raiz.join("visivel.txt"), "nada demais\n").unwrap();
        let link = raiz.join("atalho.txt");
        if !criar_link(&segredo, &link, false) {
            // Sem privilégio de link o escape não é encenado; a contenção do
            // resolver sobre o alvo do link continua provada.
            assert!(pastas.resolver(&segredo.to_string_lossy()).is_err());
            limpar(&raiz);
            limpar(&fora);
            return;
        }
        for recursivo in [true, false] {
            let saida =
                buscar(&args(json!({"termo": "segredofora", "recursivo": recursivo})), &pastas)
                    .unwrap();
            assert!(!saida.texto.contains("SEGREDOFORA"), "recursivo={recursivo}");
            let Vista::Busca { total, achados, .. } = saida.vista else {
                panic!("vista errada");
            };
            assert_eq!(total, 0, "recursivo={recursivo}: o link não pode ser lido");
            assert!(achados.is_empty());
        }
        limpar(&raiz);
        limpar(&fora);
    }

    /// O mesmo vale para link de pasta: a travessia não pode descer por ele
    /// e ler a árvore de fora da concessão.
    #[test]
    fn buscar_nao_desce_em_link_de_pasta_que_aponta_para_fora() {
        let (_, pastas) = cenario("buscar-link-pasta");
        let raiz = pastas.raizes[0].clone();
        let fora = raiz
            .parent()
            .unwrap()
            .join(format!("aether-arq-linkpasta-fora-{}", std::process::id()));
        fs::create_dir_all(fora.join("sub")).unwrap();
        fs::write(fora.join("sub").join("segredo.txt"), "SEGREDOFORA fundo\n").unwrap();
        let link = raiz.join("atalho");
        if !criar_link(&fora, &link, true) {
            assert!(
                pastas
                    .resolver(&fora.join("sub").join("segredo.txt").to_string_lossy())
                    .is_err()
            );
            limpar(&raiz);
            limpar(&fora);
            return;
        }
        let saida = buscar(&args(json!({"termo": "segredofora"})), &pastas).unwrap();
        assert!(!saida.texto.contains("SEGREDOFORA"));
        let Vista::Busca { total, .. } = saida.vista else { panic!("vista errada") };
        assert_eq!(total, 0);
        limpar(&raiz);
        limpar(&fora);
    }

    /// A raiz concedida é a permissão do usuário, não conteúdo: renomeá-la
    /// revogaria a concessão por dentro, o mesmo motivo de `apagar` recusar.
    #[test]
    fn mover_a_propria_raiz_concedida_e_recusado() {
        let (raiz, pastas) = cenario("mover-raiz");
        let destino = raiz
            .parent()
            .unwrap()
            .join(format!("aether-arq-raiz-renomeada-{}", std::process::id()));
        let erro = mover(&args(json!({"origem": raiz, "destino": destino})), &pastas)
            .expect_err("a raiz concedida não se renomeia");
        assert!(erro.to_string().contains("pasta concedida"), "{erro}");
        assert!(raiz.exists());
        assert!(!destino.exists());
        limpar(&raiz);
        limpar(&destino);
    }

    #[test]
    fn o_padrao_casa_estrela_e_interrogacao() {
        assert!(casa("*.rs", "main.rs"));
        assert!(casa("*.rs", "MAIN.RS"));
        assert!(!casa("*.rs", "main.txt"));
        assert!(casa("a?c", "abc"));
        assert!(!casa("a?c", "ac"));
        assert!(casa("*", "qualquer"));
        assert!(casa("src/*", "src/main.rs"));
        assert!(!casa("src/*", "outro/main.rs"));
        assert!(casa("a*b*c", "a-xx-b-yy-c"));
        assert!(!casa("a*b*c", "a-xx-b"));
    }
}