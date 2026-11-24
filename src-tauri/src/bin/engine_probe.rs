//! Sonda de inferência: carrega pesos reais, gera e mede. Sem simulação.
//!
//! uso: engine-probe <dir-do-modelo> <arquivo.gguf> [contexto] [max_tokens] [prompt]

use aether_lib::ai::{ChatMessage, ChatTemplate, Emission, Engine, Generation, SamplingParams, ToolSpec};
use anyhow::Context;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

/// As pastas onde procurar a ggml vendorizada. O executável da sonda sai na
/// mesma pasta do `build.rs`, então o padrão já é certo; `AETHER_DLL_DIR` existe
/// para medir um binário publicado sem mover nada.
fn dlls() -> Vec<PathBuf> {
    let mut pastas = Vec::new();
    if let Some(dir) = std::env::var_os("AETHER_DLL_DIR") {
        pastas.push(PathBuf::from(dir));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(pai) = exe.parent() {
            pastas.push(pai.to_path_buf());
        }
    }
    pastas
}

fn main() -> anyhow::Result<()> {
    // Mesmo cinto do `aether`: ver `src/main.rs` e llama.cpp#15752.
    unsafe { std::env::set_var("GGML_NO_BACKTRACE", "1") };
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("uso: engine-probe <dir> <arquivo.gguf> [contexto] [max_tokens] [prompt]");
        std::process::exit(2);
    }
    let dir = PathBuf::from(&args[1]);
    let weights = args[2].clone();
    let context: usize = args.get(3).and_then(|v| v.parse().ok()).unwrap_or(4096);
    let max_tokens: usize = args.get(4).and_then(|v| v.parse().ok()).unwrap_or(160);
    let prompt = args
        .get(5)
        .cloned()
        .unwrap_or_else(|| "Em uma frase, o que você consegue fazer?".to_string());

    if prompt.starts_with("placas:") {
        return listar_placas();
    }
    if let Some(list) = prompt.strip_prefix("diag:") {
        return diagnose(&dir, &weights, context, list);
    }
    if let Some(text) = prompt.strip_prefix("diff:") {
        return differential(&dir, &weights, context, text);
    }
    if prompt.starts_with("quant:") {
        return quant_kernel_audit(&dir, &weights);
    }
    if let Some(list) = prompt.strip_prefix("ref:") {
        let texts: Vec<String> = list.split('|').map(str::to_string).collect();
        return reference(&dir, &weights, context, &texts);
    }
    // O template de chat contém "<|im_start|>", que o separador '|' do modo ref:
    // picotaria. Aqui cada item é um caminho de arquivo, lido inteiro.
    if let Some(list) = prompt.strip_prefix("refile:") {
        let texts = list
            .split('|')
            .map(std::fs::read_to_string)
            .collect::<Result<Vec<_>, _>>()
            .context("falha ao ler um prompt do disco")?;
        return reference(&dir, &weights, context, &texts);
    }
    if let Some(names) = prompt.strip_prefix("deq:") {
        return dump_dequantized(&dir, &weights, names);
    }
    if let Some(path) = prompt.strip_prefix("prefill:") {
        return prefill_file(&dir, &weights, context, path, max_tokens);
    }
    if let Some(path) = prompt.strip_prefix("genfile:") {
        let temperature: f64 = args.get(6).and_then(|v| v.parse().ok()).unwrap_or(0.7);
        return generate_file(&dir, &weights, context, path, max_tokens, temperature);
    }
    if let Some(path) = prompt.strip_prefix("kvc:") {
        return kv_consistency(&dir, &weights, context, path, max_tokens);
    }
    // O esquecimento entre turnos: a mesma janela, dois turnos seguidos, sem
    // recarregar nada. É a miniatura do segundo turno de uma conversa real.
    if let Some(path) = prompt.strip_prefix("esquecer:") {
        return dois_turnos(&dir, &weights, context, max_tokens, path);
    }
    // O reaproveitamento do prefixo do KV, medido contra o defeito que ele
    // corrige: dois braços, um com o cache vivo entre rodadas e outro esquecendo
    // tudo antes de cada geração.
    if let Some(path) = prompt.strip_prefix("prefixo:") {
        return prefixo_reutilizado(&dir, &weights, context, max_tokens, path);
    }
    // O veredito de alojamento previsto, contra o delta de VRAM que a carga real
    // produz. `alojar:` traz as janelas separadas por vírgula.
    if let Some(list) = prompt.strip_prefix("alojar:") {
        let janelas: Vec<usize> = list
            .split(',')
            .filter_map(|v| v.trim().parse().ok())
            .collect();
        return alojamento(&dir, &weights, &janelas);
    }

    println!("carregando {weights} (janela {context})...");
    let load_start = Instant::now();
    let mut engine = Engine::load("granite-4.2-3b", &dir, &weights, context, &dlls())?;
    let acelerador = engine.acelerador();
    println!(
        "carregado em {:.2}s | backend={} dispositivo={:?} motivo={:?} janela={} vocab={}",
        load_start.elapsed().as_secs_f64(),
        acelerador.tipo,
        acelerador.dispositivo,
        acelerador.motivo,
        engine.context(),
        engine.vocabulario(),
    );
    if let Some(config) = engine.config() {
        println!(
            "granite: camadas={} hidden={} heads={}/{} head_dim={} vocab={}",
            config.block_count,
            config.embedding_length,
            config.head_count,
            config.head_count_kv,
            config.head_dim,
            config.vocab_size,
        );
        println!(
            "escalares: attn={} emb={} resid={} logit={}",
            config.attention_scale,
            config.embedding_scale,
            config.residual_scale,
            config.logit_scale,
        );
    }
    println!("eos={:?}", engine.eos_tokens());

    let template = ChatTemplate::from_file(&dir.join("chat_template.jinja"))?;
    let tools = vec![ToolSpec {
        name: "everything__echo".into(),
        description: "Echoes back the given message.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": { "message": { "type": "string", "description": "Message to echo" } },
            "required": ["message"]
        }),
        alvo: Default::default(),
    }];
    let nomes: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    let raw = prompt.strip_prefix("raw:");    let rendered = match raw {
        Some(text) => text.to_string(),
        None => {
            let messages = vec![
                ChatMessage::new(
                    "system",
                    "Você é Aether, um assistente local. Responda em português, de forma breve. \
                     Use as ferramentas quando elas forem necessárias; caso contrário responda direto.",
                ),
                ChatMessage::new("user", prompt),
            ];
            template.render(&messages, &tools, false)?
        }
    };
    println!("--- prompt renderizado ({} bytes) ---\n{rendered}\n--- fim ---", rendered.len());

    let tokens = engine
        .tokenizer()
        .encode(rendered.as_str(), false)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let ids: Vec<u32> = tokens.get_ids().to_vec();
    println!("prompt: {} tokens", ids.len());

    let temperature: f64 = args.get(6).and_then(|v| v.parse().ok()).unwrap_or(0.7);
    let params = SamplingParams { max_tokens, temperature, ..SamplingParams::default() };
    let cancel = AtomicBool::new(false);
    let mut stdout = std::io::stdout();
    println!("--- geração ---");
    let generation = engine.generate(
        &ids,
        &params,
        &["<|im_start|>".to_string()],
        &cancel,
        |emission| match emission {
            Emission::Delta(text) => {
                print!("{text}");
                let _ = stdout.flush();
            }
            Emission::Replace(text) => {
                println!("\n[truncado para] {text}");
            }
        },
    )?;
    println!("\n--- fim da geração ---");
    println!(
        "parada={:?} | prompt={} tokens em {}ms ({:.1} t/s) | gerados={} tokens em {}ms ({:.2} t/s)",
        generation.stop,
        generation.prompt_tokens,
        generation.prompt_ms,
        generation.prefill_tokens_per_second(),
        generation.generated_tokens,
        generation.decode_ms,
        generation.decode_tokens_per_second(),
    );
    let calls = aether_lib::ai::chat::parse_tool_calls(&generation.text, &nomes);
    println!("chamadas de ferramenta detectadas: {}", calls.len());
    for call in &calls {
        println!("  {} {}", call.name, call.arguments);
    }
    println!("texto limpo: {:?}", aether_lib::ai::chat::strip_tool_calls(&generation.text, &nomes));
    Ok(())
}

/// O que a biblioteca vê na placa agora: a VRAM livre da escolhida, lida do
/// registro bruto. É a mesma medição que `alojar()` recebe, então prever e
/// carregar olham para o mesmo número.
fn vram_livre(pastas: &[PathBuf]) -> Option<(String, u64)> {
    use aether_lib::ai::ggml;
    let dispositivos = ggml::registro(pastas).ok()?;
    let escolhida = ggml::escolher(&dispositivos)?;
    Some((escolhida.nome.clone(), dispositivos[escolhida.indice].livre as u64))
}

fn mib(bytes: u64) -> String {
    format!("{:.2} MiB", bytes as f64 / 1048576.0)
}

/// A sonda do alojamento: o veredito previsto do arquivo + da placa, e o delta
/// real de VRAM que a carga produz. Sem o delta, a previsão seria só uma conta
/// bonita; é a comparação que diz se o módulo mede o que a biblioteca faz.
fn alojamento(dir: &PathBuf, weights: &str, janelas: &[usize]) -> anyhow::Result<()> {
    use aether_lib::ai::alojamento::{
        Alojamento, alojar, capacidade, celulas_da_janela, despacho, fichar, media_da_camada,
    };
    use aether_lib::ai::ggml;

    let caminho = dir.join(weights);
    let ficha = fichar(&caminho)?;
    println!("arquivo: {}", ficha.arquivo);
    println!(
        "geometria: arquitetura={} camadas={} janela_maxima={}",
        ficha.arquitetura, ficha.camadas, ficha.janela_maxima
    );
    println!(
        "pesos: {} (fixo fora de camadas {}, média por camada {})",
        mib(ficha.pesos_total),
        mib(ficha.fixo),
        mib(media_da_camada(&ficha))
    );
    println!(
        "memória: {} de kv por token + {} de estado recorrente fixo",
        mib(ficha.kv_por_token),
        mib(ficha.estado_recorrente)
    );

    let pastas = dlls();
    let cap = capacidade(&pastas)?;
    match &cap {
        Some(c) => println!(
            "placa escolhida: {} | livre {} de {}",
            c.placa,
            mib(c.livre),
            mib(c.total)
        ),
        None => println!("placa escolhida: nenhuma — esta máquina é o caso de CPU"),
    }

    for janela in janelas {
        let veredito = alojar(&ficha, cap.as_ref(), *janela);
        match veredito {
            Alojamento::Inteira { pesos, kv, estado, livre } => println!(
                "  janela {janela}: INTEIRA na placa (pesos {} + kv {} + estado {} = {} de {} livres)",
                mib(pesos),
                mib(kv),
                mib(estado),
                mib(pesos + kv + estado),
                mib(livre)
            ),
            Alojamento::Dividida { camadas, total, na_placa, em_ram, estado, livre } => println!(
                "  janela {janela}: DIVIDIDA — {camadas}/{total} camadas na placa ({}, estado {} em f32), resto em RAM ({}) | livre {}",
                mib(na_placa),
                mib(estado),
                mib(em_ram),
                mib(livre)
            ),
            Alojamento::SoSistema { pesos, kv, estado } => println!(
                "  janela {janela}: SÓ SISTEMA (pesos {}, kv {}, estado {})",
                mib(pesos),
                mib(kv),
                mib(estado)
            ),
        }
    }

    // A carga real, com o registro conferido antes e depois. O primeiro motor é
    // o previsto; o número da direita é o que a placa realmente cedeu. O despacho
    // é o mesmo que o aplicativo passa: o veredito desta janela, em camadas.
    let Some(context) = janelas.first().copied() else {
        return Ok(());
    };
    let Some((_, antes)) = vram_livre(&pastas) else {
        println!("carga: nenhuma placa registrada — a sonda de alojamento mede o delta de VRAM");
        return Ok(());
    };
    let veredito = alojar(&ficha, cap.as_ref(), context);
    let camadas = despacho(&veredito);
    let kv = ficha.kv_por_token * celulas_da_janela(context) as u64;
    // O estado recorrente é reserva fixa da placa, não proporcional à janela:
    // entra no esperado do delta mesmo quando o modelo divide as camadas.
    let estado = ficha.estado_recorrente;
    let na_placa = match veredito {
        Alojamento::Inteira { pesos, .. } => pesos + kv + estado,
        Alojamento::Dividida { na_placa, .. } => na_placa + kv + estado,
        Alojamento::SoSistema { .. } => 0,
    };
    let inicio = Instant::now();
    let motor = ggml::Motor::novo(&pastas, &caminho, context, camadas)
        .map_err(|e| anyhow::anyhow!("carga falhou: {e}"))?;
    // A segunda leitura é a metade medida do delta. Repetir o valor de antes
    // quando a placa não respondeu produziria um "0 MiB cedidos" que parece uma
    // medição e é a ausência dela.
    let cedido = vram_livre(&pastas).map(|(_, depois)| antes.saturating_sub(depois));
    println!(
        "carga real em {:.2}s: janela={} dispositivo={} despacho={} camadas | VRAM cedida {} | previsto na placa {} | diferença {}",
        inicio.elapsed().as_secs_f64(),
        motor.janela,
        motor.dispositivo,
        camadas,
        match cedido {
            Some(cedido) => mib(cedido),
            None => "não medida".to_string(),
        },
        mib(na_placa),
        match cedido {
            Some(cedido) => mib(cedido.max(na_placa) - cedido.min(na_placa)),
            None => "não medida".to_string(),
        }
    );
    Ok(())
}

/// O registro Vulkan visto pelo nosso próprio carregador, com a escolha que ele
/// produz. Sem isto não dá para conferir se a placa preferida é a que o
/// aplicativo realmente usa — nem que a ordem do registro é outra.
fn listar_placas() -> anyhow::Result<()> {
    use aether_lib::ai::ggml;

    let pastas = dlls();
    println!("buscando em: {}", pastas.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(" | "));
    let dispositivos = ggml::registro(&pastas)?;
    for d in &dispositivos {
        println!(
            "  [{}] {:<40} tipo={} livre={} MiB total={} MiB",
            d.indice,
            d.nome,
            d.tipo,
            d.livre / (1024 * 1024),
            d.total / (1024 * 1024),
        );
    }
    println!("candidatas, na ordem de preferência:");
    for p in ggml::Motor::placas(&pastas)? {
        println!(
            "  [{}] {:<40} dedicada={} total={} MiB",
            p.indice, p.nome, p.dedicada, p.total / (1024 * 1024)
        );
    }
    match ggml::escolher(&dispositivos) {
        Some(p) => println!("escolhida: [{}] {}", p.indice, p.nome),
        None => println!("escolhida: nenhuma — este computador é o caso de CPU"),
    }
    Ok(())
}

/// Instrumento de diagnóstico: carrega os pesos uma vez e, para cada prompt,
/// imprime os ids tokenizados e a distribuição real dos logits do prefill.
/// Isto separa "o prefill está errado" de "o decode está errado" sem chute.
fn diagnose(dir: &PathBuf, weights: &str, context: usize, list: &str) -> anyhow::Result<()> {
    use candle::quantized::gguf_file;
    use candle::Device;

    let tokenizer = tokenizers::Tokenizer::from_file(dir.join("tokenizer.json"))
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let device = Device::Cpu;
    let path = dir.join(weights);
    let mut file = std::fs::File::open(&path)?;
    let content = gguf_file::Content::read(&mut file)?;
    let start = Instant::now();
    let mut model = aether_lib::ai::granite::Model::from_gguf(content, &mut file, &device, context)?;
    println!("carregado em {:.2}s | attn={}", start.elapsed().as_secs_f64(), model.config.attention_scale);

    let prompts: Vec<&str> = if list.trim().is_empty() {
        vec![
            "A capital do Brasil é",
            "The capital of France is",
            "1 2 3 4 5 6 7",
            "O céu é",
            "def soma(a, b):",
        ]
    } else {
        list.split('|').collect()
    };

    for prompt in prompts {
        let enc = tokenizer.encode(prompt, false).map_err(|e| anyhow::anyhow!("{e}"))?;
        let ids: Vec<u32> = enc.get_ids().to_vec();
        println!("\n================ {prompt:?} ================");
        probe_prompt(&mut model, &tokenizer, &device, &ids, 1)?;
    }
    Ok(())
}

/// Prefill + N passos de decode guloso, com a distribuição real de cada passo.
/// É o instrumento do caminho de chat: separa "o prompt está errado" de
/// "o decode está errado" sem depender do streamer nem do sampler.
fn probe_prompt(
    model: &mut aether_lib::ai::granite::Model,
    tokenizer: &tokenizers::Tokenizer,
    device: &candle::Device,
    ids: &[u32],
    steps: usize,
) -> anyhow::Result<()> {
    use candle::Tensor;
    let piece = |id: u32| tokenizer.decode(&[id], true).unwrap_or_default();
    println!("ids ({}) = {:?}", ids.len(), ids);
    println!("peças = {:?}", ids.iter().map(|&i| piece(i)).collect::<Vec<_>>());

    model.clear_kv_cache();
    let input = Tensor::new(ids, device)?.reshape((1, ids.len()))?;
    let t0 = Instant::now();
    let logits = model.forward(&input, 0)?.squeeze(0)?;
    let prefill_ms = t0.elapsed().as_millis();
    let values = logits.to_vec1::<f32>()?;

    let max = values.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    let min = values.iter().cloned().fold(f32::INFINITY, f32::min);
    let mean = values.iter().sum::<f32>() / values.len() as f32;
    let var = values.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / values.len() as f32;
    let exps: Vec<f32> = values.iter().map(|v| (v - max).exp()).collect();
    let sum: f32 = exps.iter().sum();
    let entropy = -exps
        .iter()
        .map(|e| {
            let p = e / sum;
            if p > 0.0 { p * p.ln() } else { 0.0 }
        })
        .sum::<f32>();
    println!(
        "prefill {prefill_ms}ms | logits max={max:.3} min={min:.3} média={mean:.3} desvio={:.3} | entropia={entropy:.3} nats (máx {:.3})",
        var.sqrt(),
        (values.len() as f32).ln()
    );

    let mut order: Vec<usize> = (0..values.len()).collect();
    order.sort_unstable_by(|&a, &b| values[b].total_cmp(&values[a]));
    println!("top-15 do prefill:");
    for &id in order.iter().take(15) {
        println!(
            "  {:>7}  {:>9.3}  p={:.4}  {:?}",
            id,
            values[id],
            exps[id] / sum,
            piece(id as u32)
        );
    }

    let mut next = order[0] as u32;
    let mut pos = ids.len();
    let mut emitted = vec![next];
    for step in 0..steps {
        if next == 100257 || next == 100256 {
            println!("decode parou no token especial {} ({:?})", next, piece(next));
            break;
        }
        let input = Tensor::new(&[next], device)?.reshape((1, 1))?;
        let t1 = Instant::now();
        let lv = model.forward(&input, pos)?.squeeze(0)?.to_vec1::<f32>()?;
        let ms = t1.elapsed().as_millis();
        let mut o: Vec<usize> = (0..lv.len()).collect();
        o.sort_unstable_by(|&a, &b| lv[b].total_cmp(&lv[a]));
        let mx = lv[o[0]];
        let ex: Vec<f32> = o.iter().take(5).map(|&i| (lv[i] - mx).exp()).collect();
        let sm: f32 = ex.iter().sum();
        println!(
            "passo {:>3} ({}ms) {:>7} {:?} | top-3: {}",
            step + 1,
            ms,
            next,
            piece(next),
            o.iter()
                .take(3)
                .enumerate()
                .map(|(k, &i)| format!("{:?} p={:.3}", piece(i as u32), ex[k] / sm))
                .collect::<Vec<_>>()
                .join("  ")
        );
        next = o[0] as u32;
        emitted.push(next);
        pos += 1;
    }
    println!(
        "texto gerado: {:?}",
        tokenizer.decode(&emitted, true).unwrap_or_default()
    );
    Ok(())
}

/// Lê o prompt renderizado de um arquivo (o template ChatML contém '|', que o
/// modo diag: usaria como separador) e instrumenta prefill + decode.
/// Caminho real do produto: Engine::generate com amostragem, prompt lido do
/// disco. O modo prefill: usa argmax, que não é como o modelo foi ajustado para
/// rodar (generation_config.json pede do_sample=true, temp=1.0, top_p=0.95).
fn generate_file(
    dir: &PathBuf,
    weights: &str,
    context: usize,
    path: &str,
    max_tokens: usize,
    temperature: f64,
) -> anyhow::Result<()> {
    let mut engine = Engine::load("granite-4.2-3b", dir, weights, context, &dlls())?;
    let text = std::fs::read_to_string(path)?;
    let ids: Vec<u32> = engine
        .tokenizer()
        .encode(text.as_str(), false)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .get_ids()
        .to_vec();
    let env_f64 = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<f64>().ok());
    let params = SamplingParams {
        temperature,
        top_k: env_f64("AETHER_TOP_K").map(|v| v as usize),
        top_p: env_f64("AETHER_TOP_P"),
        seed: env_f64("AETHER_SEED").map(|v| v as u64).unwrap_or(42),
        max_tokens,
        ..SamplingParams::default()
    };
    println!(
        "genfile {path}: {} tokens | temp={} top_k={:?} top_p={:?} seed={} max={}",
        ids.len(), params.temperature, params.top_k, params.top_p, params.seed, params.max_tokens
    );
    let cancel = AtomicBool::new(false);
    let mut stdout = std::io::stdout();
    let generation = engine.generate(
        &ids,
        &params,
        &[concat!("<", "|im_start|>").to_string()],
        &cancel,
        |emission| match emission {
            Emission::Delta(t) => {
                print!("{t}");
                let _ = stdout.flush();
            }
            Emission::Replace(t) => println!("\n[truncado para] {t}"),
        },
    )?;
    println!("\n--- fim ---");
    let acelerador = engine.acelerador();
    println!(
        "backend={} dispositivo={:?} | parada={:?} | prompt={} tok em {}ms ({:.1} t/s) | gerados={} tok em {}ms ({:.2} t/s)",
        acelerador.tipo,
        acelerador.dispositivo,
        generation.stop,
        generation.prompt_tokens,
        generation.prompt_ms,
        generation.prefill_tokens_per_second(),
        generation.generated_tokens,
        generation.decode_ms,
        generation.decode_tokens_per_second(),
    );
    println!("texto bruto: {:?}", generation.text);
    // Prompt cru, nenhuma ferramenta conectada: aqui nada é chamada, só texto.
    println!(
        "texto limpo: {:?}",
        aether_lib::ai::chat::strip_tool_calls(&generation.text, &[])
    );
    Ok(())
}

fn prefill_file(
    dir: &PathBuf,
    weights: &str,
    context: usize,
    path: &str,
    steps: usize,
) -> anyhow::Result<()> {
    use candle::quantized::gguf_file;
    use candle::Device;

    let tokenizer = tokenizers::Tokenizer::from_file(dir.join("tokenizer.json"))
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let device = Device::Cpu;
    let mut file = std::fs::File::open(dir.join(weights))?;
    let content = gguf_file::Content::read(&mut file)?;
    let mut model =
        aether_lib::ai::granite::Model::from_gguf(content, &mut file, &device, context)?;
    let text = std::fs::read_to_string(path)?;
    println!("prompt de {path}: {} bytes", text.len());
    let ids = tokenizer
        .encode(text.as_str(), false)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .get_ids()
        .to_vec();
    probe_prompt(&mut model, &tokenizer, &device, &ids, steps)
}

/// Prefill de uma vez versus token a token pelo KV cache: os logits têm de ser
/// os mesmos. Varre todo prefixo para achar a primeira posição que diverge.
fn differential(dir: &PathBuf, weights: &str, context: usize, text: &str) -> anyhow::Result<()> {
    use candle::quantized::gguf_file;
    use candle::{Device, Tensor};

    let tokenizer = tokenizers::Tokenizer::from_file(dir.join("tokenizer.json"))
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let device = Device::Cpu;
    let mut file = std::fs::File::open(dir.join(weights))?;
    let content = gguf_file::Content::read(&mut file)?;
    let mut model = aether_lib::ai::granite::Model::from_gguf(content, &mut file, &device, context)?;

    let ids: Vec<u32> = tokenizer
        .encode(text, false)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .get_ids()
        .to_vec();
    let piece = |id: u32| tokenizer.decode(&[id], true).unwrap_or_default();
    println!("prompt {text:?} -> {} tokens", ids.len());

    let prefill = |model: &mut aether_lib::ai::granite::Model, tokens: &[u32], pos: usize| -> anyhow::Result<Vec<f32>> {
        let input = Tensor::new(tokens, &device)?.reshape((1, tokens.len()))?;
        Ok(model.forward(&input, pos)?.squeeze(0)?.to_vec1::<f32>()?)
    };

    // Caminho incremental: um token por vez, mask=None, rope no ramo t==1.
    model.clear_kv_cache();
    let mut incremental: Vec<Vec<f32>> = Vec::with_capacity(ids.len());
    for (i, &id) in ids.iter().enumerate() {
        incremental.push(prefill(&mut model, &[id], i)?);
    }

    // Caminho em blocos de 16: exercise o ramo t>1 com cache já preenchido.
    model.clear_kv_cache();
    let mut chunked: Vec<f32> = Vec::new();
    let mut pos = 0usize;
    while pos < ids.len() {
        let end = (pos + 16).min(ids.len());
        chunked = prefill(&mut model, &ids[pos..end], pos)?;
        pos = end;
    }

    let argmax = |v: &[f32]| -> usize {
        v.iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, _)| i)
            .unwrap_or(0)
    };
    let stats = |a: &[f32], b: &[f32]| -> (f32, f32, f32) {
        let mut max = 0f32;
        let mut sum = 0f32;
        let (mut dot, mut na, mut nb) = (0f32, 0f32, 0f32);
        for (x, y) in a.iter().zip(b) {
            let d = (x - y).abs();
            max = max.max(d);
            sum += d;
            dot += x * y;
            na += x * x;
            nb += y * y;
        }
        (max, sum / a.len() as f32, dot / (na.sqrt() * nb.sqrt()))
    };

    println!("\n=== varredura de prefixos: prefill completo vs incremental ===");
    println!("  n   argmax_prefill   argmax_incr   max|d|    méd|d|    cos");
    let mut first_divergence = None;
    for n in 1..=ids.len() {
        model.clear_kv_cache();
        let full = prefill(&mut model, &ids[..n], 0)?;
        let inc = &incremental[n - 1];
        let (max, mean, cos) = stats(&full, inc);
        let (af, ai) = (argmax(&full), argmax(inc));
        let mark = if af == ai { "" } else { "  <-- DIVERGE" };
        if af != ai && first_divergence.is_none() {
            first_divergence = Some(n);
        }
        println!(
            "{:>3}   {:>7} {:<10} {:>7} {:<10} {:>8.4}  {:>8.4}  {:>7.5}{mark}",
            n,
            af,
            format!("{:?}", piece(af as u32)),
            ai,
            format!("{:?}", piece(ai as u32)),
            max,
            mean,
            cos
        );
    }

    model.clear_kv_cache();
    let full = prefill(&mut model, &ids, 0)?;
    let last = incremental.last().expect("prompt não vazio");
    let (max, mean, cos) = stats(&full, last);
    println!("\n=== prompt completo ===");
    println!("prefill vs incremental: max|d|={max:.5} méd|d|={mean:.5} cos={cos:.6}");
    let (max, mean, cos) = stats(&full, &chunked);
    println!("prefill vs blocos16:    max|d|={max:.5} méd|d|={mean:.5} cos={cos:.6}");
    println!("primeira divergência de argmax: {first_divergence:?}");

    for (name, v) in [("prefill", &full), ("incremental", last), ("blocos16", &chunked)] {
        let mut order: Vec<usize> = (0..v.len()).collect();
        order.sort_unstable_by(|&a, &b| v[b].total_cmp(&v[a]));
        let top: Vec<String> = order
            .iter()
            .take(8)
            .map(|&i| format!("{}={:.2}", piece(i as u32), v[i]))
            .collect();
        println!("{name:<12} top-8: {}", top.join(" | "));
    }
    Ok(())
}

/// O kernel quantizado do candle contra o próprio dequantize dele: para cada
/// tensor e cada número de linhas, `QMatMul::forward` tem de bater com
/// `x @ Wᵀ` em f32. Se divergir só quando m>1, o defeito é o caminho em bloco.
fn quant_kernel_audit(dir: &PathBuf, weights: &str) -> anyhow::Result<()> {
    use candle::quantized::{QMatMul, gguf_file};
    use candle::{Device, Tensor};
    use candle_nn::Module;

    let device = Device::Cpu;
    let mut file = std::fs::File::open(dir.join(weights))?;
    let ct = gguf_file::Content::read(&mut file)?;

    let names = [
        "blk.0.attn_q.weight",
        "blk.0.attn_v.weight",
        "blk.0.ffn_down.weight",
        "blk.20.attn_q.weight",
        "blk.20.ffn_down.weight",
        "blk.39.attn_v.weight",
        "output.weight",
    ];
    let rows = [1usize, 2, 3, 5, 8, 16, 32, 64, 128];

    for name in names {
        let t = match ct.tensor(&mut file, name, &device) {
            Ok(t) => t,
            Err(e) => {
                println!("{name}: AUSENTE ({e})");
                continue;
            }
        };
        let w = t.dequantize(&device)?;
        let (n, k) = (w.dim(0)?, w.dim(1)?);
        let wt = w.t()?.contiguous()?;
        println!("\n=== {name} {:?} shape=({n},{k}) ===", t.dtype());

        let qmm = QMatMul::from_qtensor(t)?;
        for m in rows {
            let mut seed = 0x1234_5678_9abc_def0u64;
            let mut next = move || {
                seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                ((seed >> 33) as f32 / (1u64 << 31) as f32) - 1.0
            };
            let data: Vec<f32> = (0..m * k).map(|_| next()).collect();
            let x = Tensor::from_slice(&data, (1, m, k), &device)?;
            let x2 = Tensor::from_slice(&data, (m, k), &device)?;

            let got = qmm.forward(&x)?;
            let want = x2.matmul(&wt)?;
            let g = got.flatten_all()?.to_vec1::<f32>()?;
            let wv = want.flatten_all()?.to_vec1::<f32>()?;

            let mut max = 0f32;
            let mut sum = 0f32;
            let mut scale = 0f32;
            let count = g.len();
            for (a, b) in g.iter().zip(wv.iter()) {
                let d = (a - b).abs();
                max = max.max(d);
                sum += d;
                scale += b * b;
            }
            let rms = (scale / count as f32).sqrt();
            println!(
                "  m={m:>4}  max|d|={max:.6}  méd|d|={:.6}  rms(sinal)={rms:.4}  max/rms={:.5}",
                sum / count as f32,
                if rms > 0.0 { max / rms } else { 0.0 }
            );
        }
    }
    Ok(())
}

/// Oráculo independente: pesos dequantizados em f32 e RoPE, máscara, softmax e
/// GQA escritos do zero aqui, sem tocar no motor. Se a referência acerta onde o
/// motor erra, o defeito está no motor; se erra igual, está na minha leitura da
/// arquitetura.
/// Prefill de n+k tokens contra prefill n + k passos de decode, no MESMO motor.
/// As duas rotas têm de dar os mesmos logits: qualquer diferença é bug de cache
/// KV, sem o ruído numérico de comparar implementações distintas.
fn kv_consistency(
    dir: &PathBuf,
    weights: &str,
    context: usize,
    path: &str,
    steps: usize,
) -> anyhow::Result<()> {
    use aether_lib::ai::granite;
    use candle::quantized::gguf_file;
    use candle::{Device, Tensor};

    let tokenizer = tokenizers::Tokenizer::from_file(dir.join("tokenizer.json"))
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let text = std::fs::read_to_string(path).context("falha ao ler o prompt")?;
    let ids: Vec<u32> = tokenizer
        .encode(text.as_str(), false)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .get_ids()
        .to_vec();
    let n = ids.len();
    let steps = steps.max(1);
    let device = Device::Cpu;
    let mut file = std::fs::File::open(dir.join(weights))?;
    let ct = gguf_file::Content::read(&mut file)?;
    let mut model = granite::Model::from_gguf(ct, &mut file, &device, context)?;
    println!("kvc: prompt={n} tokens | {steps} passos | janela={context}");

    let argmax = |lg: &[f32]| -> usize {
        lg.iter()
            .enumerate()
            .fold(0, |best, (i, v)| if *v > lg[best] { i } else { best })
    };
    let top = |lg: &[f32], k: usize| -> Vec<(usize, f32)> {
        let mut order: Vec<usize> = (0..lg.len()).collect();
        order.sort_by(|&a, &b| lg[b].partial_cmp(&lg[a]).unwrap());
        order.into_iter().take(k).map(|i| (i, lg[i])).collect()
    };

    // Rota incremental: um prefill, depois um token por passo, cache vivo.
    model.clear_kv_cache();
    let input = Tensor::new(ids.as_slice(), &device)?.reshape((1, n))?;
    let mut logits = model.forward(&input, 0)?.squeeze(0)?.to_vec1::<f32>()?;
    let mut chosen: Vec<u32> = Vec::new();
    let mut incremental: Vec<Vec<f32>> = Vec::new();
    for step in 1..=steps {
        let t = argmax(&logits) as u32;
        chosen.push(t);
        let input = Tensor::new(&[t], &device)?.reshape((1, 1))?;
        logits = model
            .forward(&input, n + step - 1)?
            .squeeze(0)?
            .to_vec1::<f32>()?;
        incremental.push(logits.clone());
    }
    println!("tokens escolhidos: {chosen:?}");

    // Rota densa: o prompt inteiro mais os mesmos tokens, num prefill só.
    for step in 1..=steps {
        let mut full = ids.clone();
        full.extend_from_slice(&chosen[..step]);
        model.clear_kv_cache();
        let input = Tensor::new(full.as_slice(), &device)?.reshape((1, full.len()))?;
        let dense = model.forward(&input, 0)?.squeeze(0)?.to_vec1::<f32>()?;
        let inc = &incremental[step - 1];

        let mut maxd = 0f32;
        let mut sumd = 0f32;
        let mut dot = 0f32;
        let mut na = 0f32;
        let mut nb = 0f32;
        for (a, b) in inc.iter().zip(dense.iter()) {
            let d = (a - b).abs();
            maxd = maxd.max(d);
            sumd += d;
            dot += a * b;
            na += a * a;
            nb += b * b;
        }
        let ti = argmax(inc);
        let td = argmax(&dense);
        println!(
            "passo {step}: {} | inc top1={ti} denso top1={td} | max|d|={maxd:.5} méd|d|={:.5} cos={:.8}",
            if ti == td { "IGUAL  " } else { "DIVERGE" },
            sumd / inc.len() as f32,
            dot / (na.sqrt() * nb.sqrt()),
        );
        if step == 1 {
            println!("  inc   top5: {:?}", top(inc, 5));
            println!("  denso top5: {:?}", top(&dense, 5));
        }
    }
    Ok(())
}

fn reference(dir: &PathBuf, weights: &str, context: usize, texts: &[String]) -> anyhow::Result<()> {
    use candle::quantized::gguf_file;
    use candle::{Device, IndexOp, Tensor, D};

    let tokenizer = tokenizers::Tokenizer::from_file(dir.join("tokenizer.json"))
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let device = Device::Cpu;
    let mut file = std::fs::File::open(dir.join(weights))?;
    let ct = gguf_file::Content::read(&mut file)?;
    let md = &ct.metadata;
    let u32_of = |k: &str| -> anyhow::Result<usize> {
        Ok(md
            .get(k)
            .ok_or_else(|| anyhow::anyhow!("metadado ausente: {k}"))?
            .to_u32()? as usize)
    };
    let f32_of = |k: &str, d: f32| -> f32 {
        md.get(k).and_then(|v| v.to_f32().ok()).unwrap_or(d)
    };

    let hidden = u32_of("granite.embedding_length")?;
    let heads = u32_of("granite.attention.head_count")?;
    let kv_heads = u32_of("granite.attention.head_count_kv")?;
    let blocks = u32_of("granite.block_count")?;
    let hd = hidden / heads;
    let rope_dim = u32_of("granite.rope.dimension_count")?;
    if rope_dim != hd {
        anyhow::bail!("referência assume RoPE total: rope_dim={rope_dim} head_dim={hd}");
    }
    let base = f32_of("granite.rope.freq_base", 10_000.0);
    let base = match std::env::var("AETHER_REF_ROPE_BASE") {
        Ok(v) => v.parse::<f32>().context("AETHER_REF_ROPE_BASE inválido")?,
        Err(_) => base,
    };
    let eps = f32_of("granite.attention.layer_norm_rms_epsilon", 1e-5) as f64;
    let scale = match f32_of("granite.attention.scale", 0.0) {
        0.0 => 1.0 / (hd as f32).sqrt(),
        s => s,
    };
    // Ablação: o GGUF declara 1/head_dim, mas a convenção usual é 1/sqrt(head_dim).
    // Sem medir os dois não dá para saber qual o modelo foi treinado com.
    let scale = match std::env::var("AETHER_REF_ATTENTION_SCALE") {
        Ok(v) => v.parse::<f32>().context("AETHER_REF_ATTENTION_SCALE inválido")?,
        Err(_) => scale,
    };
    println!("atenção: escala={scale} (1/sqrt(head_dim)={})", 1.0 / (hd as f32).sqrt());
    let emb_scale = f32_of("granite.embedding_scale", 1.0);
    let res_scale = f32_of("granite.residual_scale", 1.0);
    let logit_scale = f32_of("granite.logit_scale", 1.0);
    let rep = heads / kv_heads;
    let half = rope_dim / 2;

    println!(
        "referência densa f32 | head_dim={hd} heads={heads}/{kv_heads} rep={rep} rope_dim={rope_dim} base={base} attn_scale={scale}"
    );

    let dense = |f: &mut std::fs::File, name: &str| -> anyhow::Result<Tensor> {
        Ok(ct.tensor(f, name, &device)?.dequantize(&device)?)
    };
    let inv_freq: Vec<f32> = (0..rope_dim)
        .step_by(2)
        .map(|i| 1f32 / base.powf(i as f32 / rope_dim as f32))
        .collect();
    let gqa_tile = std::env::var("AETHER_REF_GQA").as_deref() == Ok("tile");
    let rope_gptj = std::env::var("AETHER_REF_ROPE").as_deref() != Ok("neox");
    println!(
        "ablação: rope_base={base} gqa={} rope={}",
        if gqa_tile { "tile" } else { "interleave" },
        if rope_gptj { "gptj" } else { "neox" }
    );
    let rep_idx = Tensor::new(
        (0..heads)
            .map(|h| if gqa_tile { h % kv_heads } else { h / rep } as u32)
            .collect::<Vec<_>>()
            .as_slice(),
        &device,
    )?;

    let mut file2 = std::fs::File::open(dir.join(weights))?;
    let ct2 = gguf_file::Content::read(&mut file2)?;
    let mut model =
        aether_lib::ai::granite::Model::from_gguf(ct2, &mut file2, &device, context)?;

    for text in texts {
        let ids: Vec<u32> = tokenizer
            .encode(text.as_str(), false)
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .get_ids()
            .to_vec();
        let n = ids.len();
        let piece = |id: u32| tokenizer.decode(&[id], true).unwrap_or_default();
        println!("\n================ referência: {n} tokens ================");
        println!("{}", &text[..text.len().min(100)]);

        let start = Instant::now();
        let want = {
            let mut cos_v = Vec::with_capacity(n * half);
            let mut sin_v = Vec::with_capacity(n * half);
            for p in 0..n {
                for j in 0..half {
                    let a = p as f32 * inv_freq[j];
                    cos_v.push(a.cos());
                    sin_v.push(a.sin());
                }
            }
            let cos = Tensor::from_slice(&cos_v, (1, 1, n, half), &device)?;
            let sin = Tensor::from_slice(&sin_v, (1, 1, n, half), &device)?;
            let rope = |x: &Tensor| -> anyhow::Result<Tensor> {
                if rope_gptj {
                    let nh = x.dim(1)?;
                    let p = x.reshape((1, nh, n, half, 2))?;
                    let x1 = p.narrow(4, 0, 1)?.squeeze(4)?;
                    let x2 = p.narrow(4, 1, 1)?.squeeze(4)?;
                    let a = (x1.broadcast_mul(&cos)? - x2.broadcast_mul(&sin)?)?;
                    let b = (x2.broadcast_mul(&cos)? + x1.broadcast_mul(&sin)?)?;
                    let out = Tensor::cat(&[&a.unsqueeze(4)?, &b.unsqueeze(4)?], 4)?;
                    return Ok(out.reshape((1, nh, n, rope_dim))?);
                }
                let x1 = x.narrow(3, 0, half)?;
                let x2 = x.narrow(3, half, half)?;
                let a = (x1.broadcast_mul(&cos)? - x2.broadcast_mul(&sin)?)?;
                let b = (x2.broadcast_mul(&cos)? + x1.broadcast_mul(&sin)?)?;
                Ok(Tensor::cat(&[&a, &b], 3)?)
            };
            let mut m = vec![0f32; n * n];
            for i in 0..n {
                for j in (i + 1)..n {
                    m[i * n + j] = f32::NEG_INFINITY;
                }
            }
            let mask = Tensor::from_slice(&m, (1, 1, n, n), &device)?;
            let rms = |x: &Tensor, w: &Tensor| -> anyhow::Result<Tensor> {
                let var = (x.sqr()?.mean_keepdim(D::Minus1)? + eps)?;
                Ok(x.broadcast_div(&var.sqrt()?)?.broadcast_mul(w)?)
            };
            let softmax = |x: &Tensor| -> anyhow::Result<Tensor> {
                let e = x.broadcast_sub(&x.max_keepdim(D::Minus1)?)?.exp()?;
                Ok(e.broadcast_div(&e.sum_keepdim(D::Minus1)?)?)
            };
            let r = |t: &Tensor| -> f32 {
                let v = t.flatten_all().unwrap().to_vec1::<f32>().unwrap();
                (v.iter().map(|x| x * x).sum::<f32>() / v.len() as f32).sqrt()
            };

            let emb = dense(&mut file, "token_embd.weight")?;
            let tids = Tensor::new(ids.as_slice(), &device)?;
            let mut h = emb.index_select(&tids, 0)?;
            println!("  rms(embedding)={:.4}", r(&h));
            drop(emb);
            if emb_scale != 1.0 {
                h = (h * emb_scale as f64)?;
            }

            for l in 0..blocks {
                let p = format!("blk.{l}");
                let an = dense(&mut file, &format!("{p}.attn_norm.weight"))?;
                let wq = dense(&mut file, &format!("{p}.attn_q.weight"))?;
                let wk = dense(&mut file, &format!("{p}.attn_k.weight"))?;
                let wv = dense(&mut file, &format!("{p}.attn_v.weight"))?;
                let wo = dense(&mut file, &format!("{p}.attn_output.weight"))?;
                let x = rms(&h, &an)?;
                let q = rope(
                    &x.matmul(&wq.t()?)?
                        .reshape((1, n, heads, hd))?
                        .transpose(1, 2)?
                        .contiguous()?,
                )?;
                let k = rope(
                    &x.matmul(&wk.t()?)?
                        .reshape((1, n, kv_heads, hd))?
                        .transpose(1, 2)?
                        .contiguous()?,
                )?;
                let v = x
                    .matmul(&wv.t()?)?
                    .reshape((1, n, kv_heads, hd))?
                    .transpose(1, 2)?
                    .contiguous()?;
                let h_in = r(&h);
                let k = k.index_select(&rep_idx, 1)?;
                let v = v.index_select(&rep_idx, 1)?;
                let raw = q.matmul(&k.t()?)?;
                let att = softmax(&(&raw * scale as f64)?.broadcast_add(&mask)?)?;
                if l == 0 || l == blocks / 2 || l == blocks - 1 {
                    let r = raw.i((.., .., n - 1, ..))?.flatten_all()?.to_vec1::<f32>()?;
                    let a = att.i((.., .., n - 1, ..))?.flatten_all()?.to_vec1::<f32>()?;
                    let rmax = r.iter().cloned().fold(f32::MIN, f32::max);
                    let rmean = r.iter().sum::<f32>() / r.len() as f32;
                    let mut ent = 0f32;
                    let mut pmax = 0f32;
                    for hh in 0..heads {
                        let row = &a[hh * n..(hh + 1) * n];
                        let mut e = 0f32;
                        let mut mx = 0f32;
                        for &p in row {
                            if p > 0.0 {
                                e -= p * p.ln();
                            }
                            mx = mx.max(p);
                        }
                        ent += e / heads as f32;
                        pmax += mx / heads as f32;
                    }
                    println!(
                        "  camada {l:>2}: q·k bruto max={rmax:.3} méd={rmean:.3} | escalado max={:.3} | softmax peso_máx(média)={pmax:.4} entropia={ent:.3} posições_efetivas={:.1}",
                        rmax * scale,
                        ent.exp()
                    );
                }
                let y = att.matmul(&v)?.transpose(1, 2)?.reshape((n, hidden))?;
                let ao = y.matmul(&wo.t()?)?;
                let ao = if res_scale == 1.0 { ao } else { (ao * res_scale as f64)? };
                let a_rms = r(&ao);
                h = (&h + &ao)?;

                let fnw = dense(&mut file, &format!("{p}.ffn_norm.weight"))?;
                let wg = dense(&mut file, &format!("{p}.ffn_gate.weight"))?;
                let wu = dense(&mut file, &format!("{p}.ffn_up.weight"))?;
                let wd = dense(&mut file, &format!("{p}.ffn_down.weight"))?;
                let x = rms(&h, &fnw)?;
                let act = (candle_nn::ops::silu(&x.matmul(&wg.t()?)?)? * x.matmul(&wu.t()?)?)?;
                let fo = act.matmul(&wd.t()?)?;
                let fo = if res_scale == 1.0 { fo } else { (fo * res_scale as f64)? };
                let f_rms = r(&fo);
                h = (&h + &fo)?;
                // RMS por canal e por token: uma "massive activation" mora em poucos
                // canais, e nesse caso o RMS global alto é propriedade da arquitetura.
                let chan = h.sqr()?.mean(0)?.to_vec1::<f32>()?;
                let tok = h.sqr()?.mean(1)?.to_vec1::<f32>()?;
                let top = |v: &[f32]| {
                    let (i, m) = v
                        .iter()
                        .enumerate()
                        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
                        .unwrap();
                    (i, m.sqrt())
                };
                let (ci, cv) = top(&chan);
                let (ti, tv) = top(&tok);
                let spread = chan.iter().filter(|&&c| c.sqrt() > 1.0).count();
                println!(
                    "  L{l:>2} h {:.3}->{:.3} a={a_rms:.3} f={f_rms:.3} | canal#{ci}={cv:.1} tok#{ti}={tv:.1} canais>1={spread}",
                    h_in,
                    r(&h)
                );
            }
            let on = dense(&mut file, "output_norm.weight")?;
            let h = rms(&h, &on)?.i((n - 1, ..))?.reshape((1, hidden))?;
            let ow = dense(&mut file, "output.weight")?;
            let lg = h.matmul(&ow.t()?)?.flatten_all()?.to_vec1::<f32>()?;
            if logit_scale == 1.0 {
                lg
            } else {
                lg.iter().map(|v| v / logit_scale).collect()
            }
        };
        println!("referência em {:.1}s", start.elapsed().as_secs_f64());

        let report = |name: &str, lg: &[f32]| {
            let max = lg.iter().cloned().fold(f32::MIN, f32::max);
            let sum: f32 = lg.iter().map(|v| (v - max).exp()).sum();
            let mut ent = 0f32;
            for &v in lg {
                let p = (v - max).exp() / sum;
                if p > 0.0 {
                    ent -= p * p.ln();
                }
            }
            let mut order: Vec<usize> = (0..lg.len()).collect();
            order.sort_by(|&a, &b| lg[b].partial_cmp(&lg[a]).unwrap());
            println!(
                "{name}: max={max:.3} média={:.3} entropia={ent:.3} nats",
                lg.iter().sum::<f32>() / lg.len() as f32
            );
            for &i in order.iter().take(10) {
                println!(
                    "    {:>6}  {:>8.3}  p={:.4}  {:?}",
                    i,
                    lg[i],
                    (lg[i] - max).exp() / sum,
                    piece(i as u32)
                );
            }
        };
        report("referência", &want);

        model.clear_kv_cache();
        let input = Tensor::new(ids.as_slice(), &device)?.reshape((1, n))?;
        let got = model.forward(&input, 0)?.squeeze(0)?.to_vec1::<f32>()?;
        report("motor     ", &got);

        let mut maxd = 0f32;
        let mut sumd = 0f32;
        let mut dot = 0f32;
        let mut na = 0f32;
        let mut nb = 0f32;
        for (a, b) in want.iter().zip(got.iter()) {
            let d = (a - b).abs();
            maxd = maxd.max(d);
            sumd += d;
            dot += a * b;
            na += a * a;
            nb += b * b;
        }
        println!(
            "motor vs referência: max|d|={maxd:.4} méd|d|={:.4} cos={:.6}",
            sumd / want.len() as f32,
            dot / (na.sqrt() * nb.sqrt())
        );
    }
    Ok(())
}

/// Escreve o dequantize do candle em f32 cru para conferência externa: todos os
/// outros testes usaram o dequantize do candle como verdade, o que é circular.
fn dump_dequantized(dir: &PathBuf, weights: &str, names: &str) -> anyhow::Result<()> {
    use candle::Device;
    use candle::quantized::gguf_file;
    let device = Device::Cpu;
    let mut file = std::fs::File::open(dir.join(weights))?;
    let ct = gguf_file::Content::read(&mut file)?;
    for name in names.split('|') {
        let t = ct.tensor(&mut file, name, &device)?;
        let d = t.dequantize(&device)?;
        let v = d.flatten_all()?.to_vec1::<f32>()?;
        let path = std::env::temp_dir().join(format!("deq_{}.f32", name.replace('.', "_")));
        let mut out = std::fs::File::create(&path)?;
        for x in &v {
            out.write_all(&x.to_le_bytes())?;
        }
        println!(
            "{name} {:?} shape={:?} -> {} ({} floats)",
            t.dtype(),
            d.dims(),
            path.display(),
            v.len()
        );
    }
    Ok(())
}

/// Dois turnos seguidos no mesmo contexto carregado, sem recarregar nada entre
/// eles. É a miniatura do que a conversa faz: o primeiro turno preenche a janela
/// (o texto do arquivo é repetido até não sobrar espaço para a resposta) e o
/// segundo chega com um prompt maior que a sobra do primeiro.
///
/// O que está em jogo é o esquecimento do motor. Medido no aplicativo com o
/// Ornith 1.5 9B e a `llama.dll` vendorizada, o segundo turno morria em
/// `decode: failed to find a memory slot for batch of size 2048` porque o
/// `llama_memory_seq_rm(mem, -1, -1, -1)` era recusado pela biblioteca e o cache
/// do turno anterior continuava ocupado. A sonda sai com código 1 quando o
/// segundo turno falha — é isso que faz dela um gate e não um relatório.
fn dois_turnos(
    dir: &PathBuf,
    weights: &str,
    context: usize,
    max_tokens: usize,
    path: &str,
) -> anyhow::Result<()> {
    let mut engine = Engine::load("ggml", dir, weights, context, &dlls())?;
    let acelerador = engine.acelerador();
    println!(
        "carga: {} | backend={} dispositivo={:?} janela={}",
        weights, acelerador.tipo, acelerador.dispositivo, engine.context()
    );

    let base = std::fs::read_to_string(path).context("falha ao ler o prompt")?;
    let alvo = engine.context().saturating_sub(max_tokens + 64).max(64);
    let mut cheio = String::new();
    let mut blocos = 0usize;
    while engine.count_tokens(&cheio)? < alvo {
        cheio.push_str(base.trim_end());
        cheio.push('\n');
        blocos += 1;
    }
    println!(
        "turno 1: {} blocos de {} bytes = {} tokens (alvo {}, janela {})",
        blocos,
        base.len(),
        engine.count_tokens(&cheio)?,
        alvo,
        engine.context()
    );

    let params = SamplingParams { max_tokens, temperature: 0.0, ..SamplingParams::default() };
    let stop = [concat!("<", "|im_start|>").to_string()];
    let cancel = AtomicBool::new(false);

    let mut turno = |n: usize, texto: &str| -> anyhow::Result<String> {
        let ids: Vec<u32> = engine
            .tokenizer()
            .encode(texto, false)
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .get_ids()
            .to_vec();
        let inicio = Instant::now();
        let resposta = engine.generate(&ids, &params, &stop, &cancel, |_| {})?;
        println!(
            "turno {n}: {} tokens de prompt + {} gerados em {:.2}s ({:.1} t/s) | parada={:?} | resposta={:?}{}",
            resposta.prompt_tokens,
            resposta.generated_tokens,
            inicio.elapsed().as_secs_f64(),
            resposta.decode_tokens_per_second(),
            resposta.stop,
            resposta.text.chars().take(120).collect::<String>(),
            if resposta.text.contains('\u{fffd}') { " | TEM CARACTERE DE SUBSTITUIÇÃO" } else { "" },
        );
        Ok(resposta.text)
    };

    turno(1, &cheio)?;
    // O segundo turno traz texto próprio, maior que a sobra da janela: é a conta
    // que só fecha porque o primeiro foi esquecido. Com uma pergunta de duas
    // linhas isto passaria até sem esquecer nada, e a sonda não mediria nada.
    let pergunta = format!(
        "{}\nResponda com uma palavra: qual é a capital do Brasil?",
        base.trim_end()
    );
    match turno(2, &pergunta) {
        Ok(texto) if !texto.trim().is_empty() => {
            println!("esquecer: OK — dois turnos na mesma janela carregada");
            Ok(())
        }
        Ok(_) => {
            eprintln!("esquecer: o segundo turno devolveu texto vazio");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("esquecer: O SEGUNDO TURNO FALHOU: {e:#}");
            std::process::exit(1);
        }
    }
}

/// O prefixo do KV reaproveitado entre rodadas, medido contra o defeito.
///
/// Dois braços exclusivos sobre a mesma janela carregada: o braço A deixa o
/// cache viver de uma rodada para a outra (o produto), o braço B esquece tudo
/// antes de cada geração (o comportamento anterior, que re-prefilava o histórico
/// inteiro a cada rodada de ferramenta). A amostragem é argmax, então os dois
/// braços têm de devolver o mesmo texto: se o recorte do cache mudasse a
/// resposta, o tempo poupado seria mentira. O que se compara é o prefill da
/// segunda rodada, que nos dois braços já encontra a placa quente.
///
/// A segunda rodada é a de uma ferramenta de verdade: o prompt da primeira, mais
/// a resposta do modelo, mais o resultado que voltou. É esse começo igual que a
/// placa já calculou e que não precisa calcular outra vez.
fn prefixo_reutilizado(
    dir: &PathBuf,
    weights: &str,
    context: usize,
    max_tokens: usize,
    path: &str,
) -> anyhow::Result<()> {
    let mut engine = Engine::load("ggml", dir, weights, context, &dlls())?;
    let acelerador = engine.acelerador();
    println!(
        "carga: {} | backend={} dispositivo={:?} janela={}",
        weights, acelerador.tipo, acelerador.dispositivo, engine.context()
    );
    if acelerador.tipo != "gpu" {
        eprintln!("prefixo: o recorte de cache é do caminho ggml; no candle o prefill é inteiro");
        std::process::exit(2);
    }

    let base = std::fs::read_to_string(path).context("falha ao ler o prompt")?;
    let janela = engine.context();
    let alvo = janela.saturating_sub(2 * max_tokens + 256).max(64);
    let mut historico = String::new();
    let mut blocos = 0usize;
    while engine.count_tokens(&historico)? < alvo {
        historico.push_str(base.trim_end());
        historico.push('\n');
        blocos += 1;
    }
    let pergunta1 = format!("{historico}\nPergunta: qual é a capital do Brasil?");
    println!(
        "histórico: {} blocos de {} bytes = {} tokens (alvo {}, janela {})",
        blocos, base.len(), engine.count_tokens(&historico)?, alvo, janela
    );

    let params = SamplingParams { max_tokens, temperature: 0.0, ..SamplingParams::default() };
    let stop = [concat!("<", "|im_start|>").to_string()];
    let cancel = AtomicBool::new(false);

    let ids_de = |engine: &Engine, texto: &str| -> anyhow::Result<Vec<u32>> {
        Ok(engine
            .tokenizer()
            .encode(texto, false)
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .get_ids()
            .to_vec())
    };
    let medir = |engine: &mut Engine, ids: &[u32], rotulo: &str| -> anyhow::Result<Generation> {
        let g = engine.generate(ids, &params, &stop, &cancel, |_| {})?;
        println!(
            "{rotulo}: prompt={} reaproveitados={} calculados={} em {}ms ({:.1} t/s) | gerados={} em {}ms ({:.2} t/s) | parada={:?} | resposta={:?}{}",
            g.prompt_tokens,
            g.reused_tokens,
            g.prompt_tokens - g.reused_tokens,
            g.prompt_ms,
            g.prefill_tokens_per_second(),
            g.generated_tokens,
            g.decode_ms,
            g.decode_tokens_per_second(),
            g.stop,
            g.text.chars().take(80).collect::<String>(),
            if g.text.contains('\u{fffd}') { " | TEM CARACTERE DE SUBSTITUIÇÃO" } else { "" },
        );
        Ok(g)
    };
    let cabe = |ids: &[u32], rodada: usize| -> anyhow::Result<()> {
        if ids.len() + max_tokens >= janela {
            anyhow::bail!(
                "a rodada {rodada} não cabe: {} tokens + {} gerados numa janela de {}",
                ids.len(), max_tokens, janela
            );
        }
        Ok(())
    };

    let ids1 = ids_de(&engine, &pergunta1)?;
    cabe(&ids1, 1)?;
    println!("\n=== braço A: o cache vive entre as rodadas ===");
    let a1 = medir(&mut engine, &ids1, "A rodada 1")?;
    let pergunta2 = format!(
        "{pergunta1}{}\nResultado da ferramenta: arquivo gravado.\nPergunta: e a capital da França?",
        a1.text
    );
    let ids2 = ids_de(&engine, &pergunta2)?;
    cabe(&ids2, 2)?;
    let a2 = medir(&mut engine, &ids2, "A rodada 2")?;
    // Lido do motor, não calculado por fora: o que a rodada 3 pode reaproveitar
    // são as células que ele mesmo declara ocupadas. Somar ids + gerados seria
    // o meu palpite sobre o KV, e um palpite não serve de régua.
    // A régua do recorte é a célula que o motor declara ocupada. Se ele não
    // declara, somar ids + gerados seria o meu palpite sobre o KV — e um palpite
    // usado como régua aprova ou reprova sem medir nada.
    let Some(cache_apos_2) = engine.cache_ocupado() else {
        anyhow::bail!(
            "o motor não declarou o KV ocupado depois da rodada 2; sem esta leitura a régua de recorte não existe"
        );
    };
    // A rodada que diverge no meio do histórico: o resultado da ferramenta é
    // outro. Aqui o cache tem de ser recortado a partir da divergência — é o
    // caminho do `seq_rm`, e sem ele a posição automática continuaria depois do
    // rabo velho. A rodada 2 sozinha não prova isso: ela é prefixo inteiro do
    // cache e nenhum recorte acontece.
    let pergunta3 = format!(
        "{pergunta1}{}\nResultado da ferramenta: ERRO, pasta não concedida.\nPergunta: e a capital do Japão?",
        a1.text
    );
    let ids3 = ids_de(&engine, &pergunta3)?;
    cabe(&ids3, 3)?;
    let a3 = medir(&mut engine, &ids3, "A rodada 3 (diverge)")?;

    println!("\n=== braço B: esquece tudo antes de cada rodada (o defeito) ===");
    engine.reset();
    let b1 = medir(&mut engine, &ids1, "B rodada 1")?;
    engine.reset();
    let b2 = medir(&mut engine, &ids2, "B rodada 2")?;
    engine.reset();
    let b3 = medir(&mut engine, &ids3, "B rodada 3 (diverge)")?;

    let mut falhas: Vec<String> = Vec::new();
    let curta = |t: &str| t.chars().take(60).collect::<String>();
    if a1.reused_tokens != 0 {
        falhas.push(format!("a primeira rodada reaproveitou {} tokens de antes da carga", a1.reused_tokens));
    }
    for (n, a, b) in [(1usize, &a1, &b1), (2, &a2, &b2), (3, &a3, &b3)] {
        if a.text != b.text {
            falhas.push(format!(
                "A RODADA {n} DIVERGIU entre os braços: o cache mudou a resposta. A={:?} B={:?}",
                curta(&a.text), curta(&b.text)
            ));
        }
        if a.text.contains('\u{fffd}') || b.text.contains('\u{fffd}') {
            falhas.push(format!("a rodada {n} tem caractere de substituição"));
        }
        if b.reused_tokens != 0 {
            falhas.push(format!("o braço B reaproveitou {} tokens na rodada {n}: ele não é o defeito", b.reused_tokens));
        }
    }
    if a2.reused_tokens < ids1.len() / 2 {
        falhas.push(format!(
            "o braço A reaproveitou {} de {} tokens do histórico: não é o prefixo que está sendo poupado",
            a2.reused_tokens, ids1.len()
        ));
    }
    if a3.reused_tokens >= cache_apos_2 {
        falhas.push(format!(
            "a rodada 3 reaproveitou além do ponto de divergência: {} de {} células ocupadas",
            a3.reused_tokens, cache_apos_2
        ));
    } else if !a3.recorte_recusado && a3.reused_tokens == 0 {
        falhas.push(format!(
            "a rodada 3 não recortou o cache: reaproveitou {} de {} células que estavam ocupadas",
            a3.reused_tokens, cache_apos_2
        ));
    }
    for (n, a, b) in [(2usize, &a2, &b2), (3, &a3, &b3)] {
        // A rodada 3 só tem de ser mais rápida se o recorte aconteceu. Quando a
        // memória do modelo recusa o recorte — um híbrido com estado recorrente
        // só desfaz dentro dos instantâneos do último lote — os dois braços
        // prefila tudo e a igualdade é o resultado correto, não um defeito.
        if a.recorte_recusado && n == 3 {
            println!("rodada 3: a memória recusou o recorte (devolveu false) → prefill inteiro nos dois braços");
            continue;
        }
        if a.prompt_ms >= b.prompt_ms {
            falhas.push(format!(
                "o prefill da rodada {n} não acelerou: A={}ms B={}ms", a.prompt_ms, b.prompt_ms
            ));
        }
    }

    println!("\n=== veredito ===");
    for (n, ids, a, b) in [(2usize, &ids2, &a2, &b2), (3, &ids3, &a3, &b3)] {
        println!(
            "rodada {n}: {} tokens de prompt | A reaproveitou {} e calculou {} em {}ms | B calculou {} em {}ms | {:.2}x | {}ms poupados",
            ids.len(),
            a.reused_tokens,
            a.prompt_tokens - a.reused_tokens,
            a.prompt_ms,
            b.prompt_tokens - b.reused_tokens,
            b.prompt_ms,
            b.prompt_ms as f64 / a.prompt_ms.max(1) as f64,
            b.prompt_ms.saturating_sub(a.prompt_ms),
        );
    }
    println!("recorte da rodada 3: {} células ocupadas, prefixo guardado até {}", cache_apos_2, a3.reused_tokens);
    if !falhas.is_empty() {
        for f in &falhas {
            eprintln!("prefixo: FALHOU — {f}");
        }
        std::process::exit(1);
    }
    println!("prefixo: OK — mesma resposta nos dois braços, prefill menor, e o braço do defeito medido ao lado");
    Ok(())
}
