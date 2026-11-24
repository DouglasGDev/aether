//! Onde o modelo vai morar, medido antes de ele ser carregado.
//!
//! A pergunta que este módulo responde é a única que decide a velocidade de uma
//! conversa: os pesos cabem na placa? A resposta sai de três medidas somadas —
//! os bytes de tensor do próprio arquivo, o KV cache da janela pedida e a folga
//! que a biblioteca reserva para os buffers de computação — contra a VRAM livre
//! que a ggml respondeu agora.
//!
//! Nada aqui é chutado, e as duas contas que parecem fórmula têm prova:
//!
//! - **KV cache**: a soma, camada a camada, de `cabeças_kv * (dimensão_de_k +
//!   dimensão_de_v) * 2 bytes` — os 2 bytes são o f16 com que a biblioteca escreve
//!   K e V por padrão (`type_k`/`type_v` em `llama_context_default_params`). O
//!   que cada camada tem vem do cabeçalho dela, pelas mesmas chaves e os mesmos
//!   padrões do b12387 vendorizado (`camadas_de_cache` abaixo). No granite-4.2-3b
//!   Q4_K_M, onde todas as 40 camadas são iguais, isso dá 81.920 bytes por token
//!   e, com janela de 4096, 335.544.320 bytes — e a biblioteca escreveu
//!   `llama_kv_cache: size = 320.00 MiB (4096 cells, 40 layers, 1/1 seqs), K
//!   (f16): 160.00 MiB, V (f16): 160.00 MiB`: 320,00 MiB é exatamente
//!   335.544.320 / 1048576. O caso que não é uniforme é o gemma-4-12b-it, onde a
//!   conta antiga nem de perto passava: `gemma4.attention.head_count_kv` é um
//!   array de 48 inteiros (8 nas camadas de janela deslizante, 1 nas de atenção
//!   cheia) e as dimensões de cabeça são 256 em umas e 512 nas outras.
//! - **bytes de tensor**: `elementos / bloco * tamanho_do_bloco`, com o cabeçalho
//!   lido por [`crate::ai::cabecalho`] e o bloco de cada tipo medido nos arquivos
//!   oferecidos, publicado em `cabecalho::BLOCOS`. A soma sai de
//!   `Cabecalho::bytes_por_tensor`, e um tipo sem medida é recusa que nomeia o
//!   tensor — não um zero na soma. Somando os tensores do mesmo arquivo dá
//!   2136,65 MiB, e a carga respondeu `Vulkan0 model buffer size = 1998.84 MiB`
//!   mais `CPU_Mapped model buffer size = 137.81 MiB` — os mesmos 2136,65 MiB.

use super::cabecalho::{self, Valor};
use super::ggml;
use crate::idiomas::tr;
use anyhow::{Result, bail};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// O que a biblioteca reserva além dos pesos, do KV e do estado: os buffers de
/// computação do grafo, por onde passam os ativos. Não é um chute — é o
/// `sched_reserve: Vulkan0 compute buffer size` que a própria biblioteca escreve
/// ao carregar, lido em treze cargas desta placa (RX 9060 XT, driver AMD, flash
/// attention ligada), com dois arquivos e sete janelas:
///
/// | janela | granite-4.2-3b Q4_K_M | ornith-1.5-9b Q4_K_M |
/// | --- | --- | --- |
/// | 4.096 | 220,02 MiB | 521,02 MiB |
/// | 32.768 | 248,02 MiB | 549,02 MiB |
/// | 49.152 | 264,02 MiB | 565,02 MiB |
/// | 61.440 | 276,02 MiB | 577,02 MiB |
/// | 65.536 | 280,02 MiB | 581,02 MiB |
/// | 131.072 | 344,02 MiB | 645,02 MiB |
/// | 262.144 | — | 773,02 MiB |
///
/// Os dois crescem exatos 1.024 bytes por célula de janela: `216,02 + janela /
/// 1024` fecha as seis linhas da densa e `517,02 + janela / 1024` fecha as sete
/// da híbrida, com diferença zero em todas — a parte proporcional é medida, não
/// estimada. O piso, porém, é do grafo de cada arquitetura (a híbrida paga o
/// dobro: as camadas recorrentes atravessam o grafo com estados que a densa não
/// tem), e nada no cabeçalho autoriza adivinhar o piso de um arquivo que ainda
/// não rodou. O que esta função cobra é o **maior piso medido, arredondado para
/// cima**, nunca o piso daquele arquivo: cobrir o piso menor deixa folga
/// sobrando, cobrir o maior a menos seria prometer à pessoa uma placa mais gorda
/// do que ela é — o erro que este módulo não pode cometer.
const PISO_DE_COMPUTACAO: u64 = 518 * 1024 * 1024;

/// Os 1.024 bytes por célula de janela que os treze pontos acima medem nos dois
/// arquivos, no mesmo arredondamento de célula que o KV paga.
const CRESCIMENTO_POR_CELULA: u64 = 1024;

/// A folga do grafo para uma janela já arredondada em células — a mesma
/// [`celulas_da_janela`] que a conta do KV usa, para que as duas cresçam com o
/// que a biblioteca realmente aloca, não com o número que a pessoa digitou.
pub fn folga_de_computacao(celulas: u64) -> u64 {
    PISO_DE_COMPUTACAO.saturating_add(CRESCIMENTO_POR_CELULA.saturating_mul(celulas))
}

/// A placa que a ggml escolheria agora, com o que ela tem de livre neste
/// instante. `None` é a resposta honesta de "esta máquina não tem placa".
#[derive(Debug, Clone)]
pub struct Capacidade {
    pub placa: String,
    pub livre: u64,
    pub total: u64,
}

/// Lê o registro de dispositivos e aplica a mesma preferência do motor: a
/// dedicada de maior memória. É a mesma função que a carga usa, então o que a
/// tela anuncia e o que roda são a mesma placa por construção, não por sorte.
pub fn capacidade(dlls: &[PathBuf]) -> Result<Option<Capacidade>> {
    let dispositivos = ggml::registro(dlls).map_err(anyhow::Error::msg)?;
    let Some(escolhida) = ggml::escolher(&dispositivos) else {
        return Ok(None);
    };
    let bruto = &dispositivos[escolhida.indice];
    Ok(Some(Capacidade {
        placa: escolhida.nome.clone(),
        livre: bruto.livre as u64,
        total: bruto.total as u64,
    }))
}

/// O que o cabeçalho de um GGUF diz sobre o tamanho das coisas. Os tensores não
/// são lidos: só as fichas deles, que é onde estão a forma e o dtype.
#[derive(Debug, Clone)]
pub struct Ficha {
    pub arquivo: String,
    pub arquitetura: String,
    pub camadas: usize,
    /// Bytes de todos os tensores do arquivo, somados do dtype e da forma.
    pub pesos_total: u64,
    /// Bytes de cada `blk.N.*`, por índice de camada. Denso e MoE têm a mesma
    /// entrada aqui: quem difere entre camadas difere na lista, não na conta.
    pub camada: Vec<u64>,
    /// O que não pertence a nenhuma camada: tabela de tokens, cabeça de saída,
    /// normalizações. Fica do lado de fora da conta de camadas porque é isso que
    /// a biblioteca faz quando a placa aperta.
    pub fixo: u64,
    /// Bytes de KV cache por token, no dtype que o flash attention usa. Só as
    /// camadas que abrem cache entram aqui; o que uma camada de atenção linear
    /// guarda não cresce com a janela e está em `estado_recorrente`.
    pub kv_por_token: u64,
    /// Bytes do estado recorrente das camadas de atenção linear, para uma
    /// sequência. É constante: 0 num modelo denso, e no `Qwen3.5-35B-A3B` os
    /// 62,8 MiB das suas 30 camadas de gated delta net.
    pub estado_recorrente: u64,
    /// A janela que o arquivo declara — o teto do que o modelo aceita.
    pub janela_maxima: usize,
    /// O template de conversa que o próprio arquivo publica. É o que a biblioteca
    /// usaria ao renderizar o prompt; o aplicativo usa o mesmo, porque um
    /// template de outro lugar — o repositório de pesos, por exemplo — pode ser
    /// outra versão, e aí a gramática de chamada de ferramenta que o modelo
    /// aprendeu não é a que ele está sendo convidado a falar.
    pub template: Option<String>,
    /// Os ids que encerram uma resposta, lidos do arquivo: `eos_token_id` e,
    /// quando o modelo separa os dois, `eot_token_id`.
    pub eos: Vec<u32>,
    /// Os mesmos ids escritos como tokens, que é a forma que a geração observa
    /// como sequência de parada. Vêm do vocabulário do próprio arquivo, não de
    /// uma lista de marcadores supostos.
    pub paradas: Vec<String>,
}

fn inteiro(metadata: &HashMap<String, Valor>, chave: &str) -> Option<u32> {
    match metadata.get(chave) {
        Some(Valor::U32(v)) => Some(*v),
        Some(Valor::U64(v)) => u32::try_from(*v).ok(),
        Some(Valor::I32(v)) if *v >= 0 => Some(*v as u32),
        _ => None,
    }
}

fn lista_de_textos<'a>(
    metadata: &'a HashMap<String, Valor>,
    chave: &str,
) -> Option<&'a [Valor]> {
    match metadata.get(chave) {
        Some(Valor::Array(itens)) => Some(itens),
        _ => None,
    }
}

/// O que o arquivo diz sobre como falar com o modelo: template, fins de
/// resposta e os tokens que os nomeiam. Um arquivo sem vocabulário escrito nele
/// devolve as partes que tem — quem chama decide o que fazer com a ausência.
fn vocabulario(
    metadata: &HashMap<String, Valor>,
) -> (Option<String>, Vec<u32>, Vec<String>) {
    let template = match metadata.get("tokenizer.chat_template") {
        Some(Valor::String(t)) if !t.is_empty() => Some(t.clone()),
        _ => None,
    };
    let mut eos: Vec<u32> = Vec::new();
    for chave in ["tokenizer.ggml.eos_token_id", "tokenizer.ggml.eot_token_id"] {
        if let Some(id) = inteiro(metadata, chave) {
            if !eos.contains(&id) {
                eos.push(id);
            }
        }
    }
    let tokens = lista_de_textos(metadata, "tokenizer.ggml.tokens");
    let paradas: Vec<String> = eos
        .iter()
        .filter_map(|id| tokens?.get(*id as usize))
        .filter_map(|v| match v {
            Valor::String(t) if !t.is_empty() => Some(t.clone()),
            _ => None,
        })
        .collect();
    (template, eos, paradas)
}

fn numero(metadata: &HashMap<String, Valor>, chave: &str) -> Result<u64> {
    let valor = metadata
        .get(chave)
        .and_then(|v| if matches!(v, Valor::Array(_)) { None } else { Some(v) })
        .and_then(contagem)
        .ok_or_else(|| anyhow::anyhow!(tr("erros.metadadoAusente", &[("chave", chave)])))?;
    Ok(valor)
}

/// Uma linha do cabeçalho lida como contagem: os inteiros que o GGUF escreve
/// para geometria, e o `Bool` que o array de `sliding_window_pattern` usa.
fn contagem(valor: &Valor) -> Option<u64> {
    match valor {
        Valor::U8(v) => Some(*v as u64),
        Valor::U16(v) => Some(*v as u64),
        Valor::U32(v) => Some(*v as u64),
        Valor::U64(v) => Some(*v),
        Valor::I8(v) if *v >= 0 => Some(*v as u64),
        Valor::I16(v) if *v >= 0 => Some(*v as u64),
        Valor::I32(v) if *v >= 0 => Some(*v as u64),
        Valor::I64(v) if *v >= 0 => Some(*v as u64),
        Valor::Bool(v) => Some(u64::from(*v)),
        _ => None,
    }
}

/// Uma chave de geometria resolvida camada a camada.
///
/// É a regra do motor, lida no `get_key_or_arr` do b12387 vendorizado
/// (`llama-model-loader.cpp:463-501`): um número só vale para todas as camadas,
/// e um array tem de trazer exatamente uma entrada por camada — comprimento que
/// não bate é recusa do arquivo, não aproximação. Os GGUFs de 2026 usam a
/// segunda forma: `gemma4.attention.head_count_kv` são 48 inteiros, 8 nas
/// camadas de janela deslizante e 1 nas de atenção cheia. `None` é a chave
/// ausente, que cada chamada trata do jeito que o motor trata.
fn escalares_por_camada(
    metadata: &HashMap<String, Valor>,
    chave: &str,
    camadas: usize,
) -> Result<Option<Vec<u64>>> {
    let Some(valor) = metadata.get(chave) else {
        return Ok(None);
    };
    match valor {
        Valor::Array(itens) => {
            if itens.len() != camadas {
                return Err(anyhow::anyhow!(tr(
                    "erros.alojamentoArray",
                    &[
                        ("chave", chave),
                        ("esperado", &camadas.to_string()),
                        ("recebido", &itens.len().to_string())
                    ]
                )));
            }
            let lidas: Result<Vec<u64>> = itens
                .iter()
                .map(|item| {
                    contagem(item).ok_or_else(|| {
                        anyhow::anyhow!(tr("erros.metadadoAusente", &[("chave", chave)]))
                    })
                })
                .collect();
            Ok(Some(lidas?))
        }
        outra => Ok(contagem(outra).map(|v| vec![v; camadas])),
    }
}

/// Uma chave de geometria escrita como número só. Um array aqui é a ausência: o
/// motor também ignora array nessas chaves, porque as lê com `get_key`, e a
/// dimensão de cabeça do Gemma 4 é a única forma que ele aceita escrita em
/// escalar (`llama-model.cpp:1621-1648`).
fn escalar(metadata: &HashMap<String, Valor>, chave: &str) -> Option<u64> {
    match metadata.get(chave) {
        Some(valor) if !matches!(valor, Valor::Array(_)) => contagem(valor),
        _ => None,
    }
}

/// Quais camadas atendem à janela deslizante declarada no arquivo.
///
/// O Gemma 4 escreve um array — `get_arr` copia as entradas e deixa o resto como
/// estava, e o resto estava zerado, ou seja: atenção cheia
/// (`llama-model-loader.cpp:365-405`). Sem array, o motor aceita um número, que
/// é o período do padrão: com `dense_first = false`, é deslizante toda camada
/// com `il % n < n - 1`, e `n = 0` torna todas deslizantes
/// (`llama-hparams.cpp:8-18`). Sem nenhuma das duas chaves, nada é deslizante.
fn padrao_deslizante(
    metadata: &HashMap<String, Valor>,
    chave: &str,
    camadas: usize,
) -> Vec<bool> {
    if let Some(Valor::Array(itens)) = metadata.get(chave) {
        let mut marcas = vec![false; camadas];
        for (indice, item) in itens.iter().take(camadas).enumerate() {
            marcas[indice] = contagem(item).unwrap_or(0) != 0;
        }
        return marcas;
    }
    match escalar(metadata, chave) {
        Some(0) => vec![true; camadas],
        Some(periodo) if periodo > 1 => (0..camadas)
            .map(|il| (il as u64) % periodo < periodo - 1)
            .collect(),
        Some(_) => vec![false; camadas],
        None => vec![false; camadas],
    }
}

/// O que uma camada aloja de cache de atenção, dito pelo cabeçalho dela.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CamadaKv {
    /// Cabeças de KV da camada. É por elas, e não pelas cabeças de consulta,
    /// que o cache é dimensionado: é o ganho do GQA.
    cabecas: u64,
    /// Elementos por célula, de K e de V. Uma camada de janela deslizante os lê
    /// de `key_length_swa`/`value_length_swa`, e no Gemma 4 são 256 contra os
    /// 512 das camadas de atenção cheia.
    k: u64,
    v: u64,
    /// `false` é a camada que não aloja nada: ela lê o cache de outra, que é o
    /// que `attention.shared_kv_layers` declara para as últimas N camadas
    /// (`src/models/gemma4.cpp:11-18` do b12387).
    propria: bool,
    /// `true` é a camada de atenção linear: ela não abre cache de KV nenhum,
    /// porque o que ela guarda é um estado recorrente de tamanho fixo —
    /// `estado_recorrente` abaixo. A camada continua existindo e continuando
    /// paga pelos próprios pesos.
    recorrente: bool,
}

/// A parte recorrente da memória de um cabeçalho: quais camadas são de atenção
/// linear e quanto custa o estado de uma delas.
pub struct MemoriaRecurrente {
    recorrentes: Vec<bool>,
    /// Bytes do estado (`cache_r` + `cache_s`) de uma camada recorrente, para uma
    /// sequência. `0` é o arquivo que não tem camada recorrente alguma.
    bytes_por_camada: u64,
}

/// Camadas recorrentes, como o motor as marca.
///
/// Um arquivo só tem camada recorrente quando declara as chaves do gated
/// delta net — `ssm.conv_kernel`, `ssm.inner_size`, `ssm.state_size` e
/// `ssm.group_count` — que `qwen35.cpp:9-13` e `qwen35moe.cpp:11-15` lêem como
/// obrigatórias. Sem elas a arquitetura não é híbrida e nenhuma camada deixa de
/// pagar KV, o que vale para o granite e para o gemma4 como vale para qualquer
/// denso: a regra é lida do arquivo, não de uma lista de arquiteturas.
///
/// Tendo-as, o padrão vem de `attention.recurrent_layers` (um array, ou um
/// número para todas) e, na falta dela, do período `full_attention_interval`,
/// cujo padrão ausente é 4: recorrente é a camada em que `(il + 1) % intervalo
/// != 0`, ou seja, a última de cada grupo é a única com atenção
/// (`qwen35.cpp:17-22`, `qwen35moe.cpp:19-25`). Medido no censo de tensores do
/// `Qwen3.5-35B-A3B-UD-Q4_K_XL`: 40 camadas, 10 com `attn_k.weight`, 30 com
/// `ssm_conv1d.weight`.
///
/// O estado é o que a biblioteca aloja por camada recorrente, em f32
/// (`GGML_TYPE_F32` para R e S em `llama-model.cpp:3368-3369`), com uma linha por
/// sequência e `n_rs_seq = 0` (`llama-context.cpp:4421`, e o aplicativo não toca
/// nele): `n_embd_r = (conv - 1) × (inner + 2 × grupo × estado)` e
/// `n_embd_s = estado × inner` (`llama-hparams.cpp:229` e `:257`). No A3B são
/// 24.576 + 524.288 elementos = 2.195.456 bytes por camada, 62,8 MiB pelas 30 —
/// constante com a janela, e é por isso que ele não entra na taxa por token.
pub(crate) fn memoria_recorrente(
    metadata: &HashMap<String, Valor>,
    prefixo: &str,
    camadas: usize,
) -> Result<MemoriaRecurrente> {
    let chave = |s: &str| format!("{prefixo}{s}");
    let algumas = |nomes: &[&str]| nomes.iter().all(|n| metadata.contains_key(&chave(n)));
    let do_gdn = [
        "ssm.conv_kernel",
        "ssm.inner_size",
        "ssm.state_size",
        "ssm.group_count",
    ];
    if !algumas(&do_gdn) {
        return Ok(MemoriaRecurrente { recorrentes: vec![false; camadas], bytes_por_camada: 0 });
    }
    let recorrentes = match escalares_por_camada(metadata, &chave("attention.recurrent_layers"), camadas)? {
        Some(marcas) => marcas.into_iter().map(|v| v != 0).collect(),
        None => {
            let intervalo = escalar(metadata, &chave("full_attention_interval")).unwrap_or(4);
            (0..camadas)
                .map(|il| if intervalo == 0 { true } else { (il as u64 + 1) % intervalo != 0 })
                .collect()
        }
    };
    let conv = numero(metadata, &chave("ssm.conv_kernel"))?;
    let inner = numero(metadata, &chave("ssm.inner_size"))?;
    let estado = numero(metadata, &chave("ssm.state_size"))?;
    let grupo = numero(metadata, &chave("ssm.group_count"))?;
    // `checked` aqui é o que separa um número honesto de um cabeçalho fabricado
    // que passaria a cobrar estado de uma camada que não existe.
    let elementos = conv
        .checked_sub(1)
        .and_then(|menos| menos.checked_mul(inner.checked_add(2u64.checked_mul(grupo)?.checked_mul(estado)?)?))
        .and_then(|r| r.checked_add(estado.checked_mul(inner)?))
        .ok_or_else(|| anyhow::anyhow!(tr("erros.alojamentoEstouro", &[])))?;
    Ok(MemoriaRecurrente {
        recorrentes,
        bytes_por_camada: elementos
            .checked_mul(4)
            .ok_or_else(|| anyhow::anyhow!(tr("erros.alojamentoEstouro", &[])))?,
    })
}

/// Bytes de estado recorrente que a carga abre: nada quando o arquivo é denso.
pub(crate) fn estado_recorrente(
    metadata: &HashMap<String, Valor>,
    prefixo: &str,
    camadas: usize,
) -> Result<u64> {
    let memoria = memoria_recorrente(metadata, prefixo, camadas)?;
    let quantas = memoria.recorrentes.iter().filter(|recorrente| **recorrente).count() as u64;
    checked_or_estouro(memoria.bytes_por_camada.checked_mul(quantas))
}

/// A geometria de cache de um cabeçalho, camada a camada.
///
/// As chaves são as mesmas que o motor lê, com os mesmos padrões quando faltam:
/// sem `attention.head_count_kv`, as cabeças de KV são as de consulta
/// (`llama-model.cpp:1573-1576`); sem `attention.key_length`, a dimensão de
/// cabeça é `embedding_length / head_count`.
pub(crate) fn camadas_de_cache(
    metadata: &HashMap<String, Valor>,
    prefixo: &str,
    camadas: usize,
) -> Result<Vec<CamadaKv>> {
    let chave = |s: &str| format!("{prefixo}{s}");
    let cabecas = escalares_por_camada(metadata, &chave("attention.head_count"), camadas)?.ok_or_else(
        || anyhow::anyhow!(tr("erros.metadadoAusente", &[("chave", &chave("attention.head_count"))])),
    )?;
    let cabecas_kv =
        escalares_por_camada(metadata, &chave("attention.head_count_kv"), camadas)?
            .unwrap_or_else(|| cabecas.clone());
    let comprimento = numero(metadata, &chave("embedding_length"))?;
    // `n_embd / n_head`, e `n_head` é a cabeça da primeira camada (`hparams.n_head()`
    // chama `n_head(0)`): por isso o teto é um número só, não um por camada.
    let primeira = *cabecas.first().unwrap_or(&0);
    let padrao = if primeira > 0 {
        comprimento.checked_div(primeira).unwrap_or(0)
    } else {
        0
    };
    let k_cheia = escalar(metadata, &chave("attention.key_length")).unwrap_or(padrao);
    let v_cheia = escalar(metadata, &chave("attention.value_length")).unwrap_or(padrao);
    let k_deslizante = escalar(metadata, &chave("attention.key_length_swa")).unwrap_or(k_cheia);
    let v_deslizante = escalar(metadata, &chave("attention.value_length_swa")).unwrap_or(v_cheia);
    let deslizante = padrao_deslizante(metadata, &chave("attention.sliding_window_pattern"), camadas);
    // As últimas `shared_kv_layers` camadas não abrem cache próprio.
    let compartilhadas = escalar(metadata, &chave("attention.shared_kv_layers")).unwrap_or(0) as usize;
    let primeira_de_saida = camadas.saturating_sub(compartilhadas);
    let recorrentes = memoria_recorrente(metadata, prefixo, camadas)?.recorrentes;
    Ok((0..camadas)
        .map(|indice| CamadaKv {
            cabecas: cabecas_kv[indice],
            k: if deslizante[indice] { k_deslizante } else { k_cheia },
            v: if deslizante[indice] { v_deslizante } else { v_cheia },
            propria: indice < primeira_de_saida,
            recorrente: recorrentes[indice],
        })
        .collect())
}

/// O custo de KV por token: a soma do que cada camada de atenção aloja, vezes os
/// 2 bytes do f16 com que a biblioteca escreve K e V por padrão (`type_k`/`type_v`
/// em `llama_context_default_params`, `llama-context.cpp:4415`).
///
/// Ficam fora desta soma as camadas que não abrem cache: a que lê o cache de
/// outra (`shared_kv_layers`) e a de atenção linear, que não tem K nem V — no
/// `Qwen3.5-35B-A3B` são 30 das 40, e é `llama-memory-hybrid.cpp` quem filtra: o
/// `llama_kv_cache` recebe `filter_attn = !is_recr(il)` e o estado dessas camadas
/// vai para o `llama_memory_recurrent`, que é o `estado_recorrente` acima. Cobrar
/// KV delas era o que fazia o número da tela sair 4× acima do que a placa abre.
///
/// As células de quem tem janela deslizante continuam sendo a janela inteira,
/// porque o aplicativo deixa `swa_full` no padrão da biblioteca — `true`. É este
/// o número que a tela mostra e o veredito obedece; mudar aquele padrão é decisão
/// de produto, não de conta.
///
/// Cada produto é `checked`: um cabeçalho fabricado que embrulhasse viraria um
/// KV barato de mentira, e a placa juraria caber o modelo inteiro.
fn taxa_kv(camadas: &[CamadaKv]) -> Result<u64> {
    const BYTES_POR_ELEMENTO: u64 = 2;
    let mut total = 0u64;
    for camada in camadas.iter().filter(|c| c.propria && !c.recorrente) {
        let elementos = camada
            .k
            .checked_add(camada.v)
            .and_then(|dupla| dupla.checked_mul(camada.cabecas))
            .and_then(|por_celula| por_celula.checked_mul(BYTES_POR_ELEMENTO))
            .ok_or_else(|| anyhow::anyhow!(tr("erros.alojamentoEstouro", &[])))?;
        total = checked_or_estouro(total.checked_add(elementos))?;
    }
    Ok(total)
}

fn checked_or_estouro(valor: Option<u64>) -> Result<u64> {
    valor.ok_or_else(|| anyhow::anyhow!(tr("erros.alojamentoEstouro", &[])))
}

/// Quantas células a biblioteca reserva para uma janela pedida: ela arredonda o
/// `n_ctx` para cima, em múltiplos de 256, antes de dimensionar o cache
/// (`llama-context.cpp:409-415` do b12387). Toda janela que a escala de geração
/// oferece já é múltipla, então o número só muda para quem pede uma janela
/// torta — e aí o que se prevê é o que se aloja, não o que se pediu.
pub fn celulas_da_janela(janela: usize) -> usize {
    if janela == 0 {
        return 0;
    }
    janela.div_ceil(256) * 256
}

fn texto(metadata: &HashMap<String, Valor>, chave: &str) -> String {
    match metadata.get(chave) {
        Some(Valor::String(v)) => v.clone(),
        _ => String::new(),
    }
}

/// Os bytes de `blk.N.*` somados por camada. A camada de um tensor se lê do
/// nome, que é o que o contêiner GGUF usa para dizer a quem o tensor pertence.
/// O peso de cada ficha vem de `bytes_por_tensor`, que recusa o tipo sem conta
/// medida — somar o que se conhece e chamar o resto de zero é como um veredito
/// mentiria "cabe".
fn por_camada(cabecalho: &cabecalho::Cabecalho, camadas: usize) -> Result<(Vec<u64>, u64)> {
    let pesos = cabecalho.bytes_por_tensor()?;
    let mut soma = vec![0u64; camadas];
    let mut fixo = 0u64;
    for (tensor, bytes) in cabecalho.tensores.iter().zip(pesos.iter()) {
        match camada_do_nome(&tensor.nome, camadas) {
            Some(indice) => soma[indice] = soma[indice].saturating_add(*bytes),
            None => fixo = fixo.saturating_add(*bytes),
        }
    }
    Ok((soma, fixo))
}

fn camada_do_nome(nome: &str, camadas: usize) -> Option<usize> {
    let resto = nome.strip_prefix("blk.")?;
    let fim = resto.find('.')?;
    match resto[..fim].parse::<usize>() {
        Ok(indice) if indice < camadas => Some(indice),
        _ => None,
    }
}

/// Abre o GGUF e tira dele o que a decisão precisa. Um arquivo que não é GGUF,
/// ou a que faltam as chaves de geometria, é recusado aqui e não na tela.
pub fn fichar(caminho: &Path) -> Result<Ficha> {
    let lido = cabecalho::ler(caminho)?;
    let md = &lido.kv;
    let arquitetura = texto(md, "general.architecture");
    if arquitetura.is_empty() {
        bail!(tr("erros.alojamentoSemArquitetura", &[]));
    }
    let prefixo = format!("{arquitetura}.");
    let chave = |s: &str| format!("{prefixo}{s}");
    let camadas = numero(md, &chave("block_count"))? as usize;
    if camadas == 0 {
        bail!(tr("erros.alojamentoSemCamadas", &[]));
    }
    let (camada, fixo) = por_camada(&lido, camadas)?;
    // Somar com `sum` embrulharia num arquivo fabricado; o teto em `u64::MAX` é
    // o valor honesto para "estes bytes não cabem em lugar nenhum".
    let pesos_total = camada
        .iter()
        .fold(0u64, |acum, bytes| acum.saturating_add(*bytes))
        .saturating_add(fixo);
    let (template, eos, paradas) = vocabulario(md);
    // O KV por token é a soma do que cada camada de atenção aloja, camada a
    // camada, com as chaves e os padrões do motor — `camadas_de_cache` acima. As
    // camadas de atenção linear ficam de fora dessa soma por um motivo lido no
    // fonte: elas não abrem cache de K nem de V (`llama-memory-hybrid.cpp`, com
    // `filter_attn = !is_recr(il)`), e o que guardem é o estado fixo que
    // `estado_recorrente` conta logo abaixo. Um arquivo híbrido cujo estado
    // faltasse na conta sairia barato demais — é desta direção que o teto
    // anterior protegia, e é por isso que os dois números andam juntos aqui.
    let kv_por_token = taxa_kv(&camadas_de_cache(md, &prefixo, camadas)?)?;
    let estado_recorrente = estado_recorrente(md, &prefixo, camadas)?;
    Ok(Ficha {
        arquivo: caminho.display().to_string(),
        arquitetura,
        camadas,
        pesos_total,
        camada,
        fixo,
        kv_por_token,
        estado_recorrente,
        janela_maxima: numero(md, &chave("context_length")).unwrap_or(0) as usize,
        template,
        eos,
        paradas,
    })
}

/// Onde os bytes vão ficar. A conta é a da biblioteca: ela põe na placa o que
/// cabe, começando das camadas de cima, e o que sobra fica do lado do sistema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "tipo"
)]
pub enum Alojamento {
    /// Cabe inteiro: pesos, KV da janela, estado recorrente e folga dentro da
    /// VRAM livre medida.
    Inteira {
        /// Bytes de peso que vão para a placa.
        pesos: u64,
        kv: u64,
        /// Bytes do estado das camadas de atenção linear — 0 num modelo denso.
        estado: u64,
        livre: u64,
    },
    /// Não cabe: `camadas` de `total` vão para a placa e o resto fica em RAM. É
    /// o caso dos modelos grandes, e não é uma falha — é a conta da placa.
    Dividida {
        camadas: usize,
        total: usize,
        na_placa: u64,
        em_ram: u64,
        estado: u64,
        livre: u64,
    },
    /// Sem placa nesta máquina: tudo fica do lado do sistema.
    SoSistema { pesos: u64, kv: u64, estado: u64 },
}

/// O veredito para uma janela pedida. `janela` é o que o aplicativo vai pedir,
/// não o máximo do modelo: o KV é proporcional a ela, e é por isso que a mesma
/// placa pode caber um modelo em 4096 e não caber em 32768.
pub fn alojar(ficha: &Ficha, capacidade: Option<&Capacidade>, janela: usize) -> Alojamento {
    // Toda a conta abaixo é saturante: a `fichar` acima recusa um cabeçalho que
    // estoura, mas o veredito é uma função pública sobre uma `Ficha` que pode
    // vir de outra construção. Um produto que embrulhasse aqui diria "cabe" para
    // um modelo que nunca caberia — o pior engano que esta tela pode cometer.
    let Some(cap) = capacidade else {
        return Alojamento::SoSistema {
            pesos: ficha.pesos_total,
            kv: ficha.kv_por_token.saturating_mul(celulas_da_janela(janela) as u64),
            estado: ficha.estado_recorrente,
        };
    };
    let kv = ficha.kv_por_token.saturating_mul(celulas_da_janela(janela) as u64);
    let estado = ficha.estado_recorrente;
    let folga = folga_de_computacao(celulas_da_janela(janela) as u64);
    if ficha
        .pesos_total
        .saturating_add(kv)
        .saturating_add(estado)
        .saturating_add(folga)
        <= cap.livre
    {
        return Alojamento::Inteira { pesos: ficha.pesos_total, kv, estado, livre: cap.livre };
    }
    // O orçamento da placa, tirado o que ela é obrigada a guardar para si: o KV
    // da janela, o estado dos que não têm janela e a folga do grafo. As camadas
    // entram de cima para baixo, que é a ordem em que a biblioteca empilha quando
    // falta memória.
    let orcamento = cap
        .livre
        .saturating_sub(kv.saturating_add(estado).saturating_add(folga));
    let mut usados = 0u64;
    let mut postas = 0usize;
    for bytes in ficha.camada.iter().rev() {
        if usados.saturating_add(*bytes) > orcamento {
            break;
        }
        usados += bytes;
        postas += 1;
    }
    Alojamento::Dividida {
        camadas: postas,
        total: ficha.camadas,
        na_placa: usados,
        em_ram: ficha.pesos_total.saturating_sub(usados),
        estado,
        livre: cap.livre,
    }
}

/// A maior janela de potência de dois, até `teto`, em que o modelo ainda cabe
/// inteiro na placa. É o número que a escala de geração oferece como teto
/// cômodo: acima dele o veredito vira `Dividida` e o fundo do modelo desce para
/// a RAM — continua rodando, só custa mais lento, e a pessoa é quem decide se
/// vale o troca.
///
/// A busca começa no teto e desce pela metade porque `alojar` é monotônico na
/// janela: o KV cresce com ela, então se uma cabe, todas as menores cabem. Uma
/// potência de dois é o que a reserva de KV aceita bem e o que a tela sabe
/// mostrar. `None` é a placa que não segura nem os pesos — não é um zero.
pub fn maior_janela_inteira(
    ficha: &Ficha,
    capacidade: Option<&Capacidade>,
    teto: usize,
) -> Option<usize> {
    let cap = capacidade?;
    let teto = ficha.janela_maxima.min(teto);
    let mut janela = teto.next_power_of_two();
    if janela > teto {
        janela /= 2;
    }
    while janela > 0 {
        if matches!(alojar(ficha, Some(cap), janela), Alojamento::Inteira { .. }) {
            return Some(janela);
        }
        janela /= 2;
    }
    None
}

/// O veredito traduzido na moeda da carga: quantas camadas vão para a placa.
/// Negativo é o sentinela do llama.cpp para "todas", zero é "nenhuma". Esta
/// função existe porque prever sem obedecer é só uma tela bonita — o número que
/// ela devolve é o que a carga recebe, e é por ele que um modelo maior que a
/// placa roda com o topo na placa em vez de cair inteiro na RAM do sistema.
pub fn despacho(alojado: &Alojamento) -> i32 {
    match alojado {
        Alojamento::Inteira { .. } => -1,
        Alojamento::Dividida { camadas, .. } => *camadas as i32,
        Alojamento::SoSistema { .. } => 0,
    }
}

/// Bytes por camada, em média — o número que a tela usa para dizer de quanto em
/// quanto o modelo cresce quando a janela ou a quant mudam.
pub fn media_da_camada(ficha: &Ficha) -> u64 {
    if ficha.camadas == 0 {
        return 0;
    }
    ficha.camada.iter().sum::<u64>() / ficha.camadas as u64
}

#[cfg(test)]
mod testes {
    use super::*;

    /// Uma ficha montada à mão com a geometria do granite-4.2-3b Q4_K_M medida
    /// no arquivo real: 40 camadas, 8 cabeças de KV, 64 por cabeça, 2 GiB de
    /// pesos e 20 MiB por camada. Os testes não lêem disco — o que se verifica
    /// aqui é a decisão; a sonda `engine-probe alojar:` é o que confere a ficha
    /// contra a carga real.
    fn ficha_granite() -> Ficha {
        Ficha {
            arquivo: "granite-4.2-3b-Q4_K_M.gguf".to_string(),
            arquitetura: "granite".to_string(),
            camadas: 40,
            pesos_total: 2136 * 1024 * 1024,
            camada: vec![20 * 1024 * 1024; 40],
            fixo: 2136 * 1024 * 1024 - 40 * 20 * 1024 * 1024,
            kv_por_token: 2 * 40 * 8 * 64 * 2,
            // Granite é denso: nenhuma camada é de atenção linear, então o
            // estado recorrente é zero — e é assim que ele deve aparecer num
            // cabeçalho sem as chaves `ssm.*`.
            estado_recorrente: 0,
            janela_maxima: 131072,
            template: None,
            eos: vec![100257],
            paradas: vec!["<|im_end|>".to_string()],
        }
    }

    fn placa(livre_mib: u64) -> Option<Capacidade> {
        Some(Capacidade {
            placa: "AMD Radeon RX 9060 XT".to_string(),
            livre: livre_mib * 1024 * 1024,
            total: 16 * 1024 * 1024 * 1024,
        })
    }

    #[test]
    fn kv_de_quatro_mil_tokens_e_o_que_a_biblioteca_escreveu() {
        // A fórmula é a única parte que parece chute, então o pino é o número
        // que o llama.cpp imprime: `size = 320.00 MiB (4096 cells, 40 layers)`.
        let ficha = ficha_granite();
        let kv = ficha.kv_por_token * 4096;
        assert_eq!(kv, 335_544_320);
        assert_eq!(kv as f64 / 1048576.0, 320.0);
    }

    #[test]
    fn com_vram_de_sobra_a_resposta_e_inteira() {
        let ficha = ficha_granite();
        // 15 GiB livres: pesos + KV de 4096 + folga cabem com margem.
        let veredito = alojar(&ficha, placa(15 * 1024).as_ref(), 4096);
        match veredito {
            Alojamento::Inteira { pesos, kv, .. } => {
                assert_eq!(pesos, ficha.pesos_total);
                assert_eq!(kv, ficha.kv_por_token * 4096);
            }
            other => panic!("esperava inteira, veio {other:?}"),
        }
    }

    #[test]
    fn na_placa_apertada_a_janela_decide_sozinha() {
        // O modelo é o mesmo, a placa é a mesma, só a janela muda: ela é o que
        // come o orçamento antes das camadas. Uma placa de 15 GiB leva este 3B
        // inteira em qualquer janela que o arquivo declara — e é isso que a
        // conta diz, não o contrário.
        let ficha = ficha_granite();
        let apertada = placa(4 * 1024);
        assert!(matches!(
            alojar(&ficha, apertada.as_ref(), 4096),
            Alojamento::Inteira { .. }
        ));
        match alojar(&ficha, apertada.as_ref(), 65536) {
            Alojamento::Dividida { camadas, total, na_placa, em_ram, .. } => {
                assert_eq!(total, 40);
                // O KV de 65536 já passou da VRAM livre antes de qualquer camada.
                assert_eq!(camadas, 0);
                assert_eq!(na_placa, 0);
                assert_eq!(em_ram, ficha.pesos_total);
            }
            other => panic!("esperava dividida, veio {other:?}"),
        }
    }

    #[test]
    fn a_mesma_janela_muda_de_veredito_conforme_a_placa() {
        // É esta a pergunta do usuário: onde o modelo fica, dado o hardware de
        // agora. A placa grande leva, a pequena divide, e a janela é idêntica.
        let ficha = ficha_granite();
        let longa = 65536;
        assert!(matches!(
            alojar(&ficha, placa(16 * 1024).as_ref(), longa),
            Alojamento::Inteira { .. }
        ));
        match alojar(&ficha, placa(6 * 1024).as_ref(), longa) {
            Alojamento::Dividida { na_placa, em_ram, .. } => {
                assert!(na_placa > 0 && em_ram > 0);
                assert_eq!(na_placa + em_ram, ficha.pesos_total);
            }
            other => panic!("esperava dividida, veio {other:?}"),
        }
    }

    #[test]
    fn dividida_soma_os_dois_lados_e_poe_as_camadas_de_cima() {
        let ficha = ficha_granite();
        // Orçamento que comporta exatamente dez camadas depois do KV e da folga.
        let livre = ficha.kv_por_token * 4096
            + folga_de_computacao(celulas_da_janela(4096) as u64)
            + 10 * 20 * 1024 * 1024;
        match alojar(&ficha, Some(&Capacidade {
            placa: "AMD Radeon RX 9060 XT".to_string(),
            livre,
            total: 16 * 1024 * 1024 * 1024,
        }), 4096) {
            Alojamento::Dividida { camadas, na_placa, em_ram, total, .. } => {
                assert_eq!(camadas, 10);
                assert_eq!(total, 40);
                assert_eq!(na_placa, 10 * 20 * 1024 * 1024);
                assert_eq!(em_ram, ficha.pesos_total - na_placa);
            }
            other => panic!("esperava dividida, veio {other:?}"),
        }
    }

    #[test]
    fn o_veredito_e_a_ordem_de_carga() {
        // O que a conta diz e o que a placa recebe têm de ser o mesmo número:
        // pedir `-1` para um modelo que não cabe é o caminho que joga a conversa
        // inteira na RAM do sistema.
        let ficha = ficha_granite();
        assert_eq!(despacho(&alojar(&ficha, placa(15 * 1024).as_ref(), 4096)), -1);
        assert_eq!(despacho(&alojar(&ficha, None, 4096)), 0);
        // A placa de 4 GiB com janela de 65536 não leva camada nenhuma: o zero
        // aqui é instrução, não derrota — a biblioteca não vai tentar.
        assert_eq!(despacho(&alojar(&ficha, placa(4 * 1024).as_ref(), 65536)), 0);
        // E no meio da faixa, o despacho é exatamente a contagem do veredito.
        let dividida = alojar(&ficha, Some(&Capacidade {
            placa: "AMD Radeon RX 9060 XT".to_string(),
            livre: ficha.kv_por_token * 4096
                + folga_de_computacao(celulas_da_janela(4096) as u64)
                + 10 * 20 * 1024 * 1024,
            total: 16 * 1024 * 1024 * 1024,
        }), 4096);
        assert_eq!(despacho(&dividida), 10);
    }

    #[test]
    fn a_recomendacao_para_na_maior_potencia_que_cabe_inteira() {
        // A placa de 8 GiB leva este 3B inteiro até 65536 (2136 + 5120 + 220),
        // e em 131072 o KV sozinho passa do livre. A resposta não é 131072 nem
        // 7476: é a janela que a escala sabe oferecer.
        let ficha = ficha_granite();
        assert_eq!(maior_janela_inteira(&ficha, placa(8 * 1024).as_ref(), 131072), Some(65536));
    }

    #[test]
    fn a_recomendacao_respeita_o_teto_do_arquivo_nao_o_pedido() {
        // O teto que a tela oferece é o que o GGUF declara, e a ficha diz
        // 131072 para o granite. Pedir 262144 não inventa janela no modelo.
        let ficha = ficha_granite();
        assert_eq!(maior_janela_inteira(&ficha, placa(16 * 1024).as_ref(), 262144), Some(131072));
    }

    #[test]
    fn um_teto_fora_de_potencia_de_dois_arredonda_para_baixo() {
        // 100000 não é janela que se reserve: começa-se em 65536 e desce-se até
        // caber. Na placa de 4 GiB quem cabe é 16384 (2136 + 1280 + 220).
        let ficha = ficha_granite();
        assert_eq!(maior_janela_inteira(&ficha, placa(4 * 1024).as_ref(), 100_000), Some(16384));
    }

    #[test]
    fn sem_placa_ou_sem_pesos_nao_ha_recomendacao() {
        let ficha = ficha_granite();
        assert_eq!(maior_janela_inteira(&ficha, None, 131072), None);
        // Uma placa que não leva nem os pesos não tem janela cômoda alguma — e
        // um zero aqui seria a tela oferecer uma janela que não roda.
        assert_eq!(maior_janela_inteira(&ficha, placa(1000).as_ref(), 131072), None);
    }

    #[test]
    fn sem_placa_a_resposta_e_sistema_e_sem_chute_de_vram() {
        let ficha = ficha_granite();
        match alojar(&ficha, None, 4096) {
            Alojamento::SoSistema { pesos, kv, estado } => {
                assert_eq!(pesos, ficha.pesos_total);
                assert_eq!(kv, ficha.kv_por_token * 4096);
                // Granite é denso: o estado é a conta que o arquivo híbrido
                // acrescenta, e num denso ele tem de ser zero, não um chute.
                assert_eq!(estado, 0);
            }
            other => panic!("sem placa só existe o caso de sistema, veio {other:?}"),
        }
    }

    #[test]
    fn camada_se_le_do_nome_do_tensor() {
        assert_eq!(camada_do_nome("blk.7.ffn_up.weight", 40), Some(7));
        assert_eq!(camada_do_nome("blk.39.attn_q.weight", 40), Some(39));
        // Fora da geometria não é camada: é um nome que o parser do arquivo já
        // viu, e contá-lo como camada empurraria bytes para o lugar errado.
        assert_eq!(camada_do_nome("blk.40.attn_q.weight", 40), None);
        assert_eq!(camada_do_nome("token_embd.weight", 40), None);
        assert_eq!(camada_do_nome("output.weight", 40), None);
    }

    /// Uma ficha como o cabeçalho a escreve: o número do tipo é o que o arquivo
    /// guarda, e a forma é o produto das dimensões.
    fn ficha(nome: &str, dimensoes: &[u64], tipo: u32) -> cabecalho::Tensor {
        cabecalho::Tensor {
            nome: nome.to_string(),
            dimensoes: dimensoes.to_vec(),
            tipo,
            deslocamento: 0,
        }
    }

    fn cabecalho_de(tensores: Vec<cabecalho::Tensor>) -> cabecalho::Cabecalho {
        cabecalho::Cabecalho {
            versao: 3,
            alinhamento: cabecalho::ALINHAMENTO_PADRAO,
            inicio_dos_dados: cabecalho::ALINHAMENTO_PADRAO,
            kv: HashMap::new(),
            tensores,
        }
    }

    #[test]
    fn bytes_de_tensor_usam_o_bloco_do_dtype_e_arredondam_para_cima() {
        // Q4_K_M: 256 elementos por bloco de 144 bytes.
        let q4 = ficha("blk.0.attn_q.weight", &[4096, 4096], 12);
        assert_eq!(q4.bytes(), Some(4096 * 4096 / 256 * 144));
        // f32 é bloco de um: a conta degenera em elementos vezes o tamanho.
        let f32 = ficha("t", &[10, 10], 0);
        assert_eq!(f32.bytes(), Some(400));
        // Um tensor que não fecha o bloco inteiro paga o bloco mesmo assim.
        let torto = ficha("t", &[300, 1], 12);
        assert_eq!(torto.bytes(), Some(2 * 144));
        // O tipo sem conta medida não devolve zero: devolve `None`, e é isso que
        // impede um arquivo pesado pela metade de parecer barato.
        let sem_conta = ficha("t", &[300, 1], 9);
        assert_eq!(sem_conta.bytes(), None);
    }

    #[test]
    fn por_camada_separa_o_que_e_da_placa_do_que_fica_fora() {
        let lido = cabecalho_de(vec![
            ficha("blk.0.attn_q.weight", &[10, 10], 0),
            ficha("blk.1.attn_q.weight", &[10, 10], 0),
            ficha("token_embd.weight", &[4, 4], 0),
        ]);
        let (camada, fixo) = por_camada(&lido, 2).unwrap();
        assert_eq!(camada, vec![400, 400]);
        assert_eq!(fixo, 64);
    }

    #[test]
    fn um_tipo_sem_conta_recusa_a_ficha_em_vez_de_pesar_meio_arquivo() {
        // Exatamente o caso que deixava o arquivo fora do catálogo: o candle o
        // recusava antes de qualquer conta. Aqui ele tem conta, e o que sobra para
        // recusar é o tipo nunca medido — que não pode virar zero na soma.
        let lido = cabecalho_de(vec![
            ficha("blk.0.attn_q.weight", &[10, 10], 0),
            ficha("blk.0.attn_k.weight", &[10, 10], 140),
        ]);
        let erro = por_camada(&lido, 1).unwrap_err().to_string();
        assert!(erro.contains("blk.0.attn_k.weight"), "a recusa devia nomear o tensor: {erro}");
        assert!(erro.contains("140"), "a recusa devia dizer o número lido: {erro}");
    }

    #[test]
    fn media_da_camada_e_o_passo_de_crescimento() {
        let ficha = ficha_granite();
        assert_eq!(media_da_camada(&ficha), 20 * 1024 * 1024);
        let vazio = Ficha { camada: vec![], camadas: 0, ..ficha };
        assert_eq!(media_da_camada(&vazio), 0);
    }

    /// O fio é o contrato com `types.ts`, e ele já foi quebrado uma vez: num
    /// enum, `rename_all` só troca os nomes das variantes — os campos pedem
    /// `rename_all_fields`. As três formas ficam pinadas aqui, byte a byte.
    #[test]
    fn as_tres_formas_do_veredito_sao_camelcase_no_wire() {
        let json = |a: &Alojamento| serde_json::to_string(a).unwrap();
        assert_eq!(
            json(&Alojamento::Inteira { pesos: 2240, kv: 335, estado: 613, livre: 15_360 }),
            r#"{"tipo":"inteira","pesos":2240,"kv":335,"estado":613,"livre":15360}"#
        );
        assert_eq!(
            json(&Alojamento::Dividida {
                camadas: 27,
                total: 40,
                na_placa: 3132,
                em_ram: 1967,
                estado: 613,
                livre: 4096,
            }),
            r#"{"tipo":"dividida","camadas":27,"total":40,"naPlaca":3132,"emRam":1967,"estado":613,"livre":4096}"#
        );
        assert_eq!(
            json(&Alojamento::SoSistema { pesos: 2240, kv: 335, estado: 613 }),
            r#"{"tipo":"soSistema","pesos":2240,"kv":335,"estado":613}"#
        );
    }

    /// Um GGUF mínimo escrito byte a byte, com os metadados que a ficha lê. O
    /// parser é o do candle, então o que o teste exercita é a nossa leitura de
    /// vocabulário — template, fins de resposta e os tokens que os nomeiam.
    /// `tipo_de_tensor` acrescenta um tensor de 64 elementos com aquele número
    /// ggml: `None` é o arquivo só de metadados, e um número fora da tabela do
    /// leitor é o caso dos IQ.
    fn gguf_de_vocabulario(caminho: &Path, com_eot: bool, tipo_de_tensor: Option<u32>) {
        const STR: u32 = 8;
        const U32: u32 = 4;
        const ARR: u32 = 9;

        fn texto(s: &str) -> Vec<u8> {
            let mut v = (s.len() as u64).to_le_bytes().to_vec();
            v.extend_from_slice(s.as_bytes());
            v
        }
        fn par(chave: &str, tipo: u32, valor: Vec<u8>) -> Vec<u8> {
            let mut v = texto(chave);
            v.extend_from_slice(&tipo.to_le_bytes());
            v.extend_from_slice(&valor);
            v
        }
        fn u32_bytes(v: u32) -> Vec<u8> {
            v.to_le_bytes().to_vec()
        }
        fn array_de_textos(itens: &[&str]) -> Vec<u8> {
            let mut v = STR.to_le_bytes().to_vec();
            v.extend_from_slice(&(itens.len() as u64).to_le_bytes());
            for item in itens {
                v.extend_from_slice(&texto(item));
            }
            v
        }

        let mut corpo: Vec<u8> = Vec::new();
        for bytes in [
            par("general.architecture", STR, texto("qwen35")),
            par("qwen35.block_count", U32, u32_bytes(2)),
            par("qwen35.embedding_length", U32, u32_bytes(64)),
            par("qwen35.attention.head_count", U32, u32_bytes(4)),
            par("qwen35.attention.head_count_kv", U32, u32_bytes(1)),
            par("qwen35.attention.key_length", U32, u32_bytes(16)),
            par("qwen35.context_length", U32, u32_bytes(262_144)),
            par("tokenizer.chat_template", STR, texto("{{ messages }}")),
            par("tokenizer.ggml.tokens", ARR, array_de_textos(&["<unk>", "<bos>", "<|im_end|>", "<|im_start|>"])),
            par("tokenizer.ggml.eos_token_id", U32, u32_bytes(2)),
        ] {
            corpo.extend_from_slice(&bytes);
        }
        let pares = 10 + usize::from(com_eot);
        if com_eot {
            corpo.extend_from_slice(&par(
                "tokenizer.ggml.eot_token_id",
                U32,
                u32_bytes(3),
            ));
        }

        let mut tensor: Vec<u8> = Vec::new();
        if let Some(numero) = tipo_de_tensor {
            tensor.extend_from_slice(&texto("blk.0.attn_q.weight"));
            tensor.extend_from_slice(&1u32.to_le_bytes());
            tensor.extend_from_slice(&64u64.to_le_bytes());
            tensor.extend_from_slice(&numero.to_le_bytes());
            tensor.extend_from_slice(&0u64.to_le_bytes());
        }

        let mut arquivo: Vec<u8> = b"GGUF".to_vec();
        arquivo.extend_from_slice(&3u32.to_le_bytes());
        arquivo.extend_from_slice(&(tipo_de_tensor.is_some() as u64).to_le_bytes());
        arquivo.extend_from_slice(&(pares as u64).to_le_bytes());
        arquivo.extend_from_slice(&corpo);
        arquivo.extend_from_slice(&tensor);
        std::fs::write(caminho, arquivo).unwrap();
    }

    #[test]
    fn a_ficha_le_template_eos_e_paradas_do_gguf() {
        let dir = std::env::temp_dir().join(format!("aether-aloj-vocab-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let caminho = dir.join("mini.gguf");

        gguf_de_vocabulario(&caminho, true, None);
        let ficha = fichar(&caminho).unwrap();
        assert_eq!(ficha.arquitetura, "qwen35");
        assert_eq!(ficha.janela_maxima, 262_144);
        assert_eq!(ficha.template.as_deref(), Some("{{ messages }}"));
        assert_eq!(ficha.eos, vec![2, 3]);
        // O que a geração observa como parada é o token escrito, lido do
        // vocabulário do arquivo — não um marcador copiado de outro modelo.
        assert_eq!(ficha.paradas, vec!["<|im_end|>", "<|im_start|>"]);

        // Sem o eot declarado fica só o eos: nada é inventado.
        gguf_de_vocabulario(&caminho, false, None);
        let ficha = fichar(&caminho).unwrap();
        assert_eq!(ficha.eos, vec![2]);
        assert_eq!(ficha.paradas, vec!["<|im_end|>"]);

        // Um arquivo sem template publicado não faz a ficha mentir sobre ele: o
        // chamador recua para o chat_template.jinja baixado.
        let vazio = dir.join("sem-template.gguf");
        std::fs::write(&vazio, b"GGUF").unwrap();
        assert!(fichar(&vazio).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// O preço de um tipo sem bloco medido é a ausência de veredito, não uma
    /// conta aproximada. O número abaixo que tem conta é o 16 — `iq2_xxs`, o tipo
    /// que o leitor anterior não conhecia e que deixava os onze arquivos do
    /// `Qwen3.5-35B-A3B` sem ficha; hoje ele pesa, e é o que devolve veredito a
    /// eles. O 9 (`q8_1`) é o que continua de fora: o candle nomeia o tipo, mas
    /// nenhum dos 74 arquivos lidos tensor a tensor tem um tensor dele, então não
    /// há bloco medido — e um `block_size` chutado diria "cabe" sobre um arquivo
    /// que ninguém pesou.
    #[test]
    fn tipo_sem_bloco_medido_nao_vira_ficha_inventada() {
        let dir = std::env::temp_dir().join(format!("aether-aloj-dtype-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let caminho = dir.join("um-tensor.gguf");

        gguf_de_vocabulario(&caminho, true, Some(8));
        let ficha = fichar(&caminho).unwrap();
        assert_eq!(ficha.pesos_total, 68, "q8_0: 64 elementos são 2 blocos de 34 bytes");

        // O tipo que abria porta fechada: 64 elementos não fecham o bloco de 256,
        // e o tensor paga o bloco inteiro — 66 bytes, como o ggml faz.
        gguf_de_vocabulario(&caminho, true, Some(16));
        let ficha = fichar(&caminho).unwrap();
        assert_eq!(ficha.pesos_total, 66, "iq2_xxs entra na conta em vez de recusar o arquivo");

        gguf_de_vocabulario(&caminho, true, Some(9));
        let erro = fichar(&caminho)
            .err()
            .expect("q8_1 não teve bloco medido em arquivo algum");
        let erro = erro.to_string();
        assert!(erro.contains("blk.0.attn_q.weight"), "a recusa nomeia o tensor: {erro}");
        assert!(erro.contains('9'), "a recusa diz o número lido: {erro}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn um_kv_que_estoura_a_conta_jamais_diz_que_cabe_inteira() {
        // O defeito: `kv_por_token * janela` embrulha em u64 num release, o KV
        // parece zero e a placa juraria caber um modelo que nunca caberia. A
        // conta é o `saturating_mul` de `alojar`, exercida direto, com uma ficha
        // armada à mão para o estouro (a `fichar` real já recusaria este cabeçalho).
        let impossivel = Ficha {
            arquivo: "impossivel.gguf".to_string(),
            arquitetura: "granite".to_string(),
            camadas: 40,
            pesos_total: 2136 * 1024 * 1024,
            camada: vec![20 * 1024 * 1024; 40],
            fixo: 2136 * 1024 * 1024 - 40 * 20 * 1024 * 1024,
            // 1 << 63 vezes uma janela de 2 = 1 << 64: embrulha em zero.
            kv_por_token: 1u64 << 63,
            estado_recorrente: 0,
            janela_maxima: 131072,
            template: None,
            eos: vec![100257],
            paradas: vec![],
        };
        let veredito = alojar(&impossivel, placa(15 * 1024).as_ref(), 2);
        assert!(
            !matches!(veredito, Alojamento::Inteira { .. }),
            "um KV que estoura a conta não pode virar veredito inteira: {veredito:?}"
        );
    }

    /// Um GGUF só de geometria, sem tensores nem vocabulário, com o número de
    /// cabeças de KV fabricado para estourar a conta do KV cache. É o mesmo
    /// esqueleto de `gguf_de_vocabulario`, com os valores trocados.
    fn gguf_de_geometria_estourada(caminho: &Path) {
        const STR: u32 = 8;
        const U32: u32 = 4;
        fn texto(s: &str) -> Vec<u8> {
            let mut v = (s.len() as u64).to_le_bytes().to_vec();
            v.extend_from_slice(s.as_bytes());
            v
        }
        fn par(chave: &str, tipo: u32, valor: Vec<u8>) -> Vec<u8> {
            let mut v = texto(chave);
            v.extend_from_slice(&tipo.to_le_bytes());
            v.extend_from_slice(&valor);
            v
        }
        let u32_bytes = |v: u32| v.to_le_bytes().to_vec();
        let corpo: Vec<u8> = [
            par("general.architecture", STR, texto("granite")),
            // block_count pequeno: o vetor de camadas não aloca memória a mais.
            par("granite.block_count", U32, u32_bytes(2)),
            par("granite.embedding_length", U32, u32_bytes(u32::MAX)),
            par("granite.attention.head_count", U32, u32_bytes(1)),
            // head_count_kv no teto: 2*2*MAX*MAX*2 estoura um u64 com folga.
            par("granite.attention.head_count_kv", U32, u32_bytes(u32::MAX)),
            par("granite.attention.key_length", U32, u32_bytes(u32::MAX)),
            par("granite.context_length", U32, u32_bytes(4096)),
        ]
        .concat();
        let mut arquivo: Vec<u8> = b"GGUF".to_vec();
        arquivo.extend_from_slice(&3u32.to_le_bytes());
        arquivo.extend_from_slice(&0u64.to_le_bytes());
        arquivo.extend_from_slice(&(7u64).to_le_bytes());
        arquivo.extend_from_slice(&corpo);
        std::fs::write(caminho, arquivo).unwrap();
    }

    #[test]
    fn a_ficha_recusa_geometria_que_estoura_a_conta() {
        // A fronteira de entrada: um cabeçalho fabricado não pode virar uma
        // Ficha com um `kv_por_token` embrulhado (que a tela leria como um KV
        // barato). A recusa é o motivo real, não um zero silencioso.
        let dir = std::env::temp_dir().join(format!("aether-aloj-estouro-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let caminho = dir.join("estouro.gguf");
        gguf_de_geometria_estourada(&caminho);
        let erro = fichar(&caminho).expect_err("geometria que estoura o u64 deve ser recusada");
        assert!(
            erro.to_string().contains("estoura"),
            "a recusa deve nomear o estouro, veio: {erro}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---------------------------------------------------------------------
    // A geometria de cache camada a camada — o que os arquivos de 2026 escrevem
    // ---------------------------------------------------------------------

    /// Os tipos que a geometria de um GGUF usa: um número, um texto e o array
    /// — de inteiros (`head_count_kv`) ou de bools (`sliding_window_pattern`),
    /// que é como o gemma-4-12b-it escreve as 48 entradas de cada um deles.
    enum Geometria {
        Numero(u32),
        Texto(&'static str),
        Inteiros(Vec<i32>),
        Bools(Vec<bool>),
    }

    fn valor_de(geometria: &Geometria) -> Valor {
        match geometria {
            Geometria::Numero(v) => Valor::U32(*v),
            Geometria::Texto(s) => Valor::String((*s).to_string()),
            Geometria::Inteiros(itens) => Valor::Array(
                itens.iter().map(|v| Valor::I32(*v)).collect(),
            ),
            Geometria::Bools(itens) => Valor::Array(
                itens.iter().map(|v| Valor::Bool(*v)).collect(),
            ),
        }
    }

    fn mapa_de(pares: &[(&'static str, Geometria)]) -> HashMap<String, Valor> {
        pares
            .iter()
            .map(|(chave, valor)| (chave.to_string(), valor_de(valor)))
            .collect()
    }

    /// O mesmo cabeçalho escrito como bytes de GGUF v3. Vale o caminho inteiro:
    /// o parser é o do candle, que é o da carga, então um array que só existisse
    /// na nossa conta não passaria daqui.
    fn escreve_gguf(caminho: &Path, pares: &[(&'static str, Geometria)]) {
        const U32: u32 = 4;
        const I32: u32 = 5;
        const BOOL: u32 = 7;
        const STR: u32 = 8;
        const ARR: u32 = 9;

        fn texto(s: &str) -> Vec<u8> {
            let mut v = (s.len() as u64).to_le_bytes().to_vec();
            v.extend_from_slice(s.as_bytes());
            v
        }

        let mut arquivo: Vec<u8> = b"GGUF".to_vec();
        arquivo.extend_from_slice(&3u32.to_le_bytes());
        arquivo.extend_from_slice(&0u64.to_le_bytes());
        arquivo.extend_from_slice(&(pares.len() as u64).to_le_bytes());
        for (chave, geometria) in pares {
            arquivo.extend_from_slice(&texto(chave));
            let (tipo, corpo): (u32, Vec<u8>) = match geometria {
                Geometria::Numero(v) => (U32, v.to_le_bytes().to_vec()),
                Geometria::Texto(s) => (STR, texto(s)),
                Geometria::Inteiros(itens) => {
                    let mut corpo = I32.to_le_bytes().to_vec();
                    corpo.extend_from_slice(&(itens.len() as u64).to_le_bytes());
                    for item in itens {
                        corpo.extend_from_slice(&item.to_le_bytes());
                    }
                    (ARR, corpo)
                }
                Geometria::Bools(itens) => {
                    let mut corpo = BOOL.to_le_bytes().to_vec();
                    corpo.extend_from_slice(&(itens.len() as u64).to_le_bytes());
                    for item in itens {
                        corpo.push(u8::from(*item));
                    }
                    (ARR, corpo)
                }
            };
            arquivo.extend_from_slice(&tipo.to_le_bytes());
            arquivo.extend_from_slice(&corpo);
        }
        std::fs::write(caminho, arquivo).unwrap();
    }

    /// `gemma-4-12b-it-Q4_K_M.gguf` chave por chave, como ele está em disco. As
    /// duas listas de 48 entradas são a forma que o arquivo usa: `head_count_kv`
    /// é 8 nas camadas de janela deslizante e 1 nas de atenção cheia, e o padrão
    /// escreve `1` naquelas e `0` nestas — uma camada cheia a cada seis, ou
    /// 40 deslizantes e 8 cheias.
    fn gemma4() -> Vec<(&'static str, Geometria)> {
        let cabecas_kv = (0..48).map(|il| if il % 6 == 5 { 1 } else { 8 }).collect();
        let padrao = (0..48).map(|il| il % 6 != 5).collect();
        vec![
            ("general.architecture", Geometria::Texto("gemma4")),
            ("gemma4.block_count", Geometria::Numero(48)),
            ("gemma4.embedding_length", Geometria::Numero(3840)),
            ("gemma4.context_length", Geometria::Numero(262_144)),
            ("gemma4.attention.head_count", Geometria::Numero(16)),
            ("gemma4.attention.head_count_kv", Geometria::Inteiros(cabecas_kv)),
            ("gemma4.attention.key_length", Geometria::Numero(512)),
            ("gemma4.attention.value_length", Geometria::Numero(512)),
            ("gemma4.attention.key_length_swa", Geometria::Numero(256)),
            ("gemma4.attention.value_length_swa", Geometria::Numero(256)),
            ("gemma4.attention.sliding_window", Geometria::Numero(1024)),
            ("gemma4.attention.sliding_window_pattern", Geometria::Bools(padrao)),
            ("gemma4.attention.shared_kv_layers", Geometria::Numero(0)),
        ]
    }

    /// `granite-4.2-3b-Q4_K_M.gguf`, o caso uniforme: um número por chave, e as
    /// dimensões de cabeça vindas do padrão `embedding_length / head_count`.
    fn granite() -> Vec<(&'static str, Geometria)> {
        vec![
            ("general.architecture", Geometria::Texto("granite")),
            ("granite.block_count", Geometria::Numero(40)),
            ("granite.embedding_length", Geometria::Numero(2560)),
            ("granite.context_length", Geometria::Numero(131_072)),
            ("granite.attention.head_count", Geometria::Numero(40)),
            ("granite.attention.head_count_kv", Geometria::Numero(8)),
        ]
    }

    /// `Qwen3.5-35B-A3B-UD-Q4_K_XL.gguf`, lido do cabeçalho do próprio arquivo
    /// (Range sobre o LFS, 52 pares): o híbrido de 40 camadas em que só 10 têm
    /// atenção — as outras 30 são gated delta net, marcadas pelo período 4, não
    /// por um array. É o caso que a conta antiga pagava cache por todas.
    fn qwen35a3b() -> Vec<(&'static str, Geometria)> {
        vec![
            ("general.architecture", Geometria::Texto("qwen35moe")),
            ("qwen35moe.block_count", Geometria::Numero(40)),
            ("qwen35moe.embedding_length", Geometria::Numero(2048)),
            ("qwen35moe.context_length", Geometria::Numero(262_144)),
            ("qwen35moe.attention.head_count", Geometria::Numero(16)),
            ("qwen35moe.attention.head_count_kv", Geometria::Numero(2)),
            ("qwen35moe.attention.key_length", Geometria::Numero(256)),
            ("qwen35moe.attention.value_length", Geometria::Numero(256)),
            ("qwen35moe.full_attention_interval", Geometria::Numero(4)),
            ("qwen35moe.ssm.conv_kernel", Geometria::Numero(4)),
            ("qwen35moe.ssm.state_size", Geometria::Numero(128)),
            ("qwen35moe.ssm.group_count", Geometria::Numero(16)),
            ("qwen35moe.ssm.time_step_rank", Geometria::Numero(32)),
            ("qwen35moe.ssm.inner_size", Geometria::Numero(4096)),
        ]
    }

    #[test]
    fn o_gemma4_paga_cada_camada_pelo_que_a_camada_aloja() {
        let camadas = camadas_de_cache(&mapa_de(&gemma4()), "gemma4.", 48).unwrap();
        assert_eq!(camadas.len(), 48);
        // As 40 de janela deslizante com 8 cabeças de 256; as 8 cheias com uma
        // cabeça de 512. A conta antiga tratava o primeiro número do array como
        // se valesse para todas as camadas e chegava em 786.432 — 2,28× sobre o
        // valor real, o bastante para mandar para a RAM camadas que a placa segura.
        assert_eq!(camadas.iter().filter(|c| c.k == 256).count(), 40);
        assert_eq!(camadas.iter().filter(|c| c.cabecas == 1).count(), 8);
        assert_eq!(taxa_kv(&camadas).unwrap(), 344_064);
        // Com a janela de 4096 que a escala oferece, o teto fechado em MiB é a
        // prova de que o número é uma conta e não um chute: 344.064 / 256 = 1344.
        assert_eq!(
            taxa_kv(&camadas).unwrap() * celulas_da_janela(4096) as u64 / 1048576,
            1344
        );
    }

    #[test]
    fn o_granite_de_camadas_iguais_chega_nos_mesmos_320_mib_da_biblioteca() {
        // A outra metade da prova: quando todas as camadas são iguais, a soma
        // camada a camada tem de dar exatamente o que a biblioteca imprimiu na
        // carga — `llama_kv_cache: size = 320.00 MiB (4096 cells, 40 layers)`.
        let camadas = camadas_de_cache(&mapa_de(&granite()), "granite.", 40).unwrap();
        // Sem `key_length` no arquivo, a dimensão é o padrão do motor:
        // 2560 / 40 = 64 por cabeça, em K e V.
        assert!(camadas.iter().all(|c| c.k == 64 && c.v == 64 && c.cabecas == 8));
        let taxa = taxa_kv(&camadas).unwrap();
        assert_eq!(taxa, 81_920);
        assert_eq!(taxa * celulas_da_janela(4096) as u64, 335_544_320);
        assert_eq!(taxa * 4096 / 1048576, 320);
    }

    /// O defeito que esta conta fecha: as camadas de atenção linear não abrem
    /// KV cache nenhum — `llama-memory-hybrid.cpp` entrega ao `llama_kv_cache` o
    /// `filter_attn = !is_recr(il)` — e cobrá-las como se tivessem K e V fazia a
    /// placa jurar 4× mais cache do que ela reserva.
    #[test]
    fn as_camadas_lineares_nao_pagam_cache_e_o_estado_ocupa_o_lugar_delas() {
        let md = mapa_de(&qwen35a3b());
        let camadas = camadas_de_cache(&md, "qwen35moe.", 40).unwrap();
        assert_eq!(camadas.iter().filter(|c| c.recorrente).count(), 30);
        // A última camada de cada grupo de 4 é a única com atenção: 3, 7, 11 …
        for (il, camada) in camadas.iter().enumerate() {
            assert_eq!(camada.recorrente, (il + 1) % 4 != 0, "camada {il}");
        }
        let taxa = taxa_kv(&camadas).unwrap();
        assert_eq!(taxa, 20_480);
        // Cobrar K e V também das 30 lineares dava 81.920 por token — o quádruplo
        // exato, que é o número que a tela mostrava e o motivo pelo qual camadas
        // que a placa segura eram mandadas para a RAM.
        let se_todas_pagassem: u64 = camadas
            .iter()
            .filter(|c| c.propria)
            .map(|c| (c.k + c.v) * c.cabecas * 2)
            .sum();
        assert_eq!(se_todas_pagassem, 81_920);
        assert_eq!(taxa * 4, se_todas_pagassem);
        // O estado é o que a biblioteca abre no lugar do cache: (conv-1)×(inner +
        // 2×grupo×estado) + estado×inner, em f32 — 2.195.456 bytes por camada.
        let memoria = memoria_recorrente(&md, "qwen35moe.", 40).unwrap();
        assert_eq!(memoria.bytes_por_camada, 2_195_456);
        assert_eq!(estado_recorrente(&md, "qwen35moe.", 40).unwrap(), 65_863_680);
    }

    #[test]
    fn o_estado_recorrente_nao_cresce_com_a_janela_e_muda_o_veredito() {
        let md = mapa_de(&qwen35a3b());
        let estado = estado_recorrente(&md, "qwen35moe.", 40).unwrap();
        let ficha = Ficha {
            arquivo: "Qwen3.5-35B-A3B-UD-Q4_K_XL.gguf".to_string(),
            arquitetura: "qwen35moe".to_string(),
            camadas: 40,
            pesos_total: 12 * 1024 * 1024 * 1024,
            camada: vec![12 * 1024 * 1024 * 1024 / 40; 40],
            fixo: 0,
            kv_por_token: taxa_kv(&camadas_de_cache(&md, "qwen35moe.", 40).unwrap()).unwrap(),
            estado_recorrente: estado,
            janela_maxima: 262_144,
            template: None,
            eos: vec![],
            paradas: vec![],
        };
        // A placa justa para os pesos, o KV de 4096 e a folga — e nada de estado.
        let kv = ficha.kv_por_token * celulas_da_janela(4096) as u64;
        let justa = Capacidade {
            placa: "AMD Radeon RX 9060 XT".to_string(),
            livre: ficha.pesos_total + kv + folga_de_computacao(celulas_da_janela(4096) as u64),
            total: 16 * 1024 * 1024 * 1024,
        };
        // Sem estado, esta placa é o caso inteiro; com ele, falta um grão e o
        // veredito desce para dividido. É assim que o número entra na decisão em
        // vez de só enfeitar a tela.
        let denso = Ficha { estado_recorrente: 0, ..ficha.clone() };
        assert!(matches!(
            alojar(&denso, Some(&justa), 4096),
            Alojamento::Inteira { .. }
        ));
        match alojar(&ficha, Some(&justa), 4096) {
            Alojamento::Dividida { estado, .. } => assert_eq!(estado, 65_863_680),
            other => panic!("com estado, a placa justa não cabe inteira: {other:?}"),
        }
        // E o estado é o mesmo de 4096 a 131072: reserva fixa por sequência, não
        // proporcional à janela — é por isso que ele não entra na taxa por token.
        let folgada = Capacidade {
            livre: ficha.pesos_total
                + ficha.kv_por_token * celulas_da_janela(131_072) as u64
                + estado
                + folga_de_computacao(celulas_da_janela(131_072) as u64),
            ..justa
        };
        for janela in [4096, 32_768, 131_072] {
            match alojar(&ficha, Some(&folgada), janela) {
                Alojamento::Inteira { estado, .. } => assert_eq!(estado, 65_863_680, "janela {janela}"),
                other => panic!("com a placa folgada tudo cabe inteira, veio {other:?}"),
            }
        }
    }

    /// `Qwen3.5-4B-Q4_K_M.gguf`, a entrada que já está no catálogo: 32 camadas
    /// com cabeças de KV 4 e o mesmo gated delta net do A3B, lidos do cabeçalho.
    fn qwen35_4b() -> Vec<(&'static str, Geometria)> {
        vec![
            ("general.architecture", Geometria::Texto("qwen35")),
            ("qwen35.block_count", Geometria::Numero(32)),
            ("qwen35.embedding_length", Geometria::Numero(2560)),
            ("qwen35.context_length", Geometria::Numero(262_144)),
            ("qwen35.attention.head_count", Geometria::Numero(16)),
            ("qwen35.attention.head_count_kv", Geometria::Numero(4)),
            ("qwen35.attention.key_length", Geometria::Numero(256)),
            ("qwen35.attention.value_length", Geometria::Numero(256)),
            ("qwen35.full_attention_interval", Geometria::Numero(4)),
            ("qwen35.ssm.conv_kernel", Geometria::Numero(4)),
            ("qwen35.ssm.state_size", Geometria::Numero(128)),
            ("qwen35.ssm.group_count", Geometria::Numero(16)),
            ("qwen35.ssm.time_step_rank", Geometria::Numero(32)),
            ("qwen35.ssm.inner_size", Geometria::Numero(4096)),
        ]
    }

    #[test]
    fn a_um_hibrido_de_32_camadas_o_estado_sobe_com_elas() {
        let md = mapa_de(&qwen35_4b());
        let camadas = camadas_de_cache(&md, "qwen35.", 32).unwrap();
        assert_eq!(camadas.iter().filter(|c| !c.recorrente).count(), 8);
        // A linha da tabela do catálogo: 32.768 por token, e não os 131.072 que
        // a conta cobrava pelas 32 camadas inteiras.
        assert_eq!(taxa_kv(&camadas).unwrap(), 32_768);
        // O mesmo gated delta net do A3B: 2.195.456 bytes por camada, por 24
        // delas = 50,25 MiB de estado fixo por sequência.
        assert_eq!(estado_recorrente(&md, "qwen35.", 32).unwrap(), 52_690_944);
    }

    #[test]
    fn a_chave_de_camadas_recorrentes_manda_no_periodo() {
        // O motor consulta `attention.recurrent_layers` primeiro; tendo-a, o
        // período não é lido (`qwen35.cpp:17-22`). O arquivo do A3B não traz a
        // chave, mas um exportador que traga não pode ser aproximado pelo 4.
        let mut md = mapa_de(&qwen35a3b());
        md.insert(
            "qwen35moe.attention.recurrent_layers".to_string(),
            Valor::Array((0..40).map(|il| Valor::Bool(il == 0)).collect()),
        );
        let camadas = camadas_de_cache(&md, "qwen35moe.", 40).unwrap();
        assert_eq!(camadas.iter().filter(|c| c.recorrente).count(), 1);
        assert_eq!(taxa_kv(&camadas).unwrap(), 39 * (256 + 256) * 2 * 2);
        assert_eq!(estado_recorrente(&md, "qwen35moe.", 40).unwrap(), 2_195_456);
    }

    #[test]
    fn sem_a_chave_de_periodo_o_padrao_do_motor_e_quatro() {
        let mut md = mapa_de(&qwen35a3b());
        md.remove("qwen35moe.full_attention_interval");
        // 4 é o padrão do próprio motor, então o A3B sem a chave marca as mesmas
        // 30 camadas — não é um chute nosso, é a linha 18 de `qwen35.cpp`.
        let memoria = memoria_recorrente(&md, "qwen35moe.", 40).unwrap();
        assert_eq!(memoria.recorrentes.iter().filter(|r| **r).count(), 30);
        // Com 8 camadas o mesmo período dá 6 lineares e 2 de cache.
        let oito = memoria_recorrente(&md, "qwen35moe.", 8).unwrap();
        assert_eq!(
            oito.recorrentes,
            vec![true, true, true, false, true, true, true, false]
        );
    }

    #[test]
    fn um_arquivo_denso_nao_abre_estado_recorrente() {
        // A regra é lida do cabeçalho, não de uma lista de arquiteturas: sem as
        // chaves do gated delta net, nenhuma camada deixa de pagar cache.
        let md = mapa_de(&granite());
        assert_eq!(estado_recorrente(&md, "granite.", 40).unwrap(), 0);
        let camadas = camadas_de_cache(&md, "granite.", 40).unwrap();
        assert!(camadas.iter().all(|c| !c.recorrente));
        assert_eq!(taxa_kv(&camadas).unwrap(), 81_920);
        // Metade das chaves não é híbrido: o motor nem carregaria o arquivo, e a
        // conta não pode passar a cobrar estado de uma camada que ele não declara.
        let sem_inner = {
            let mut md = mapa_de(&qwen35a3b());
            md.remove("qwen35moe.ssm.inner_size");
            md
        };
        assert_eq!(estado_recorrente(&sem_inner, "qwen35moe.", 40).unwrap(), 0);
    }

    #[test]
    fn a_ficha_hibrida_sai_do_cabecalho_escrito_e_entra_na_conta() {
        // O caminho inteiro: os bytes são GGUF v3 de verdade, o parser é o do
        // candle (o da carga), e a ficha que sai deles é a que o veredito obedece.
        let dir = std::env::temp_dir().join(format!("aether-aloj-hibrido-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let caminho = dir.join("qwen35moe.gguf");
        escreve_gguf(&caminho, &qwen35a3b());
        let ficha = fichar(&caminho).expect("um cabeçalho híbrido deve ser medido");
        assert_eq!(ficha.camadas, 40);
        assert_eq!(ficha.janela_maxima, 262_144);
        assert_eq!(ficha.kv_por_token, 20_480);
        assert_eq!(ficha.estado_recorrente, 65_863_680);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_ficha_le_os_array_do_gemma4_sem_recusar_o_arquivo() {
        // O defeito que este teste fecha: `head_count_kv` escrito como array de
        // 48 era lido como metadado ausente, a ficha falhava, e o veredito de
        // alojamento caía em "não medido" — o Gemma 4 do catálogo nunca teve
        // número de placa. Agora o array é a fonte da geometria.
        let dir = std::env::temp_dir().join(format!("aether-aloj-gemma4-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let caminho = dir.join("gemma4.gguf");
        escreve_gguf(&caminho, &gemma4());
        let ficha = fichar(&caminho)
            .expect("um GGUF com head_count_kv e sliding_window_pattern em array deve ser medido");
        assert_eq!(ficha.camadas, 48);
        assert_eq!(ficha.janela_maxima, 262_144);
        assert_eq!(ficha.kv_por_token, 344_064);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn um_array_que_nao_bate_com_as_camadas_e_recusado_pelo_nome() {
        // A mesma regra do `get_key_or_arr` do motor: um array de geometria tem
        // de trazer uma entrada por camada. Aproximar (usar os primeiros, ou o
        // primeiro) produziria um KV de mentira com a cara de medido, então o
        // motivo cita a chave e os dois comprimentos.
        let mut md = mapa_de(&gemma4());
        md.insert(
            "gemma4.attention.head_count_kv".to_string(),
            Valor::Array((0..47).map(|_| Valor::I32(8)).collect()),
        );
        let erro = camadas_de_cache(&md, "gemma4.", 48)
            .expect_err("47 entradas para 48 camadas não é uma geometria lida");
        // Os dois comprimentos estão na mensagem em qualquer idioma, e são eles
        // que separam esta recusa de um simples "metadado ausente".
        let motivo = erro.to_string();
        assert!(
            motivo.contains("gemma4.attention.head_count_kv")
                && motivo.contains("47")
                && motivo.contains("48"),
            "a recusa deve nomear a chave e os dois comprimentos, veio: {motivo}"
        );
    }

    #[test]
    fn quem_le_o_cache_de_outra_camada_nao_paga_cache() {
        // `attention.shared_kv_layers` são as últimas N camadas que abrem mão de
        // cache próprio (`gemma4.cpp:11-18`): a biblioteca não aloca nada para
        // elas, e cobra-lhes os bytes seria dizer que o modelo precisa de mais
        // placa do que ele precisa.
        let mut pares = gemma4();
        pares[12] = ("gemma4.attention.shared_kv_layers", Geometria::Numero(8));
        let camadas = camadas_de_cache(&mapa_de(&pares), "gemma4.", 48).unwrap();
        assert_eq!(camadas.iter().filter(|c| !c.propria).count(), 8);
        assert!(camadas[..40].iter().all(|c| c.propria));
        // As oito descartadas são as camadas 40..47: seis deslizantes de 8.192
        // bytes por token e duas cheias de 2.048, ou 53.248 a menos que o teto
        // sem compartilhamento — o que a placa deixa de reservar.
        assert_eq!(taxa_kv(&camadas).unwrap(), 344_064 - 53_248);
    }

    #[test]
    fn a_janela_deslizante_lida_do_periodo_quando_o_arquivo_nao_escreve_array() {
        // A outra forma que o motor aceita: um número, que é o período do padrão.
        // Com `n = 4` e oito camadas, o que não é o último do grupo é deslizante
        // (`il % n < n - 1`), então 6 deslizantes e 2 cheias.
        let pares = [
            ("x.block_count", Geometria::Numero(8)),
            ("x.embedding_length", Geometria::Numero(2048)),
            ("x.attention.head_count", Geometria::Numero(8)),
            ("x.attention.head_count_kv", Geometria::Numero(4)),
            ("x.attention.key_length", Geometria::Numero(256)),
            ("x.attention.value_length", Geometria::Numero(256)),
            ("x.attention.key_length_swa", Geometria::Numero(128)),
            ("x.attention.value_length_swa", Geometria::Numero(128)),
            ("x.attention.sliding_window_pattern", Geometria::Numero(4)),
        ];
        let camadas = camadas_de_cache(&mapa_de(&pares), "x.", 8).unwrap();
        assert_eq!(camadas.iter().filter(|c| c.k == 128).count(), 6);
        assert_eq!(taxa_kv(&camadas).unwrap(), 6 * 2048 + 2 * 4096);
    }

    #[test]
    fn sem_chave_de_padrao_nada_e_de_janela_deslizante() {
        // Ausência é atenção cheia para todas as camadas — o que o arquivo do
        // granite declara, e o motivo de `sliding_window_pattern` não poder ter
        // um padrão inventado aqui: inventar mudaria a conta sem mudar o modelo.
        let camadas = camadas_de_cache(&mapa_de(&granite()), "granite.", 40).unwrap();
        assert!(camadas.iter().all(|c| c.k == 64 && c.propria));
        // Sem `key_length_swa`, a dimensão deslizante é a cheia (a ordem do
        // motor em `llama-model.cpp:1645-1648`), então o padrão de período 0,
        // que torna todas deslizantes, não muda um byte.
        let mut md = mapa_de(&granite());
        md.insert(
            "granite.attention.sliding_window_pattern".to_string(),
            Valor::U32(0),
        );
        assert_eq!(taxa_kv(&camadas_de_cache(&md, "granite.", 40).unwrap()).unwrap(), 81_920);
    }

    #[test]
    fn as_celulas_da_janela_seguem_o_arredondamento_da_biblioteca() {
        // `n_ctx` é arredondado para cima em múltiplos de 256 antes de o cache
        // ser dimensionado (`llama-context.cpp:409-415`). Toda janela da escala
        // já é múltipla; quem pede torta recebe o que seria alojado, não o que
        // pediu escrito como se fosse medida.
        assert_eq!(celulas_da_janela(0), 0);
        assert_eq!(celulas_da_janela(1), 256);
        assert_eq!(celulas_da_janela(256), 256);
        assert_eq!(celulas_da_janela(4000), 4096);
        assert_eq!(celulas_da_janela(4096), 4096);
        assert_eq!(celulas_da_janela(65_536), 65_536);
    }

    /// O `sched_reserve: Vulkan0 compute buffer size` de treze cargas medidas
    /// com a placa ligada (RX 9060 XT, driver AMD, flash attention), em MiB
    /// inteiros truncados dos 0,02 que a biblioteca escreve. É a única origem
    /// dos números que `folga_de_computacao` cobra: cair em vermelho aqui
    /// significa que o grafo mudou e alguém tem de voltar a medir.
    const COMPUTACAO_MEDIDA: &[(&str, usize, u64)] = &[
        ("granite-4.2-3b", 4_096, 220),
        ("granite-4.2-3b", 32_768, 248),
        ("granite-4.2-3b", 49_152, 264),
        ("granite-4.2-3b", 61_440, 276),
        ("granite-4.2-3b", 65_536, 280),
        ("granite-4.2-3b", 131_072, 344),
        ("ornith-1.5-9b", 4_096, 521),
        ("ornith-1.5-9b", 32_768, 549),
        ("ornith-1.5-9b", 49_152, 565),
        ("ornith-1.5-9b", 61_440, 577),
        ("ornith-1.5-9b", 65_536, 581),
        ("ornith-1.5-9b", 131_072, 645),
        ("ornith-1.5-9b", 262_144, 773),
    ];

    #[test]
    fn a_folga_cobre_cada_buffer_de_computacao_medido_na_placa() {
        // Cobrar a menos é prometer folga que a placa não tem; cobrar a mais
        // sem motivo é encolher a janela que a pessoa podia ter. As duas pontas
        // ficam presas aqui, com a medida como régua.
        for (arquivo, janela, medida) in COMPUTACAO_MEDIDA {
            let cobrada = folga_de_computacao(celulas_da_janela(*janela) as u64) / 1048576;
            assert!(
                cobrada > *medida,
                "{arquivo} em {janela}: a conta cobrou {cobrada} MiB e a placa registrou {medida}"
            );
            // O teto da sobra é a diferença entre os dois pisos medidos
            // (517,02 − 216,02 = 301 MiB) mais o MiB do arredondamento para
            // cima. Passar disso é a folga tendo virado imposto.
            assert!(
                cobrada - medida <= 302,
                "{arquivo} em {janela}: cobrada {cobrada} MiB contra {medida} MiB medidos"
            );
        }
        // E a parte proporcional é do tamanho que foi medido: entre duas janelas
        // da mesma arquitetura, o que a biblioteca registrou cresce exatamente
        // 1.024 bytes por célula — os trinta e seis pares possíveis entre as
        // treze cargas, sem uma exceção.
        for (arquivo, janela, medida) in COMPUTACAO_MEDIDA {
            for (outra, outra_janela, outra_medida) in COMPUTACAO_MEDIDA {
                if arquivo != outra || outra_janela <= janela {
                    continue;
                }
                let celulas = celulas_da_janela(*outra_janela) as u64
                    - celulas_da_janela(*janela) as u64;
                assert_eq!(
                    *outra_medida - medida,
                    celulas / 1024,
                    "{arquivo}: entre {janela} e {outra_janela} a placa não cresceu 1 KiB por célula"
                );
            }
        }
    }
}
