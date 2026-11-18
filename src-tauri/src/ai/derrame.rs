//! O depósito dos resultados longos de ferramenta.
//!
//! Uma leitura de arquivo devolve até duzentos e cinquenta mil bytes e a saída de
//! um comando, até sessenta e quatro. O laço de conversa re-lê cada byte deles
//! pela placa em toda rodada seguinte, e o que o modelo costuma precisar de um
//! despejo assim são as duzentas linhas do meio — quando precisa. Aqui o corpo
//! fica num arquivo do aplicativo e ao modelo vão o começo, o fim e um id;
//! `ler_resultado` devolve o pedaço que ele pedir.
//!
//! Nada é jogado fora e nada é escondido: a janela diz que é janela, diz quantos
//! caracteres faltaram e com que nome ler o resto. O cartão da pessoa continua
//! recebendo o resultado inteiro — quem assiste não pode ver menos que o modelo.

use crate::idiomas::tr;
use crate::local::{inteiro, obrigatorio, Saida, Vista};
use anyhow::{Result, anyhow};
use serde_json::{Map, Value};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// A partir disto o corpo sai do histórico e entra no depósito. Abaixo, guardar
/// custaria um arquivo, um id e uma ida e volta à placa para não economizar nada.
pub const LIMIAR: usize = 4_096;
/// Caracteres do começo que continuam visíveis na janela.
pub const CABECA: usize = 1_536;
/// Caracteres do fim. Moram ali o erro de um comando e o resumo de uma saída.
pub const CAUDA: usize = 512;
/// Folga que a leitura deixa para o cabeçalho que vai na frente dela. Medido: a
/// linha que abre uma janela tem ~150 caracteres nos dois idiomas; 512 é a conta
/// com folga de sobra.
const FOLGA_CABECALHO: usize = 512;
/// Teto de uma leitura pedida. Fica abaixo do `LIMIAR` de propósito, e pela conta
/// e não pela intenção: o texto que a ferramenta devolve é o cabeçalho mais o
/// pedaço, e um pedaço do tamanho exato do limiar passaria dele — ler o depósito
/// viraria um resultado longo, derramado de novo, e o laço passaria a guardar
/// janelas de janelas.
pub const LEITURA_MAX: usize = LIMIAR - FOLGA_CABECALHO;
/// Quantos resultados o depósito guarda. Os mais antigos saem quando entram
/// mais novos, e a retenção é do corpo da conversa, não de uma conversa só.
pub const GUARDA: usize = 64;

const EXTENSAO: &str = ".txt";
/// Um instante nunca repetido dentro do processo, em milissegundos. É a chave de
/// ordem do id, e é por ela que a poda sabe qual resultado é o mais velho.
static INSTANTE: AtomicU64 = AtomicU64::new(0);

/// A forma de um id: treze dígitos de instante, um traço e quatro hexadecimais do
/// gerador aleatório do sistema. Nada mais é aceito, e é isso que faz um `id`
/// escrito pelo modelo ser um número de resultado, nunca um caminho.
pub fn eh_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    bytes.len() == 13 + 1 + 4
        && bytes[13] == b'-'
        && bytes[..13].iter().all(|c| c.is_ascii_digit())
        && bytes[14..]
            .iter()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

/// O id de um resultado novo. O instante é empurrado um milésimo para frente
/// quando dois resultados caem no mesmo milissegundo, porque a ordem dos nomes é
/// a ordem da chegada e a poda depende dela.
fn novo_id() -> String {
    let agora = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let mut anterior = INSTANTE.load(Ordering::Relaxed);
    loop {
        let novo = agora.max(anterior + 1);
        match INSTANTE.compare_exchange_weak(
            anterior,
            novo,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => {
                return format!("{novo:013}-{}", sufixo_aleatorio());
            }
            Err(atual) => anterior = atual,
        }
    }
}

fn sufixo_aleatorio() -> String {
    let mut bytes = [0u8; 2];
    // Sem o gerador o id ainda seria único dentro do processo, e um arquivo
    // escrito por outro processo do aplicativo seria sobrescrito em vez de
    // recusado: dois nomes iguais são dois resultados perdidos.
    if getrandom::fill(&mut bytes).is_err() {
        return "0000".to_string();
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn arquivo_da(pasta: &Path, id: &str) -> std::path::PathBuf {
    pasta.join(format!("{id}{EXTENSAO}"))
}

/// Os ids guardados, do mais novo para o mais antigo. Um nome que não é id não
/// entra: o depósito é esta pasta, e um arquivo solto ali não é um resultado.
fn guardados(pasta: &Path) -> Vec<String> {
    let Ok(leitura) = fs::read_dir(pasta) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = leitura
        .filter_map(|item| item.ok())
        .filter_map(|item| item.file_name().to_str().map(|n| n.to_owned()))
        .filter(|nome| nome.ends_with(EXTENSAO))
        .map(|nome| nome[..nome.len() - EXTENSAO.len()].to_string())
        .filter(|id| eh_id(id))
        .collect();
    ids.sort();
    ids.reverse();
    ids
}

/// A recusa de um id: diz o que foi pedido e o que existe agora, porque um número
/// que o modelo copiou torto se corrige com a lista, não com um "não encontrei".
fn ausente(pasta: &Path, id: &str) -> anyhow::Error {
    let ids = guardados(pasta);
    let lista = if ids.is_empty() {
        tr("derrame.depositoVazio", &[])
    } else {
        tr(
            "derrame.guardados",
            &[("lista", &ids.iter().take(3).cloned().collect::<Vec<_>>().join(", "))],
        )
    };
    anyhow!(tr("erros.resultadoAusente", &[("id", id), ("lista", &lista)]))
}

/// Guarda o corpo inteiro e devolve o id dele.
pub fn guardar(pasta: &Path, texto: &str) -> Result<String> {
    fs::create_dir_all(pasta).map_err(|e| {
        anyhow!(tr("erros.falhaDeposito", &[("detalhe", &e.to_string())]))
    })?;
    let id = novo_id();
    let alvo = arquivo_da(pasta, &id);
    fs::write(&alvo, texto).map_err(|e| {
        anyhow!(tr("erros.falhaDeposito", &[("detalhe", &e.to_string())]))
    })?;
    // Uma poda que falha não custa o resultado escrito: o próximo guardar tenta
    // de novo, e o teto de arquivos é folga de disco, não contrato de segurança.
    let _ = podar(pasta);
    Ok(id)
}

/// Mantém só os `GUARDA` resultados mais novos.
fn podar(pasta: &Path) -> Result<()> {
    let ids = guardados(pasta);
    for id in ids.iter().skip(GUARDA) {
        let _ = fs::remove_file(arquivo_da(pasta, id));
    }
    Ok(())
}

/// O que uma ferramenta devolve ao laço: `None` é "leve o texto inteiro",
/// `Some` é a janela, com o corpo guardado atrás dela. Um disco que recusa a
/// escrita devolve `None` de propósito — entre cortar em silêncio e despejar na
/// janela, o prejuízo menor é o da janela.
pub fn derramar(pasta: &Path, texto: &str) -> Option<String> {
    let total = texto.chars().count();
    if total <= LIMIAR {
        return None;
    }
    let id = guardar(pasta, texto).ok()?;
    let cabeca: String = texto.chars().take(CABECA).collect();
    let cauda: String = texto.chars().skip(total - CAUDA).collect();
    let omitidos = total - CABECA - CAUDA;
    let numero = |n: usize| n.to_string();
    Some(format!(
        "{}\n{}\n\n{}\n\n{}",
        tr(
            "derrame.cabecalho",
            &[
                ("total", &numero(total)),
                ("id", &id),
                ("cabeca", &numero(CABECA)),
                ("cauda", &numero(CAUDA)),
                ("omitidos", &numero(omitidos)),
            ]
        ),
        cabeca,
        tr("derrame.meio", &[("omitidos", &numero(omitidos))]),
        cauda,
    ))
}

/// Uma janela do corpo guardado.
#[derive(Debug, Clone)]
pub struct Trecho {
    pub id: String,
    pub texto: String,
    /// O índice do primeiro caractere devolvido e do último, mais um.
    pub desde: usize,
    pub ate: usize,
    pub total: usize,
}

/// Lê um pedaço do que está guardado. O `id` é aceito em qualquer caixa porque é
/// um número de resultado, não uma senha, e um modelo que ecoou o id em
/// maiúsculas não merece ter de rodar a ferramenta de novo.
pub fn ler(pasta: &Path, id: &str, desde: usize, quantos: usize) -> Result<Trecho> {
    let id = id.trim().to_lowercase();
    if !eh_id(&id) {
        return Err(ausente(pasta, &id));
    }
    let alvo = arquivo_da(pasta, &id);
    let corpo = match fs::read_to_string(&alvo) {
        Ok(corpo) => corpo,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ausente(pasta, &id));
        }
        Err(e) => {
            return Err(anyhow!(tr(
                "erros.falhaDeposito",
                &[("detalhe", &e.to_string())]
            )));
        }
    };
    let total = corpo.chars().count();
    let desde = desde.min(total);
    let quantos = quantos.clamp(1, LEITURA_MAX);
    let texto: String = corpo.chars().skip(desde).take(quantos).collect();
    let ate = desde + texto.chars().count();
    Ok(Trecho { id, texto, desde, ate, total })
}

/// A ferramenta com que o modelo lê o resto. Devolve a janela pedida e diz, no
/// próprio texto, se sobrou alguma coisa depois dela.
pub fn ferramenta(args: &Map<String, Value>, pasta: &Path) -> Result<Saida> {
    let pedido = obrigatorio(args, "id")?;
    let desde = inteiro(args, "desde").unwrap_or(0);
    let quantos = inteiro(args, "quantos").unwrap_or(LEITURA_MAX);
    let trecho = ler(pasta, &pedido, desde, quantos)?;
    let sobrou = trecho.total - trecho.ate;
    let cabecalho = if sobrou == 0 {
        tr(
            "derrame.janelaFechada",
            &[
                ("id", &trecho.id),
                ("desde", &trecho.desde.to_string()),
                ("ate", &trecho.ate.to_string()),
                ("total", &trecho.total.to_string()),
            ],
        )
    } else {
        tr(
            "derrame.janelaAberta",
            &[
                ("id", &trecho.id),
                ("desde", &trecho.desde.to_string()),
                ("ate", &trecho.ate.to_string()),
                ("total", &trecho.total.to_string()),
                ("sobrou", &sobrou.to_string()),
            ],
        )
    };
    let texto = format!("{cabecalho}\n{}", trecho.texto);
    Ok(Saida::nova(
        texto,
        Vista::Resultado {
            id: trecho.id,
            total: trecho.total,
            desde: trecho.desde,
            ate: trecho.ate,
            sobrou,
            texto: trecho.texto,
        },
    ))
}

#[cfg(test)]
mod testes {
    use super::*;
    use serde_json::json;

    fn pasta(nome: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("aether-derrame-{nome}-{}", std::process::id()))
    }

    /// Um corpo maior que o limiar, com o começo e o fim inconfundíveis e um
    /// meio que não aparece em nenhum dos dois.
    fn corpo_grande(medidas: usize) -> String {
        let mut texto = "COMEÇO".to_string();
        for i in 0..medidas {
            texto.push_str(&format!("|trecho {i:05} de corpo guardado|"));
        }
        texto.push_str("FIM");
        texto
    }

    fn limpar(pasta: &Path) {
        let _ = fs::remove_dir_all(pasta);
    }

    fn args(valores: Value) -> Map<String, Value> {
        valores.as_object().expect("objeto").clone()
    }

    #[test]
    fn um_resultado_ate_o_limiar_fica_inteiro_na_conversa() {
        let p = pasta("curto");
        assert_eq!(derramar(&p, &"a".repeat(LIMIAR)), None);
        assert_eq!(derramar(&p, &"a".repeat(LIMIAR - 1)), None);
        assert_eq!(derramar(&p, ""), None);
        // Curto demais para guardar: nada foi escrito, nada foi cortado.
        assert!(!p.exists());
    }

    #[test]
    fn o_corpo_largo_fica_guardado_inteiro_e_o_modelo_recebe_a_janela() {
        let p = pasta("largo");
        limpar(&p);
        let corpo = corpo_grande(2_000);
        let janela = derramar(&p, &corpo).expect("derramou");
        let total = corpo.chars().count();
        let id = guardados(&p)[0].clone();

        // O que ficou no depósito é byte a byte o que a ferramenta devolveu.
        assert_eq!(fs::read_to_string(arquivo_da(&p, &id)).expect("lido"), corpo);
        // O que vai ao modelo é a janela declarada: começo, meio omitido dito,
        // fim, e o nome com que se lê o resto.
        assert!(janela.contains("COMEÇO"), "{janela}");
        assert!(janela.ends_with("FIM"), "{janela}");
        assert!(janela.contains(&id), "{janela}");
        assert!(janela.contains(&total.to_string()), "{janela}");
        assert!(janela.contains(&(total - CABECA - CAUDA).to_string()), "{janela}");
        assert!(janela.contains("ler_resultado"), "{janela}");
        assert!(!janela.contains("|trecho 01000 de"), "o meio não foi despejado");
        assert!(janela.chars().count() < total / 2, "a janela tem de caber menor");
        limpar(&p);
    }

    #[test]
    fn ler_o_deposito_nao_produz_um_resultado_para_derramar_de_novo() {
        // A conta que fecha o laço: a maior leitura pedida cabe no limiar com o
        // cabeçalho na frente, então o modelo pode ler janela atrás de janela sem
        // nunca ver uma janela de janela ser guardada outra vez.
        assert!(LEITURA_MAX < LIMIAR);
        let p = pasta("regressao");
        limpar(&p);
        let corpo = corpo_grande(20_000);
        let id = guardar(&p, &corpo).expect("guardado");
        let lido = ferramenta(&json!({ "id": id }).as_object().unwrap().clone(), &p)
            .expect("li")
            .texto;
        assert_eq!(derramar(&p, &lido), None, "a leitura não virou despejo");
        limpar(&p);
    }

    #[test]
    fn as_janelas_pedidas_em_sequencia_recompoe_o_corpo() {
        let p = pasta("recomposicao");
        limpar(&p);
        let corpo = corpo_grande(300);
        let id = guardar(&p, &corpo).expect("guardado");
        let mut montado = String::new();
        let mut desde = 0usize;
        loop {
            let trecho = ler(&p, &id, desde, 200).expect("trecho");
            if trecho.ate >= trecho.total {
                montado.push_str(&trecho.texto);
                break;
            }
            montado.push_str(&trecho.texto);
            desde = trecho.ate;
        }
        assert_eq!(montado, corpo, "o depósito devolve o corpo todo, em pedaços");
        limpar(&p);
    }

    #[test]
    fn a_leitura_para_na_fronteira_de_um_caracter() {
        let p = pasta("multibyte");
        limpar(&p);
        // Quatro caracteres por repetição e seis bytes: 'ç' e 'ã' têm dois bytes
        // cada, então um recorte em byte partiça a palavra no meio.
        let corpo = "ação".repeat(3_000);
        assert_eq!(corpo.len(), 18_000);
        assert_eq!(corpo.chars().count(), 12_000);
        let total = corpo.chars().count();
        let janela = derramar(&p, &corpo).expect("derramou");
        assert!(!janela.contains('\u{fffd}'), "a janela não quebrou caractere");

        let id = guardados(&p)[0].clone();
        for desde in [0usize, 1, 2, 3, 4, 999, 1_500] {
            let trecho = ler(&p, &id, desde, 1).expect("um caractere");
            assert_eq!(trecho.texto.chars().count(), 1, "desde {desde}");
            assert_eq!(trecho.ate, desde + 1);
            assert!(!trecho.texto.contains('\u{fffd}'), "{desde} = {:?}", trecho.texto);
        }
        // Fora do corpo não há caractere nenhum, e nada estoura.
        assert_eq!(ler(&p, &id, total, 10).expect("no fim").texto, "");
        assert_eq!(ler(&p, &id, total + 9_000, 10).expect("longe demais").texto, "");
        limpar(&p);
    }

    #[test]
    fn um_id_fora_da_forma_nunca_vira_caminho() {
        let raiz = pasta("contencao");
        let p = raiz.join("deposito");
        limpar(&raiz);
        fs::create_dir_all(&p).expect("depósito");
        // Um corpo fora do depósito, com um texto que não pode aparecer em
        // nenhuma resposta.
        fs::write(raiz.join("segredo.txt"), "NADA DISSO SAI DO DEPÓSITO").expect("escrevi");

        for id in [
            "../segredo.txt",
            "../segredo",
            "..\\segredo.txt",
            "segredo.txt",
            "",
            "0000000000000-abcd",
            "12",
            &p.display().to_string(),
        ] {
            let erro = ler(&p, id, 0, 100).expect_err("id inválido não lê");
            let texto = format!("{erro:#}");
            assert!(!texto.contains("NADA DISSO"), "{id} → {texto}");
            assert!(!texto.starts_with("erros."), "mensagem crua: {texto}");
        }
        assert_eq!(guardados(&p), Vec::<String>::new(), "o depósito continua vazio");
        limpar(&raiz);
    }

    #[test]
    fn um_id_valido_que_saiu_do_deposito_diz_o_que_existe() {
        let p = pasta("ausente");
        limpar(&p);
        let id = guardar(&p, &corpo_grande(10)).expect("guardado");
        let erro = format!("{:#}", ler(&p, "9999999999999-ffff", 0, 10).unwrap_err());
        assert!(erro.contains("9999999999999-ffff"), "{erro}");
        // A lista é o que corrige um id copiado torto sem rodar a ferramenta outra vez.
        assert!(erro.contains(&id), "{erro}");
        limpar(&p);
    }

    #[test]
    fn o_deposito_guarda_somente_os_resultados_mais_novos() {
        let p = pasta("poda");
        limpar(&p);
        let mut ids = Vec::new();
        for i in 0..GUARDA + 5 {
            ids.push(guardar(&p, &format!("resultado {i}")).expect("guardado"));
        }
        let guardados = guardados(&p);
        assert_eq!(guardados.len(), GUARDA, "o teto é o que está em disco");
        assert_eq!(guardados[0], ids[ids.len() - 1], "o mais novo é o primeiro");
        for id in &ids[..5] {
            assert!(!guardados.contains(id), "{id} devia ter saído");
            assert!(!arquivo_da(&p, id).exists());
        }
        // O corpo do mais novo é o corpo dele, não o do que ficou no lugar.
        assert_eq!(
            ler(&p, &guardados[0], 0, 100).expect("li o mais novo").texto,
            format!("resultado {}", ids.len() - 1)
        );
        limpar(&p);
    }

    #[test]
    fn dois_resultados_no_mesmo_milissegundo_tem_ordens_diferentes() {
        let p = pasta("instante");
        limpar(&p);
        let mut ids = Vec::new();
        for _ in 0..50 {
            ids.push(guardar(&p, "corpo").expect("guardado"));
        }
        let distintos: std::collections::BTreeSet<&String> = ids.iter().collect();
        assert_eq!(distintos.len(), 50, "id repetido sobrescreveria um resultado");
        let mut ordenado = ids.clone();
        ordenado.sort();
        assert_eq!(ordenado, ids, "a ordem dos nomes é a ordem da chegada");
        limpar(&p);
    }

    #[test]
    fn um_deposito_que_recusa_a_escrita_devolve_o_texto_inteiro() {
        // A pasta do depósito é um arquivo: o caminho inteiro de escrita falha, e
        // o laço continua com o texto na mão em vez de perder o corpo.
        let p = pasta("recusa");
        limpar(&p);
        fs::write(&p, "isto não é uma pasta").expect("escrevi");
        assert_eq!(derramar(&p, &corpo_grande(5_000)), None);
        assert!(guardar(&p, "corpo").is_err());
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn a_ferramenta_diz_quanto_falta_depois_da_janela() {
        let p = pasta("ferramenta");
        limpar(&p);
        let corpo = corpo_grande(2_000);
        let id = guardar(&p, &corpo).expect("guardado");
        let total = corpo.chars().count();

        let aberta = ferramenta(&args(json!({ "id": id })), &p).expect("li o começo");
        assert!(aberta.texto.contains("ler_resultado"), "{}", aberta.texto);
        let Vista::Resultado { texto, total: t, desde, ate, sobrou, .. } = &aberta.vista else {
            panic!("a leitura do depósito desenha cartão de resultado");
        };
        assert_eq!(texto.chars().count(), LEITURA_MAX, "sem pedir quantos, a leitura vem no teto");
        assert_eq!((*t, *desde, *ate), (total, 0, LEITURA_MAX));
        assert_eq!(*sobrou, total - LEITURA_MAX);

        // No fim do corpo a frase é outra: não há o que pedir de novo.
        let fechada =
            ferramenta(&args(json!({ "id": id, "desde": total - 10 })), &p).expect("li o fim");
        let Vista::Resultado { sobrou, texto, .. } = &fechada.vista else {
            panic!("cartão de resultado");
        };
        assert_eq!(*sobrou, 0);
        assert_eq!(texto.chars().count(), 10);
        assert!(!fechada.texto.contains("ler_resultado"), "{}", fechada.texto);
        limpar(&p);
    }

    #[test]
    fn sem_id_a_ferramenta_nomeia_o_argumento_que_falta() {
        let p = pasta("semid");
        let erro = ferramenta(&args(json!({})), &p).expect_err("sem id não se lê nada");
        assert!(format!("{erro:#}").contains("id"), "{erro}");
    }
}
