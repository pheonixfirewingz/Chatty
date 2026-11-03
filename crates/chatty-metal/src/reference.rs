//! CPU implementation of the DeepSeek-V4.1-Flash forward pass, used to check the GPU kernels. It
//! keeps activations in f32 where the upstream engine rounds to bf16 between kernels; quantization
//! points (FP8 and FP4 activations) match upstream, since they change results materially.

#![allow(clippy::chunks_exact_to_as_chunks)]

use crate::{
    Result,
    catalog::{Catalog, Dtype, EXPERT_EXTENTS},
    engram::{EngramHasher, HASH_COLS},
    invalid,
    plan::{Budget, GIB, Plan},
    routing::RouteTrace,
    store::{EngramCache, Reader},
    tokenizer::Tokenizer,
};
use rayon::prelude::*;
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

pub const DIM: usize = 5120;
pub const MOE_DIM: usize = 2304;
pub const HC: usize = 4;
pub const MIX: usize = (2 + HC) * HC;
pub const HEADS: usize = 64;
pub const HEAD_DIM: usize = 512;
pub const ROPE_DIM: usize = 64;
pub const Q_LORA: usize = 1280;
pub const GROUPS: usize = 8;
pub const O_LORA: usize = 1024;
pub const WINDOW: usize = 128;
pub const INDEX_HEADS: usize = 32;
pub const INDEX_DIM: usize = 128;
pub const INDEX_TOPK: usize = 512;
pub const ROUTED: usize = 384;
pub const ACTIVE: usize = 6;
pub const LAYERS: usize = 40;
pub const ROUTE_SCALE: f32 = 1.5;
const SWIGLU_LIMIT: f32 = 10.0;
const NORM_EPS: f32 = 1e-20;
const HC_EPS: f32 = 1e-6;
const SINKHORN_ITERS: usize = 20;
pub const CANDIDATE_SOURCE: usize = 20;
const CANDIDATE_BLOCKS: usize = 2048;
const CANDIDATE_BLOCK_SIZE: usize = 8;
const BLOCK: usize = 32;
pub const KV_SOURCES: [usize; 4] = [2, 8, 14, 20];
pub const INDEX_SOURCES: [usize; 8] = [2, 8, 14, 20, 24, 28, 32, 36];
const ROPE_THETA: f32 = 10_000.0;
const COMPRESS_ROPE_THETA: f32 = 160_000.0;
const YARN_ORIGINAL: f64 = 65_536.0;
const YARN_FACTOR: f32 = 16.0;
const YARN_BETA_FAST: f64 = 32.0;
const YARN_BETA_SLOW: f64 = 1.0;
/// Slots in each layer's window ring: the window plus the largest chunk [`Model::forward`] takes.
pub const MAX_CHUNK: usize = 256;
const RING: usize = WINDOW + MAX_CHUNK;
const FP4_TABLE: [f32; 16] = [
    0.0, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0, -0.0, -0.5, -1.0, -1.5, -2.0, -3.0, -4.0, -6.0,
];

const fn e4m3_table() -> [f32; 256] {
    let mut table = [0.0; 256];
    let mut byte = 0;
    while byte < 256 {
        let exponent = (byte >> 3) & 0xF;
        let mantissa = (byte & 7) as u32;
        let magnitude = if exponent == 15 && mantissa == 7 {
            f32::NAN
        } else if exponent == 0 {
            mantissa as f32 * f32::from_bits((127 - 9) << 23)
        } else {
            f32::from_bits(((exponent as u32 + 120) << 23) | (mantissa << 20))
        };
        table[byte] = if byte & 0x80 != 0 {
            -magnitude
        } else {
            magnitude
        };
        byte += 1;
    }
    table
}

const fn e8m0_table() -> [f32; 256] {
    let mut table = [0.0; 256];
    let mut byte = 0;
    while byte < 255 {
        table[byte] = if byte == 0 {
            // 2^-127 is subnormal in f32.
            f32::from_bits(1 << 22)
        } else {
            f32::from_bits((byte as u32) << 23)
        };
        byte += 1;
    }
    table[255] = f32::NAN;
    table
}

pub static E4M3: [f32; 256] = e4m3_table();
pub static E8M0: [f32; 256] = e8m0_table();

fn pow2(exponent: i32) -> f32 {
    f32::from_bits(((exponent + 127) as u32) << 23)
}

/// `ceil(log2(x))` for positive normal `x`, from its bits.
fn log2_ceil(x: f32) -> i32 {
    let bits = x.to_bits();
    ((bits >> 23) & 0xFF) as i32 - 127 + i32::from(bits & 0x7F_FFFF != 0)
}

/// Round to the nearest `float8_e4m3fn` value, ties to even.
pub fn round_e4m3(value: f32) -> f32 {
    let exponent = (((value.to_bits() >> 23) & 0xFF) as i32 - 127).max(-6);
    let quantum = pow2(exponent - 3);
    ((value.abs() / quantum).round_ties_even() * quantum).copysign(value)
}

/// Round to the nearest `float4_e2m1fn` value, ties to even, for `|value| <= 6`.
pub fn round_e2m1(value: f32) -> f32 {
    let a = value.abs();
    let rounded = if a <= 0.25 {
        0.0
    } else if a < 0.75 {
        0.5
    } else if a <= 1.25 {
        1.0
    } else if a < 1.75 {
        1.5
    } else if a <= 2.5 {
        2.0
    } else if a < 3.5 {
        3.0
    } else if a <= 5.0 {
        4.0
    } else {
        6.0
    };
    f32::copysign(rounded, value)
}

fn absmax(block: &[f32]) -> f32 {
    block.iter().fold(0.0, |max: f32, v| max.max(v.abs()))
}

/// FP8 activation quantization with power-of-two scales, returned already rescaled to f32.
pub fn quant_fp8(values: &mut [f32]) {
    for block in values.chunks_mut(BLOCK) {
        let scale = pow2(log2_ceil(absmax(block).max(1e-4) / 448.0));
        for v in block {
            *v = round_e4m3((*v / scale).clamp(-448.0, 448.0)) * scale;
        }
    }
}

/// FP4 with power-of-two scales per 32 elements, as the indexer uses.
pub fn quant_fp4_pow2(values: &mut [f32]) {
    for block in values.chunks_mut(BLOCK) {
        let floor = 6.0 * f32::from_bits(1 << 23);
        let scale = pow2(log2_ceil(absmax(block).max(floor) / 6.0));
        for v in block {
            *v = round_e2m1((*v / scale).clamp(-6.0, 6.0)) * scale;
        }
    }
}

/// FP4 with E4M3 scales per 16 elements, as the compressed KV uses.
pub fn quant_fp4_e4m3(values: &mut [f32]) {
    for block in values.chunks_mut(16) {
        let floor = 6.0 * pow2(-9);
        let scale = round_e4m3(absmax(block).max(floor) / 6.0);
        for v in block {
            *v = round_e2m1((*v / scale).clamp(-6.0, 6.0)) * scale;
        }
    }
}

/// Order-independent dot product, in eight lanes so the compiler can vectorize it.
#[inline]
fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut lanes = [0.0f32; 8];
    let mut ca = a.chunks_exact(8);
    let mut cb = b.chunks_exact(8);
    for (x, y) in (&mut ca).zip(&mut cb) {
        for i in 0..8 {
            lanes[i] += x[i] * y[i];
        }
    }
    let mut sum: f32 = lanes.iter().sum();
    for (x, y) in ca.remainder().iter().zip(cb.remainder()) {
        sum += x * y;
    }
    sum
}

fn rms_norm(rows: &mut [f32], weight: &[f32]) {
    for row in rows.chunks_exact_mut(weight.len()) {
        let var = dot(row, row) / weight.len() as f32;
        let scale = (var + NORM_EPS).sqrt().recip();
        for (v, w) in row.iter_mut().zip(weight) {
            *v *= scale * w;
        }
    }
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// A weight matrix whose rows multiply activations. FP8 and FP4 inputs must be quantized first.
#[derive(Clone, Copy)]
pub enum Mat<'a> {
    /// `[n, k]` with one E8M0 scale per 32x32 block.
    Fp8 { weight: &'a [u8], scale: &'a [u8] },
    /// `[n, k / 2]` of nibbles, low first, with one E8M0 scale per row and 32 elements.
    Fp4 { weight: &'a [u8], scale: &'a [u8] },
    /// `[n, k]` of bf16.
    Bf16 { weight: &'a [u8] },
}

impl Mat<'_> {
    pub fn row(&self, n: usize, k: usize, x: &[f32]) -> f32 {
        let blocks = k / BLOCK;
        let mut total = 0.0;
        let mut buf = [0.0f32; BLOCK];
        match *self {
            Mat::Fp8 { weight, scale } => {
                let w = &weight[n * k..(n + 1) * k];
                let s = &scale[(n / BLOCK) * blocks..(n / BLOCK + 1) * blocks];
                for ((wb, xb), &sb) in w.chunks_exact(BLOCK).zip(x.chunks_exact(BLOCK)).zip(s) {
                    for (out, &byte) in buf.iter_mut().zip(wb) {
                        *out = E4M3[byte as usize];
                    }
                    total += E8M0[sb as usize] * dot(&buf, xb);
                }
            }
            Mat::Fp4 { weight, scale } => {
                let w = &weight[n * k / 2..(n + 1) * k / 2];
                let s = &scale[n * blocks..(n + 1) * blocks];
                for ((wb, xb), &sb) in w.chunks_exact(BLOCK / 2).zip(x.chunks_exact(BLOCK)).zip(s) {
                    for (pair, &byte) in buf.chunks_exact_mut(2).zip(wb) {
                        pair[0] = FP4_TABLE[(byte & 15) as usize];
                        pair[1] = FP4_TABLE[(byte >> 4) as usize];
                    }
                    total += E8M0[sb as usize] * dot(&buf, xb);
                }
            }
            Mat::Bf16 { weight } => {
                let w = &weight[n * k * 2..(n + 1) * k * 2];
                for (wb, xb) in w.chunks(BLOCK * 2).zip(x.chunks(BLOCK)) {
                    for (out, b) in buf.iter_mut().zip(wb.chunks_exact(2)) {
                        *out = bf16(b);
                    }
                    total += dot(&buf[..xb.len()], xb);
                }
            }
        }
        total
    }

    /// `out[m, n] = x[m, k] @ self[n, k]^T`, computing each output element once on some core.
    pub fn apply(&self, x: &[f32], k: usize, out: &mut [f32], n: usize) {
        debug_assert_eq!(x.len() / k, out.len() / n);
        out.par_iter_mut()
            .with_min_len(16)
            .enumerate()
            .for_each(|(index, value)| {
                let row = index / n;
                *value = self.row(index % n, k, &x[row * k..(row + 1) * k]);
            });
    }
}

fn bf16(bytes: &[u8]) -> f32 {
    f32::from_bits(u32::from(u16::from_le_bytes([bytes[0], bytes[1]])) << 16)
}

/// Multiply by a quantized matrix, quantizing a copy of the input first.
fn linear_q(x: &[f32], k: usize, mat: Mat<'_>, n: usize) -> Vec<f32> {
    let mut q = x.to_vec();
    q.par_chunks_mut(k).for_each(quant_fp8);
    let mut out = vec![0.0; x.len() / k * n];
    mat.apply(&q, k, &mut out, n);
    out
}

fn linear_f(x: &[f32], k: usize, mat: Mat<'_>, n: usize) -> Vec<f32> {
    let mut out = vec![0.0; x.len() / k * n];
    mat.apply(x, k, &mut out, n);
    out
}

/// Rotary frequencies for one attention kind.
#[derive(Clone)]
pub struct Rope {
    freqs: [f32; ROPE_DIM / 2],
}

impl Rope {
    /// The rope of a layer with this compression ratio.
    pub fn for_ratio(ratio: usize) -> Self {
        if ratio > 0 {
            Self::new(COMPRESS_ROPE_THETA, true)
        } else {
            Self::new(ROPE_THETA, false)
        }
    }

    /// One position's (cos, sin) pairs, as the GPU kernels read them.
    pub fn write_table(&self, position: usize, out: &mut [f32]) {
        for (pair, f) in out.chunks_exact_mut(2).zip(&self.freqs) {
            let (sin, cos) = (position as f32 * f).sin_cos();
            pair[0] = cos;
            pair[1] = sin;
        }
    }

    fn new(base: f32, yarn: bool) -> Self {
        let dim = ROPE_DIM as f64;
        let mut freqs = [0.0; ROPE_DIM / 2];
        for (i, f) in freqs.iter_mut().enumerate() {
            *f = 1.0 / base.powf((2 * i) as f32 / ROPE_DIM as f32);
        }
        if yarn {
            let corrected = |rotations: f64| {
                dim * (YARN_ORIGINAL / (rotations * 2.0 * std::f64::consts::PI)).ln()
                    / (2.0 * f64::from(base).ln())
            };
            let low = corrected(YARN_BETA_FAST).floor().max(0.0);
            let high = corrected(YARN_BETA_SLOW).ceil().min(dim - 1.0);
            let span = ((high - low) as f32).max(1e-3);
            for (i, f) in freqs.iter_mut().enumerate() {
                let ramp = ((i as f32 - low as f32) / span).clamp(0.0, 1.0);
                let smooth = 1.0 - ramp;
                *f = *f / YARN_FACTOR * (1.0 - smooth) + *f * smooth;
            }
        }
        Self { freqs }
    }

    /// Rotate the last [`ROPE_DIM`] elements of a vector, as complex pairs.
    fn apply(&self, vector: &mut [f32], position: usize, inverse: bool) {
        let tail = vector.len() - ROPE_DIM;
        for (pair, f) in vector[tail..].chunks_exact_mut(2).zip(&self.freqs) {
            let angle = position as f32 * f;
            let (sin, cos) = angle.sin_cos();
            let sin = if inverse { -sin } else { sin };
            let (re, im) = (pair[0], pair[1]);
            pair[0] = re * cos - im * sin;
            pair[1] = re * sin + im * cos;
        }
    }
}

struct Mixes {
    pre: Vec<[f32; HC]>,
    post: Vec<[f32; HC]>,
    comb: Vec<[[f32; HC]; HC]>,
}

struct HcWeights {
    fun: Vec<f32>,
    base: Vec<f32>,
    scale: Vec<f32>,
}

impl HcWeights {
    fn mixes(&self, h: &[f32]) -> Mixes {
        let rows = h.len() / (HC * DIM);
        let mut pre = Vec::with_capacity(rows);
        let mut post = Vec::with_capacity(rows);
        let mut comb = Vec::with_capacity(rows);
        for row in h.chunks_exact(HC * DIM) {
            let rsqrt = (dot(row, row) / row.len() as f32 + NORM_EPS).sqrt().recip();
            let mut mix = [0.0; MIX];
            mix.par_iter_mut().enumerate().for_each(|(i, m)| {
                *m = dot(&self.fun[i * row.len()..(i + 1) * row.len()], row) * rsqrt;
            });
            let (p, q, c) = sinkhorn(&mix, &self.scale, &self.base);
            pre.push(p);
            post.push(q);
            comb.push(c);
        }
        Mixes { pre, post, comb }
    }
}

/// Split one projection into pre, post, and a doubly stochastic comb.
pub fn sinkhorn(
    mix: &[f32; MIX],
    scale: &[f32],
    base: &[f32],
) -> ([f32; HC], [f32; HC], [[f32; HC]; HC]) {
    let mut pre = [0.0; HC];
    let mut post = [0.0; HC];
    let mut comb = [[0.0; HC]; HC];
    for j in 0..HC {
        pre[j] = sigmoid(mix[j] * scale[0] + base[j]) + HC_EPS;
        post[j] = 2.0 * sigmoid(mix[j + HC] * scale[1] + base[j + HC]);
        for (k, value) in comb[j].iter_mut().enumerate() {
            let at = j * HC + k + HC * 2;
            *value = mix[at] * scale[2] + base[at];
        }
    }
    for row in &mut comb {
        let max = row.iter().fold(f32::NEG_INFINITY, |m, v| m.max(*v));
        let mut sum = 0.0;
        for v in row.iter_mut() {
            *v = (*v - max).exp();
            sum += *v;
        }
        for v in row.iter_mut() {
            *v = *v / sum + HC_EPS;
        }
    }
    normalize_columns(&mut comb);
    for _ in 1..SINKHORN_ITERS {
        for row in &mut comb {
            let sum: f32 = row.iter().sum();
            for v in row.iter_mut() {
                *v /= sum + HC_EPS;
            }
        }
        normalize_columns(&mut comb);
    }
    (pre, post, comb)
}

fn normalize_columns(comb: &mut [[f32; HC]; HC]) {
    for k in 0..HC {
        let sum: f32 = comb.iter().map(|row| row[k]).sum();
        for row in comb.iter_mut() {
            row[k] /= sum + HC_EPS;
        }
    }
}

fn hc_pre(h: &[f32], pre: &[[f32; HC]]) -> Vec<f32> {
    let mut out = vec![0.0; pre.len() * DIM];
    for ((o, row), mix) in out
        .chunks_exact_mut(DIM)
        .zip(h.chunks_exact(HC * DIM))
        .zip(pre)
    {
        for (copy, m) in row.chunks_exact(DIM).zip(mix) {
            for (v, c) in o.iter_mut().zip(copy) {
                *v += m * c;
            }
        }
    }
    out
}

fn hc_post(x: &[f32], residual: &[f32], mixes: &Mixes) -> Vec<f32> {
    let mut out = vec![0.0; residual.len()];
    for (t, row) in out.chunks_exact_mut(HC * DIM).enumerate() {
        let res = &residual[t * HC * DIM..(t + 1) * HC * DIM];
        let input = &x[t * DIM..(t + 1) * DIM];
        for (j, copy) in row.chunks_exact_mut(DIM).enumerate() {
            let post = mixes.post[t][j];
            for (o, v) in copy.iter_mut().zip(input) {
                *o = post * v;
            }
            for i in 0..HC {
                let c = mixes.comb[t][i][j];
                for (o, r) in copy.iter_mut().zip(res[i * DIM..(i + 1) * DIM].iter()) {
                    *o += c * r;
                }
            }
        }
    }
    out
}

/// Parallel access to the checkpoint's core tensors, loaded once for the process lifetime.
struct Core {
    bytes: &'static [u8],
    offsets: HashMap<String, (usize, usize, Dtype)>,
}

impl Core {
    fn load(catalog: &Catalog, reader: &Reader) -> Result<Self> {
        let total: u64 = catalog.core.values().map(|t| t.extent.len).sum();
        let mut bytes = vec![0u8; total as usize];
        let mut offsets = HashMap::with_capacity(catalog.core.len());
        let mut items = Vec::with_capacity(catalog.core.len());
        let mut rest = bytes.as_mut_slice();
        let mut at = 0;
        for (name, tensor) in &catalog.core {
            let len = tensor.extent.len as usize;
            let (head, tail) = rest.split_at_mut(len);
            items.push((tensor.extent, head));
            rest = tail;
            offsets.insert(name.clone(), (at, len, tensor.dtype));
            at += len;
        }
        reader.read_extents(&mut items)?;
        drop(items);
        Ok(Self {
            bytes: Box::leak(bytes.into_boxed_slice()),
            offsets,
        })
    }

    fn bytes(&self, name: &str) -> Result<&'static [u8]> {
        let Some((at, len, _)) = self.offsets.get(name) else {
            return invalid(format!("missing tensor {name}"));
        };
        Ok(&self.bytes[*at..*at + *len])
    }

    fn fp8(&self, prefix: &str) -> Result<Mat<'static>> {
        Ok(Mat::Fp8 {
            weight: self.bytes(&format!("{prefix}.weight"))?,
            scale: self.bytes(&format!("{prefix}.scale"))?,
        })
    }

    fn bf16_mat(&self, prefix: &str) -> Result<Mat<'static>> {
        Ok(Mat::Bf16 {
            weight: self.bytes(&format!("{prefix}.weight"))?,
        })
    }

    fn floats(&self, name: &str) -> Result<Vec<f32>> {
        let bytes = self.bytes(name)?;
        let Some((_, _, dtype)) = self.offsets.get(name) else {
            return invalid(format!("missing tensor {name}"));
        };
        Ok(match dtype {
            Dtype::F32 => bytes
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect(),
            Dtype::Bf16 => bytes.chunks_exact(2).map(bf16).collect(),
            other => return invalid(format!("{name} is {other:?}, expected a float")),
        })
    }
}

struct Indexer {
    wq_b: Mat<'static>,
    weights_proj: Mat<'static>,
}

struct Compressor {
    wkv: Mat<'static>,
    wgate: Option<Mat<'static>>,
    norm: Vec<f32>,
    wk: Mat<'static>,
    k_norm: Vec<f32>,
    kv_state: Vec<f32>,
    score_state: Vec<f32>,
}

/// The compressed KV and index keys one source layer has produced so far.
#[derive(Default)]
struct SourceStore {
    kv: Vec<f32>,
    keys: Vec<f32>,
}

struct Engram {
    table: usize,
    wkv: Mat<'static>,
    weight: Vec<f32>,
}

struct Layer {
    id: usize,
    ratio: usize,
    rope: Rope,
    hc_attn: HcWeights,
    hc_ffn: HcWeights,
    attn_norm: Vec<f32>,
    ffn_norm: Vec<f32>,
    q_norm: Vec<f32>,
    kv_norm: Vec<f32>,
    attn_sink: Vec<f32>,
    wq_a: Mat<'static>,
    wq_b: Mat<'static>,
    wkv: Mat<'static>,
    wo_a: Mat<'static>,
    wo_b: Mat<'static>,
    gate: Mat<'static>,
    gate_bias: Vec<f32>,
    shared: [Mat<'static>; 3],
    indexer: Option<Indexer>,
    compressor: Option<Compressor>,
    engram: Option<Engram>,
    ring: Vec<f32>,
}

impl Layer {
    fn load(id: usize, core: &Core, ratio: usize, engram_table: Option<usize>) -> Result<Self> {
        let p = format!("layers.{id}");
        let hc = |kind: &str| -> Result<HcWeights> {
            Ok(HcWeights {
                fun: core.floats(&format!("{p}.hc_{kind}_fn"))?,
                base: core.floats(&format!("{p}.hc_{kind}_base"))?,
                scale: core.floats(&format!("{p}.hc_{kind}_scale"))?,
            })
        };
        let is_index = INDEX_SOURCES.contains(&id);
        let indexer = is_index
            .then(|| -> Result<Indexer> {
                Ok(Indexer {
                    wq_b: core.fp8(&format!("{p}.attn.indexer.wq_b"))?,
                    weights_proj: core.bf16_mat(&format!("{p}.attn.indexer.weights_proj"))?,
                })
            })
            .transpose()?;
        let compressor = KV_SOURCES
            .contains(&id)
            .then(|| -> Result<Compressor> {
                let wgate = (ratio > 1)
                    .then(|| core.bf16_mat(&format!("{p}.attn.compressor.wgate")))
                    .transpose()?;
                Ok(Compressor {
                    wkv: core.bf16_mat(&format!("{p}.attn.compressor.wkv"))?,
                    wgate,
                    norm: core.floats(&format!("{p}.attn.compressor.norm.weight"))?,
                    wk: core.bf16_mat(&format!("{p}.attn.indexer.wk"))?,
                    k_norm: core.floats(&format!("{p}.attn.indexer.k_norm.weight"))?,
                    kv_state: vec![0.0; ratio * HEAD_DIM],
                    score_state: vec![f32::NEG_INFINITY; ratio * HEAD_DIM],
                })
            })
            .transpose()?;
        let engram = engram_table
            .map(|table| -> Result<Engram> {
                let q = core.floats(&format!("{p}.engram.q_weight"))?;
                let k = core.floats(&format!("{p}.engram.k_weight"))?;
                Ok(Engram {
                    table,
                    wkv: core.fp8(&format!("{p}.engram.wkv"))?,
                    weight: q.iter().zip(&k).map(|(a, b)| a * b).collect(),
                })
            })
            .transpose()?;
        let rope = Rope::for_ratio(ratio);
        Ok(Self {
            id,
            ratio,
            rope,
            hc_attn: hc("attn")?,
            hc_ffn: hc("ffn")?,
            attn_norm: core.floats(&format!("{p}.attn_norm.weight"))?,
            ffn_norm: core.floats(&format!("{p}.ffn_norm.weight"))?,
            q_norm: core.floats(&format!("{p}.attn.q_norm.weight"))?,
            kv_norm: core.floats(&format!("{p}.attn.kv_norm.weight"))?,
            attn_sink: core.floats(&format!("{p}.attn.attn_sink"))?,
            wq_a: core.fp8(&format!("{p}.attn.wq_a"))?,
            wq_b: core.fp8(&format!("{p}.attn.wq_b"))?,
            wkv: core.fp8(&format!("{p}.attn.wkv"))?,
            wo_a: core.fp8(&format!("{p}.attn.wo_a"))?,
            wo_b: core.fp8(&format!("{p}.attn.wo_b"))?,
            gate: core.bf16_mat(&format!("{p}.ffn.gate"))?,
            gate_bias: core.floats(&format!("{p}.ffn.gate.bias"))?,
            shared: [
                core.fp8(&format!("{p}.ffn.shared_experts.w1"))?,
                core.fp8(&format!("{p}.ffn.shared_experts.w2"))?,
                core.fp8(&format!("{p}.ffn.shared_experts.w3"))?,
            ],
            indexer,
            compressor,
            engram,
            ring: vec![0.0; RING * HEAD_DIM],
        })
    }
}

/// One expert's bytes, in catalog order: weight and scale of w1, w2, w3.
struct LoadedExpert {
    bytes: Vec<u8>,
    ends: [usize; EXPERT_EXTENTS],
    stamp: u64,
}

impl LoadedExpert {
    fn slice(&self, index: usize) -> &[u8] {
        let start = if index == 0 { 0 } else { self.ends[index - 1] };
        &self.bytes[start..self.ends[index]]
    }

    fn mat(&self, matrix: usize) -> Mat<'_> {
        Mat::Fp4 {
            weight: self.slice(matrix * 2),
            scale: self.slice(matrix * 2 + 1),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ExpertCounters {
    pub hits: u64,
    pub drive_reads: u64,
}

/// LRU over expert bytes in RAM. Routing is known before any expert runs, so one call reads every
/// miss of a layer in a single coalesced batch.
struct ExpertRam {
    catalog: Arc<Catalog>,
    entries: HashMap<(usize, usize), LoadedExpert>,
    capacity: usize,
    tick: u64,
    counters: ExpertCounters,
}

impl ExpertRam {
    fn ensure(&mut self, layer: usize, experts: &[usize], reader: &Reader) -> Result<()> {
        self.tick += 1;
        let mut missing = Vec::new();
        for &expert in experts {
            if let Some(entry) = self.entries.get_mut(&(layer, expert)) {
                entry.stamp = self.tick;
                self.counters.hits += 1;
            } else {
                missing.push(expert);
            }
        }
        self.counters.drive_reads += missing.len() as u64;
        while self.entries.len() + missing.len() > self.capacity {
            let oldest = self
                .entries
                .iter()
                .filter(|(_, e)| e.stamp != self.tick)
                .min_by_key(|(_, e)| e.stamp)
                .map(|(k, _)| *k);
            let Some(oldest) = oldest else {
                break;
            };
            self.entries.remove(&oldest);
        }
        let mut loaded = Vec::with_capacity(missing.len());
        for &expert in &missing {
            let Some(extents) = self.catalog.expert(layer, expert) else {
                return invalid(format!("missing expert ({layer}, {expert})"));
            };
            let total: u64 = extents.iter().map(|e| e.len).sum();
            let mut ends = [0; EXPERT_EXTENTS];
            let mut end = 0;
            for (slot, extent) in ends.iter_mut().zip(extents) {
                end += extent.len as usize;
                *slot = end;
            }
            loaded.push((expert, *extents, vec![0u8; total as usize], ends));
        }
        let mut items = Vec::with_capacity(loaded.len() * EXPERT_EXTENTS);
        for (_, extents, bytes, _) in &mut loaded {
            let mut rest = bytes.as_mut_slice();
            for extent in extents.iter() {
                let (head, tail) = rest.split_at_mut(extent.len as usize);
                items.push((*extent, head));
                rest = tail;
            }
        }
        reader.read_extents(&mut items)?;
        drop(items);
        for (expert, _, bytes, ends) in loaded {
            self.entries.insert(
                (layer, expert),
                LoadedExpert {
                    bytes,
                    ends,
                    stamp: self.tick,
                },
            );
        }
        Ok(())
    }
}

pub struct Model {
    reader: Reader,
    layers: Vec<Layer>,
    embed: &'static [u8],
    head: Mat<'static>,
    norm: Vec<f32>,
    experts: ExpertRam,
    engram: EngramCache,
    hasher: EngramHasher,
    shared: Shared,
    position: usize,
    pub trace: RouteTrace,
    /// When set, per-layer activation fingerprints are recorded for GPU comparison.
    pub fingerprints: Option<Vec<(usize, f32, f32)>>,
    /// When set, values after each sub-step of the first layers, for GPU comparison.
    pub probes: Probes,
}

/// Named activation snapshots.
pub type Probes = Option<Vec<(String, Vec<f32>)>>;
/// Layers whose sub-steps are captured when probing.
pub const PROBE_LAYERS: usize = 3;

fn probe(probes: &mut Probes, layer: usize, name: &str, values: &[f32]) {
    if layer < PROBE_LAYERS
        && let Some(list) = probes
    {
        list.push((format!("{layer}.{name}"), values.to_vec()));
    }
}

fn mix_values(mix: &Mixes) -> Vec<f32> {
    let mut out = Vec::with_capacity(mix.pre.len() * MIX);
    for t in 0..mix.pre.len() {
        out.extend_from_slice(&mix.pre[t]);
        out.extend_from_slice(&mix.post[t]);
        for row in &mix.comb[t] {
            out.extend_from_slice(row);
        }
    }
    out
}

impl Model {
    pub fn load(catalog: Arc<Catalog>, tokenizer: &Tokenizer, expert_ram_gib: u64) -> Result<Self> {
        let reader = Reader::open(&catalog.shards, crate::store::DEFAULT_IO_WORKERS)?;
        let core = Core::load(&catalog, &reader)?;
        let config = &catalog.config;
        if config.n_layers != LAYERS
            || config.n_routed_experts != ROUTED
            || config.n_activated_experts != ACTIVE
        {
            return invalid("checkpoint shape differs from what this engine implements");
        }
        let (map, vocab) = tokenizer.compressed_map();
        let hasher = EngramHasher::new(
            map,
            vocab,
            &config.engram_layer_ids,
            &config.engram_num_embeddings,
        )?;
        let mut layers = Vec::with_capacity(LAYERS);
        for id in 0..LAYERS {
            let table = catalog.engram.iter().position(|t| t.layer == id);
            let ratio = config.compress_ratios.get(id).copied().unwrap_or(0) as usize;
            layers.push(Layer::load(id, &core, ratio, table)?);
        }
        let plan = Plan::new(&catalog, Budget::default())?;
        let engram = EngramCache::new(&catalog, &plan)?;
        let slot = catalog.expert_slot.max(1);
        let capacity = (expert_ram_gib * GIB / slot) as usize;
        Ok(Self {
            embed: core.bytes("embed.weight")?,
            head: core.bf16_mat("head")?,
            norm: core.floats("norm.weight")?,
            reader,
            layers,
            experts: ExpertRam {
                catalog,
                entries: HashMap::new(),
                capacity,
                tick: 0,
                counters: ExpertCounters::default(),
            },
            engram,
            hasher,
            shared: Shared::default(),
            position: 0,
            trace: RouteTrace::default(),
            fingerprints: None,
            probes: None,
        })
    }

    pub fn position(&self) -> usize {
        self.position
    }

    pub fn expert_counters(&self) -> ExpertCounters {
        self.experts.counters
    }

    /// Forget the conversation. The caches of weights stay warm.
    pub fn reset(&mut self) {
        self.position = 0;
        self.hasher.reset();
        for layer in &mut self.layers {
            if let Some(c) = &mut layer.compressor {
                c.score_state.fill(f32::NEG_INFINITY);
            }
        }
        self.shared = Shared::default();
    }

    /// Run up to [`MAX_CHUNK`] tokens and return the logits after the last one.
    pub fn forward(&mut self, ids: &[u32]) -> Result<Vec<f32>> {
        let m = ids.len();
        if m == 0 || m > MAX_CHUNK {
            return invalid(format!("chunk of {m} tokens outside 1..={MAX_CHUNK}"));
        }
        let start = self.position;
        let hashes = self.hasher.push(ids);
        let engram_layers = self.layers.iter().filter(|l| l.engram.is_some()).count();
        let trace_tokens: Vec<u32> = ids.iter().map(|_| self.trace.next_token()).collect();

        let mut h = vec![0.0; m * HC * DIM];
        for (row, &id) in h.chunks_exact_mut(HC * DIM).zip(ids) {
            let at = id as usize * DIM * 2;
            let Some(bytes) = self.embed.get(at..at + DIM * 2) else {
                return invalid(format!("token {id} outside the embedding"));
            };
            for copy in row.chunks_exact_mut(DIM) {
                for (v, b) in copy.iter_mut().zip(bytes.chunks_exact(2)) {
                    *v = bf16(b);
                }
            }
        }
        let mut pre_mix = vec![[1.0, 0.0, 0.0, 0.0]; m];

        self.shared.topk = vec![Vec::new(); m];
        self.shared.candidates = vec![Vec::new(); m];
        for index in 0..LAYERS {
            let layer = &mut self.layers[index];
            if let Some(engram) = &layer.engram {
                let table = engram.table;
                let row_stride = engram_layers * HASH_COLS;
                let rows: Vec<u64> = hashes
                    .chunks_exact(row_stride)
                    .flat_map(|t| {
                        t[table * HASH_COLS..(table + 1) * HASH_COLS]
                            .iter()
                            .copied()
                    })
                    .collect();
                let row_bytes = self.engram.row_bytes();
                let mut raw = vec![0u8; rows.len() * row_bytes];
                self.engram
                    .lookup_batch(table, &rows, &mut raw, &self.reader)?;
                let weight_bytes = self.engram.weight_bytes();
                apply_engram(engram, &mut h, &raw, row_bytes, weight_bytes);
            }

            let residual = h;
            let attn_mix = layer.hc_attn.mixes(&residual);
            probe(&mut self.probes, index, "mix_attn", &mix_values(&attn_mix));
            let mut x = hc_pre(&residual, &pre_mix);
            rms_norm(&mut x, &layer.attn_norm);
            probe(&mut self.probes, index, "x_attn", &x);
            let out = attention(layer, &x, start, &mut self.shared, &mut self.probes);
            probe(&mut self.probes, index, "out_attn", &out);
            h = hc_post(&out, &residual, &attn_mix);
            probe(&mut self.probes, index, "h_attn", &h);

            let residual = h;
            let ffn_mix = layer.hc_ffn.mixes(&residual);
            let mut x = hc_pre(&residual, &attn_mix.pre);
            rms_norm(&mut x, &layer.ffn_norm);
            probe(&mut self.probes, index, "mix_ffn", &mix_values(&ffn_mix));
            probe(&mut self.probes, index, "x_ffn", &x);
            let out = moe(
                layer,
                &x,
                &mut self.experts,
                &self.reader,
                &mut self.trace,
                &trace_tokens,
            )?;
            probe(&mut self.probes, index, "out_ffn", &out);
            h = hc_post(&out, &residual, &ffn_mix);
            pre_mix = ffn_mix.pre;

            if let Some(fingerprints) = &mut self.fingerprints {
                let last = &h[(m - 1) * HC * DIM..];
                let mean = last.iter().sum::<f32>() / last.len() as f32;
                fingerprints.push((index, mean, dot(last, last).sqrt()));
            }
        }
        let mut x = hc_pre(&h, &pre_mix);
        rms_norm(&mut x, &self.norm);
        let vocab = self.embed.len() / (DIM * 2);
        let last = &x[(m - 1) * DIM..];
        let logits = linear_f(last, DIM, self.head, vocab);
        self.position += m;
        Ok(logits)
    }
}

/// What later layers read from their source layers within one forward.
#[derive(Default)]
struct Shared {
    /// Per query, the compressed positions to attend to.
    topk: Vec<Vec<u32>>,
    /// Per query, which visible compressed positions the candidate source kept.
    candidates: Vec<Vec<bool>>,
    /// One store per KV source layer, kept across forwards.
    stores: [SourceStore; KV_SOURCES.len()],
    /// The store of the most recent source layer.
    current: usize,
}

/// Dequantize looked-up rows into one `HASH_COLS * weight_bytes` vector per token.
pub fn engram_values(raw: &[u8], tokens: usize, row_bytes: usize, weight_bytes: usize) -> Vec<f32> {
    let mut values = vec![0.0; tokens * HASH_COLS * weight_bytes];
    for (out, row) in values
        .chunks_exact_mut(weight_bytes)
        .zip(raw.chunks_exact(row_bytes))
    {
        let (weight, scale) = row.split_at(weight_bytes);
        for (block, (w, s)) in out
            .chunks_exact_mut(BLOCK)
            .zip(weight.chunks_exact(BLOCK).zip(scale))
        {
            let s = E8M0[*s as usize];
            for (v, b) in block.iter_mut().zip(w) {
                *v = E4M3[*b as usize] * s;
            }
        }
    }
    values
}

fn apply_engram(engram: &Engram, h: &mut [f32], raw: &[u8], row_bytes: usize, weight_bytes: usize) {
    let m = h.len() / (HC * DIM);
    let flat = HASH_COLS * weight_bytes;
    let values = engram_values(raw, m, row_bytes, weight_bytes);
    let kv = linear_q(&values, flat, engram.wkv, (HC + 1) * DIM);
    for (row, out) in h
        .chunks_exact_mut(HC * DIM)
        .zip(kv.chunks_exact((HC + 1) * DIM))
    {
        let (keys, value) = out.split_at(HC * DIM);
        for (j, (copy, key)) in row
            .chunks_exact_mut(DIM)
            .zip(keys.chunks_exact(DIM))
            .enumerate()
        {
            let weight = &engram.weight[j * DIM..(j + 1) * DIM];
            let rstd = (dot(copy, copy) / DIM as f32 + NORM_EPS).sqrt().recip()
                * (dot(key, key) / DIM as f32 + NORM_EPS).sqrt().recip();
            let mut sum = 0.0;
            for ((c, w), k) in copy.iter().zip(weight).zip(key) {
                sum += c * w * k;
            }
            let dotted = sum * rstd * (DIM as f32).sqrt().recip();
            let root = dotted.abs().max(1e-6).sqrt().copysign(dotted);
            let gate = sigmoid(root);
            for (c, v) in copy.iter_mut().zip(value) {
                *c += gate * v;
            }
        }
    }
}

fn attention(
    layer: &mut Layer,
    x: &[f32],
    start: usize,
    shared: &mut Shared,
    probes: &mut Probes,
) -> Vec<f32> {
    let m = x.len() / DIM;
    if layer.id < PROBE_LAYERS && probes.is_some() {
        let mut quantized = x.to_vec();
        quantized.chunks_mut(DIM).for_each(quant_fp8);
        probe(probes, layer.id, "xq", &quantized);
    }
    let mut qr = linear_q(x, DIM, layer.wq_a, Q_LORA);
    probe(probes, layer.id, "qr_raw", &qr);
    rms_norm(&mut qr, &layer.q_norm);
    probe(probes, layer.id, "qr", &qr);
    let mut q = linear_q(&qr, Q_LORA, layer.wq_b, HEADS * HEAD_DIM);
    for (t, query) in q.chunks_exact_mut(HEADS * HEAD_DIM).enumerate() {
        for head in query.chunks_exact_mut(HEAD_DIM) {
            layer.rope.apply(head, start + t, false);
        }
    }
    probe(probes, layer.id, "q", &q);

    let mut kv_new = linear_q(x, DIM, layer.wkv, HEAD_DIM);
    rms_norm(&mut kv_new, &layer.kv_norm);
    for (t, row) in kv_new.chunks_exact_mut(HEAD_DIM).enumerate() {
        layer.rope.apply(row, start + t, false);
        quant_fp8(row);
        let slot = (start + t) % RING;
        layer.ring[slot * HEAD_DIM..(slot + 1) * HEAD_DIM].copy_from_slice(row);
    }

    probe(probes, layer.id, "kv_new", &kv_new);

    if layer.ratio > 0 {
        compress_and_index(layer, x, &qr, start, shared);
    }

    let mut o = vec![0.0; m * HEADS * HEAD_DIM];
    let layer = &*layer;
    for t in 0..m {
        let position = start + t;
        let first = position.saturating_sub(WINDOW - 1);
        let mut sources: Vec<&[f32]> = (first..=position)
            .map(|p| {
                let slot = p % RING;
                &layer.ring[slot * HEAD_DIM..(slot + 1) * HEAD_DIM]
            })
            .collect();
        if layer.ratio > 0 {
            let kv = &shared.stores[shared.current].kv;
            for &index in &shared.topk[t] {
                let at = index as usize * HEAD_DIM;
                sources.push(&kv[at..at + HEAD_DIM]);
            }
        }
        let query = &q[t * HEADS * HEAD_DIM..(t + 1) * HEADS * HEAD_DIM];
        let out = &mut o[t * HEADS * HEAD_DIM..(t + 1) * HEADS * HEAD_DIM];
        out.par_chunks_mut(HEAD_DIM)
            .zip(query.par_chunks(HEAD_DIM))
            .zip(layer.attn_sink.par_iter())
            .for_each(|((out, head), &sink)| {
                sparse_attention(head, &sources, sink, out);
            });
    }
    for (t, query) in o.chunks_exact_mut(HEADS * HEAD_DIM).enumerate() {
        for head in query.chunks_exact_mut(HEAD_DIM) {
            layer.rope.apply(head, start + t, true);
        }
        quant_fp8(query);
    }

    probe(probes, layer.id, "o", &o);

    // wo_a is block-diagonal over groups: each group projects only its own heads.
    let group_in = HEADS * HEAD_DIM / GROUPS;
    let mat = layer.wo_a;
    let mut flat = vec![0.0; m * GROUPS * O_LORA];
    flat.par_iter_mut()
        .with_min_len(16)
        .enumerate()
        .for_each(|(index, value)| {
            let t = index / (GROUPS * O_LORA);
            let row = index % (GROUPS * O_LORA);
            let group = row / O_LORA;
            let start = t * HEADS * HEAD_DIM + group * group_in;
            *value = mat.row(row, group_in, &o[start..start + group_in]);
        });
    probe(probes, layer.id, "flat", &flat);
    linear_q(&flat, GROUPS * O_LORA, layer.wo_b, DIM)
}

fn sparse_attention(q: &[f32], sources: &[&[f32]], sink: f32, out: &mut [f32]) {
    let scale = (HEAD_DIM as f32).sqrt().recip();
    let scores: Vec<f32> = sources.iter().map(|kv| dot(q, kv) * scale).collect();
    let max = scores.iter().fold(-1e30f32, |m, s| m.max(*s));
    out.fill(0.0);
    let mut sum = 0.0;
    for (kv, score) in sources.iter().zip(&scores) {
        let weight = (score - max).exp();
        sum += weight;
        for (o, v) in out.iter_mut().zip(*kv) {
            *o += weight * v;
        }
    }
    sum += (sink - max).exp();
    for o in out.iter_mut() {
        *o /= sum;
    }
}

/// Source layers append this chunk's completed groups; index sources pick per-query positions.
fn compress_and_index(layer: &mut Layer, x: &[f32], qr: &[f32], start: usize, shared: &mut Shared) {
    let m = x.len() / DIM;
    let ratio = layer.ratio;
    if let Some(source) = KV_SOURCES.iter().position(|&id| id == layer.id) {
        shared.current = source;
    }
    if let Some(c) = &mut layer.compressor {
        let store = &mut shared.stores[shared.current];
        let kv = linear_f(x, DIM, c.wkv, HEAD_DIM);
        let mut latents: Vec<f32> = Vec::with_capacity(m / ratio.max(1) * HEAD_DIM);
        let mut positions = Vec::with_capacity(m / ratio.max(1) + 1);
        if let Some(wgate) = c.wgate {
            let score = linear_f(x, DIM, wgate, HEAD_DIM);
            for t in 0..m {
                let position = start + t;
                let slot = position % ratio;
                let range = t * HEAD_DIM..(t + 1) * HEAD_DIM;
                c.kv_state[slot * HEAD_DIM..(slot + 1) * HEAD_DIM]
                    .copy_from_slice(&kv[range.clone()]);
                c.score_state[slot * HEAD_DIM..(slot + 1) * HEAD_DIM]
                    .copy_from_slice(&score[range]);
                if (position + 1).is_multiple_of(ratio) {
                    latents.extend(pool_group(&c.kv_state, &c.score_state, ratio));
                    positions.push(position + 1 - ratio);
                }
            }
        } else {
            latents.extend_from_slice(&kv);
            positions.extend(start..start + m);
        }
        rms_norm(&mut latents, &c.norm);
        if !latents.is_empty() {
            let mut keys = linear_f(&latents, HEAD_DIM, c.wk, INDEX_DIM);
            rms_norm(&mut keys, &c.k_norm);
            for (key, &position) in keys.chunks_exact_mut(INDEX_DIM).zip(&positions) {
                layer.rope.apply(key, position, false);
                quant_fp4_pow2(key);
            }
            store.keys.extend_from_slice(&keys);
            // Indexing needs the latent before rotation, so the KV is finished only here.
            for (latent, &position) in latents.chunks_exact_mut(HEAD_DIM).zip(&positions) {
                layer.rope.apply(latent, position, false);
                quant_fp4_e4m3(latent);
            }
            store.kv.extend_from_slice(&latents);
        }
    }

    let Some(indexer) = &layer.indexer else {
        return;
    };
    let mut q = linear_q(qr, Q_LORA, indexer.wq_b, INDEX_HEADS * INDEX_DIM);
    for (t, query) in q.chunks_exact_mut(INDEX_HEADS * INDEX_DIM).enumerate() {
        for head in query.chunks_exact_mut(INDEX_DIM) {
            layer.rope.apply(head, start + t, false);
            quant_fp4_pow2(head);
        }
    }
    let mut weights = linear_f(x, DIM, indexer.weights_proj, INDEX_HEADS);
    let scale = ((INDEX_DIM * INDEX_HEADS) as f32).sqrt().recip();
    for w in &mut weights {
        *w *= scale;
    }
    let keys = &shared.stores[shared.current].keys;
    for t in 0..m {
        let visible = (start + t + 1) / ratio;
        if visible == 0 {
            shared.topk[t].clear();
            continue;
        }
        let query = &q[t * INDEX_HEADS * INDEX_DIM..(t + 1) * INDEX_HEADS * INDEX_DIM];
        let head_weights = &weights[t * INDEX_HEADS..(t + 1) * INDEX_HEADS];
        let mut scores: Vec<f32> = (0..visible)
            .into_par_iter()
            .with_min_len(256)
            .map(|position| {
                let key = &keys[position * INDEX_DIM..(position + 1) * INDEX_DIM];
                query
                    .chunks_exact(INDEX_DIM)
                    .zip(head_weights)
                    .map(|(head, w)| dot(head, key).max(0.0) * w)
                    .sum()
            })
            .collect();
        if layer.id == CANDIDATE_SOURCE {
            shared.candidates[t] = candidate_blocks(&scores);
        } else if layer.id > CANDIDATE_SOURCE {
            for (score, keep) in scores.iter_mut().zip(&shared.candidates[t]) {
                if !keep {
                    *score = f32::NEG_INFINITY;
                }
            }
        }
        shared.topk[t] = top_positions(&scores, INDEX_TOPK);
    }
}

/// Softmax-gated pooling of one complete group, independently per dimension.
fn pool_group(kv: &[f32], score: &[f32], ratio: usize) -> Vec<f32> {
    let mut out = vec![0.0; HEAD_DIM];
    for (d, value) in out.iter_mut().enumerate() {
        let max = (0..ratio).fold(f32::NEG_INFINITY, |m, r| m.max(score[r * HEAD_DIM + d]));
        let mut sum = 0.0;
        let mut acc = 0.0;
        for r in 0..ratio {
            let weight = (score[r * HEAD_DIM + d] - max).exp();
            sum += weight;
            acc += weight * kv[r * HEAD_DIM + d];
        }
        *value = acc / sum;
    }
    out
}

/// Level one of the two-level top-k: keep the best blocks by their best position. The newest
/// block is only partly filled, so it always stays in.
pub fn candidate_blocks(scores: &[f32]) -> Vec<bool> {
    let blocks = scores.len().div_ceil(CANDIDATE_BLOCK_SIZE);
    let mut best: Vec<(f32, usize)> = scores
        .chunks(CANDIDATE_BLOCK_SIZE)
        .enumerate()
        .map(|(block, chunk)| {
            (
                chunk.iter().fold(f32::NEG_INFINITY, |m, s| m.max(*s)),
                block,
            )
        })
        .collect();
    best[blocks - 1].0 = f32::INFINITY;
    if blocks > CANDIDATE_BLOCKS {
        best.select_nth_unstable_by(CANDIDATE_BLOCKS - 1, |a, b| b.0.total_cmp(&a.0));
        best.truncate(CANDIDATE_BLOCKS);
    }
    let mut keep = vec![false; scores.len()];
    for (score, block) in best {
        if score > f32::NEG_INFINITY {
            let start = block * CANDIDATE_BLOCK_SIZE;
            let end = (start + CANDIDATE_BLOCK_SIZE).min(scores.len());
            keep[start..end].fill(true);
        }
    }
    keep
}

/// The best `k` positions with a finite score, in position order.
pub fn top_positions(scores: &[f32], k: usize) -> Vec<u32> {
    let mut order: Vec<u32> = (0..scores.len() as u32)
        .filter(|&i| scores[i as usize] > f32::NEG_INFINITY)
        .collect();
    if order.len() > k {
        order.select_nth_unstable_by(k - 1, |a, b| {
            scores[*b as usize].total_cmp(&scores[*a as usize])
        });
        order.truncate(k);
    }
    order.sort_unstable();
    order
}

fn moe(
    layer: &Layer,
    x: &[f32],
    experts: &mut ExpertRam,
    reader: &Reader,
    trace: &mut RouteTrace,
    tokens: &[u32],
) -> Result<Vec<f32>> {
    let m = x.len() / DIM;
    let raw = linear_f(x, DIM, layer.gate, ROUTED);
    let mut quantized = x.to_vec();
    quantized.par_chunks_mut(DIM).for_each(quant_fp8);

    // Route first. Group tokens by expert, so each expert's weights are walked once per chunk.
    let mut groups: BTreeMap<usize, Vec<(usize, f32)>> = BTreeMap::new();
    for (t, row) in raw.chunks_exact(ROUTED).enumerate() {
        for (expert, weight) in route(row, &layer.gate_bias) {
            trace.record(tokens[t], layer.id, expert);
            groups.entry(expert).or_default().push((t, weight));
        }
    }
    let active: Vec<usize> = groups.keys().copied().collect();
    experts.ensure(layer.id, &active, reader)?;

    let mut y = vec![0.0; m * DIM];
    for (&expert, members) in &groups {
        let Some(entry) = experts.entries.get(&(layer.id, expert)) else {
            return invalid("expert was evicted while in use");
        };
        let mut input = Vec::with_capacity(members.len() * DIM);
        for &(t, _) in members {
            input.extend_from_slice(&quantized[t * DIM..(t + 1) * DIM]);
        }
        let weights: Vec<f32> = members.iter().map(|(_, w)| *w).collect();
        let out = expert_forward(
            &input,
            [entry.mat(0), entry.mat(1), entry.mat(2)],
            Some(&weights),
        );
        for (&(t, _), row) in members.iter().zip(out.chunks_exact(DIM)) {
            for (o, v) in y[t * DIM..(t + 1) * DIM].iter_mut().zip(row) {
                *o += v;
            }
        }
    }
    let shared = expert_forward(&quantized, layer.shared, None);
    for (o, v) in y.iter_mut().zip(&shared) {
        *o += v;
    }
    Ok(y)
}

/// Pick a token's experts from its raw gate outputs. The bias picks; it does not scale.
pub fn route(raw: &[f32], bias: &[f32]) -> [(usize, f32); ACTIVE] {
    let scores: Vec<f32> = raw.iter().map(|s| s.exp().ln_1p().sqrt()).collect();
    let mut order: Vec<usize> = (0..scores.len()).collect();
    let key = |i: usize| scores[i] + bias[i];
    order.select_nth_unstable_by(ACTIVE - 1, |a, b| key(*b).total_cmp(&key(*a)));
    let sum: f32 = order[..ACTIVE].iter().map(|&i| scores[i]).sum::<f32>() + 1e-20;
    let mut chosen = [(0, 0.0); ACTIVE];
    for (slot, &i) in chosen.iter_mut().zip(&order[..ACTIVE]) {
        *slot = (i, scores[i] / sum * ROUTE_SCALE);
    }
    chosen
}

/// One SwiGLU FFN over already-quantized inputs. `mats` is w1, w2, w3.
fn expert_forward(x: &[f32], mats: [Mat<'_>; 3], weights: Option<&[f32]>) -> Vec<f32> {
    let [w1, w2, w3] = mats;
    let gate = linear_f(x, DIM, w1, MOE_DIM);
    let up = linear_f(x, DIM, w3, MOE_DIM);
    let mut mid: Vec<f32> = gate
        .iter()
        .zip(&up)
        .map(|(g, u)| {
            let g = g.min(SWIGLU_LIMIT);
            let u = u.clamp(-SWIGLU_LIMIT, SWIGLU_LIMIT);
            g * sigmoid(g) * u
        })
        .collect();
    if let Some(weights) = weights {
        for (row, w) in mid.chunks_exact_mut(MOE_DIM).zip(weights) {
            for v in row {
                *v *= w;
            }
        }
    }
    linear_q(&mid, MOE_DIM, w2, DIM)
}

/// The `float8_e4m3fn` byte of a value this format can represent.
pub fn encode_e4m3(value: f32) -> u8 {
    let bits = value.to_bits();
    let sign = ((bits >> 24) & 0x80) as u8;
    let a = value.abs();
    if a < f32::from_bits((127 - 6) << 23) {
        // Subnormal in E4M3: multiples of 2^-9.
        return sign | (a * 512.0) as u8;
    }
    let exponent = ((bits >> 23) & 0xFF) as i32 - 127 + 7;
    let mantissa = (bits >> 20) & 7;
    sign | ((exponent as u8) << 3) | mantissa as u8
}

/// How `requantize_row` rounds a matrix row's weights to FP4.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Fp4Format {
    /// Groups of 16 with E4M3 scales, after a per-tensor factor. NVIDIA's NVFP4.
    Nv,
    /// Groups of 32 with power-of-two scales. The experts' own format, MXFP4.
    Mx,
}

/// Round one row of real-valued weights to FP4 values, in place.
pub fn requantize_row(values: &mut [f32], format: Fp4Format, tensor_factor: f32) {
    match format {
        Fp4Format::Nv => {
            for group in values.chunks_mut(16) {
                let scale = round_e4m3(absmax(group) / tensor_factor / 6.0) * tensor_factor;
                if scale == 0.0 {
                    group.fill(0.0);
                    continue;
                }
                for v in group {
                    *v = round_e2m1((*v / scale).clamp(-6.0, 6.0)) * scale;
                }
            }
        }
        Fp4Format::Mx => quant_fp4_pow2(values),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn e4m3_encoding_inverts_the_table() {
        for (byte, &value) in E4M3.iter().enumerate() {
            if !value.is_nan() && byte != 0x80 {
                assert_eq!(encode_e4m3(value) as usize, byte, "value {value}");
            }
        }
    }

    #[test]
    fn nv_rounding_keeps_the_group_peak_and_zeroes_empty_groups() {
        let mut row = vec![0.0f32; 32];
        row[0] = 6.0;
        row[1] = 1.1;
        requantize_row(&mut row, Fp4Format::Nv, 1.0);
        assert_eq!(row[0], 6.0);
        assert_eq!(row[1], 1.0);
        assert!(row[16..].iter().all(|v| *v == 0.0));
    }

    #[test]
    fn fp8_table_matches_known_values() {
        assert_eq!(E4M3[0], 0.0);
        assert_eq!(E4M3[1], 2f32.powi(-9));
        assert_eq!(E4M3[8], 2f32.powi(-6));
        assert_eq!(E4M3[0x38], 1.0);
        assert_eq!(E4M3[0x7E], 448.0);
        assert_eq!(E4M3[0xB8], -1.0);
        assert!(E4M3[0x7F].is_nan());
        assert_eq!(E8M0[127], 1.0);
        assert_eq!(E8M0[130], 8.0);
        assert_eq!(E8M0[0], 2f32.powi(-127));
    }

    #[test]
    fn every_fp8_value_rounds_to_itself() {
        for (byte, &value) in E4M3.iter().enumerate() {
            if value.is_nan() {
                continue;
            }
            assert_eq!(round_e4m3(value).to_bits(), value.to_bits(), "byte {byte}");
        }
    }

    #[test]
    fn fp8_rounding_ties_to_even() {
        // 1.0 and 1.125 are neighbours; the even mantissa wins at the midpoint.
        assert_eq!(round_e4m3(1.0625), 1.0);
        assert_eq!(round_e4m3(1.1875), 1.25);
        assert_eq!(round_e4m3(-1.0625), -1.0);
        // Subnormal step.
        assert_eq!(round_e4m3(2f32.powi(-10)), 0.0);
        assert_eq!(round_e4m3(3.0 * 2f32.powi(-10)), 2.0 * 2f32.powi(-9));
    }

    #[test]
    fn every_fp4_value_rounds_to_itself() {
        for &value in &FP4_TABLE {
            assert_eq!(round_e2m1(value), value);
        }
        assert_eq!(round_e2m1(0.25), 0.0);
        assert_eq!(round_e2m1(0.75), 1.0);
        assert_eq!(round_e2m1(1.25), 1.0);
        assert_eq!(round_e2m1(1.75), 2.0);
        assert_eq!(round_e2m1(2.5), 2.0);
        assert_eq!(round_e2m1(3.5), 4.0);
        assert_eq!(round_e2m1(5.0), 4.0);
    }

    #[test]
    fn activation_scale_keeps_the_block_in_range() {
        let mut block = [1000.0f32; 32];
        block[1] = -3.0;
        quant_fp8(&mut block);
        // Scale 2^2: 1000 / 4 = 250, which rounds to 256 at this exponent's quantum of 16.
        assert_eq!(block[0], 1024.0);
        assert_eq!(block[1], -3.0);
    }

    #[test]
    fn fp4_pow2_scale_keeps_the_block_in_range() {
        let mut block = [0.0f32; 32];
        block[0] = 6.0;
        block[1] = 1.2;
        quant_fp4_pow2(&mut block);
        assert_eq!(block[0], 6.0);
        assert_eq!(block[1], 1.0);
    }

    #[test]
    fn fp4_e4m3_scale_uses_nearest_scale() {
        let mut block = [0.0f32; 16];
        block[0] = 12.0;
        quant_fp4_e4m3(&mut block);
        assert_eq!(block[0], 12.0);
    }

    #[test]
    fn sinkhorn_is_doubly_stochastic() {
        let mix: [f32; MIX] = std::array::from_fn(|i| (i as f32 * 0.7).sin());
        let scale = [1.0, 1.0, 1.0];
        let base = [0.1; MIX];
        let (pre, post, comb) = sinkhorn(&mix, &scale, &base);
        for j in 0..HC {
            assert!(pre[j] > 0.0 && pre[j] < 1.01);
            assert!(post[j] > 0.0 && post[j] < 2.0);
            let row: f32 = comb[j].iter().sum();
            let col: f32 = comb.iter().map(|r| r[j]).sum();
            assert!((row - 1.0).abs() < 1e-3, "row {row}");
            assert!((col - 1.0).abs() < 1e-3, "col {col}");
        }
    }

    #[test]
    fn rope_inverse_undoes_rotation() {
        let rope = Rope::new(COMPRESS_ROPE_THETA, true);
        let original: Vec<f32> = (0..HEAD_DIM).map(|i| (i as f32).cos()).collect();
        let mut rotated = original.clone();
        rope.apply(&mut rotated, 12345, false);
        assert_ne!(rotated, original);
        rope.apply(&mut rotated, 12345, true);
        for (a, b) in rotated.iter().zip(&original) {
            assert!((a - b).abs() < 1e-4);
        }
    }

    #[test]
    fn plain_rope_starts_at_frequency_one() {
        let rope = Rope::new(ROPE_THETA, false);
        assert_eq!(rope.freqs[0], 1.0);
        assert!(rope.freqs[31] < 1e-3);
    }

    #[test]
    fn yarn_slows_high_wavelengths_only() {
        let plain = Rope::new(COMPRESS_ROPE_THETA, false);
        let yarn = Rope::new(COMPRESS_ROPE_THETA, true);
        assert_eq!(plain.freqs[0], yarn.freqs[0]);
        assert!((yarn.freqs[31] - plain.freqs[31] / YARN_FACTOR).abs() < 1e-9);
    }

    #[test]
    fn sparse_attention_with_sink_shrinks_output() {
        let q = vec![0.0; HEAD_DIM];
        let kv = vec![1.0; HEAD_DIM];
        let sources: Vec<&[f32]> = vec![&kv];
        let mut with_sink = vec![0.0; HEAD_DIM];
        let mut without = vec![0.0; HEAD_DIM];
        sparse_attention(&q, &sources, 0.0, &mut with_sink);
        sparse_attention(&q, &sources, -1e6, &mut without);
        assert!(with_sink[0] < without[0]);
        assert!((without[0] - 1.0).abs() < 1e-4);
    }

    #[test]
    fn group_pooling_favours_the_higher_score_per_dimension() {
        let mut kv = vec![0.0; 2 * HEAD_DIM];
        let mut score = vec![0.0; 2 * HEAD_DIM];
        kv[HEAD_DIM] = 8.0;
        score[HEAD_DIM] = 50.0;
        kv[1] = 4.0;
        let out = pool_group(&kv, &score, 2);
        assert!((out[0] - 8.0).abs() < 1e-3);
        assert!((out[1] - 2.0).abs() < 1e-3);
    }

    #[test]
    fn top_positions_returns_position_order_and_skips_masked() {
        let scores = [1.0, 5.0, f32::NEG_INFINITY, 3.0, 4.0];
        assert_eq!(top_positions(&scores, 2), [1, 4]);
        assert_eq!(top_positions(&scores, 9), [0, 1, 3, 4]);
    }

    #[test]
    fn candidate_blocks_pin_the_newest_and_keep_the_best() {
        let blocks = CANDIDATE_BLOCKS + 2;
        let mut scores = vec![1.0; blocks * CANDIDATE_BLOCK_SIZE - 3];
        scores[0] = -5.0;
        for s in &mut scores[..CANDIDATE_BLOCK_SIZE] {
            *s = -5.0;
        }
        for s in &mut scores[CANDIDATE_BLOCK_SIZE..2 * CANDIDATE_BLOCK_SIZE] {
            *s = -4.0;
        }
        let last = scores.len() - 1;
        for s in &mut scores[(blocks - 1) * CANDIDATE_BLOCK_SIZE..] {
            *s = -9.0;
        }
        let keep = candidate_blocks(&scores);
        assert!(!keep[0], "the lowest block goes");
        assert!(keep[last], "the newest block stays despite its score");
        assert_eq!(keep.len(), scores.len());
    }

    #[test]
    fn fp4_matrix_reads_the_low_nibble_first() {
        // Row one of [1, 64]: first element 1.0 (0x2), second 2.0 (0x4), the rest zero.
        let mut weight = vec![0u8; 32];
        weight[0] = 0x42;
        let scale = vec![127u8; 2];
        let mat = Mat::Fp4 {
            weight: &weight,
            scale: &scale,
        };
        let mut x = vec![0.0; 64];
        x[0] = 3.0;
        x[1] = 5.0;
        assert_eq!(mat.row(0, 64, &x), 13.0);
    }

    #[test]
    fn fp8_matrix_shares_scales_per_32_rows() {
        let weight = vec![0x38u8; 33 * 32];
        let mut scale = vec![127u8; 2];
        scale[1] = 128;
        let mat = Mat::Fp8 {
            weight: &weight,
            scale: &scale,
        };
        let x = vec![1.0; 32];
        assert_eq!(mat.row(0, 32, &x), 32.0);
        assert_eq!(mat.row(31, 32, &x), 32.0);
        assert_eq!(mat.row(32, 32, &x), 64.0);
    }
}
