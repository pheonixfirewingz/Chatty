//! Where each routed expert lives, chosen for one RX 7900 XTX over PCIe 3.0.
//!
//! An expert runs about 30x faster from VRAM than from RAM, and a drive read costs more again, so
//! nothing here blocks a token on the drive unless asked to. RAM experts run on the CPU in place
//! (see `cpu_fp4`). Worker threads fetch cold experts and promote hot ones in the background. Weights never change, so a RAM copy stays valid and eviction copies nothing back.
//! Slots go to the most-demanded experts: routing is skewed, and a recency policy would let one-off
//! experts evict hot ones.

use crate::{
    Result,
    catalog::{Catalog, EXPERT_EXTENTS},
    invalid,
    kfd::{Buffer, Gpu, Memory},
    store::Reader,
};
use std::{
    ptr, slice,
    sync::{
        Arc,
        mpsc::{Receiver, Sender, TryRecvError, channel},
    },
    thread,
};

const NONE: u32 = u32::MAX;
const SLICE_BYTES: u64 = 256 << 20;
const IO_THREADS: usize = 4;
/// Demand counts halve this often, so old conversations fade.
const DECAY_TICKS: u64 = 256 * 40;
/// Demands before a RAM expert is worth a VRAM slot.
const PROMOTE_AFTER: u32 = 2;
const MAX_QUEUED: usize = 16;
/// Preloaded experts start with at most this demand, so live traffic can still displace them.
const PRELOAD_DEMAND_CAP: u32 = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tier {
    Vram,
    Ram,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Counters {
    pub fetched: u64,
    pub promoted: u64,
    pub evicted_vram: u64,
    pub blocking_loads: u64,
}

struct FetchJob {
    id: usize,
    ptr: usize,
}

struct CopyJob {
    id: usize,
    src: usize,
    dst: usize,
    bytes: usize,
}

enum Done {
    Fetched(usize),
    Copied(usize),
}

/// A virtually contiguous range made of slices, since single huge buffers can fail to allocate.
struct Arena {
    va: u64,
    stride: u64,
    slots: usize,
    _buffers: Vec<Buffer>,
}

impl Arena {
    fn new(gpu: &mut Gpu, slots: usize, stride: u64, memory: Memory) -> Result<Self> {
        let bytes = slots as u64 * stride;
        let mut buffers = Vec::new();
        let mut done = 0;
        let mut va = 0;
        while done < bytes {
            let buffer = gpu.alloc(SLICE_BYTES.min(bytes - done), memory, false)?;
            if buffers.is_empty() {
                va = buffer.va;
            } else if buffer.va != va + done {
                return invalid("expert arena is not contiguous");
            }
            done += buffer.size;
            buffers.push(buffer);
        }
        Ok(Self {
            va,
            stride,
            slots,
            _buffers: buffers,
        })
    }

    /// Plain memory for the CPU. Untouched pages stay unallocated until an expert lands in them.
    fn in_ram(slots: usize, stride: u64) -> Self {
        let memory = vec![0u8; slots * stride as usize].leak();
        Self {
            va: memory.as_mut_ptr() as u64,
            stride,
            slots,
            _buffers: Vec::new(),
        }
    }

    fn slot(&self, slot: u32) -> u64 {
        self.va + u64::from(slot) * self.stride
    }
}

pub struct Residency {
    catalog: Arc<Catalog>,
    n_experts: usize,
    /// Byte offset of each of an expert's tensors, per expert.
    offsets: Vec<[u32; EXPERT_EXTENTS]>,
    expert_bytes: Vec<u32>,
    vram: Arena,
    ram: Arena,
    in_vram: Vec<u32>,
    in_ram: Vec<u32>,
    vram_owner: Vec<u32>,
    ram_owner: Vec<u32>,
    vram_free: Vec<u32>,
    ram_free: Vec<u32>,
    /// Decayed demand per expert.
    demand: Vec<u32>,
    /// Queued or running on a worker.
    busy: Vec<bool>,
    /// The tick that last chose this expert to run.
    used: Vec<u64>,
    tick: u64,
    queued: usize,
    fetch_tx: Sender<FetchJob>,
    copy_tx: Sender<CopyJob>,
    done_rx: Receiver<Done>,
    pub counters: Counters,
}

impl Residency {
    pub fn new(
        catalog: Arc<Catalog>,
        gpu: &mut Gpu,
        vram_slots: usize,
        ram_slots: usize,
    ) -> Result<Self> {
        let stride = catalog.expert_slot.next_multiple_of(4096);
        let count = catalog.expert_count();
        let n_experts = catalog.config.n_routed_experts;
        let vram = Arena::new(gpu, vram_slots, stride, Memory::VramVisible)?;
        let ram = Arena::in_ram(ram_slots, stride);
        let mut offsets = Vec::with_capacity(count);
        let mut expert_bytes = Vec::with_capacity(count);
        for id in 0..count {
            let Some(extents) = catalog.expert(id / n_experts, id % n_experts) else {
                return invalid(format!("no expert {id}"));
            };
            let mut at = 0u32;
            let mut table = [0; EXPERT_EXTENTS];
            for (offset, extent) in table.iter_mut().zip(extents) {
                *offset = at;
                at += extent.len as u32;
            }
            offsets.push(table);
            expert_bytes.push(at);
        }

        let (fetch_tx, fetch_rx) = channel();
        let (copy_tx, copy_rx) = channel();
        let (done_tx, done_rx) = channel();
        let reader = Reader::open(&catalog.shards, IO_THREADS)?;
        spawn_fetcher(catalog.clone(), reader, fetch_rx, done_tx.clone());
        spawn_copier(copy_rx, done_tx);
        Ok(Self {
            n_experts,
            offsets,
            expert_bytes,
            in_vram: vec![NONE; count],
            in_ram: vec![NONE; count],
            vram_owner: vec![NONE; vram_slots],
            ram_owner: vec![NONE; ram_slots],
            vram_free: (0..vram_slots as u32).rev().collect(),
            ram_free: (0..ram_slots as u32).rev().collect(),
            demand: vec![0; count],
            busy: vec![false; count],
            used: vec![0; count],
            tick: 1,
            queued: 0,
            fetch_tx,
            copy_tx,
            done_rx,
            counters: Counters::default(),
            vram,
            ram,
            catalog,
        })
    }

    pub const fn vram_slots(&self) -> usize {
        self.vram.slots
    }

    fn id(&self, layer: usize, expert: usize) -> usize {
        layer * self.n_experts + expert
    }

    /// Call when no GPU work is running: everything chosen before now is free to evict.
    pub fn begin_layer(&mut self) {
        self.tick += 1;
        if self.tick.is_multiple_of(DECAY_TICKS) {
            for demand in &mut self.demand {
                *demand >>= 1;
            }
        }
    }

    pub fn tier(&self, layer: usize, expert: usize) -> Option<Tier> {
        let id = self.id(layer, expert);
        if self.in_vram[id] != NONE {
            Some(Tier::Vram)
        } else if self.in_ram[id] != NONE {
            Some(Tier::Ram)
        } else {
            None
        }
    }

    /// Record that routing wanted this expert, whether or not it runs.
    pub fn note_demand(&mut self, layer: usize, expert: usize) {
        let id = self.id(layer, expert);
        self.demand[id] = self.demand[id].saturating_add(1);
    }

    /// Mark an expert as running this layer, so nothing evicts it, and return its tensors.
    pub fn claim(&mut self, layer: usize, expert: usize) -> Option<(Tier, [u64; EXPERT_EXTENTS])> {
        let id = self.id(layer, expert);
        let (tier, base) = if self.in_vram[id] != NONE {
            (Tier::Vram, self.vram.slot(self.in_vram[id]))
        } else if self.in_ram[id] != NONE {
            (Tier::Ram, self.ram.slot(self.in_ram[id]))
        } else {
            return None;
        };
        self.used[id] = self.tick;
        let offsets = self.offsets[id];
        Some((tier, offsets.map(|offset| base + u64::from(offset))))
    }

    /// The tensors of a claimed RAM expert, for the CPU.
    pub fn ram_tensors(&self, layer: usize, expert: usize) -> Option<[&[u8]; EXPERT_EXTENTS]> {
        let id = self.id(layer, expert);
        let slot = self.in_ram[id];
        if slot == NONE {
            return None;
        }
        let base = self.ram.slot(slot) as *const u8;
        let offsets = self.offsets[id];
        let total = self.expert_bytes[id] as usize;
        Some(std::array::from_fn(|i| {
            let start = offsets[i] as usize;
            let end = offsets.get(i + 1).map_or(total, |o| *o as usize);
            // SAFETY: the slot holds this expert until evicted, and `claim` stops eviction during
            // the layer that reads these. Workers write only slots no expert is mapped to.
            unsafe { slice::from_raw_parts(base.add(start), end - start) }
        }))
    }

    /// Start fetching a cold expert this layer must run, ahead of the queue limit, so that it loads
    /// while the GPU and CPU work. Protected from eviction until the next layer.
    pub fn request(&mut self, layer: usize, expert: usize) -> Result<()> {
        let id = self.id(layer, expert);
        self.used[id] = self.tick;
        if self.in_ram[id] == NONE
            && self.in_vram[id] == NONE
            && !self.busy[id]
            && !self.start_fetch(id, u32::MAX)
        {
            return invalid("no RAM slot is free for an expert the model needs");
        }
        Ok(())
    }

    /// Queue whatever moves this expert one tier closer, if a slot is worth taking for it. Call
    /// after claiming this layer's experts.
    pub fn want(&mut self, layer: usize, expert: usize) {
        let id = self.id(layer, expert);
        if self.busy[id] || self.in_vram[id] != NONE || self.queued >= MAX_QUEUED {
            return;
        }
        if self.in_ram[id] == NONE {
            self.start_fetch(id, self.demand[id]);
        } else if self.demand[id] >= PROMOTE_AFTER {
            self.start_promote(id);
        }
    }

    fn start_fetch(&mut self, id: usize, demand: u32) -> bool {
        let Some(slot) = self.take_ram_slot(demand) else {
            return false;
        };
        self.ram_owner[slot as usize] = id as u32;
        self.busy[id] = true;
        self.queued += 1;
        let _ = self.fetch_tx.send(FetchJob {
            id,
            ptr: self.ram.slot(slot) as usize,
        });
        true
    }

    fn start_promote(&mut self, id: usize) {
        let Some(slot) = self.take_vram_slot(self.demand[id]) else {
            return;
        };
        self.vram_owner[slot as usize] = id as u32;
        self.busy[id] = true;
        self.queued += 1;
        let _ = self.copy_tx.send(CopyJob {
            id,
            src: self.ram.slot(self.in_ram[id]) as usize,
            dst: self.vram.slot(slot) as usize,
            bytes: self.expert_bytes[id] as usize,
        });
    }

    /// A free slot, or the slot of the least-demanded expert if this one out-demands it.
    fn take_vram_slot(&mut self, demand: u32) -> Option<u32> {
        if let Some(slot) = self.vram_free.pop() {
            return Some(slot);
        }
        let mut best: Option<(u32, u32, usize)> = None;
        for (slot, &owner) in self.vram_owner.iter().enumerate() {
            let owner = owner as usize;
            if owner == NONE as usize || self.busy[owner] || self.used[owner] == self.tick {
                continue;
            }
            if best.is_none_or(|(d, _, _)| self.demand[owner] < d) {
                best = Some((self.demand[owner], slot as u32, owner));
            }
        }
        let (victim, slot, owner) = best?;
        if victim >= demand {
            return None;
        }
        self.in_vram[owner] = NONE;
        self.vram_owner[slot as usize] = NONE;
        self.counters.evicted_vram += 1;
        Some(slot)
    }

    fn take_ram_slot(&mut self, demand: u32) -> Option<u32> {
        if let Some(slot) = self.ram_free.pop() {
            return Some(slot);
        }
        let mut best: Option<(u32, u32, usize)> = None;
        for (slot, &owner) in self.ram_owner.iter().enumerate() {
            let owner = owner as usize;
            if owner == NONE as usize || self.busy[owner] || self.used[owner] == self.tick {
                continue;
            }
            // An expert also in VRAM can still run if its RAM copy goes, so it leaves first.
            let cost = if self.in_vram[owner] != NONE {
                0
            } else {
                self.demand[owner].saturating_add(1)
            };
            if best.is_none_or(|(d, _, _)| cost < d) {
                best = Some((cost, slot as u32, owner));
            }
        }
        let (victim, slot, owner) = best?;
        if victim > demand {
            return None;
        }
        self.in_ram[owner] = NONE;
        self.ram_owner[slot as usize] = NONE;
        Some(slot)
    }

    /// Adopt finished background work. Returns true if anything the GPU may read has changed, in
    /// which case the caller must run a system-scope acquire before the next dispatch.
    pub fn poll(&mut self, gpu: &Gpu) -> bool {
        let mut changed = false;
        loop {
            match self.done_rx.try_recv() {
                Ok(done) => {
                    self.adopt(done, gpu);
                    changed = true;
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return changed,
            }
        }
    }

    fn adopt(&mut self, done: Done, gpu: &Gpu) {
        self.queued -= 1;
        match done {
            Done::Fetched(id) => {
                let slot = self
                    .ram_owner
                    .iter()
                    .position(|&owner| owner == id as u32)
                    .map_or(NONE, |slot| slot as u32);
                self.in_ram[id] = slot;
                self.busy[id] = false;
                self.counters.fetched += 1;
            }
            Done::Copied(id) => {
                gpu.flush_cpu_writes();
                let slot = self
                    .vram_owner
                    .iter()
                    .position(|&owner| owner == id as u32)
                    .map_or(NONE, |slot| slot as u32);
                self.in_vram[id] = slot;
                self.busy[id] = false;
                self.counters.promoted += 1;
            }
        }
    }

    /// Wait until this expert can run, loading it if needed. For when no substitute exists, or
    /// when routing must stay exact.
    pub fn load_now(&mut self, layer: usize, expert: usize, gpu: &Gpu) -> Result<()> {
        let id = self.id(layer, expert);
        if self.in_ram[id] == NONE && self.in_vram[id] == NONE && !self.busy[id] {
            self.counters.blocking_loads += 1;
            if !self.start_fetch(id, u32::MAX) {
                return invalid("no RAM slot is free for an expert the model needs");
            }
        }
        while self.in_ram[id] == NONE && self.in_vram[id] == NONE {
            match self.done_rx.recv() {
                Ok(done) => self.adopt(done, gpu),
                Err(_) => return invalid("expert workers stopped"),
            }
        }
        Ok(())
    }

    /// Warm the tiers from a list of (layer, expert, uses), hottest first: VRAM until it is full,
    /// then RAM. The first requests served after boot then find these already in place.
    pub fn preload(&mut self, hot: &[(usize, usize, u32)], gpu: &Gpu) -> Result<()> {
        let vram_count = self.vram.slots.min(hot.len());
        for &(layer, expert, uses) in &hot[..vram_count] {
            let id = self.id(layer, expert);
            self.demand[id] = uses.min(PRELOAD_DEMAND_CAP);
            self.load_now(layer, expert, gpu)?;
            self.start_promote(id);
            self.drain_until_idle(id, gpu)?;
        }
        for &(layer, expert, uses) in &hot[vram_count..] {
            let id = self.id(layer, expert);
            self.demand[id] = uses.min(PRELOAD_DEMAND_CAP);
            while self.queued >= MAX_QUEUED {
                match self.done_rx.recv() {
                    Ok(done) => self.adopt(done, gpu),
                    Err(_) => return invalid("expert workers stopped"),
                }
            }
            if !self.start_fetch(id, u32::MAX) {
                break;
            }
        }
        while self.queued > 0 {
            match self.done_rx.recv() {
                Ok(done) => self.adopt(done, gpu),
                Err(_) => return invalid("expert workers stopped"),
            }
        }
        Ok(())
    }

    fn drain_until_idle(&mut self, id: usize, gpu: &Gpu) -> Result<()> {
        while self.busy[id] {
            match self.done_rx.recv() {
                Ok(done) => self.adopt(done, gpu),
                Err(_) => return invalid("expert workers stopped"),
            }
        }
        Ok(())
    }

    /// Experts by demand, hottest first, as (layer, expert, demand).
    pub fn hottest(&self, limit: usize) -> Vec<(usize, usize, u32)> {
        let mut order: Vec<usize> = (0..self.demand.len())
            .filter(|&id| self.demand[id] > 0)
            .collect();
        order.sort_unstable_by_key(|&id| std::cmp::Reverse(self.demand[id]));
        order.truncate(limit);
        order
            .into_iter()
            .map(|id| (id / self.n_experts, id % self.n_experts, self.demand[id]))
            .collect()
    }

    /// Bytes per expert slot, for reporting.
    pub fn slot_bytes(&self) -> u64 {
        self.vram.stride
    }

    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }
}

fn spawn_fetcher(
    catalog: Arc<Catalog>,
    reader: Reader,
    jobs: Receiver<FetchJob>,
    done: Sender<Done>,
) {
    let n_experts = catalog.config.n_routed_experts;
    thread::spawn(move || {
        while let Ok(job) = jobs.recv() {
            let Some(extents) = catalog.expert(job.id / n_experts, job.id % n_experts) else {
                continue;
            };
            let mut at = job.ptr;
            let mut items = Vec::with_capacity(EXPERT_EXTENTS);
            for extent in extents {
                // SAFETY: the slot was handed to this job alone, and is a stride long.
                let bytes =
                    unsafe { slice::from_raw_parts_mut(at as *mut u8, extent.len as usize) };
                items.push((*extent, bytes));
                at += extent.len as usize;
            }
            if reader.read_extents(&mut items).is_err() {
                continue;
            }
            if done.send(Done::Fetched(job.id)).is_err() {
                return;
            }
        }
    });
}

fn spawn_copier(jobs: Receiver<CopyJob>, done: Sender<Done>) {
    thread::spawn(move || {
        while let Ok(job) = jobs.recv() {
            // SAFETY: the source slot is pinned by `busy`, and the destination slot belongs to this
            // job alone until `Done` is adopted.
            unsafe {
                ptr::copy_nonoverlapping(job.src as *const u8, job.dst as *mut u8, job.bytes);
            }
            if done.send(Done::Copied(job.id)).is_err() {
                return;
            }
        }
    });
}
