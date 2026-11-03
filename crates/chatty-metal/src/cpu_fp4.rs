//! FP4 expert matrices on the i9-9900K, for experts that live in RAM. The CPU reads RAM about three
//! times as fast as the GPU can over PCIe 3.0, and it runs while the GPU runs VRAM experts.

use crate::reference::{DIM, E8M0, MOE_DIM, quant_fp8};
use rayon::prelude::*;
use std::arch::x86_64::{
    __m128i, _mm_add_ps, _mm_add_ss, _mm_and_si128, _mm_cvtss_f32, _mm_loadu_si128, _mm_movehl_ps,
    _mm_set1_epi8, _mm_setr_epi8, _mm_shuffle_epi8, _mm_shuffle_ps, _mm_srli_epi16, _mm_srli_si128,
    _mm_unpackhi_epi8, _mm_unpacklo_epi8, _mm256_castps256_ps128, _mm256_cvtepi8_epi32,
    _mm256_cvtepi32_ps, _mm256_extractf128_ps, _mm256_fmadd_ps, _mm256_loadu_ps, _mm256_setzero_ps,
};

const BLOCK: usize = 32;
/// Output elements a rayon task computes. A cache line of results, so threads do not share lines.
const TASK: usize = 32;

/// Twice each E2M1 value, by nibble: 0, .5, 1, 1.5, 2, 3, 4, 6 and their negatives.
#[inline]
#[target_feature(enable = "avx2,fma")]
fn nibble_table() -> __m128i {
    _mm_setr_epi8(0, 1, 2, 3, 4, 6, 8, 12, 0, -1, -2, -3, -4, -6, -8, -12)
}

/// One output element: `weight` is `k / 2` bytes of nibbles, low first, and `x` is `k` activations.
///
/// # Safety
/// `weight` must hold `blocks * 16` bytes, `x` `blocks * 32` values, and `scale` `blocks` bytes.
#[target_feature(enable = "avx2,fma")]
unsafe fn row(weight: *const u8, scale: *const u8, x: *const f32, blocks: usize) -> f32 {
    let table = nibble_table();
    let mask = _mm_set1_epi8(0x0F);
    let mut total = 0.0f32;
    for b in 0..blocks {
        // SAFETY: the caller guarantees these ranges.
        unsafe {
            let bytes = _mm_loadu_si128(weight.add(b * 16).cast());
            let low = _mm_shuffle_epi8(table, _mm_and_si128(bytes, mask));
            let high = _mm_shuffle_epi8(table, _mm_and_si128(_mm_srli_epi16::<4>(bytes), mask));
            let first = _mm_unpacklo_epi8(low, high);
            let second = _mm_unpackhi_epi8(low, high);
            let xb = x.add(b * BLOCK);
            let mut acc = _mm256_setzero_ps();
            acc = _mm256_fmadd_ps(
                _mm256_cvtepi32_ps(_mm256_cvtepi8_epi32(first)),
                _mm256_loadu_ps(xb),
                acc,
            );
            acc = _mm256_fmadd_ps(
                _mm256_cvtepi32_ps(_mm256_cvtepi8_epi32(_mm_srli_si128::<8>(first))),
                _mm256_loadu_ps(xb.add(8)),
                acc,
            );
            acc = _mm256_fmadd_ps(
                _mm256_cvtepi32_ps(_mm256_cvtepi8_epi32(second)),
                _mm256_loadu_ps(xb.add(16)),
                acc,
            );
            acc = _mm256_fmadd_ps(
                _mm256_cvtepi32_ps(_mm256_cvtepi8_epi32(_mm_srli_si128::<8>(second))),
                _mm256_loadu_ps(xb.add(24)),
                acc,
            );
            let pair = _mm_add_ps(_mm256_castps256_ps128(acc), _mm256_extractf128_ps::<1>(acc));
            let folded = _mm_add_ps(pair, _mm_movehl_ps(pair, pair));
            let sum = _mm_cvtss_f32(_mm_add_ss(folded, _mm_shuffle_ps::<1>(folded, folded)));
            total += sum * E8M0[*scale.add(b) as usize];
        }
    }
    total * 0.5
}

/// `out[m, n] = x[m, k] @ weight[n, k]^T`, with `weight` as `[n, k / 2]` nibble bytes and `scale` as
/// `[n, k / 32]` E8M0 bytes. Activations must already be quantized and rescaled.
pub fn apply(weight: &[u8], scale: &[u8], x: &[f32], k: usize, out: &mut [f32], n: usize) {
    assert!(is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma"));
    let blocks = k / BLOCK;
    assert_eq!(weight.len(), n * k / 2);
    assert_eq!(scale.len(), n * blocks);
    assert_eq!(x.len() / k, out.len() / n);
    out.par_chunks_mut(TASK)
        .enumerate()
        .for_each(|(task, chunk)| {
            for (offset, value) in chunk.iter_mut().enumerate() {
                let index = task * TASK + offset;
                let (m, col) = (index / n, index % n);
                // SAFETY: AVX2 and FMA were checked above, and the asserts bound every range:
                // `col < n` and `m` is a row of `x`.
                *value = unsafe {
                    row(
                        weight.as_ptr().add(col * k / 2),
                        scale.as_ptr().add(col * blocks),
                        x.as_ptr().add(m * k),
                        blocks,
                    )
                };
            }
        });
}

/// One expert over `rows` quantized activations: w1 and w3, SwiGLU, route weight, then w2.
pub fn expert(x: &[f32], tensors: &[&[u8]; 6], weights: &[f32]) -> Vec<f32> {
    let rows = weights.len();
    let mut gate = vec![0.0; rows * MOE_DIM];
    let mut up = vec![0.0; rows * MOE_DIM];
    apply(tensors[0], tensors[1], x, DIM, &mut gate, MOE_DIM);
    apply(tensors[4], tensors[5], x, DIM, &mut up, MOE_DIM);
    let mut mid = vec![0.0; rows * MOE_DIM];
    mid.par_chunks_mut(MOE_DIM)
        .zip(gate.par_chunks(MOE_DIM))
        .zip(up.par_chunks(MOE_DIM))
        .zip(weights.par_iter())
        .for_each(|(((m, g), u), w)| {
            for ((out, g), u) in m.iter_mut().zip(g).zip(u) {
                let g = g.min(10.0);
                let u = u.clamp(-10.0, 10.0);
                *out = g / (1.0 + (-g).exp()) * u * w;
            }
            quant_fp8(m);
        });
    let mut out = vec![0.0; rows * DIM];
    apply(tensors[2], tensors[3], &mid, MOE_DIM, &mut out, DIM);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reference::Mat;

    #[test]
    fn matches_the_reference_matrix() {
        let (n, k) = (70, 128);
        let weight: Vec<u8> = (0..n * k / 2).map(|i| (i * 37 % 251) as u8).collect();
        let scale: Vec<u8> = (0..n * k / 32).map(|i| 120 + (i % 9) as u8).collect();
        let x: Vec<f32> = (0..3 * k).map(|i| (i as f32 * 0.37).sin() * 3.0).collect();
        let mut got = vec![0.0; 3 * n];
        apply(&weight, &scale, &x, k, &mut got, n);
        let mut want = vec![0.0; 3 * n];
        Mat::Fp4 {
            weight: &weight,
            scale: &scale,
        }
        .apply(&x, k, &mut want, n);
        for (a, b) in got.iter().zip(&want) {
            assert!((a - b).abs() <= 1e-4 * b.abs().max(1.0), "{a} vs {b}");
        }
    }
}
