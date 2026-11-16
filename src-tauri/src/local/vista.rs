//! O que a interface desenha no lugar de um despejo técnico.
//!
//! Cada variante é derivada do resultado real da ferramenta: nada aqui é
//! estimado nem inventado. Uma ferramenta sem vista (um servidor MCP qualquer)
//! continua sendo mostrada como texto.

use serde::Serialize;

use crate::ai::memoria::Origem;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entrada {
    pub nome: String,
    pub pasta: bool,
    pub bytes: u64,
    /// Milissegundos desde 1970; a interface formata na data do idioma escolhido.
    pub modificado: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Achado {
    pub caminho: String,
    pub nome: String,
    pub linha: usize,
    pub texto: String,
}

/// Um trecho que a memória devolve. Diferente de um achado de busca de texto
/// porque aqui o número que o coloca na lista é uma distância medida entre dois
/// vetores — mostrá-lo como se fosse uma correspondência literal seria um resultado
/// falso.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrechoMemoria {
    pub caminho: String,
    pub nome: String,
    pub linha: usize,
    pub texto: String,
    /// Proximidade do cosseno do trecho com a pergunta, entre 0 e 1.
    pub pontuacao: f64,
    /// O que trouxe o trecho: o vetor, a palavra escrita ou os dois. É a mesma
    /// origem que a tela de memória mostra, para o cartão da conversa não
    /// esconder como o trecho entrou.
    pub origem: Origem,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Troca {
    pub antes: String,
    pub depois: String,
    pub linha: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Par {
    pub rotulo: String,
    /// Texto já pronto, que também vai ao modelo.
    pub valor: String,
    /// Quando a linha é uma medida, o número cru vai junto: a interface formata
    /// com o separador decimal do idioma escolhido, em vez de receber um número
    /// formatado para outro idioma.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub numero: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unidade: Option<String>,
}

impl Par {
    pub fn texto(rotulo: impl Into<String>, valor: impl Into<String>) -> Self {
        Self { rotulo: rotulo.into(), valor: valor.into(), numero: None, unidade: None }
    }

    pub fn medida(rotulo: impl Into<String>, numero: f64, unidade: impl Into<String>) -> Self {
        let unidade = unidade.into();
        Self {
            rotulo: rotulo.into(),
            valor: format!("{numero:.1} {unidade}"),
            numero: Some(numero),
            unidade: Some(unidade),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Vista {
    Arquivo {
        caminho: String,
        nome: String,
        linhas: usize,
        bytes: u64,
        previa: String,
        linguagem: String,
        inicio: usize,
        truncado: bool,
    },
    Lista {
        caminho: String,
        entradas: Vec<Entrada>,
        total: usize,
        truncado: bool,
    },
    Busca {
        termo: String,
        caminho: String,
        achados: Vec<Achado>,
        total: usize,
        truncado: bool,
    },
    /// A memória recuperada. É variante própria, e não um reaproveitamento
    /// de `busca`, porque o critério é outro: a ordem sai de uma medida entre
    /// vetores e de uma lista de palavras, não do texto igual.
    Memoria {
        consulta: String,
        /// O modo que ordenou a lista, em palavra de máquina.
        modo: &'static str,
        achados: Vec<TrechoMemoria>,
        total: usize,
        truncado: bool,
    },
    Escrita {
        caminho: String,
        nome: String,
        bytes: u64,
        linhas: usize,
        criado: bool,
    },
    Edicao {
        caminho: String,
        nome: String,
        trocas: Vec<Troca>,
        aplicadas: usize,
    },
    Pasta {
        caminho: String,
        nome: String,
    },
    Movido {
        origem: String,
        destino: String,
        nome: String,
    },
    Apagado {
        caminho: String,
        nome: String,
        pasta: bool,
    },
    Comando {
        comando: String,
        pasta: String,
        codigo: Option<i32>,
        saida: String,
        erro: String,
        duracao_ms: u64,
        estourou: bool,
        truncado: bool,
    },
    Sistema {
        linhas: Vec<Par>,
    },
}

impl Vista {
    pub fn para_json(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("vista é sempre serializável")
    }
}

/// Extensão do arquivo, para o cartão mostrar de que tipo de código se trata.
pub fn linguagem(caminho: &std::path::Path) -> String {
    caminho
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

pub fn nome(caminho: &std::path::Path) -> String {
    caminho
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| caminho.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_vista_chega_na_interface_com_os_campos_em_camel_case() {
        let vista = Vista::Comando {
            comando: "git status".into(),
            pasta: "C:/projeto".into(),
            codigo: Some(0),
            saida: "nada a fazer".into(),
            erro: String::new(),
            duracao_ms: 42,
            estourou: false,
            truncado: false,
        };
        let json = vista.para_json();
        assert_eq!(json["type"], "comando");
        assert_eq!(json["duracaoMs"], 42);
        assert_eq!(json["codigo"], 0);
        assert!(json.get("duracao_ms").is_none());
    }

    #[test]
    fn cada_variante_tem_um_rotulo_de_uma_palavra_so() {
        let vistas = [
            Vista::Pasta { caminho: "a".into(), nome: "a".into() },
            Vista::Movido { origem: "a".into(), destino: "b".into(), nome: "a".into() },
            Vista::Apagado { caminho: "a".into(), nome: "a".into(), pasta: false },
            Vista::Sistema { linhas: vec![Par::texto("r", "v")] },
        ];
        for vista in vistas {
            let rotulo = vista.para_json()["type"].as_str().expect("rótulo textual").to_string();
            assert!(!rotulo.is_empty());
            assert!(!rotulo.contains(char::is_whitespace), "{rotulo}");
        }
    }

    #[test]
    fn a_extensao_vira_linguagem_em_minusculas() {
        assert_eq!(linguagem(std::path::Path::new("C:/x/Main.RS")), "rs");
        assert_eq!(linguagem(std::path::Path::new("C:/x/Makefile")), "");
    }

    #[test]
    fn uma_medida_manda_o_numero_cru_para_a_interface_formatar() {
        let vista = Vista::Sistema { linhas: vec![Par::medida("Memória", 31.94, "GiB")] };
        let json = vista.para_json();
        let linha = &json["linhas"][0];
        assert_eq!(linha["numero"], 31.94);
        assert_eq!(linha["unidade"], "GiB");
        assert_eq!(linha["valor"], "31.9 GiB");
    }

    #[test]
    fn um_texto_comum_nao_carrega_numero() {
        let json = Vista::Sistema { linhas: vec![Par::texto("Núcleos", "16")] }.para_json();
        assert!(json["linhas"][0].get("numero").is_none());
        assert_eq!(json["linhas"][0]["valor"], "16");
    }
}