//! Fixed memory layout for the two caches in front of the model drive.

use crate::{Result, catalog::Catalog, invalid};

pub const GIB: u64 = 1 << 30;
/// `max_position_embeddings` of the model.
pub const MAX_CONTEXT: u64 = 1 << 20;
/// Upper bound per KV element; the reference engine's caches are at most bf16.
const KV_ELEMENT_BYTES: u64 = 2;
/// Per-position int64 cache of compressed token ids for n-gram hashing.
const TOKEN_CACHE_BYTES: u64 = 8;
/// One complex64 per rope frequency, per position, shared by the layers.
const ROPE_ELEMENT_BYTES: u64 = 8;

#[derive(Clone, Copy, Debug)]
pub struct Budget {
    pub vram: u64,
    pub ram: u64,
    /// Activations and command buffers.
    pub vram_reserved: u64,
    /// Kernel and this process.
    pub ram_reserved: u64,
    pub engram_ram: u64,
    pub context: u64,
}

impl Default for Budget {
    /// Measured on the target host at full context: its RX 7900 XTX reports
    /// 25,753,026,560 bytes of VRAM and Linux sees 62 GiB of its 64 GB of RAM. The
    /// engram share is a starting point until routing and n-gram traces can tune it.
    fn default() -> Self {
        Self {
            vram: 25_753_026_560,
            ram: 62 * GIB,
            vram_reserved: GIB,
            ram_reserved: 4 * GIB,
            engram_ram: 12 * GIB,
            context: MAX_CONTEXT,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Plan {
    pub core: u64,
    pub kv: u64,
    pub vram_expert_slots: u64,
    pub ram_expert_slots: u64,
    pub engram_ram: u64,
    pub expert_total: u64,
    pub engram_total: u64,
    pub drive_total: u64,
}

impl Plan {
    pub fn new(catalog: &Catalog, budget: Budget) -> Result<Self> {
        if budget.context == 0 || budget.context > MAX_CONTEXT {
            return invalid(format!("context must be 1..={MAX_CONTEXT}"));
        }
        if catalog.expert_slot == 0 {
            return invalid("model has no routed experts");
        }
        let sizes = catalog.sizes;
        let kv = kv_bytes(catalog, budget.context)?;
        let needed = sizes.core + kv + budget.vram_reserved;
        let Some(vram_free) = budget.vram.checked_sub(needed) else {
            return invalid(format!(
                "needs {needed} bytes of VRAM, have {}",
                budget.vram
            ));
        };
        let needed = budget.ram_reserved + budget.engram_ram;
        let Some(ram_free) = budget.ram.checked_sub(needed) else {
            return invalid(format!("needs {needed} bytes of RAM, have {}", budget.ram));
        };

        // The tiers are exclusive: an expert promoted to VRAM leaves its RAM slot.
        let expert_total = catalog.expert_count() as u64;
        let vram_expert_slots = (vram_free / catalog.expert_slot).min(expert_total);
        let ram_expert_slots =
            (ram_free / catalog.expert_slot).min(expert_total - vram_expert_slots);
        Ok(Self {
            core: sizes.core,
            kv,
            vram_expert_slots,
            ram_expert_slots,
            engram_ram: budget.engram_ram.min(sizes.engram),
            expert_total,
            engram_total: sizes.engram,
            drive_total: sizes.core + sizes.experts + sizes.engram + sizes.mtp + sizes.vision,
        })
    }
}

/// Full-context cache memory, following the buffers the reference engine allocates.
fn kv_bytes(catalog: &Catalog, context: u64) -> Result<u64> {
    let config = &catalog.config;
    let mut total =
        context * TOKEN_CACHE_BYTES + context * (config.rope_head_dim / 2) * ROPE_ELEMENT_BYTES;
    for &layer in &config.kv_source_layers {
        let ratio = config.compress_ratios.get(layer).copied().unwrap_or(0);
        if ratio == 0 {
            return invalid(format!("KV source layer {layer} has no compression ratio"));
        }
        total += context / ratio * (config.head_dim + config.index_head_dim) * KV_ELEMENT_BYTES;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::tests::fixture;

    fn budget() -> Budget {
        Budget {
            vram: 16 * 1024,
            ram: 16 * 1024,
            vram_reserved: 0,
            ram_reserved: 0,
            engram_ram: 0,
            context: 16,
        }
    }

    #[test]
    fn sizes_kv_from_source_layers() {
        let fixture = fixture(|_| {});
        let catalog = Catalog::open(&fixture.0).unwrap();
        let plan = Plan::new(&catalog, budget()).unwrap();
        // tokens 16*8 + rope 16*2*8 + layer 1 at ratio 2: 8 * (8 + 4) * 2.
        assert_eq!(plan.kv, 128 + 256 + 192);
        assert_eq!(plan.core, 16);
    }

    #[test]
    fn caps_tiers_at_expert_count_without_overlap() {
        let fixture = fixture(|_| {});
        let catalog = Catalog::open(&fixture.0).unwrap();
        let plan = Plan::new(&catalog, budget()).unwrap();
        assert_eq!(plan.vram_expert_slots, 4);
        assert_eq!(plan.ram_expert_slots, 0);
    }

    #[test]
    fn spills_experts_to_ram_when_vram_runs_out() {
        let fixture = fixture(|_| {});
        let catalog = Catalog::open(&fixture.0).unwrap();
        // Core 16 + KV 576 leaves room for exactly one 24-byte slot.
        let plan = Plan::new(
            &catalog,
            Budget {
                vram: 16 + 576 + 24,
                ram: 48,
                ..budget()
            },
        )
        .unwrap();
        assert_eq!((plan.vram_expert_slots, plan.ram_expert_slots), (1, 2));
    }

    #[test]
    fn rejects_budget_smaller_than_core() {
        let fixture = fixture(|_| {});
        let catalog = Catalog::open(&fixture.0).unwrap();
        let error = Plan::new(
            &catalog,
            Budget {
                vram: 16,
                ..budget()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("bytes of VRAM"));
        let error = Plan::new(
            &catalog,
            Budget {
                engram_ram: 1 << 20,
                ..budget()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("bytes of RAM"));
    }

    #[test]
    fn rejects_context_above_model_limit() {
        let fixture = fixture(|_| {});
        let catalog = Catalog::open(&fixture.0).unwrap();
        assert!(
            Plan::new(
                &catalog,
                Budget {
                    context: MAX_CONTEXT + 1,
                    ..budget()
                }
            )
            .is_err()
        );
        assert!(
            Plan::new(
                &catalog,
                Budget {
                    context: 0,
                    ..budget()
                }
            )
            .is_err()
        );
    }
}
