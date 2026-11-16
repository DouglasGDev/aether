//! Inferência quantizada da arquitetura Granite sobre GGUF.
//!
//! candle 0.11.1 traz `granite.rs` apenas para pesos densos via VarBuilder e sem
//! os escalares da arquitetura. O GGUF de Granite 4.2 usa o layout de tensores do
//! Llama, mas aplica `attention_scale`, `embedding_scale`, `residual_scale` e
//! `logit_scale` — ignorá-los produz logits errados, não apenas texto pior.

use crate::idiomas::tr;
use candle::quantized::{QMatMul, QTensor, gguf_file};
use candle::{DType, Device, IndexOp, Result, Tensor};
use candle_nn::{Embedding, Module};
use candle_transformers::quantized_nn::RmsNorm;
use candle_transformers::utils::build_causal_mask;
use std::collections::HashMap;
use std::io::{Read, Seek};

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub block_count: usize,
    pub embedding_length: usize,
    pub feed_forward_length: usize,
    pub head_count: usize,
    pub head_count_kv: usize,
    pub head_dim: usize,
    pub rope_dim: usize,
    pub rope_freq_base: f32,
    pub rms_norm_eps: f64,
    pub context_length: usize,
    pub vocab_size: usize,
    pub attention_scale: f32,
    pub embedding_scale: f32,
    pub residual_scale: f32,
    pub logit_scale: f32,
}

impl Config {
    pub fn from_metadata(md: &HashMap<String, gguf_file::Value>) -> Result<Self> {
        let get = |key: &str| -> Result<&gguf_file::Value> {
            md.get(key)
                .ok_or_else(|| {
                    candle::Error::Msg(tr("erros.metadadoAusente", &[("chave", key)]))
                })
        };
        let u32_of = |key: &str| -> Result<usize> { Ok(get(key)?.to_u32()? as usize) };
        let f32_of = |key: &str, default: f32| -> Result<f32> {
            Ok(md.get(key).and_then(|v| v.to_f32().ok()).unwrap_or(default))
        };

        let embedding_length = u32_of("granite.embedding_length")?;
        let head_count = u32_of("granite.attention.head_count")?;
        let head_dim = embedding_length / head_count;
        // llama.cpp: escala 0 significa "use 1/sqrt(head_dim)".
        let attention_scale = match f32_of("granite.attention.scale", 0.0)? {
            0.0 => 1.0 / (head_dim as f32).sqrt(),
            scale => scale,
        };

        Ok(Self {
            block_count: u32_of("granite.block_count")?,
            embedding_length,
            feed_forward_length: u32_of("granite.feed_forward_length")?,
            head_count,
            head_count_kv: u32_of("granite.attention.head_count_kv")?,
            head_dim,
            rope_dim: u32_of("granite.rope.dimension_count")?,
            rope_freq_base: f32_of("granite.rope.freq_base", 10_000.0)?,
            rms_norm_eps: f32_of("granite.attention.layer_norm_rms_epsilon", 1e-5)? as f64,
            context_length: u32_of("granite.context_length")?,
            vocab_size: u32_of("granite.vocab_size")?,
            attention_scale,
            embedding_scale: f32_of("granite.embedding_scale", 1.0)?,
            residual_scale: f32_of("granite.residual_scale", 1.0)?,
            logit_scale: f32_of("granite.logit_scale", 1.0)?,
        })
    }
}

fn scaled(t: Tensor, factor: f32) -> Result<Tensor> {
    if factor == 1.0 {
        Ok(t)
    } else {
        t * factor as f64
    }
}

fn rotary_tables(
    rope_dim: usize,
    freq_base: f32,
    max_seq_len: usize,
    device: &Device,
) -> Result<(Tensor, Tensor)> {
    let theta: Vec<f32> = (0..rope_dim)
        .step_by(2)
        .map(|i| 1f32 / freq_base.powf(i as f32 / rope_dim as f32))
        .collect();
    let theta = Tensor::new(theta.as_slice(), device)?.reshape((1, rope_dim / 2))?;
    let positions = Tensor::arange(0u32, max_seq_len as u32, device)?
        .to_dtype(DType::F32)?
        .reshape((max_seq_len, 1))?;
    let angles = positions.matmul(&theta)?;
    Ok((angles.cos()?, angles.sin()?))
}

/// Granite 4.2 usa RoPE intercalado (GPT-J), não o rotate_half do NEOX que o
/// granite.rs do candle aplica. Medido contra llama.cpp no mesmo GGUF: NEOX dá
/// "Não" p=0.34 num prompt de 117 tokens; intercalado dá " Paris" p=0.98.
fn apply_rope(x: &Tensor, cos: &Tensor, sin: &Tensor) -> Result<Tensor> {
    candle_nn::rotary_emb::rope_i(&x.contiguous()?, cos, sin)
}

#[derive(Clone)]
struct Layer {
    attn_norm: RmsNorm,
    ffn_norm: RmsNorm,
    wq: QMatMul,
    wk: QMatMul,
    wv: QMatMul,
    wo: QMatMul,
    gate: QMatMul,
    up: QMatMul,
    down: QMatMul,
    cos: Tensor,
    sin: Tensor,
    neg_inf: Tensor,
    head_count: usize,
    head_count_kv: usize,
    head_dim: usize,
    attention_scale: f32,
    kv_cache: Option<(Tensor, Tensor)>,
}

impl Layer {
    fn load<R: Read + Seek>(
        ct: &gguf_file::Content,
        reader: &mut R,
        device: &Device,
        index: usize,
        cfg: &Config,
        cos: &Tensor,
        sin: &Tensor,
        neg_inf: &Tensor,
    ) -> Result<Self> {
        let p = format!("blk.{index}");
        let mut tensor = |name: &str| ct.tensor(reader, &format!("{p}.{name}"), device);
        let qmatmul = |t: QTensor| QMatMul::from_qtensor(t);
        Ok(Self {
            attn_norm: RmsNorm::from_qtensor(tensor("attn_norm.weight")?, cfg.rms_norm_eps)?,
            ffn_norm: RmsNorm::from_qtensor(tensor("ffn_norm.weight")?, cfg.rms_norm_eps)?,
            wq: qmatmul(tensor("attn_q.weight")?)?,
            wk: qmatmul(tensor("attn_k.weight")?)?,
            wv: qmatmul(tensor("attn_v.weight")?)?,
            wo: qmatmul(tensor("attn_output.weight")?)?,
            gate: qmatmul(tensor("ffn_gate.weight")?)?,
            up: qmatmul(tensor("ffn_up.weight")?)?,
            down: qmatmul(tensor("ffn_down.weight")?)?,
            cos: cos.clone(),
            sin: sin.clone(),
            neg_inf: neg_inf.clone(),
            head_count: cfg.head_count,
            head_count_kv: cfg.head_count_kv,
            head_dim: cfg.head_dim,
            attention_scale: cfg.attention_scale,
            kv_cache: None,
        })
    }

    fn rope(&self, x: &Tensor, index_pos: usize) -> Result<Tensor> {
        let seq_len = x.dim(2)?;
        let cos = self.cos.narrow(0, index_pos, seq_len)?;
        let sin = self.sin.narrow(0, index_pos, seq_len)?;
        apply_rope(x, &cos, &sin)
    }

    fn attention(
        &mut self,
        x: &Tensor,
        mask: Option<&Tensor>,
        index_pos: usize,
    ) -> Result<Tensor> {
        let (b, seq_len, hidden) = x.dims3()?;
        let shape = |n: usize| (b, seq_len, n, self.head_dim);

        let q = self.wq.forward(x)?.reshape(shape(self.head_count))?.transpose(1, 2)?;
        let k = self.wk.forward(x)?.reshape(shape(self.head_count_kv))?.transpose(1, 2)?;
        let v = self
            .wv
            .forward(x)?
            .reshape(shape(self.head_count_kv))?
            .transpose(1, 2)?
            .contiguous()?;

        let q = self.rope(&q, index_pos)?;
        let k = self.rope(&k, index_pos)?;

        let (k, v) = match &self.kv_cache {
            Some((kc, vc)) if index_pos > 0 => (
                Tensor::cat(&[kc, &k], 2)?,
                Tensor::cat(&[vc, &v], 2)?,
            ),
            _ => (k, v),
        };
        self.kv_cache = Some((k.clone(), v.clone()));

        let repeats = self.head_count / self.head_count_kv;
        // O GQA repete cada cabeça de K/V `repeats` vezes. Materializar essa
        // repetição custava 5x os tensores de cache por token gerado — medido,
        // 0,26 ms por token de contexto. Como a numeração de cabeças do repeat
        // já é kv*repeats + r, os `repeats*s` do reshape são a mesma vista de q:
        // a conta por grupo dá os valores idênticos sem copiar K nem V.
        let q = q.reshape((b, self.head_count_kv, repeats * seq_len, self.head_dim))?;
        let att = (q.matmul(&k.transpose(2, 3)?)? * self.attention_scale as f64)?;
        // A máscara é por (linha, coluna) e `repeats` linhas são o mesmo bloco
        // de `seq_len` empilhado: as cinco dimensões deixam a máscara casar com
        // cada cópia da cabeça sem repetir nem a máscara nem o K/V.
        let att = match mask {
            None => att,
            Some(mask) => {
                let shape5 = (b, self.head_count_kv, repeats, seq_len, att.dim(3)?);
                let att = att.reshape(shape5)?;
                let mask = mask.broadcast_as(att.shape())?;
                let att = mask.where_cond(&self.neg_inf.broadcast_as(att.shape())?, &att)?;
                att.reshape((b, self.head_count_kv, repeats * seq_len, shape5.4))?
            }
        };
        let att = candle_nn::ops::softmax_last_dim(&att)?;
        let y = att.matmul(&v)?;
        let y = y
            .reshape((b, self.head_count, seq_len, self.head_dim))?
            .transpose(1, 2)?
            .reshape((b, seq_len, hidden))?;
        self.wo.forward(&y)
    }

    fn mlp(&self, x: &Tensor) -> Result<Tensor> {
        let gate = candle_nn::ops::silu(&self.gate.forward(x)?)?;
        let up = self.up.forward(x)?;
        self.down.forward(&(gate * up)?)
    }
}

pub struct Model {
    pub config: Config,
    embeddings: Embedding,
    layers: Vec<Layer>,
    norm: RmsNorm,
    output: QMatMul,
    masks: HashMap<(usize, usize), Tensor>,
    max_seq_len: usize,
}

impl Model {
    pub fn from_gguf<R: Read + Seek>(
        ct: gguf_file::Content,
        reader: &mut R,
        device: &Device,
        max_seq_len: usize,
    ) -> Result<Self> {
        let config = Config::from_metadata(&ct.metadata)?;
        if max_seq_len > config.context_length {
            candle::bail!(
                "{}",
                tr(
                    "erros.janelaExcedeContexto",
                    &[
                        ("janela", &max_seq_len.to_string()),
                        ("contexto", &config.context_length.to_string())
                    ]
                )
            );
        }
        let (cos, sin) = rotary_tables(config.rope_dim, config.rope_freq_base, max_seq_len, device)?;
        let neg_inf = Tensor::new(f32::NEG_INFINITY, device)?;

        let embeddings_q = ct.tensor(reader, "token_embd.weight", device)?;
        let embeddings = Embedding::new(embeddings_q.dequantize(device)?, config.embedding_length);
        let norm = RmsNorm::from_qtensor(
            ct.tensor(reader, "output_norm.weight", device)?,
            config.rms_norm_eps,
        )?;
        let output = QMatMul::from_qtensor(ct.tensor(reader, "output.weight", device)?)?;

        let mut layers = Vec::with_capacity(config.block_count);
        for index in 0..config.block_count {
            layers.push(Layer::load(
                &ct, reader, device, index, &config, &cos, &sin, &neg_inf,
            )?);
        }

        Ok(Self {
            config,
            embeddings,
            layers,
            norm,
            output,
            masks: HashMap::new(),
            max_seq_len,
        })
    }

    pub fn max_seq_len(&self) -> usize {
        self.max_seq_len
    }

    pub fn clear_kv_cache(&mut self) {
        for layer in &mut self.layers {
            layer.kv_cache = None;
        }
        self.masks.clear();
    }

    fn mask(&mut self, seq_len: usize, index_pos: usize, device: &Device) -> Result<Tensor> {
        let key = (seq_len, index_pos + seq_len);
        if let Some(mask) = self.masks.get(&key) {
            return Ok(mask.clone());
        }
        let mask = build_causal_mask(seq_len, index_pos, device)?;
        self.masks.insert(key, mask.clone());
        Ok(mask)
    }

    /// Logits da última posição, shape `(batch, vocab_size)`.
    pub fn forward(&mut self, tokens: &Tensor, index_pos: usize) -> Result<Tensor> {
        let (_b, seq_len) = tokens.dims2()?;
        if index_pos + seq_len > self.max_seq_len {
            candle::bail!(
                "{}",
                tr(
                    "erros.posicaoForaDaJanela",
                    &[
                        ("posicao", &index_pos.to_string()),
                        ("tokens", &seq_len.to_string()),
                        ("janela", &self.max_seq_len.to_string())
                    ]
                )
            );
        }
        let mask = if seq_len == 1 {
            None
        } else {
            Some(self.mask(seq_len, index_pos, tokens.device())?)
        };

        let mut h = scaled(self.embeddings.forward(tokens)?, self.config.embedding_scale)?;
        for layer in &mut self.layers {
            let residual = h;
            let normed = layer.attn_norm.forward(&residual)?;
            let attn = layer.attention(&normed, mask.as_ref(), index_pos)?;
            h = (scaled(attn, self.config.residual_scale)? + residual)?;

            let residual = h;
            let normed = layer.ffn_norm.forward(&residual)?;
            let ffn = layer.mlp(&normed)?;
            h = (scaled(ffn, self.config.residual_scale)? + residual)?;
        }
        let h = self.norm.forward(&h)?;
        let h = h.i((.., seq_len - 1, ..))?;
        let logits = self.output.forward(&h)?;
        // llama.cpp divide pelo logit_scale; multiplicar inverteria a temperatura.
        if self.config.logit_scale == 1.0 {
            Ok(logits)
        } else {
            logits / self.config.logit_scale as f64
        }
    }
}

/// Bytes de KV cache por token, para o dimensionamento honesto da janela.
pub fn kv_bytes_per_token(cfg: &Config, dtype: DType) -> usize {
    let per_element = match dtype {
        DType::F16 | DType::BF16 => 2,
        _ => 4,
    };
    2 * cfg.block_count * cfg.head_count_kv * cfg.head_dim * per_element
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value_u32(v: u32) -> gguf_file::Value {
        gguf_file::Value::U32(v)
    }

    fn granite_metadata() -> HashMap<String, gguf_file::Value> {
        let mut md = HashMap::new();
        md.insert("granite.block_count".into(), value_u32(40));
        md.insert("granite.embedding_length".into(), value_u32(2560));
        md.insert("granite.feed_forward_length".into(), value_u32(8192));
        md.insert("granite.attention.head_count".into(), value_u32(40));
        md.insert("granite.attention.head_count_kv".into(), value_u32(8));
        md.insert("granite.rope.dimension_count".into(), value_u32(64));
        md.insert("granite.context_length".into(), value_u32(131072));
        md.insert("granite.vocab_size".into(), value_u32(100352));
        md.insert(
            "granite.attention.scale".into(),
            gguf_file::Value::F32(0.015625),
        );
        md
    }

    #[test]
    fn reads_granite_4_2_3b_geometry() {
        let cfg = Config::from_metadata(&granite_metadata()).unwrap();
        assert_eq!(cfg.head_dim, 64);
        assert_eq!(cfg.head_count / cfg.head_count_kv, 5);
        assert_eq!(cfg.attention_scale, 0.015625);
        assert_eq!(cfg.embedding_scale, 1.0);
        assert_eq!(cfg.logit_scale, 1.0);
    }

    #[test]
    fn zero_attention_scale_falls_back_to_sqrt_head_dim() {
        let mut md = granite_metadata();
        md.insert("granite.attention.scale".into(), gguf_file::Value::F32(0.0));
        let cfg = Config::from_metadata(&md).unwrap();
        assert_eq!(cfg.attention_scale, 1.0 / 8.0);
    }

    #[test]
    fn missing_required_key_is_an_error_not_a_default() {
        let mut md = granite_metadata();
        md.remove("granite.block_count");
        let err = Config::from_metadata(&md).unwrap_err().to_string();
        assert!(err.contains("granite.block_count"), "{err}");
    }

    #[test]
    fn scaling_by_one_is_identity() {
        let t = Tensor::new(&[1f32, 2.0], &Device::Cpu).unwrap();
        let out = scaled(t.clone(), 1.0).unwrap();
        assert_eq!(out.to_vec1::<f32>().unwrap(), vec![1.0, 2.0]);
        let out = scaled(t, 0.5).unwrap();
        assert_eq!(out.to_vec1::<f32>().unwrap(), vec![0.5, 1.0]);
    }

    #[test]
    fn kv_budget_matches_gqa_geometry() {
        let cfg = Config::from_metadata(&granite_metadata()).unwrap();
        // 2 (k+v) * 40 camadas * 8 cabeças kv * 64 dim * 4 bytes = 163840
        assert_eq!(kv_bytes_per_token(&cfg, DType::F32), 163_840);
    }

    #[test]
    fn rotary_tables_have_half_head_dim_columns() {
        let (cos, sin) = rotary_tables(64, 1e7, 16, &Device::Cpu).unwrap();
        assert_eq!(cos.dims(), [16, 32]);
        assert_eq!(sin.dims(), [16, 32]);
    }

    #[test]
    fn rope_pairs_adjacent_channels_not_halves() {
        let dev = &Device::Cpu;
        let x = Tensor::new(&[[[[1f32, 2.0, 3.0, 4.0], [5.0, 6.0, 7.0, 8.0]]]], dev).unwrap();
        let (cos, sin) = rotary_tables(4, 10_000.0, 2, dev).unwrap();
        let flat = |t: Tensor| t.flatten_all().unwrap().to_vec1::<f32>().unwrap();
        let got = flat(apply_rope(&x, &cos, &sin).unwrap());
        let neox = flat(candle_nn::rotary_emb::rope(&x, &cos, &sin).unwrap());
        let xs = flat(x.clone());
        let c = cos.to_vec2::<f32>().unwrap();
        let s = sin.to_vec2::<f32>().unwrap();
        for p in 0..2 {
            for j in 0..2 {
                let a = xs[p * 4 + 2 * j];
                let b = xs[p * 4 + 2 * j + 1];
                assert!((got[p * 4 + 2 * j] - (a * c[p][j] - b * s[p][j])).abs() < 1e-5);
                assert!((got[p * 4 + 2 * j + 1] - (a * s[p][j] + b * c[p][j])).abs() < 1e-5);
            }
        }
        assert_ne!(got, neox, "NEOX e intercalado não podem coincidir");
    }
}