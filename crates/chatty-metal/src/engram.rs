//! Engram n-gram hashing: which table rows each token position looks up.

#![allow(clippy::chunks_exact_to_as_chunks)]

use crate::{Result, invalid};

pub const MAX_NGRAM: usize = 4;
pub const NGRAM_HEADS: usize = 8;
/// Rows fetched per position: one per (n-gram size 2..=4, head).
pub const HASH_COLS: usize = (MAX_NGRAM - 1) * NGRAM_HEADS;
const FIRST_PRIME_AFTER: u64 = 15_999_999;
const COMPRESSED_VOCAB: u32 = 99_092;
const PAD_TOKEN: usize = 2;
/// Hash multipliers per engram layer and look-back. The reference derives them from
/// `numpy.random.default_rng(10007 * layer)` and the compressed vocabulary size. Both are fixed
/// for this model, so these are that generator's values for layers 1 and 14.
const MULTIPLIERS: [(usize, [u64; MAX_NGRAM]); 2] = [
    (
        1,
        [
            76_632_096_046_245,
            4_839_876_093_313,
            35_959_672_319_349,
            73_987_337_458_391,
        ],
    ),
    (
        14,
        [
            67_716_810_739_261,
            51_510_806_800_915,
            30_921_347_202_721,
            82_619_226_485_591,
        ],
    ),
];

struct LayerHash {
    multipliers: [u64; MAX_NGRAM],
    primes: [u64; HASH_COLS],
    offsets: [u64; HASH_COLS],
}

pub struct EngramHasher {
    map: Vec<u32>,
    pad: u64,
    layers: Vec<LayerHash>,
    history: Vec<u32>,
}

impl EngramHasher {
    /// Fails if the tokenizer or table sizes differ from what the checkpoint trained with, since
    /// any mismatch would silently rehash every table.
    pub fn new(map: Vec<u32>, vocab: u32, layer_ids: &[usize], rows: &[u64]) -> Result<Self> {
        if vocab != COMPRESSED_VOCAB {
            return invalid(format!(
                "compressed vocabulary is {vocab}, the model trained with {COMPRESSED_VOCAB}"
            ));
        }
        if layer_ids.len() != rows.len() {
            return invalid("engram layers and rows differ in length");
        }
        let Some(&pad) = map.get(PAD_TOKEN) else {
            return invalid("token map has no pad token");
        };
        let mut candidate = FIRST_PRIME_AFTER;
        let mut layers = Vec::with_capacity(layer_ids.len());
        for (&layer, &table_rows) in layer_ids.iter().zip(rows) {
            let Some(&(_, multipliers)) = MULTIPLIERS.iter().find(|(id, _)| *id == layer) else {
                return invalid(format!("no hash multipliers for engram layer {layer}"));
            };
            let mut primes = [0; HASH_COLS];
            let mut offsets = [0; HASH_COLS];
            let mut sum = 0;
            for (prime, offset) in primes.iter_mut().zip(&mut offsets) {
                candidate = next_prime(candidate);
                *prime = candidate;
                *offset = sum;
                sum += candidate;
            }
            if sum != table_rows {
                return invalid(format!(
                    "engram layer {layer} has {table_rows} rows, its primes sum to {sum}"
                ));
            }
            layers.push(LayerHash {
                multipliers,
                primes,
                offsets,
            });
        }
        Ok(Self {
            map,
            pad: u64::from(pad),
            layers,
            history: Vec::new(),
        })
    }

    /// Append tokens and return their rows, shaped `[tokens, layers, HASH_COLS]`.
    pub fn push(&mut self, ids: &[u32]) -> Vec<u64> {
        let mut out = Vec::with_capacity(ids.len() * self.layers.len() * HASH_COLS);
        for &id in ids {
            self.history.push(
                self.map
                    .get(id as usize)
                    .copied()
                    .unwrap_or(self.pad as u32),
            );
            let position = self.history.len() - 1;
            for layer in &self.layers {
                let mut rolling = 0;
                let mut column = 0;
                for shift in 0..MAX_NGRAM {
                    let token = if position >= shift {
                        u64::from(self.history[position - shift])
                    } else {
                        self.pad
                    };
                    rolling ^= token * layer.multipliers[shift];
                    if shift == 0 {
                        continue;
                    }
                    for _ in 0..NGRAM_HEADS {
                        out.push(rolling % layer.primes[column] + layer.offsets[column]);
                        column += 1;
                    }
                }
            }
        }
        out
    }

    pub fn len(&self) -> usize {
        self.history.len()
    }

    pub fn is_empty(&self) -> bool {
        self.history.is_empty()
    }

    pub fn reset(&mut self) {
        self.history.clear();
    }
}

fn next_prime(after: u64) -> u64 {
    (after + 1..)
        .find(|&n| {
            n % 2 != 0
                && (3..)
                    .step_by(2)
                    .take_while(|d| d * d <= n)
                    .all(|d| n % d != 0)
        })
        .unwrap_or(after)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAYERS: [usize; 2] = [1, 14];
    const ROWS: [u64; 2] = [384_006_168, 384_016_682];

    fn hasher() -> EngramHasher {
        let map = (0..100).collect();
        EngramHasher::new(map, COMPRESSED_VOCAB, &LAYERS, &ROWS).unwrap()
    }

    #[test]
    fn primes_sum_to_the_checkpoint_table_sizes() {
        hasher();
    }

    #[test]
    fn rejects_a_mismatched_vocabulary() {
        assert!(EngramHasher::new(vec![0; 3], 5, &LAYERS, &ROWS).is_err());
    }

    #[test]
    fn rows_stay_in_each_columns_range() {
        let mut h = hasher();
        let rows = h.push(&[5, 6, 7, 8, 9]);
        assert_eq!(rows.len(), 5 * 2 * HASH_COLS);
        for token in rows.chunks_exact(2 * HASH_COLS) {
            for (layer, (cols, table)) in token.chunks_exact(HASH_COLS).zip(&h.layers).enumerate() {
                for (col, &row) in cols.iter().enumerate() {
                    assert!(row >= table.offsets[col], "layer {layer} col {col}");
                    assert!(row < table.offsets[col] + table.primes[col]);
                }
            }
        }
    }

    #[test]
    fn one_call_and_many_calls_hash_alike() {
        let mut a = hasher();
        let mut b = hasher();
        let all = a.push(&[5, 6, 7, 8]);
        let mut parts = b.push(&[5, 6]);
        parts.extend(b.push(&[7, 8]));
        assert_eq!(all, parts);
    }

    #[test]
    fn history_before_the_start_hashes_as_pad() {
        let mut h = hasher();
        let first = h.push(&[5]);
        let table = &h.layers[0];
        let two_gram = (5 * table.multipliers[0]) ^ (2 * table.multipliers[1]);
        assert_eq!(first[0], two_gram % table.primes[0] + table.offsets[0]);
    }
}
