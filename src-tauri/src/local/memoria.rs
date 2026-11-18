//! A memória como ferramenta: o assistente pergunta e o índice local responde.
//!
//! É exatamente a mesma busca que a injeção automática do prompt usa, pela mesma
//! porta: o fio do grafo que liga `buscar_memoria` é o que autoriza as duas, e
//! nenhum byte desta chamada sai da máquina.

use super::vista::{TrechoMemoria, Vista};
use super::{Saida, inteiro, obrigatorio};
use crate::ai::memoria::{Achado, Custodio, Resposta, TRECHOS_MAX};
use crate::idiomas::tr;
use anyhow::Result;
use serde_json::{Map, Value};

/// O cartão e o texto do modelo saem da mesma lista medida: o que a interface
/// mostra como proximidade é o número que classificou o trecho, não uma nota
/// escrita depois.
fn trecho(achado: &Achado) -> TrechoMemoria {
    TrechoMemoria {
        caminho: achado.arquivo.clone(),
        nome: achado.nome.clone(),
        linha: achado.linha,
        texto: achado.texto.clone(),
        pontuacao: achado.pontuacao as f64,
        origem: achado.origem,
        fonte: achado.fonte,
        quando: achado.quando,
    }
}

pub fn buscar(args: &Map<String, Value>, memoria: &Custodio) -> Result<Saida> {
    let consulta = obrigatorio(args, "consulta")?;
    // O pedido do modelo é um teto, e não uma ordem: o que ele escreveu maior
    // que `TRECHOS_MAX` volta cortado ao máximo que o índice entrega.
    let pedido = inteiro(args, "trechos").map(|n| (n as u32).min(TRECHOS_MAX));
    // A conversa em curso não é ignorada aqui, e é na injeção do prompt. Quando o
    // modelo chama esta ferramenta, a pergunta pode justamente ser sobre o que a
    // janela dele já perdeu.
    let Resposta { achados, alcancados, modo } =
        memoria.buscar(&consulta, pedido.unwrap_or(0), &[])?;
    let texto = if achados.is_empty() {
        tr("memoria.nadaEncontrado", &[])
    } else {
        Custodio::texto_do_bloco(&achados)
    };
    Ok(Saida::nova(
        texto,
        Vista::Memoria {
            consulta,
            modo,
            total: alcancados,
            truncado: alcancados > achados.len(),
            achados: achados.iter().map(trecho).collect(),
        },
    ))
}

#[cfg(test)]
mod testes {
    use super::*;
    use crate::local::pastas::Pastas;
    use serde_json::Value;
    use std::path::PathBuf;
    use std::sync::{Arc, RwLock};

    /// Um custódio sem nada instalado, numa pasta só deste teste. O segundo
    /// valor é a pasta: o teste a apaga quando termina.
    fn custodio(nome: &str) -> (Custodio, PathBuf) {
        let raiz = std::env::temp_dir().join(format!(
            "aether-ferr-memoria-{nome}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&raiz).unwrap();
        let guardado = raiz.clone();
        (
            Custodio::novo(
                &[],
                &raiz.join("modelos"),
                &raiz.join("memoria"),
                raiz.join("rag.json"),
                Arc::new(RwLock::new(Pastas::default())),
            ),
            guardado,
        )
    }

    fn consulta(texto: &str) -> Map<String, Value> {
        let mut mapa = Map::new();
        mapa.insert("consulta".to_string(), Value::String(texto.to_string()));
        mapa
    }

    /// A ferramenta existe para ser chamada pelo modelo, e a recusa tem de vir
    /// com o motivo real — nunca com um resultado inventado para a tela ficar
    /// bonita.
    #[test]
    fn sem_codigo_instalado_a_busca_recusa_com_o_motivo() {
        let (memoria, raiz) = custodio("sem-modelo");
        let erro = buscar(&consulta("como ligar a placa"), &memoria)
            .unwrap_err()
            .to_string();
        assert_eq!(erro, tr("erros.memoriaSemModelo", &[]));
        std::fs::remove_dir_all(&raiz).ok();
    }

    #[test]
    fn uma_consulta_em_branco_e_recusada_antes_de_mexer_no_motor() {
        let (memoria, raiz) = custodio("em-branco");
        assert!(buscar(&consulta("   "), &memoria).is_err());
        std::fs::remove_dir_all(&raiz).ok();
    }

    #[test]
    fn sem_consulta_nao_ha_o_que_procurar() {
        let (memoria, raiz) = custodio("sem-consulta");
        let mut mapa = Map::new();
        mapa.insert("trechos".to_string(), Value::from(3));
        let erro = buscar(&mapa, &memoria).unwrap_err().to_string();
        assert!(erro.contains("consulta"), "{erro}");
        std::fs::remove_dir_all(&raiz).ok();
    }
}
