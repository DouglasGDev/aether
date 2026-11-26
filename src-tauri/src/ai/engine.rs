//! Carregamento e geração, com streaming token a token.
//!
//! Dois motores, uma amostragem: a conta vai para a placa quando existe uma
//! (ggml/Vulkan, de qualquer fabricante) e, quando não existe, para a mesma
//! biblioteca contando no processador (`ggml::Destino::Sistema`). O candle é o degrau
//! depois desse, para a máquina em que nem a llama.cpp abriu: é o único caminho
//! que conhece uma arquitetura só. Os dois entregam logits na mesma forma, e é um
//! único caminho de amostragem que os consome — sampler duplicado é como duas
//! respostas passam a divergir entre si.
//!
//! A inferência é bloqueante e pesada: quem chama precisa rodar isto fora do
//! runtime async (spawn_blocking ou thread dedicada).

use super::ggml;
use super::granite::{self, Config};
use crate::idiomas::tr;
use anyhow::{Context, Result, bail};
use candle::quantized::gguf_file;
use candle::{Device, Tensor};
use candle_transformers::generation::{LogitsProcessor, Sampling};
use candle_transformers::utils::apply_repeat_penalty;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use tokenizers::Tokenizer;

#[derive(Debug, Clone)]
pub struct SamplingParams {
    pub temperature: f64,
    pub top_k: Option<usize>,
    pub top_p: Option<f64>,
    pub repeat_penalty: f32,
    pub repeat_last_n: usize,
    pub seed: u64,
    pub max_tokens: usize,
}

impl Default for SamplingParams {
    fn default() -> Self {
        Self {
            temperature: 0.7,
            top_k: Some(40),
            top_p: Some(0.9),
            repeat_penalty: 1.1,
            repeat_last_n: 128,
            seed: 0x4145_5448_4552,
            max_tokens: 512,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StopReason {
    Eos,
    StopSequence,
    MaxTokens,
    Cancelled,
    ContextFull,
}

#[derive(Debug, Clone)]
pub struct Generation {
    pub text: String,
    pub prompt_tokens: usize,
    /// Quantos tokens do prompt já estavam no KV cache e não foram calculados
    /// nesta rodada. É a medida do reaproveitamento: zero é um prefill inteiro.
    pub reused_tokens: usize,
    /// A rodada pediu um recorte do cache — porque o histórico diverge no meio —
    /// e a memória do modelo **recusou**. Não é o mesmo que `reused_tokens == 0`:
    /// zero pode ser um bug nosso, e isto é a resposta que a biblioteca deu.
    pub recorte_recusado: bool,
    pub generated_tokens: usize,
    pub prompt_ms: u64,
    pub decode_ms: u64,
    pub stop: StopReason,
}

impl Generation {
    pub fn decode_tokens_per_second(&self) -> f64 {
        if self.decode_ms == 0 {
            return 0.0;
        }
        self.generated_tokens as f64 * 1000.0 / self.decode_ms as f64
    }

    /// Tokens por segundo do que foi de fato calculado. O prefixo reaproveitado
    /// não passou pela placa nesta rodada: contá-lo aqui faria a medida dizer que
    /// o prefill acelerou sem nada ter mudado no motor.
    pub fn prefill_tokens_per_second(&self) -> f64 {
        if self.prompt_ms == 0 {
            return 0.0;
        }
        let calculados = self.prompt_tokens.saturating_sub(self.reused_tokens);
        calculados as f64 * 1000.0 / self.prompt_ms as f64
    }
}

/// O que o motor entrega a quem escuta a geração.
#[derive(Debug, Clone)]
pub enum Emission {
    /// Texto novo, para anexar ao que já foi mostrado.
    Delta(String),
    /// Uma sequência de parada foi transmitida pela metade: o texto acumulado
    /// passa a ser exatamente este, sem o trecho retractado.
    Replace(String),
}

/// Decodificação incremental que nunca emite um caractere UTF-8 pela metade.
/// Só a cauda com caractere de substituição fica pendurada; o resto sai inteiro.
/// Segurar espaço e pontuação e depois emitir apenas o delta seguinte descartava
/// o texto pendurado — foi assim que "arquitetura como" virou "arquiteturacomo".
struct TextStream {
    tokenizer: Tokenizer,
    tokens: Vec<u32>,
    committed: usize,
    /// `true` no fluxo que a pessoa lê: token especial não aparece na resposta.
    /// `false` na vigia de parada — ver `TextStream::bruto`.
    pular_especiais: bool,
}

impl TextStream {
    fn new(tokenizer: Tokenizer) -> Self {
        Self { tokenizer, tokens: Vec::new(), committed: 0, pular_especiais: true }
    }

    /// O mesmo fluxo incremental, mas com os tokens especiais à vista. Existe
    /// porque a rede de parada de ChatML procurava o marcador no texto do
    /// usuário: Granite/Qwen marcam `<|im_start|>` como especial, o decode com
    /// `skip_special_tokens` o apaga, e `text.contains` nunca casava — o modelo
    /// repetindo turnos ia até max_tokens. A vigia decodifica com `false` só
    /// para detectar a parada; o que a pessoa lê continua saindo de `new`.
    fn bruto(tokenizer: Tokenizer) -> Self {
        Self { tokenizer, tokens: Vec::new(), committed: 0, pular_especiais: false }
    }

    fn decode(&self, tokens: &[u32]) -> Result<String> {
        self.tokenizer
            .decode(tokens, self.pular_especiais)
            .map_err(|e| anyhow::anyhow!(tr("erros.falhaDecodificar", &[("detalhe", &e.to_string())])))
    }

    fn push(&mut self, token: u32) -> Result<Option<String>> {
        self.tokens.push(token);
        let text = self.decode(&self.tokens[self.committed..])?;
        if text.ends_with('\u{fffd}') {
            return Ok(None);
        }
        self.committed = self.tokens.len();
        if text.is_empty() { Ok(None) } else { Ok(Some(text)) }
    }

    fn flush(&mut self) -> Result<Option<String>> {
        let before = self.decode(&self.tokens[self.committed..])?;
        if before.is_empty() {
            return Ok(None);
        }
        self.committed = self.tokens.len();
        Ok(Some(before))
    }
}

/// Quantos tokens gerados precedem o corte de uma parada no texto bruto.
/// O marcador começa em `corte` (byte do bruto, que inclui os especiais); o
/// texto da pessoa não os inclui. Recua-se token a token até o prefixo bruto
/// terminar antes do marcador: esse prefixo, decodificado sem especiais, é o
/// que se mostra. Recuar pelo comprimento do marcador não bastaria — um token
/// pode decodificar em vários caracteres.
fn tokens_antes_do_corte(tokenizer: &Tokenizer, tokens: &[u32], corte: usize) -> Result<usize> {
    let mut k = tokens.len();
    while k > 0 {
        let bruto = tokenizer.decode(&tokens[..k], false).map_err(|e| {
            anyhow::anyhow!(tr("erros.falhaDecodificar", &[("detalhe", &e.to_string())]))
        })?;
        if bruto.len() <= corte {
            break;
        }
        k -= 1;
    }
    Ok(k)
}

/// Onde a conta acontece, e com qual biblioteca.
enum Motor {
    /// ggml/llama.cpp — na placa escolhida ou, quando ela não serve, no
    /// processador. Posições e KV cache são da biblioteca: ela mesma sabe onde
    /// termina a sequência.
    Ggml { motor: ggml::Motor },
    /// candle na CPU, único caminho que conhece só uma arquitetura. As posições
    /// são contadas aqui, e é exatamente o que `granite::Layer::rope` espera:
    /// `narrow(index_pos..index_pos + n)`.
    Candle { model: granite::Model, config: Config, pos: usize },
}

impl Motor {
    /// Logits da última posição de `tokens`, 1-D. Prefill e passo de decode
    /// entram pelo mesmo método porque o que muda entre eles é só o tamanho do
    /// lote.
    fn passo(&mut self, tokens: &[u32]) -> Result<Tensor> {
        match self {
            Motor::Ggml { motor } => {
                let logits = motor.passo(tokens)?;
                let n = logits.len();
                Ok(Tensor::from_vec(logits, (n,), &Device::Cpu)?)
            }
            Motor::Candle { model, pos, .. } => {
                let input = Tensor::new(tokens, &Device::Cpu)?.reshape((1, tokens.len()))?;
                let logits = model.forward(&input, *pos)?.squeeze(0)?;
                *pos += tokens.len();
                Ok(logits)
            }
        }
    }

    /// Prefila o prompt do turno e diz quantos tokens não precisaram de conta.
    /// Na placa o começo que o cache já tem é reaproveitado; no candle não há
    /// recorte parcial de cache, então o turno prefila inteiro — e o zero é a
    /// medida honesta disso, não um reaproveitamento que não aconteceu.
    fn passo_prompt(&mut self, tokens: &[u32]) -> Result<(Tensor, usize, bool)> {
        match self {
            Motor::Ggml { motor } => {
                let (logits, reaproveitados, recusado) = motor.passo_com_prefixo(tokens)?;
                let n = logits.len();
                Ok((Tensor::from_vec(logits, (n,), &Device::Cpu)?, reaproveitados, recusado))
            }
            Motor::Candle { .. } => {
                self.limpar();
                Ok((self.passo(tokens)?, 0, false))
            }
        }
    }

    /// Esquece a conversa: cache e posições voltam ao estado de modelo novo.
    fn limpar(&mut self) {
        match self {
            Motor::Ggml { motor } => motor.limpar(),
            Motor::Candle { model, pos, .. } => {
                model.clear_kv_cache();
                *pos = 0;
            }
        }
    }

    /// A janela que o backend aceitou, que pode ser menor que a pedida.
    fn janela(&self) -> usize {
        match self {
            Motor::Ggml { motor } => motor.janela,
            Motor::Candle { model, .. } => model.max_seq_len(),
        }
    }

    fn vocabulario(&self) -> usize {
        match self {
            Motor::Ggml { motor } => motor.vocabulario(),
            Motor::Candle { config, .. } => config.vocab_size,
        }
    }
}

/// O que a interface mostra sobre onde o modelo está rodando. Nunca um vazio:
/// se a conta foi para a CPU, o motivo de a placa não ter servido vem junto.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Acelerador {
    /// `"gpu"` ou `"cpu"` — o que foi usado, não o que foi pedido.
    pub tipo: &'static str,
    /// Nome que a placa se deu no registro Vulkan; vazio na CPU.
    pub dispositivo: String,
    /// Por que a placa não serviu, quando ela não serviu — e, quando serviu
    /// sem medida completa, o motivo real de cada sonda que falhou. `None` na
    /// placa só quando nada falhou: offload não-medido nunca se apresenta como
    /// normal.
    pub motivo: Option<String>,
    /// Onde os bytes ficaram, na placa que serviu. `None` só na CPU, onde não há
    /// placa para alojar nada.
    pub alojado: Option<Alojado>,
    /// A maior janela de potência de dois em que este modelo ainda iria inteiro
    /// para a placa — a mesma conta de `alojado`, varrida do teto que o arquivo
    /// declara para baixo. É um número medido agora, com a VRAM livre desta
    /// máquina, não uma ficha do folheto: por isso `None` na CPU e `None` para o
    /// modelo que não cabe inteiro em janela nenhuma. A escala de geração oferece
    /// este valor como o teto cômodo, e o que a pessoa pedir acima dele roda
    /// dividido, mais devagar.
    pub janela_inteira: Option<usize>,
}

/// A previsão feita com a VRAM livre de antes da carga, e o que a carga de fato
/// tirou da placa. Os dois lados juntos são a única forma de a tela dizer "está
/// inteiro na placa" sem estar pedindo confiança.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Alojado {
    /// Veredito calculado a partir do cabeçalho do GGUF e da VRAM livre medida
    /// antes de `llama_load_model_from_file`.
    pub previsto: crate::ai::alojamento::Alojamento,
    /// Bytes de VRAM que sumiram do livre entre antes e depois da carga.
    pub cedido: u64,
}

/// O motivo que acompanha a carga: cada sonda que falhou deixa o motivo real
/// aqui, na ordem em que falhou. `None` só quando nada falhou. A tela lê
/// `motivo` para decidir se há explicação a mostrar — um offload total sem
/// medida com `motivo: null` seria a tela apresentando o não-medido como
/// normal, que é exatamente o que este campo existe para impedir.
fn motivo_de_carga(avisos: [Option<String>; 3]) -> Option<String> {
    let juntos: Vec<String> = avisos.into_iter().flatten().collect();
    (!juntos.is_empty()).then(|| juntos.join("\n"))
}

pub struct Engine {
    pub model_id: String,
    motor: Motor,
    tokenizer: Tokenizer,
    eos_tokens: Vec<u32>,
    /// O template de conversa que o arquivo GGUF publica, quando publica. É a
    /// fonte que a sessão usa para renderizar o prompt; sem ele, o aplicativo
    /// recua para o `chat_template.jinja` baixado junto.
    template: Option<String>,
    /// Os tokens que encerram uma resposta, escritos como texto: a sequência de
    /// parada que a geração observa. Vêm do vocabulário do arquivo, não de uma
    /// lista de marcadores supostos.
    paradas: Vec<String>,
    acelerador: Acelerador,
}

impl Engine {
    /// `dir` contém `tokenizer.json`, `generation_config.json` e o GGUF nomeado
    /// por `weights_file`. Os ids de EOS vêm do generation_config do próprio
    /// modelo — nunca de uma lista de nomes chutada.
    ///
    /// `dlls` são as pastas onde o aplicativo publica a ggml vendorizada. Sem
    /// elas não há caminho de GPU, e a regra do produto (placa primeiro) viraria
    /// "CPU porque não havia opção" — por isso o motivo da recusa da placa é
    /// guardado e devolvido, nunca descartado.
    pub fn load(
        model_id: &str,
        dir: &Path,
        weights_file: &str,
        context: usize,
        dlls: &[PathBuf],
    ) -> Result<Self> {
        let tokenizer_path = dir.join("tokenizer.json");
        let tokenizer = Tokenizer::from_file(&tokenizer_path).map_err(|e| {
            anyhow::anyhow!(tr(
                "erros.falhaAbrirTokenizador",
                &[("caminho", &tokenizer_path.display().to_string()), ("detalhe", &e.to_string())]
            ))
        })?;
        let weights = dir.join(weights_file);
        // A ficha é lida uma vez e serve a três decisões: onde os pesos vão
        // morar, qual template renderizar e quais tokens encerram a resposta.
        // A falha não vai para o chão: sem ficha o alojamento não foi medido,
        // e o motivo real acompanha a carga até a tela (`motivo_de_carga`).
        let (ficha, erro_ficha) = match super::alojamento::fichar(&weights) {
            Ok(f) => (Some(f), None),
            Err(e) => (None, Some(format!("{e:#}"))),
        };
        let eos_tokens = match eos_from_generation_config(&dir.join("generation_config.json")) {
            Ok(ids) if !ids.is_empty() => ids,
            // Um GGUF que traz o vocabulário inteiro dentro de si pode não ter
            // generation_config.json no repositório de origem. O fim de resposta
            // continua escrito no arquivo que roda, e é dele que a biblioteca lê
            // — não é uma suposição, é o mesmo metadado.
            _ => ficha.as_ref().map(|f| f.eos.clone()).unwrap_or_default(),
        };
        if eos_tokens.is_empty() {
            bail!(tr("erros.semEosDeclarado", &[]));
        }
        // A VRAM é lida antes da carga. Depois dela o livre já descontaria o
        // próprio modelo, e a conta deixaria de ser previsão para ser retrospecto.
        // `Err` é sonda que falhou — diferente de `Ok(None)`, que é máquina sem
        // placa — e o motivo é retido, não descartado por `.ok()`. O erro da
        // sonda já vem traduzido pelas chaves existentes do motor: é ele mesmo
        // que vai à tela, sem embrulho que o apagasse.
        let (capacidade, erro_sonda) = match super::alojamento::capacidade(dlls) {
            Ok(c) => (c, None),
            Err(e) => (None, Some(format!("{e:#}"))),
        };
        let livre_antes = capacidade.as_ref().map(|c| c.livre).unwrap_or(0);
        // O veredito antes da carga é o que manda a placa e é o que a tela mostra:
        // o mesmo número decide quantas camadas sobem e conta a história no chip.
        // Recalculá-lo depois com a janela que a biblioteca aceitou poderia dizer
        // "inteiro" para uma carga que foi comandada dividida.
        let previsto = ficha
            .as_ref()
            .zip(capacidade.as_ref())
            .map(|(ficha, capacidade)| {
                super::alojamento::alojar(ficha, Some(capacidade), context)
            });
        // Sem medida o despacho continua sendo o sentinela de "todas as camadas":
        // a regra do produto é placa primeiro, e recusar a carga porque a sonda
        // falhou deixaria a máquina sem resposta. O que não pode é calar — o
        // `motivo` abaixo leva o motivo real de cada sonda que falhou, e a tela
        // mostra que este offload total é uma tentativa, não uma medida.
        let despacho = previsto
            .as_ref()
            .map(super::alojamento::despacho)
            .unwrap_or(super::ggml::ffi::CAMADAS_NA_GPU);
        // O teto cômodo que a escala de geração vai oferecer, medido com a mesma
        // conta que acabou de comandar a carga: o livre lido antes dela e o
        // cabeçalho destes pesos. Acima dele o veredito divide, e dividir é
        // escolha de quem usa a máquina, não falha dela.
        let inteira = ficha
            .as_ref()
            .zip(capacidade.as_ref())
            .and_then(|(ficha, capacidade)| {
                super::alojamento::maior_janela_inteira(
                    ficha,
                    Some(capacidade),
                    super::geracao::CONTEXTO_MAX as usize,
                )
            });
        match ggml::Motor::novo(
            dlls,
            &weights,
            context,
            ggml::Destino::Placa { camadas_gpu: despacho },
        ) {
            Ok(motor) => {
                // O cedido só existe quando a placa respondeu depois da carga:
                // zero sem medição seria dizer "nada foi levado" sem ter lido.
                // Sonda que falha aqui também deixa o motivo, e o `alojado`
                // não publicado é a resposta honesta de que não há medida.
                let (alojado, erro_pos) = match previsto {
                    Some(previsto) => match super::alojamento::capacidade(dlls) {
                        Ok(Some(depois)) => (
                            Some(Alojado {
                                previsto,
                                cedido: livre_antes.saturating_sub(depois.livre),
                            }),
                            None,
                        ),
                        // A placa que carregou não respondeu à medição de depois:
                        // o motivo é o que o motor já sabe dizer, sem chave nova.
                        Ok(None) => (None, Some(tr("erros.motorSemPlaca", &[]))),
                        Err(e) => (None, Some(format!("{e:#}"))),
                    },
                    None => (None, None),
                };
                let acelerador = Acelerador {
                    tipo: "gpu",
                    dispositivo: motor.dispositivo.clone(),
                    motivo: motivo_de_carga([erro_ficha, erro_sonda, erro_pos]),
                    alojado,
                    janela_inteira: inteira,
                };
                Ok(Self {
                    model_id: model_id.to_string(),
                    tokenizer,
                    eos_tokens,
                    template: ficha.as_ref().and_then(|f| f.template.clone()),
                    paradas: ficha.map(|f| f.paradas).unwrap_or_default(),
                    motor: Motor::Ggml { motor },
                    acelerador,
                })
            }
            Err(falha_gpu) => {
                // Fallback legítimo: nenhuma placa serviu. A alternativa imediata
                // não é um motor menor, é o mesmo motor sem placa — llama.cpp
                // contando no processador com a escala lida desta máquina. Ele
                // continua enxergando qualquer arquitetura e cortando prefixo de
                // cache, que é o que o candle não faz: cair direto nele trocaria
                // "sem placa" por "só Granite".
                let escala = ggml::Escala::da_maquina();
                match ggml::Motor::novo(
                    dlls,
                    &weights,
                    context,
                    ggml::Destino::Sistema(escala),
                ) {
                    Ok(motor) => {
                        let dispositivo = motor.dispositivo.clone();
                        // As sondas que falharam antes da tentativa entram junto no
                        // motivo: explicam por que a carga chegou ao processador.
                        let motivo = match motivo_de_carga([erro_ficha, erro_sonda, None]) {
                            Some(avisos) => format!("{falha_gpu}\n{avisos}"),
                            None => falha_gpu.to_string(),
                        };
                        Ok(Self {
                            model_id: model_id.to_string(),
                            tokenizer,
                            eos_tokens,
                            template: ficha.as_ref().and_then(|f| f.template.clone()),
                            paradas: ficha.map(|f| f.paradas).unwrap_or_default(),
                            motor: Motor::Ggml { motor },
                            acelerador: Acelerador {
                                tipo: "cpu",
                                dispositivo,
                                motivo: Some(motivo),
                                alojado: None,
                                janela_inteira: None,
                            },
                        })
                    }
                    // Nem a biblioteca sem placa serviu: aqui entra o candle, e o
                    // erro dele vai junto com o da placa — os dois são o que a
                    // máquina respondeu, e esconder um deixaria a tela sem causa.
                    Err(falha_cpu_ggml) => {
                        let (model, config) = carregar_na_cpu(&weights, context).map_err(
                            |falha_candle| {
                                anyhow::anyhow!("{falha_gpu}\n{falha_cpu_ggml}\n{falha_candle}")
                            },
                        )?;
                        let motivo = match motivo_de_carga([erro_ficha, erro_sonda, None]) {
                            Some(avisos) => format!("{falha_gpu}\n{falha_cpu_ggml}\n{avisos}"),
                            None => format!("{falha_gpu}\n{falha_cpu_ggml}"),
                        };
                        Ok(Self {
                            model_id: model_id.to_string(),
                            tokenizer,
                            eos_tokens,
                            template: ficha.as_ref().and_then(|f| f.template.clone()),
                            paradas: ficha.map(|f| f.paradas).unwrap_or_default(),
                            motor: Motor::Candle { model, config, pos: 0 },
                            acelerador: Acelerador {
                                tipo: "cpu",
                                dispositivo: String::new(),
                                motivo: Some(motivo),
                                alojado: None,
                                janela_inteira: None,
                            },
                        })
                    }
                }
            }
        }
    }

    pub fn tokenizer(&self) -> &Tokenizer {
        &self.tokenizer
    }

    pub fn acelerador(&self) -> &Acelerador {
        &self.acelerador
    }

    /// Põe `pedidos` sobre os pesos que estão em memória agora, sem recarregar
    /// nada, e devolve linha por linha o que aconteceu.
    ///
    /// O caminho de CPU recusa à vista. Não é preguiça de implementação: os
    /// pesos granite são lidos pelo candle e não há adaptador nele, e fingir que
    /// aplicou — ou pior, calar o motivo — deixaria a tela mostrar um ajuste
    /// fino ligado que não muda uma palavra da resposta.
    pub fn usar_adaptadores(&mut self, pedidos: &[(PathBuf, f32)]) -> Vec<ggml::Aplicado> {
        match &mut self.motor {
            Motor::Ggml { motor } => motor.usar_adaptadores(pedidos),
            Motor::Candle { .. } => pedidos
                .iter()
                .map(|(caminho, _)| ggml::Aplicado {
                    caminho: caminho.clone(),
                    aplicada: false,
                    erro: Some(tr("erros.adaptadorSemCpu", &[])),
                })
                .collect(),
        }
    }

    /// A metadata granite lida pelo candle. `None` no caminho de GPU, onde quem
    /// lê o GGUF é a llama.cpp e estes escalares nunca passam por nós.
    pub fn config(&self) -> Option<&Config> {
        match &self.motor {
            Motor::Ggml { .. } => None,
            Motor::Candle { config, .. } => Some(config),
        }
    }

    /// Quantos ids cabem na conversa, ditos pelo backend — não os que pedimos.
    pub fn context(&self) -> usize {
        self.motor.janela()
    }

    pub fn vocabulario(&self) -> usize {
        self.motor.vocabulario()
    }

    pub fn eos_tokens(&self) -> &[u32] {
        &self.eos_tokens
    }

    /// O template que os pesos carregados declaram, no texto exato do arquivo.
    pub fn template(&self) -> Option<&str> {
        self.template.as_deref()
    }

    /// As sequências de parada do modelo. Uma lista vazia é a resposta honesta
    /// de um arquivo que não declara vocabulário — quem gera continua parando no
    /// EOS, que é o que a biblioteca também faz.
    pub fn paradas(&self) -> &[String] {
        &self.paradas
    }

    pub fn count_tokens(&self, text: &str) -> Result<usize> {
        Ok(self
            .tokenizer
            .encode(text, false)
            .map_err(|e| anyhow::anyhow!(tr("erros.tokenizar", &[("detalhe", &e.to_string())])))?
            .len())
    }

    pub fn reset(&mut self) {
        self.motor.limpar();
    }

    /// Células do KV ocupadas agora, ou `None` onde não há o que contar. É o
    /// número que a bancada confere depois de um `reset`: o caminho de CPU
    /// (candle) não publica essa contagem, e dizer zero ali seria medir o que a
    /// biblioteca não disse.
    pub fn cache_ocupado(&self) -> Option<usize> {
        match &self.motor {
            Motor::Ggml { motor } => Some(motor.ocupadas()),
            Motor::Candle { .. } => None,
        }
    }

    /// Se a conta está na placa. A regra do produto é placa primeiro, e a tela
    /// diz onde ela foi parada — um motor ggml no processador não é uma placa.
    pub fn na_placa(&self) -> bool {
        matches!(&self.motor, Motor::Ggml { motor } if motor.na_placa)
    }

    /// Se quem conta é a llama.cpp, na placa ou no processador. É esta a
    /// pergunta que o cache de prefixo faz: a redução é da biblioteca, não do
    /// dispositivo, e o candle não tem recorte parcial de cache nenhum.
    pub fn usa_ggml(&self) -> bool {
        matches!(self.motor, Motor::Ggml { .. })
    }

    /// Gera a continuação de `prompt_tokens`, emitindo texto por `emit`.
    pub fn generate(
        &mut self,
        prompt_tokens: &[u32],
        params: &SamplingParams,
        stop_sequences: &[String],
        cancel: &AtomicBool,
        mut emit: impl FnMut(Emission),
    ) -> Result<Generation> {
        if prompt_tokens.is_empty() {
            bail!(tr("erros.nadaParaInferir", &[]));
        }
        let janela = self.context();
        if prompt_tokens.len() >= janela {
            bail!(tr(
                "erros.promptForaDaJanela",
                &[
                    ("tokens", &prompt_tokens.len().to_string()),
                    ("janela", &janela.to_string())
                ]
            ));
        }
        let sampling = match (params.top_k, params.top_p) {
            (Some(k), Some(p)) => Sampling::TopKThenTopP { k, p, temperature: params.temperature },
            (Some(k), None) => Sampling::TopK { k, temperature: params.temperature },
            (None, Some(p)) => Sampling::TopP { p, temperature: params.temperature },
            (None, None) => Sampling::All { temperature: params.temperature },
        };
        let sampling = if params.temperature < 1e-7 { Sampling::ArgMax } else { sampling };
        let mut logits_processor = LogitsProcessor::from_sampling(params.seed, sampling);

        let mut all_tokens = prompt_tokens.to_vec();
        let mut stream = TextStream::new(self.tokenizer.clone());
        // A vigia vê os tokens especiais que o stream da pessoa esconde: é neles
        // que mora o marcador de turno do ChatML. A parada é procurada aqui, não
        // no texto do usuário — ver `TextStream::bruto`.
        let mut vigia = TextStream::bruto(self.tokenizer.clone());
        let mut bruto = String::new();
        let mut text = String::new();

        let prefill_start = Instant::now();
        let (logits, reused_tokens, recorte_recusado) = self.motor.passo_prompt(prompt_tokens)?;
        let prompt_ms = prefill_start.elapsed().as_millis() as u64;

        let mut next = logits_processor.sample(&logits)?;
        let decode_start = Instant::now();
        let mut generated = 0usize;
        let budget = params.max_tokens.min(janela - prompt_tokens.len());

        let stop = loop {
            if cancel.load(Ordering::Relaxed) {
                break StopReason::Cancelled;
            }
            if self.eos_tokens.contains(&next) {
                break StopReason::Eos;
            }
            if generated >= budget {
                break if generated >= params.max_tokens {
                    StopReason::MaxTokens
                } else {
                    StopReason::ContextFull
                };
            }

            all_tokens.push(next);
            generated += 1;
            if let Some(piece) = stream.push(next)? {
                text.push_str(&piece);
                emit(Emission::Delta(piece));
            }
            if let Some(piece) = vigia.push(next)? {
                bruto.push_str(&piece);
            }
            if let Some(hit) = stop_sequences
                .iter()
                .find(|s| !s.is_empty() && bruto.contains(s.as_str()))
            {
                let cut = bruto.find(hit.as_str()).expect("parada recém-encontrada");
                // O que a pessoa vê é o prefixo sem especiais que antecede o
                // marcador — não o bruto truncado, que vazaria o token especial.
                let k = tokens_antes_do_corte(&self.tokenizer, &vigia.tokens, cut)?;
                text = stream.decode(&vigia.tokens[..k])?;
                emit(Emission::Replace(text.clone()));
                break StopReason::StopSequence;
            }

            let logits = self.motor.passo(&[next])?;
            let logits = if params.repeat_penalty == 1.0 {
                logits
            } else {
                let start = all_tokens.len().saturating_sub(params.repeat_last_n);
                apply_repeat_penalty(&logits, params.repeat_penalty, &all_tokens[start..])?
            };
            next = logits_processor.sample(&logits)?;
        };

        if stop != StopReason::StopSequence {
            if let Some(rest) = stream.flush()? {
                text.push_str(&rest);
                emit(Emission::Delta(rest));
            }
        }
        let decode_ms = decode_start.elapsed().as_millis() as u64;

        Ok(Generation {
            text,
            prompt_tokens: prompt_tokens.len(),
            reused_tokens,
            recorte_recusado,
            generated_tokens: generated,
            prompt_ms,
            decode_ms,
            stop,
        })
    }
}

/// O último degrau: candle sobre o granite nativo, para a máquina em que a
/// llama.cpp não abriu nem no processador. É aqui que a arquitetura continua
/// restrita a granite, porque é o que este motor implementa — o degrau de cima,
/// `ggml::Destino::Sistema`, não tem essa restrição.
fn carregar_na_cpu(weights: &Path, context: usize) -> Result<(granite::Model, Config)> {
    let mut file = std::fs::File::open(weights).map_err(|e| {
        anyhow::anyhow!(tr(
            "erros.falhaAbrir",
            &[("caminho", &weights.display().to_string()), ("detalhe", &e.to_string())]
        ))
    })?;
    let content = gguf_file::Content::read(&mut file).with_context(|| {
        tr("erros.ggufInvalido", &[("caminho", &weights.display().to_string())])
    })?;
    let arch = content
        .metadata
        .get("general.architecture")
        .and_then(|v| v.to_string().ok())
        .cloned()
        .unwrap_or_default();
    if arch != "granite" {
        bail!(tr("erros.arquiteturaNaoGranite", &[("arquitetura", &arch)]));
    }
    let model = granite::Model::from_gguf(content, &mut file, &Device::Cpu, context)
        .context(tr("erros.falhaCarregarTensores", &[]))?;
    let config = model.config.clone();
    Ok((model, config))
}

/// EOS declarados no generation_config.json do modelo. Aceita id único ou lista,
/// como o Transformers faz. Sem chute: arquivo ausente ou sem o campo é erro.
fn eos_from_generation_config(path: &Path) -> Result<Vec<u32>> {
    let detalhe = path.display().to_string();
    let raw = std::fs::read_to_string(path).map_err(|e| {
        anyhow::anyhow!(tr("erros.falhaLer", &[("caminho", &detalhe), ("detalhe", &e.to_string())]))
    })?;
    let json: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| {
            anyhow::anyhow!(tr(
                "erros.jsonInvalidoEm",
                &[("caminho", &detalhe), ("detalhe", &e.to_string())]
            ))
        })?;
    let field = json
        .get("eos_token_id")
        .with_context(|| tr("erros.semCampoEos", &[("caminho", &detalhe)]))?;
    let mut ids = Vec::new();
    match field {
        serde_json::Value::Number(n) => {
            if let Some(id) = n.as_u64() {
                ids.push(id as u32);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                if let Some(id) = item.as_u64() {
                    ids.push(id as u32);
                }
            }
        }
        other => bail!(tr(
            "erros.eosFormatoInesperado",
            &[("valor", &other.to_string())]
        )),
    }
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WordLevel + Fuse concatena os pedaços sem separador, como o BPE do
    /// Granite, sem depender do arquivo do modelo.
    fn stream_tokenizer() -> Tokenizer {
        let json = r#"{
            "version": "1.0",
            "truncation": null,
            "padding": null,
            "added_tokens": [],
            "normalizer": null,
            "pre_tokenizer": null,
            "post_processor": null,
            "decoder": { "type": "Fuse" },
            "model": {
                "type": "WordLevel",
                "vocab": {
                    "capital": 0, " ": 1, "Aether": 2, ".": 3,
                    "é": 4, "\ufffd": 5, "x": 6
                },
                "unk_token": "[UNK]"
            }
        }"#;
        Tokenizer::from_bytes(json.as_bytes()).expect("tokenizer de teste")
    }

    #[test]
    fn stream_keeps_spaces_and_punctuation() {
        let mut stream = TextStream::new(stream_tokenizer());
        let mut out = String::new();
        for token in [0u32, 1, 2, 3, 1, 4] {
            if let Some(piece) = stream.push(token).unwrap() {
                out.push_str(&piece);
            }
        }
        if let Some(rest) = stream.flush().unwrap() {
            out.push_str(&rest);
        }
        assert_eq!(out, "capital Aether. é");
    }

    #[test]
    fn stream_holds_back_incomplete_character() {
        let mut stream = TextStream::new(stream_tokenizer());
        assert!(
            stream.push(5).unwrap().is_none(),
            "caractere parcial não pode sair"
        );
        let mut out = stream.push(6).unwrap().unwrap_or_default();
        if let Some(rest) = stream.flush().unwrap() {
            out.push_str(&rest);
        }
        assert_eq!(out, "\u{fffd}x");
    }

    #[test]
    fn default_sampling_is_bounded() {
        let p = SamplingParams::default();
        assert!(p.temperature > 0.0);
        assert!(p.max_tokens > 0);
        assert!(p.repeat_last_n > 0);
    }

    #[test]
    fn throughput_is_zero_without_measurement() {
        let g = Generation {
            text: String::new(),
            prompt_tokens: 10,
            reused_tokens: 0,
            recorte_recusado: false,
            generated_tokens: 0,
            prompt_ms: 0,
            decode_ms: 0,
            stop: StopReason::Cancelled,
        };
        assert_eq!(g.decode_tokens_per_second(), 0.0);
        assert_eq!(g.prefill_tokens_per_second(), 0.0);
    }

    #[test]
    fn throughput_uses_measured_milliseconds() {
        let g = Generation {
            text: "x".into(),
            prompt_tokens: 100,
            reused_tokens: 0,
            recorte_recusado: false,
            generated_tokens: 50,
            prompt_ms: 500,
            decode_ms: 2000,
            stop: StopReason::Eos,
        };
        assert_eq!(g.decode_tokens_per_second(), 25.0);
        assert_eq!(g.prefill_tokens_per_second(), 200.0);
    }

    #[test]
    fn o_prefixo_reaproveitado_nao_conta_como_prefill() {
        // 100 tokens de prompt, 90 já no cache: a placa calculou 10 em 500 ms.
        // Contar os 100 diria 200 t/s — uma velocidade que ninguém mediu.
        let g = Generation {
            text: "x".into(),
            prompt_tokens: 100,
            reused_tokens: 90,
            recorte_recusado: false,
            generated_tokens: 50,
            prompt_ms: 500,
            decode_ms: 2000,
            stop: StopReason::Eos,
        };
        assert_eq!(g.prefill_tokens_per_second(), 20.0);
        assert_eq!(g.decode_tokens_per_second(), 25.0);
    }

    #[test]
    fn cpu_so_existe_quando_a_placa_recusou() {
        // O motivo da recusa é o que a interface mostra: sem ele, "CPU" vira um
        // fato sem explicação e ninguém descobre que a placa estava lá.
        let a = Acelerador {
            tipo: "cpu",
            dispositivo: String::new(),
            motivo: Some("nenhuma GPU".to_string()),
            alojado: None,
            janela_inteira: None,
        };
        let json = serde_json::to_string(&a).unwrap();
        assert!(json.contains("\"tipo\":\"cpu\""));
        assert!(json.contains("nenhuma GPU"));
        assert!(json.contains("\"dispositivo\":\"\""));
    }

    #[test]
    fn gpu_mostra_a_placa_e_nenhum_motivo() {
        let a = Acelerador {
            tipo: "gpu",
            dispositivo: "AMD Radeon RX 9060 XT".to_string(),
            motivo: None,
            alojado: None,
            janela_inteira: None,
        };
        let json = serde_json::to_string(&a).unwrap();
        assert!(json.contains("RX 9060 XT"));
        // A chave vem sempre no despacho — `types.ts` declara `motivo: string |
        // null`. Na placa ela é `null`, e é isso que a interface lê para não
        // desenhar explicação onde não houve recusa.
        assert!(json.contains("\"motivo\":null"));
        assert!(json.contains("\"alojado\":null"));
        assert!(json.contains("\"janelaInteira\":null"));
    }

    /// WordLevel + Fuse com `<|im_start|>` como token especial adicionado —
    /// a forma como Granite/Qwen publicam o marcador de turno no tokenizer.json.
    /// É exatamente ela que torna inerte uma parada procurada no texto decodificado
    /// com `skip_special_tokens = true`.
    fn tokenizer_com_marcador_especial() -> Tokenizer {
        let json = r#"{
            "version": "1.0",
            "truncation": null,
            "padding": null,
            "added_tokens": [
                {
                    "id": 7, "content": "<|im_start|>",
                    "single_word": false, "lstrip": false,
                    "normalized": false, "rstrip": false, "special": true
                }
            ],
            "normalizer": null,
            "pre_tokenizer": null,
            "post_processor": null,
            "decoder": { "type": "Fuse" },
            "model": {
                "type": "WordLevel",
                "vocab": {
                    "capital": 0, " ": 1, "Aether": 2, ".": 3,
                    "é": 4, "\ufffd": 5, "x": 6
                },
                "unk_token": "[UNK]"
            }
        }"#;
        Tokenizer::from_bytes(json.as_bytes()).expect("tokenizer de teste")
    }

    /// O defeito A2 em forma de teste: o marcador de turno some do texto que a
    /// pessoa lê (decode com especiais pulados), então a rede antiga —
    /// `text.contains("<|im_start|>")` sobre esse texto — nunca casava. A vigia
    /// decodifica com os especiais à vista e vê o marcador.
    #[test]
    fn marcador_especial_e_invisivel_no_stream_mas_nao_na_vigia() {
        let tk = tokenizer_com_marcador_especial();
        // "capital Aether" + marcador de turno + "x": o modelo repetindo turno.
        let tokens = [0u32, 1, 2, 7, 6];
        let mut usuario = TextStream::new(tk.clone());
        let mut vigia = TextStream::bruto(tk);
        let mut saida = String::new();
        let mut bruto = String::new();
        for t in tokens {
            if let Some(p) = usuario.push(t).unwrap() {
                saida.push_str(&p);
            }
            if let Some(p) = vigia.push(t).unwrap() {
                bruto.push_str(&p);
            }
        }
        assert!(!saida.contains("<|im_start|>"), "o stream do usuário esconde o especial: {saida}");
        assert!(bruto.contains("<|im_start|>"), "a vigia tem de ver o marcador: {bruto}");
    }

    /// O corte da parada devolve à pessoa exatamente o que precede o marcador,
    /// sem o marcador e sem o turno que ele abriria.
    #[test]
    fn o_corte_da_parada_devolve_o_texto_sem_o_marcador() {
        let tk = tokenizer_com_marcador_especial();
        let tokens = [0u32, 1, 2, 7, 6];
        let bruto = tk.decode(&tokens, false).unwrap();
        let corte = bruto.find("<|im_start|>").expect("marcador no bruto");
        let k = tokens_antes_do_corte(&tk, &tokens, corte).unwrap();
        assert_eq!(k, 3);
        assert_eq!(tk.decode(&tokens[..k], true).unwrap(), "capital Aether");
    }

    /// Parada que não é token especial (um "." escrito no vocabulário comum)
    /// continua sendo vista pela vigia: a correção não pode quebrar o caso normal.
    #[test]
    fn parada_comum_continua_visivel_na_vigia() {
        let tk = tokenizer_com_marcador_especial();
        let tokens = [0u32, 1, 2, 3];
        let bruto = tk.decode(&tokens, false).unwrap();
        assert!(bruto.contains('.'));
        let corte = bruto.find('.').unwrap();
        let k = tokens_antes_do_corte(&tk, &tokens, corte).unwrap();
        assert_eq!(tk.decode(&tokens[..k], true).unwrap(), "capital Aether");
    }

    /// A sonda sem as pastas da ggml falha de verdade — é o mesmo `Err` de uma
    /// máquina com a DLL ausente, não uma falha inventada. O motivo real tem de
    /// chegar ao acelerador que a tela lê, nunca ser descartado por `.ok()`.
    #[test]
    fn sonda_que_falha_deixa_o_motivo_real_no_acelerador() {
        let erro = super::super::alojamento::capacidade(&[]).unwrap_err();
        let detalhe = format!("{erro:#}");
        let a = Acelerador {
            tipo: "gpu",
            dispositivo: "placa de teste".to_string(),
            motivo: motivo_de_carga([None, Some(detalhe.clone()), None]),
            alojado: None,
            janela_inteira: None,
        };
        assert!(a.motivo.as_deref().unwrap().contains(&detalhe));
        let json = serde_json::to_string(&a).unwrap();
        assert!(json.contains("\"tipo\":\"gpu\""), "{json}");
        // O defeito A5: offload total não-medido apresentado como normal,
        // com motivo:null e alojado:null e nenhuma explicação.
        assert!(!json.contains("\"motivo\":null"), "{json}");
    }

    #[test]
    fn carga_sem_falha_nao_inventa_motivo() {
        assert_eq!(motivo_de_carga([None, None, None]), None);
    }

    #[test]
    fn ficha_ilegivel_tambem_chega_a_tela() {
        let motivo =
            motivo_de_carga([Some("cabeçalho recusado".to_string()), None, None]).unwrap();
        assert!(motivo.contains("cabeçalho recusado"));
    }

    /// A premissa da vigia no tokenizer real: o marcador de turno é especial e
    /// o decode do usuário o apaga. Precisa de modelo ChatML em disco.
    #[test]
    #[ignore = "requer modelo real em disco: AETHER_TESTE_MODELO=<pasta com tokenizer.json de Granite/Qwen>"]
    fn marcador_de_turno_e_especial_no_tokenizer_real() {
        let dir = std::env::var("AETHER_TESTE_MODELO")
            .expect("defina AETHER_TESTE_MODELO=<pasta do modelo>");
        let tk = Tokenizer::from_file(Path::new(&dir).join("tokenizer.json")).unwrap();
        let ids = tk.encode("<|im_start|>", false).unwrap().get_ids().to_vec();
        assert!(!ids.is_empty(), "o marcador tem de existir no vocabulário");
        assert!(!tk.decode(&ids, true).unwrap().contains("<|im_start|>"));
        assert!(tk.decode(&ids, false).unwrap().contains("<|im_start|>"));
    }

    /// A sonda de VRAM real: ou devolve uma medida, ou devolve o motivo —
    /// nunca um número inventado. Precisa das DLLs vendorizadas e de placa.
    #[test]
    #[ignore = "requer DLLs vendorizadas e placa: AETHER_TESTE_DLLS=<pasta da ggml>"]
    fn sonda_de_vram_real_medida_ou_motivo() {
        let pasta = std::env::var("AETHER_TESTE_DLLS")
            .expect("defina AETHER_TESTE_DLLS=<pasta da ggml>");
        match super::super::alojamento::capacidade(&[PathBuf::from(pasta)]) {
            Ok(Some(c)) => assert!(c.total > 0, "placa sem memória total não é medida"),
            // Máquina sem placa: `None` é a resposta honesta, não falha.
            Ok(None) => {}
            Err(e) => assert!(!format!("{e:#}").is_empty(), "a falha tem motivo real"),
        }
    }

    #[test]
    fn alojado_dividido_diz_as_camadas_e_os_dois_lados() {
        // O card da placa lê exatamente estas chaves. Se o enum mudasse de forma
        // sem a interface mudar junto, o turno continuaria rodando e a tela
        // mostraria "na placa" para um modelo metade em RAM.
        use super::super::alojamento::Alojamento;
        let a = Acelerador {
            tipo: "gpu",
            dispositivo: "AMD Radeon RX 9060 XT".to_string(),
            motivo: None,
            alojado: Some(Alojado {
                previsto: Alojamento::Dividida {
                    camadas: 21,
                    total: 40,
                    na_placa: 1000,
                    em_ram: 1136,
                    estado: 0,
                    livre: 6000,
                },
                cedido: 1900,
            }),
            janela_inteira: Some(32768),
        };
        let json = serde_json::to_string(&a).unwrap();
        assert!(json.contains("\"tipo\":\"gpu\""), "{json}");
        assert!(json.contains("\"alojado\":{"), "{json}");
        assert!(json.contains("\"cedido\":1900"), "{json}");
        assert!(json.contains("\"tipo\":\"dividida\""), "{json}");
        assert!(json.contains("\"camadas\":21"), "{json}");
        assert!(json.contains("\"emRam\":1136"), "{json}");
        // A escala lê esta chave para saber até onde o cursor pode ir inteiro.
        assert!(json.contains("\"janelaInteira\":32768"), "{json}");
    }
}
