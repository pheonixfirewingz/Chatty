//! What would FP4 attention and shared-expert weights cost? Generates reference text with the
//! checkpoint's FP8 weights and exact routing, then replays the same tokens with those weights
//! rounded to FP4. A second FP8 replay gives the noise floor. Reads the model; writes only RAM and
//! VRAM.
#![deny(unsafe_code)]

use chatty_metal::{
    catalog::Catalog,
    gpu_model::{GpuModel, Policy},
    reference::Fp4Format,
    tokenizer::Tokenizer,
};
use std::{env, path::Path, process::ExitCode, sync::Arc, time::Instant};

const MODEL: &str = "/debug-model";
const CHUNK: usize = 16;
const SAMPLE_TOKENS: usize = 80;
const TESTS: [(&str, &str); 2] = [
    (
        "roleplay",
        "You are Mira, a sarcastic ship's engineer aboard a failing freighter. Stay in character. The captain storms into engineering and demands to know why the reactor is making that noise. Reply in character and continue the scene with vivid detail.",
    ),
    (
        "coding",
        "Write a Rust function that parses a CSV line with quoted fields and escaped quotes into a Vec<String>, returning an error on an unterminated quote. Include unit tests.",
    ),
];

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

fn kl(exact: &[f64], other: &[f64]) -> f64 {
    exact
        .iter()
        .zip(other)
        .filter(|(p, _)| **p > 1e-12)
        .map(|(p, q)| p * (p / q.max(1e-12)).ln())
        .sum()
}

/// Top-1 agreement and mean KL against the reference, over the same tokens.
fn replay(
    gpu: &mut GpuModel,
    ids: &[u32],
    output: &[u32],
    reference: &[Vec<f64>],
) -> Result<(f64, f64), String> {
    let text = |error: chatty_metal::Error| error.to_string();
    gpu.reset();
    let mut logits = Vec::new();
    for chunk in ids.chunks(CHUNK) {
        logits = gpu.forward(chunk).map_err(text)?;
    }
    let mut agree = 0;
    let mut total = 0.0;
    for (step, &token) in output.iter().enumerate() {
        agree += i32::from(argmax(&logits) == token);
        total += kl(&reference[step], &softmax(&logits));
        logits = gpu.forward(&[token]).map_err(text)?;
    }
    let n = output.len().max(1) as f64;
    Ok((f64::from(agree) * 100.0 / n, total / n))
}

fn run() -> Result<(), String> {
    let text = |error: chatty_metal::Error| error.to_string();
    let tokens: usize = env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .ok_or("usage: metal-fp4-test TOKENS")?;
    let dir = Path::new(MODEL);
    let tokenizer = Tokenizer::open(&dir.join("tokenizer.json")).map_err(text)?;
    let catalog = Arc::new(Catalog::open(dir).map_err(text)?);
    let mut gpu = GpuModel::load(catalog, &tokenizer).map_err(text)?;
    gpu.set_policy(Policy::EXACT);
    let stop = tokenizer.special("<｜end▁of▁sentence｜>").map_err(text)?;

    let mut cases = Vec::new();
    for (name, prompt) in TESTS {
        let mut ids = vec![
            tokenizer.special("<｜begin▁of▁sentence｜>").map_err(text)?,
            tokenizer.special("<｜User｜>").map_err(text)?,
        ];
        ids.extend(tokenizer.encode(prompt));
        ids.push(tokenizer.special("<｜Assistant｜>").map_err(text)?);
        ids.push(tokenizer.special("</think>").map_err(text)?);
        gpu.reset();
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
            "--- {name} FP8 reference, {} tokens in {:.1?}\n{}\n---",
            output.len(),
            start.elapsed(),
            tokenizer.decode(&output)
        );
        cases.push((name, ids, output, reference));
    }

    println!("weights          prompt    top-1 agree  mean KL");
    for (name, ids, output, reference) in &cases {
        let (agree, divergence) = replay(&mut gpu, ids, output, reference)?;
        println!("FP8 (noise floor) {name:9} {agree:9.1}% {divergence:8.3}");
    }
    for (label, format) in [("NVFP4", Fp4Format::Nv), ("MXFP4", Fp4Format::Mx)] {
        gpu.set_core_fp4(Some(format)).map_err(text)?;
        for (name, ids, output, reference) in &cases {
            let (agree, divergence) = replay(&mut gpu, ids, output, reference)?;
            println!("{label:17} {name:9} {agree:9.1}% {divergence:8.3}");
        }
        for (name, ids, _, _) in &cases {
            gpu.reset();
            let mut logits = Vec::new();
            for chunk in ids.chunks(CHUNK) {
                logits = gpu.forward(chunk).map_err(text)?;
            }
            let mut sample = Vec::new();
            for _ in 0..SAMPLE_TOKENS {
                let next = argmax(&logits);
                if next == stop {
                    break;
                }
                sample.push(next);
                logits = gpu.forward(&[next]).map_err(text)?;
            }
            println!(
                "--- {label} {name} free run\n{}\n---",
                tokenizer.decode(&sample)
            );
        }
    }
    Ok(())
}
