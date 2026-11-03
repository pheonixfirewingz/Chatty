//! Read-only NVMe weight streaming and exclusive VRAM + RAM expert and engram caches.
//!
//! Hardware constraints measured on the target appliance (`192.168.0.97`):
//! - Samsung 970 EVO Plus reaches `3.55 GB/s` (`3.31 GiB/s`) at 4 `O_DIRECT` threads and
//!   `120,140` random engram rows/s, while buffered I/O is 2.75x slower and pollutes RAM.
//! - Linux TTM caps `GTT` at 50% of system RAM (`31.36 GiB`), so the 46 GiB RAM expert cache
//!   and 12 GiB engram cache live in anonymous `MAP_NORESERVE` mappings rather than KFD GTT.
//! - `Memory::VramVisible` (24 GiB resizable BAR) accepts fast write-combined CPU uploads and
//!   gets full GPU Infinity Cache bandwidth, but CPU reads from WC BAR memory crawl at ~50 MB/s.
//!   Therefore the CPU never reads VRAM: when a hot RAM expert promotes into a full VRAM tier,
//!   a GPU `copy_bytes` kernel DMAs the evicted VRAM victim into a coherent `Memory::Host`
//!   staging slot over PCIe (`~14 GB/s`) before copying it into the vacated RAM slot.

use crate::{
    Error, Result,
    catalog::{Catalog, Dtype, EXPERT_EXTENTS, EngramTable, Extent},
    invalid,
    kfd::{Buffer, Dispatch, Gpu, Memory, Module, Queue, SIGNAL_BYTES, signal_reset, signal_wait},
    plan::Plan,
};
use rustix::{
    fs::{Advice, Mode, OFlags, fadvise, open},
    io::Errno,
    mm::{MapFlags, ProtFlags, mmap_anonymous, munmap},
};
use std::{
    collections::{BTreeMap, HashMap},
    fs::File,
    io,
    ops::Range,
    os::unix::fs::FileExt,
    path::{Path, PathBuf},
    ptr, slice,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

const STORE_HSACO: &[u8] = include_bytes!("../shaders/store.hsaco");

/// Default `O_DIRECT` worker count (4 threads saturate PCIe 3.0 x4 at 3.55 GB/s).
pub const DEFAULT_IO_WORKERS: usize = 4;
const DIRECT_ALIGN: u64 = 4096;
const CHUNK_BYTES: u64 = 4 << 20;
const COALESCE_GAP: u64 = 64 << 10;

/// Maximum single TTM VRAM BO size; contiguous virtual ranges are stitched from 256 MiB slices.
const VRAM_SLICE_BYTES: u64 = 256 << 20;
const TENSOR_ALIGN: u64 = 256;
const SLOT_ALIGN: u64 = 256;

/// Slot 0 in the `Memory::Host` staging buffer is reserved for VRAM -> Host demotion DMA;
/// slots `1..STAGING_SLOTS` hold unpromoted RAM/cold experts for the current layer dispatch.
const STAGING_SLOTS: usize = 17;
/// Minimum hits in RAM before an expert tries to displace a colder VRAM resident.
const PROMOTE_MIN_HITS: u32 = 2;

/// Sentinel row index in [`EngramCache::lookup_batch`] that writes zeros without disk I/O.
pub const ENGRAM_SKIP_ROW: u64 = u64::MAX;
const ENGRAM_WAYS: usize = 8;

/// Anonymous page-aligned mapping released via `munmap` on drop.
struct AnonMap {
    ptr: *mut u8,
    len: usize,
}

// SAFETY: `AnonMap` uniquely owns its anonymous mapping for its lifetime.
unsafe impl Send for AnonMap {}
// SAFETY: Shared references only allow synchronized or disjoint byte access.
unsafe impl Sync for AnonMap {}

impl AnonMap {
    fn new(len: usize, noreserve: bool) -> Result<Self> {
        if len == 0 {
            return Ok(Self {
                ptr: ptr::NonNull::dangling().as_ptr(),
                len: 0,
            });
        }
        let mut flags = MapFlags::PRIVATE;
        if noreserve {
            flags |= MapFlags::NORESERVE;
        }
        // SAFETY: anonymous mapping with kernel-chosen address; length is non-zero.
        let ptr = unsafe {
            mmap_anonymous(
                ptr::null_mut(),
                len,
                ProtFlags::READ | ProtFlags::WRITE,
                flags,
            )
        }
        .map_err(|err| os_err("mmap_anonymous", err))?
        .cast::<u8>();
        Ok(Self { ptr, len })
    }

    fn as_ptr(&self) -> *mut u8 {
        self.ptr
    }

    fn slice(&self, offset: usize, len: usize) -> &[u8] {
        assert!(offset.checked_add(len).is_some_and(|end| end <= self.len));
        // SAFETY: bounds checked above, and caller holds `&self`.
        unsafe { slice::from_raw_parts(self.ptr.add(offset), len) }
    }

    fn slice_mut(&mut self, offset: usize, len: usize) -> &mut [u8] {
        assert!(offset.checked_add(len).is_some_and(|end| end <= self.len));
        // SAFETY: bounds checked above, and caller holds `&mut self`.
        unsafe { slice::from_raw_parts_mut(self.ptr.add(offset), len) }
    }
}

impl Drop for AnonMap {
    fn drop(&mut self) {
        if self.len > 0 {
            // SAFETY: `ptr..ptr + len` was mapped by `mmap_anonymous` in `Self::new`.
            unsafe {
                let _ = munmap(self.ptr.cast(), self.len);
            }
        }
    }
}

fn os_err(name: &str, err: Errno) -> Error {
    Error::Io(
        PathBuf::from(name),
        io::Error::from_raw_os_error(err.raw_os_error()),
    )
}

#[derive(Clone, Copy)]
struct SubReadOp {
    shard: u16,
    offset: u64,
    len: usize,
    dst: usize,
}

#[derive(Clone)]
struct ChunkTask {
    shard: u16,
    aligned_offset: u64,
    aligned_len: usize,
    required_len: usize,
    ops: Range<usize>,
}

struct ShardHandle {
    path: PathBuf,
    direct: Option<File>,
    buffered: File,
}

impl ShardHandle {
    fn open(path: &Path) -> Result<Self> {
        let io_err = |e| Error::Io(path.to_path_buf(), e);
        let buffered = File::open(path).map_err(io_err)?;
        let direct = match open(
            path,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::DIRECT,
            Mode::empty(),
        ) {
            Ok(fd) => Some(File::from(fd)),
            Err(Errno::INVAL | Errno::OPNOTSUPP) => None,
            Err(e) => return Err(io_err(io::Error::from_raw_os_error(e.raw_os_error()))),
        };
        Ok(Self {
            path: path.to_path_buf(),
            direct,
            buffered,
        })
    }

    fn execute(&self, task: &ChunkTask, sub_ops: &[SubReadOp], bounce: &mut [u8]) -> Result<()> {
        let buf = &mut bounce[..task.aligned_len];
        let used_direct = if let Some(direct) = &self.direct {
            match pread_until(direct, buf, task.aligned_offset, task.required_len) {
                Ok(()) => true,
                Err(e) if e.raw_os_error() == Some(libc_einval()) => false,
                Err(e) => return Err(Error::Io(self.path.clone(), e)),
            }
        } else {
            false
        };
        if !used_direct {
            pread_until(
                &self.buffered,
                &mut buf[..task.required_len],
                task.aligned_offset,
                task.required_len,
            )
            .map_err(|e| Error::Io(self.path.clone(), e))?;
            let _ = fadvise(
                &self.buffered,
                task.aligned_offset,
                Some(
                    std::num::NonZeroU64::new(task.aligned_len as u64)
                        .expect("chunk length is non-zero"),
                ),
                Advice::DontNeed,
            );
        }
        for op in &sub_ops[task.ops.clone()] {
            let rel = (op.offset - task.aligned_offset) as usize;
            // SAFETY: `task.required_len` covers `rel..rel + op.len` inside `buf`, and each
            // `SubReadOp` owns a disjoint destination slice alive until `read_raw` finishes.
            unsafe {
                ptr::copy_nonoverlapping(buf.as_ptr().add(rel), op.dst as *mut u8, op.len);
            }
        }
        Ok(())
    }
}

const fn libc_einval() -> i32 {
    Errno::INVAL.raw_os_error()
}

fn pread_until(file: &File, dst: &mut [u8], offset: u64, required: usize) -> io::Result<()> {
    let mut done = 0usize;
    while done < dst.len() {
        let n = file.read_at(&mut dst[done..], offset + done as u64)?;
        if n == 0 {
            break;
        }
        done += n;
    }
    if done < required {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!("short shard read at offset {offset}: got {done} of {required} bytes"),
        ));
    }
    Ok(())
}

struct ActiveBatch {
    generation: u64,
    tasks_ptr: usize,
    tasks_len: usize,
    ops_ptr: usize,
    ops_len: usize,
    remaining_workers: usize,
    first_error: Option<Error>,
}

struct PoolShared {
    shards: Vec<ShardHandle>,
    cursor: AtomicUsize,
    state: Mutex<PoolState>,
    work_cv: Condvar,
    done_cv: Condvar,
}

struct PoolState {
    shutdown: bool,
    batch: Option<ActiveBatch>,
}

impl PoolShared {
    fn drain_tasks(&self, bounce: &mut [u8], tasks: &[ChunkTask], ops: &[SubReadOp]) -> Result<()> {
        loop {
            let idx = self.cursor.fetch_add(1, Ordering::Relaxed);
            if idx >= tasks.len() {
                return Ok(());
            }
            let task = &tasks[idx];
            let shard = self
                .shards
                .get(usize::from(task.shard))
                .ok_or_else(|| Error::Invalid(format!("shard {} out of range", task.shard)))?;
            shard.execute(task, ops, bounce)?;
        }
    }
}

/// Read-only parallel `O_DIRECT` reader over the model's safetensors shards.
pub struct Reader {
    shared: Arc<PoolShared>,
    caller_bounce: Mutex<AnonMap>,
    workers: Vec<JoinHandle<()>>,
    next_generation: Mutex<u64>,
}

impl Reader {
    /// Open every shard strictly read-only (`O_DIRECT` when the filesystem supports it) and
    /// spawn `workers.max(1) - 1` persistent background I/O threads (the caller acts as worker 0).
    pub fn open(shards: &[PathBuf], workers: usize) -> Result<Self> {
        let mut handles = Vec::with_capacity(shards.len());
        for path in shards {
            handles.push(ShardHandle::open(path)?);
        }
        let caller_bounce = AnonMap::new(CHUNK_BYTES as usize, false)?;
        let bg_count = workers.max(1) - 1;
        let mut worker_bounces = Vec::with_capacity(bg_count);
        for _ in 0..bg_count {
            worker_bounces.push(AnonMap::new(CHUNK_BYTES as usize, false)?);
        }

        let shared = Arc::new(PoolShared {
            shards: handles,
            cursor: AtomicUsize::new(0),
            state: Mutex::new(PoolState {
                shutdown: false,
                batch: None,
            }),
            work_cv: Condvar::new(),
            done_cv: Condvar::new(),
        });

        let mut threads = Vec::with_capacity(bg_count);
        for mut bounce in worker_bounces {
            let pool = Arc::clone(&shared);
            let handle = thread::Builder::new()
                .name("metal-nvme".into())
                .spawn(move || worker_loop(&pool, &mut bounce))
                .map_err(|e| Error::Io(PathBuf::from("spawn metal-nvme"), e))?;
            threads.push(handle);
        }

        Ok(Self {
            shared,
            caller_bounce: Mutex::new(caller_bounce),
            workers: threads,
            next_generation: Mutex::new(1),
        })
    }

    /// True when every shard opened with `O_DIRECT`.
    pub fn using_direct_io(&self) -> bool {
        !self.shared.shards.is_empty() && self.shared.shards.iter().all(|s| s.direct.is_some())
    }

    /// Read a single extent into `dst`.
    pub fn read_extent(&self, extent: Extent, dst: &mut [u8]) -> Result<()> {
        if dst.len() as u64 != extent.len {
            return invalid(format!(
                "buffer length {} does not match extent length {}",
                dst.len(),
                extent.len
            ));
        }
        self.read_raw(&[(extent, dst.as_mut_ptr())])
    }

    /// Read multiple disjoint `(Extent, &mut [u8])` pairs in one coalesced parallel batch.
    pub fn read_extents(&self, items: &mut [(Extent, &mut [u8])]) -> Result<()> {
        let mut raw = Vec::with_capacity(items.len());
        for (extent, slice) in items.iter_mut() {
            if slice.len() as u64 != extent.len {
                return invalid(format!(
                    "buffer length {} does not match extent length {}",
                    slice.len(),
                    extent.len
                ));
            }
            raw.push((*extent, slice.as_mut_ptr()));
        }
        self.read_raw(&raw)
    }

    /// Coalesce adjacent shard reads within 64 KiB, split at 4 MiB aligned boundaries, and
    /// execute across the persistent `O_DIRECT` pool.
    fn read_raw(&self, ops: &[(Extent, *mut u8)]) -> Result<()> {
        let (sub_ops, tasks) = plan_read_tasks(ops, self.shared.shards.len())?;
        if tasks.is_empty() {
            return Ok(());
        }

        let mut caller_bounce = self.caller_bounce.lock().unwrap_or_else(|e| e.into_inner());
        let bounce_slice = caller_bounce.slice_mut(0, CHUNK_BYTES as usize);

        // Single-chunk fast path: zero thread wakeups or mutex/condvar traffic.
        if tasks.len() == 1 || self.workers.is_empty() {
            self.shared.cursor.store(0, Ordering::Relaxed);
            return self.shared.drain_tasks(bounce_slice, &tasks, &sub_ops);
        }

        let mut gen_guard = self
            .next_generation
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let generation = *gen_guard;
        *gen_guard += 1;

        self.shared.cursor.store(0, Ordering::Release);
        {
            let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            state.batch = Some(ActiveBatch {
                generation,
                tasks_ptr: tasks.as_ptr() as usize,
                tasks_len: tasks.len(),
                ops_ptr: sub_ops.as_ptr() as usize,
                ops_len: sub_ops.len(),
                remaining_workers: self.workers.len(),
                first_error: None,
            });
            self.shared.work_cv.notify_all();
        }

        let caller_res = self.shared.drain_tasks(bounce_slice, &tasks, &sub_ops);

        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        while state
            .batch
            .as_ref()
            .is_some_and(|b| b.remaining_workers > 0)
        {
            state = self
                .shared
                .done_cv
                .wait(state)
                .unwrap_or_else(|e| e.into_inner());
        }
        let finished = state.batch.take();
        drop(state);
        drop(gen_guard);

        caller_res?;
        if let Some(batch) = finished
            && let Some(err) = batch.first_error
        {
            return Err(err);
        }
        Ok(())
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        {
            let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            state.shutdown = true;
            self.shared.work_cv.notify_all();
        }
        for handle in self.workers.drain(..) {
            let _ = handle.join();
        }
    }
}

fn worker_loop(pool: &PoolShared, bounce: &mut AnonMap) {
    let mut seen_gen = 0u64;
    let bounce_slice = bounce.slice_mut(0, CHUNK_BYTES as usize);
    loop {
        let (tasks_ptr, tasks_len, ops_ptr, ops_len) = {
            let mut state = pool.state.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if state.shutdown {
                    return;
                }
                if let Some(batch) = &state.batch
                    && batch.generation != seen_gen
                {
                    seen_gen = batch.generation;
                    break (
                        batch.tasks_ptr,
                        batch.tasks_len,
                        batch.ops_ptr,
                        batch.ops_len,
                    );
                }
                state = pool.work_cv.wait(state).unwrap_or_else(|e| e.into_inner());
            }
        };

        // SAFETY: `Reader::read_raw` blocks on `done_cv` until every worker decrements
        // `remaining_workers` to 0 before `tasks` or `sub_ops` leave scope.
        let (tasks, ops) = unsafe {
            (
                slice::from_raw_parts(tasks_ptr as *const ChunkTask, tasks_len),
                slice::from_raw_parts(ops_ptr as *const SubReadOp, ops_len),
            )
        };
        let res = pool.drain_tasks(bounce_slice, tasks, ops);

        let mut state = pool.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(batch) = &mut state.batch
            && batch.generation == seen_gen
        {
            if let Err(err) = res
                && batch.first_error.is_none()
            {
                batch.first_error = Some(err);
            }
            batch.remaining_workers -= 1;
            if batch.remaining_workers == 0 {
                pool.done_cv.notify_all();
            }
        }
    }
}

fn plan_read_tasks(
    ops: &[(Extent, *mut u8)],
    shard_count: usize,
) -> Result<(Vec<SubReadOp>, Vec<ChunkTask>)> {
    let mut sub_ops = Vec::with_capacity(ops.len());
    for &(extent, dst) in ops {
        if extent.len == 0 {
            continue;
        }
        if usize::from(extent.shard) >= shard_count {
            return invalid(format!(
                "extent shard {} exceeds shard count {shard_count}",
                extent.shard
            ));
        }
        let mut off = extent.offset;
        let end = off
            .checked_add(extent.len)
            .ok_or_else(|| Error::Invalid("extent offset overflow".into()))?;
        let mut cur_dst = dst as usize;
        while off < end {
            // Split at 4 MiB file boundaries so every sub-op lies inside a single aligned chunk.
            let window_end = ((off / CHUNK_BYTES) + 1) * CHUNK_BYTES;
            let piece_end = end.min(window_end);
            let piece_len = (piece_end - off) as usize;
            sub_ops.push(SubReadOp {
                shard: extent.shard,
                offset: off,
                len: piece_len,
                dst: cur_dst,
            });
            off = piece_end;
            cur_dst += piece_len;
        }
    }
    if sub_ops.is_empty() {
        return Ok((sub_ops, Vec::new()));
    }

    sub_ops.sort_unstable_by_key(|op| (op.shard, op.offset));

    let mut tasks = Vec::new();
    let mut group_start = 0usize;
    let mut cur_shard = sub_ops[0].shard;
    let mut span_start = sub_ops[0].offset;
    let mut span_end = span_start + sub_ops[0].len as u64;

    for (idx, op) in sub_ops.iter().enumerate().skip(1) {
        let op_end = op.offset + op.len as u64;
        let merged_end = span_end.max(op_end);
        let aligned_start = span_start & !(DIRECT_ALIGN - 1);
        let aligned_merged_end = merged_end.next_multiple_of(DIRECT_ALIGN);
        let can_coalesce = op.shard == cur_shard
            && op.offset <= span_end + COALESCE_GAP
            && aligned_merged_end - aligned_start <= CHUNK_BYTES;

        if can_coalesce {
            span_end = merged_end;
        } else {
            tasks.push(finish_chunk(
                cur_shard,
                span_start,
                span_end,
                group_start..idx,
            ));
            group_start = idx;
            cur_shard = op.shard;
            span_start = op.offset;
            span_end = op_end;
        }
    }
    tasks.push(finish_chunk(
        cur_shard,
        span_start,
        span_end,
        group_start..sub_ops.len(),
    ));
    Ok((sub_ops, tasks))
}

fn finish_chunk(shard: u16, start: u64, end: u64, ops: Range<usize>) -> ChunkTask {
    let aligned_offset = start & !(DIRECT_ALIGN - 1);
    let aligned_end = end.next_multiple_of(DIRECT_ALIGN);
    ChunkTask {
        shard,
        aligned_offset,
        aligned_len: (aligned_end - aligned_offset) as usize,
        required_len: (end - aligned_offset) as usize,
        ops,
    }
}

/// Allocate a virtually contiguous `Memory::VramVisible` range in `<= 256 MiB` TTM slices so
/// multi-GiB stores never hit TTM contiguous-allocation limits while keeping flat indexing.
fn alloc_vram_contiguous(gpu: &mut Gpu, bytes: u64) -> Result<(u64, Vec<Buffer>)> {
    if bytes == 0 {
        return Ok((0, Vec::new()));
    }
    let mut buffers = Vec::new();
    let mut remaining = bytes;
    let mut base_va = 0u64;
    let mut expected_va = 0u64;
    while remaining > 0 {
        let ask = remaining.min(VRAM_SLICE_BYTES);
        let buf = gpu.alloc(ask, Memory::VramVisible, false)?;
        if buffers.is_empty() {
            base_va = buf.va;
        } else if buf.va != expected_va {
            return invalid(format!(
                "non-contiguous VRAM arena slice: expected {expected_va:#x}, got {:#x}",
                buf.va
            ));
        }
        expected_va = buf.va + buf.size;
        remaining = remaining.saturating_sub(buf.size);
        buffers.push(buf);
    }
    Ok((base_va, buffers))
}

/// GPU DMA and BAR write-flush hooks used by [`ExpertCache`].
pub trait DeviceTransfer {
    /// Flush CPU write-combined BAR writes before a GPU dispatch consumes VRAM slots.
    fn flush_cpu_writes(&self);
    /// Copy `bytes` from `src_va` (in VRAM) to `dst_va` (in `Memory::Host` GTT staging) on the GPU.
    fn copy_vram_to_host(&mut self, dst_va: u64, src_va: u64, bytes: u64) -> Result<()>;
}

/// Loaded `store.hsaco` helper for GPU-driven VRAM -> Host expert demotion copies.
pub struct GpuCopier {
    _module: Module,
    copy_kernel: u64,
    cmd: Buffer,
    scratch: Buffer,
}

#[repr(C)]
struct CopyBytesArgs {
    dst: u64,
    src: u64,
    len: u64,
    grid_threads: u64,
}

impl GpuCopier {
    const SIGNAL_OFFSET: u64 = 0;
    const ARGS_OFFSET: u64 = 128;
    const WORKGROUP: u16 = 256;
    /// 96 CUs * 4 workgroups/CU saturates PCIe 3.0 x16 without excess wave launch overhead.
    const MAX_GROUPS: u32 = 384;
    const SCRATCH_CHUNK: usize = 4 << 20;

    pub fn new(gpu: &mut Gpu) -> Result<Self> {
        let module = gpu.load_module(STORE_HSACO)?;
        let copy_kernel = module.kernel("copy_bytes")?;
        let cmd = gpu.alloc(4096, Memory::Host, false)?;
        let scratch = gpu.alloc((Self::SCRATCH_CHUNK + 4096) as u64, Memory::Host, false)?;
        const { assert!(SIGNAL_BYTES <= Self::ARGS_OFFSET) };
        Ok(Self {
            _module: module,
            copy_kernel,
            cmd,
            scratch,
        })
    }

    pub fn transfer<'a>(&'a self, gpu: &'a Gpu, queue: &'a mut Queue) -> KfdTransfer<'a> {
        KfdTransfer {
            gpu,
            queue,
            copier: self,
        }
    }
}

pub struct KfdTransfer<'a> {
    gpu: &'a Gpu,
    queue: &'a mut Queue,
    copier: &'a GpuCopier,
}

impl KfdTransfer<'_> {
    /// Read arbitrary GPU-mapped bytes (`VRAM` or `Host`) into `dst` via the `copy_bytes` shader,
    /// avoiding slow CPU BAR reads.
    pub fn read_device_bytes(&mut self, src_va: u64, dst: &mut [u8]) -> Result<()> {
        let mut offset = 0usize;
        while offset < dst.len() {
            let chunk = (dst.len() - offset).min(GpuCopier::SCRATCH_CHUNK);
            let cur_va = src_va + offset as u64;
            let aligned_va = cur_va & !15;
            let lead = (cur_va - aligned_va) as usize;
            let dma_len = (lead + chunk) as u64;
            self.copy_vram_to_host(self.copier.scratch.va, aligned_va, dma_len)?;
            // SAFETY: `scratch` maps `SCRATCH_CHUNK + 4096` coherent Host bytes, and `lead + chunk`
            // fits inside it (`lead < 16`, `chunk <= SCRATCH_CHUNK`).
            unsafe {
                ptr::copy_nonoverlapping(
                    self.copier.scratch.ptr::<u8>().add(lead),
                    dst.as_mut_ptr().add(offset),
                    chunk,
                );
            }
            offset += chunk;
        }
        Ok(())
    }
}

impl DeviceTransfer for KfdTransfer<'_> {
    fn flush_cpu_writes(&self) {
        self.gpu.flush_cpu_writes();
    }

    fn copy_vram_to_host(&mut self, dst_va: u64, src_va: u64, bytes: u64) -> Result<()> {
        if bytes == 0 {
            return Ok(());
        }
        let vecs = bytes.div_ceil(16);
        let groups = vecs
            .div_ceil(u64::from(GpuCopier::WORKGROUP))
            .clamp(1, u64::from(GpuCopier::MAX_GROUPS)) as u32;
        let grid = groups * u32::from(GpuCopier::WORKGROUP);
        let signal_va = self.copier.cmd.va + GpuCopier::SIGNAL_OFFSET;
        let args_va = self.copier.cmd.va + GpuCopier::ARGS_OFFSET;

        // SAFETY: `cmd` is a 4 KiB `Memory::Host` page exclusively used synchronously here.
        unsafe {
            (args_va as *mut CopyBytesArgs).write_volatile(CopyBytesArgs {
                dst: dst_va,
                src: src_va,
                len: bytes,
                grid_threads: u64::from(grid),
            });
            signal_reset(signal_va, 1);
        }
        self.queue.submit(&Dispatch {
            kernel: self.copier.copy_kernel,
            kernarg: args_va,
            grid,
            rows: 1,
            workgroup: GpuCopier::WORKGROUP,
            signal: signal_va,
        });
        // SAFETY: `signal_va` is aligned at offset 0 of `cmd`.
        let done = unsafe { signal_wait(signal_va, Duration::from_secs(5)) };
        if !done {
            let (rptr, wptr) = self.queue.counters();
            return invalid(format!(
                "copy_bytes GPU DMA timed out (rptr={rptr}, wptr={wptr})"
            ));
        }
        Ok(())
    }
}

/// Metadata and GPU virtual address of a loaded backbone tensor in [`CoreStore`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadedCoreTensor {
    pub dtype: Dtype,
    pub shape: Vec<u64>,
    pub va: u64,
    pub len: u64,
}

enum BackingMem {
    Kfd {
        slices: Vec<Buffer>,
    },
    #[cfg(test)]
    Anon {
        _map: AnonMap,
    },
}

impl BackingMem {
    fn slice_count(&self) -> usize {
        match self {
            Self::Kfd { slices } => slices.len(),
            #[cfg(test)]
            Self::Anon { .. } => 1,
        }
    }
}

/// Resident VRAM store for all non-routed-expert, non-engram backbone weights (`~9.17 GiB`).
pub struct CoreStore {
    base_va: u64,
    total_bytes: u64,
    backing: BackingMem,
    tensors: BTreeMap<String, LoadedCoreTensor>,
}

impl CoreStore {
    /// Allocate chunked `Memory::VramVisible` buffers, stream every core tensor from disk in one
    /// coalesced parallel `O_DIRECT` pass, and flush CPU writes to VRAM.
    pub fn load(catalog: &Catalog, reader: &Reader, gpu: &mut Gpu) -> Result<Self> {
        let total_bytes = aligned_core_bytes(catalog);
        let (base_va, buffers) = alloc_vram_contiguous(gpu, total_bytes)?;
        let tensors = populate_core(catalog, reader, base_va)?;
        gpu.flush_cpu_writes();
        Ok(Self {
            base_va,
            total_bytes,
            backing: BackingMem::Kfd { slices: buffers },
            tensors,
        })
    }

    pub fn base_va(&self) -> u64 {
        self.base_va
    }

    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    pub fn vram_slices(&self) -> usize {
        self.backing.slice_count()
    }

    pub fn len(&self) -> usize {
        self.tensors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tensors.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&LoadedCoreTensor> {
        self.tensors.get(name)
    }

    pub fn require(&self, name: &str) -> Result<&LoadedCoreTensor> {
        self.tensors
            .get(name)
            .ok_or_else(|| Error::Invalid(format!("missing core tensor {name}")))
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &LoadedCoreTensor)> {
        self.tensors.iter().map(|(k, v)| (k.as_str(), v))
    }
}

fn aligned_core_bytes(catalog: &Catalog) -> u64 {
    let mut cursor = 0u64;
    for tensor in catalog.core.values() {
        cursor = cursor.next_multiple_of(TENSOR_ALIGN) + tensor.extent.len;
    }
    cursor
}

fn populate_core(
    catalog: &Catalog,
    reader: &Reader,
    base_va: u64,
) -> Result<BTreeMap<String, LoadedCoreTensor>> {
    let mut cursor = 0u64;
    let mut tensors = BTreeMap::new();
    let mut ops = Vec::with_capacity(catalog.core.len());
    for (name, spec) in &catalog.core {
        cursor = cursor.next_multiple_of(TENSOR_ALIGN);
        let va = base_va + cursor;
        ops.push((spec.extent, va as *mut u8));
        tensors.insert(
            name.clone(),
            LoadedCoreTensor {
                dtype: spec.dtype,
                shape: spec.shape.clone(),
                va,
                len: spec.extent.len,
            },
        );
        cursor += spec.extent.len;
    }
    reader.read_raw(&ops)?;
    Ok(tensors)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExpertTier {
    Vram,
    Ram,
    Cold,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExpertSource {
    VramHit,
    RamPromotedToVram,
    RamSwappedToVram,
    RamStagedToHost,
    ColdToVram,
    ColdToRamAndHost,
    ColdStagedToHost,
}

/// GPU virtual addresses of one routed expert's 6 tensors (`w1/w2/w3.{weight,scale}`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExpertView {
    pub layer: usize,
    pub expert: usize,
    pub base_va: u64,
    pub tensors: [u64; EXPERT_EXTENTS],
    pub source: ExpertSource,
}

impl ExpertView {
    pub const fn w1_weight(&self) -> u64 {
        self.tensors[0]
    }
    pub const fn w1_scale(&self) -> u64 {
        self.tensors[1]
    }
    pub const fn w2_weight(&self) -> u64 {
        self.tensors[2]
    }
    pub const fn w2_scale(&self) -> u64 {
        self.tensors[3]
    }
    pub const fn w3_weight(&self) -> u64 {
        self.tensors[4]
    }
    pub const fn w3_scale(&self) -> u64 {
        self.tensors[5]
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ExpertStats {
    pub vram_hits: u64,
    pub ram_hits: u64,
    pub cold_misses: u64,
    pub promotions: u64,
    pub demotions: u64,
    pub bytes_read: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Residency {
    Cold,
    Ram(usize),
    Vram(usize),
}

/// Strictly exclusive two-tier (`VRAM` + `RAM`) routed-expert cache with a `Memory::Host`
/// coherent GTT staging buffer for PCIe streaming and GPU-driven demotions.
pub struct ExpertCache {
    n_layers: usize,
    n_routed_experts: usize,
    slot_bytes: usize,
    slot_stride: usize,
    extents: Vec<[Extent; EXPERT_EXTENTS]>,
    sub_offsets: Vec<[u64; EXPERT_EXTENTS]>,

    vram_base_va: u64,
    vram_capacity: usize,
    _vram_backing: BackingMem,

    ram: AnonMap,
    ram_capacity: usize,

    host_base_va: u64,
    host_slots: usize,
    _host_backing: BackingMem,

    directory: Vec<Residency>,
    routing_counts: Vec<u64>,
    pinned: Vec<bool>,

    vram_owners: Vec<Option<usize>>,
    vram_ref: Vec<bool>,
    vram_free: Vec<usize>,
    vram_hand: usize,

    ram_owners: Vec<Option<usize>>,
    ram_ref: Vec<bool>,
    ram_hits: Vec<u32>,
    ram_free: Vec<usize>,
    ram_hand: usize,

    stats: ExpertStats,
}

impl ExpertCache {
    pub fn new(catalog: &Catalog, plan: &Plan, gpu: &mut Gpu) -> Result<Self> {
        let slot_bytes = catalog.expert_slot as usize;
        let slot_stride = (catalog.expert_slot.next_multiple_of(SLOT_ALIGN)) as usize;
        let vram_capacity = plan.vram_expert_slots as usize;
        let ram_capacity = plan.ram_expert_slots as usize;

        let vram_bytes = (vram_capacity as u64) * (slot_stride as u64);
        let (vram_base_va, vram_buffers) = alloc_vram_contiguous(gpu, vram_bytes)?;

        let host_bytes = (STAGING_SLOTS as u64) * (slot_stride as u64);
        let host_buf = gpu.alloc(host_bytes.max(4096), Memory::HostCached, false)?;

        let ram_bytes = ram_capacity
            .checked_mul(slot_stride)
            .ok_or_else(|| Error::Invalid("RAM expert cache byte overflow".into()))?;
        let ram = AnonMap::new(ram_bytes, true)?;

        Ok(Self::build(
            catalog,
            slot_bytes,
            slot_stride,
            vram_base_va,
            vram_capacity,
            BackingMem::Kfd {
                slices: vram_buffers,
            },
            ram,
            ram_capacity,
            host_buf.va,
            STAGING_SLOTS,
            BackingMem::Kfd {
                slices: vec![host_buf],
            },
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn build(
        catalog: &Catalog,
        slot_bytes: usize,
        slot_stride: usize,
        vram_base_va: u64,
        vram_capacity: usize,
        vram_backing: BackingMem,
        ram: AnonMap,
        ram_capacity: usize,
        host_base_va: u64,
        host_slots: usize,
        host_backing: BackingMem,
    ) -> Self {
        let total = catalog.expert_count();
        let mut extents = Vec::with_capacity(total);
        let mut sub_offsets = Vec::with_capacity(total);
        for layer in 0..catalog.config.n_layers {
            for expert in 0..catalog.config.n_routed_experts {
                let ex = *catalog
                    .expert(layer, expert)
                    .expect("catalog validated every expert");
                let mut offs = [0u64; EXPERT_EXTENTS];
                let mut cur = 0u64;
                for i in 0..EXPERT_EXTENTS {
                    offs[i] = cur;
                    cur += ex[i].len;
                }
                debug_assert!(cur as usize <= slot_bytes);
                extents.push(ex);
                sub_offsets.push(offs);
            }
        }

        Self {
            n_layers: catalog.config.n_layers,
            n_routed_experts: catalog.config.n_routed_experts,
            slot_bytes,
            slot_stride,
            extents,
            sub_offsets,
            vram_base_va,
            vram_capacity,
            _vram_backing: vram_backing,
            ram,
            ram_capacity,
            host_base_va,
            host_slots,
            _host_backing: host_backing,
            directory: vec![Residency::Cold; total],
            routing_counts: vec![0; total],
            pinned: vec![false; total],
            vram_owners: vec![None; vram_capacity],
            vram_ref: vec![false; vram_capacity],
            vram_free: (0..vram_capacity).rev().collect(),
            vram_hand: 0,
            ram_owners: vec![None; ram_capacity],
            ram_ref: vec![false; ram_capacity],
            ram_hits: vec![0; ram_capacity],
            ram_free: (0..ram_capacity).rev().collect(),
            ram_hand: 0,
            stats: ExpertStats::default(),
        }
    }

    pub const fn slot_bytes(&self) -> usize {
        self.slot_bytes
    }

    pub fn stats(&self) -> ExpertStats {
        self.stats
    }

    pub fn routing_counts(&self) -> &[u64] {
        &self.routing_counts
    }

    pub fn vram_used(&self) -> usize {
        self.vram_capacity - self.vram_free.len()
    }

    pub fn ram_used(&self) -> usize {
        self.ram_capacity - self.ram_free.len()
    }

    pub fn tier_of(&self, layer: usize, expert: usize) -> Option<ExpertTier> {
        let id = self.expert_id(layer, expert).ok()?;
        Some(match self.directory[id] {
            Residency::Vram(_) => ExpertTier::Vram,
            Residency::Ram(_) => ExpertTier::Ram,
            Residency::Cold => ExpertTier::Cold,
        })
    }

    /// Ensure every requested routed expert of `layer` is mapped for the GPU (either in its VRAM
    /// slot or staged in the coherent `Memory::Host` buffer) and return its 6 tensor GPU VAs.
    pub fn acquire_layer(
        &mut self,
        layer: usize,
        experts: &[usize],
        reader: &Reader,
        xfer: &mut dyn DeviceTransfer,
    ) -> Result<Vec<ExpertView>> {
        if layer >= self.n_layers {
            return invalid(format!("layer {layer} out of range {}", self.n_layers));
        }
        // Deduplicate while preserving caller order so duplicate indices never double-stage.
        let mut unique_ids = Vec::with_capacity(experts.len());
        for &expert in experts {
            let id = self.expert_id(layer, expert)?;
            self.routing_counts[id] += 1;
            if !self.pinned[id] {
                self.pinned[id] = true;
                unique_ids.push((expert, id));
            }
        }
        let max_staged = self.host_slots.saturating_sub(1);
        if unique_ids.len() > max_staged {
            for &(_, id) in &unique_ids {
                self.pinned[id] = false;
            }
            return invalid(format!(
                "layer requested {} unique experts, exceeding {} host staging slots",
                unique_ids.len(),
                max_staged
            ));
        }

        let res = self.acquire_pinned(layer, experts, &unique_ids, reader, xfer);
        for &(_, id) in &unique_ids {
            self.pinned[id] = false;
        }
        res
    }

    fn acquire_pinned(
        &mut self,
        layer: usize,
        experts: &[usize],
        unique_ids: &[(usize, usize)],
        reader: &Reader,
        xfer: &mut dyn DeviceTransfer,
    ) -> Result<Vec<ExpertView>> {
        let mut need_hdp_flush = false;
        let mut next_stage_slot = 1usize; // Slot 0 is reserved for VRAM -> Host demotion DMA.
        let mut resolved: HashMap<usize, ExpertView> = HashMap::with_capacity(unique_ids.len());

        // Phase 1: Batch all cold NVMe misses in parallel across the 4 O_DIRECT workers.
        enum ColdTarget {
            Vram { slot: usize },
            RamAndHost { ram_slot: usize, stage_slot: usize },
            HostOnly { stage_slot: usize },
        }
        let mut cold_list = Vec::new();
        let mut read_ops = Vec::new();

        for &(expert, id) in unique_ids {
            if self.directory[id] != Residency::Cold {
                continue;
            }
            self.stats.cold_misses += 1;
            let bytes = self.expert_bytes(id);
            self.stats.bytes_read += bytes;

            let target = if let Some(vram_slot) = self.vram_free.pop() {
                let dst = self.vram_slot_ptr(vram_slot);
                self.push_expert_read_ops(id, dst, &mut read_ops);
                need_hdp_flush = true;
                ColdTarget::Vram { slot: vram_slot }
            } else if self.ram_capacity > 0 {
                let ram_slot = self.alloc_ram_slot()?;
                let stage_slot = next_stage_slot;
                next_stage_slot += 1;
                let dst = self.ram_slot_ptr(ram_slot);
                self.push_expert_read_ops(id, dst, &mut read_ops);
                ColdTarget::RamAndHost {
                    ram_slot,
                    stage_slot,
                }
            } else if self.vram_capacity > 0 {
                let vram_slot = self.evict_vram_slot()?;
                let dst = self.vram_slot_ptr(vram_slot);
                self.push_expert_read_ops(id, dst, &mut read_ops);
                need_hdp_flush = true;
                ColdTarget::Vram { slot: vram_slot }
            } else {
                let stage_slot = next_stage_slot;
                next_stage_slot += 1;
                let dst = self.host_slot_ptr(stage_slot);
                self.push_expert_read_ops(id, dst, &mut read_ops);
                ColdTarget::HostOnly { stage_slot }
            };
            cold_list.push((expert, id, target));
        }

        if !read_ops.is_empty() {
            reader.read_raw(&read_ops)?;
        }

        for (expert, id, target) in cold_list {
            let view = match target {
                ColdTarget::Vram { slot } => {
                    self.bind_vram(slot, id);
                    self.make_view(
                        layer,
                        expert,
                        id,
                        self.vram_slot_va(slot),
                        ExpertSource::ColdToVram,
                    )
                }
                ColdTarget::RamAndHost {
                    ram_slot,
                    stage_slot,
                } => {
                    self.bind_ram(ram_slot, id);
                    let len = self.expert_bytes(id) as usize;
                    // SAFETY: `ram_slot` was just filled by `read_raw`, and `stage_slot` is disjoint.
                    unsafe {
                        ptr::copy_nonoverlapping(
                            self.ram_slot_ptr(ram_slot),
                            self.host_slot_ptr(stage_slot),
                            len,
                        );
                    }
                    self.make_view(
                        layer,
                        expert,
                        id,
                        self.host_slot_va(stage_slot),
                        ExpertSource::ColdToRamAndHost,
                    )
                }
                ColdTarget::HostOnly { stage_slot } => self.make_view(
                    layer,
                    expert,
                    id,
                    self.host_slot_va(stage_slot),
                    ExpertSource::ColdStagedToHost,
                ),
            };
            resolved.insert(id, view);
        }

        // Phase 2: Resolve VRAM hits and RAM hits (promoting hot RAM experts into VRAM).
        for &(expert, id) in unique_ids {
            if resolved.contains_key(&id) {
                continue;
            }
            let view = match self.directory[id] {
                Residency::Vram(vram_slot) => {
                    self.stats.vram_hits += 1;
                    self.vram_ref[vram_slot] = true;
                    self.make_view(
                        layer,
                        expert,
                        id,
                        self.vram_slot_va(vram_slot),
                        ExpertSource::VramHit,
                    )
                }
                Residency::Ram(ram_slot) => {
                    self.stats.ram_hits += 1;
                    self.ram_ref[ram_slot] = true;
                    self.ram_hits[ram_slot] = self.ram_hits[ram_slot].saturating_add(1);
                    let len = self.expert_bytes(id) as usize;

                    if let Some(vram_slot) = self.vram_free.pop() {
                        // Free VRAM slot available: promote directly and vacate the RAM slot!
                        // SAFETY: `ram_slot` and `vram_slot` are valid, disjoint allocations.
                        unsafe {
                            ptr::copy_nonoverlapping(
                                self.ram_slot_ptr(ram_slot),
                                self.vram_slot_ptr(vram_slot),
                                len,
                            );
                        }
                        self.vacate_ram_slot(ram_slot);
                        self.bind_vram(vram_slot, id);
                        self.stats.promotions += 1;
                        need_hdp_flush = true;
                        self.make_view(
                            layer,
                            expert,
                            id,
                            self.vram_slot_va(vram_slot),
                            ExpertSource::RamPromotedToVram,
                        )
                    } else if let Some(vram_slot) = self.select_promotable_vram_victim(id, ram_slot)
                    {
                        // Full VRAM swap: GPU DMAs victim VRAM -> Host[0], CPU WC-writes
                        // hot expert RAM -> VRAM, then CPU copies Host[0] -> vacated RAM slot!
                        let victim_id = self.vram_owners[vram_slot]
                            .expect("selected VRAM slot must have an owner");
                        let victim_len = self.expert_bytes(victim_id);
                        if need_hdp_flush {
                            xfer.flush_cpu_writes();
                        }
                        xfer.copy_vram_to_host(
                            self.host_slot_va(0),
                            self.vram_slot_va(vram_slot),
                            victim_len,
                        )?;
                        // SAFETY: GPU DMA to `host_slot(0)` completed; `ram_slot` holds `id`;
                        // copying `ram_slot -> vram_slot` before overwriting `ram_slot` with
                        // `host_slot(0)` preserves both experts with zero extra buffer.
                        unsafe {
                            ptr::copy_nonoverlapping(
                                self.ram_slot_ptr(ram_slot),
                                self.vram_slot_ptr(vram_slot),
                                len,
                            );
                            ptr::copy_nonoverlapping(
                                self.host_slot_ptr(0),
                                self.ram_slot_ptr(ram_slot),
                                victim_len as usize,
                            );
                        }
                        self.bind_vram(vram_slot, id);
                        self.bind_ram(ram_slot, victim_id);
                        self.ram_ref[ram_slot] = false;
                        self.stats.promotions += 1;
                        self.stats.demotions += 1;
                        need_hdp_flush = true;
                        self.make_view(
                            layer,
                            expert,
                            id,
                            self.vram_slot_va(vram_slot),
                            ExpertSource::RamSwappedToVram,
                        )
                    } else {
                        let stage_slot = next_stage_slot;
                        next_stage_slot += 1;
                        // SAFETY: `ram_slot` and `stage_slot` are valid, disjoint slots.
                        unsafe {
                            ptr::copy_nonoverlapping(
                                self.ram_slot_ptr(ram_slot),
                                self.host_slot_ptr(stage_slot),
                                len,
                            );
                        }
                        self.make_view(
                            layer,
                            expert,
                            id,
                            self.host_slot_va(stage_slot),
                            ExpertSource::RamStagedToHost,
                        )
                    }
                }
                Residency::Cold => unreachable!("cold misses were resolved in phase 1"),
            };
            resolved.insert(id, view);
        }

        if need_hdp_flush {
            xfer.flush_cpu_writes();
        }
        debug_assert!(self.check_exclusivity_invariant());

        let mut out = Vec::with_capacity(experts.len());
        for &expert in experts {
            let id = layer * self.n_routed_experts + expert;
            out.push(resolved[&id]);
        }
        Ok(out)
    }

    fn expert_id(&self, layer: usize, expert: usize) -> Result<usize> {
        if layer >= self.n_layers || expert >= self.n_routed_experts {
            return invalid(format!(
                "expert ({layer}, {expert}) outside ({}, {})",
                self.n_layers, self.n_routed_experts
            ));
        }
        Ok(layer * self.n_routed_experts + expert)
    }

    fn expert_bytes(&self, id: usize) -> u64 {
        let offs = &self.sub_offsets[id];
        let ex = &self.extents[id];
        offs[EXPERT_EXTENTS - 1] + ex[EXPERT_EXTENTS - 1].len
    }

    fn push_expert_read_ops(
        &self,
        id: usize,
        slot_base: *mut u8,
        out: &mut Vec<(Extent, *mut u8)>,
    ) {
        let ex = &self.extents[id];
        let offs = &self.sub_offsets[id];
        for i in 0..EXPERT_EXTENTS {
            // SAFETY: `offs[i] + ex[i].len <= slot_bytes <= slot_stride` inside the caller's slot.
            let dst = unsafe { slot_base.add(offs[i] as usize) };
            out.push((ex[i], dst));
        }
    }

    fn make_view(
        &self,
        layer: usize,
        expert: usize,
        id: usize,
        base_va: u64,
        source: ExpertSource,
    ) -> ExpertView {
        let offs = &self.sub_offsets[id];
        let mut tensors = [0u64; EXPERT_EXTENTS];
        for i in 0..EXPERT_EXTENTS {
            tensors[i] = base_va + offs[i];
        }
        ExpertView {
            layer,
            expert,
            base_va,
            tensors,
            source,
        }
    }

    fn vram_slot_va(&self, slot: usize) -> u64 {
        self.vram_base_va + (slot * self.slot_stride) as u64
    }

    fn vram_slot_ptr(&self, slot: usize) -> *mut u8 {
        self.vram_slot_va(slot) as *mut u8
    }

    fn ram_slot_ptr(&self, slot: usize) -> *mut u8 {
        assert!(slot < self.ram_capacity);
        // SAFETY: `slot * slot_stride < ram.len`.
        unsafe { self.ram.as_ptr().add(slot * self.slot_stride) }
    }

    fn host_slot_va(&self, slot: usize) -> u64 {
        assert!(slot < self.host_slots);
        self.host_base_va + (slot * self.slot_stride) as u64
    }

    fn host_slot_ptr(&self, slot: usize) -> *mut u8 {
        self.host_slot_va(slot) as *mut u8
    }

    fn bind_vram(&mut self, slot: usize, id: usize) {
        self.vram_owners[slot] = Some(id);
        self.vram_ref[slot] = true;
        self.directory[id] = Residency::Vram(slot);
    }

    fn bind_ram(&mut self, slot: usize, id: usize) {
        self.ram_owners[slot] = Some(id);
        self.ram_ref[slot] = true;
        self.ram_hits[slot] = 0;
        self.directory[id] = Residency::Ram(slot);
    }

    fn vacate_ram_slot(&mut self, slot: usize) {
        self.ram_owners[slot] = None;
        self.ram_ref[slot] = false;
        self.ram_hits[slot] = 0;
        self.ram_free.push(slot);
    }

    fn alloc_ram_slot(&mut self) -> Result<usize> {
        if let Some(slot) = self.ram_free.pop() {
            return Ok(slot);
        }
        for _ in 0..(self.ram_capacity * 2 + 1) {
            let s = self.ram_hand;
            self.ram_hand = (self.ram_hand + 1) % self.ram_capacity;
            let Some(owner) = self.ram_owners[s] else {
                return Ok(s);
            };
            if self.pinned[owner] {
                continue;
            }
            if self.ram_ref[s] {
                self.ram_ref[s] = false;
                continue;
            }
            self.directory[owner] = Residency::Cold;
            self.ram_owners[s] = None;
            self.ram_hits[s] = 0;
            return Ok(s);
        }
        invalid("all RAM expert slots are pinned by the current layer")
    }

    fn evict_vram_slot(&mut self) -> Result<usize> {
        for _ in 0..(self.vram_capacity * 2 + 1) {
            let s = self.vram_hand;
            self.vram_hand = (self.vram_hand + 1) % self.vram_capacity;
            let Some(owner) = self.vram_owners[s] else {
                return Ok(s);
            };
            if self.pinned[owner] {
                continue;
            }
            if self.vram_ref[s] {
                self.vram_ref[s] = false;
                continue;
            }
            self.directory[owner] = Residency::Cold;
            self.vram_owners[s] = None;
            return Ok(s);
        }
        invalid("all VRAM expert slots are pinned by the current layer")
    }

    fn select_promotable_vram_victim(&mut self, hot_id: usize, ram_slot: usize) -> Option<usize> {
        if self.vram_capacity == 0 || self.ram_hits[ram_slot] < PROMOTE_MIN_HITS {
            return None;
        }
        let hot_score = self.routing_counts[hot_id];
        // Bound probe steps so non-promoted hits remain O(1) while giving small caches a 2nd-chance sweep.
        let probes = (self.vram_capacity * 2).min(32);
        for _ in 0..probes {
            let s = self.vram_hand;
            self.vram_hand = (self.vram_hand + 1) % self.vram_capacity;
            let Some(owner) = self.vram_owners[s] else {
                return Some(s);
            };
            if self.pinned[owner] {
                continue;
            }
            if self.vram_ref[s] {
                self.vram_ref[s] = false;
                continue;
            }
            if hot_score > self.routing_counts[owner] {
                return Some(s);
            }
        }
        None
    }

    fn check_exclusivity_invariant(&self) -> bool {
        for (v, &owner) in self.vram_owners.iter().enumerate() {
            if let Some(id) = owner
                && self.directory[id] != Residency::Vram(v)
            {
                return false;
            }
        }
        for (r, &owner) in self.ram_owners.iter().enumerate() {
            if let Some(id) = owner
                && self.directory[id] != Residency::Ram(r)
            {
                return false;
            }
        }
        true
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EngramStats {
    pub hits: u64,
    pub misses: u64,
    pub unique_misses: u64,
    pub evictions: u64,
    pub bytes_read: u64,
}

/// 8-way set-associative Clock RAM cache for n-gram embedding rows (`weight` + `scale`).
pub struct EngramCache {
    tables: Vec<EngramTable>,
    weight_bytes: usize,
    scale_bytes: usize,
    row_bytes: usize,
    num_sets: usize,
    tags: AnonMap,
    clocks: Vec<u16>, // low byte = 8 reference bits, high byte = clock hand 0..8
    payloads: AnonMap,
    stats: EngramStats,
}

impl EngramCache {
    pub fn new(catalog: &Catalog, plan: &Plan) -> Result<Self> {
        if catalog.engram.is_empty() {
            return Ok(Self {
                tables: Vec::new(),
                weight_bytes: 0,
                scale_bytes: 0,
                row_bytes: 0,
                num_sets: 0,
                tags: AnonMap::new(0, false)?,
                clocks: Vec::new(),
                payloads: AnonMap::new(0, false)?,
                stats: EngramStats::default(),
            });
        }

        let first = &catalog.engram[0];
        let weight_bytes = (first.weight.len / first.rows) as usize;
        let scale_bytes = (first.scale.len / first.rows) as usize;
        for table in &catalog.engram[1..] {
            let w = (table.weight.len / table.rows) as usize;
            let s = (table.scale.len / table.rows) as usize;
            if w != weight_bytes || s != scale_bytes {
                return invalid("engram tables have inconsistent row byte widths");
            }
        }
        let row_bytes = weight_bytes + scale_bytes;
        // 8 u64 tags (64 B) + 1 u16 clock (2 B) + 8 * row_bytes payload per set.
        let bytes_per_set = ENGRAM_WAYS * 8 + 2 + ENGRAM_WAYS * row_bytes;
        let num_sets = (plan.engram_ram as usize) / bytes_per_set;

        let tags = AnonMap::new(num_sets * ENGRAM_WAYS * 8, true)?;
        let clocks = vec![0u16; num_sets];
        let payloads = AnonMap::new(num_sets * ENGRAM_WAYS * row_bytes, true)?;

        Ok(Self {
            tables: catalog.engram.clone(),
            weight_bytes,
            scale_bytes,
            row_bytes,
            num_sets,
            tags,
            clocks,
            payloads,
            stats: EngramStats::default(),
        })
    }

    pub const fn num_sets(&self) -> usize {
        self.num_sets
    }

    pub const fn capacity_rows(&self) -> usize {
        self.num_sets * ENGRAM_WAYS
    }

    pub const fn row_bytes(&self) -> usize {
        self.row_bytes
    }

    pub const fn weight_bytes(&self) -> usize {
        self.weight_bytes
    }

    pub const fn scale_bytes(&self) -> usize {
        self.scale_bytes
    }

    pub const fn stats(&self) -> EngramStats {
        self.stats
    }

    /// Resolve a batch of row indices from `tables[table_index]` into `out` (`rows.len() * row_bytes`,
    /// laid out as `[weight_bytes | scale_bytes]` per entry).
    ///
    /// - Rows equal to [`ENGRAM_SKIP_ROW`] write zeros without touching cache or disk.
    /// - Duplicate cold misses in the same batch are read from NVMe once and fanned out.
    pub fn lookup_batch(
        &mut self,
        table_index: usize,
        rows: &[u64],
        out: &mut [u8],
        reader: &Reader,
    ) -> Result<EngramStats> {
        let Some(&table) = self.tables.get(table_index) else {
            return invalid(format!(
                "engram table index {table_index} outside {}",
                self.tables.len()
            ));
        };
        let expected_out = rows
            .len()
            .checked_mul(self.row_bytes)
            .ok_or_else(|| Error::Invalid("engram output size overflow".into()))?;
        if out.len() != expected_out {
            return invalid(format!(
                "engram output slice is {} bytes, expected {expected_out}",
                out.len()
            ));
        }

        let mut batch_stats = EngramStats::default();
        // Map `row -> (first_slot_in_out, extra_slots_in_out)`.
        let mut misses: HashMap<u64, (usize, Vec<usize>)> = HashMap::new();
        let mut miss_order: Vec<u64> = Vec::new();

        for (i, &row) in rows.iter().enumerate() {
            let dst_off = i * self.row_bytes;
            if row == ENGRAM_SKIP_ROW {
                out[dst_off..dst_off + self.row_bytes].fill(0);
                continue;
            }
            if row >= table.rows {
                return invalid(format!(
                    "engram row {row} out of bounds for layer {} ({} rows)",
                    table.layer, table.rows
                ));
            }
            let tag = make_engram_tag(table_index, row);
            if let Some(way_off) = self.probe_hit(tag) {
                let src = self.payloads.slice(way_off, self.row_bytes);
                out[dst_off..dst_off + self.row_bytes].copy_from_slice(src);
                batch_stats.hits += 1;
            } else {
                batch_stats.misses += 1;
                misses
                    .entry(row)
                    .and_modify(|(_, extras)| extras.push(i))
                    .or_insert_with(|| {
                        miss_order.push(row);
                        (i, Vec::new())
                    });
            }
        }

        if !miss_order.is_empty() {
            batch_stats.unique_misses = miss_order.len() as u64;
            batch_stats.bytes_read = (miss_order.len() * self.row_bytes) as u64;

            let mut read_ops = Vec::with_capacity(miss_order.len() * 2);
            let out_base = out.as_mut_ptr();
            for &row in &miss_order {
                let (first_idx, _) = misses[&row];
                let (w_ext, s_ext) = table.row(row).expect("row bounds checked above");
                // SAFETY: each `first_idx` is unique across `miss_order`, so its `[weight | scale]`
                // region inside `out` is disjoint from every other `first_idx`.
                unsafe {
                    let row_ptr = out_base.add(first_idx * self.row_bytes);
                    read_ops.push((w_ext, row_ptr));
                    read_ops.push((s_ext, row_ptr.add(self.weight_bytes)));
                }
            }
            reader.read_raw(&read_ops)?;

            for row in miss_order {
                let (first_idx, extras) = &misses[&row];
                let first_off = first_idx * self.row_bytes;
                if self.num_sets > 0 {
                    let tag = make_engram_tag(table_index, row);
                    if self.insert_row(tag, &out[first_off..first_off + self.row_bytes]) {
                        batch_stats.evictions += 1;
                    }
                }
                for &dup_idx in extras {
                    let dup_off = dup_idx * self.row_bytes;
                    out.copy_within(first_off..first_off + self.row_bytes, dup_off);
                }
            }
        }

        self.stats.hits += batch_stats.hits;
        self.stats.misses += batch_stats.misses;
        self.stats.unique_misses += batch_stats.unique_misses;
        self.stats.evictions += batch_stats.evictions;
        self.stats.bytes_read += batch_stats.bytes_read;
        Ok(batch_stats)
    }

    fn set_index(&self, tag: u64) -> usize {
        let h = mix64(tag);
        ((u128::from(h) * (self.num_sets as u128)) >> 64) as usize
    }

    fn probe_hit(&mut self, tag: u64) -> Option<usize> {
        if self.num_sets == 0 {
            return None;
        }
        let set = self.set_index(tag);
        // SAFETY: `tags` holds `num_sets * 8` aligned `u64`s.
        let set_tags = unsafe {
            slice::from_raw_parts(
                self.tags.as_ptr().cast::<u64>().add(set * ENGRAM_WAYS),
                ENGRAM_WAYS,
            )
        };
        for (way, &stored) in set_tags.iter().enumerate() {
            if stored == tag {
                self.clocks[set] |= 1u16 << way;
                return Some((set * ENGRAM_WAYS + way) * self.row_bytes);
            }
        }
        None
    }

    /// Insert `row_data` under `tag`, returning `true` if a valid cached row was evicted.
    fn insert_row(&mut self, tag: u64, row_data: &[u8]) -> bool {
        let set = self.set_index(tag);
        // SAFETY: `tags` holds `num_sets * 8` aligned `u64`s, and `&mut self` grants exclusive access.
        let set_tags = unsafe {
            slice::from_raw_parts_mut(
                self.tags.as_ptr().cast::<u64>().add(set * ENGRAM_WAYS),
                ENGRAM_WAYS,
            )
        };
        for (way, slot_tag) in set_tags.iter_mut().enumerate() {
            if *slot_tag == 0 || *slot_tag == tag {
                *slot_tag = tag;
                self.clocks[set] |= 1u16 << way;
                let dst = self
                    .payloads
                    .slice_mut((set * ENGRAM_WAYS + way) * self.row_bytes, self.row_bytes);
                dst.copy_from_slice(row_data);
                return false;
            }
        }

        let mut used = (self.clocks[set] & 0xFF) as u8;
        let mut hand = ((self.clocks[set] >> 8) as usize) % ENGRAM_WAYS;
        let victim = loop {
            let bit = 1u8 << hand;
            if used & bit == 0 {
                let chosen = hand;
                hand = (hand + 1) % ENGRAM_WAYS;
                break chosen;
            }
            used &= !bit;
            hand = (hand + 1) % ENGRAM_WAYS;
        };
        set_tags[victim] = tag;
        used |= 1u8 << victim;
        self.clocks[set] = u16::from(used) | ((hand as u16) << 8);
        let dst = self.payloads.slice_mut(
            (set * ENGRAM_WAYS + victim) * self.row_bytes,
            self.row_bytes,
        );
        dst.copy_from_slice(row_data);
        true
    }
}

fn make_engram_tag(table_index: usize, row: u64) -> u64 {
    debug_assert!(row < (1u64 << 48));
    1u64.wrapping_add(((table_index as u64) << 48) | row)
}

fn mix64(mut x: u64) -> u64 {
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        catalog::tests::{Fixture, fixture},
        plan::Budget,
    };
    use std::fs::OpenOptions;
    use std::io::{Seek, SeekFrom, Write};

    struct MockTransfer {
        flushes: usize,
        dma_copies: usize,
    }

    impl DeviceTransfer for MockTransfer {
        fn flush_cpu_writes(&self) {
            // Tracked via interior check not needed; no-op on CPU memory.
            let _ = self.flushes;
        }

        fn copy_vram_to_host(&mut self, dst_va: u64, src_va: u64, bytes: u64) -> Result<()> {
            self.dma_copies += 1;
            // SAFETY: test arenas are valid disjoint `AnonMap` regions.
            unsafe {
                ptr::copy_nonoverlapping(src_va as *const u8, dst_va as *mut u8, bytes as usize);
            }
            Ok(())
        }
    }

    /// Stamp deterministic non-zero bytes into every byte of the shard payload after the header
    /// so every tensor, expert matrix, and engram row has a unique verifiable signature.
    fn stamped_fixture() -> (Fixture, Catalog, Vec<u8>) {
        let fx = fixture(|_| {});
        let shard_path = fx.0.join("model-1.safetensors");
        let mut raw = std::fs::read(&shard_path).unwrap();
        let header_len = u64::from_le_bytes(raw[..8].try_into().unwrap()) as usize;
        let data_start = 8 + header_len;
        for (i, b) in raw[data_start..].iter_mut().enumerate() {
            *b = ((i * 37 + 19) & 0xFF) as u8;
        }
        let mut f = OpenOptions::new().write(true).open(&shard_path).unwrap();
        f.seek(SeekFrom::Start(0)).unwrap();
        f.write_all(&raw).unwrap();
        drop(f);
        let catalog = Catalog::open(&fx.0).unwrap();
        (fx, catalog, raw)
    }

    fn test_expert_cache(catalog: &Catalog, vram_slots: usize, ram_slots: usize) -> ExpertCache {
        let slot_bytes = catalog.expert_slot as usize;
        let slot_stride = (catalog.expert_slot.next_multiple_of(SLOT_ALIGN)) as usize;
        let vram_map = AnonMap::new((vram_slots * slot_stride).max(4096), false).unwrap();
        let vram_va = vram_map.as_ptr() as u64;
        let host_map = AnonMap::new(STAGING_SLOTS * slot_stride, false).unwrap();
        let host_va = host_map.as_ptr() as u64;
        let ram_map = AnonMap::new(ram_slots * slot_stride, false).unwrap();
        ExpertCache::build(
            catalog,
            slot_bytes,
            slot_stride,
            vram_va,
            vram_slots,
            BackingMem::Anon { _map: vram_map },
            ram_map,
            ram_slots,
            host_va,
            STAGING_SLOTS,
            BackingMem::Anon { _map: host_map },
        )
    }

    fn expected_expert_bytes(
        catalog: &Catalog,
        raw: &[u8],
        layer: usize,
        expert: usize,
    ) -> Vec<u8> {
        let mut out = Vec::new();
        for ext in catalog.expert(layer, expert).unwrap() {
            let s = ext.offset as usize;
            out.extend_from_slice(&raw[s..s + ext.len as usize]);
        }
        out
    }

    #[test]
    fn reader_coalesces_and_splits_across_chunk_boundaries() {
        let (_fx, catalog, raw) = stamped_fixture();
        let reader = Reader::open(&catalog.shards, 4).unwrap();
        let embed = catalog.core.get("embed.weight").unwrap();
        let mut got = vec![0u8; embed.extent.len as usize];
        reader.read_extent(embed.extent, &mut got).unwrap();
        let start = embed.extent.offset as usize;
        assert_eq!(got, &raw[start..start + got.len()]);

        // Verify chunk splitter handles spans > 4 MiB cleanly at planner level.
        let fake_dst = 0x1000_0000usize as *mut u8;
        let big = Extent {
            shard: 0,
            offset: 123,
            len: (9 << 20) + 777,
        };
        let (subs, tasks) = plan_read_tasks(&[(big, fake_dst)], 1).unwrap();
        assert_eq!(subs.len(), 3);
        assert_eq!(tasks.len(), 3);
        assert!(tasks.iter().all(|t| t.aligned_len <= CHUNK_BYTES as usize));
        assert_eq!(subs[0].len + subs[1].len + subs[2].len, big.len as usize);
    }

    #[test]
    fn core_store_loads_exact_tensor_bytes() {
        let (_fx, catalog, raw) = stamped_fixture();
        let reader = Reader::open(&catalog.shards, 2).unwrap();
        let total = aligned_core_bytes(&catalog);
        let arena = AnonMap::new((total as usize).max(4096), false).unwrap();
        let base_va = arena.as_ptr() as u64;
        let tensors = populate_core(&catalog, &reader, base_va).unwrap();
        let store = CoreStore {
            base_va,
            total_bytes: total,
            backing: BackingMem::Anon { _map: arena },
            tensors,
        };
        assert_eq!(store.vram_slices(), 1);
        let loaded = store.require("embed.weight").unwrap();
        assert_eq!(loaded.dtype, Dtype::Bf16);
        assert_eq!(loaded.shape, vec![4, 2]);
        // SAFETY: `loaded.va` lies inside `store.backing`.
        let actual = unsafe { slice::from_raw_parts(loaded.va as *const u8, loaded.len as usize) };
        let spec = &catalog.core["embed.weight"];
        let off = spec.extent.offset as usize;
        assert_eq!(actual, &raw[off..off + spec.extent.len as usize]);
    }

    #[test]
    fn expert_cache_preserves_strict_exclusivity_across_promotions_and_swaps() {
        let (_fx, catalog, raw) = stamped_fixture();
        let reader = Reader::open(&catalog.shards, 2).unwrap();
        // 1 VRAM slot + 2 RAM slots for 4 total experts -> exercises Cold->VRAM, Cold->RAM,
        // RAM staged hit, and hot RAM<->VRAM swap via GPU DMA!
        let mut cache = test_expert_cache(&catalog, 1, 2);
        let mut xfer = MockTransfer {
            flushes: 0,
            dma_copies: 0,
        };

        // 1) Cold load (0, 0) -> fills free VRAM slot 0.
        let v = cache.acquire_layer(0, &[0], &reader, &mut xfer).unwrap();
        assert_eq!(v[0].source, ExpertSource::ColdToVram);
        assert_eq!(cache.tier_of(0, 0), Some(ExpertTier::Vram));
        assert_eq!((cache.vram_used(), cache.ram_used()), (1, 0));
        // SAFETY: `v[0].base_va` points to a valid `slot_bytes` expert slot.
        let bytes00 = unsafe {
            slice::from_raw_parts(v[0].base_va as *const u8, catalog.expert_slot as usize)
        };
        assert_eq!(
            bytes00,
            expected_expert_bytes(&catalog, &raw, 0, 0).as_slice()
        );

        // 2) Cold load (0, 1) and (1, 0) -> VRAM is full, so both land in RAM + Host staging!
        let v01 = cache.acquire_layer(0, &[1], &reader, &mut xfer).unwrap();
        let v10 = cache.acquire_layer(1, &[0], &reader, &mut xfer).unwrap();
        assert_eq!(v01[0].source, ExpertSource::ColdToRamAndHost);
        assert_eq!(v10[0].source, ExpertSource::ColdToRamAndHost);
        assert_eq!(cache.tier_of(0, 1), Some(ExpertTier::Ram));
        assert_eq!(cache.tier_of(1, 0), Some(ExpertTier::Ram));
        assert_eq!((cache.vram_used(), cache.ram_used()), (1, 2));

        // 3) Hit (0, 1) twice more in RAM so its routing count (3) exceeds (0, 0)'s count (1)
        // and clears the VRAM Clock reference bit on (0, 0).
        let h1 = cache.acquire_layer(0, &[1], &reader, &mut xfer).unwrap();
        assert_eq!(h1[0].source, ExpertSource::RamStagedToHost);
        let h2 = cache.acquire_layer(0, &[1], &reader, &mut xfer).unwrap();
        assert_eq!(h2[0].source, ExpertSource::RamSwappedToVram);
        assert_eq!(xfer.dma_copies, 1);

        // Verify strict exclusivity: (0, 1) promoted to VRAM, (0, 0) demoted to RAM!
        assert_eq!(cache.tier_of(0, 1), Some(ExpertTier::Vram));
        assert_eq!(cache.tier_of(0, 0), Some(ExpertTier::Ram));
        assert_eq!((cache.vram_used(), cache.ram_used()), (1, 2));

        // Verify byte integrity of BOTH swapped experts!
        // SAFETY: `h2[0].base_va` is the VRAM slot now holding `(0, 1)`.
        let swapped_vram = unsafe {
            slice::from_raw_parts(h2[0].base_va as *const u8, catalog.expert_slot as usize)
        };
        assert_eq!(
            swapped_vram,
            expected_expert_bytes(&catalog, &raw, 0, 1).as_slice()
        );

        let check00 = cache.acquire_layer(0, &[0], &reader, &mut xfer).unwrap();
        // SAFETY: `check00[0].base_va` holds `(0, 0)` staged from its demoted RAM slot.
        let demoted_bytes = unsafe {
            slice::from_raw_parts(
                check00[0].base_va as *const u8,
                catalog.expert_slot as usize,
            )
        };
        assert_eq!(
            demoted_bytes,
            expected_expert_bytes(&catalog, &raw, 0, 0).as_slice()
        );
    }

    #[test]
    fn engram_cache_deduplicates_misses_handles_skip_and_evicts() {
        let (_fx, catalog, raw) = stamped_fixture();
        let reader = Reader::open(&catalog.shards, 2).unwrap();
        // 1 set of 8 ways = 64 + 2 + 8*6 = 114 bytes.
        let plan = Plan::new(
            &catalog,
            Budget {
                vram: 16 * 1024,
                ram: 16 * 1024,
                vram_reserved: 0,
                ram_reserved: 0,
                engram_ram: 114,
                context: 16,
            },
        )
        .unwrap();
        // `plan.engram_ram` is capped at `sizes.engram` (18B), so build a second plan check and
        // test both zero-set direct streaming and 1-set caching.
        assert_eq!(plan.engram_ram, 18);
        let mut custom_plan = plan;
        custom_plan.engram_ram = 114;

        let mut cache = EngramCache::new(&catalog, &custom_plan).unwrap();
        assert_eq!(cache.num_sets(), 1);
        assert_eq!(cache.row_bytes(), 6);

        let mut out = vec![0xFFu8; 4 * 6];
        let stats = cache
            .lookup_batch(0, &[2, ENGRAM_SKIP_ROW, 2, 0], &mut out, &reader)
            .unwrap();
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 3);
        assert_eq!(stats.unique_misses, 2); // row 2 deduplicated!

        let table = catalog.engram[0];
        let expected_row = |r: u64| -> Vec<u8> {
            let (w, s) = table.row(r).unwrap();
            let mut v = raw[w.offset as usize..(w.offset + w.len) as usize].to_vec();
            v.extend_from_slice(&raw[s.offset as usize..(s.offset + s.len) as usize]);
            v
        };
        assert_eq!(&out[0..6], expected_row(2).as_slice());
        assert_eq!(&out[6..12], &[0, 0, 0, 0, 0, 0]);
        assert_eq!(&out[12..18], expected_row(2).as_slice());
        assert_eq!(&out[18..24], expected_row(0).as_slice());

        // Second lookup hits RAM with zero disk reads!
        let mut out2 = vec![0u8; 2 * 6];
        let stats2 = cache.lookup_batch(0, &[0, 2], &mut out2, &reader).unwrap();
        assert_eq!(stats2.hits, 2);
        assert_eq!(stats2.misses, 0);
        assert_eq!(stats2.bytes_read, 0);
        assert_eq!(&out2[0..6], expected_row(0).as_slice());
        assert_eq!(&out2[6..12], expected_row(2).as_slice());
    }
}
