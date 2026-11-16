//! O terminal de verdade: um processo filho, saída real e código de saída real.
//!
//! O comando nunca abre janela nem espera digitação: a entrada é fechada, a
//! saída é lida até o fim e há um prazo. Quando o prazo estoura, a árvore
//! inteira é derrubada — matar só o processo direto deixaria um `npm` com netos
//! vivos rodando escondido.

use crate::idiomas::tr;
use crate::local::pastas::Pastas;
use crate::local::{Saida, Vista, inteiro, raiz_de_trabalho, texto};
use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value};
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const TEMPO_PADRAO: u64 = 60;
const TEMPO_MINIMO: u64 = 1;
const TEMPO_MAXIMO: u64 = 600;
const LIMITE_SAIDA: usize = 64 * 1024;
const PASSO: Duration = Duration::from_millis(30);
const ESPERA_TUBOS: Duration = Duration::from_secs(3);

#[cfg(windows)]
const SEM_JANELA: u32 = 0x0800_0000;

/// Lê um tubo até o fim, guardando só o começo. Continua lendo depois do limite
/// de propósito: parar de ler encheria o tubo e o filho travaria para sempre.
fn drenar(mut fonte: impl Read + Send + 'static) -> mpsc::Receiver<(Vec<u8>, bool)> {
    let (envia, recebe) = mpsc::channel();
    std::thread::spawn(move || {
        let mut guardado: Vec<u8> = Vec::new();
        let mut truncado = false;
        let mut pedaco = [0u8; 8192];
        loop {
            match fonte.read(&mut pedaco) {
                Ok(0) | Err(_) => break,
                Ok(lidos) => {
                    let cabe = LIMITE_SAIDA.saturating_sub(guardado.len()).min(lidos);
                    guardado.extend_from_slice(&pedaco[..cabe]);
                    truncado |= cabe < lidos;
                }
            }
        }
        let _ = envia.send((guardado, truncado));
    });
    recebe
}

fn receber(recebe: &mpsc::Receiver<(Vec<u8>, bool)>) -> (Vec<u8>, bool, bool) {
    match recebe.recv_timeout(ESPERA_TUBOS) {
        Ok((bruto, truncado)) => (bruto, truncado, false),
        // Um neto pode manter o tubo aberto depois de o filho morrer. Não
        // prendemos o aplicativo esperando: dizemos que a saída não veio toda.
        Err(_) => (Vec::new(), true, true),
    }
}

/// O console do Windows escreve na página de códigos local, não em UTF-8. Sem
/// esta troca o acento de um `dir` viraria caractere de substituição.
fn decodificar(bruto: &[u8]) -> String {
    match std::str::from_utf8(bruto) {
        Ok(texto) => texto.to_string(),
        Err(_) => encoding_rs::WINDOWS_1252.decode(bruto).0.into_owned(),
    }
}

#[cfg(windows)]
fn encerrar(filho: &mut Child) {
    use std::os::windows::process::CommandExt;
    let _ = Command::new("taskkill")
        .args(["/T", "/F", "/PID", &filho.id().to_string()])
        .creation_flags(SEM_JANELA)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = filho.kill();
    let _ = filho.wait();
}

#[cfg(not(windows))]
fn encerrar(filho: &mut Child) {
    let _ = filho.kill();
    let _ = filho.wait();
}

pub fn executar(args: &Map<String, Value>, pastas: &Pastas) -> Result<Saida> {
    let comando = texto(args, "comando").unwrap_or_default().trim().to_string();
    if comando.is_empty() {
        bail!(tr("erros.comandoVazio", &[]));
    }

    let pasta = match texto(args, "caminho") {
        Some(pedido) => pastas.resolver(&pedido)?,
        None => raiz_de_trabalho(pastas).ok_or_else(|| anyhow!(tr("erros.semPastas", &[])))?,
    };
    if !pasta.is_dir() {
        bail!(tr("erros.naoEPasta", &[("caminho", &pasta.display().to_string())]));
    }

    let segundos = inteiro(args, "tempoLimite")
        .map(|v| v as u64)
        .unwrap_or(TEMPO_PADRAO)
        .clamp(TEMPO_MINIMO, TEMPO_MAXIMO);

    let mut programa = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/C", &comando]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", &comando]);
        c
    };
    programa
        .current_dir(&pasta)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        programa.creation_flags(SEM_JANELA);
    }

    let inicio = Instant::now();
    let mut filho = programa
        .spawn()
        .map_err(|e| anyhow!(tr("erros.falhaExecutar", &[("detalhe", &e.to_string())])))?;

    let tubo_saida = drenar(filho.stdout.take().expect("saída criada como tubo"));
    let tubo_erro = drenar(filho.stderr.take().expect("erro criado como tubo"));

    let prazo = Duration::from_secs(segundos);
    let mut estourou = false;
    let mut falha: Option<String> = None;
    let estado = loop {
        match filho.try_wait() {
            Ok(Some(estado)) => break Some(estado),
            Ok(None) => {
                if inicio.elapsed() >= prazo {
                    estourou = true;
                    encerrar(&mut filho);
                    break None;
                }
                std::thread::sleep(PASSO);
            }
            Err(e) => {
                falha = Some(e.to_string());
                encerrar(&mut filho);
                break None;
            }
        }
    };

    let (bruto_saida, corte_saida, perda_saida) = receber(&tubo_saida);
    let (bruto_erro, corte_erro, perda_erro) = receber(&tubo_erro);
    let duracao = inicio.elapsed().as_millis() as u64;

    let saida = decodificar(&bruto_saida).trim_end().to_string();
    let erros = decodificar(&bruto_erro).trim_end().to_string();
    let codigo = estado.and_then(|e| e.code());
    let incompleto = perda_saida || perda_erro;

    // Um código diferente de zero é resultado, não defeito da ferramenta: o
    // modelo precisa ler a saída para decidir o que fazer. O cartão é que
    // pinta a falha, a partir deste mesmo código.
    let mut para_modelo = format!("$ {comando}\n");
    if !saida.is_empty() {
        para_modelo.push_str(&saida);
        para_modelo.push('\n');
    }
    if !erros.is_empty() {
        para_modelo.push_str(&format!("{}:\n{erros}\n", tr("cartao.saidaErro", &[])));
    }
    if let Some(codigo) = codigo {
        para_modelo.push_str(&tr("cartao.codigoSaida", &[("n", &codigo.to_string())]));
        para_modelo.push('\n');
    }
    if estourou {
        para_modelo.push_str(&tr("erros.tempoEsgotado", &[("segundos", &segundos.to_string())]));
        para_modelo.push('\n');
    }
    if let Some(detalhe) = &falha {
        para_modelo.push_str(&tr("erros.falhaExecutar", &[("detalhe", detalhe)]));
        para_modelo.push('\n');
    }
    if incompleto {
        para_modelo.push_str(&tr("cartao.saidaIncompleta", &[]));
        para_modelo.push('\n');
    }
    if saida.is_empty() && erros.is_empty() && !estourou && falha.is_none() {
        para_modelo.push_str(&tr("cartao.semSaida", &[]));
        para_modelo.push('\n');
    }

    Ok(Saida::nova(
        para_modelo,
        Vista::Comando {
            comando,
            pasta: pasta.display().to_string(),
            codigo,
            saida,
            erro: erros,
            duracao_ms: duracao,
            estourou,
            truncado: corte_saida || corte_erro || incompleto,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::pastas::Pastas;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static CONTADOR: AtomicUsize = AtomicUsize::new(0);

    /// Cada teste ganha a própria pasta: testes rodam em paralelo e um não pode
    /// apagar o arquivo que o outro acabou de escrever.
    struct Temporaria(std::path::PathBuf);

    impl Drop for Temporaria {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn pasta() -> (Pastas, Temporaria) {
        let n = CONTADOR.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!("aether-terminal-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&base).expect("criei a pasta de teste");
        let mut pastas = Pastas::default();
        pastas.conceder(&base.display().to_string()).expect("concedi");
        (pastas, Temporaria(base))
    }

    fn rodar(pastas: &Pastas, comando: &str) -> Saida {
        let args = json!({ "comando": comando }).as_object().expect("objeto").clone();
        executar(&args, pastas).expect("executei")
    }

    #[test]
    fn a_saida_e_o_codigo_de_saida_sao_os_do_processo_real() {
        let (pastas, _guarda) = pasta();
        let saida = rodar(&pastas, "echo aether");
        assert!(saida.texto.contains("aether"), "{}", saida.texto);
        match saida.vista {
            Vista::Comando { codigo, saida: texto, estourou, .. } => {
                assert_eq!(codigo, Some(0));
                assert!(texto.contains("aether"), "{texto}");
                assert!(!estourou);
            }
            outra => panic!("vista errada: {outra:?}"),
        }
    }

    #[test]
    fn um_comando_que_falha_devolve_o_codigo_verdadeiro() {
        let (pastas, _guarda) = pasta();
        let saida = rodar(&pastas, "exit 7");
        match saida.vista {
            Vista::Comando { codigo, .. } => assert_eq!(codigo, Some(7)),
            outra => panic!("vista errada: {outra:?}"),
        }
        assert!(saida.texto.contains('7'), "{}", saida.texto);
    }

    #[test]
    #[cfg(windows)]
    fn o_comando_roda_dentro_da_pasta_concedida() {
        let (pastas, guarda) = pasta();
        std::fs::write(guarda.0.join("marca.txt"), "presente").expect("escrevi");
        let saida = rodar(&pastas, "dir /b");
        assert!(saida.texto.contains("marca.txt"), "{}", saida.texto);
    }

    #[test]
    #[cfg(windows)]
    fn o_prazo_interrompe_o_comando_e_diz_que_interrompeu() {
        let (pastas, _guarda) = pasta();
        let args = json!({ "comando": "ping -n 30 127.0.0.1", "tempoLimite": 1 })
            .as_object()
            .expect("objeto")
            .clone();
        let inicio = Instant::now();
        let saida = executar(&args, &pastas).expect("executei");
        assert!(inicio.elapsed() < Duration::from_secs(20), "demorou demais");
        match saida.vista {
            Vista::Comando { estourou, duracao_ms, .. } => {
                assert!(estourou);
                assert!(duracao_ms < 20_000, "{duracao_ms}");
            }
            outra => panic!("vista errada: {outra:?}"),
        }
        assert!(saida.texto.contains("interrompido"), "{}", saida.texto);
    }

    #[test]
    #[cfg(windows)]
    fn saida_gigante_e_cortada_sem_travar_o_filho() {
        let (pastas, _guarda) = pasta();
        // Mais que o limite: se parássemos de ler, o tubo encheria e o comando
        // nunca terminaria.
        let args = json!({
            "comando": "for /l %i in (1,1,4000) do @echo 0123456789012345678901234567890123456789",
            "tempoLimite": 30
        })
        .as_object()
        .expect("objeto")
        .clone();
        let saida = executar(&args, &pastas).expect("executei");
        match saida.vista {
            Vista::Comando { truncado, saida: texto, codigo, .. } => {
                assert!(truncado);
                assert!(texto.len() <= LIMITE_SAIDA, "{}", texto.len());
                assert_eq!(codigo, Some(0));
            }
            outra => panic!("vista errada: {outra:?}"),
        }
    }

    #[test]
    fn sem_pasta_concedida_o_comando_e_recusado() {
        let args = json!({ "comando": "echo oi" }).as_object().expect("objeto").clone();
        let erro = executar(&args, &Pastas::default()).unwrap_err().to_string();
        assert_eq!(erro, tr("erros.semPastas", &[]));
    }

    #[test]
    fn comando_vazio_e_recusado() {
        let (pastas, _guarda) = pasta();
        let args = json!({ "comando": "   " }).as_object().expect("objeto").clone();
        let erro = executar(&args, &pastas).unwrap_err().to_string();
        assert_eq!(erro, tr("erros.comandoVazio", &[]));
    }

    #[test]
    fn pasta_fora_das_concedidas_e_recusada() {
        let (pastas, guarda) = pasta();
        // A pasta-mãe da concedida está fora dela: é o próprio diretório temporário.
        let fora = guarda.0.parent().expect("pasta-mãe").display().to_string();
        let args = json!({ "comando": "echo oi", "caminho": fora })
            .as_object()
            .expect("objeto")
            .clone();
        let erro = executar(&args, &pastas).unwrap_err().to_string();
        assert!(erro.contains("fora das pastas"), "{erro}");
    }

    #[test]
    fn o_prazo_pedido_e_limitado_ao_maximo() {
        let (pastas, _guarda) = pasta();
        let args = json!({ "comando": "echo oi", "tempoLimite": 999_999 })
            .as_object()
            .expect("objeto")
            .clone();
        let saida = executar(&args, &pastas).expect("executei");
        assert!(matches!(saida.vista, Vista::Comando { estourou: false, .. }));
    }

    #[test]
    fn a_saida_do_erro_chega_separada_da_saida_normal() {
        let (pastas, _guarda) = pasta();
        let saida = rodar(&pastas, "echo problema 1>&2");
        match saida.vista {
            Vista::Comando { erro, saida: texto, .. } => {
                assert!(erro.contains("problema"), "{erro}");
                assert!(!texto.contains("problema"), "{texto}");
            }
            outra => panic!("vista errada: {outra:?}"),
        }
    }
}