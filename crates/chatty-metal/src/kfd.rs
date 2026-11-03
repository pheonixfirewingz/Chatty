//! GPU compute through KFD user-mode queues, with no Mesa or ROCr. The CPU and GPU share one
//! address layout: every allocation has the same virtual address on both.
//!
//! Layouts follow `uapi/linux/kfd_ioctl.h` and `amd_queue.h`/`amd_signal.h` from ROCr.

use crate::{Error, Result, invalid};
use rustix::{
    fd::{AsFd, OwnedFd},
    fs::{Mode, OFlags, open},
    io::Errno,
    ioctl::{Opcode, Updater, ioctl, opcode},
    mm::{MapFlags, ProtFlags, mmap, mmap_anonymous},
};
use std::{
    collections::BTreeMap,
    fs, io, mem,
    path::Path,
    ptr,
    sync::atomic::{AtomicI64, AtomicU32, Ordering, fence},
    thread,
    time::{Duration, Instant},
};

const KFD: &str = "/dev/kfd";
const TOPOLOGY: &str = "/sys/devices/virtual/kfd/kfd/topology/nodes";
/// Above anything the loader or kernel picks in a static binary, and well inside the GPU's
/// 48-bit range. Nothing here is committed until an allocation maps over it.
const ARENA_BASE: u64 = 0x1_0000_0000;
const ARENA_SIZE: u64 = 128 << 30;
const PAGE: u64 = 4096;
const HUGE: u64 = 2 << 20;

const RING_BYTES: u64 = 64 << 10;
const PACKET_BYTES: u64 = 64;
/// `eop_buffer_size` in `kfd_queue_ctx_save_restore_size`, which sysfs does not export.
const EOP_BYTES: u64 = 4096;
/// Bytes of a process's doorbells on SOC15: 1024 queues of 8 bytes.
const DOORBELL_SLICE: u64 = 8192;
const DEBUG_BYTES_PER_WAVE: u64 = 32;
const WAVES_PER_CU: u64 = 32;

/// `KFD_MMAP_GPU_ID`. The zero mmap type is the page of remapped GPU registers.
const MMAP_GPU_ID_SHIFT: u32 = 46;
const MMAP_GPU_ID_MASK: u64 = 0xFFFF << MMAP_GPU_ID_SHIFT;

const QUEUE_TYPE_COMPUTE_AQL: u32 = 2;
const QUEUE_FULL_PERCENT: u32 = 100;
const QUEUE_NORMAL_PRIORITY: u32 = 7;

const FLAG_VRAM: u32 = 1 << 0;
const FLAG_GTT: u32 = 1 << 1;
const FLAG_WRITABLE: u32 = 1 << 31;
const FLAG_EXECUTABLE: u32 = 1 << 30;
const FLAG_PUBLIC: u32 = 1 << 29;
const FLAG_NO_SUBSTITUTE: u32 = 1 << 28;
const FLAG_COHERENT: u32 = 1 << 26;

// Offsets in ROCr's `amd_queue_t`. The command processor finds the rest of the structure from
// the read pointer, so keep the layout even though only these fields are used.
const QUEUE_WRITE_DISPATCH_ID: usize = 56;
const QUEUE_READ_DISPATCH_ID: usize = 128;
const QUEUE_READ_ID_OFFSET_FIELD: usize = 136;

// Offsets in `kfd_context_save_area_header`.
const SAVE_AREA_DEBUG_OFFSET: usize = 16;
const SAVE_AREA_DEBUG_SIZE: usize = 20;

const PACKET_TYPE_INVALID: u32 = 1;
const PACKET_TYPE_KERNEL_DISPATCH: u32 = 2;
const PACKET_BARRIER: u32 = 1 << 8;
const FENCE_SYSTEM: u32 = 2;
const FENCE_AGENT: u32 = 1;
const PACKET_ACQUIRE_SHIFT: u32 = 9;
const PACKET_RELEASE_SHIFT: u32 = 11;
const SETUP_SHIFT: u32 = 16;
const PACKET_TYPE_BARRIER_AND: u32 = 3;

// Offsets in ROCr's 64-byte `amd_signal_t`.
const SIGNAL_VALUE: u64 = 8;
pub const SIGNAL_BYTES: u64 = 64;

#[repr(C)]
#[derive(Default)]
struct VersionArgs {
    major: u32,
    minor: u32,
}

#[repr(C)]
#[derive(Default)]
struct CreateQueueArgs {
    ring_base: u64,
    write_pointer: u64,
    read_pointer: u64,
    doorbell_offset: u64,
    ring_size: u32,
    gpu_id: u32,
    queue_type: u32,
    queue_percentage: u32,
    queue_priority: u32,
    queue_id: u32,
    eop_base: u64,
    eop_size: u64,
    save_base: u64,
    save_size: u32,
    control_stack_size: u32,
    sdma_engine_id: u32,
    pad: u32,
}

#[repr(C)]
#[derive(Default)]
struct DeviceApertures {
    lds_base: u64,
    lds_limit: u64,
    scratch_base: u64,
    scratch_limit: u64,
    gpuvm_base: u64,
    gpuvm_limit: u64,
    gpu_id: u32,
    pad: u32,
}

#[repr(C)]
#[derive(Default)]
struct AperturesArgs {
    apertures: u64,
    nodes: u32,
    pad: u32,
}

#[repr(C)]
#[derive(Default)]
struct AcquireVmArgs {
    drm_fd: u32,
    gpu_id: u32,
}

#[repr(C)]
#[derive(Default)]
struct AllocArgs {
    va: u64,
    size: u64,
    handle: u64,
    mmap_offset: u64,
    gpu_id: u32,
    flags: u32,
}

#[repr(C)]
#[derive(Default)]
struct MapArgs {
    handle: u64,
    device_ids: u64,
    devices: u32,
    success: u32,
}

const fn read_op<T>(nr: u8) -> Opcode {
    opcode::read::<T>(b'K', nr)
}
const fn write_op<T>(nr: u8) -> Opcode {
    opcode::write::<T>(b'K', nr)
}
const fn update_op<T>(nr: u8) -> Opcode {
    opcode::read_write::<T>(b'K', nr)
}

const GET_VERSION: Opcode = read_op::<VersionArgs>(0x01);
const CREATE_QUEUE: Opcode = update_op::<CreateQueueArgs>(0x02);
const GET_APERTURES: Opcode = update_op::<AperturesArgs>(0x14);
const ACQUIRE_VM: Opcode = write_op::<AcquireVmArgs>(0x15);
const ALLOC_MEMORY: Opcode = update_op::<AllocArgs>(0x16);
const MAP_MEMORY: Opcode = update_op::<MapArgs>(0x18);

fn call<const OPCODE: Opcode, T>(fd: &OwnedFd, args: &mut T, name: &str) -> Result<()> {
    // SAFETY: `T` is the `repr(C)` struct the opcode declares, and `Updater` keeps it alive
    // for the call. Pointers inside it are checked by the kernel.
    unsafe { ioctl(fd, Updater::<OPCODE, T>::new(args)) }.map_err(|error| os(name, error))
}

fn os(name: &str, error: Errno) -> Error {
    Error::Io(
        name.into(),
        io::Error::from_raw_os_error(error.raw_os_error()),
    )
}

/// Where an allocation lives. Host and visible memory are mapped for the CPU too.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Memory {
    /// System RAM that the GPU reaches over PCIe, uncached by the GPU.
    Host,
    /// System RAM the GPU may cache. Reads run at PCIe speed, where `Host` pays a round trip per
    /// read. For data the GPU only reads; a system-scope acquire must follow any CPU rewrite.
    HostCached,
    /// VRAM the CPU cannot reach.
    Vram,
    /// VRAM the CPU reaches through the resizable BAR, write-combined.
    VramVisible,
}

impl Memory {
    fn flags(self) -> u32 {
        FLAG_WRITABLE
            | FLAG_NO_SUBSTITUTE
            | match self {
                Self::Host => FLAG_GTT | FLAG_COHERENT,
                Self::HostCached => FLAG_GTT,
                Self::Vram => FLAG_VRAM,
                Self::VramVisible => FLAG_VRAM | FLAG_PUBLIC,
            }
    }

    fn cpu_mapped(self) -> bool {
        self != Self::Vram
    }
}

/// One GPU allocation, alive until the process exits. Its GPU address is `va`, and when the
/// memory is CPU-mapped, its CPU address too.
#[derive(Clone, Copy, Debug)]
pub struct Buffer {
    pub va: u64,
    pub size: u64,
    pub memory: Memory,
}

impl Buffer {
    /// The CPU pointer. Only memory that is not [`Memory::Vram`] may be dereferenced.
    pub fn ptr<T>(&self) -> *mut T {
        debug_assert!(self.memory.cpu_mapped());
        self.va as *mut T
    }
}

/// The values from a topology node's `properties` file that queue setup needs.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NodeProperties {
    pub simd_count: u64,
    pub simd_per_cu: u64,
    pub save_area_bytes: u64,
    pub control_stack_bytes: u64,
    pub render_minor: u64,
    pub gfx_target_version: u64,
}

impl NodeProperties {
    fn parse(text: &str) -> Self {
        let mut node = Self::default();
        for line in text.lines() {
            let Some((name, value)) = line.split_once(' ') else {
                continue;
            };
            let value = value.trim().parse().unwrap_or(0);
            match name {
                "simd_count" => node.simd_count = value,
                "simd_per_cu" => node.simd_per_cu = value,
                "cwsr_size" => node.save_area_bytes = value,
                "ctl_stack_size" => node.control_stack_bytes = value,
                "drm_render_minor" => node.render_minor = value,
                "gfx_target_version" => node.gfx_target_version = value,
                _ => {}
            }
        }
        node
    }

    /// `debug_memory_size` in `kfd_queue_ctx_save_restore_size`, which sysfs does not export.
    fn debug_bytes(&self) -> u64 {
        let compute_units = self.simd_count / self.simd_per_cu.max(1);
        (compute_units * WAVES_PER_CU * DEBUG_BYTES_PER_WAVE).next_multiple_of(64)
    }

    /// The kernel requires the save area mapping to be exactly this size.
    fn save_mapping_bytes(&self) -> u64 {
        (self.save_area_bytes + self.debug_bytes()).next_multiple_of(PAGE)
    }
}

/// The GPU and this process's address space on it.
pub struct Gpu {
    kfd: OwnedFd,
    drm: OwnedFd,
    pub gpu_id: u32,
    pub node: NodeProperties,
    pub kfd_version: (u32, u32),
    pub gpuvm: (u64, u64),
    next_va: u64,
    /// `KFD_MMIO_REMAP_HDP_MEM_FLUSH_CNTL`, the first register of the remapped page.
    hdp_flush: *mut u32,
}

impl Gpu {
    /// Open the one GPU and acquire its address space.
    pub fn open() -> Result<Self> {
        let (gpu_id, node) = find_gpu()?;
        let kfd = open(KFD, OFlags::RDWR | OFlags::CLOEXEC, Mode::empty())
            .map_err(|error| os(KFD, error))?;
        let render = format!("/dev/dri/renderD{}", node.render_minor);
        let drm = open(
            render.as_str(),
            OFlags::RDWR | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| os(&render, error))?;

        let mut version = VersionArgs::default();
        call::<GET_VERSION, _>(&kfd, &mut version, "kfd version")?;

        let mut acquire = AcquireVmArgs {
            drm_fd: fd_number(&drm),
            gpu_id,
        };
        call::<ACQUIRE_VM, _>(&kfd, &mut acquire, "kfd acquire vm")?;

        let mut aperture = DeviceApertures::default();
        let mut apertures = AperturesArgs {
            apertures: ptr::from_mut(&mut aperture) as u64,
            nodes: 1,
            pad: 0,
        };
        call::<GET_APERTURES, _>(&kfd, &mut apertures, "kfd apertures")?;
        if apertures.nodes != 1 || aperture.gpu_id != gpu_id {
            return invalid(format!(
                "kfd reported {} apertures, for gpu {:#x}",
                apertures.nodes, aperture.gpu_id
            ));
        }
        if ARENA_BASE < aperture.gpuvm_base || ARENA_BASE + ARENA_SIZE > aperture.gpuvm_limit {
            return invalid(format!(
                "arena is outside gpuvm {:#x}..{:#x}",
                aperture.gpuvm_base, aperture.gpuvm_limit
            ));
        }

        // SAFETY: the fixed address is reserved with no access, and FIXED_NOREPLACE refuses
        // to touch an existing mapping.
        unsafe {
            mmap_anonymous(
                ARENA_BASE as *mut _,
                ARENA_SIZE as usize,
                ProtFlags::empty(),
                MapFlags::PRIVATE | MapFlags::NORESERVE | MapFlags::FIXED_NOREPLACE,
            )
        }
        .map_err(|error| os("reserve gpu arena", error))?;

        // SAFETY: the kernel accepts only this exact size at this offset, and it picks the
        // address. The mapping lives until the process exits.
        let hdp_flush = unsafe {
            mmap(
                ptr::null_mut(),
                PAGE as usize,
                ProtFlags::READ | ProtFlags::WRITE,
                MapFlags::SHARED,
                kfd.as_fd(),
                (u64::from(gpu_id) << MMAP_GPU_ID_SHIFT) & MMAP_GPU_ID_MASK,
            )
        }
        .map_err(|error| os("map gpu registers", error))?
        .cast();

        Ok(Self {
            kfd,
            drm,
            gpu_id,
            node,
            kfd_version: (version.major, version.minor),
            gpuvm: (aperture.gpuvm_base, aperture.gpuvm_limit),
            next_va: ARENA_BASE,
            hdp_flush,
        })
    }

    /// Push CPU writes through the BAR into VRAM, which the GPU's memory path needs before it
    /// reads them. The read holds this thread until the flush has landed.
    pub fn flush_cpu_writes(&self) {
        fence(Ordering::SeqCst);
        // SAFETY: the page is mapped for the process's lifetime, and offset 0 is the flush
        // register, which the kernel exposes for this purpose.
        unsafe {
            self.hdp_flush.write_volatile(0);
            self.hdp_flush.read_volatile();
        }
    }

    /// Allocate memory and map it for the GPU, and for the CPU where the memory allows.
    pub fn alloc(&mut self, size: u64, memory: Memory, executable: bool) -> Result<Buffer> {
        let size = size.max(1).next_multiple_of(PAGE);
        // Large allocations keep 2 MiB alignment so the GPU can use large page fragments.
        let align = if size >= HUGE { HUGE } else { PAGE };
        let va = self.next_va.next_multiple_of(align);
        if va + size > ARENA_BASE + ARENA_SIZE {
            return invalid("gpu arena is full");
        }

        let mut flags = memory.flags();
        if executable {
            flags |= FLAG_EXECUTABLE;
        }
        let mut alloc = AllocArgs {
            va,
            size,
            gpu_id: self.gpu_id,
            flags,
            ..AllocArgs::default()
        };
        call::<ALLOC_MEMORY, _>(&self.kfd, &mut alloc, "kfd alloc")?;

        if memory.cpu_mapped() {
            // SAFETY: `va..va + size` is inside the arena this type reserved, and the bump
            // allocator gives each address out once. The offset comes from the kernel.
            unsafe {
                mmap(
                    va as *mut _,
                    size as usize,
                    ProtFlags::READ | ProtFlags::WRITE,
                    MapFlags::SHARED | MapFlags::FIXED,
                    self.drm.as_fd(),
                    alloc.mmap_offset,
                )
            }
            .map_err(|error| os("map gpu memory for cpu", error))?;
        }

        let device = self.gpu_id;
        let mut map = MapArgs {
            handle: alloc.handle,
            device_ids: ptr::from_ref(&device) as u64,
            devices: 1,
            success: 0,
        };
        call::<MAP_MEMORY, _>(&self.kfd, &mut map, "kfd map")?;

        self.next_va = va + size;
        Ok(Buffer { va, size, memory })
    }

    /// Create an AQL compute queue. It dispatches with no system call.
    pub fn create_queue(&mut self) -> Result<Queue> {
        // The command processor reads these as code, and it faults on a plain mapping.
        let state = self.alloc(PAGE, Memory::Host, true)?;
        let ring = self.alloc(RING_BYTES, Memory::Host, true)?;
        let eop = self.alloc(EOP_BYTES, Memory::Vram, true)?;
        let save = self.alloc(self.node.save_mapping_bytes(), Memory::Host, true)?;

        let slots = RING_BYTES / PACKET_BYTES;
        for slot in 0..slots {
            // SAFETY: each slot is inside the mapped ring, and the queue does not exist yet.
            unsafe {
                ring.ptr::<u32>()
                    .add((slot * PACKET_BYTES / 4) as usize)
                    .write_volatile(PACKET_TYPE_INVALID);
            }
        }

        // SAFETY: all offsets are inside the zeroed page-sized state and larger save area,
        // and the queue that shares them does not exist yet.
        unsafe {
            let base = state.ptr::<u8>();
            base.add(QUEUE_READ_ID_OFFSET_FIELD)
                .cast::<u32>()
                .write_volatile(QUEUE_READ_DISPATCH_ID as u32);
            let header = save.ptr::<u8>();
            header
                .add(SAVE_AREA_DEBUG_OFFSET)
                .cast::<u32>()
                .write_volatile(self.node.save_area_bytes as u32);
            header
                .add(SAVE_AREA_DEBUG_SIZE)
                .cast::<u32>()
                .write_volatile(self.node.debug_bytes() as u32);
        }

        let mut args = CreateQueueArgs {
            ring_base: ring.va,
            write_pointer: state.va + QUEUE_WRITE_DISPATCH_ID as u64,
            read_pointer: state.va + QUEUE_READ_DISPATCH_ID as u64,
            ring_size: RING_BYTES as u32,
            gpu_id: self.gpu_id,
            queue_type: QUEUE_TYPE_COMPUTE_AQL,
            queue_percentage: QUEUE_FULL_PERCENT,
            queue_priority: QUEUE_NORMAL_PRIORITY,
            eop_base: eop.va,
            eop_size: EOP_BYTES,
            save_base: save.va,
            save_size: self.node.save_area_bytes as u32,
            control_stack_size: self.node.control_stack_bytes as u32,
            ..CreateQueueArgs::default()
        };
        call::<CREATE_QUEUE, _>(&self.kfd, &mut args, "kfd create queue")?;

        let (page, offset) = split_doorbell(args.doorbell_offset);
        // SAFETY: the kernel accepts only this exact size at this offset, and it picks the
        // address. The mapping lives until the process exits, like the queue.
        let doorbells = unsafe {
            mmap(
                ptr::null_mut(),
                DOORBELL_SLICE as usize,
                ProtFlags::READ | ProtFlags::WRITE,
                MapFlags::SHARED,
                self.kfd.as_fd(),
                page,
            )
        }
        .map_err(|error| os("map doorbell", error))?;

        Ok(Queue {
            id: args.queue_id,
            ring,
            state,
            // SAFETY: the kernel's offset is inside the slice just mapped.
            doorbell: unsafe { doorbells.cast::<u8>().add(offset as usize).cast() },
            next: 0,
        })
    }
}

fn fd_number(fd: &OwnedFd) -> u32 {
    use rustix::fd::AsRawFd;
    fd.as_raw_fd() as u32
}

/// The kernel returns the mmap offset of the doorbell slice, plus the queue's byte offset
/// inside that slice.
fn split_doorbell(offset: u64) -> (u64, u64) {
    (
        offset & !(DOORBELL_SLICE - 1),
        offset & (DOORBELL_SLICE - 1),
    )
}

fn find_gpu() -> Result<(u32, NodeProperties)> {
    let read = |path: &Path| {
        fs::read_to_string(path).map_err(|error| Error::Io(path.to_path_buf(), error))
    };
    let root = Path::new(TOPOLOGY);
    let nodes = fs::read_dir(root).map_err(|error| Error::Io(root.to_path_buf(), error))?;
    for entry in nodes.flatten() {
        let path = entry.path();
        // The CPU is a node too, with a gpu_id of 0.
        let gpu_id: u32 = read(&path.join("gpu_id"))?.trim().parse().unwrap_or(0);
        if gpu_id != 0 {
            return Ok((
                gpu_id,
                NodeProperties::parse(&read(&path.join("properties"))?),
            ));
        }
    }
    invalid("kfd topology has no GPU node")
}

/// One dispatch. The grid counts work-items, and the shader gets its workgroup's index.
#[derive(Clone, Copy, Debug)]
pub struct Dispatch {
    /// GPU address of the kernel descriptor.
    pub kernel: u64,
    /// GPU address of the kernel arguments.
    pub kernarg: u64,
    pub grid: u32,
    /// Rows in the grid's second dimension, each its own workgroup.
    pub rows: u32,
    pub workgroup: u16,
    /// GPU address of a [`SIGNAL_BYTES`] signal, which reaches 0 on completion.
    pub signal: u64,
}

impl Dispatch {
    /// The 64-byte AQL packet as dwords. The first dword makes it valid, so it goes last.
    fn encode(&self) -> [u32; 16] {
        self.encode_with_fence(FENCE_SYSTEM, true)
    }

    /// Encode with an explicit acquire/release fence scope (`0` = none, `1` = agent,
    /// `2` = system) and barrier bit, for chaining back-to-back dispatches.
    pub fn encode_with_fence(&self, fence_scope: u32, barrier: bool) -> [u32; 16] {
        let header = PACKET_TYPE_KERNEL_DISPATCH
            | if barrier { PACKET_BARRIER } else { 0 }
            | (fence_scope << PACKET_ACQUIRE_SHIFT)
            | (fence_scope << PACKET_RELEASE_SHIFT);
        let dimensions = (if self.rows > 1 { 2 } else { 1 }) << SETUP_SHIFT;
        let mut packet = [0; 16];
        packet[0] = header | dimensions;
        packet[1] = u32::from(self.workgroup) | (1 << 16);
        packet[2] = 1;
        packet[3] = self.grid;
        packet[4] = self.rows;
        packet[5] = 1;
        packet[8] = self.kernel as u32;
        packet[9] = (self.kernel >> 32) as u32;
        packet[10] = self.kernarg as u32;
        packet[11] = (self.kernarg >> 32) as u32;
        packet[14] = self.signal as u32;
        packet[15] = (self.signal >> 32) as u32;
        packet
    }
}

/// An AQL queue with one producer.
pub struct Queue {
    pub id: u32,
    ring: Buffer,
    state: Buffer,
    doorbell: *mut u64,
    next: u64,
}

impl Queue {
    /// Publish a dispatch and ring the doorbell. The caller resets the signal first.
    pub fn submit(&mut self, dispatch: &Dispatch) {
        self.submit_packet(&dispatch.encode());
    }

    /// Publish an encoded 64-byte AQL packet and ring the doorbell.
    pub fn submit_packet(&mut self, packet: &[u32; 16]) {
        let slots = RING_BYTES / PACKET_BYTES;
        let index = self.next;
        self.next += 1;
        // SAFETY: the masked slot is inside the mapped ring. The GPU ignores a slot until its
        // header stops being invalid, and the release store publishes the other dwords first.
        // The state and doorbell addresses stay mapped until the process exits.
        unsafe {
            let slot = self
                .ring
                .ptr::<u32>()
                .add(((index % slots) * PACKET_BYTES / 4) as usize);
            for (word, value) in packet.iter().enumerate().skip(1) {
                slot.add(word).write_volatile(*value);
            }
            AtomicU32::from_ptr(slot).store(packet[0], Ordering::Release);
            self.state
                .ptr::<u8>()
                .add(QUEUE_WRITE_DISPATCH_ID)
                .cast::<u64>()
                .write_volatile(index + 1);
            fence(Ordering::SeqCst);
            self.doorbell.write_volatile(index);
        }
    }

    /// Publish a barrier-AND packet with no dependencies: it completes, and decrements `signal`,
    /// once everything before it has finished and its results are visible to the CPU.
    pub fn submit_barrier(&mut self, signal: u64) {
        let mut packet = [0; 16];
        packet[0] = PACKET_TYPE_BARRIER_AND
            | PACKET_BARRIER
            | (FENCE_SYSTEM << PACKET_ACQUIRE_SHIFT)
            | (FENCE_SYSTEM << PACKET_RELEASE_SHIFT);
        packet[14] = signal as u32;
        packet[15] = (signal >> 32) as u32;
        self.submit_packet(&packet);
    }

    /// A barrier-AND packet with no completion signal, at agent scope, that orders the packets
    /// after it behind those before it.
    pub fn submit_order_barrier(&mut self) {
        let mut packet = [0; 16];
        packet[0] = PACKET_TYPE_BARRIER_AND
            | PACKET_BARRIER
            | (FENCE_AGENT << PACKET_ACQUIRE_SHIFT)
            | (FENCE_AGENT << PACKET_RELEASE_SHIFT);
        self.submit_packet(&packet);
    }

    /// The (read, write) dispatch counters, for reporting a stuck queue.
    pub fn counters(&self) -> (u64, u64) {
        // SAFETY: both offsets are aligned and inside the mapped state page.
        unsafe {
            let base = self.state.ptr::<u8>();
            (
                base.add(QUEUE_READ_DISPATCH_ID)
                    .cast::<u64>()
                    .read_volatile(),
                base.add(QUEUE_WRITE_DISPATCH_ID)
                    .cast::<u64>()
                    .read_volatile(),
            )
        }
    }
}

/// Set a signal in CPU-mapped memory to the number of completions to wait for.
///
/// # Safety
/// `signal` must be the CPU address of a mapped, aligned signal.
pub unsafe fn signal_reset(signal: u64, value: i64) {
    // SAFETY: the caller guarantees the address. Offset 0 is `amd_signal_t::kind`
    // (`AMD_SIGNAL_KIND_USER = 1`), which CP MEC firmware inspects before signaling.
    unsafe {
        AtomicI64::from_ptr(signal as *mut i64).store(1, Ordering::Relaxed);
        AtomicI64::from_ptr((signal + SIGNAL_VALUE) as *mut i64).store(value, Ordering::Release);
    }
}

/// Spin until a signal reaches 0. Returns false on timeout.
///
/// # Safety
/// `signal` must be the CPU address of a mapped, aligned signal.
pub unsafe fn signal_wait(signal: u64, timeout: Duration) -> bool {
    // SAFETY: the caller guarantees the address.
    let value = unsafe { AtomicI64::from_ptr((signal + SIGNAL_VALUE) as *mut i64) };
    let start = Instant::now();
    loop {
        for _ in 0..1000 {
            if value.load(Ordering::Acquire) <= 0 {
                return true;
            }
            std::hint::spin_loop();
        }
        if start.elapsed() > timeout {
            return false;
        }
        thread::yield_now();
    }
}

const MAX_MODULE_BYTES: usize = 16 << 20;
const EM_AMDGPU: u16 = 224;
const PT_LOAD: u32 = 1;
const SHT_SYMTAB: u32 = 2;
const SHT_DYNSYM: u32 = 11;

/// Loaded AMDHSA code object (`gfx1100` ELF) in executable VRAM, indexed by kernel name.
pub struct Module {
    pub buffer: Buffer,
    descriptors: BTreeMap<String, u64>,
}

impl Module {
    /// GPU virtual address of the 64-byte `kernel_descriptor_t` for `name`.
    pub fn kernel(&self, name: &str) -> Result<u64> {
        self.descriptors
            .get(name)
            .map(|&offset| self.buffer.va + offset)
            .ok_or_else(|| Error::Invalid(format!("missing GPU kernel {name:?}")))
    }
}

impl Gpu {
    /// Load a linked `amdgcn-amd-amdhsa` ELF (`.hsaco`) into executable VRAM.
    pub fn load_module(&mut self, elf: &[u8]) -> Result<Module> {
        let parsed = HsacoImage::parse(elf)?;
        let buffer = self.alloc(parsed.mem_size as u64, Memory::VramVisible, true)?;
        // SAFETY: `buffer` maps at least `parsed.mem_size` writable bytes in the BAR, and
        // `HsacoImage::parse` validated every segment's bounds inside `parsed.mem_size`.
        unsafe {
            let dst = buffer.ptr::<u8>();
            ptr::write_bytes(dst, 0, parsed.mem_size);
            for (vaddr, bytes) in parsed.segments {
                ptr::copy_nonoverlapping(bytes.as_ptr(), dst.add(vaddr), bytes.len());
            }
        }
        self.flush_cpu_writes();
        Ok(Module {
            buffer,
            descriptors: parsed.descriptors,
        })
    }
}

#[derive(Debug)]
struct HsacoImage<'a> {
    mem_size: usize,
    segments: Vec<(usize, &'a [u8])>,
    descriptors: BTreeMap<String, u64>,
}

impl<'a> HsacoImage<'a> {
    fn parse(elf: &'a [u8]) -> Result<Self> {
        if elf.len() < 64 || &elf[..6] != b"\x7fELF\x02\x01" {
            return invalid("invalid ELF64 little-endian header");
        }
        let u16_at = |off: usize| -> Result<u16> {
            elf.get(off..off + 2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .ok_or_else(|| Error::Invalid("truncated ELF".into()))
        };
        let u32_at = |off: usize| -> Result<u32> {
            elf.get(off..off + 4)
                .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .ok_or_else(|| Error::Invalid("truncated ELF".into()))
        };
        let u64_at = |off: usize| -> Result<u64> {
            elf.get(off..off + 8)
                .map(|b| u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
                .ok_or_else(|| Error::Invalid("truncated ELF".into()))
        };

        if u16_at(18)? != EM_AMDGPU {
            return invalid("ELF is not EM_AMDGPU");
        }
        let phoff = u64_at(32)? as usize;
        let shoff = u64_at(40)? as usize;
        let phentsize = u16_at(54)? as usize;
        let phnum = u16_at(56)? as usize;
        let shentsize = u16_at(58)? as usize;
        let shnum = u16_at(60)? as usize;
        if phentsize < 56 || shentsize < 64 {
            return invalid("unexpected ELF entry size");
        }

        let mut mem_size = 0usize;
        let mut segments = Vec::new();
        for i in 0..phnum {
            let ph = phoff
                .checked_add(i * phentsize)
                .ok_or_else(|| Error::Invalid("ELF phdr overflow".into()))?;
            if u32_at(ph)? != PT_LOAD {
                continue;
            }
            let offset = u64_at(ph + 8)? as usize;
            let vaddr = u64_at(ph + 16)? as usize;
            let filesz = u64_at(ph + 32)? as usize;
            let memsz = u64_at(ph + 40)? as usize;
            if filesz > memsz {
                return invalid("ELF PT_LOAD filesz exceeds memsz");
            }
            let end = vaddr
                .checked_add(memsz)
                .filter(|&end| end <= MAX_MODULE_BYTES)
                .ok_or_else(|| Error::Invalid("ELF module exceeds size limit".into()))?;
            let slice = elf
                .get(offset..offset.saturating_add(filesz))
                .filter(|s| s.len() == filesz)
                .ok_or_else(|| Error::Invalid("ELF segment past end of file".into()))?;
            mem_size = mem_size.max(end);
            if filesz > 0 {
                segments.push((vaddr, slice));
            }
        }
        if mem_size == 0 {
            return invalid("ELF has no loadable segments");
        }

        let section_slice = |idx: usize| -> Result<&'a [u8]> {
            if idx >= shnum {
                return invalid("ELF section index out of range");
            }
            let sh = shoff
                .checked_add(idx * shentsize)
                .ok_or_else(|| Error::Invalid("ELF shdr overflow".into()))?;
            let off = u64_at(sh + 24)? as usize;
            let size = u64_at(sh + 32)? as usize;
            elf.get(off..off.saturating_add(size))
                .filter(|s| s.len() == size)
                .ok_or_else(|| Error::Invalid("ELF section past end of file".into()))
        };

        let mut descriptors = BTreeMap::new();
        for i in 0..shnum {
            let sh = shoff
                .checked_add(i * shentsize)
                .ok_or_else(|| Error::Invalid("ELF shdr overflow".into()))?;
            let sh_type = u32_at(sh + 4)?;
            if sh_type != SHT_DYNSYM && sh_type != SHT_SYMTAB {
                continue;
            }
            let strtab = section_slice(u32_at(sh + 40)? as usize)?;
            let syms = section_slice(i)?;
            for sym in syms.as_chunks::<24>().0 {
                let st_name = u32::from_le_bytes([sym[0], sym[1], sym[2], sym[3]]) as usize;
                let st_value = u64::from_le_bytes([
                    sym[8], sym[9], sym[10], sym[11], sym[12], sym[13], sym[14], sym[15],
                ]);
                let st_size = u64::from_le_bytes([
                    sym[16], sym[17], sym[18], sym[19], sym[20], sym[21], sym[22], sym[23],
                ]);
                let Some(tail) = strtab.get(st_name..) else {
                    continue;
                };
                let nul = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
                let Ok(name) = std::str::from_utf8(&tail[..nul]) else {
                    continue;
                };
                if let Some(kernel) = name.strip_suffix(".kd")
                    && st_size == 64
                    && st_value.saturating_add(64) <= mem_size as u64
                {
                    descriptors.insert(kernel.to_owned(), st_value);
                }
            }
        }
        if descriptors.is_empty() {
            return invalid("ELF has no AMDHSA kernel descriptors");
        }
        Ok(Self {
            mem_size,
            segments,
            descriptors,
        })
    }
}

const _: () = {
    assert!(mem::size_of::<VersionArgs>() == 8);
    assert!(mem::size_of::<CreateQueueArgs>() == 96);
    assert!(mem::size_of::<DeviceApertures>() == 56);
    assert!(mem::size_of::<AperturesArgs>() == 16);
    assert!(mem::size_of::<AcquireVmArgs>() == 8);
    assert!(mem::size_of::<AllocArgs>() == 40);
    assert!(mem::size_of::<MapArgs>() == 24);
};

#[cfg(test)]
mod tests {
    use super::*;

    /// Values read from the real Navi 31 node on 2026-11-02.
    const NAVI31: &str = "simd_count 192\nsimd_per_cu 2\ncwsr_size 46047232\nctl_stack_size 40960\ndrm_render_minor 128\ngfx_target_version 110000\nunknown line here\n";

    #[test]
    fn parses_node_properties() {
        let node = NodeProperties::parse(NAVI31);
        assert_eq!(node.simd_count, 192);
        assert_eq!(node.simd_per_cu, 2);
        assert_eq!(node.save_area_bytes, 46_047_232);
        assert_eq!(node.control_stack_bytes, 40960);
        assert_eq!(node.render_minor, 128);
        assert_eq!(node.gfx_target_version, 110_000);
    }

    #[test]
    fn save_mapping_matches_kernel_formula() {
        let node = NodeProperties::parse(NAVI31);
        assert_eq!(node.debug_bytes(), 98_304);
        assert_eq!(node.save_mapping_bytes(), 46_145_536);
    }

    #[test]
    fn encodes_dispatch_packet() {
        let packet = Dispatch {
            kernel: 0x1122_3344_5566_7788,
            kernarg: 0x99AA_BBCC_DDEE_FF00,
            grid: 4096,
            rows: 1,
            workgroup: 64,
            signal: 0x0102_0304_0506_0708,
        }
        .encode();
        // Type 2, barrier, system acquire and release, one dimension.
        assert_eq!(packet[0], 0x0001_1502);
        assert_eq!(packet[1], 64 | (1 << 16));
        assert_eq!(packet[2], 1);
        assert_eq!(&packet[3..6], &[4096, 1, 1]);
        assert_eq!(&packet[6..8], &[0, 0]);
        assert_eq!(&packet[8..10], &[0x5566_7788, 0x1122_3344]);
        assert_eq!(&packet[10..12], &[0xDDEE_FF00, 0x99AA_BBCC]);
        assert_eq!(&packet[12..14], &[0, 0]);
        assert_eq!(&packet[14..16], &[0x0506_0708, 0x0102_0304]);
    }

    #[test]
    fn splits_doorbell_offset() {
        let page = 3u64 << 62 | 0x1234u64 << 46;
        assert_eq!(split_doorbell(page | 0x18), (page, 0x18));
    }

    #[test]
    fn signals_complete_at_zero() {
        let signal = [0u64; 8];
        let address = signal.as_ptr() as u64;
        // SAFETY: the array is aligned, and it outlives both calls.
        unsafe {
            signal_reset(address, 1);
            assert!(!signal_wait(address, Duration::from_millis(1)));
            signal_reset(address, 0);
            assert!(signal_wait(address, Duration::from_millis(1)));
        }
    }

    #[test]
    fn parses_compiled_hsaco_and_rejects_malformed_elfs() {
        const STORE_HSACO: &[u8] = include_bytes!("../shaders/store.hsaco");
        let parsed = HsacoImage::parse(STORE_HSACO).unwrap();
        assert!(parsed.mem_size >= 64);
        assert!(!parsed.segments.is_empty());
        let kd = *parsed.descriptors.get("copy_bytes").unwrap();
        assert!(kd + 64 <= parsed.mem_size as u64);

        assert!(HsacoImage::parse(b"not an elf").is_err());

        let mut wrong_arch = STORE_HSACO.to_vec();
        wrong_arch[18..20].copy_from_slice(&62u16.to_le_bytes());
        assert!(
            HsacoImage::parse(&wrong_arch)
                .unwrap_err()
                .to_string()
                .contains("EM_AMDGPU")
        );

        let truncated = &STORE_HSACO[..STORE_HSACO.len() / 2];
        assert!(HsacoImage::parse(truncated).is_err());
    }
}
