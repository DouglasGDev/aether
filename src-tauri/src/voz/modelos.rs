//! Catálogo dos modelos de voz: o que fala (Kokoro) e o que escuta (Whisper).
//!
//! Os tamanhos abaixo não são estimativas: são os bytes que o HuggingFace e o
//! release do sherpa-onnx declaram para cada arquivo, conferidos contra o que já
//! está em disco. Um modelo só é considerado baixado quando cada arquivo obrigatório
//! existe com exatamente esse tamanho.

use serde::Serialize;
use std::path::{Path, PathBuf};

/// Para que serve o modelo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Servico {
    /// Texto -> fala.
    Fala,
    /// Microfone -> texto.
    Escuta,
}

/// Como o modelo chega ao disco.
#[derive(Debug, Clone, Copy)]
pub enum Fonte {
    /// Um pacote `.tar.bz2` do release do sherpa-onnx, extraído localmente pelo
    /// `tar` do próprio Windows. É o caminho dos pacotes com muitas centenas de
    /// arquivos, como o `espeak-ng-data` do Kokoro.
    Pacote {
        url: &'static str,
        bytes: u64,
        /// Pasta que o pacote cria lá dentro.
        conteudo: &'static str,
    },
    /// Arquivos individuais do HuggingFace, baixados um a um com retomada.
    Arquivos { repo: &'static str },
}

#[derive(Debug, Clone, Copy)]
pub struct EspecVoz {
    pub id: &'static str,
    /// Nome curto, é o que a lista mostra.
    pub nome: &'static str,
    pub servico: Servico,
    /// Pasta do modelo dentro de `modelos/voz`.
    pub pasta: &'static str,
    pub fonte: Fonte,
    /// Arquivos que precisam existir, com o tamanho exato, para o modelo funcionar.
    pub obrigatorios: &'static [(&'static str, u64)],
}

impl EspecVoz {
    /// Quantos bytes esta entrada do catálogo ocupa quando estiver completa.
    pub fn bytes_ocupados(&self) -> u64 {
        match self.fonte {
            Fonte::Pacote { bytes, .. } => bytes,
            Fonte::Arquivos { .. } => self.obrigatorios.iter().map(|(_, b)| *b).sum(),
        }
    }
}

pub const CATALOGO_VOZ: [EspecVoz; 3] = [
    EspecVoz {
        id: "kokoro-ptbr",
        nome: "Kokoro",
        servico: Servico::Fala,
        pasta: "kokoro-int8-multi-lang-v1_0",
        fonte: Fonte::Pacote {
            url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/kokoro-int8-multi-lang-v1_0.tar.bz2",
            bytes: 132_353_410,
            conteudo: "kokoro-int8-multi-lang-v1_0",
        },
        // Os pesos, as vozes e o dicionário de fonemas do português. Os três
        // últimos são o que o espeak-ng lê para pronunciar pt-br.
        obrigatorios: &[
            ("model.int8.onnx", 114_202_993),
            ("voices.bin", 28_200_960),
            ("tokens.txt", 687),
            ("espeak-ng-data/phontab", 55_796),
            ("espeak-ng-data/phonindex", 39_074),
            ("espeak-ng-data/phondata", 550_424),
            ("espeak-ng-data/phondata-manifest", 21_821),
            ("espeak-ng-data/pt_dict", 67_817),
        ],
    },
    EspecVoz {
        id: "whisper-base",
        nome: "Whisper base",
        servico: Servico::Escuta,
        pasta: "sherpa-onnx-whisper-base",
        fonte: Fonte::Arquivos { repo: "csukuangfj/sherpa-onnx-whisper-base" },
        obrigatorios: &[
            ("base-encoder.int8.onnx", 29_120_534),
            ("base-decoder.int8.onnx", 130_672_026),
            ("base-tokens.txt", 816_730),
        ],
    },
    EspecVoz {
        id: "whisper-tiny",
        nome: "Whisper tiny",
        servico: Servico::Escuta,
        pasta: "sherpa-onnx-whisper-tiny",
        fonte: Fonte::Arquivos { repo: "csukuangfj/sherpa-onnx-whisper-tiny" },
        obrigatorios: &[
            ("tiny-encoder.int8.onnx", 12_937_772),
            ("tiny-decoder.int8.onnx", 89_855_401),
            ("tiny-tokens.txt", 816_730),
        ],
    },
];

/// Uma voz do Kokoro. Os nomes e os ids vêm da tabela de alto-falantes que o
/// próprio modelo carrega na metadados (`id2speaker`): 54 vozes, das quais três
/// são de português.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Voz {
    pub sid: i32,
    pub nome: &'static str,
    /// Chave de idioma para traduzir "feminina"/"masculina" na interface.
    pub genero: &'static str,
}

pub const VOZES_KOKORO: [Voz; 3] = [
    Voz { sid: 42, nome: "Dora", genero: "feminina" },
    Voz { sid: 43, nome: "Alex", genero: "masculina" },
    Voz { sid: 44, nome: "Santa", genero: "masculina" },
];

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusVoz {
    pub id: String,
    pub nome: String,
    pub servico: Servico,
    pub bytes: u64,
    pub baixado: bool,
    /// O que falta, quando não está baixado: arquivo por arquivo.
    pub faltando: Vec<String>,
}

pub fn encontrar(id: &str) -> Option<&'static EspecVoz> {
    CATALOGO_VOZ.iter().find(|e| e.id == id)
}

pub fn modelo_dir(raiz: &Path, espec: &EspecVoz) -> PathBuf {
    raiz.join(espec.pasta)
}

/// O que a fonte manda baixar, como pares (url, destino, bytes).
pub fn arquivos(espec: &'static EspecVoz, raiz: &Path) -> Vec<(String, PathBuf, Option<u64>)> {
    let dir = modelo_dir(raiz, espec);
    match espec.fonte {
        Fonte::Pacote { url, bytes, .. } => {
            vec![(url.to_string(), pacote_dir(raiz).join(nome_pacote(espec)), Some(bytes))]
        }
        Fonte::Arquivos { repo } => espec
            .obrigatorios
            .iter()
            .map(|(arquivo, tam)| {
                (
                    format!("https://huggingface.co/{repo}/resolve/main/{arquivo}"),
                    dir.join(arquivo),
                    Some(*tam),
                )
            })
            .collect(),
    }
}

/// Nome do arquivo de pacote no cache, derivado da pasta do modelo.
pub fn nome_pacote(espec: &EspecVoz) -> String {
    match espec.fonte {
        Fonte::Pacote { .. } => format!("{}.tar.bz2", espec.pasta),
        Fonte::Arquivos { .. } => String::new(),
    }
}

/// Onde os pacotes baixados ficam antes de extrair: um cache, não a pasta do
/// modelo, para não confundir o estado de instalação.
pub fn pacote_dir(raiz: &Path) -> PathBuf {
    raiz.join("_pacotes")
}

pub fn pacote_caminho(raiz: &Path, espec: &EspecVoz) -> PathBuf {
    pacote_dir(raiz).join(nome_pacote(espec))
}

/// Estado de instalação de um modelo de voz, lido do disco de verdade.
pub fn status(raiz: &Path, espec: &'static EspecVoz) -> StatusVoz {
    let dir = modelo_dir(raiz, espec);
    let mut faltando = Vec::new();
    for (arquivo, tam) in espec.obrigatorios {
        let caminho = dir.join(arquivo);
        let medido = std::fs::metadata(&caminho).map(|m| m.len()).unwrap_or(0);
        if medido != *tam {
            faltando.push(arquivo.to_string());
        }
    }
    StatusVoz {
        id: espec.id.to_string(),
        nome: espec.nome.to_string(),
        servico: espec.servico,
        bytes: espec.bytes_ocupados(),
        baixado: faltando.is_empty(),
        faltando,
    }
}

pub fn todos_status(raiz: &Path) -> Vec<StatusVoz> {
    CATALOGO_VOZ.iter().map(|e| status(raiz, e)).collect()
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn catalogo_tem_fala_e_escuta() {
        assert_eq!(CATALOGO_VOZ.len(), 3);
        assert!(CATALOGO_VOZ.iter().any(|e| e.servico == Servico::Fala));
        assert!(CATALOGO_VOZ.iter().any(|e| e.servico == Servico::Escuta));
    }

    #[test]
    fn as_vozes_do_kokoro_sao_de_portugues() {
        // pf_ e pm_ são os prefixos de português na tabela do modelo.
        assert_eq!(VOZES_KOKORO.len(), 3);
        assert!(VOZES_KOKORO.iter().all(|v| (0..54).contains(&v.sid)));
        assert_eq!(VOZES_KOKORO[0].sid, 42);
    }

    #[test]
    fn um_pacote_soma_os_bytes_da_url_e_nao_dos_arquivos() {
        let kokoro = encontrar("kokoro-ptbr").unwrap();
        // O que se baixa é o `.tar.bz2` do release: a barra dele não pode ser a
        // soma dos arquivos extraídos, que é maior.
        let Fonte::Pacote { bytes, url, conteudo } = kokoro.fonte else {
            panic!("o kokoro vem em pacote");
        };
        assert_eq!(bytes, 132_353_410);
        assert_eq!(conteudo, kokoro.pasta);
        assert!(url.ends_with(&nome_pacote(kokoro)));
        let extraido: u64 = kokoro.obrigatorios.iter().map(|(_, b)| *b).sum();
        assert!(
            extraido > bytes,
            "extraído ({extraido}) deveria passar o comprimido ({bytes})"
        );
        assert_eq!(kokoro.bytes_ocupados(), bytes);
        // O Whisper são arquivos: aí o que se baixa é exatamente a soma deles.
        let whisper = encontrar("whisper-tiny").unwrap();
        let soma: u64 = whisper.obrigatorios.iter().map(|(_, b)| *b).sum();
        assert_eq!(whisper.bytes_ocupados(), soma);
        assert_eq!(nome_pacote(whisper), "");
    }

    #[test]
    fn modelo_incompleto_nao_conta_como_baixado() {
        let raiz = std::env::temp_dir().join(format!("aether-voz-{}", std::process::id()));
        let espec = encontrar("whisper-tiny").unwrap();
        let _ = std::fs::remove_dir_all(&raiz);
        assert!(!status(&raiz, espec).baixado);

        let dir = modelo_dir(&raiz, espec);
        std::fs::create_dir_all(dir.join("..")).unwrap();
        for (arquivo, tam) in espec.obrigatorios {
            let alvo = dir.join(arquivo);
            if let Some(pai) = alvo.parent() {
                std::fs::create_dir_all(pai).unwrap();
            }
            std::fs::write(&alvo, vec![0u8; *tam as usize]).unwrap();
        }
        let completo = status(&raiz, espec);
        assert!(completo.baixado, "faltando: {:?}", completo.faltando);

        // Um arquivo truncado derruba o estado de novo.
        let primeiro = dir.join(espec.obrigatorios[0].0);
        let dados = std::fs::read(&primeiro).unwrap();
        std::fs::write(&primeiro, &dados[..dados.len() - 1]).unwrap();
        assert!(!status(&raiz, espec).baixado);
        let _ = std::fs::remove_dir_all(&raiz);
    }

    #[test]
    fn urls_vem_da_fonte_de_cada_formato() {
        let raiz = Path::new("C:/models");
        let kokoro = encontrar("kokoro-ptbr").unwrap();
        let (url, destino, bytes) = &arquivos(kokoro, raiz)[0];
        assert!(url.starts_with("https://github.com/k2-fsa/sherpa-onnx/releases/"));
        assert_eq!(*bytes, Some(132_353_410));
        assert_eq!(
            destino,
            &raiz.join("_pacotes").join("kokoro-int8-multi-lang-v1_0.tar.bz2")
        );

        let whisper = encontrar("whisper-base").unwrap();
        let lista = arquivos(whisper, raiz);
        assert_eq!(lista.len(), 3);
        assert!(lista[0].0.starts_with(
            "https://huggingface.co/csukuangfj/sherpa-onnx-whisper-base/resolve/main/"
        ));
        assert_eq!(lista[0].2, Some(29_120_534));
    }
}
