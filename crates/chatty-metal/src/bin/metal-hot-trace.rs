//! Generates text from varied prompts and prints which experts routing wanted, most used first, as
//! lines for `src/hot_experts.txt`. Reads the model; writes only RAM and VRAM.
#![deny(unsafe_code)]

use chatty_metal::{
    catalog::Catalog,
    gpu_model::{GpuModel, Missing, Policy},
    tokenizer::Tokenizer,
};
use std::{env, path::Path, process::ExitCode, sync::Arc, time::Instant};

const MODEL: &str = "/debug-model";
const CHUNK: usize = 16;
/// VRAM and RAM slots together, rounded up.
const KEEP: usize = 3400;
const PROMPTS: [&str; 8] = [
    "You are Mira, a sarcastic ship's engineer. Stay in character. The captain has just asked why the reactor is making that noise. Reply, then continue the scene.",
    "Continue this roleplay as Elias, a weary innkeeper in a rain-soaked fantasy town. A stranger walks in, soaked and bleeding, and asks for a room. Describe the scene and speak in character.",
    "Write a Python function that merges overlapping intervals, then explain its complexity and write unit tests.",
    "Explain how TCP congestion control works, and compare Reno with BBR.",
    "What should I cook this week on a tight budget? Give me a plan with a shopping list.",
    "Solve step by step: a train leaves at 3pm at 60 km/h, another at 4pm at 90 km/h on the same track. When does the second catch the first?",
    "Summarize the causes of the French Revolution for a high school student.",
    "I had a rough day at work and my manager criticized me in front of everyone. Can we talk about it? I don't know how to respond tomorrow.",
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

fn run() -> Result<(), String> {
    let text = |error: chatty_metal::Error| error.to_string();
    let tokens: usize = env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .ok_or("usage: metal-hot-trace TOKENS")?;
    let dir = Path::new(MODEL);
    let tokenizer = Tokenizer::open(&dir.join("tokenizer.json")).map_err(text)?;
    let catalog = Arc::new(Catalog::open(dir).map_err(text)?);
    let mut gpu = GpuModel::load(catalog, &tokenizer).map_err(text)?;
    gpu.set_policy(Policy {
        missing: Missing::DropRenormalized,
        host_budget: usize::MAX,
        must_run: 3,
        ram_ranks: 6,
    });
    let stop = tokenizer.special("<｜end▁of▁sentence｜>").map_err(text)?;
    for prompt in PROMPTS {
        let start = Instant::now();
        gpu.reset();
        let mut ids = vec![
            tokenizer.special("<｜begin▁of▁sentence｜>").map_err(text)?,
            tokenizer.special("<｜User｜>").map_err(text)?,
        ];
        ids.extend(tokenizer.encode(prompt));
        ids.push(tokenizer.special("<｜Assistant｜>").map_err(text)?);
        ids.push(tokenizer.special("</think>").map_err(text)?);
        let mut logits = Vec::new();
        for chunk in ids.chunks(CHUNK) {
            logits = gpu.forward(chunk).map_err(text)?;
        }
        let mut count = 0;
        for _ in 0..tokens {
            let mut best = (f32::NEG_INFINITY, 0u32);
            for (id, &value) in logits.iter().enumerate() {
                if value > best.0 {
                    best = (value, id as u32);
                }
            }
            if best.1 == stop {
                break;
            }
            logits = gpu.forward(&[best.1]).map_err(text)?;
            count += 1;
        }
        eprintln!("{count} tokens in {:.1?}", start.elapsed());
    }
    let counts = gpu.trace.counts();
    let total: u64 = counts.iter().map(|c| u64::from(c.2)).sum();
    let mut running = 0u64;
    for (rank, entry) in counts.iter().enumerate() {
        running += u64::from(entry.2);
        if matches!(rank + 1, 595 | 760 | 840 | 1000 | 2000 | 3000 | 3400) {
            eprintln!(
                "top {} experts cover {:.1}% of {total} uses",
                rank + 1,
                running as f64 * 100.0 / total as f64
            );
        }
    }
    eprintln!("distinct {}", counts.len());
    println!("BEGIN");
    for (layer, expert, uses) in counts.iter().take(KEEP) {
        println!("{layer} {expert} {uses}");
    }
    println!("END");
    Ok(())
}
