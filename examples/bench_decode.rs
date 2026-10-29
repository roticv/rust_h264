use rust_h264::decoder::{Decoder, Frame};
use rust_h264::nal::{parse_annex_b, NalUnit};
use rust_h264::sha256::sha256_hex;
use rust_h264::threading::ThreadedDecoder;
use std::alloc::{GlobalAlloc, Layout, System};
#[cfg(not(feature = "profile"))]
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(not(feature = "profile"))]
use std::time::Duration;
use std::time::Instant;

struct CountingAllocator;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static DEALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        DEALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

#[cfg(not(feature = "profile"))]
const STREAMS: &[&str] = &[
    "bench_720p_300f_ponly.h264",
    "bench_720p_300f_ponly_complex.h264",
    "bench_720p_300f_bframes.h264",
    "bench_720p_300f_bframes_complex.h264",
    "bench_1080p_100f.h264",
    "bench_1080p_100f_complex.h264",
    "bench_1080p_100f_ponly.h264",
];

#[cfg(feature = "profile")]
const PROFILE_STREAM: &str = "bench_1080p_100f_complex.h264";

#[derive(Clone)]
struct DecodeResult {
    frames: usize,
    digest: String,
    allocs: usize,
}

/// Parse and decode a stream; the digest of the output is left to the caller.
#[cfg(not(feature = "profile"))]
fn decode_once(data: &[u8]) -> (DecodeResult, Vec<Frame>) {
    let nals = parse_annex_b(data);
    decode_frames(&nals)
}

/// Worker count from `BENCH_THREADS` (unset or 0: serial `Decoder`).
fn bench_threads() -> usize {
    std::env::var("BENCH_THREADS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

#[cfg(feature = "profile")]
fn decode_nals(nals: &[NalUnit], compute_digest: bool) -> DecodeResult {
    let (mut result, frames) = decode_frames(nals);
    if compute_digest {
        result.digest = digest_frames(&frames);
    }
    result
}

/// Decode `nals` into display-order frames (no digest).
fn decode_frames(nals: &[NalUnit]) -> (DecodeResult, Vec<Frame>) {
    let before_allocs = ALLOCATIONS.load(Ordering::Relaxed);
    let mut frames = Vec::new();
    let threads = bench_threads();
    if threads > 0 {
        let mut decoder = ThreadedDecoder::new(threads);
        for nal in nals {
            frames.extend(decoder.decode_nal(nal).unwrap());
        }
        frames.extend(decoder.flush());
    } else {
        let mut decoder = Decoder::new();
        for nal in nals {
            if let Some(frame) = decoder.decode_nal(nal).unwrap() {
                frames.push(frame);
            }
        }
        if let Some(frame) = decoder.flush() {
            frames.push(frame);
        }
    }
    let allocs = ALLOCATIONS.load(Ordering::Relaxed) - before_allocs;

    frames.sort_by_key(|frame| frame.pic_order_cnt);
    let result = DecodeResult {
        frames: frames.len(),
        digest: String::new(),
        allocs,
    };
    (result, frames)
}

fn digest_frames(frames: &[Frame]) -> String {
    let mut output = Vec::new();
    for frame in frames {
        output.extend_from_slice(&frame.y);
        output.extend_from_slice(&frame.u);
        output.extend_from_slice(&frame.v);
    }
    sha256_hex(&output)
}

#[cfg(not(feature = "profile"))]
/// Time parsing and decoding only: hashing the output (several hundred MB
/// for the 1080p streams) would otherwise take about as long as decoding.
fn decode_timed(data: &[u8]) -> (DecodeResult, Duration) {
    let start = Instant::now();
    let (mut result, frames) = decode_once(data);
    let elapsed = start.elapsed();
    result.digest = digest_frames(&frames);
    (result, elapsed)
}

#[cfg(not(feature = "profile"))]
fn has_tool(name: &str) -> bool {
    Command::new("which")
        .arg(name)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[cfg(not(feature = "profile"))]
fn generate_missing_1080p_ponly(path: &str) -> bool {
    if !has_tool("ffmpeg") {
        return false;
    }

    Command::new("ffmpeg")
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=1920x1080:rate=30:duration=3.33",
            "-frames:v",
            "100",
            "-c:v",
            "libx264",
            "-preset",
            "medium",
            "-crf",
            "23",
            "-x264opts",
            "bframes=0:ref=1:no-deblock:keyint=250:min-keyint=25:cabac=1",
            "-f",
            "h264",
            path,
        ])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(feature = "profile")]
fn main() {
    profile_main();
}

#[cfg(not(feature = "profile"))]
fn main() {
    bench_main();
}

#[cfg(not(feature = "profile"))]
fn bench_main() {
    let args: Vec<String> = std::env::args().collect();
    let warmup = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(3usize);
    let measured = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(5usize);
    let testdata = format!("{}/testdata", env!("CARGO_MANIFEST_DIR"));
    let missing_ponly = format!("{testdata}/bench_1080p_100f_ponly.h264");

    if !std::path::Path::new(&missing_ponly).exists()
        && !generate_missing_1080p_ponly(&missing_ponly)
    {
        eprintln!("[MISSING] 1080p ponly cabac — generate with x264");
    }

    match bench_threads() {
        0 => println!("decoder: serial Decoder (set BENCH_THREADS=N for ThreadedDecoder)"),
        n => println!("decoder: ThreadedDecoder with {n} threads"),
    }
    println!("stream | frames | decode_ms | fps | allocs | sha256(prefix)");
    println!("--- | ---: | ---: | ---: | ---: | ---");

    for stream in STREAMS {
        let path = format!("{testdata}/{stream}");
        let Ok(data) = std::fs::read(&path) else {
            println!("{stream} | MISSING | - | - | - | -");
            continue;
        };

        for _ in 0..warmup {
            let _ = decode_once(&data);
        }

        let mut best: Option<(DecodeResult, Duration)> = None;
        for _ in 0..measured {
            let current = decode_timed(&data);
            if best
                .as_ref()
                .map(|(_, best_elapsed)| current.1 < *best_elapsed)
                .unwrap_or(true)
            {
                best = Some(current);
            }
        }

        let (result, elapsed) = best.unwrap();
        let decode_ms = elapsed.as_secs_f64() * 1000.0;
        let fps = result.frames as f64 / elapsed.as_secs_f64();
        let prefix = &result.digest[..16];
        println!(
            "{stream} | {} | {:.3} | {:.2} | {} | {prefix}",
            result.frames, decode_ms, fps, result.allocs
        );
        println!("golden {stream}: {}", result.digest);
    }
}

#[cfg(feature = "profile")]
fn profile_main() {
    use std::collections::HashMap;
    use std::fs::File;

    let profile_iterations = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(3usize);
    let path = format!("{}/testdata/{PROFILE_STREAM}", env!("CARGO_MANIFEST_DIR"));
    let data = std::fs::read(&path).unwrap_or_else(|err| panic!("failed to read {path}: {err}"));
    let nals = parse_annex_b(&data);

    println!("profile stream: {PROFILE_STREAM}");
    println!("warmup decode...");
    let warmup = decode_nals(&nals, true);
    println!(
        "warmup frames={} allocs={} sha256={}",
        warmup.frames, warmup.allocs, warmup.digest
    );

    let guard = pprof::ProfilerGuardBuilder::default()
        .frequency(1000)
        .blocklist(&["libc", "libsystem", "pthread"])
        .build()
        .expect("failed to start pprof profiler");

    rust_h264::profile::set_enabled(false);
    rust_h264::profile::reset();
    let start = Instant::now();
    let mut profiled_frames = 0usize;
    let mut profiled_allocs = 0usize;
    for _ in 0..profile_iterations {
        let profiled = decode_nals(&nals, false);
        profiled_frames += profiled.frames;
        profiled_allocs += profiled.allocs;
    }
    let elapsed = start.elapsed();

    let report = guard
        .report()
        .build()
        .expect("failed to build pprof report");
    drop(guard);
    let svg_path = format!(
        "{}/target/profile-1080p-complex.svg",
        env!("CARGO_MANIFEST_DIR")
    );
    let file =
        File::create(&svg_path).unwrap_or_else(|err| panic!("failed to create {svg_path}: {err}"));
    report
        .flamegraph(file)
        .unwrap_or_else(|err| panic!("failed to write flamegraph {svg_path}: {err}"));

    let sample_total: isize = report.data.values().sum();
    println!(
        "profiled iterations={} frames={} decode_ms={:.3} fps={:.2} allocs={} sha256={}",
        profile_iterations,
        profiled_frames,
        elapsed.as_secs_f64() * 1000.0,
        profiled_frames as f64 / elapsed.as_secs_f64(),
        profiled_allocs,
        warmup.digest
    );
    println!("samples={} flamegraph={svg_path}", sample_total);

    rust_h264::profile::reset();
    rust_h264::profile::set_enabled(true);
    let phase_start = Instant::now();
    let mut phase_frames = 0usize;
    for _ in 0..profile_iterations {
        phase_frames += decode_nals(&nals, false).frames;
    }
    let phase_elapsed = phase_start.elapsed();
    rust_h264::profile::set_enabled(false);
    let phase_samples = rust_h264::profile::snapshot();

    println!(
        "phase iterations={} frames={} decode_ms={:.3} fps={:.2}",
        profile_iterations,
        phase_frames,
        phase_elapsed.as_secs_f64() * 1000.0,
        phase_frames as f64 / phase_elapsed.as_secs_f64()
    );
    // Phases nest (see profile.rs). Top-level phases are the children of
    // decode_nal_total; anything decode_nal_total covers that they do not is
    // reported as "unaccounted". Nested phases are indented and excluded
    // from that sum.
    println!("phase timing:");
    println!(
        "(motion_comp is timed per MC call, down to 4x4 blocks, so timer overhead \
         inflates it; use the sampled flamegraph for its true share)"
    );
    println!("phase | us | percent_of_wall | percent_of_decode_nal");
    println!("--- | ---: | ---: | ---:");
    let wall_us = phase_elapsed.as_micros() as f64;
    let total_us = phase_samples
        .iter()
        .find(|s| s.name == "decode_nal_total")
        .map(|s| s.micros)
        .unwrap_or(0);
    let pct = |us: u64, denom: f64| {
        if denom > 0.0 {
            us as f64 * 100.0 / denom
        } else {
            0.0
        }
    };
    let mut top_level_sum = 0u64;
    for sample in phase_samples {
        let indent = match sample.parent {
            None => "",
            Some("decode_nal_total") => {
                top_level_sum += sample.micros;
                "  "
            }
            Some("mb_loop") => "    ",
            Some(_) => "      ",
        };
        println!(
            "{}{} | {} | {:.2}% | {:.2}%",
            indent,
            sample.name,
            sample.micros,
            pct(sample.micros, wall_us),
            pct(sample.micros, total_us as f64)
        );
    }
    let unaccounted = total_us.saturating_sub(top_level_sum);
    println!(
        "  unaccounted | {} | {:.2}% | {:.2}%",
        unaccounted,
        pct(unaccounted, wall_us),
        pct(unaccounted, total_us as f64)
    );
    println!("top self-time functions:");
    println!("rank | samples | percent | function");
    println!("---: | ---: | ---: | ---");

    let mut self_counts: HashMap<String, isize> = HashMap::new();
    for (frames, count) in &report.data {
        let name = frames
            .frames
            .first()
            .and_then(|frame| frame.first())
            .map(|symbol| symbol.name().to_string())
            .unwrap_or_else(|| "<unknown>".to_string());
        *self_counts.entry(name).or_default() += *count;
    }
    let mut top: Vec<_> = self_counts.into_iter().collect();
    top.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    for (idx, (name, count)) in top.into_iter().take(20).enumerate() {
        let percent = if sample_total > 0 {
            count as f64 * 100.0 / sample_total as f64
        } else {
            0.0
        };
        println!("{} | {} | {:.2}% | {}", idx + 1, count, percent, name);
    }
}
