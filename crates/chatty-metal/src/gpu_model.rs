//! The DeepSeek-V4.1-Flash forward pass on the GPU, through KFD. It mirrors `reference.rs`, which
//! the job binaries compare it against.
//!
//! Only 6 of 384 experts run per layer per token, which shapes this file. The gate runs first and
//! its result comes back to the CPU, so only the chosen experts are fetched.
//!
//! Experts are independent, so VRAM experts overlap to fill the GPU, and RAM experts run on the CPU
//! at the same time: the same 14 matrices take 205 us overlapped in VRAM, 2.9 ms on the CPU from RAM,
//! and 7.1 ms on the GPU from RAM over PCIe 3.0. A policy may skip experts that are not resident,
//! which is faster but no longer matches the checkpoint's routing.

#![allow(clippy::chunks_exact_to_as_chunks)]

use crate::{
    Result,
    catalog::{Catalog, Dtype, EXPERT_EXTENTS},
    cpu_fp4,
    engram::{EngramHasher, HASH_COLS},
    invalid,
    kfd::{Buffer, Dispatch, Gpu, Memory, Module, Queue, SIGNAL_BYTES, signal_reset, signal_wait},
    plan::{Budget, Plan},
    reference::{
        ACTIVE, CANDIDATE_SOURCE, DIM, E4M3, E8M0, Fp4Format, GROUPS, HC, HEAD_DIM, HEADS,
        INDEX_DIM, INDEX_HEADS, INDEX_SOURCES, INDEX_TOPK, KV_SOURCES, LAYERS, MOE_DIM, O_LORA,
        PROBE_LAYERS, Probes, Q_LORA, ROUTE_SCALE, ROUTED, Rope, WINDOW, candidate_blocks,
        encode_e4m3, engram_values, quant_fp8, requantize_row, round_e4m3, route, top_positions,
    },
    residency::{Residency, Tier},
    routing::RouteTrace,
    store::{CoreStore, DEFAULT_IO_WORKERS, EngramCache, Reader},
    tokenizer::Tokenizer,
};
use rayon::prelude::*;
use std::{
    collections::BTreeMap,
    mem::size_of,
    ptr,
    sync::Arc,
    time::{Duration, Instant},
};

const MODEL: &[u8] = include_bytes!("../shaders/model.hsaco");
const HOT_EXPERTS: &str = include_str!("hot_experts.txt");
/// Largest chunk `forward` takes. Bounds the scratch buffers.
pub const MAX_CHUNK: usize = 64;
/// Context the KV stores hold for now, while they keep f32 rows rather than packed FP4.
pub const CONTEXT: usize = 1 << 16;
const WORKGROUP: u16 = 32;
/// Factor `quant_fp8_gemm` puts on activations. See `e4m3_bits` in the shader.
pub const GEMM_PRESCALE: f32 = f32::from_bits((96 + 127) << 23);
/// Ordering and cache maintenance live on barrier packets, so a dispatch starting beside running
/// waves does not invalidate their caches.
const FENCE_NONE: u32 = 0;
const RING: usize = WINDOW + MAX_CHUNK;
const PAIR_RING: usize = MAX_CHUNK + 2;
/// The queue ring holds 1024 packets; a sync drains it well before that.
const MAX_PENDING: usize = 900;
const KERNARG_BYTES: u64 = 1 << 20;
const HOST_BYTES: u64 = 32 << 20;
const SCORE_BYTES: u64 = 32 << 20;
const TIMEOUT: Duration = Duration::from_secs(60);
const MIX: usize = 24;
/// Fragments of the 20,480-element stream in the mix projection, each one wave per row.
const MIX_FRAGS: usize = 80;
/// Elements per wave when summing squares for a norm.
const NORM_FRAGMENT: usize = 256;
const MAX_NORM_FRAGS: usize = DIM / NORM_FRAGMENT;
/// RAM for experts. Most of what the 62 GiB leaves after engram rows and this process.
const RAM_EXPERT_BYTES: u64 = 44 << 30;
/// VRAM kept out of the expert arena for KV stores, scratch, and code.
const VRAM_RESERVE: u64 = 3 << 29;
const MAX_SOURCES: usize = WINDOW + INDEX_TOPK;
/// Per-token working memory of one expert: gate, up, middle, and output.
const EXPERT_FLOATS: usize = MOE_DIM * 3 + DIM + DIM;

macro_rules! kernels {
    ($($name:ident),* $(,)?) => {
        struct Kernels { $($name: u64),* }
        impl Kernels {
            fn name(&self, kernel: u64) -> &'static str {
                $(if kernel == self.$name { return stringify!($name); })*
                "unknown"
            }

            fn load(module: &Module) -> Result<Self> {
                Ok(Self { $($name: module.kernel(stringify!($name))?),* })
            }
        }
    };
}

kernels!(
    gemm_fp8,
    swiglu_quant,
    sum_row,
    norm_partial,
    norm_apply,
    mix_partial,
    quant_fp8_into,
    quant_fp8_gemm,
    gemm_fp4,
    gemm_bf16,
    gemm_f32,
    quant_fp8,
    quant_fp4_pow2,
    quant_fp4_e4m3,
    hc_sinkhorn,
    hc_pre,
    hc_post,
    rope,
    put_rows,
    take_rows,
    swiglu,
    embed_rows,
    engram_gate,
    pool_pairs,
    attn_scores,
    attn_weights,
    attn_output,
    index_scores,
    scale_values,
);

#[repr(C)]
#[derive(Clone, Copy)]
struct GemmArgs {
    out: u64,
    x: u64,
    weight: u64,
    scale: u64,
    n: u32,
    k: u32,
    x_stride: u32,
    out_stride: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct QuantArgs {
    buf: u64,
    stride: u32,
    pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct NormArgs {
    buf: u64,
    weight: u64,
    partial: u64,
    dim: u32,
    stride: u32,
    frags: u32,
    pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct PartialArgs {
    out: u64,
    x: u64,
    weight: u64,
    frags: u32,
    pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct QuantIntoArgs {
    src: u64,
    dst: u64,
    stride: u32,
    pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MixArgs {
    mix: u64,
    frags: u64,
    scale: u64,
    base: u64,
    out: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct PreArgs {
    out: u64,
    h: u64,
    mixes: u64,
    mix_stride: u32,
    mix_offset: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct PostArgs {
    out: u64,
    x: u64,
    residual: u64,
    mixes: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RopeArgs {
    buf: u64,
    table: u64,
    stride: u32,
    head_dim: u32,
    sign: f32,
    pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CopyArgs {
    dst: u64,
    src: u64,
    table: u64,
    dim: u32,
    add: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SwigluArgs {
    out: u64,
    gate: u64,
    up: u64,
    weights: u64,
    dim: u32,
    pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SumArgs {
    out: u64,
    sources: u64,
    count: u32,
    pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct EmbedArgs {
    out: u64,
    weight: u64,
    ids: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct EngramArgs {
    h: u64,
    kv: u64,
    q_weight: u64,
    k_weight: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct PoolArgs {
    out: u64,
    kv: u64,
    score: u64,
    table: u64,
    ring: u32,
    pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AttnArgs {
    out: u64,
    q: u64,
    sources: u64,
    scores: u64,
    sink: u64,
    count: u32,
    pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct IndexArgs {
    out: u64,
    q: u64,
    keys: u64,
    weights: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ScaleArgs {
    buf: u64,
    factor: f32,
    pad: u32,
}

/// Whether a packet waits for everything before it, or may overlap the packet before it.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Order {
    After,
    Overlap,
}

/// Packets and their kernel arguments, up to the next CPU sync.
struct Commands {
    queue: Queue,
    kernarg: Buffer,
    kernarg_used: u64,
    signal: u64,
    pending: usize,
    wait: Duration,
    dispatches: u64,
    /// When set, every dispatch runs alone and its time is charged to its kernel address.
    profile: Option<BTreeMap<u64, (u64, Duration)>>,
    /// Never overlap dispatches. See the module documentation.
    serial: bool,
}

impl Commands {
    fn go<T: Copy>(
        &mut self,
        kernel: u64,
        args: T,
        grid: usize,
        rows: usize,
        order: Order,
    ) -> Result<()> {
        if self.pending >= MAX_PENDING || self.kernarg_used + 4096 > KERNARG_BYTES {
            self.sync()?;
        }
        let kernarg = self.kernarg.va + self.kernarg_used;
        const { assert!(size_of::<T>() <= 64) };
        // SAFETY: the kernel arguments stay inside the mapped buffer, and nothing reads this
        // slot until the packet below publishes it.
        unsafe {
            ptr::with_exposed_provenance_mut::<T>(kernarg as usize).write_volatile(args);
        }
        self.kernarg_used += 64;
        let packet = Dispatch {
            kernel,
            kernarg,
            grid: grid as u32,
            rows: rows as u32,
            workgroup: WORKGROUP,
            signal: 0,
        }
        .encode_with_fence(FENCE_NONE, false);
        if order == Order::After || self.serial {
            self.queue.submit_order_barrier();
            self.pending += 1;
        }
        self.queue.submit_packet(&packet);
        self.pending += 1;
        self.dispatches += 1;
        if self.profile.is_some() {
            let start = Instant::now();
            self.sync()?;
            if let Some(profile) = &mut self.profile {
                let entry = profile.entry(kernel).or_default();
                entry.0 += 1;
                entry.1 += start.elapsed();
            }
        }
        Ok(())
    }

    /// Wait for everything submitted, with its results visible to the CPU.
    fn sync(&mut self) -> Result<()> {
        // SAFETY: the signal lies in a mapped page this object owns, and nothing is using it.
        unsafe { signal_reset(self.signal, 1) };
        self.queue.submit_barrier(self.signal);
        let start = Instant::now();
        // SAFETY: as above.
        let done = unsafe { signal_wait(self.signal, TIMEOUT) };
        self.wait += start.elapsed();
        if !done {
            let (read, write) = self.queue.counters();
            return invalid(format!("GPU stuck: read {read}, write {write}"));
        }
        self.pending = 0;
        self.kernarg_used = 0;
        Ok(())
    }
}

/// A bump allocator over a CPU-visible buffer holding per-layer tables.
struct HostArena {
    buffer: Buffer,
    used: u64,
}

impl HostArena {
    fn put<T: Copy>(&mut self, values: &[T]) -> Result<u64> {
        let bytes = std::mem::size_of_val(values) as u64;
        let at = self.used.next_multiple_of(16);
        if at + bytes > self.buffer.size {
            return invalid("host table arena is full");
        }
        self.used = at + bytes;
        let va = self.buffer.va + at;
        // SAFETY: the range is inside the mapped buffer and no GPU work reads it yet.
        unsafe {
            ptr::copy_nonoverlapping(
                values.as_ptr(),
                ptr::with_exposed_provenance_mut::<T>(va as usize),
                values.len(),
            );
        }
        Ok(va)
    }
}

/// Rows-of-f32 scratch memory in VRAM.
struct Scratch {
    h: [Buffer; 2],
    mix_raw: Buffer,
    mix_attn: Buffer,
    mix_ffn: [Buffer; 2],
    x: Buffer,
    xq: Buffer,
    qr: Buffer,
    qrq: Buffer,
    q: Buffer,
    kv_new: Buffer,
    scores: Buffer,
    o: Buffer,
    flat: Buffer,
    out: Buffer,
    norm_partial: Buffer,
    index_q: Buffer,
    index_w: Buffer,
    latent: Buffer,
    keys: Buffer,
    pair_kv: Buffer,
    pair_score: Buffer,
    engram_x: Buffer,
    engram_kv: Buffer,
    experts: Buffer,
}

struct SourceStore {
    kv: Buffer,
    keys: Buffer,
    pair_kv: Buffer,
    pair_score: Buffer,
    count: usize,
}

struct Tensors {
    id: usize,
    ratio: usize,
    rope: Rope,
    ring: Buffer,
    gate_bias: Vec<f32>,
    va: BTreeMap<String, u64>,
}

impl Tensors {
    fn va(&self, suffix: &str) -> Result<u64> {
        self.va
            .get(suffix)
            .copied()
            .ok_or_else(|| crate::Error::Invalid(format!("layer {} has no {suffix}", self.id)))
    }
}

/// What happens to a chosen expert that cannot run right now.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Missing {
    /// Wait for it. Output follows the checkpoint's routing.
    Exact,
    /// Run the next-best resident expert instead.
    StandIn,
    /// Skip it and share its route weight among the others.
    DropRenormalized,
    /// Skip it and leave the others' route weights as they were.
    DropKeepScale,
}

/// How routing trades fidelity for speed.
#[derive(Clone, Copy, Debug)]
pub struct Policy {
    pub missing: Missing,
    /// RAM runs a token may spend, each about 0.6 ms of the CPU.
    pub host_budget: usize,
    /// This many top-ranked experts always run, waiting if they must.
    pub must_run: usize,
    /// Experts ranked below this run only if they are in VRAM.
    pub ram_ranks: usize,
}

impl Policy {
    pub const EXACT: Self = Self {
        missing: Missing::Exact,
        host_budget: usize::MAX,
        must_run: ACTIVE,
        ram_ranks: ACTIVE,
    };
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Timing {
    pub experts: Duration,
    pub cpu_experts: Duration,
    /// The part of `cpu_experts` spent waiting for the drive.
    pub fetch_wait: Duration,
    pub engram: Duration,
    pub gpu_wait: Duration,
    pub dispatches: u64,
    pub vram_runs: u64,
    pub ram_runs: u64,
    /// Chosen experts replaced by a resident stand-in.
    pub substituted: u64,
    pub fetched: u64,
    pub promoted: u64,
    pub evicted_vram: u64,
    pub blocking_loads: u64,
}

pub struct GpuModel {
    catalog: Arc<Catalog>,
    gpu: Gpu,
    commands: Commands,
    host: HostArena,
    score_host: Buffer,
    logits: Buffer,
    gate_out: Buffer,
    x_host: Buffer,
    cpu_out: Buffer,
    k: Kernels,
    _module: Module,
    reader: Reader,
    core: CoreStore,
    experts: Residency,
    policy: Policy,
    host_left: Vec<usize>,
    engram: EngramCache,
    hasher: EngramHasher,
    layers: Vec<Tensors>,
    stores: Vec<SourceStore>,
    scratch: Scratch,
    identity: u64,
    current_store: usize,
    topk: Vec<Vec<u32>>,
    candidates: Vec<Vec<bool>>,
    position: usize,
    timing: Timing,
    pub trace: RouteTrace,
    pub fingerprints: Option<Vec<(usize, f32, f32)>>,
    /// When set, values after each sub-step of the first layers, for CPU comparison.
    pub probes: Probes,
}

impl GpuModel {
    pub fn load(catalog: Arc<Catalog>, tokenizer: &Tokenizer) -> Result<Self> {
        let config = &catalog.config;
        if config.n_layers != LAYERS
            || config.n_routed_experts != ROUTED
            || config.n_activated_experts != ACTIVE
        {
            return invalid("checkpoint shape differs from what this engine implements");
        }
        let reader = Reader::open(&catalog.shards, DEFAULT_IO_WORKERS)?;
        let mut gpu = Gpu::open()?;
        let queue = gpu.create_queue()?;
        let module = gpu.load_module(MODEL)?;
        let k = Kernels::load(&module)?;
        let core = CoreStore::load(&catalog, &reader, &mut gpu)?;
        let plan = Plan::new(&catalog, Budget::default())?;
        let stride = catalog.expert_slot.next_multiple_of(4096);
        let vram_slots = (Budget::default().vram - catalog.sizes.core - VRAM_RESERVE) / stride;
        let experts = Residency::new(
            catalog.clone(),
            &mut gpu,
            vram_slots as usize,
            (RAM_EXPERT_BYTES / stride) as usize,
        )?;
        let engram = EngramCache::new(&catalog, &plan)?;
        let (map, vocab) = tokenizer.compressed_map();
        let hasher = EngramHasher::new(
            map,
            vocab,
            &config.engram_layer_ids,
            &config.engram_num_embeddings,
        )?;

        let kernarg = gpu.alloc(KERNARG_BYTES, Memory::Host, false)?;
        let signal_page = gpu.alloc(4096, Memory::Host, false)?;
        const { assert!(SIGNAL_BYTES <= 4096) };
        let host = gpu.alloc(HOST_BYTES, Memory::Host, false)?;
        let score_host = gpu.alloc(SCORE_BYTES, Memory::Host, false)?;
        let logits = gpu.alloc((tokenizer.len() as u64 + 64) * 4, Memory::Host, false)?;
        let gate_out = gpu.alloc((MAX_CHUNK * ROUTED * 4) as u64, Memory::Host, false)?;
        let x_host = gpu.alloc((MAX_CHUNK * DIM * 4) as u64, Memory::Host, false)?;
        let cpu_out = gpu.alloc((MAX_CHUNK * DIM * 4) as u64, Memory::Host, false)?;
        let mut arena = HostArena {
            buffer: host,
            used: 0,
        };
        let identity_rows: Vec<u32> = (0..MAX_CHUNK as u32).collect();
        let identity = arena.put(&identity_rows)?;
        let persistent = arena.used;

        let scratch = alloc_scratch(&mut gpu)?;
        let mut layers = Vec::with_capacity(LAYERS);
        for id in 0..LAYERS {
            let prefix = format!("layers.{id}.");
            let mut va = BTreeMap::new();
            for (name, tensor) in core.iter() {
                if let Some(suffix) = name.strip_prefix(&prefix) {
                    va.insert(suffix.to_owned(), tensor.va);
                }
            }
            let bias_extent = catalog.core[&format!("{prefix}ffn.gate.bias")].extent;
            let mut bytes = vec![0u8; bias_extent.len as usize];
            reader.read_extent(bias_extent, &mut bytes)?;
            let gate_bias = bytes
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect();
            let ratio = config.compress_ratios.get(id).copied().unwrap_or(0) as usize;
            layers.push(Tensors {
                id,
                ratio,
                rope: Rope::for_ratio(ratio),
                ring: gpu.alloc((RING * HEAD_DIM * 4) as u64, Memory::VramVisible, false)?,
                gate_bias,
                va,
            });
        }
        let mut stores = Vec::with_capacity(KV_SOURCES.len());
        for &source in &KV_SOURCES {
            let ratio = config
                .compress_ratios
                .get(source)
                .copied()
                .unwrap_or(1)
                .max(1) as usize;
            let rows = (CONTEXT / ratio) as u64;
            stores.push(SourceStore {
                kv: gpu.alloc(rows * (HEAD_DIM * 4) as u64, Memory::VramVisible, false)?,
                keys: gpu.alloc(rows * (INDEX_DIM * 4) as u64, Memory::VramVisible, false)?,
                pair_kv: gpu.alloc(
                    (PAIR_RING * HEAD_DIM * 4) as u64,
                    Memory::VramVisible,
                    false,
                )?,
                pair_score: gpu.alloc(
                    (PAIR_RING * HEAD_DIM * 4) as u64,
                    Memory::VramVisible,
                    false,
                )?,
                count: 0,
            });
        }
        gpu.flush_cpu_writes();
        arena.used = persistent;
        Ok(Self {
            commands: Commands {
                queue,
                kernarg,
                kernarg_used: 0,
                signal: signal_page.va,
                pending: 0,
                wait: Duration::ZERO,
                dispatches: 0,
                profile: None,
                serial: false,
            },
            catalog,
            gpu,
            host: arena,
            score_host,
            logits,
            gate_out,
            x_host,
            cpu_out,
            k,
            _module: module,
            reader,
            core,
            experts,
            policy: Policy::EXACT,
            host_left: Vec::new(),
            engram,
            hasher,
            layers,
            stores,
            scratch,
            identity,
            current_store: 0,
            topk: Vec::new(),
            candidates: Vec::new(),
            position: 0,
            timing: Timing::default(),
            trace: RouteTrace::default(),
            fingerprints: None,
            probes: None,
        })
    }

    pub fn position(&self) -> usize {
        self.position
    }

    pub fn timing(&self) -> Timing {
        Timing {
            gpu_wait: self.commands.wait,
            dispatches: self.commands.dispatches,
            fetched: self.experts.counters.fetched,
            promoted: self.experts.counters.promoted,
            evicted_vram: self.experts.counters.evicted_vram,
            blocking_loads: self.experts.counters.blocking_loads,
            ..self.timing
        }
    }

    /// Quality test: reload every FP8 matrix from the drive, round it to `format`, and write it to
    /// VRAM as the nearest FP8 values, so the existing kernels run it. `None` restores FP8. Memory
    /// use does not change; this only measures what FP4 weights would cost in output quality.
    pub fn set_core_fp4(&mut self, format: Option<Fp4Format>) -> Result<()> {
        self.commands.sync()?;
        let names: Vec<String> = self
            .catalog
            .core
            .iter()
            .filter(|(name, tensor)| tensor.dtype == Dtype::F8E4m3 && name.ends_with(".weight"))
            .map(|(name, _)| name.clone())
            .collect();
        for name in names {
            let tensor = &self.catalog.core[&name];
            let scale_name = format!("{}.scale", name.trim_end_matches(".weight"));
            let scale_tensor = &self.catalog.core[&scale_name];
            let k = tensor.shape[1] as usize;
            let blocks = k / 32;
            let mut weight = vec![0u8; tensor.extent.len as usize];
            let mut scale = vec![0u8; scale_tensor.extent.len as usize];
            self.reader.read_extent(tensor.extent, &mut weight)?;
            self.reader.read_extent(scale_tensor.extent, &mut scale)?;
            if let Some(format) = format {
                let peak = weight
                    .par_chunks(k)
                    .enumerate()
                    .map(|(row, bytes)| {
                        let scales = &scale[(row / 32) * blocks..(row / 32 + 1) * blocks];
                        let mut peak = 0.0f32;
                        for (b, chunk) in bytes.chunks(32).enumerate() {
                            let s = E8M0[scales[b] as usize];
                            for &byte in chunk {
                                peak = peak.max((E4M3[byte as usize] * s).abs());
                            }
                        }
                        peak
                    })
                    .reduce(|| 0.0, f32::max);
                let factor = (peak / (448.0 * 6.0)).max(f32::MIN_POSITIVE);
                weight
                    .par_chunks_mut(k)
                    .enumerate()
                    .for_each(|(row, bytes)| {
                        let scales = &scale[(row / 32) * blocks..(row / 32 + 1) * blocks];
                        let mut values: Vec<f32> = bytes
                            .iter()
                            .enumerate()
                            .map(|(j, &byte)| E4M3[byte as usize] * E8M0[scales[j / 32] as usize])
                            .collect();
                        requantize_row(&mut values, format, factor);
                        for (j, (byte, value)) in bytes.iter_mut().zip(&values).enumerate() {
                            let s = E8M0[scales[j / 32] as usize];
                            let rounded = round_e4m3((value / s).clamp(-448.0, 448.0));
                            *byte = encode_e4m3(rounded);
                        }
                    });
            }
            let va = self.core.require(&name)?.va;
            // SAFETY: the tensor was loaded at this CPU-visible address with this length, and the
            // sync above stopped all GPU work.
            unsafe {
                ptr::copy_nonoverlapping(
                    weight.as_ptr(),
                    ptr::with_exposed_provenance_mut::<u8>(va as usize),
                    weight.len(),
                );
            }
        }
        self.gpu.flush_cpu_writes();
        self.commands.sync()
    }

    /// Warm the expert tiers from `hot_experts.txt`, the routing trace shipped with this build.
    pub fn preload_hot(&mut self) -> Result<usize> {
        let hot = parse_hot_experts(HOT_EXPERTS)?;
        self.experts.preload(&hot, &self.gpu)?;
        // Workers wrote memory the GPU may have cached.
        self.commands.sync()?;
        Ok(hot.len())
    }

    /// Experts by decayed demand, hottest first, for building a preload list.
    pub fn hot_experts(&self, limit: usize) -> Vec<(usize, usize, u32)> {
        self.experts.hottest(limit)
    }

    /// Run each dispatch alone from now on and record what each kernel costs.
    pub fn enable_profile(&mut self) {
        self.commands.profile = Some(BTreeMap::new());
    }

    pub fn set_policy(&mut self, policy: Policy) {
        self.policy = policy;
    }

    /// Never overlap dispatches, for measuring what overlap gains.
    pub fn set_serial(&mut self, serial: bool) {
        self.commands.serial = serial;
    }

    /// Kernel name, dispatches, and total time, most expensive first.
    pub fn profile(&self) -> Vec<(&'static str, u64, Duration)> {
        let mut out: Vec<_> = self
            .commands
            .profile
            .iter()
            .flatten()
            .map(|(kernel, (count, time))| (self.k.name(*kernel), *count, *time))
            .collect();
        out.sort_by_key(|entry| std::cmp::Reverse(entry.2));
        out
    }

    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// Forget the conversation. Cached weights stay warm.
    pub fn reset(&mut self) {
        self.position = 0;
        self.hasher.reset();
        for store in &mut self.stores {
            store.count = 0;
        }
    }

    /// Run up to [`MAX_CHUNK`] tokens and return the logits after the last one.
    pub fn forward(&mut self, ids: &[u32]) -> Result<Vec<f32>> {
        let m = ids.len();
        if m == 0 || m > MAX_CHUNK {
            return invalid(format!("chunk of {m} tokens outside 1..={MAX_CHUNK}"));
        }
        if self.position + m > CONTEXT {
            return invalid(format!("context is limited to {CONTEXT} tokens"));
        }
        let start = self.position;
        let hashes = self.hasher.push(ids);
        let engram_layers = self.catalog.engram.len();
        let tokens: Vec<u32> = ids.iter().map(|_| self.trace.next_token()).collect();
        self.topk = vec![Vec::new(); m];
        self.candidates = vec![Vec::new(); m];
        self.host_left = vec![self.policy.host_budget; m];
        self.host.used = (MAX_CHUNK * 4) as u64;

        let ids_table = self.host.put(ids)?;
        let embed = self.core.require("embed.weight")?.va;
        let mut current = 0;
        self.commands.go(
            self.k.embed_rows,
            EmbedArgs {
                out: self.scratch.h[current].va,
                weight: embed,
                ids: ids_table,
            },
            DIM,
            m,
            Order::After,
        )?;
        let identity_mix: Vec<f32> = (0..m * MIX)
            .map(|i| if i % MIX == 0 { 1.0 } else { 0.0 })
            .collect();
        let mut pre_mix = self.host_keep(&identity_mix)?;

        for index in 0..LAYERS {
            self.host.used = self.host.used.max((MAX_CHUNK * 4) as u64);
            if self.layers[index].va.contains_key("engram.wkv.weight") {
                self.engram(index, &hashes, engram_layers, m, current)?;
            }
            let (next, mix_ffn) = self.layer(index, m, start, current, pre_mix, &tokens)?;
            current = next;
            pre_mix = mix_ffn;
            self.commands.sync()?;
            if self.fingerprints.is_some() {
                self.fingerprint(index, m, current);
            }
            // Later layers read the identity mix only at layer 0, and nothing else persists here.
            self.host.used = (MAX_CHUNK * 4) as u64;
        }

        let x = self.scratch.x.va;
        self.commands.go(
            self.k.hc_pre,
            PreArgs {
                out: x,
                h: self.scratch.h[current].va,
                mixes: pre_mix,
                mix_stride: MIX as u32,
                mix_offset: 0,
            },
            DIM,
            m,
            Order::After,
        )?;
        let weight = self.core.require("norm.weight")?.va;
        self.norm(x, weight, DIM, m)?;
        let vocab = self.catalog.core["head.weight"].shape[0] as usize;
        self.commands.go(
            self.k.gemm_bf16,
            GemmArgs {
                out: self.logits.va,
                x: x + ((m - 1) * DIM * 4) as u64,
                weight: self.core.require("head.weight")?.va,
                scale: 0,
                n: vocab as u32,
                k: DIM as u32,
                x_stride: DIM as u32,
                out_stride: vocab as u32,
            },
            vocab,
            1,
            Order::After,
        )?;
        self.commands.sync()?;
        let mut logits = vec![0.0f32; vocab];
        // SAFETY: the GPU has finished and the buffer holds `vocab` f32 values.
        unsafe { ptr::copy_nonoverlapping(self.logits.ptr::<f32>(), logits.as_mut_ptr(), vocab) };
        self.position += m;
        Ok(logits)
    }

    fn probe(&mut self, layer: usize, name: &str, va: u64, values: usize) -> Result<()> {
        if layer >= PROBE_LAYERS || self.probes.is_none() {
            return Ok(());
        }
        self.commands.sync()?;
        let mut out = vec![0.0f32; values];
        // SAFETY: probes name VRAM scratch buffers of at least this size, CPU-visible and idle.
        unsafe {
            ptr::copy_nonoverlapping(
                ptr::with_exposed_provenance::<f32>(va as usize),
                out.as_mut_ptr(),
                values,
            );
        }
        if let Some(list) = &mut self.probes {
            list.push((format!("{layer}.{name}"), out));
        }
        Ok(())
    }

    fn host_keep(&mut self, values: &[f32]) -> Result<u64> {
        self.host.put(values)
    }

    fn fingerprint(&mut self, index: usize, m: usize, current: usize) {
        let mut last = vec![0.0f32; HC * DIM];
        // SAFETY: the buffer is mapped and holds `m` rows, and the GPU has finished.
        unsafe {
            ptr::copy_nonoverlapping(
                self.scratch.h[current].ptr::<f32>().add((m - 1) * HC * DIM),
                last.as_mut_ptr(),
                HC * DIM,
            );
        }
        let mean = last.iter().sum::<f32>() / last.len() as f32;
        let norm = last.iter().map(|v| v * v).sum::<f32>().sqrt();
        if let Some(prints) = &mut self.fingerprints {
            prints.push((index, mean, norm));
        }
    }

    fn engram(
        &mut self,
        index: usize,
        hashes: &[u64],
        engram_layers: usize,
        m: usize,
        current: usize,
    ) -> Result<()> {
        let start = Instant::now();
        let table = self
            .catalog
            .engram
            .iter()
            .position(|t| t.layer == index)
            .unwrap_or(0);
        let rows: Vec<u64> = hashes
            .chunks_exact(engram_layers * HASH_COLS)
            .flat_map(|t| {
                t[table * HASH_COLS..(table + 1) * HASH_COLS]
                    .iter()
                    .copied()
            })
            .collect();
        let row_bytes = self.engram.row_bytes();
        let weight_bytes = self.engram.weight_bytes();
        let mut raw = vec![0u8; rows.len() * row_bytes];
        self.engram
            .lookup_batch(table, &rows, &mut raw, &self.reader)?;
        let mut values = engram_values(&raw, m, row_bytes, weight_bytes);
        let flat = HASH_COLS * weight_bytes;
        for row in values.chunks_exact_mut(flat) {
            quant_fp8(row);
            for value in row {
                *value *= GEMM_PRESCALE;
            }
        }
        // SAFETY: the scratch buffer holds MAX_CHUNK rows, and no GPU work uses it right now.
        unsafe {
            ptr::copy_nonoverlapping(
                values.as_ptr(),
                self.scratch.engram_x.ptr::<f32>(),
                values.len(),
            );
        }
        self.gpu.flush_cpu_writes();
        self.timing.engram += start.elapsed();

        let t = &self.layers[index];
        let (w, s, qw, kw) = (
            t.va("engram.wkv.weight")?,
            t.va("engram.wkv.scale")?,
            t.va("engram.q_weight")?,
            t.va("engram.k_weight")?,
        );
        let n = (HC + 1) * DIM;
        self.commands.go(
            self.k.gemm_fp8,
            GemmArgs {
                out: self.scratch.engram_kv.va,
                x: self.scratch.engram_x.va,
                weight: w,
                scale: s,
                n: n as u32,
                k: flat as u32,
                x_stride: flat as u32,
                out_stride: n as u32,
            },
            n,
            m,
            Order::After,
        )?;
        self.commands.go(
            self.k.engram_gate,
            EngramArgs {
                h: self.scratch.h[current].va,
                kv: self.scratch.engram_kv.va,
                q_weight: qw,
                k_weight: kw,
            },
            1,
            m * HC,
            Order::After,
        )
    }

    fn gemm(&mut self, kernel: u64, args: GemmArgs, m: usize, order: Order) -> Result<()> {
        self.commands.go(kernel, args, args.n as usize, m, order)
    }

    /// Quantize `m` rows into `dst` for multiplying weights, and return its address.
    fn quantized(&mut self, src: u64, dst: u64, dim: usize, m: usize) -> Result<u64> {
        self.commands.go(
            self.k.quant_fp8_into,
            QuantIntoArgs {
                src,
                dst,
                stride: dim as u32,
                pad: 0,
            },
            dim / 32,
            m,
            Order::After,
        )?;
        Ok(dst)
    }

    /// The projection half of the mix: many short waves. Reads only the stream `h`.
    fn mix_project(&mut self, fun: u64, h: u64, m: usize, order: Order) -> Result<()> {
        self.commands.go(
            self.k.mix_partial,
            PartialArgs {
                out: self.scratch.mix_raw.va,
                x: h,
                weight: fun,
                frags: MIX_FRAGS as u32,
                pad: 0,
            },
            MIX + 1,
            m * MIX_FRAGS,
            order,
        )
    }

    /// The coefficient half of the mix, once the projection has finished.
    fn mix_finish(
        &mut self,
        scale: u64,
        base: u64,
        out: u64,
        m: usize,
        order: Order,
    ) -> Result<()> {
        self.commands.go(
            self.k.hc_sinkhorn,
            MixArgs {
                mix: self.scratch.mix_raw.va,
                frags: MIX_FRAGS as u64,
                scale,
                base,
                out,
            },
            1,
            m,
            order,
        )
    }

    fn norm_begin(
        &mut self,
        buf: u64,
        weight: u64,
        dim: usize,
        m: usize,
        slot: u64,
        order: Order,
    ) -> Result<NormArgs> {
        let frags = (dim / NORM_FRAGMENT).max(1);
        let args = NormArgs {
            buf,
            weight,
            partial: self.scratch.norm_partial.va + slot * (MAX_CHUNK * MAX_NORM_FRAGS * 4) as u64,
            dim: dim as u32,
            stride: dim as u32,
            frags: frags as u32,
            pad: 0,
        };
        self.commands
            .go(self.k.norm_partial, args, 1, m * frags, order)?;
        Ok(args)
    }

    fn norm_end(&mut self, args: NormArgs, m: usize, order: Order) -> Result<()> {
        self.commands
            .go(self.k.norm_apply, args, args.dim as usize, m, order)
    }

    fn norm(&mut self, buf: u64, weight: u64, dim: usize, m: usize) -> Result<()> {
        let args = self.norm_begin(buf, weight, dim, m, 0, Order::After)?;
        self.norm_end(args, m, Order::After)
    }

    /// One full block. Returns the index of the stream buffer now current and the mix for the next.
    fn layer(
        &mut self,
        index: usize,
        m: usize,
        start: usize,
        current: usize,
        pre_mix: u64,
        tokens: &[u32],
    ) -> Result<(usize, u64)> {
        let t = &self.layers[index];
        let (attn_fn, attn_scale, attn_base) = (
            t.va("hc_attn_fn")?,
            t.va("hc_attn_scale")?,
            t.va("hc_attn_base")?,
        );
        let (ffn_fn, ffn_scale, ffn_base) = (
            t.va("hc_ffn_fn")?,
            t.va("hc_ffn_scale")?,
            t.va("hc_ffn_base")?,
        );
        let (attn_norm, ffn_norm) = (t.va("attn_norm.weight")?, t.va("ffn_norm.weight")?);
        let residual = self.scratch.h[current].va;
        let mix_attn = self.scratch.mix_attn.va;
        let x = self.scratch.x.va;
        // Both read only the stream, so they overlap; so do the two steps that need them.
        self.mix_project(attn_fn, residual, m, Order::After)?;
        self.commands.go(
            self.k.hc_pre,
            PreArgs {
                out: x,
                h: residual,
                mixes: pre_mix,
                mix_stride: MIX as u32,
                mix_offset: 0,
            },
            DIM,
            m,
            Order::Overlap,
        )?;
        self.mix_finish(attn_scale, attn_base, mix_attn, m, Order::After)?;
        let norm = self.norm_begin(x, attn_norm, DIM, m, 0, Order::Overlap)?;
        self.norm_end(norm, m, Order::After)?;
        self.probe(index, "mix_attn", mix_attn, m * MIX)?;
        self.probe(index, "x_attn", x, m * DIM)?;
        self.attention(index, m, start)?;
        self.probe(index, "out_attn", self.scratch.out.va, m * DIM)?;
        let next = 1 - current;
        self.commands.go(
            self.k.hc_post,
            PostArgs {
                out: self.scratch.h[next].va,
                x: self.scratch.out.va,
                residual,
                mixes: mix_attn,
            },
            HC * DIM,
            m,
            Order::After,
        )?;

        let residual = self.scratch.h[next].va;
        self.probe(index, "h_attn", residual, m * HC * DIM)?;
        let mix_ffn = self.scratch.mix_ffn[index % 2].va;
        self.mix_project(ffn_fn, residual, m, Order::After)?;
        self.commands.go(
            self.k.hc_pre,
            PreArgs {
                out: x,
                h: residual,
                mixes: mix_attn,
                mix_stride: MIX as u32,
                mix_offset: 0,
            },
            DIM,
            m,
            Order::Overlap,
        )?;
        self.mix_finish(ffn_scale, ffn_base, mix_ffn, m, Order::After)?;
        let norm = self.norm_begin(x, ffn_norm, DIM, m, 0, Order::Overlap)?;
        self.norm_end(norm, m, Order::After)?;
        self.probe(index, "mix_ffn", mix_ffn, m * MIX)?;
        self.probe(index, "x_ffn", x, m * DIM)?;
        self.moe(index, m, tokens)?;
        self.probe(index, "out_ffn", self.scratch.out.va, m * DIM)?;
        self.commands.go(
            self.k.hc_post,
            PostArgs {
                out: self.scratch.h[current].va,
                x: self.scratch.out.va,
                residual,
                mixes: mix_ffn,
            },
            HC * DIM,
            m,
            Order::After,
        )?;
        Ok((current, mix_ffn))
    }

    fn attention(&mut self, index: usize, m: usize, start: usize) -> Result<()> {
        let t = &self.layers[index];
        let (wq_a, wq_a_s) = (t.va("attn.wq_a.weight")?, t.va("attn.wq_a.scale")?);
        let (wq_b, wq_b_s) = (t.va("attn.wq_b.weight")?, t.va("attn.wq_b.scale")?);
        let (wkv, wkv_s) = (t.va("attn.wkv.weight")?, t.va("attn.wkv.scale")?);
        let (wo_a, wo_a_s) = (t.va("attn.wo_a.weight")?, t.va("attn.wo_a.scale")?);
        let (wo_b, wo_b_s) = (t.va("attn.wo_b.weight")?, t.va("attn.wo_b.scale")?);
        let (q_norm, kv_norm) = (t.va("attn.q_norm.weight")?, t.va("attn.kv_norm.weight")?);
        let sink = t.va("attn.attn_sink")?;
        let ratio = t.ratio;
        let ring = t.ring.va;
        let rope = t.rope.clone();
        let s = &self.scratch;
        let (x, xq, qr, qrq, q, kv_new, o, flat, out, scores) = (
            s.x.va,
            s.xq.va,
            s.qr.va,
            s.qrq.va,
            s.q.va,
            s.kv_new.va,
            s.o.va,
            s.flat.va,
            s.out.va,
            s.scores.va,
        );

        let mut table = vec![0.0f32; m * 64];
        for (row, chunk) in table.chunks_exact_mut(64).enumerate() {
            rope.write_table(start + row, chunk);
        }
        let rope_table = self.host.put(&table)?;

        self.quantized(x, xq, DIM, m)?;
        self.probe(index, "xq", xq, m * DIM)?;
        self.gemm(
            self.k.gemm_fp8,
            fp8(qr, xq, wq_a, wq_a_s, Q_LORA, DIM),
            m,
            Order::After,
        )?;
        self.gemm(
            self.k.gemm_fp8,
            fp8(kv_new, xq, wkv, wkv_s, HEAD_DIM, DIM),
            m,
            Order::Overlap,
        )?;
        self.probe(index, "qr_raw", qr, m * Q_LORA)?;
        self.norm(qr, q_norm, Q_LORA, m)?;
        self.probe(index, "qr", qr, m * Q_LORA)?;
        self.quantized(qr, qrq, Q_LORA, m)?;
        let q_dim = HEADS * HEAD_DIM;
        self.gemm(
            self.k.gemm_fp8,
            fp8(q, qrq, wq_b, wq_b_s, q_dim, Q_LORA),
            m,
            Order::After,
        )?;
        self.rope(q, rope_table, q_dim, HEAD_DIM, HEADS, m, 1.0)?;
        self.probe(index, "q", q, m * q_dim)?;

        self.norm(kv_new, kv_norm, HEAD_DIM, m)?;
        self.rope(kv_new, rope_table, HEAD_DIM, HEAD_DIM, 1, m, 1.0)?;
        self.commands.go(
            self.k.quant_fp8,
            QuantArgs {
                buf: kv_new,
                stride: HEAD_DIM as u32,
                pad: 0,
            },
            HEAD_DIM / 32,
            m,
            Order::After,
        )?;
        let slots: Vec<u32> = (0..m).map(|t| ((start + t) % RING) as u32).collect();
        let slots = self.host.put(&slots)?;
        self.commands.go(
            self.k.put_rows,
            CopyArgs {
                dst: ring,
                src: kv_new,
                table: slots,
                dim: HEAD_DIM as u32,
                add: 0,
            },
            HEAD_DIM,
            m,
            Order::After,
        )?;

        self.probe(index, "kv_new", kv_new, m * HEAD_DIM)?;
        if ratio > 0 {
            self.compress_and_index(index, m, start)?;
        }

        // Three overlapping steps per query, each separated from the next by one barrier.
        let mut calls = Vec::with_capacity(m);
        for row in 0..m {
            let position = start + row;
            let first = position.saturating_sub(WINDOW - 1);
            let mut sources: Vec<u64> = (first..=position)
                .map(|p| ring + ((p % RING) * HEAD_DIM * 4) as u64)
                .collect();
            if ratio > 0 {
                let kv = self.stores[self.current_store].kv.va;
                sources.extend(
                    self.topk[row]
                        .iter()
                        .map(|&i| kv + u64::from(i) * (HEAD_DIM * 4) as u64),
                );
            }
            let count = sources.len();
            debug_assert!(count <= MAX_SOURCES);
            calls.push(AttnArgs {
                out: o + (row * q_dim * 4) as u64,
                q: q + (row * q_dim * 4) as u64,
                sources: self.host.put(&sources)?,
                scores: scores + (row * HEADS * MAX_SOURCES * 4) as u64,
                sink,
                count: count as u32,
                pad: 0,
            });
        }
        for (row, args) in calls.iter().enumerate() {
            let order = if row == 0 {
                Order::After
            } else {
                Order::Overlap
            };
            self.commands
                .go(self.k.attn_scores, *args, args.count as usize, HEADS, order)?;
        }
        for (row, args) in calls.iter().enumerate() {
            let order = if row == 0 {
                Order::After
            } else {
                Order::Overlap
            };
            self.commands
                .go(self.k.attn_weights, *args, 1, HEADS, order)?;
        }
        for (row, args) in calls.iter().enumerate() {
            let order = if row == 0 {
                Order::After
            } else {
                Order::Overlap
            };
            // Lanes are heads and rows are 16-dimension blocks, per the kernel.
            self.commands
                .go(self.k.attn_output, *args, HEADS, HEAD_DIM / 16, order)?;
        }

        self.rope(o, rope_table, q_dim, HEAD_DIM, HEADS, m, -1.0)?;
        self.commands.go(
            self.k.quant_fp8_gemm,
            QuantArgs {
                buf: o,
                stride: q_dim as u32,
                pad: 0,
            },
            q_dim / 32,
            m,
            Order::After,
        )?;
        self.probe(index, "o", o, m * q_dim)?;
        let group_in = q_dim / GROUPS;
        for group in 0..GROUPS {
            let order = if group == 0 {
                Order::After
            } else {
                Order::Overlap
            };
            self.gemm(
                self.k.gemm_fp8,
                GemmArgs {
                    out: flat + (group * O_LORA * 4) as u64,
                    x: o + (group * group_in * 4) as u64,
                    weight: wo_a + (group * O_LORA * group_in) as u64,
                    scale: wo_a_s + (group * (O_LORA / 32) * (group_in / 32)) as u64,
                    n: O_LORA as u32,
                    k: group_in as u32,
                    x_stride: q_dim as u32,
                    out_stride: (GROUPS * O_LORA) as u32,
                },
                m,
                order,
            )?;
        }
        let flat_dim = GROUPS * O_LORA;
        self.commands.go(
            self.k.quant_fp8_gemm,
            QuantArgs {
                buf: flat,
                stride: flat_dim as u32,
                pad: 0,
            },
            flat_dim / 32,
            m,
            Order::After,
        )?;
        self.probe(index, "flat", flat, m * flat_dim)?;
        self.gemm(
            self.k.gemm_fp8,
            fp8(out, flat, wo_b, wo_b_s, DIM, flat_dim),
            m,
            Order::After,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn rope(
        &mut self,
        buf: u64,
        table: u64,
        stride: usize,
        head_dim: usize,
        heads: usize,
        rows: usize,
        sign: f32,
    ) -> Result<()> {
        self.commands.go(
            self.k.rope,
            RopeArgs {
                buf,
                table,
                stride: stride as u32,
                head_dim: head_dim as u32,
                sign,
                pad: 0,
            },
            heads,
            rows,
            Order::After,
        )
    }

    fn compress_and_index(&mut self, index: usize, m: usize, start: usize) -> Result<()> {
        if let Some(source) = KV_SOURCES.iter().position(|&id| id == index) {
            self.current_store = source;
            self.compress(index, source, m, start)?;
        }
        if !INDEX_SOURCES.contains(&index) {
            return Ok(());
        }
        let t = &self.layers[index];
        let (wq, wq_s) = (
            t.va("attn.indexer.wq_b.weight")?,
            t.va("attn.indexer.wq_b.scale")?,
        );
        let proj = t.va("attn.indexer.weights_proj.weight")?;
        let ratio = t.ratio;
        let rope = t.rope.clone();
        let (qrq, x, iq, iw) = (
            self.scratch.qrq.va,
            self.scratch.x.va,
            self.scratch.index_q.va,
            self.scratch.index_w.va,
        );
        let q_dim = INDEX_HEADS * INDEX_DIM;
        self.gemm(
            self.k.gemm_fp8,
            fp8(iq, qrq, wq, wq_s, q_dim, Q_LORA),
            m,
            Order::After,
        )?;
        let mut table = vec![0.0f32; m * 64];
        for (row, chunk) in table.chunks_exact_mut(64).enumerate() {
            rope.write_table(start + row, chunk);
        }
        let table = self.host.put(&table)?;
        self.rope(iq, table, q_dim, INDEX_DIM, INDEX_HEADS, m, 1.0)?;
        self.commands.go(
            self.k.quant_fp4_pow2,
            QuantArgs {
                buf: iq,
                stride: q_dim as u32,
                pad: 0,
            },
            q_dim / 32,
            m,
            Order::After,
        )?;
        self.gemm(
            self.k.gemm_bf16,
            GemmArgs {
                out: iw,
                x,
                weight: proj,
                scale: 0,
                n: INDEX_HEADS as u32,
                k: DIM as u32,
                x_stride: DIM as u32,
                out_stride: INDEX_HEADS as u32,
            },
            m,
            Order::After,
        )?;
        self.commands.go(
            self.k.scale_values,
            ScaleArgs {
                buf: iw,
                factor: ((INDEX_DIM * INDEX_HEADS) as f32).sqrt().recip(),
                pad: 0,
            },
            m * INDEX_HEADS,
            1,
            Order::After,
        )?;

        let keys = self.stores[self.current_store].keys.va;
        let mut offsets = Vec::with_capacity(m);
        let mut used = 0usize;
        for row in 0..m {
            let visible = (start + row + 1) / ratio;
            offsets.push((used, visible));
            if visible > 0 {
                let order = if used == 0 {
                    Order::After
                } else {
                    Order::Overlap
                };
                self.commands.go(
                    self.k.index_scores,
                    IndexArgs {
                        out: self.score_host.va + (used * 4) as u64,
                        q: iq + (row * q_dim * 4) as u64,
                        keys,
                        weights: iw + (row * INDEX_HEADS * 4) as u64,
                    },
                    visible,
                    1,
                    order,
                )?;
            }
            used += visible;
            if (used * 4) as u64 > SCORE_BYTES {
                return invalid("index scores overflow their buffer");
            }
        }
        self.commands.sync()?;
        for (row, &(at, visible)) in offsets.iter().enumerate() {
            if visible == 0 {
                self.topk[row].clear();
                continue;
            }
            let mut scores = vec![0.0f32; visible];
            // SAFETY: the GPU has finished and this range was bounds-checked above.
            unsafe {
                ptr::copy_nonoverlapping(
                    self.score_host.ptr::<f32>().add(at),
                    scores.as_mut_ptr(),
                    visible,
                );
            }
            if index == CANDIDATE_SOURCE {
                self.candidates[row] = candidate_blocks(&scores);
            } else if index > CANDIDATE_SOURCE {
                for (score, keep) in scores.iter_mut().zip(&self.candidates[row]) {
                    if !keep {
                        *score = f32::NEG_INFINITY;
                    }
                }
            }
            self.topk[row] = top_positions(&scores, INDEX_TOPK);
        }
        Ok(())
    }

    /// Append this chunk's completed groups to the source's KV and index keys.
    fn compress(&mut self, index: usize, source: usize, m: usize, start: usize) -> Result<()> {
        let t = &self.layers[index];
        let ratio = t.ratio;
        let rope = t.rope.clone();
        let wkv = t.va("attn.compressor.wkv.weight")?;
        let wgate = if ratio > 1 {
            Some(t.va("attn.compressor.wgate.weight")?)
        } else {
            None
        };
        let norm = t.va("attn.compressor.norm.weight")?;
        let wk = t.va("attn.indexer.wk.weight")?;
        let k_norm = t.va("attn.indexer.k_norm.weight")?;
        let (x, latent, keys, kv_tmp, score_tmp) = (
            self.scratch.x.va,
            self.scratch.latent.va,
            self.scratch.keys.va,
            self.scratch.pair_kv.va,
            self.scratch.pair_score.va,
        );
        let bf16 = |out: u64, x: u64, weight: u64, n: usize, k: usize| GemmArgs {
            out,
            x,
            weight,
            scale: 0,
            n: n as u32,
            k: k as u32,
            x_stride: k as u32,
            out_stride: n as u32,
        };
        let mut groups = Vec::new();
        if let Some(wgate) = wgate {
            let store = &self.stores[source];
            let (ring_kv, ring_score) = (store.pair_kv.va, store.pair_score.va);
            self.gemm(
                self.k.gemm_bf16,
                bf16(kv_tmp, x, wkv, HEAD_DIM, DIM),
                m,
                Order::After,
            )?;
            self.gemm(
                self.k.gemm_bf16,
                bf16(score_tmp, x, wgate, HEAD_DIM, DIM),
                m,
                Order::Overlap,
            )?;
            let slots: Vec<u32> = (0..m).map(|t| ((start + t) % PAIR_RING) as u32).collect();
            let slots = self.host.put(&slots)?;
            for (dst, src, order) in [
                (ring_kv, kv_tmp, Order::After),
                (ring_score, score_tmp, Order::Overlap),
            ] {
                self.commands.go(
                    self.k.put_rows,
                    CopyArgs {
                        dst,
                        src,
                        table: slots,
                        dim: HEAD_DIM as u32,
                        add: 0,
                    },
                    HEAD_DIM,
                    m,
                    order,
                )?;
            }
            let mut first_rows = Vec::new();
            for row in 0..m {
                let position = start + row;
                if (position + 1).is_multiple_of(ratio) {
                    groups.push(position + 1 - ratio);
                    first_rows.push(((position + 1 - ratio) % PAIR_RING) as u32);
                }
            }
            if groups.is_empty() {
                return Ok(());
            }
            let first_rows = self.host.put(&first_rows)?;
            self.commands.go(
                self.k.pool_pairs,
                PoolArgs {
                    out: latent,
                    kv: ring_kv,
                    score: ring_score,
                    table: first_rows,
                    ring: PAIR_RING as u32,
                    pad: 0,
                },
                HEAD_DIM,
                groups.len(),
                Order::After,
            )?;
        } else {
            self.gemm(
                self.k.gemm_bf16,
                bf16(latent, x, wkv, HEAD_DIM, DIM),
                m,
                Order::After,
            )?;
            groups.extend(start..start + m);
        }
        let g = groups.len();
        let store = &self.stores[source];
        let (kv_store, key_store, count) = (store.kv.va, store.keys.va, store.count);
        if (count + g) as u64 * (HEAD_DIM * 4) as u64 > store.kv.size {
            return invalid("compressed KV store is full");
        }
        self.norm(latent, norm, HEAD_DIM, g)?;
        self.gemm(
            self.k.gemm_bf16,
            bf16(keys, latent, wk, INDEX_DIM, HEAD_DIM),
            g,
            Order::After,
        )?;
        self.norm(keys, k_norm, INDEX_DIM, g)?;
        let mut table = vec![0.0f32; g * 64];
        for (chunk, &position) in table.chunks_exact_mut(64).zip(&groups) {
            rope.write_table(position, chunk);
        }
        let table = self.host.put(&table)?;
        self.rope(keys, table, INDEX_DIM, INDEX_DIM, 1, g, 1.0)?;
        self.commands.go(
            self.k.quant_fp4_pow2,
            QuantArgs {
                buf: keys,
                stride: INDEX_DIM as u32,
                pad: 0,
            },
            INDEX_DIM / 32,
            g,
            Order::After,
        )?;
        self.rope(latent, table, HEAD_DIM, HEAD_DIM, 1, g, 1.0)?;
        self.commands.go(
            self.k.quant_fp4_e4m3,
            QuantArgs {
                buf: latent,
                stride: HEAD_DIM as u32,
                pad: 0,
            },
            HEAD_DIM / 16,
            g,
            Order::After,
        )?;
        let rows: Vec<u32> = (0..g).map(|i| (count + i) as u32).collect();
        let rows = self.host.put(&rows)?;
        for (dst, src, dim, order) in [
            (key_store, keys, INDEX_DIM, Order::After),
            (kv_store, latent, HEAD_DIM, Order::Overlap),
        ] {
            self.commands.go(
                self.k.put_rows,
                CopyArgs {
                    dst,
                    src,
                    table: rows,
                    dim: dim as u32,
                    add: 0,
                },
                dim,
                g,
                order,
            )?;
        }
        self.stores[source].count += g;
        Ok(())
    }

    /// The gate, then only the experts it picked, overlapping, then the shared expert.
    fn moe(&mut self, index: usize, m: usize, tokens: &[u32]) -> Result<()> {
        let t = &self.layers[index];
        let gate = t.va("ffn.gate.weight")?;
        let shared = [
            (
                t.va("ffn.shared_experts.w1.weight")?,
                t.va("ffn.shared_experts.w1.scale")?,
            ),
            (
                t.va("ffn.shared_experts.w2.weight")?,
                t.va("ffn.shared_experts.w2.scale")?,
            ),
            (
                t.va("ffn.shared_experts.w3.weight")?,
                t.va("ffn.shared_experts.w3.scale")?,
            ),
        ];
        let bias = t.gate_bias.clone();
        let (x, xq) = (self.scratch.x.va, self.scratch.xq.va);
        self.gemm(
            self.k.gemm_bf16,
            GemmArgs {
                out: self.gate_out.va,
                x,
                weight: gate,
                scale: 0,
                n: ROUTED as u32,
                k: DIM as u32,
                x_stride: DIM as u32,
                out_stride: ROUTED as u32,
            },
            m,
            Order::After,
        )?;
        self.quantized(x, xq, DIM, m)?;
        self.commands.go(
            self.k.take_rows,
            CopyArgs {
                dst: self.x_host.va,
                src: xq,
                table: self.identity,
                dim: DIM as u32,
                add: 0,
            },
            DIM,
            m,
            Order::After,
        )?;
        self.commands.sync()?;
        let mut raw = vec![0.0f32; m * ROUTED];
        // SAFETY: the GPU has finished and the buffer holds MAX_CHUNK rows.
        unsafe {
            ptr::copy_nonoverlapping(self.gate_out.ptr::<f32>(), raw.as_mut_ptr(), raw.len())
        };

        let start = Instant::now();
        if self.experts.poll(&self.gpu) {
            // Workers rewrote memory the GPU may have cached.
            self.commands.sync()?;
        }
        self.experts.begin_layer();
        let mut groups: BTreeMap<usize, Vec<(u32, f32)>> = BTreeMap::new();
        for (row, scores) in raw.chunks_exact(ROUTED).enumerate() {
            let picks = self.pick(index, row, scores, &bias, tokens[row])?;
            for (expert, weight) in picks {
                groups.entry(expert).or_default().push((row as u32, weight));
            }
        }
        for scores in raw.chunks_exact(ROUTED) {
            for (expert, _) in route(scores, &bias) {
                self.experts.want(index, expert);
            }
        }

        let mut vram = Vec::with_capacity(groups.len());
        let mut ram = Vec::new();
        let mut cold = Vec::new();
        for &expert in groups.keys() {
            match self.experts.claim(index, expert) {
                Some((Tier::Vram, tensors)) => vram.push((expert, tensors)),
                Some((Tier::Ram, _)) => ram.push(expert),
                None => cold.push(expert),
            }
        }
        self.timing.experts += start.elapsed();
        self.run_experts(m, &vram, &groups, shared)?;
        // Ready experts first; cold ones finish loading while the GPU and CPU work.
        ram.append(&mut cold);
        self.run_ram_experts(index, m, &ram, &groups)
    }

    /// RAM experts on the CPU, while the GPU works, then add their output to the GPU's.
    fn run_ram_experts(
        &mut self,
        index: usize,
        m: usize,
        ram: &[usize],
        groups: &BTreeMap<usize, Vec<(u32, f32)>>,
    ) -> Result<()> {
        if ram.is_empty() {
            return Ok(());
        }
        let start = Instant::now();
        let mut x = vec![0.0f32; m * DIM];
        // SAFETY: the sync before routing finished this write, and the buffer holds `m` rows.
        unsafe { ptr::copy_nonoverlapping(self.x_host.ptr::<f32>(), x.as_mut_ptr(), x.len()) };
        for value in &mut x {
            *value /= GEMM_PRESCALE;
        }
        let mut sum = vec![0.0f32; m * DIM];
        let run = |expert: usize, tensors: &[&[u8]; EXPERT_EXTENTS]| {
            let members = &groups[&expert];
            let mut input = Vec::with_capacity(members.len() * DIM);
            for &(row, _) in members {
                let row = row as usize;
                input.extend_from_slice(&x[row * DIM..(row + 1) * DIM]);
            }
            let weights: Vec<f32> = members.iter().map(|(_, w)| *w).collect();
            (expert, cpu_fp4::expert(&input, tensors, &weights))
        };
        // Everything already in RAM runs together, in one burst across the cores.
        let mut ready = Vec::with_capacity(ram.len());
        let mut cold = Vec::new();
        for &expert in ram {
            match self.experts.ram_tensors(index, expert) {
                Some(tensors) => ready.push((expert, tensors)),
                None => cold.push(expert),
            }
        }
        let mut outputs: Vec<(usize, Vec<f32>)> = ready
            .par_iter()
            .map(|(expert, tensors)| run(*expert, tensors))
            .collect();
        drop(ready);
        for expert in cold {
            let wait = Instant::now();
            self.experts.load_now(index, expert, &self.gpu)?;
            self.timing.fetch_wait += wait.elapsed();
            let Some(tensors) = self.experts.ram_tensors(index, expert) else {
                return invalid("RAM expert left memory while being scheduled");
            };
            outputs.push(run(expert, &tensors));
        }
        for (expert, out) in &outputs {
            for (&(row, _), values) in groups[expert].iter().zip(out.chunks_exact(DIM)) {
                let row = row as usize;
                for (total, value) in sum[row * DIM..(row + 1) * DIM].iter_mut().zip(values) {
                    *total += value;
                }
            }
        }
        // SAFETY: no GPU work reads this buffer until the packet below, and it holds `m` rows.
        unsafe { ptr::copy_nonoverlapping(sum.as_ptr(), self.cpu_out.ptr::<f32>(), sum.len()) };
        self.timing.cpu_experts += start.elapsed();
        self.commands.go(
            self.k.put_rows,
            CopyArgs {
                dst: self.scratch.out.va,
                src: self.cpu_out.va,
                table: self.identity,
                dim: DIM as u32,
                add: 1,
            },
            DIM,
            m,
            Order::After,
        )
    }

    /// Choose a token's experts: the true top six where they can run now, else the best stand-ins.
    fn pick(
        &mut self,
        index: usize,
        row: usize,
        raw: &[f32],
        bias: &[f32],
        token: u32,
    ) -> Result<Vec<(usize, f32)>> {
        let scores: Vec<f32> = raw.iter().map(|s| s.exp().ln_1p().sqrt()).collect();
        let mut order: Vec<usize> = (0..ROUTED).collect();
        order.sort_unstable_by(|a, b| (scores[*b] + bias[*b]).total_cmp(&(scores[*a] + bias[*a])));
        for &expert in &order[..ACTIVE] {
            self.trace.record(token, index, expert);
            self.experts.note_demand(index, expert);
        }
        let policy = self.policy;
        // Stand-ins may reach past the true six; the other policies stay inside them.
        let reach = if policy.missing == Missing::StandIn {
            ROUTED
        } else {
            ACTIVE
        };
        let mut chosen = Vec::with_capacity(ACTIVE);
        for (rank, &expert) in order[..reach].iter().enumerate() {
            if chosen.len() == ACTIVE {
                break;
            }
            let required = rank < policy.must_run;
            let usable = match self.experts.tier(index, expert) {
                Some(Tier::Vram) => true,
                Some(Tier::Ram)
                    if required || (rank < policy.ram_ranks && self.host_left[row] > 0) =>
                {
                    self.host_left[row] = self.host_left[row].saturating_sub(1);
                    true
                }
                None if required => {
                    self.experts.request(index, expert)?;
                    self.host_left[row] = self.host_left[row].saturating_sub(1);
                    true
                }
                _ => false,
            };
            if usable {
                chosen.push(expert);
                // Claim now, so that loading another expert cannot evict this one.
                let _ = self.experts.claim(index, expert);
                if rank >= ACTIVE {
                    self.timing.substituted += 1;
                }
            }
        }
        // Nothing resident at all, as on the first tokens: run the top choice.
        if chosen.is_empty() {
            let expert = order[0];
            self.experts.request(index, expert)?;
            chosen.push(expert);
            let _ = self.experts.claim(index, expert);
        }
        if policy.missing != Missing::StandIn {
            self.timing.substituted += (ACTIVE - chosen.len()) as u64;
        }
        for &expert in &chosen {
            match self.experts.tier(index, expert) {
                Some(Tier::Vram) => self.timing.vram_runs += 1,
                _ => self.timing.ram_runs += 1,
            }
        }
        let over = if policy.missing == Missing::DropKeepScale {
            &order[..ACTIVE]
        } else {
            &chosen[..]
        };
        let sum: f32 = over.iter().map(|&e| scores[e]).sum::<f32>() + 1e-20;
        Ok(chosen
            .into_iter()
            .map(|e| (e, scores[e] / sum * ROUTE_SCALE))
            .collect())
    }

    /// The shared expert and the routed experts, phase by phase, then their sum.
    fn run_experts(
        &mut self,
        m: usize,
        views: &[(usize, [u64; EXPERT_EXTENTS])],
        groups: &BTreeMap<usize, Vec<(u32, f32)>>,
        shared: [(u64, u64); 3],
    ) -> Result<()> {
        let (xq, out, arena) = (
            self.scratch.xq.va,
            self.scratch.out.va,
            self.scratch.experts.va,
        );
        struct Job {
            rows: usize,
            input: u64,
            gate: u64,
            up: u64,
            mid: u64,
            out: u64,
            table: u64,
            weights: u64,
            mats: [(u64, u64); 3],
        }
        let mut at = arena;
        let mut carve = |rows: usize, dim: usize| {
            let va = at;
            at += (rows * dim * 4) as u64;
            va
        };
        let mut jobs = Vec::with_capacity(views.len());
        for (expert, tensors) in views {
            let members = &groups[expert];
            let rows = members.len();
            let table: Vec<u32> = members.iter().map(|(r, _)| *r).collect();
            let weights: Vec<f32> = members.iter().map(|(_, w)| *w).collect();
            jobs.push(Job {
                rows,
                input: if rows == m { xq } else { carve(rows, DIM) },
                gate: carve(rows, MOE_DIM),
                up: carve(rows, MOE_DIM),
                mid: carve(rows, MOE_DIM),
                out: carve(rows, DIM),
                table: self.host.put(&table)?,
                weights: self.host.put(&weights)?,
                mats: [
                    (tensors[0], tensors[1]),
                    (tensors[2], tensors[3]),
                    (tensors[4], tensors[5]),
                ],
            });
        }
        let shared_gate = carve(m, MOE_DIM);
        let shared_up = carve(m, MOE_DIM);
        let shared_mid = carve(m, MOE_DIM);
        let shared_out = carve(m, DIM);
        if at - arena > self.scratch.experts.size {
            return invalid("expert working memory is full");
        }
        // The shared expert leads each phase after a barrier, and the routed experts overlap it.

        let copies = jobs.iter().filter(|job| job.input != xq);
        for (i, job) in copies.enumerate() {
            self.commands.go(
                self.k.take_rows,
                CopyArgs {
                    dst: job.input,
                    src: xq,
                    table: job.table,
                    dim: DIM as u32,
                    add: 0,
                },
                DIM,
                job.rows,
                if i == 0 { Order::After } else { Order::Overlap },
            )?;
        }
        self.gemm(
            self.k.gemm_fp8,
            fp8(shared_gate, xq, shared[0].0, shared[0].1, MOE_DIM, DIM),
            m,
            Order::After,
        )?;
        self.gemm(
            self.k.gemm_fp8,
            fp8(shared_up, xq, shared[2].0, shared[2].1, MOE_DIM, DIM),
            m,
            Order::Overlap,
        )?;
        for job in &jobs {
            for (output, mat) in [(job.gate, job.mats[0]), (job.up, job.mats[2])] {
                self.gemm(
                    self.k.gemm_fp4,
                    GemmArgs {
                        out: output,
                        x: job.input,
                        weight: mat.0,
                        scale: mat.1,
                        n: MOE_DIM as u32,
                        k: DIM as u32,
                        x_stride: DIM as u32,
                        out_stride: MOE_DIM as u32,
                    },
                    job.rows,
                    Order::Overlap,
                )?;
            }
        }
        self.commands.go(
            self.k.swiglu_quant,
            SwigluArgs {
                out: shared_mid,
                gate: shared_gate,
                up: shared_up,
                weights: 0,
                dim: MOE_DIM as u32,
                pad: 0,
            },
            MOE_DIM / 32,
            m,
            Order::After,
        )?;
        for job in &jobs {
            self.commands.go(
                self.k.swiglu_quant,
                SwigluArgs {
                    out: job.mid,
                    gate: job.gate,
                    up: job.up,
                    weights: job.weights,
                    dim: MOE_DIM as u32,
                    pad: 0,
                },
                MOE_DIM / 32,
                job.rows,
                Order::Overlap,
            )?;
        }
        self.gemm(
            self.k.gemm_fp8,
            fp8(
                shared_out,
                shared_mid,
                shared[1].0,
                shared[1].1,
                DIM,
                MOE_DIM,
            ),
            m,
            Order::After,
        )?;
        for job in &jobs {
            self.gemm(
                self.k.gemm_fp4,
                GemmArgs {
                    out: job.out,
                    x: job.mid,
                    weight: job.mats[1].0,
                    scale: job.mats[1].1,
                    n: DIM as u32,
                    k: MOE_DIM as u32,
                    x_stride: MOE_DIM as u32,
                    out_stride: DIM as u32,
                },
                job.rows,
                Order::Overlap,
            )?;
        }
        // Sum the outputs: in one pass for a single token, else row tables, one after another.
        if m == 1 {
            let mut sources = Vec::with_capacity(jobs.len() + 1);
            sources.push(shared_out);
            sources.extend(jobs.iter().map(|job| job.out));
            let table = self.host.put(&sources)?;
            return self.commands.go(
                self.k.sum_row,
                SumArgs {
                    out,
                    sources: table,
                    count: sources.len() as u32,
                    pad: 0,
                },
                DIM,
                1,
                Order::After,
            );
        }
        self.commands.go(
            self.k.take_rows,
            CopyArgs {
                dst: out,
                src: shared_out,
                table: self.identity,
                dim: DIM as u32,
                add: 0,
            },
            DIM,
            m,
            Order::After,
        )?;
        for job in &jobs {
            self.commands.go(
                self.k.put_rows,
                CopyArgs {
                    dst: out,
                    src: job.out,
                    table: job.table,
                    dim: DIM as u32,
                    add: 1,
                },
                DIM,
                job.rows,
                Order::After,
            )?;
        }
        Ok(())
    }
}

/// Lines of `layer expert uses`, hottest first.
fn parse_hot_experts(text: &str) -> Result<Vec<(usize, usize, u32)>> {
    let mut hot = Vec::new();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let parsed = (|| {
            let layer = fields.next()?.parse().ok()?;
            let expert = fields.next()?.parse().ok()?;
            let uses = fields.next()?.parse().ok()?;
            Some((layer, expert, uses))
        })();
        let Some(entry) = parsed else {
            return invalid(format!("bad hot expert line {line:?}"));
        };
        hot.push(entry);
    }
    Ok(hot)
}

const fn fp8(out: u64, x: u64, weight: u64, scale: u64, n: usize, k: usize) -> GemmArgs {
    GemmArgs {
        out,
        x,
        weight,
        scale,
        n: n as u32,
        k: k as u32,
        x_stride: k as u32,
        out_stride: n as u32,
    }
}

fn alloc_scratch(gpu: &mut Gpu) -> Result<Scratch> {
    let mut rows = |rows: usize, dim: usize| -> Result<Buffer> {
        gpu.alloc((rows * dim * 4) as u64, Memory::VramVisible, false)
    };
    let c = MAX_CHUNK;
    Ok(Scratch {
        h: [rows(c, HC * DIM)?, rows(c, HC * DIM)?],
        mix_raw: rows(c * MIX_FRAGS, MIX + 1)?,
        mix_attn: rows(c, MIX)?,
        mix_ffn: [rows(c, MIX)?, rows(c, MIX)?],
        x: rows(c, DIM)?,
        xq: rows(c, DIM)?,
        qr: rows(c, Q_LORA)?,
        qrq: rows(c, Q_LORA)?,
        q: rows(c, HEADS * HEAD_DIM)?,
        kv_new: rows(c, HEAD_DIM)?,
        scores: rows(c, HEADS * MAX_SOURCES)?,
        o: rows(c, HEADS * HEAD_DIM)?,
        flat: rows(c, GROUPS * O_LORA)?,
        out: rows(c, DIM)?,
        norm_partial: rows(c, MAX_NORM_FRAGS)?,
        index_q: rows(c, INDEX_HEADS * INDEX_DIM)?,
        index_w: rows(c, INDEX_HEADS)?,
        latent: rows(c, HEAD_DIM)?,
        keys: rows(c, INDEX_DIM)?,
        pair_kv: rows(c, HEAD_DIM)?,
        pair_score: rows(c, HEAD_DIM)?,
        engram_x: rows(c, HASH_COLS * 256)?,
        engram_kv: rows(c, (HC + 1) * DIM)?,
        experts: rows(c * (ACTIVE + 1), EXPERT_FLOATS)?,
    })
}
