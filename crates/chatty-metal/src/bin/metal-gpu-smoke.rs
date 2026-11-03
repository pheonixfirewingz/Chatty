//! First compute job, run with `metal_run`: dispatches a shader through a KFD user-mode queue
//! and checks every word it writes, in system RAM and in VRAM. Touches only the GPU.

use chatty_metal::kfd::{Buffer, Dispatch, Gpu, Memory, SIGNAL_BYTES, signal_reset, signal_wait};
use std::{
    process::ExitCode,
    ptr,
    time::{Duration, Instant},
};

const ITEMS: u32 = 4096;
const WORKGROUP: u16 = 64;
const MULTIPLIER: u32 = 0x9E37_79B1;
const LATENCY_RUNS: usize = 2000;
const TIMEOUT: Duration = Duration::from_secs(5);
/// Entry point offset from the descriptor. The hardware needs 256-byte code alignment.
const CODE_OFFSET: usize = 256;

/// Assembled for gfx1100 by `llvm-mc -triple=amdgcn-amd-amdhsa -mcpu=gfx1100`:
///
/// ```text
/// s_load_dwordx2 s[4:5], s[0:1], 0          ; output address from the kernel arguments
/// s_lshl_b32 s3, s2, 6                      ; first index of this 64-item workgroup
/// s_waitcnt lgkmcnt(0)
/// v_and_b32 v0, 0x3ff, v0                   ; local work-item id
/// v_add_nc_u32 v0, s3, v0                   ; global index
/// v_lshlrev_b32 v1, 2, v0                   ; byte offset
/// v_mul_lo_u32 v2, v0, 0x9e3779b1
/// global_store_dword v1, v2, s[4:5]
/// s_endpgm
/// ```
const CODE: [u32; 14] = [
    0xf404_0100,
    0xf800_0000,
    0x8403_8602,
    0xbf89_fc07,
    0x3600_00ff,
    0x0000_03ff,
    0x4a00_0003,
    0x3002_0082,
    0xd72c_0002,
    0x0201_ff00,
    0x9e37_79b1,
    0xdc6a_0000,
    0x0004_0201,
    0xbfb0_0000,
];

/// The 64-byte `kernel_descriptor_t` from the same assembly: kernarg size 8, entry at
/// [`CODE_OFFSET`], wave32, one kernel-argument pointer, workgroup id X in s2.
const DESCRIPTOR: [u32; 16] = [
    0,
    0,
    8,
    0,
    CODE_OFFSET as u32,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0xE0AC_0000,
    0x0000_0084,
    0x0000_0408,
    0,
];

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

fn run() -> Result<(), String> {
    let text = |error: chatty_metal::Error| error.to_string();
    let mut gpu = Gpu::open().map_err(text)?;
    println!(
        "kfd {}.{}, gpu {:#x}, gfx target {}, gpuvm {:#x}..{:#x}",
        gpu.kfd_version.0,
        gpu.kfd_version.1,
        gpu.gpu_id,
        gpu.node.gfx_target_version,
        gpu.gpuvm.0,
        gpu.gpuvm.1
    );

    let mut queue = gpu.create_queue().map_err(text)?;
    println!("queue {} created", queue.id);

    let kernel = gpu.alloc(4096, Memory::VramVisible, true).map_err(text)?;
    load_kernel(&kernel);
    gpu.flush_cpu_writes();

    let shared = gpu.alloc(4096, Memory::Host, false).map_err(text)?;
    let signal = shared.va;
    let kernarg = shared.va + SIGNAL_BYTES;
    let bytes = u64::from(ITEMS) * 4;
    let host = gpu.alloc(bytes, Memory::Host, false).map_err(text)?;
    let vram = gpu.alloc(bytes, Memory::VramVisible, false).map_err(text)?;

    let mut run_dispatch = |target: &Buffer, grid: u32, group: u16| -> Result<(), String> {
        // SAFETY: the kernel arguments and signal are in the mapped page `shared`.
        unsafe {
            ptr::with_exposed_provenance_mut::<u64>(kernarg as usize).write_volatile(target.va);
            signal_reset(signal, 1);
        }
        queue.submit(&Dispatch {
            kernel: kernel.va,
            kernarg,
            grid,
            rows: 1,
            workgroup: group,
            signal,
        });
        // SAFETY: as above.
        if unsafe { signal_wait(signal, TIMEOUT) } {
            return Ok(());
        }
        let (read, write) = queue.counters();
        Err(format!(
            "dispatch did not complete (read {read}, write {write}); check metal_dmesg"
        ))
    };

    run_dispatch(&host, ITEMS, WORKGROUP)?;
    check("system RAM", &host)?;
    run_dispatch(&vram, ITEMS, WORKGROUP)?;
    check("VRAM", &vram)?;

    let mut samples = Vec::with_capacity(LATENCY_RUNS);
    let start = Instant::now();
    for _ in 0..LATENCY_RUNS {
        let round = Instant::now();
        run_dispatch(&vram, u32::from(WORKGROUP), WORKGROUP)?;
        samples.push(round.elapsed());
    }
    let elapsed = start.elapsed();
    samples.sort_unstable();
    println!(
        "ok   {LATENCY_RUNS} dispatches in {elapsed:?}: min {:?}, median {:?}, p99 {:?}",
        samples[0],
        samples[LATENCY_RUNS / 2],
        samples[LATENCY_RUNS * 99 / 100],
    );
    Ok(())
}

fn load_kernel(kernel: &Buffer) {
    let base = kernel.ptr::<u32>();
    // SAFETY: both ranges fit in the mapped 4 KiB allocation.
    unsafe {
        for (index, word) in DESCRIPTOR.iter().enumerate() {
            base.add(index).write_volatile(*word);
        }
        for (index, word) in CODE.iter().enumerate() {
            base.add(CODE_OFFSET / 4 + index).write_volatile(*word);
        }
    }
}

fn check(name: &str, buffer: &Buffer) -> Result<(), String> {
    let base = buffer.ptr::<u32>();
    let mut wrong = 0;
    let mut first = None;
    for index in 0..ITEMS {
        // SAFETY: the buffer holds `ITEMS` mapped words.
        let word = unsafe { base.add(index as usize).read_volatile() };
        let expected = index.wrapping_mul(MULTIPLIER);
        if word != expected {
            wrong += 1;
            first.get_or_insert((index, word, expected));
        }
    }
    if let Some((index, word, expected)) = first {
        return Err(format!(
            "{name}: {wrong} of {ITEMS} words wrong; word {index} is {word:#x}, expected {expected:#x}"
        ));
    }
    println!(
        "ok   {name}: {ITEMS} words from {} workgroups",
        ITEMS / u32::from(WORKGROUP)
    );
    Ok(())
}
