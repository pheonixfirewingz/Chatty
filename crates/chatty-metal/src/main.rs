use chatty_metal::{
    catalog::Catalog,
    plan::{Budget, GIB, Plan},
};
use std::{path::PathBuf, process::ExitCode};

const USAGE: &str = "usage: chatty-metal plan --model-dir DIR [--vram-gib N] [--ram-gib N] [--engram-ram-gib N] [--context N]";

fn main() -> ExitCode {
    if rustix::process::getpid().is_init() {
        chatty_metal::boot::run();
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let command = args.next();
    // Serves the debug endpoints without being PID 1, to test them off the hardware.
    #[cfg(feature = "debug-agent")]
    if command.as_deref() == Some("debug-agent") {
        let address = args.next().ok_or(USAGE)?;
        chatty_metal::server::serve_on(&address, "local debug agent\n");
    }
    if command.as_deref() != Some("plan") {
        return Err(USAGE.to_owned());
    }
    let mut model_dir = None;
    let mut budget = Budget::default();
    while let Some(flag) = args.next() {
        let value = args.next().ok_or(USAGE)?;
        let number = || {
            value
                .parse::<u64>()
                .map_err(|_| format!("{flag}: not a number"))
        };
        let gib = || {
            number()?
                .checked_mul(GIB)
                .ok_or_else(|| format!("{flag}: too large"))
        };
        match flag.as_str() {
            "--model-dir" => model_dir = Some(PathBuf::from(&value)),
            "--vram-gib" => budget.vram = gib()?,
            "--ram-gib" => budget.ram = gib()?,
            "--engram-ram-gib" => budget.engram_ram = gib()?,
            "--context" => budget.context = number()?,
            _ => return Err(USAGE.to_owned()),
        }
    }
    let model_dir = model_dir.ok_or(USAGE)?;

    let catalog = Catalog::open(&model_dir).map_err(|error| error.to_string())?;
    let plan = Plan::new(&catalog, budget).map_err(|error| error.to_string())?;
    let sizes = catalog.sizes;
    let gib = |bytes: u64| bytes as f64 / GIB as f64;
    let cached = plan.vram_expert_slots + plan.ram_expert_slots;
    println!("shards            {}", catalog.shards.len());
    println!("on drive          {:.2} GiB", gib(plan.drive_total));
    println!(
        "skipped for text  {:.2} GiB (mtp + vision)",
        gib(sizes.mtp + sizes.vision)
    );
    println!("VRAM core         {:.2} GiB", gib(plan.core));
    println!(
        "VRAM KV           {:.2} GiB at context {}",
        gib(plan.kv),
        budget.context
    );
    println!(
        "expert slot       {:.2} MiB",
        catalog.expert_slot as f64 / (1u64 << 20) as f64
    );
    println!(
        "VRAM experts      {} of {}",
        plan.vram_expert_slots, plan.expert_total
    );
    println!(
        "RAM experts       {} of {}",
        plan.ram_expert_slots, plan.expert_total
    );
    println!(
        "experts cached    {:.1}%",
        cached as f64 * 100.0 / plan.expert_total as f64
    );
    println!(
        "engram cached     {:.2} of {:.2} GiB",
        gib(plan.engram_ram),
        gib(plan.engram_total)
    );
    Ok(())
}
