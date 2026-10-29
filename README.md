# rust_h264

While working on rust_media, it was found that there isn't any sufficiently good open source h264 decoder. There is openH264, but it is limited to baseline h264. ffmpeg has its own h264 decoder but it isn't split out as a library.

Hence, the idea is to attempt to create an open source h264 decoder.
Yes, most devices have hardware h264 decoder, but if we want to be truly portable, then software implementation of h264 decoder is needed.

Decodes Baseline, Main, High and High 4:2:2 profile H.264 (8-bit 4:2:0 and 4:2:2, and monochrome output as 4:2:0 with mid-grey chroma):

- CAVLC and CABAC entropy coding, 4x4 and 8x8 transforms, custom scaling matrices
- All partition types (16x16 through 4x4), P and B slices, B-pyramid, spatial and temporal direct mode
- Multiple and long-term reference pictures, reference list modification, memory management operations
- Explicit and implicit weighted prediction
- Multiple slices per picture, per-slice deblocking
- Interlaced content: MBAFF and field pictures
- NEON (aarch64) and SSE2/SSSE3/AVX2 (x86-64) motion compensation
- Optional frame-threaded decoding, and a `no_std` + `alloc` build

Output is byte-identical to FFmpeg on the test and benchmark streams, from
tiny synthetic clips up to 1080p x264 and JM reference-encoder streams.

## Design

- **Input:** Both Annex B (start code delimited `00 00 00 01` / `00 00 01`) and AVCC (length-prefixed, used inside MP4/MKV containers) bitstreams are supported. The decoder itself accepts `NalUnit` values; the choice of parser determines the input format.
- **Streaming:** The decoder exposes a streaming API. NAL units are fed incrementally and decoded frames are emitted as they become available.
- **Performance:** The decoder aims to be fast, with performance relative to ffmpeg's software H.264 decoder as the target benchmark. Current numbers are in [BENCHMARK.md](BENCHMARK.md).

## Not supported

Streams using any of these are rejected with `DecodeError::Unsupported` on
every slice, rather than decoded into wrong pictures:

- 4:4:4 chroma
- Bit depths above 8 (High 10 and up)
- Lossless coding (transform bypass)

Also not implemented: SP/SI slices, and slice groups (FMO) or arbitrary
slice order (ASO).

## Cargo features

- `std` (default): standard library support. Without it the crate is
  `no_std` + `alloc`; `ThreadedDecoder` needs `std`.
- `portable-simd`: motion compensation via `core::simd` instead of the
  NEON/x86 kernels (nightly only).
- `dev-internals`: exposes internal modules (SPS/PPS/slice parsing, SHA-256)
  for the diagnostic and benchmark examples. Not a stable API.
- `minifb`: windowed player example.
- `profile`: pprof sampling and phase timers for the benchmark example.

## Usage

The recommended entry point is `OrderedDecoder`, which buffers and emits
frames in **display order** automatically — no manual sorting or GOP tracking.

```rust
use rust_h264::decoder::OrderedDecoder;
use rust_h264::nal::parse_annex_b;

let h264_data = std::fs::read("input.h264").unwrap();
let nals = parse_annex_b(&h264_data);
let mut decoder = OrderedDecoder::new();

for nal in &nals {
    // decode_nal returns 0 or more frames in display order
    for frame in decoder.decode_nal(nal).unwrap() {
        // `frame` is a decoded YUV420 picture:
        //   frame.y, frame.u, frame.v  — pixel planes
        //   frame.width, frame.height  — dimensions
        //   frame.pic_order_cnt        — POC (already display-ordered)
    }
}
// Drain any remaining buffered frames at end-of-stream
for frame in decoder.flush() {
    // handle final frames
}
```

If your demuxer has per-picture metadata such as PTS timestamps, use the
generic metadata API so that timestamps follow frames through H.264 B-frame
reordering:

```rust
use rust_h264::decoder::OrderedDecoder;

let mut decoder = OrderedDecoder::<u64>::new(); // metadata is a PTS timestamp

for access_unit in samples {
    let pts = access_unit.pts;
    for nal in access_unit.nals {
        for (frame, frame_pts) in decoder.decode_nal_with_meta(&nal, pts).unwrap() {
            // `frame` is in display order and `frame_pts` belongs to this frame.
        }
    }
}
for (frame, frame_pts) in decoder.flush_with_meta() {
    // handle final frames with their PTS values
}
```

`Decoder::decode_nal` has a one-picture pipeline delay: a slice that starts a
new picture finalizes and returns the previous picture. `OrderedDecoder` stores
the metadata for the picture currently being built and pairs it with that
delayed frame when finalization occurs, avoiding the common off-by-one PTS bug.
Metadata passed for non-slice NALs (SPS/PPS/SEI/etc.) is ignored, and
continuation slices keep the metadata attached by the first slice of the same
picture. Without metadata, `OrderedDecoder` is `OrderedDecoder<()>` and
`decode_nal`/`flush` return plain `Vec<Frame>`.

### Multithreaded decoding

`ThreadedDecoder` pipelines whole pictures across worker threads and returns
frames in display order. Its output is byte-identical to `OrderedDecoder`:

```rust
use rust_h264::threading::ThreadedDecoder;

let mut decoder = ThreadedDecoder::new(4); // up to 4 pictures in flight
for nal in &nals {
    for frame in decoder.decode_nal(nal).unwrap() {
        // display order
    }
}
for frame in decoder.flush() {
    // remaining frames
}
```

Each worker publishes its picture row by row, and a picture that references
it waits only for the rows its motion vectors actually reach. A P-frame can
therefore start while the frame it references is still being decoded, so
both B-frame and P-only streams scale. Requires the `std` feature (on by
default).

### Low-level `Decoder` (decode order)

If you need raw decode order — for example, to drive a custom reorder buffer
or feed frames directly to an encoder that doesn't care about display order
— use `Decoder` instead. It returns one frame at a time in **decode order**.

```rust
use rust_h264::decoder::Decoder;
use rust_h264::nal::parse_annex_b;

let nals = parse_annex_b(&std::fs::read("input.h264").unwrap());
let mut decoder = Decoder::new();

for nal in &nals {
    match decoder.decode_nal(nal) {
        Ok(Some(frame)) => { /* handle frame in decode order */ }
        Ok(None) => {} // NAL consumed, no frame ready yet (e.g. SPS/PPS)
        Err(e) => eprintln!("decode error: {:?}", e),
    }
}
if let Some(frame) = decoder.flush() {
    // handle final frame
}
```

When using `Decoder` directly, you must manually sort by `pic_order_cnt`
within each GOP if you want display order. **Be careful with IDR boundary
tracking** — `decode_nal` returns the *previous* picture when called with
the start of a new picture, so increment your GOP counter **after** the
call, not before:

```rust
use rust_h264::nal::NalUnitType;

let mut idr_count: u32 = 0;
let mut frames = Vec::new();

for nal in &nals {
    let is_idr = nal.nal_unit_type == NalUnitType::SliceIdr;
    if let Ok(Some(frame)) = decoder.decode_nal(nal) {
        // Push with CURRENT idr_count — this frame belongs to the
        // previous picture, before the IDR boundary
        frames.push((idr_count, frame));
    }
    if is_idr {
        idr_count += 1;  // AFTER decode_nal, not before
    }
}
if let Some(frame) = decoder.flush() {
    frames.push((idr_count, frame));
}
frames.sort_by_key(|(idr, f)| (*idr, f.pic_order_cnt));
```

This is exactly what `OrderedDecoder` does for you — prefer it unless you
need decode order specifically.

### AVCC input (MP4/MKV containers)

For length-prefixed bitstreams from MP4/MKV containers, use `parse_avcc_config`
for the `avcC` configuration box and `parse_avcc` for each sample. The decoder
itself is unchanged — only the framing parser differs.

```rust
use rust_h264::decoder::OrderedDecoder;
use rust_h264::nal::{parse_avcc, parse_avcc_config};

// Get the avcC box payload from your MP4 demuxer
let config = parse_avcc_config(&avcc_box_payload).unwrap();
let mut decoder = OrderedDecoder::new();

// Feed SPS/PPS once at startup (they live in the avcC box, not in samples)
for nal in config.sps_nals.iter().chain(config.pps_nals.iter()) {
    decoder.decode_nal(nal).unwrap();
}

// For each sample (MP4 chunk), parse and decode its NALs
for sample_data in mp4_samples {
    for nal in parse_avcc(&sample_data, config.length_size) {
        for frame in decoder.decode_nal(&nal).unwrap() {
            // frame is in display order
        }
    }
}
for frame in decoder.flush() {
    // final buffered frames
}
```

`length_size` is taken from the `avcC` box (typically 4) and matches the
`lengthSizeMinusOne + 1` field. The AVCC NAL payload is identical to Annex B
(same NAL header, same RBSP, same emulation prevention handling).

## Tools

### Player

Decode and display an H.264 bitstream in a window:

```
cargo run --release --features minifb --example play -- input.h264 [--fps 30] [--loop]
```

- `--fps N` — set playback frame rate (default: 30)
- `--loop` — loop playback continuously
- Press Escape to quit

### Dump frames

Decode an H.264 bitstream to raw YUV420 output:

```
cargo run --release --example dump_frames -- input.h264 [output.yuv]
```

Frames are written in display order (sorted by POC).

### Compare against FFmpeg

Decode a stream with both this crate and FFmpeg and compare the raw YUV
byte for byte. Accepts `.h264` files or any container FFmpeg can read (the
H.264 track is extracted with a bitstream copy); needs `ffmpeg` and
`ffprobe` on `PATH`:

```
python3 tools/verify_vs_ffmpeg.py input.mp4 [more inputs...]
```

### Benchmark

Decode time, allocation count and output digest for each bench stream in
`testdata/`; set `BENCH_THREADS=N` to use `ThreadedDecoder`:

```
cargo run --release --features dev-internals --example bench_decode
BENCH_THREADS=4 cargo run --release --features dev-internals --example bench_decode
```

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.
