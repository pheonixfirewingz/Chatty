//! Compile bundled guests into Cargo's build output, never into source folders.
use std::{env, path::PathBuf, process::Command};

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let workspace = manifest.parent().unwrap().parent().unwrap();
    let target = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("wasm");
    for input in ["Cargo.toml", "Cargo.lock", "crates/chatty-adapters"] {
        println!("cargo:rerun-if-changed={}", workspace.join(input).display());
    }

    // A separate target directory avoids taking the parent Cargo invocation's
    // artifact lock. One worker keeps nested builds within a bounded CPU budget.
    let status = Command::new(env::var_os("CARGO").unwrap())
        .current_dir(workspace)
        .args(["build", "--release", "--locked", "--offline", "--jobs", "1"])
        .args(["--target", "wasm32-unknown-unknown", "--target-dir"])
        .arg(&target)
        .args([
            "-p",
            "chatty-adapter-openai-compatible",
            "-p",
            "chatty-adapter-ollama",
            "-p",
            "chatty-adapter-hugging-face",
            "-p",
            "chatty-adapter-freetoken",
            "-p",
            "chatty-adapter-comfyui",
        ])
        // Native compiler/linker flags do not apply to the WASM guests.
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .status()
        .expect("could not start Cargo to build bundled adapters");
    assert!(
        status.success(),
        "bundled adapter build failed; install the matching Rust target with `rustup target add wasm32-unknown-unknown` and cache the locked dependencies"
    );
}
