//! Measures what each routing policy trades. It generates reference text with exact routing, then
//! replays the same tokens under each policy, reporting speed, how often the top token still agrees,
//! and how far the distribution moved. Reads the model; writes only RAM and VRAM.
#![deny(unsafe_code)]

use chatty_metal::{
    catalog::Catalog,
    gpu_model::{GpuModel, Missing, Policy},
    tokenizer::Tokenizer,
};
use std::{env, fs, path::Path, process::ExitCode, sync::Arc, time::Instant};

const MODEL: &str = "/debug-model";
const CHUNK: usize = 16;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            println!("FAIL: {error}");
            ExitCode::FAILURE
        }
    }
}

fn argmax(values: &[f32]) -> u32 {
    let mut best = (f32::NEG_INFINITY, 0);
    for (i, &v) in values.iter().enumerate() {
        if v > best.0 {
            best = (v, i as u32);
        }
    }
    best.1
}

fn softmax(logits: &[f32]) -> Vec<f64> {
    let max = logits.iter().fold(f32::NEG_INFINITY, |m, v| m.max(*v));
    let mut p: Vec<f64> = logits.iter().map(|v| f64::from(v - max).exp()).collect();
    let sum: f64 = p.iter().sum();
    p.iter_mut().for_each(|v| *v /= sum);
    p
}

/// KL(exact || other), with the other side floored so a zero cannot make it infinite.
fn kl(exact: &[f64], other: &[f64]) -> f64 {
    exact
        .iter()
        .zip(other)
        .filter(|(p, _)| **p > 1e-12)
        .map(|(p, q)| p * (p / q.max(1e-12)).ln())
        .sum()
}

fn clock(name: &str) -> String {
    let path = format!("/sys/class/drm/card0/device/{name}");
    fs::read_to_string(path)
        .map(|text| {
            text.lines()
                .filter(|line| line.ends_with('*'))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

fn run() -> Result<(), String> {
    let text = |error: chatty_metal::Error| error.to_string();
    let mut args = env::args().skip(1);
    let tokens: usize = args
        .next()
        .and_then(|a| a.parse().ok())
        .ok_or("usage: metal-fidelity TOKENS PROMPT...")?;
    let prompt = args.collect::<Vec<_>>().join(" ");
    let dir = Path::new(MODEL);
    let tokenizer = Tokenizer::open(&dir.join("tokenizer.json")).map_err(text)?;
    let catalog = Arc::new(Catalog::open(dir).map_err(text)?);
    let mut gpu = GpuModel::load(catalog, &tokenizer).map_err(text)?;
    let mut ids = vec![
        tokenizer.special("<｜begin▁of▁sentence｜>").map_err(text)?,
        tokenizer.special("<｜User｜>").map_err(text)?,
    ];
    ids.extend(tokenizer.encode(&prompt));
    ids.push(tokenizer.special("<｜Assistant｜>").map_err(text)?);
    ids.push(tokenizer.special("</think>").map_err(text)?);
    let stop = tokenizer.special("<｜end▁of▁sentence｜>").map_err(text)?;

    let start = Instant::now();
    let preloaded = gpu.preload_hot().map_err(text)?;
    println!("preloaded {preloaded} experts in {:.1?}", start.elapsed());

    gpu.set_policy(Policy::EXACT);
    let start = Instant::now();
    let mut logits = Vec::new();
    for chunk in ids.chunks(CHUNK) {
        logits = gpu.forward(chunk).map_err(text)?;
    }
    let mut reference = vec![softmax(&logits)];
    let mut output = Vec::new();
    for _ in 0..tokens {
        let next = argmax(&logits);
        if next == stop {
            break;
        }
        output.push(next);
        logits = gpu.forward(&[next]).map_err(text)?;
        reference.push(softmax(&logits));
    }
    println!(
        "exact pass: {} tokens in {:.1?}, sclk {}, mclk {}, fclk {}",
        output.len(),
        start.elapsed(),
        clock("pp_dpm_sclk"),
        clock("pp_dpm_mclk"),
        clock("pp_dpm_fclk"),
    );
    println!("--- reference text\n{}\n---", tokenizer.decode(&output));

    let skip = |must_run, ram_ranks| Policy {
        missing: Missing::DropRenormalized,
        host_budget: usize::MAX,
        must_run,
        ram_ranks,
    };
    let policies = [
        ("exact warm", Policy::EXACT),
        ("cold top4", skip(4, 6)),
        ("cold top2", skip(2, 6)),
        ("cold top4, ram 5", skip(4, 5)),
        ("cold top2, ram 5", skip(2, 5)),
        ("cold top2, ram 4", skip(2, 4)),
        ("cold top2, ram 3", skip(2, 3)),
        ("cold top1, ram 2", skip(1, 2)),
    ];
    println!("policy               tok/s  top-1 agree  mean KL  ran%  vram%");
    for (name, policy) in policies {
        gpu.reset();
        gpu.set_policy(policy);
        let before = gpu.timing();
        let mut logits = Vec::new();
        for chunk in ids.chunks(CHUNK) {
            logits = gpu.forward(chunk).map_err(text)?;
        }
        let mut agree = 0;
        let mut total_kl = 0.0;
        let decode_before = gpu.timing();
        let start = Instant::now();
        for (step, &token) in output.iter().enumerate() {
            let p = softmax(&logits);
            if argmax(&logits) == token {
                agree += 1;
            }
            total_kl += kl(&reference[step], &p);
            logits = gpu.forward(&[token]).map_err(text)?;
        }
        let elapsed = start.elapsed().as_secs_f64();
        let after = gpu.timing();
        let vram = (after.vram_runs - before.vram_runs) as f64;
        let ram = (after.ram_runs - before.ram_runs) as f64;
        let skipped = (after.substituted - before.substituted) as f64;
        let n = output.len().max(1) as f64;
        println!(
            "{name:18} {:6.2} {:10.1}% {:8.3} {:5.1} {:6.1}",
            n / elapsed,
            f64::from(agree) * 100.0 / n,
            total_kl / n,
            (vram + ram) * 100.0 / (vram + ram + skipped).max(1.0),
            vram * 100.0 / (vram + ram).max(1.0),
        );
        let end = gpu.timing();
        let per_token = |now: std::time::Duration, was: std::time::Duration| {
            (now - was).as_secs_f64() * 1000.0 / n
        };
        println!(
            "  ms/token: routing {:.1}, cpu {:.1} (drive {:.1}), gpu wait {:.1}, engram {:.1}; fetched/token {:.1}, promoted/token {:.1}, dispatches/token {:.0}, sclk {}",
            per_token(end.experts, decode_before.experts),
            per_token(end.cpu_experts, decode_before.cpu_experts),
            per_token(end.fetch_wait, decode_before.fetch_wait),
            per_token(end.gpu_wait, decode_before.gpu_wait),
            per_token(end.engram, decode_before.engram),
            (end.fetched - decode_before.fetched) as f64 / n,
            (end.promoted - decode_before.promoted) as f64 / n,
            (end.dispatches - decode_before.dispatches) as f64 / n,
            clock("pp_dpm_sclk"),
        );
    }
    println!("hot experts: {:?}", gpu.hot_experts(40));
    Ok(())
}
