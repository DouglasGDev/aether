//! A base de conhecimento como ferramenta: o assistente escreve a nota e o índice
//! aprende dela na mesma chamada.
//!
//! Não há porta de leitura aqui de propósito: `buscar_memoria` já responde por
//! nota desde que a terceira fonte entrou no índice (`Fonte::Documento`), e duas
//! portas para a mesma busca são duas políticas que um dia divergem — é o que o
//! cabeçalho de `local/memoria.rs` jura e o teste de catálogo cobra. O que só
//! esta porta faz é escrever, e escrever é o efeito que o cartão mostra antes de
//! rodar.
//!
//! A nota não vai para uma pasta concedida: ela vai para `base/`, ao lado de
//! `rag.json`, e é o caminho que o próprio custódio recebeu na abertura. Um
//! segundo lugar onde as notas poderiam morar seria uma segunda verdade.

use super::vista::Vista;
use super::{obrigatorio, texto, Saida};
use crate::ai::base;
use crate::ai::memoria::Custodio;
use crate::idiomas::tr;
use anyhow::Result;
use serde_json::{Map, Value};

pub fn escrever(args: &Map<String, Value>, memoria: &Custodio) -> Result<Saida> {
    let titulo = obrigatorio(args, "titulo")?;
    let corpo = obrigatorio(args, "corpo")?;
    let id = texto(args, "nota");
    // Sem a chave, a decisão do dono sobre a presa continua onde estava; com
    // ela, vale o que ele escreveu, inclusive `false`.
    let fixar = args.get("fixar").and_then(Value::as_bool);
    let raiz = memoria.raiz_base();
    // Lido antes da escrita, e não depois: "nova" é um fato sobre a pasta que
    // deixa de existir no instante em que o arquivo nasce.
    let antes = base::ids(raiz)?;
    let registro = base::escrever(raiz, &titulo, &corpo, id.as_deref(), fixar)?;
    let nova = !antes.iter().any(|existente| *existente == registro.nota.id);
    let sincronia = memoria.sincronizar_base(&base::listar(raiz)?.itens)?;

    let mut texto = tr(
        "base.escrita",
        &[
            ("id", &registro.nota.id),
            ("titulo", &registro.nota.titulo),
            ("bytes", &registro.bytes.to_string()),
            ("notas", &registro.notas.to_string()),
        ],
    );
    texto.push_str(
        if sincronia.pecas > 0 {
            tr(
                "base.aprendeu",
                &[
                    ("pecas", &sincronia.pecas.to_string()),
                    ("tokens", &sincronia.tokens.to_string()),
                    ("ms", &sincronia.ms.to_string()),
                ],
            )
        } else {
            // Zero peça não é zero memória: pode ser a nota já lembrada ou o
            // codificador ausente, e as duas têm resposta na tela de memória.
            tr("base.semPeca", &[("notas", &sincronia.notas.to_string())])
        }
        .as_str(),
    );
    if registro.nota.fixada {
        texto.push_str(tr("base.preso", &[]).as_str());
    }
    let arquivo = format!("{}.{}", registro.nota.id, base::EXTENSAO);
    Ok(Saida::nova(
        texto,
        Vista::Base {
            id: registro.nota.id,
            arquivo,
            titulo: registro.nota.titulo,
            bytes: registro.bytes,
            caracteres: registro.caracteres,
            linhas: registro.linhas,
            notas: registro.notas,
            fixada: registro.nota.fixada,
            nova,
            pecas: sincronia.pecas,
            removidas: sincronia.removidas,
            tokens: sincronia.tokens,
            ms: sincronia.ms,
        },
    ))
}

#[cfg(test)]
mod testes {
    use super::*;
    use crate::ai::memoria::Fonte;
    use crate::local::pastas::Pastas;
    use serde_json::json;
    use std::sync::{Arc, RwLock};

    /// Um custódio com a pasta `base/` apontando para um canto do temp. Sem
    /// codificador instalado, de propósito: o que a ferramenta faz com o arquivo
    /// não depende de placa nenhuma, e é isso que este teste pode provar.
    fn custodio(nome: &str) -> Custodio {
        let raiz = std::env::temp_dir().join(format!("aether-local-base-{nome}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&raiz);
        Custodio::novo(
            &[],
            &raiz.join("modelos"),
            &raiz.join("memoria"),
            raiz.join("rag.json"),
            &raiz.join(base::PASTA),
            Arc::new(RwLock::new(Pastas::default())),
        )
    }

    fn mapa(valor: Value) -> Map<String, Value> {
        valor.as_object().expect("objeto").clone()
    }

    #[test]
    fn a_ferramenta_escreve_no_arquivo_e_devolve_o_que_viu() {
        let memoria = custodio("escreve");
        let Saida { texto, vista, .. } = escrever(
            &mapa(json!({ "titulo": "Regra da casa", "corpo": "não abrir o app" })),
            &memoria,
        )
        .expect("escreveu");
        let Vista::Base { id, arquivo, bytes, notas, nova, .. } = &vista else {
            panic!("o cartão errado: {vista:?}");
        };
        assert_eq!(id, "regra-da-casa");
        assert_eq!(arquivo, "regra-da-casa.md");
        assert!(*nova, "a pasta estava vazia");
        assert_eq!(*notas, 1);
        // O arquivo existe no caminho que o custódio recebeu, e não em outro.
        let gravado = memoria.raiz_base().join(arquivo);
        assert_eq!(bytes, &std::fs::metadata(&gravado).expect("nota no disco").len());
        assert!(std::fs::read_to_string(&gravado).unwrap().contains("não abrir o app"));
        assert!(!texto.is_empty());
        let _ = std::fs::remove_dir_all(memoria.raiz_base());
    }

    /// Escrever duas vezes com o id dito no pedido é a mesma nota: o cartão tem
    /// de dizer `nova: false`, senão a tela anuncia uma nota que não existe.
    #[test]
    fn reescrever_pelo_id_nao_cria_segunda_nota() {
        let memoria = custodio("repetida");
        let primeira = escrever(
            &mapa(json!({ "titulo": "Plano A", "corpo": "velho" })),
            &memoria,
        )
        .unwrap();
        let Vista::Base { id, .. } = &primeira.vista else { panic!() };
        let segunda = escrever(
            &mapa(json!({ "titulo": "Plano B", "corpo": "novo", "nota": id })),
            &memoria,
        )
        .unwrap();
        let Vista::Base { id: segundo_id, nova, notas, .. } = &segunda.vista else { panic!() };
        assert_eq!(segundo_id, id, "o id é a identidade da nota");
        assert!(!nova);
        assert_eq!(*notas, 1);
        let _ = std::fs::remove_dir_all(memoria.raiz_base());
    }

    /// Título em branco é recusa na porta, e não arquivo `.md` sem dono na pasta.
    /// A recusa sai do argumento obrigatório — o corpo da nota só é consultado
    /// depois que há título, e o `base.erro.tituloVazio` da loja é a recusa da
    /// mesma escrita quando ela vem pela tela, onde o campo é texto livre.
    #[test]
    fn titulo_vazio_recusa_sem_deixar_arquivo() {
        let memoria = custodio("vazio");
        let erro = escrever(&mapa(json!({ "titulo": "  ", "corpo": "qualquer" })), &memoria)
            .unwrap_err()
            .to_string();
        assert_eq!(erro, tr("erros.argumentoFaltando", &[("nome", "titulo")]));
        assert_eq!(base::listar(memoria.raiz_base()).unwrap().notas, 0);
        let _ = std::fs::remove_dir_all(memoria.raiz_base());
    }

    /// A nota escrita pela porta do agente é a terceira fonte do índice, e o
    /// caminho que a tela lê para dizer "a base tem N peças" é o mesmo.
    #[test]
    fn a_nota_da_ferramenta_e_lida_pela_mesma_lista_do_estado() {
        let memoria = custodio("lista");
        escrever(&mapa(json!({ "titulo": "Índice", "corpo": "um dois" })), &memoria).unwrap();
        escrever(&mapa(json!({ "titulo": "Outra", "corpo": "tres" })), &memoria).unwrap();
        let lista = memoria.notas().expect("listou");
        assert_eq!(lista.notas, 2);
        // Sem codificador carregado a pasta tem duas notas e o índice não lembra
        // nenhuma — é a distinção que a tela mostra ("a base tem N, a memória sabe
        // de M"), e uma fonte fingindo a outra seria o número torto.
        let estado = memoria.estado(false);
        assert_eq!(estado.notas, 0, "o índice não lembra o que não pôde codificar");
        assert_eq!(estado.trechos_notas, 0);
        assert_ne!(Fonte::Documento, Fonte::Conversa);
        let _ = std::fs::remove_dir_all(memoria.raiz_base());
    }

    /// As chaves que esta família publica. Metade delas chega ao `tr` em uma linha
    /// só e o varredor de `idiomas.rs:307` as acha sozinho; a outra metade é
    /// montada em tempo de execução por `ferramenta()` e `campo()` (`format!`), ou
    /// escrita em várias linhas, e cai fora da varredura. Para essas o censo
    /// abaixo é o único censo que existe — e o teste é o que impede ele de ficar
    /// velho quando alguém renomear um argumento ou uma recusa.
    const CHAVES: &[&str] = &[
        "base.escrita",
        "base.aprendeu",
        "base.semPeca",
        "base.preso",
        "base.erro.idInvalido",
        "base.erro.leitura",
        "base.erro.criarPasta",
        "base.erro.tituloVazio",
        "base.erro.corpoGrande",
        "base.erro.notaAusente",
        "base.erro.escrever",
        "base.erro.medir",
        "base.erro.escreverTemporario",
        "base.erro.apagar",
        "ferramentas.escreverNaBase.titulo",
        "ferramentas.escreverNaBase.descricao",
        "argumentos.titulo",
        "argumentos.tituloDescricao",
        "argumentos.corpo",
        "argumentos.corpoDescricao",
        "argumentos.nota",
        "argumentos.notaDescricao",
        "argumentos.fixar",
        "argumentos.fixarDescricao",
    ];

    #[test]
    fn toda_chave_da_base_tem_texto_em_todo_idioma() {
        for chave in CHAVES {
            assert!(crate::idiomas::tem_texto(chave), "{chave} apareceria na tela como chave");
        }
    }
}
