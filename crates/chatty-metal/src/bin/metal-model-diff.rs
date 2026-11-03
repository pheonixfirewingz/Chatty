//! Runs the same tokens through the CPU reference and the GPU model and reports how far apart they
//! are at each sub-step. FP8 rounding amplifies float noise, as one element near a rounding boundary
//! jumps a whole quantum, so activations agree closely but not exactly. Failure here means the top
//! token changed or a layer drifted past the tolerance. Reads the model; writes only RAM and VRAM.
#![deny(unsafe_code)]

use chatty_metal::{catalog::Catalog, gpu_model::GpuModel, reference::Model, tokenizer::Tokenizer};
use std::{env, path::Path, process::ExitCode, sync::Arc, time::Instant};

const MODEL: &str = "/debug-model";
const EXPERT_RAM_GIB: u64 = 20;
const LAYER_TOLERANCE: f32 = 0.1;
const CHUNK: usize = 1;

fn main() -> ExitCode {
    match run() {
        Ok(true) => {
            println!("PASS");
            ExitCode::SUCCESS
        }
        Ok(false) => {
            println!("FAIL: activations differ");
            ExitCode::FAILURE
        }
        Err(error) => {
            println!("FAIL: {error}");
            ExitCode::FAILURE
        }
    }
}

fn top(logits: &[f32]) -> Vec<(u32, f32)> {
    let mut order: Vec<(u32, f32)> = logits
        .iter()
        .enumerate()
        .map(|(i, v)| (i as u32, *v))
        .collect();
    order.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));
    order.truncate(5);
    order
}

fn run() -> Result<bool, String> {
    let text = |error: chatty_metal::Error| error.to_string();
    let prompt = env::args().skip(1).collect::<Vec<_>>().join(" ");
    let dir = Path::new(MODEL);
    let tokenizer = Tokenizer::open(&dir.join("tokenizer.json")).map_err(text)?;
    let catalog = Arc::new(Catalog::open(dir).map_err(text)?);
    let mut ids = vec![
        tokenizer.special("<｜begin▁of▁sentence｜>").map_err(text)?,
        tokenizer.special("<｜User｜>").map_err(text)?,
    ];
    ids.extend(tokenizer.encode(&prompt));
    ids.push(tokenizer.special("<｜Assistant｜>").map_err(text)?);
    ids.push(tokenizer.special("</think>").map_err(text)?);

    let start = Instant::now();
    let mut gpu = GpuModel::load(catalog.clone(), &tokenizer).map_err(text)?;
    println!("gpu model: {:.2?}", start.elapsed());
    let mut cpu = Model::load(catalog, &tokenizer, EXPERT_RAM_GIB).map_err(text)?;
    println!("both loaded: {:.2?}", start.elapsed());
    cpu.probes = Some(Vec::new());
    gpu.probes = Some(Vec::new());
    cpu.fingerprints = Some(Vec::new());
    gpu.fingerprints = Some(Vec::new());

    let mut clean = true;
    for (step, chunk) in ids.chunks(CHUNK).enumerate() {
        let start = Instant::now();
        let cpu_logits = cpu.forward(chunk).map_err(text)?;
        let cpu_time = start.elapsed();
        let start = Instant::now();
        let gpu_logits = gpu.forward(chunk).map_err(text)?;
        let gpu_time = start.elapsed();
        let cp = cpu.fingerprints.replace(Vec::new()).unwrap_or_default();
        let gp = gpu.fingerprints.replace(Vec::new()).unwrap_or_default();
        let mut worst = (0usize, 0.0f32);
        let mut first_bad = None;
        for (a, b) in cp.iter().zip(&gp) {
            let relative = (a.2 - b.2).abs() / a.2.max(1e-6);
            if relative > worst.1 {
                worst = (a.0, relative);
            }
            if relative > LAYER_TOLERANCE && first_bad.is_none() {
                first_bad = Some((a.0, a.2, b.2, a.1, b.1));
            }
        }
        if step == 0 {
            let cpu_probes = cpu.probes.take().unwrap_or_default();
            let gpu_probes = gpu.probes.take().unwrap_or_default();
            for (name, want) in &cpu_probes {
                let Some((_, got)) = gpu_probes.iter().find(|(n, _)| n == name) else {
                    continue;
                };
                let peak = want.iter().fold(1e-30f32, |m, v| m.max(v.abs()));
                let mut worst = (0usize, 0.0f32);
                for (i, (a, b)) in want.iter().zip(got).enumerate() {
                    let e = (a - b).abs();
                    if e > worst.1 || e.is_nan() {
                        worst = (i, e);
                    }
                }
                println!(
                    "  {name:12} worst {:.2e} of peak {peak:.2e} at {}: cpu {} gpu {}",
                    worst.1, worst.0, want[worst.0], got[worst.0]
                );
            }
        }
        let (a, b) = (top(&cpu_logits), top(&gpu_logits));
        println!(
            "step {step} ({} tokens): cpu {cpu_time:.2?}, gpu {gpu_time:.2?}; worst norm error {:.2e} at layer {}; top cpu {:?} gpu {:?}",
            chunk.len(),
            worst.1,
            worst.0,
            &a[..2],
            &b[..2],
        );
        if let Some((layer, cn, gn, cm, gm)) = first_bad {
            println!("  first disagreement: layer {layer}, norm {cn} vs {gn}, mean {cm} vs {gm}");
            clean = false;
            break;
        }
        if a[0].0 != b[0].0 {
            println!("  top token differs");
            clean = false;
        }
    }
    let timing = gpu.timing();
    println!("gpu timing: {timing:?}");
    Ok(clean)
}
