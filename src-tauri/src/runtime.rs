//! O runtime nativo: as DLLs de que o motor de inferência e a voz dependem.
//!
//! Elas não estão no repositório — são 46 MB de binários de outros projetos, e o
//! `.gitignore` as deixa de fora de propósito. Até aqui isso significava um passo
//! manual ([`vendor/README.md`]) antes de o aplicativo conseguir carregar um
//! modelo: num PC novo o download dos pesos terminava e a inferência não abria,
//! porque `llama.dll` não estava lá. Este módulo fecha esse buraco: o aplicativo
//! baixa o pacote oficial, confere cada arquivo contra o `SHA256SUMS.txt`
//! versionado e só então o põe no lugar.
//!
//! Duas decisões que não são cosméticas:
//!
//! - **A revisão é presa.** O FFI repete campo por campo os cabeçalhos de
//!   `vendor/llama-cpp/include` (b12387) e `vendor/sherpa-onnx/include` (1.13.8).
//!   Uma DLL de outra release tem outro layout de struct, e o sintoma disso não
//!   é erro de compilação: é leitura de memória alheia. Por isso a URL aponta
//!   para a tag exata, e o teste `a_url_esta_pres_na_revisao_dos_cabecalhos`
//!   confere que ela não anda sozinha.
//! - **Nada entra sem conferir.** O hash de cada arquivo extraído é comparado com
//!   a soma versionada antes de qualquer um deles ser movido. Um pacote que
//!   diverge não instala metade: ele falha com o nome do arquivo e os dois
//!   hashes, e o que estava no disco continua lá.
//!
//! O destino é a pasta de dados do aplicativo, não a pasta do executável: em
//! `Arquivos de Programas` escrever exigiria administrador, e o instalador do
//! Windows não pediria isso para baixar uma DLL. O carregador procura nessa
//! pasta pelo mesmo caminho que procura ao lado do executável — ver
//! `pastas_de_dll` em `lib.rs`.

use crate::ai::download::{client, stream_to_file};
use crate::idiomas::tr;
use crate::voz::baixar::extrair;
use anyhow::{Context, bail};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Como o pacote chega. Os dois formatos são lidos pelo `tar` que o Windows
/// traz — o mesmo extrator que já repõe os arquivos do Kokoro.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Formato {
    Zip,
    TarBz2,
}

/// Um pacote de runtime: de onde vem, quanto pesa e o que tem de bater.
pub struct Pacote {
    pub id: &'static str,
    pub nome: &'static str,
    pub url: &'static str,
    /// Tamanho exato do asset, lido da API de releases do projeto de origem.
    /// É o que `stream_to_file` confere no fim: um download truncado não vira
    /// instalação.
    pub bytes: u64,
    pub formato: Formato,
    /// Pasta dentro do pacote onde os arquivos estão. O zip do llama.cpp põe
    /// tudo na raiz; o tar do sherpa-onnx põe tudo sob uma pasta com o nome da
    /// release.
    pub dentro: &'static str,
    /// As somas versionadas, no formato do `sha256sum -c`: `<hash>  lib/<nome>`.
    pub somas: &'static str,
}

/// Os dois pacotes de que o aplicativo depende. Tamanhos medidos na API de
/// releases em 16/11/2026: `llama-b12387-bin-win-vulkan-x64.zip` com 38.119.681
/// bytes e `sherpa-onnx-v1.13.8-win-x64-shared-MT-Release-lib.tar.bz2` com
/// 8.032.957. Os hashes de todos os arquivos extraídos batem com os DLLs
/// vendorizados desta máquina — 20 de 20 no llama.cpp, 3 de 3 no sherpa-onnx.
pub const PACOTES: &[Pacote] = &[
    Pacote {
        id: "llama-cpp",
        nome: "llama.cpp b12387 — Vulkan",
        url: "https://github.com/ggml-org/llama.cpp/releases/download/b12387/llama-b12387-bin-win-vulkan-x64.zip",
        bytes: 38_119_681,
        formato: Formato::Zip,
        dentro: "",
        somas: include_str!("../vendor/llama-cpp/SHA256SUMS.txt"),
    },
    Pacote {
        id: "sherpa-onnx",
        nome: "sherpa-onnx 1.13.8 — voz",
        url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/v1.13.8/sherpa-onnx-v1.13.8-win-x64-shared-MT-Release-lib.tar.bz2",
        bytes: 8_032_957,
        formato: Formato::TarBz2,
        dentro: "sherpa-onnx-v1.13.8-win-x64-shared-MT-Release-lib/",
        somas: include_str!("../vendor/sherpa-onnx/SHA256SUMS.txt"),
    },
];

/// O que a tela escuta enquanto o runtime chega.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Evento {
    Iniciou {
        id: String,
        total_bytes: u64,
    },
    Progresso {
        id: String,
        done_bytes: u64,
        total_bytes: u64,
        bytes_por_segundo: u64,
    },
    Extraindo {
        id: String,
    },
    /// Conferindo os hashes: aqui não há byte de rede a contar, e a barra não
    /// deve fingir que anda.
    Conferindo {
        id: String,
    },
    Concluiu {
        id: String,
    },
    Falhou {
        id: String,
        erro: String,
    },
    Cancelou {
        id: String,
    },
}

/// O que o carregador precisa de cada pacote, lido das somas versionadas.
pub fn arquivos(pacote: &Pacote) -> Vec<(String, String)> {
    let mut saida = Vec::new();
    for linha in pacote.somas.lines() {
        let linha = linha.trim();
        if linha.is_empty() || linha.starts_with('#') {
            continue;
        }
        let mut partes = linha.split_whitespace();
        let (Some(hash), Some(nome)) = (partes.next(), partes.next()) else {
            continue;
        };
        saida.push((nome.to_string(), hash.to_string()));
    }
    saida
}

/// As bibliotecas que o carregador precisa achar. É o subconjunto de
/// [`arquivos`] que faz o runtime *funcionar*: o pacote do llama.cpp também
/// traz `LICENSE-LLVM-OpenMP`, que é texto de licença e nunca é aberto por
/// ninguém.
///
/// Medido nesta máquina: a pasta do executável tinha os 19 DLLs e a faixa dizia
/// "Runtime nativo incompleto", porque a contagem de presença pedia também a
/// licença — que o instalador desta máquina colocou noutra pasta. Perguntar por
/// arquivo de licença a uma pergunta sobre o runtime é uma pergunta errada.
pub fn bibliotecas(pacote: &Pacote) -> Vec<(String, String)> {
    arquivos(pacote)
        .into_iter()
        .filter(|(nome, _)| base(nome).ends_with(".dll"))
        .collect()
}

/// O nome do arquivo dentro do pacote. As somas são escritas para o
/// `sha256sum -c` rodar na pasta vendorizada, então trazem o prefixo `lib/`;
/// o zip do llama.cpp não tem essa pasta, e o tar do sherpa tem a pasta da
/// release antes dela.
fn entrada(pacote: &Pacote, nome_soma: &str) -> String {
    let base = nome_soma.strip_prefix("lib/").unwrap_or(nome_soma);
    match pacote.formato {
        Formato::Zip => base.to_string(),
        Formato::TarBz2 => format!("{}lib/{base}", pacote.dentro),
    }
}

/// Só o nome do arquivo, que é como ele fica na pasta do runtime: o carregador
/// procura `llama.dll`, não `lib/llama.dll`.
fn base(nome_soma: &str) -> &str {
    nome_soma.rsplit('/').next().unwrap_or(nome_soma)
}

/// Onde o runtime instalado fica. Pasta de dados do aplicativo, gravável sem
/// administrador — ver o comentário do módulo.
pub fn pasta(dados: &Path) -> PathBuf {
    dados.join("runtime")
}

/// O estado de cada pacote nas pastas que o carregador procura. Um pacote está
/// completo quando UMA pasta tem todas as bibliotecas dele: espalhar `llama.dll`
/// numa pasta e `ggml-vulkan.dll` noutra não funciona, porque a `ggml.dll`
/// carrega os próprios backends da pasta de onde ela foi aberta.
///
/// Isto olha presença, não conteúdo: conferir 46 MB de hash a cada `ai_status`
/// prenderia a interface. A conferência de conteúdo é [`verificar`] e o
/// instalador, que a faz antes de mover qualquer arquivo.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EstadoPacote {
    pub id: String,
    pub nome: String,
    pub completo: bool,
    /// A pasta que atendeu ao conjunto inteiro, se alguma atendeu.
    pub pasta: Option<String>,
    pub faltando: Vec<String>,
    /// As bibliotecas procuradas — não o conteúdo inteiro do pacote, que tem
    /// licença. Ver [`bibliotecas`].
    pub arquivos: Vec<String>,
    pub bytes_pacote: u64,
}

pub fn estado(pastas: &[PathBuf]) -> Vec<EstadoPacote> {
    PACOTES
        .iter()
        .map(|p| {
            let nomes: Vec<String> = bibliotecas(p)
                .iter()
                .map(|(n, _)| base(n).to_string())
                .collect();
            let achei = pastas.iter().find(|pasta| {
                nomes.iter().all(|n| pasta.join(n).is_file())
            });
            let faltando = match achei {
                Some(pasta) => nomes
                    .iter()
                    .filter(|n| !pasta.join(n).is_file())
                    .cloned()
                    .collect(),
                // Nenhuma pasta tem o conjunto: o que falta é o que a primeira
                // pasta procurada não tem, que é o que o usuário veria no erro
                // do carregador.
                None => {
                    let primeira = pastas.first();
                    nomes
                        .iter()
                        .filter(|n| !primeira.is_some_and(|p| p.join(n).is_file()))
                        .cloned()
                        .collect()
                }
            };
            EstadoPacote {
                id: p.id.to_string(),
                nome: p.nome.to_string(),
                completo: achei.is_some(),
                pasta: achei.map(|p| p.display().to_string()),
                faltando,
                arquivos: nomes,
                bytes_pacote: p.bytes,
            }
        })
        .collect()
}

/// Um arquivo conferido contra a soma versionada.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Conferido {
    pub nome: String,
    pub presente: bool,
    pub bateu: bool,
    /// O hash medido agora, ou vazio quando o arquivo não está lá.
    pub medido: String,
    pub esperado: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Veredito {
    pub id: String,
    pub conferido: bool,
    pub arquivos: Vec<Conferido>,
}

fn sha256(caminho: &Path) -> anyhow::Result<String> {
    let mut arquivo = std::fs::File::open(caminho).map_err(|e| {
        anyhow::anyhow!(tr(
            "erros.falhaAbrir",
            &[("caminho", &caminho.display().to_string()), ("detalhe", &e.to_string())]
        ))
    })?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut arquivo, &mut hasher).map_err(|e| {
        anyhow::anyhow!(tr(
            "erros.falhaLer",
            &[("caminho", &caminho.display().to_string()), ("detalhe", &e.to_string())]
        ))
    })?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// Confere o conteúdo do que está instalado. É a resposta honesta a "está
/// inteiro?": presença e tamanho não provam que o binário é o que este código
/// foi testado em executar.
///
/// Confere as bibliotecas, não o pacote inteiro: a licença que vem junto não é
/// o que o carregador abre, e um veredito "incompleto" porque falta um `.txt`
/// manda o usuário reinstalar o que já está perfeito. Quem move os arquivos na
/// instalação continua usando [`arquivos`], licença incluída.
pub fn verificar(pastas: &[PathBuf]) -> Vec<Veredito> {
    PACOTES
        .iter()
        .map(|p| {
            let mut itens = Vec::new();
            for (nome, esperado) in bibliotecas(p) {
                let nome = base(&nome).to_string();
                let achei = pastas.iter().map(|d| d.join(&nome)).find(|c| c.is_file());
                let (presente, bateu, medido) = match achei {
                    Some(caminho) => match sha256(&caminho) {
                        Ok(h) => (true, h == esperado, h),
                        Err(_) => (true, false, String::new()),
                    },
                    None => (false, false, String::new()),
                };
                itens.push(Conferido { nome, presente, bateu, medido, esperado });
            }
            let conferido = itens.iter().all(|i| i.presente && i.bateu);
            Veredito { id: p.id.to_string(), conferido, arquivos: itens }
        })
        .collect()
}

/// O nome do asset na URL, que é como o pacote fica em disco enquanto baixa.
fn nome_do_asset(url: &str) -> &str {
    url.rsplit('/').next().unwrap_or("pacote")
}

/// Baixa, extrai, confere e instala o que falta. `destino` é a pasta do runtime.
///
/// O pacote baixado fica em `.pacotes/` e some depois de instalado: são 46 MB
/// que o usuário não pediu para guardar. Se a conferência falhar, nada é movido
/// e o `.part` do download fica para a próxima tentativa continuar.
pub async fn instalar(
    destino: &Path,
    ids: &[String],
    cancel: &AtomicBool,
    emit: impl Fn(Evento) + Clone,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(destino).map_err(|e| {
        anyhow::anyhow!(tr(
            "erros.falhaCriarPasta",
            &[("caminho", &destino.display().to_string()), ("detalhe", &e.to_string())]
        ))
    })?;
    let pacotes: Vec<&Pacote> = PACOTES
        .iter()
        .filter(|p| ids.is_empty() || ids.iter().any(|i| i == p.id))
        .collect();
    if pacotes.is_empty() {
        bail!(tr("erros.runtimePacoteDesconhecido", &[("ids", &ids.join(", "))]));
    }
    let http = client()?;
    let downloads = destino.join(".pacotes");
    std::fs::create_dir_all(&downloads).map_err(|e| {
        anyhow::anyhow!(tr(
            "erros.falhaCriarPasta",
            &[("caminho", &downloads.display().to_string()), ("detalhe", &e.to_string())]
        ))
    })?;

    for pacote in pacotes {
        let id = pacote.id.to_string();
        if cancel.load(Ordering::Relaxed) {
            emit(Evento::Cancelou { id });
            return Ok(());
        }
        let resultado = async {
            let asset = downloads.join(nome_do_asset(pacote.url));
            emit(Evento::Iniciou { id: id.clone(), total_bytes: pacote.bytes });
            let progresso = emit.clone();
            let id_progresso = id.clone();
            stream_to_file(
                &http,
                pacote.url,
                &asset,
                Some(pacote.bytes),
                cancel,
                move |done, total, speed| {
                    progresso(Evento::Progresso {
                        id: id_progresso.clone(),
                        done_bytes: done,
                        total_bytes: total,
                        bytes_por_segundo: speed,
                    });
                },
            )
            .await
            .with_context(|| tr("erros.runtimePacote", &[("nome", pacote.nome)]))?;

            if cancel.load(Ordering::Relaxed) {
                bail!(tr("erros.pedidoCancelado", &[]));
            }
            emit(Evento::Extraindo { id: id.clone() });
            let tmp = destino.join(".extraindo").join(pacote.id);
            if tmp.exists() {
                std::fs::remove_dir_all(&tmp).map_err(|e| {
                    anyhow::anyhow!(tr(
                        "erros.falhaApagar",
                        &[("caminho", &tmp.display().to_string()), ("detalhe", &e.to_string())]
                    ))
                })?;
            }
            std::fs::create_dir_all(&tmp).map_err(|e| {
                anyhow::anyhow!(tr(
                    "erros.falhaCriarPasta",
                    &[("caminho", &tmp.display().to_string()), ("detalhe", &e.to_string())]
                ))
            })?;
            extrair(&asset, &tmp)?;

            if cancel.load(Ordering::Relaxed) {
                bail!(tr("erros.pedidoCancelado", &[]));
            }
            emit(Evento::Conferindo { id: id.clone() });
            // Confere tudo antes de mover qualquer coisa: um pacote que diverge
            // não pode deixar metade de um runtime novo no disco.
            let mut conferidos: Vec<(PathBuf, PathBuf)> = Vec::new();
            for (nome, esperado) in arquivos(pacote) {
                let origem = tmp.join(entrada(pacote, &nome));
                if !origem.is_file() {
                    bail!(tr(
                        "erros.runtimeArquivoForaDoPacote",
                        &[("nome", base(&nome)), ("pacote", pacote.nome)]
                    ));
                }
                let medido = sha256(&origem)?;
                if medido != esperado {
                    bail!(tr(
                        "erros.runtimeHashDiverge",
                        &[
                            ("nome", base(&nome)),
                            ("esperado", &esperado),
                            ("medido", &medido),
                        ]
                    ));
                }
                conferidos.push((origem, destino.join(base(&nome))));
            }
            for (origem, alvo) in conferidos {
                // O carregador do Windows segura a DLL enquanto ela está aberta
                // num processo. Se este arquivo já está em uso, o erro real do
                // sistema é a resposta — não um "instalado" que não aconteceu.
                if alvo.exists() {
                    std::fs::remove_file(&alvo).map_err(|e| {
                        anyhow::anyhow!(tr(
                            "erros.falhaApagar",
                            &[("caminho", &alvo.display().to_string()), ("detalhe", &e.to_string())]
                        ))
                    })?;
                }
                std::fs::rename(&origem, &alvo).map_err(|e| {
                    anyhow::anyhow!(tr(
                        "erros.runtimeFalhaMover",
                        &[
                            ("origem", &origem.display().to_string()),
                            ("destino", &alvo.display().to_string()),
                            ("detalhe", &e.to_string()),
                        ]
                    ))
                })?;
            }
            let _ = std::fs::remove_dir_all(destino.join(".extraindo"));
            let _ = std::fs::remove_file(&asset);
            Ok::<(), anyhow::Error>(())
        }
        .await;

        match resultado {
            Ok(()) => emit(Evento::Concluiu { id }),
            Err(e) => {
                let _ = std::fs::remove_dir_all(destino.join(".extraindo").join(pacote.id));
                if cancel.load(Ordering::Relaxed) {
                    emit(Evento::Cancelou { id });
                    return Ok(());
                }
                let erro = format!("{e:#}");
                emit(Evento::Falhou { id, erro: erro.clone() });
                bail!(erro);
            }
        }
    }
    let _ = std::fs::remove_dir(&downloads);
    Ok(())
}

#[cfg(test)]
mod testes {
    use super::*;

    fn temporaria(nome: &str) -> PathBuf {
        let raiz = std::env::temp_dir().join(format!("aether-runtime-{nome}"));
        let _ = std::fs::remove_dir_all(&raiz);
        std::fs::create_dir_all(&raiz).unwrap();
        raiz
    }

    /// O FFI repete os cabeçalhos de uma revisão exata. Se a URL andar para
    /// outra release sem os cabeçalhos andarem junto, o aplicativo carrega uma
    /// biblioteca com outro layout de struct — e isso não dá erro de compilação.
    #[test]
    fn a_url_esta_pres_na_revisao_dos_cabecalhos() {
        let llama = PACOTES.iter().find(|p| p.id == "llama-cpp").unwrap();
        assert!(llama.url.contains("/b12387/"), "{}", llama.url);
        assert!(llama.url.contains("win-vulkan-x64"), "{}", llama.url);
        let sherpa = PACOTES.iter().find(|p| p.id == "sherpa-onnx").unwrap();
        assert!(sherpa.url.contains("/v1.13.8/"), "{}", sherpa.url);
        assert!(sherpa.url.contains("win-x64-shared-MT-Release-lib"), "{}", sherpa.url);
    }

    /// As somas são a lista do que o carregador procura. Um arquivo que o FFI
    /// abre e não está aqui entraria sem conferência.
    #[test]
    fn as_somas_cobrem_o_que_o_carregador_abre() {
        let llama = PACOTES.iter().find(|p| p.id == "llama-cpp").unwrap();
        let nomes: Vec<String> = arquivos(llama).iter().map(|(n, _)| base(n).to_string()).collect();
        for obrigatorio in ["llama.dll", "ggml.dll", "ggml-base.dll", "ggml-vulkan.dll", "libomp.dll"] {
            assert!(nomes.iter().any(|n| n == obrigatorio), "falta {obrigatorio}");
        }
        assert!(nomes.iter().any(|n| n.starts_with("ggml-cpu-")), "sem dispatch de CPU");
        let sherpa = PACOTES.iter().find(|p| p.id == "sherpa-onnx").unwrap();
        let vozes: Vec<String> = arquivos(sherpa).iter().map(|(n, _)| base(n).to_string()).collect();
        for obrigatorio in
            ["sherpa-onnx-c-api.dll", "onnxruntime.dll", "onnxruntime_providers_shared.dll"]
        {
            assert!(vozes.iter().any(|n| n == obrigatorio), "falta {obrigatorio}");
        }
    }

    #[test]
    fn toda_soma_e_um_hash_sha256() {
        for p in PACOTES {
            let itens = arquivos(p);
            assert!(!itens.is_empty(), "{} sem somas", p.id);
            for (nome, hash) in itens {
                assert_eq!(hash.len(), 64, "{nome}: {hash}");
                assert!(hash.chars().all(|c| c.is_ascii_hexdigit()), "{nome}: {hash}");
                assert!(nome.starts_with("lib/"), "{nome}");
            }
        }
    }

    /// O zip põe os DLLs na raiz; o tar põe sob a pasta da release. Errar este
    /// caminho é "arquivo fora do pacote" com o pacote inteiro na mão.
    #[test]
    fn a_entrada_aponta_para_onde_o_pacote_poem_o_arquivo() {
        let llama = PACOTES.iter().find(|p| p.id == "llama-cpp").unwrap();
        assert_eq!(entrada(llama, "lib/llama.dll"), "llama.dll");
        let sherpa = PACOTES.iter().find(|p| p.id == "sherpa-onnx").unwrap();
        assert_eq!(
            entrada(sherpa, "lib/onnxruntime.dll"),
            "sherpa-onnx-v1.13.8-win-x64-shared-MT-Release-lib/lib/onnxruntime.dll"
        );
    }

    /// Meia pasta não é runtime: a `ggml.dll` carrega os backends da pasta de
    /// onde ela foi aberta, então o conjunto tem de estar junto.
    #[test]
    fn pasta_pela_metade_nao_e_runtime() {
        let raiz = temporaria("metade");
        std::fs::write(raiz.join("llama.dll"), b"x").unwrap();
        std::fs::write(raiz.join("ggml.dll"), b"x").unwrap();
        let estados = estado(&[raiz.clone()]);
        let llama = estados.iter().find(|e| e.id == "llama-cpp").unwrap();
        assert!(!llama.completo);
        assert!(llama.faltando.iter().any(|n| n == "ggml-base.dll"));
        assert!(llama.faltando.iter().any(|n| n == "ggml-vulkan.dll"));
        assert!(!llama.faltando.iter().any(|n| n == "llama.dll"));
        let _ = std::fs::remove_dir_all(&raiz);
    }

    #[test]
    fn o_conjunto_inteiro_numa_pasta_e_runtime() {
        let raiz = temporaria("inteiro");
        for p in PACOTES {
            for (nome, _) in arquivos(p) {
                std::fs::write(raiz.join(base(&nome)), b"x").unwrap();
            }
        }
        let estados = estado(&[raiz.clone()]);
        assert!(estados.iter().all(|e| e.completo), "{estados:?}");
        assert!(estados.iter().all(|e| e.faltando.is_empty()));
        assert_eq!(estados[0].pasta.as_deref(), Some(raiz.display().to_string().as_str()));
        let _ = std::fs::remove_dir_all(&raiz);
    }

    /// O defeito medido nesta máquina: os 19 DLLs do llama.cpp estavam na pasta
    /// do executável, e a faixa dizia "incompleto" porque a licença do pacote
    /// não estava lá. Licença não carrega backend — quem procura runtime pergunta
    /// por biblioteca.
    #[test]
    fn a_licenca_fora_nao_deixa_o_runtime_incompleto() {
        let llama = PACOTES.iter().find(|p| p.id == "llama-cpp").unwrap();
        let raiz = temporaria("sem-licenca");
        let bibliotecas = bibliotecas(llama);
        assert_eq!(
            bibliotecas.len() + 1,
            arquivos(llama).len(),
            "o filtro cortou mais que a licença: sobraram {} de {} arquivos",
            bibliotecas.len(),
            arquivos(llama).len()
        );
        for (nome, _) in &bibliotecas {
            std::fs::write(raiz.join(base(nome)), b"x").unwrap();
        }
        assert!(!raiz.join("LICENSE-LLVM-OpenMP").exists());
        let estado = estado(&[raiz.clone()])
            .into_iter()
            .find(|e| e.id == "llama-cpp")
            .unwrap();
        assert!(estado.completo, "faltando: {:?}", estado.faltando);
        assert!(estado.faltando.is_empty());
        assert!(
            estado.arquivos.iter().all(|n| n.ends_with(".dll")),
            "a tela de estado lista o que não é biblioteca: {:?}",
            estado.arquivos
        );
        // A conferência de hashes faz a mesma pergunta. Ela não pode devolver
        // "19 de 20 batem" numa máquina que tem todas as bibliotecas certas.
        let auditoria = verificar(&[raiz.clone()])
            .into_iter()
            .find(|v| v.id == "llama-cpp")
            .unwrap();
        assert_eq!(auditoria.arquivos.len(), bibliotecas.len());
        assert!(
            auditoria.arquivos.iter().all(|a| a.nome.ends_with(".dll") && a.presente),
            "{:?}",
            auditoria.arquivos.iter().map(|a| &a.nome).collect::<Vec<_>>()
        );
        let _ = std::fs::remove_dir_all(&raiz);
    }

    /// A ordem das pastas é a do carregador: a primeira que tem o conjunto
    /// inteiro vence, mesmo que uma pasta anterior tenha parte dele.
    #[test]
    fn a_primeira_pasta_completa_vence() {
        let meia = temporaria("ordem-meia");
        let cheia = temporaria("ordem-cheia");
        std::fs::write(meia.join("llama.dll"), b"x").unwrap();
        for p in PACOTES {
            for (nome, _) in arquivos(p) {
                std::fs::write(cheia.join(base(&nome)), b"x").unwrap();
            }
        }
        let estados = estado(&[meia.clone(), cheia.clone()]);
        let llama = estados.iter().find(|e| e.id == "llama-cpp").unwrap();
        assert!(llama.completo);
        assert_eq!(llama.pasta.as_deref(), Some(cheia.display().to_string().as_str()));
        let _ = std::fs::remove_dir_all(&meia);
        let _ = std::fs::remove_dir_all(&cheia);
    }

    /// Conteúdo errado com nome certo não passa: é exatamente o caso de uma
    /// release trocada por outra com o mesmo nome de arquivo.
    #[test]
    fn verificar_recusa_conteudo_que_nao_bate() {
        let raiz = temporaria("hash");
        let sherpa = PACOTES.iter().find(|p| p.id == "sherpa-onnx").unwrap();
        for (nome, _) in arquivos(sherpa) {
            std::fs::write(raiz.join(base(&nome)), b"nao e a dll").unwrap();
        }
        let vereditos = verificar(&[raiz.clone()]);
        let voz = vereditos.iter().find(|v| v.id == "sherpa-onnx").unwrap();
        assert!(!voz.conferido);
        assert!(voz.arquivos.iter().all(|a| a.presente && !a.bateu));
        assert!(voz.arquivos.iter().all(|a| a.medido.len() == 64));
        let _ = std::fs::remove_dir_all(&raiz);
    }

    #[test]
    fn verificar_diz_o_que_falta() {
        let raiz = temporaria("ausente");
        let vereditos = verificar(&[raiz.clone()]);
        assert!(vereditos.iter().all(|v| !v.conferido));
        assert!(vereditos.iter().all(|v| v.arquivos.iter().all(|a| !a.presente && !a.bateu)));
        let _ = std::fs::remove_dir_all(&raiz);
    }

    #[test]
    fn o_nome_do_asset_e_o_da_url() {
        assert_eq!(
            nome_do_asset(PACOTES[0].url),
            "llama-b12387-bin-win-vulkan-x64.zip"
        );
        assert_eq!(
            nome_do_asset(PACOTES[1].url),
            "sherpa-onnx-v1.13.8-win-x64-shared-MT-Release-lib.tar.bz2"
        );
    }

    /// A prova de que o instalador instala o que este código foi testado em
    /// executar: baixa os dois pacotes oficiais e confere cada hash contra as
    /// somas versionadas. Ignorado porque são 46 MB de rede; rodar com
    /// `cargo test --lib runtime -- --ignored`.
    #[test]
    #[ignore = "baixa 46 MB dos releases oficiais"]
    fn o_pacote_oficial_bate_com_as_somas() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let raiz = temporaria("oficial");
        let cancel = AtomicBool::new(false);
        rt.block_on(instalar(&raiz, &[], &cancel, |_| {})).unwrap();
        let vereditos = verificar(&[raiz.clone()]);
        for v in &vereditos {
            assert!(v.conferido, "{}: {:?}", v.id, v.arquivos);
        }
        // Instalado, o conjunto tem de ser reconhecido como completo.
        assert!(estado(&[raiz.clone()]).iter().all(|e| e.completo));
        let _ = std::fs::remove_dir_all(&raiz);
    }
}
