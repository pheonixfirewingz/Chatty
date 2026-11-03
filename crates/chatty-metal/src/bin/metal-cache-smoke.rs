//! Hardware verification of Step 3 (`store.rs`): read-only `O_DIRECT` streaming into VRAM
//! `CoreStore`, strictly exclusive `VRAM` + `RAM` `ExpertCache` (including GPU-driven `copy_bytes`
//! `VRAM <-> RAM` swap byte-exactness), and 8-way set-associative `EngramCache`.
#![deny(unsafe_code)]

use chatty_metal::{
    catalog::{Catalog, EXPERT_EXTENTS},
    kfd::Gpu,
    plan::{Budget, GIB, Plan},
    store::{
        CoreStore, DEFAULT_IO_WORKERS, ENGRAM_SKIP_ROW, EngramCache, ExpertCache, ExpertSource,
        ExpertTier, GpuCopier, Reader,
    },
};
use rustix::process::getuid;
use std::{collections::BTreeMap, path::Path, process::ExitCode, time::Instant};

const DEBUG_MODEL: &str = "/debug-model";
const DEFAULT_MODEL: &str = "/model/digitech/models/DeepSeek-V4.1-Flash-1017-FP8";

fn main() -> ExitCode {
    match run() {
        Ok(()) => {
            println!("PASS");
            ExitCode::SUCCESS
        }
        Err(error) => {
            println!("FAIL: {error}");
            ExitCode::FAILURE
        }
    }
}

fn resolve_model_dir(arg: Option<&str>) -> Result<&Path, String> {
    if let Some(p) = arg {
        let path = Path::new(p);
        if path.exists() {
            return Ok(path);
        }
        return Err(format!("model path {p} does not exist"));
    }
    for candidate in [DEBUG_MODEL, DEFAULT_MODEL] {
        let p = Path::new(candidate);
        if p.exists() {
            return Ok(p);
        }
    }
    Err("neither /debug-model nor /model/... exists".into())
}

fn inspect_core(catalog: &Catalog) {
    println!(
        "core_tensors={} total_bytes={} ({:.2} GiB)",
        catalog.core.len(),
        catalog.sizes.core,
        catalog.sizes.core as f64 / GIB as f64
    );
    // Print all non-layer tensors and all layer 0/1/2/14/20 tensors to see every pattern.
    let mut layer_counts: BTreeMap<String, usize> = BTreeMap::new();
    for (name, tensor) in &catalog.core {
        if let Some(rest) = name.strip_prefix("layers.")
            && let Some((l_str, suffix)) = rest.split_once('.')
            && let Ok(l) = l_str.parse::<usize>()
        {
            *layer_counts
                .entry(format!("{suffix} {:?} {:?}", tensor.dtype, tensor.shape))
                .or_default() += 1;
            if matches!(l, 0 | 1 | 2 | 14 | 20) {
                println!(
                    "  {name}: {:?} {:?} ({} B)",
                    tensor.dtype, tensor.shape, tensor.extent.len
                );
            }
        } else {
            println!(
                "  {name}: {:?} {:?} ({} B)",
                tensor.dtype, tensor.shape, tensor.extent.len
            );
        }
    }
    println!("unique_layer_patterns={}:", layer_counts.len());
    for (pat, count) in layer_counts {
        println!("  [{count}x] {pat}");
    }
}

fn read_expected_expert(
    catalog: &Catalog,
    reader: &Reader,
    layer: usize,
    expert: usize,
) -> Result<Vec<u8>, String> {
    let extents = catalog
        .expert(layer, expert)
        .ok_or_else(|| format!("missing expert ({layer}, {expert})"))?;
    let total: usize = extents.iter().map(|e| e.len as usize).sum();
    let mut buf = vec![0u8; total];
    let mut offset = 0usize;
    for ext in extents {
        let len = ext.len as usize;
        reader
            .read_extent(*ext, &mut buf[offset..offset + len])
            .map_err(|e| e.to_string())?;
        offset += len;
    }
    Ok(buf)
}

fn run() -> Result<(), String> {
    if getuid().is_root() {
        return Err("refusing to run as root".into());
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(String::as_str);
    let path_arg = if mode == Some("inspect-core") {
        args.get(1).map(String::as_str)
    } else {
        mode
    };
    let model_dir = resolve_model_dir(path_arg)?;

    let t0 = Instant::now();
    let catalog = Catalog::open(model_dir).map_err(|e| e.to_string())?;
    println!(
        "catalog opened in {:.1} ms: {} shards, {} core tensors ({:.2} GiB), {} experts (slot {:.2} MiB)",
        t0.elapsed().as_secs_f64() * 1000.0,
        catalog.shards.len(),
        catalog.core.len(),
        catalog.sizes.core as f64 / GIB as f64,
        catalog.expert_count(),
        catalog.expert_slot as f64 / (1u64 << 20) as f64,
    );

    if mode == Some("inspect-core") {
        inspect_core(&catalog);
        return Ok(());
    }

    let reader = Reader::open(&catalog.shards, DEFAULT_IO_WORKERS).map_err(|e| e.to_string())?;
    println!(
        "reader opened: workers={DEFAULT_IO_WORKERS}, direct_io={}",
        reader.using_direct_io()
    );
    if !reader.using_direct_io() {
        return Err("expected O_DIRECT on NVMe model shards".into());
    }

    let mut gpu = Gpu::open().map_err(|e| e.to_string())?;
    let mut queue = gpu.create_queue().map_err(|e| e.to_string())?;
    let copier = GpuCopier::new(&mut gpu).map_err(|e| e.to_string())?;
    println!(
        "gpu ready: id={:#x}, gfx={}, queue={}",
        gpu.gpu_id, gpu.node.gfx_target_version, queue.id
    );

    // 1. Load entire 9.17 GiB CoreStore into chunked VRAM and verify cross-BO byte integrity via GPU DMA!
    let t_core = Instant::now();
    let core = CoreStore::load(&catalog, &reader, &mut gpu).map_err(|e| e.to_string())?;
    let core_secs = t_core.elapsed().as_secs_f64();
    println!(
        "core_store loaded: {:.2} GiB across {} VRAM BO slices in {:.3} s ({:.2} GB/s)",
        core.total_bytes() as f64 / GIB as f64,
        core.vram_slices(),
        core_secs,
        catalog.sizes.core as f64 / core_secs / 1e9,
    );

    {
        let mut xfer = copier.transfer(&gpu, &mut queue);
        // Check largest tensor head + tail (crosses multiple 256 MiB VRAM BO boundaries!)
        let (big_name, big_loaded) = core
            .iter()
            .max_by_key(|(_, t)| t.len)
            .ok_or("core store is empty")?;
        let big_spec = &catalog.core[big_name];
        let window = (64usize << 10).min(big_loaded.len as usize);
        let mut gpu_head = vec![0u8; window];
        let mut disk_head = vec![0u8; window];
        xfer.read_device_bytes(big_loaded.va, &mut gpu_head)
            .map_err(|e| e.to_string())?;
        let mut head_ext = big_spec.extent;
        head_ext.len = window as u64;
        reader
            .read_extent(head_ext, &mut disk_head)
            .map_err(|e| e.to_string())?;
        if gpu_head != disk_head {
            return Err(format!("{big_name} head mismatch in VRAM"));
        }

        let tail_off = big_loaded.len - window as u64;
        let mut gpu_tail = vec![0u8; window];
        let mut disk_tail = vec![0u8; window];
        xfer.read_device_bytes(big_loaded.va + tail_off, &mut gpu_tail)
            .map_err(|e| e.to_string())?;
        let mut tail_ext = big_spec.extent;
        tail_ext.offset += tail_off;
        tail_ext.len = window as u64;
        reader
            .read_extent(tail_ext, &mut disk_tail)
            .map_err(|e| e.to_string())?;
        if gpu_tail != disk_tail {
            return Err(format!(
                "{big_name} tail mismatch across 256 MiB VRAM slices"
            ));
        }
        println!(
            "core_store verified: {big_name} ({:.2} MiB) head+tail exact on GPU across BO slices",
            big_loaded.len as f64 / (1u64 << 20) as f64
        );
    }

    // 2. Live GPU VRAM <-> RAM expert swap torture test (1 VRAM slot, 2 RAM slots).
    {
        let mut swap_plan = Plan::new(&catalog, Budget::default()).map_err(|e| e.to_string())?;
        swap_plan.vram_expert_slots = 1;
        swap_plan.ram_expert_slots = 2;
        let mut swap_cache =
            ExpertCache::new(&catalog, &swap_plan, &mut gpu).map_err(|e| e.to_string())?;
        let mut xfer = copier.transfer(&gpu, &mut queue);

        let v0 = swap_cache
            .acquire_layer(0, &[0], &reader, &mut xfer)
            .map_err(|e| e.to_string())?;
        if v0[0].source != ExpertSource::ColdToVram {
            return Err(format!("expected ColdToVram, got {:?}", v0[0].source));
        }
        let v1 = swap_cache
            .acquire_layer(0, &[1, 2], &reader, &mut xfer)
            .map_err(|e| e.to_string())?;
        if v1[0].source != ExpertSource::ColdToRamAndHost
            || v1[1].source != ExpertSource::ColdToRamAndHost
        {
            return Err("expected ColdToRamAndHost for RAM spill".into());
        }

        // Hit (0, 1) twice: first stages to Host, second triggers GPU VRAM->Host[0]->RAM swap!
        let _ = swap_cache
            .acquire_layer(0, &[1], &reader, &mut xfer)
            .map_err(|e| e.to_string())?;
        let t_swap = Instant::now();
        let swapped = swap_cache
            .acquire_layer(0, &[1], &reader, &mut xfer)
            .map_err(|e| e.to_string())?;
        let swap_ms = t_swap.elapsed().as_secs_f64() * 1000.0;
        if swapped[0].source != ExpertSource::RamSwappedToVram {
            return Err(format!(
                "expected RamSwappedToVram, got {:?}",
                swapped[0].source
            ));
        }
        if swap_cache.tier_of(0, 1) != Some(ExpertTier::Vram)
            || swap_cache.tier_of(0, 0) != Some(ExpertTier::Ram)
        {
            return Err("tier exclusivity violated after VRAM <-> RAM swap".into());
        }

        // Read back ALL 18,800,640 bytes of promoted (0, 1) in VRAM AND demoted (0, 0) in RAM!
        let slot_len = swap_cache.slot_bytes();
        let mut gpu_promoted = vec![0u8; slot_len];
        xfer.read_device_bytes(swapped[0].base_va, &mut gpu_promoted)
            .map_err(|e| e.to_string())?;
        let exp_01 = read_expected_expert(&catalog, &reader, 0, 1)?;
        if gpu_promoted != exp_01 {
            return Err("promoted expert (0, 1) bytes corrupted in VRAM".into());
        }

        let demoted = swap_cache
            .acquire_layer(0, &[0], &reader, &mut xfer)
            .map_err(|e| e.to_string())?;
        let mut gpu_demoted = vec![0u8; slot_len];
        xfer.read_device_bytes(demoted[0].base_va, &mut gpu_demoted)
            .map_err(|e| e.to_string())?;
        let exp_00 = read_expected_expert(&catalog, &reader, 0, 0)?;
        if gpu_demoted != exp_00 {
            return Err("demoted expert (0, 0) bytes corrupted during VRAM->Host->RAM DMA".into());
        }
        println!(
            "expert_swap verified: 18.80 MiB bidirectional VRAM<->RAM swap in {:.2} ms, all {} bytes exact!",
            swap_ms,
            slot_len * 2
        );
    }

    // 3. Full production cache layout (595 VRAM + 2,627 RAM experts + 12 GiB EngramCache)!
    let plan = Plan::new(&catalog, Budget::default()).map_err(|e| e.to_string())?;
    let mut experts = ExpertCache::new(&catalog, &plan, &mut gpu).map_err(|e| e.to_string())?;
    let mut engram = EngramCache::new(&catalog, &plan).map_err(|e| e.to_string())?;
    println!(
        "production caches allocated: expert_vram={} slots, expert_ram={} slots, engram={} sets ({} rows)",
        plan.vram_expert_slots,
        plan.ram_expert_slots,
        engram.num_sets(),
        engram.capacity_rows(),
    );

    {
        let mut xfer = copier.transfer(&gpu, &mut queue);
        let active = [0usize, 63, 127, 191, 255, 383];
        let t_cold = Instant::now();
        let views_cold = experts
            .acquire_layer(7, &active, &reader, &mut xfer)
            .map_err(|e| e.to_string())?;
        let cold_ms = t_cold.elapsed().as_secs_f64() * 1000.0;
        if views_cold.len() != active.len()
            || views_cold.iter().any(|v| v.tensors.len() != EXPERT_EXTENTS)
        {
            return Err("unexpected expert views".into());
        }

        let t_warm = Instant::now();
        let views_warm = experts
            .acquire_layer(7, &active, &reader, &mut xfer)
            .map_err(|e| e.to_string())?;
        let warm_us = t_warm.elapsed().as_secs_f64() * 1e6;
        if views_warm.iter().any(|v| v.source != ExpertSource::VramHit) {
            return Err("expected 6/6 VramHit on second acquire".into());
        }
        println!(
            "expert_cache 6-expert layer: cold={:.2} ms ({:.2} GB/s), warm_vram_hit={:.2} us",
            cold_ms,
            (active.len() as u64 * catalog.expert_slot) as f64 / (cold_ms / 1000.0) / 1e9,
            warm_us,
        );
    }

    if !catalog.engram.is_empty() {
        let rows_max = catalog.engram[0].rows;
        let step = rows_max / 25;
        let mut query: Vec<u64> = (0..24).map(|i| (i as u64 + 1) * step).collect();
        query.push(ENGRAM_SKIP_ROW);
        let mut out_cold = vec![0u8; query.len() * engram.row_bytes()];
        let t_ecold = Instant::now();
        let st_cold = engram
            .lookup_batch(0, &query, &mut out_cold, &reader)
            .map_err(|e| e.to_string())?;
        let ecold_us = t_ecold.elapsed().as_secs_f64() * 1e6;

        let mut out_warm = vec![0xAAu8; query.len() * engram.row_bytes()];
        let t_ewarm = Instant::now();
        let st_warm = engram
            .lookup_batch(0, &query, &mut out_warm, &reader)
            .map_err(|e| e.to_string())?;
        let ewarm_us = t_ewarm.elapsed().as_secs_f64() * 1e6;

        if st_cold.misses != 24 || st_warm.hits != 24 || out_cold != out_warm {
            return Err("engram warm lookup mismatch".into());
        }
        if out_warm[24 * engram.row_bytes()..].iter().any(|&b| b != 0) {
            return Err("ENGRAM_SKIP_ROW did not zero output slice".into());
        }
        // Verify row 0 of query against direct shard read.
        let (w_ext, s_ext) = catalog.engram[0].row(query[0]).ok_or("bad engram row")?;
        let mut direct_row = vec![0u8; engram.row_bytes()];
        reader
            .read_extent(w_ext, &mut direct_row[..engram.weight_bytes()])
            .map_err(|e| e.to_string())?;
        reader
            .read_extent(s_ext, &mut direct_row[engram.weight_bytes()..])
            .map_err(|e| e.to_string())?;
        if out_warm[..engram.row_bytes()] != direct_row[..] {
            return Err("engram cached bytes do not match shard".into());
        }
        println!(
            "engram_cache 24-row batch: cold={:.1} us, warm_ram_hit={:.2} us (byte-exact)",
            ecold_us, ewarm_us
        );
    }

    Ok(())
}
