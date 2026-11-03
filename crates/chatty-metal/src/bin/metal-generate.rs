//! Generate text, run with `metal_run`, and report what routing actually does to the expert
//! caches. `--cpu` uses the reference. Touches only the read-only model, RAM, and VRAM.
#![deny(unsafe_code)]

use chatty_metal::{
    catalog::Catalog,
    gpu_model::{self, GpuModel, Missing, Policy},
    plan::{Budget, Plan},
    reference::{self, Model},
    routing::RouteTrace,
    tokenizer::Tokenizer,
};
use std::{
    env,
    io::{self, Write},
    path::Path,
    process::ExitCode,
    sync::Arc,
    time::Instant,
};

const MODEL: &str = "/debug-model";
const EXPERT_RAM_GIB: u64 = 30;
const USAGE: &str = "usage: metal-generate [--cpu] [--profile] [--serial] [--drop-cold] [--stand-in] [--host-budget N] [--tokens N] [--temperature T] [--chunk N] PROMPT...";

enum Engine {
    Cpu(Box<Model>),
    Gpu(Box<GpuModel>),
}

impl Engine {
    fn forward(&mut self, ids: &[u32]) -> chatty_metal::Result<Vec<f32>> {
        match self {
            Self::Cpu(model) => model.forward(ids),
            Self::Gpu(model) => model.forward(ids),
        }
    }

    fn trace(&self) -> &RouteTrace {
        match self {
            Self::Cpu(model) => &model.trace,
            Self::Gpu(model) => &model.trace,
        }
    }
}

struct Options {
    tokens: usize,
    temperature: f32,
    chunk: usize,
    cpu: bool,
    profile: bool,
    serial: bool,
    exact: bool,
    host_budget: usize,
    missing: Missing,
    prompt: String,
}

fn parse() -> Result<Options, String> {
    let mut options = Options {
        tokens: 32,
        temperature: 0.0,
        chunk: 16,
        cpu: false,
        profile: false,
        serial: false,
        exact: true,
        host_budget: usize::MAX,
        missing: Missing::DropRenormalized,
        prompt: String::new(),
    };
    let mut words = Vec::new();
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| USAGE.to_owned());
        match arg.as_str() {
            "--cpu" => options.cpu = true,
            "--profile" => options.profile = true,
            "--serial" => options.serial = true,
            "--drop-cold" => options.exact = false,
            "--stand-in" => options.missing = Missing::StandIn,
            "--host-budget" => options.host_budget = value()?.parse().map_err(|_| USAGE)?,
            "--tokens" => options.tokens = value()?.parse().map_err(|_| USAGE)?,
            "--temperature" => options.temperature = value()?.parse().map_err(|_| USAGE)?,
            "--chunk" => options.chunk = value()?.parse().map_err(|_| USAGE)?,
            _ => words.push(arg),
        }
    }
    let limit = if options.cpu {
        reference::MAX_CHUNK
    } else {
        gpu_model::MAX_CHUNK
    };
    if words.is_empty() || options.chunk == 0 || options.chunk > limit {
        return Err(USAGE.to_owned());
    }
    options.prompt = words.join(" ");
    Ok(options)
}

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
    let options = parse()?;
    let text = |error: chatty_metal::Error| error.to_string();
    let dir = Path::new(MODEL);

    let start = Instant::now();
    let tokenizer = Tokenizer::open(&dir.join("tokenizer.json")).map_err(text)?;
    let catalog = Arc::new(Catalog::open(dir).map_err(text)?);
    let plan = Plan::new(&catalog, Budget::default()).map_err(text)?;
    println!("tokenizer and catalog: {:.2?}", start.elapsed());

    let start = Instant::now();
    let mut model = if options.cpu {
        Engine::Cpu(Box::new(
            Model::load(catalog, &tokenizer, EXPERT_RAM_GIB).map_err(text)?,
        ))
    } else {
        Engine::Gpu(Box::new(GpuModel::load(catalog, &tokenizer).map_err(text)?))
    };
    println!("core weights and tables: {:.2?}", start.elapsed());

    if let Engine::Gpu(gpu) = &mut model {
        if options.profile {
            gpu.enable_profile();
        }
        gpu.set_serial(options.serial);
        gpu.set_policy(if options.exact {
            Policy::EXACT
        } else {
            Policy {
                missing: options.missing,
                host_budget: options.host_budget,
                must_run: 0,
                ram_ranks: 6,
            }
        });
    }

    let mut ids = vec![
        tokenizer.special("<｜begin▁of▁sentence｜>").map_err(text)?,
        tokenizer.special("<｜User｜>").map_err(text)?,
    ];
    ids.extend(tokenizer.encode(&options.prompt));
    ids.push(tokenizer.special("<｜Assistant｜>").map_err(text)?);
    ids.push(tokenizer.special("</think>").map_err(text)?);
    let stop = tokenizer.special("<｜end▁of▁sentence｜>").map_err(text)?;
    println!("prompt: {} tokens", ids.len());

    let start = Instant::now();
    let mut logits = Vec::new();
    for chunk in ids.chunks(options.chunk) {
        logits = model.forward(chunk).map_err(text)?;
    }
    let prompt_time = start.elapsed();
    println!(
        "prefill: {prompt_time:.2?} ({:.2} tokens/s)",
        ids.len() as f64 / prompt_time.as_secs_f64()
    );
    if let Engine::Gpu(gpu) = &model {
        println!("prefill timing: {:?}", gpu.timing());
    }

    let mut rng = 0x9E37_79B9_7F4A_7C15_u64;
    let mut output = Vec::new();
    let mut pending = Vec::new();
    let mut times = Vec::with_capacity(options.tokens);
    let start = Instant::now();
    for _ in 0..options.tokens {
        let next = sample(&logits, options.temperature, &mut rng);
        if next == stop {
            break;
        }
        output.push(next);
        pending.extend(tokenizer.token_bytes(next));
        // Print only complete UTF-8, as a token can end in the middle of a character.
        let valid = match std::str::from_utf8(&pending) {
            Ok(_) => pending.len(),
            Err(error) => error.valid_up_to(),
        };
        if valid > 0 {
            print!("{}", String::from_utf8_lossy(&pending[..valid]));
            let _ = io::stdout().flush();
            pending.drain(..valid);
        }
        let step = Instant::now();
        logits = model.forward(&[next]).map_err(text)?;
        times.push(step.elapsed());
    }
    let decode_time = start.elapsed();
    if !times.is_empty() {
        let last_half = times.len() / 2;
        let tail = &times[last_half..];
        let tail_time: f64 = tail.iter().map(|t| t.as_secs_f64()).sum();
        println!();
        println!(
            "second half of decode: {:.2} tokens/s; fastest step {:.2?}",
            tail.len() as f64 / tail_time,
            times.iter().min().unwrap_or(&decode_time),
        );
    }
    println!();
    println!(
        "decode: {} tokens in {decode_time:.2?} ({:.2} tokens/s)",
        output.len(),
        output.len() as f64 / decode_time.as_secs_f64().max(1e-9)
    );
    match &model {
        Engine::Cpu(cpu) => println!("expert RAM: {:?}", cpu.expert_counters()),
        Engine::Gpu(gpu) => println!("total timing: {:?}", gpu.timing()),
    }
    if let Engine::Gpu(gpu) = &model {
        for (name, count, time) in gpu.profile() {
            println!("  {name:16} {count:>7} dispatches {time:>10.2?}");
        }
    }
    report(model.trace(), &plan);
    Ok(())
}

fn report(trace: &RouteTrace, plan: &Plan) {
    let s = trace.summary();
    println!(
        "routing: {} tokens, {} uses, {} distinct experts of {}",
        trace.tokens(),
        s.uses,
        s.distinct,
        plan.expert_total
    );
    println!(
        "routing: busiest tenth of those takes {:.1}% of uses; {:.1}% repeat from the previous token",
        s.top_tenth_share * 100.0,
        s.repeat_share * 100.0
    );
    let hits = trace.replay(
        plan.vram_expert_slots as usize,
        plan.ram_expert_slots as usize,
    );
    let total = hits.total().max(1) as f64;
    println!(
        "replay from cold, {} VRAM + {} RAM slots: {:.1}% VRAM, {:.1}% RAM, {:.1}% drive",
        plan.vram_expert_slots,
        plan.ram_expert_slots,
        hits.vram as f64 * 100.0 / total,
        hits.ram as f64 * 100.0 / total,
        hits.drive as f64 * 100.0 / total
    );
}

/// Greedy at temperature 0; otherwise the reference's exponential-noise form of softmax sampling.
fn sample(logits: &[f32], temperature: f32, rng: &mut u64) -> u32 {
    let mut best = (f32::NEG_INFINITY, 0);
    for (id, &logit) in logits.iter().enumerate() {
        let score = if temperature <= 0.0 {
            logit
        } else {
            // log(p / e) with e ~ Exp(1), which has the same argmax as p / e.
            *rng ^= *rng << 13;
            *rng ^= *rng >> 7;
            *rng ^= *rng << 17;
            let uniform = ((*rng >> 11) as f64 + 0.5) / (1u64 << 53) as f64;
            logit / temperature.max(1e-5) - (-uniform.ln()).ln() as f32
        };
        if score > best.0 {
            best = (score, id as u32);
        }
    }
    best.1
}
