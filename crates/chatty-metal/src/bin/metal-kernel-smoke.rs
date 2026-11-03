//! Checks the GPU matrix kernels against the CPU reference on real checkpoint weights, and times
//! them with weights and activations in VRAM. Reads the model; writes only RAM and VRAM.

use chatty_metal::{
    catalog::{Catalog, Extent},
    cpu_fp4,
    gpu_model::GEMM_PRESCALE,
    kfd::{Buffer, Dispatch, Gpu, Memory, Module, Queue, signal_reset, signal_wait},
    reference::{Mat, quant_fp8},
    store::Reader,
};
use std::{
    path::Path,
    process::ExitCode,
    ptr,
    time::{Duration, Instant},
};

const MODEL: &str = "/debug-model";
const GEMM: &[u8] = include_bytes!("../../shaders/model.hsaco");
const WORKGROUP: u16 = 32;
const ROUNDS: usize = 9;
const TOLERANCE: f32 = 2e-3;

#[repr(C)]
#[derive(Clone, Copy)]
struct Args {
    out: u64,
    x: u64,
    weight: u64,
    scale: u64,
    n: u32,
    k: u32,
    x_stride: u32,
    out_stride: u32,
}

struct Rig {
    gpu: Gpu,
    queue: Queue,
    module: Module,
    command: Buffer,
    reader: Reader,
}

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
    let catalog = Catalog::open(Path::new(MODEL)).map_err(text)?;
    let reader = Reader::open(&catalog.shards, 4).map_err(text)?;
    let mut gpu = Gpu::open().map_err(text)?;
    let queue = gpu.create_queue().map_err(text)?;
    let module = gpu.load_module(GEMM).map_err(text)?;
    let command = gpu.alloc(4096, Memory::Host, false).map_err(text)?;
    let mut rig = Rig {
        gpu,
        queue,
        module,
        command,
        reader,
    };

    let tensor = |name: &str| catalog.core[name].extent;
    let wq_b = tensor("layers.0.attn.wq_b.weight");
    let wq_b_scale = tensor("layers.0.attn.wq_b.scale");
    let shared_w1 = tensor("layers.0.ffn.shared_experts.w1.weight");
    let shared_w1_scale = tensor("layers.0.ffn.shared_experts.w1.scale");
    let expert = *catalog.expert(0, 0).ok_or("no expert (0, 0)")?;
    let gate = tensor("layers.0.ffn.gate.weight");
    let head = tensor("head.weight");

    rig.case("fp8 wq_b", "gemm_fp8", wq_b, Some(wq_b_scale), 1280, 32768)?;
    rig.case(
        "fp8 shared w1",
        "gemm_fp8",
        shared_w1,
        Some(shared_w1_scale),
        5120,
        2304,
    )?;
    rig.case(
        "fp4 expert w1",
        "gemm_fp4",
        expert[0],
        Some(expert[1]),
        5120,
        2304,
    )?;
    rig.case(
        "fp4 expert w2",
        "gemm_fp4",
        expert[2],
        Some(expert[3]),
        2304,
        5120,
    )?;
    rig.case("bf16 gate", "gemm_bf16", gate, None, 5120, 384)?;
    rig.case("bf16 head", "gemm_bf16", head, None, 5120, 129_280)?;
    let experts: Vec<_> = (0..7)
        .map(|e| catalog.expert(0, e).copied().ok_or("missing expert"))
        .collect::<Result<_, _>>()?;
    rig.experts(&experts, Memory::VramVisible)?;
    rig.experts(&experts, Memory::HostCached)?;
    rig.cpu_experts(&experts)?;
    rig.overhead()?;
    Ok(())
}

impl Rig {
    /// What a dispatch and a barrier cost with almost no work in them.
    fn overhead(&mut self) -> Result<(), String> {
        const COUNT: usize = 256;
        let text = |e: chatty_metal::Error| e.to_string();
        let entry = self.module.kernel("scale_values").map_err(text)?;
        let target = self
            .gpu
            .alloc(4096, Memory::VramVisible, false)
            .map_err(text)?;
        let signal = self.command.va;
        let kernarg = self.command.va + 64;
        // SAFETY: the kernel arguments fit in the mapped command page.
        unsafe {
            ptr::with_exposed_provenance_mut::<[u64; 2]>(kernarg as usize)
                .write_volatile([target.va, 1.0f32.to_bits() as u64]);
        }
        let dispatch = Dispatch {
            kernel: entry,
            kernarg,
            grid: 1,
            rows: 1,
            workgroup: WORKGROUP,
            signal: 0,
        };
        for (name, fence, barrier_bit, packets) in [
            ("dispatch, no fence", 0, false, false),
            ("dispatch, no fence, barrier bit", 0, true, false),
            ("dispatch, agent fence", 1, false, false),
            ("dispatch, agent fence, barrier bit", 1, true, false),
            ("dispatch + order barrier packet", 0, false, true),
        ] {
            let mut times = Vec::with_capacity(ROUNDS);
            for _ in 0..ROUNDS {
                // SAFETY: the signal sits in the mapped command page.
                unsafe { signal_reset(signal, 1) };
                let start = Instant::now();
                for _ in 0..COUNT {
                    if packets {
                        self.queue.submit_order_barrier();
                    }
                    self.queue
                        .submit_packet(&dispatch.encode_with_fence(fence, barrier_bit));
                }
                self.queue.submit_barrier(signal);
                // SAFETY: as above.
                if !unsafe { signal_wait(signal, Duration::from_secs(30)) } {
                    return Err(format!("{name}: did not complete"));
                }
                times.push(start.elapsed());
            }
            times.sort_unstable();
            println!("{name:36} {:>8.2?} each", times[ROUNDS / 2] / COUNT as u32);
        }
        Ok(())
    }

    /// The same 14 multiplies on the CPU, from ordinary RAM.
    fn cpu_experts(&mut self, experts: &[[Extent; 6]]) -> Result<(), String> {
        let (k, n) = (5120usize, 2304usize);
        let mut x: Vec<f32> = (0..k).map(|i| ((i as f32) * 0.37).sin() * 3.0).collect();
        quant_fp8(&mut x);
        let mut mats = Vec::new();
        for expert in experts {
            for slot in [0, 4] {
                let mut weight = vec![0u8; expert[slot].len as usize];
                let mut scale = vec![0u8; expert[slot + 1].len as usize];
                self.reader
                    .read_extent(expert[slot], &mut weight)
                    .map_err(|e| e.to_string())?;
                self.reader
                    .read_extent(expert[slot + 1], &mut scale)
                    .map_err(|e| e.to_string())?;
                mats.push((weight, scale));
            }
        }
        let mut out = vec![0.0f32; n];
        let mut times = Vec::with_capacity(ROUNDS);
        for _ in 0..ROUNDS {
            let start = Instant::now();
            for (weight, scale) in &mats {
                cpu_fp4::apply(weight, scale, &x, k, &mut out, n);
            }
            times.push(start.elapsed());
        }
        times.sort_unstable();
        let median = times[ROUNDS / 2];
        let macs = (mats.len() * n * k) as f64;
        println!(
            "7 experts w1+w3, CPU AVX2 {median:>10.2?} ({:6.1} GMAC/s)",
            macs / median.as_secs_f64() / 1e9
        );
        let mut want = vec![0.0f32; n];
        Mat::Fp4 {
            weight: &mats[0].0,
            scale: &mats[0].1,
        }
        .apply(&x, k, &mut want, n);
        cpu_fp4::apply(&mats[0].0, &mats[0].1, &x, k, &mut out, n);
        let worst = out
            .iter()
            .zip(&want)
            .fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
        println!("CPU AVX2 worst error against the reference: {worst:.2e}");
        Ok(())
    }

    /// The shape of a real layer: w1 and w3 of 7 experts, as 14 matrix multiplies.
    fn experts(&mut self, experts: &[[Extent; 6]], memory: Memory) -> Result<(), String> {
        let (k, n) = (5120usize, 2304usize);
        let text = |e: chatty_metal::Error| e.to_string();
        let input: Vec<f32> = {
            let mut v: Vec<f32> = (0..k).map(|i| ((i as f32) * 0.37).sin() * 3.0).collect();
            quant_fp8(&mut v);
            v.iter().map(|v| v * GEMM_PRESCALE).collect()
        };
        let x = self
            .gpu
            .alloc((k * 4) as u64, Memory::VramVisible, false)
            .map_err(text)?;
        // SAFETY: `x` maps `k` f32 values.
        unsafe { ptr::copy_nonoverlapping(input.as_ptr(), x.ptr::<f32>(), k) };
        let entry = self.module.kernel("gemm_fp4").map_err(text)?;
        let signal = self.command.va;
        let mut packets = Vec::new();
        for expert in experts {
            for slot in [0, 4] {
                let weight = self.upload_to(expert[slot], memory)?;
                let scale = self.upload_to(expert[slot + 1], memory)?;
                let out = self
                    .gpu
                    .alloc((n * 4) as u64, Memory::VramVisible, false)
                    .map_err(text)?;
                let kernarg = self.command.va + 64 * (1 + packets.len() as u64);
                let args = Args {
                    out: out.va,
                    x: x.va,
                    weight: weight.va,
                    scale: scale.va,
                    n: n as u32,
                    k: k as u32,
                    x_stride: k as u32,
                    out_stride: n as u32,
                };
                // SAFETY: the kernel arguments fit in the mapped command page.
                unsafe {
                    ptr::with_exposed_provenance_mut::<Args>(kernarg as usize).write_volatile(args);
                }
                packets.push(Dispatch {
                    kernel: entry,
                    kernarg,
                    grid: n as u32,
                    rows: 1,
                    workgroup: WORKGROUP,
                    signal: 0,
                });
            }
        }
        self.gpu.flush_cpu_writes();
        let macs = (packets.len() * n * k) as f64;
        for (name, barrier) in [("serial", true), ("overlap", false)] {
            let mut times = Vec::with_capacity(ROUNDS);
            for _ in 0..ROUNDS {
                // SAFETY: the signal sits in the mapped command page.
                unsafe { signal_reset(signal, 1) };
                let start = Instant::now();
                for packet in &packets {
                    self.queue
                        .submit_packet(&packet.encode_with_fence(1, barrier));
                }
                self.queue.submit_barrier(signal);
                // SAFETY: as above.
                if !unsafe { signal_wait(signal, Duration::from_secs(30)) } {
                    return Err(format!("{name}: dispatches did not complete"));
                }
                times.push(start.elapsed());
            }
            times.sort_unstable();
            let median = times[ROUNDS / 2];
            println!(
                "7 experts w1+w3, {memory:?} {name:7} {median:>10.2?} ({:6.1} GMAC/s)",
                macs / median.as_secs_f64() / 1e9
            );
        }
        Ok(())
    }

    fn upload_to(&mut self, extent: Extent, memory: Memory) -> Result<Buffer, String> {
        let mut bytes = vec![0u8; extent.len as usize];
        self.reader
            .read_extent(extent, &mut bytes)
            .map_err(|e| e.to_string())?;
        let buffer = self
            .gpu
            .alloc(extent.len, memory, false)
            .map_err(|e| e.to_string())?;
        // SAFETY: the buffer maps at least `bytes.len()` writable bytes.
        unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), buffer.ptr::<u8>(), bytes.len()) };
        Ok(buffer)
    }

    fn upload(&mut self, extent: Extent) -> Result<(Buffer, Vec<u8>), String> {
        let mut bytes = vec![0u8; extent.len as usize];
        self.reader
            .read_extent(extent, &mut bytes)
            .map_err(|e| e.to_string())?;
        let buffer = self
            .gpu
            .alloc(extent.len, Memory::VramVisible, false)
            .map_err(|e| e.to_string())?;
        // SAFETY: the buffer maps at least `bytes.len()` writable bytes.
        unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), buffer.ptr::<u8>(), bytes.len()) };
        Ok((buffer, bytes))
    }

    fn case(
        &mut self,
        name: &str,
        kernel: &str,
        weight: Extent,
        scale: Option<Extent>,
        k: usize,
        n: usize,
    ) -> Result<(), String> {
        let (weight_vram, weight_bytes) = self.upload(weight)?;
        let scale_loaded = scale.map(|s| self.upload(s)).transpose()?;
        let input: Vec<f32> = {
            let mut v: Vec<f32> = (0..k).map(|i| ((i as f32) * 0.37).sin() * 3.0).collect();
            if kernel != "gemm_bf16" {
                quant_fp8(&mut v);
            }
            v
        };
        let x = self
            .gpu
            .alloc((k * 4) as u64, Memory::VramVisible, false)
            .map_err(|e| e.to_string())?;
        let prescaled: Vec<f32> = if kernel == "gemm_bf16" {
            input.clone()
        } else {
            input.iter().map(|v| v * GEMM_PRESCALE).collect()
        };
        // SAFETY: `x` maps `k` f32 values.
        unsafe { ptr::copy_nonoverlapping(prescaled.as_ptr(), x.ptr::<f32>(), k) };
        let out = self
            .gpu
            .alloc((n * 4) as u64, Memory::VramVisible, false)
            .map_err(|e| e.to_string())?;
        self.gpu.flush_cpu_writes();

        let entry = self.module.kernel(kernel).map_err(|e| e.to_string())?;
        let signal = self.command.va;
        let kernarg = self.command.va + 64;
        let args = Args {
            out: out.va,
            x: x.va,
            weight: weight_vram.va,
            scale: scale_loaded.as_ref().map_or(0, |(b, _)| b.va),
            n: n as u32,
            k: k as u32,
            x_stride: k as u32,
            out_stride: n as u32,
        };
        let mut times = Vec::with_capacity(ROUNDS);
        for _ in 0..ROUNDS {
            // SAFETY: the signal and kernel arguments sit in the mapped command page.
            unsafe {
                ptr::with_exposed_provenance_mut::<Args>(kernarg as usize).write_volatile(args);
                signal_reset(signal, 1);
            }
            let start = Instant::now();
            self.queue.submit(&Dispatch {
                kernel: entry,
                kernarg,
                grid: n as u32,
                rows: 1,
                workgroup: WORKGROUP,
                signal,
            });
            // SAFETY: as above.
            if !unsafe { signal_wait(signal, Duration::from_secs(30)) } {
                return Err(format!("{name}: dispatch did not complete"));
            }
            times.push(start.elapsed());
        }
        times.sort_unstable();

        let mut got = vec![0.0f32; n];
        // SAFETY: `out` maps `n` f32 values and the GPU has finished.
        unsafe { ptr::copy_nonoverlapping(out.ptr::<f32>(), got.as_mut_ptr(), n) };
        let mat = match (kernel, &scale_loaded) {
            ("gemm_fp8", Some((_, s))) => Mat::Fp8 {
                weight: &weight_bytes,
                scale: s,
            },
            ("gemm_fp4", Some((_, s))) => Mat::Fp4 {
                weight: &weight_bytes,
                scale: s,
            },
            _ => Mat::Bf16 {
                weight: &weight_bytes,
            },
        };
        let start = Instant::now();
        let mut want = vec![0.0f32; n];
        mat.apply(&input, k, &mut want, n);
        let cpu = start.elapsed();

        let peak = want.iter().fold(1e-6f32, |m, v| m.max(v.abs()));
        let mut worst = 0.0f32;
        let mut at = 0;
        for (i, (a, b)) in got.iter().zip(&want).enumerate() {
            let error = (a - b).abs();
            if error > worst || error.is_nan() {
                worst = error;
                at = i;
            }
        }
        let median = times[ROUNDS / 2];
        let macs = (n * k) as f64;
        println!(
            "{name:14} {n:>6}x{k:<5} gpu {median:>10.2?} ({:6.1} GMAC/s, {:6.1} GB/s weights), cpu {cpu:>10.2?}, worst error {:.2e} of peak {peak:.2e}",
            macs / median.as_secs_f64() / 1e9,
            weight.len as f64 / median.as_secs_f64() / 1e9,
            worst,
        );
        if worst / peak > TOLERANCE || worst.is_nan() {
            return Err(format!(
                "{name}: element {at} is {}, the reference says {}",
                got[at], want[at]
            ));
        }
        Ok(())
    }
}
