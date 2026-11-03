//! Read-only NVMe benchmark job, run on the appliance with `metal_run`: measures real
//! sequential, per-expert, multi-expert burst, and random engram-row read throughput on
//! the Samsung 970 EVO Plus (`/dev/nvme0n1`) against the actual safetensors shards.
//!
//! Strictly read-only: refuses to run if privileged, opens shards only with `O_RDONLY`,
//! and never writes to any file or device.

use chatty_metal::catalog::{Catalog, EXPERT_EXTENTS, Extent};
use rustix::{
    fs::{Advice, Mode, OFlags, fadvise, open},
    mm::{MapFlags, ProtFlags, mmap_anonymous, munmap},
    process::getuid,
};
use std::{
    fs::{self, File},
    os::unix::fs::FileExt,
    path::Path,
    process::ExitCode,
    slice,
    sync::atomic::{AtomicUsize, Ordering},
    thread,
    time::{Duration, Instant},
};

const DEBUG_MODEL: &str = "/debug-model";
const DEFAULT_MODEL: &str = "/model/digitech/models/DeepSeek-V4.1-Flash-1017-FP8";
const PAGE: usize = 4096;
const MIB: f64 = (1u64 << 20) as f64;
const GIB: f64 = (1u64 << 30) as f64;

struct AlignedBuf {
    ptr: *mut u8,
    len: usize,
}

impl AlignedBuf {
    fn new(len: usize) -> Result<Self, String> {
        let len = len.max(PAGE).next_multiple_of(PAGE);
        // SAFETY: anonymous private mapping chooses a fresh page-aligned address.
        let ptr = unsafe {
            mmap_anonymous(
                std::ptr::null_mut(),
                len,
                ProtFlags::READ | ProtFlags::WRITE,
                MapFlags::PRIVATE,
            )
        }
        .map_err(|e| format!("mmap_anonymous({len}): {e}"))?
        .cast::<u8>();
        Ok(Self { ptr, len })
    }

    fn as_mut_slice(&mut self, len: usize) -> &mut [u8] {
        assert!(len <= self.len);
        // SAFETY: `ptr..ptr + self.len` is uniquely owned by `self` and initialized by `mmap_anonymous`.
        unsafe { slice::from_raw_parts_mut(self.ptr, len) }
    }
}

impl Drop for AlignedBuf {
    fn drop(&mut self) {
        // SAFETY: matches the exact address and length returned by `mmap_anonymous`.
        let _ = unsafe { munmap(self.ptr.cast(), self.len) };
    }
}

/// Coalesced page-aligned read span covering one or more adjacent extents in the same shard.
#[derive(Clone, Copy, Debug)]
struct AlignedSpan {
    shard: u16,
    aligned_offset: u64,
    aligned_len: usize,
    payload_bytes: u64,
}

fn coalesce_extents(extents: &[Extent]) -> Vec<AlignedSpan> {
    let mut sorted: Vec<Extent> = extents.iter().copied().filter(|e| e.len > 0).collect();
    sorted.sort_by_key(|e| (e.shard, e.offset));
    let mut spans: Vec<AlignedSpan> = Vec::new();
    for ext in sorted {
        let start = ext.offset & !(PAGE as u64 - 1);
        let end = (ext.offset + ext.len).next_multiple_of(PAGE as u64);
        if let Some(last) = spans.last_mut() {
            let last_end = last.aligned_offset + last.aligned_len as u64;
            // Merge if in the same shard and within 64 KiB so one NVMe read covers both.
            if last.shard == ext.shard && start <= last_end + (64 << 10) {
                let merged_end = last_end.max(end);
                last.aligned_len = (merged_end - last.aligned_offset) as usize;
                last.payload_bytes += ext.len;
                continue;
            }
        }
        spans.push(AlignedSpan {
            shard: ext.shard,
            aligned_offset: start,
            aligned_len: (end - start) as usize,
            payload_bytes: ext.len,
        });
    }
    spans
}

fn open_shards(paths: &[std::path::PathBuf], direct: bool) -> Result<Vec<File>, String> {
    let flags = OFlags::RDONLY
        | OFlags::CLOEXEC
        | if direct {
            OFlags::DIRECT
        } else {
            OFlags::empty()
        };
    paths
        .iter()
        .map(|p| {
            open(p, flags, Mode::empty())
                .map(File::from)
                .map_err(|e| format!("open {} (direct={direct}): {e}", p.display()))
        })
        .collect()
}

fn read_exact_at(file: &File, mut buf: &mut [u8], mut offset: u64) -> Result<(), String> {
    while !buf.is_empty() {
        let n = file
            .read_at(buf, offset)
            .map_err(|e| format!("pread at {offset}: {e}"))?;
        if n == 0 {
            return Err(format!("unexpected EOF at offset {offset}"));
        }
        buf = &mut buf[n..];
        offset += n as u64;
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => {
            println!("PASS");
            ExitCode::SUCCESS
        }
        Err(err) => {
            println!("FAIL: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let status = fs::read_to_string("/proc/self/status").unwrap_or_default();
    let effective = status
        .lines()
        .find_map(|line| line.strip_prefix("CapEff:"))
        .and_then(|mask| u64::from_str_radix(mask.trim(), 16).ok());
    if getuid().is_root() || effective != Some(0) {
        return Err("refusing to run with privileges".to_owned());
    }
    let ro = fs::read_to_string("/sys/block/nvme0n1/ro").unwrap_or_default();
    if ro.trim() != "1" {
        return Err(format!(
            "/sys/block/nvme0n1/ro is {:?}, expected \"1\"",
            ro.trim()
        ));
    }

    let arg1 = std::env::args().nth(1).unwrap_or_default();
    let inspect = arg1 == "inspect";
    let model_path = if !arg1.is_empty() && !inspect {
        arg1
    } else if Path::new(DEBUG_MODEL).exists() {
        DEBUG_MODEL.to_owned()
    } else {
        DEFAULT_MODEL.to_owned()
    };
    if inspect {
        for rel in ["inference/config.json", "config.json"] {
            let p = Path::new(&model_path).join(rel);
            println!("=== {} ===", p.display());
            println!(
                "{}",
                fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?
            );
        }
        return Ok(());
    }
    let t_cat = Instant::now();
    let catalog = Catalog::open(Path::new(&model_path)).map_err(|e| e.to_string())?;
    println!(
        "catalog opened in {:?}: {} shards, {} layers, {} routed experts/layer ({} activated), {} total experts",
        t_cat.elapsed(),
        catalog.shards.len(),
        catalog.config.n_layers,
        catalog.config.n_routed_experts,
        catalog.config.n_activated_experts,
        catalog.expert_count(),
    );
    println!(
        "sizes: core {:.2} GiB, experts {:.2} GiB (slot {:.2} MiB = {} B), engram {:.2} GiB ({} tables, head_dim {})",
        catalog.sizes.core as f64 / GIB,
        catalog.sizes.experts as f64 / GIB,
        catalog.expert_slot as f64 / MIB,
        catalog.expert_slot,
        catalog.sizes.engram as f64 / GIB,
        catalog.engram.len(),
        catalog.config.engram_head_dim,
    );

    if let Some(ex0) = catalog.expert(0, 0) {
        let spans = coalesce_extents(ex0);
        println!(
            "expert(0,0) extents: {:?} -> {} coalesced aligned span(s) ({} aligned KiB for {} payload KiB)",
            ex0.map(|e| (e.shard, e.offset, e.len)),
            spans.len(),
            spans.iter().map(|s| s.aligned_len).sum::<usize>() / 1024,
            spans.iter().map(|s| s.payload_bytes).sum::<u64>() / 1024,
        );
    }
    if let Some(table) = catalog.engram.first()
        && let Some((w, s)) = table.row(0)
    {
        println!(
            "engram[0] layer {}: {} rows, row weight {} B + scale {} B",
            table.layer, table.rows, w.len, s.len
        );
    }

    let direct_files = open_shards(&catalog.shards, true)?;
    let buffered_files = open_shards(&catalog.shards, false)?;

    // 1. Sequential O_DIRECT throughput (4 MiB chunks across distinct shards, 2 GiB total).
    for threads in [1usize, 4] {
        let chunk = 4 << 20;
        let total_chunks = 512usize; // 2 GiB
        let next = AtomicUsize::new(0);
        let start = Instant::now();
        thread::scope(|scope| -> Result<(), String> {
            let mut handles = Vec::with_capacity(threads);
            for t in 0..threads {
                let files = &direct_files;
                let next = &next;
                handles.push(scope.spawn(move || -> Result<u64, String> {
                    let mut buf = AlignedBuf::new(chunk)?;
                    let mut bytes = 0u64;
                    loop {
                        let idx = next.fetch_add(1, Ordering::Relaxed);
                        if idx >= total_chunks {
                            break;
                        }
                        // Spread across shards 2..42 at 4 MiB aligned offsets inside the shard body.
                        let shard = 2 + ((idx + t * 7) % (files.len().saturating_sub(4).max(1)));
                        let offset = ((16 + (idx / files.len()) * 4) << 20) as u64;
                        read_exact_at(&files[shard], buf.as_mut_slice(chunk), offset)?;
                        bytes += chunk as u64;
                    }
                    Ok(bytes)
                }));
            }
            for h in handles {
                h.join().map_err(|_| "worker panicked".to_owned())??;
            }
            Ok(())
        })?;
        let elapsed = start.elapsed();
        let total_bytes = (total_chunks * chunk) as f64;
        println!(
            "seq O_DIRECT 4 MiB x {total_chunks} ({threads}T): {:.2} GB/s ({:.2} GiB/s) in {elapsed:?}",
            total_bytes / elapsed.as_secs_f64() / 1e9,
            total_bytes / elapsed.as_secs_f64() / GIB,
        );
    }

    // Build deterministic spread of cold experts across all layers/shards.
    let total_experts = catalog.expert_count();
    let sample_experts: Vec<[Extent; EXPERT_EXTENTS]> = (0..128usize)
        .map(|i| {
            let idx = (i * 9973 + 17) % total_experts;
            let layer = idx / catalog.config.n_routed_experts;
            let expert = idx % catalog.config.n_routed_experts;
            *catalog.expert(layer, expert).unwrap()
        })
        .collect();

    // 2. Single cold expert latency (QD=1): O_DIRECT vs buffered+DONTNEED (32 experts).
    for (label, use_direct) in [("O_DIRECT", true), ("buffered+DONTNEED", false)] {
        let files = if use_direct {
            &direct_files
        } else {
            &buffered_files
        };
        let count = 32usize;
        let offset_base = if use_direct { 0 } else { 32 };
        let max_span = (catalog.expert_slot as usize) + 128 * 1024;
        let mut buf = AlignedBuf::new(max_span)?;
        let mut samples = Vec::with_capacity(count);
        let mut payload_total = 0u64;
        let start = Instant::now();
        for extents in &sample_experts[offset_base..offset_base + count] {
            let t0 = Instant::now();
            let spans = coalesce_extents(extents);
            for span in spans {
                read_exact_at(
                    &files[span.shard as usize],
                    buf.as_mut_slice(span.aligned_len),
                    span.aligned_offset,
                )?;
                if !use_direct {
                    let _ = fadvise(
                        &files[span.shard as usize],
                        span.aligned_offset,
                        std::num::NonZeroU64::new(span.aligned_len as u64),
                        Advice::DontNeed,
                    );
                }
                payload_total += span.payload_bytes;
            }
            samples.push(t0.elapsed());
        }
        let elapsed = start.elapsed();
        samples.sort_unstable();
        println!(
            "expert 1T {label:<17} ({count} cold experts): min {:?}, median {:?}, p99 {:?} | payload {:.2} GB/s ({:.2} GiB/s)",
            samples[0],
            samples[count / 2],
            samples[count * 99 / 100],
            payload_total as f64 / elapsed.as_secs_f64() / 1e9,
            payload_total as f64 / elapsed.as_secs_f64() / GIB,
        );
    }

    // 3. Multi-expert burst (simulating 8 uncached experts in one MoE layer across 2, 4, 8 threads).
    for threads in [2usize, 4, 8] {
        let count = 64usize;
        let slice = &sample_experts[64..64 + count];
        let next = AtomicUsize::new(0);
        let max_span = (catalog.expert_slot as usize) + 128 * 1024;
        let start = Instant::now();
        let payload_total: u64 = thread::scope(|scope| -> Result<u64, String> {
            let mut handles = Vec::with_capacity(threads);
            for _ in 0..threads {
                let files = &direct_files;
                let next = &next;
                handles.push(scope.spawn(move || -> Result<u64, String> {
                    let mut buf = AlignedBuf::new(max_span)?;
                    let mut bytes = 0u64;
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        if i >= slice.len() {
                            break;
                        }
                        for span in coalesce_extents(&slice[i]) {
                            read_exact_at(
                                &files[span.shard as usize],
                                buf.as_mut_slice(span.aligned_len),
                                span.aligned_offset,
                            )?;
                            bytes += span.payload_bytes;
                        }
                    }
                    Ok(bytes)
                }));
            }
            let mut sum = 0u64;
            for h in handles {
                sum += h.join().map_err(|_| "worker panicked".to_owned())??;
            }
            Ok(sum)
        })?;
        let elapsed = start.elapsed();
        let per_8_experts = Duration::from_secs_f64(elapsed.as_secs_f64() * 8.0 / count as f64);
        println!(
            "expert {threads}T O_DIRECT ({count} experts): 8-expert burst {:?} ({:.2} ms/expert eff) | payload {:.2} GB/s ({:.2} GiB/s)",
            per_8_experts,
            elapsed.as_secs_f64() * 1000.0 / count as f64,
            payload_total as f64 / elapsed.as_secs_f64() / 1e9,
            payload_total as f64 / elapsed.as_secs_f64() / GIB,
        );
    }

    // 4. Random cold engram row reads (weight + scale) across all engram tables.
    if !catalog.engram.is_empty() {
        for threads in [1usize, 8, 32] {
            let rows_to_read = if threads == 1 { 512usize } else { 2048usize };
            let next = AtomicUsize::new(0);
            let start = Instant::now();
            thread::scope(|scope| -> Result<(), String> {
                let mut handles = Vec::with_capacity(threads);
                for _ in 0..threads {
                    let files = &direct_files;
                    let tables = &catalog.engram;
                    let next = &next;
                    handles.push(scope.spawn(move || -> Result<(), String> {
                        let mut buf = AlignedBuf::new(64 << 10)?;
                        loop {
                            let i = next.fetch_add(1, Ordering::Relaxed);
                            if i >= rows_to_read {
                                break;
                            }
                            let t = &tables[i % tables.len()];
                            let row = ((i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)) % t.rows;
                            let (w, s) = t.row(row).unwrap();
                            for span in coalesce_extents(&[w, s]) {
                                read_exact_at(
                                    &files[span.shard as usize],
                                    buf.as_mut_slice(span.aligned_len),
                                    span.aligned_offset,
                                )?;
                            }
                        }
                        Ok(())
                    }));
                }
                for h in handles {
                    h.join().map_err(|_| "worker panicked".to_owned())??;
                }
                Ok(())
            })?;
            let elapsed = start.elapsed();
            let iops = rows_to_read as f64 / elapsed.as_secs_f64();
            let us_per_row = elapsed.as_secs_f64() * 1e6 / rows_to_read as f64;
            println!(
                "engram {threads:>2}T O_DIRECT ({rows_to_read} cold rows, w+s): {iops:.0} rows/s ({us_per_row:.1} µs/row eff)",
            );
        }
    }

    Ok(())
}
