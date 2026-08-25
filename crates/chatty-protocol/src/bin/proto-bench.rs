//! Allocation + throughput benchmark for the wire codec.
//! Run: cargo run --release -p chatty-protocol --bin proto-bench
//!
//! Compares the legacy free functions (fresh zstd context per frame, i.e.
//! master's implementation) against ProtocolCodec (persistent contexts +
//! reused scratch buffers) under identical conditions.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use chatty_protocol::{
    HEADER_LEN, MessageType, ProtocolCodec, Response, StreamChunk, decode, read_frame,
    write_message,
};
use tokio::io::DuplexStream;

struct CountingAlloc;

static ALLOCATED: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATED.fetch_add(layout.size() as u64, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATED.fetch_add(new_size as u64, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static A: CountingAlloc = CountingAlloc;

const ITERATIONS: u64 = 2_000;

fn allocated_bytes() -> u64 {
    ALLOCATED.load(Ordering::Relaxed)
}

/// Warmup runs are discarded so lazy init (contexts, arenas) is not measured.
macro_rules! bench {
    ($name:expr, $ops:expr, $case:expr, $scenario:path) => {{
        for _ in 0..50 {
            $scenario($case).await;
        }
        let before_alloc = allocated_bytes();
        let start = Instant::now();
        let mut wire = 0u64;
        for _ in 0..$ops {
            wire += $scenario($case).await;
        }
        let elapsed = start.elapsed();
        let bytes_alloc = allocated_bytes() - before_alloc;
        println!(
            "{:<38} {:>8.1} ms {:>9} ops/s {:>8} B/op {:>10} KB/s",
            $name,
            elapsed.as_secs_f64() * 1e3,
            $ops as u128 * 1_000_000u128 / elapsed.as_micros().max(1) as u128,
            bytes_alloc / $ops,
            wire as u128 * 1024 * 1_000_000u128 / elapsed.as_micros().max(1) as u128 / 1000,
        );
    }};
}

struct Case {
    writer: DuplexStream,
    reader: DuplexStream,
    codec: ProtocolCodec,
    text: String,
}

fn case_with_text(text: String) -> Case {
    let (writer, reader) = tokio::io::duplex(32 * 1024 * 1024);
    Case {
        writer,
        reader,
        codec: ProtocolCodec::new().unwrap(),
        text,
    }
}

fn chunk(text: &str) -> StreamChunk {
    StreamChunk {
        message_id: "message".into(),
        sequence: 1,
        text: text.into(),
    }
}

async fn legacy_stream_chunk(c: &mut Case) -> u64 {
    let value = chunk(&c.text);
    write_message(&mut c.writer, MessageType::StreamChunk, 7, &value)
        .await
        .unwrap();
    let frame = read_frame(&mut c.reader).await.unwrap();
    debug_assert!(frame.compressed);
    debug_assert_eq!(decode::<StreamChunk>(&frame.payload).unwrap().sequence, 1);
    (HEADER_LEN + frame.payload.len()) as u64
}

async fn codec_stream_chunk(c: &mut Case) -> u64 {
    let value = chunk(&c.text);
    c.codec
        .write_message(&mut c.writer, MessageType::StreamChunk, 7, &value)
        .await
        .unwrap();
    let frame = c.codec.read_frame(&mut c.reader).await.unwrap();
    debug_assert!(frame.compressed);
    debug_assert_eq!(decode::<StreamChunk>(&frame.payload).unwrap().sequence, 1);
    (HEADER_LEN + frame.payload.len()) as u64
}

async fn legacy_pong(c: &mut Case) -> u64 {
    write_message(&mut c.writer, MessageType::Response, 42, &Response::Pong)
        .await
        .unwrap();
    let frame = read_frame(&mut c.reader).await.unwrap();
    debug_assert!(!frame.compressed);
    (HEADER_LEN + frame.payload.len()) as u64
}

async fn codec_pong(c: &mut Case) -> u64 {
    c.codec
        .write_message(&mut c.writer, MessageType::Response, 42, &Response::Pong)
        .await
        .unwrap();
    let frame = c.codec.read_frame(&mut c.reader).await.unwrap();
    debug_assert!(!frame.compressed);
    (HEADER_LEN + frame.payload.len()) as u64
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    println!("=== chatty-protocol codec benchmark (release, single thread) ===\n");

    println!("--- legacy free functions (= main): fresh zstd context per frame ---");
    let mut stream_case = case_with_text("roleplay ".repeat(100));
    bench!(
        "legacy StreamChunk ~900B",
        ITERATIONS,
        &mut stream_case,
        legacy_stream_chunk
    );
    let mut pong_case = case_with_text(String::new());
    bench!(
        "legacy tiny uncompressed Pong",
        ITERATIONS,
        &mut pong_case,
        legacy_pong
    );
    let mut big_case =
        case_with_text("The lantern flickered against the old stone wall. ".repeat(20_000));
    bench!(
        "legacy 1MB repetitive frame",
        40,
        &mut big_case,
        legacy_stream_chunk
    );

    println!("--- ProtocolCodec: persistent contexts, reused scratch ---");
    let mut stream_case = case_with_text("roleplay ".repeat(100));
    bench!(
        "codec  StreamChunk ~900B",
        ITERATIONS,
        &mut stream_case,
        codec_stream_chunk
    );
    let mut pong_case = case_with_text(String::new());
    bench!(
        "codec  tiny uncompressed Pong",
        ITERATIONS,
        &mut pong_case,
        codec_pong
    );
    let mut big_case =
        case_with_text("The lantern flickered against the old stone wall. ".repeat(20_000));
    bench!(
        "codec  1MB repetitive frame",
        40,
        &mut big_case,
        codec_stream_chunk
    );
}
