/// Decode an Annex B stream with `OrderedDecoder` and write raw YUV 4:2:0
/// frames in display order: the decoder command for the conformance runner
/// (`tools/conformance/run.sh`).
///
/// Usage: cargo run --release --example conformance_decode -- <input.h264> <output.yuv>
///
/// Exits with status 1 on a decode error (unsupported streams included).
use rust_h264::decoder::{Frame, OrderedDecoder};
use rust_h264::nal::parse_annex_b;
use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: conformance_decode <input.h264> <output.yuv>");
        std::process::exit(2);
    }
    let data =
        std::fs::read(&args[1]).unwrap_or_else(|e| panic!("failed to read {}: {e}", args[1]));
    let file = std::fs::File::create(&args[2])
        .unwrap_or_else(|e| panic!("failed to create {}: {e}", args[2]));
    let mut out = std::io::BufWriter::new(file);
    let mut write = |frames: Vec<Frame>| {
        for f in frames {
            out.write_all(&f.y).unwrap();
            out.write_all(&f.u).unwrap();
            out.write_all(&f.v).unwrap();
        }
    };
    let mut decoder = OrderedDecoder::new();
    for nal in parse_annex_b(&data) {
        match decoder.decode_nal(&nal) {
            Ok(frames) => write(frames),
            Err(e) => {
                eprintln!("decode error: {e:?}");
                std::process::exit(1);
            }
        }
    }
    write(decoder.flush());
}
