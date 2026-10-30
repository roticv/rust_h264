# Benchmark: rust_h264 vs FFmpeg

Measured 2026-10-30 on trunk (after the 10-bit change).

## Setup

- **Machine:** Apple M4 (4 performance + 6 efficiency cores), macOS. The
  machine was not idle (load average 4-6), so every figure is the best of 5
  interleaved rounds.
- **rust_h264:** rustc 1.99.1, `--release`, NEON backend.
  `cargo run --release --features dev-internals --example bench_decode -- 1 3`
  times parsing and decoding only (best of 3 after 1 warm-up, per round).
  Serial is `Decoder`; 4 threads is `ThreadedDecoder` (`BENCH_THREADS=4`).
- **FFmpeg:** 9.0.2, `ffmpeg -benchmark -threads N -i <stream> -f null -`,
  frames divided by the reported `rtime`. FFmpeg uses hand-written NEON
  assembly for MC, IDCT, intra prediction and deblocking at both bit depths.
- Every stream decodes byte-identical between the two
  (`tools/verify_vs_ffmpeg.py`).

## Streams

All x264 `--preset medium --crf 23`, CABAC, High profile, in `testdata/`
except the 10-bit one:

| Stream | Size | Source | x264 options | Deblocking |
|--------|------|--------|--------------|------------|
| `bench_720p_300f_ponly_complex` | 1280x720, 300 frames | `testsrc2` | `bframes=0:ref=1` | off |
| `bench_720p_300f_bframes_complex` | 1280x720, 300 frames | `testsrc2` | `bframes=3:ref=4` | off |
| `bench_720p_300f_ponly` | 1280x720, 300 frames | `mandelbrot` | `bframes=0:ref=1` | off |
| `bench_720p_300f_bframes` | 1280x720, 300 frames | `mandelbrot` | `bframes=3:ref=4` | off |
| `bench_1080p_100f_complex` | 1920x1080, 100 frames | `testsrc2` | `bframes=3:ref=2` | off |
| `bench_1080p_100f_ponly` | 1920x1080, 100 frames | `testsrc2` | `bframes=0:ref=1` | off |
| `bench_1080p_100f` | 1920x1080, 100 frames | `mandelbrot` | `bframes=3:ref=2` | off |
| `bench_1080p_100f_deblock` | 1920x1080, 100 frames | `testsrc2` | `bframes=3:ref=2` | **on** |
| 10-bit twin of `_deblock` (not committed) | 1920x1080, 100 frames | `testsrc2` | `bframes=3:ref=2`, High 10 | **on** |

The `testsrc2` streams have realistic motion and texture. The `mandelbrot`
ones are mostly smooth zooms and give a second content type. All use
`keyint=250:min-keyint=25`. For example, the deblocked stream and its
10-bit twin (identical options, plus `-pix_fmt yuv420p10le`):

```bash
ffmpeg -f lavfi -i "testsrc2=s=1920x1080:rate=30:duration=3.33" -frames:v 100 \
  -c:v libx264 -preset medium -crf 23 \
  -x264opts "bframes=3:ref=2:keyint=250:min-keyint=25" \
  -f h264 bench_1080p_100f_deblock.h264

ffmpeg -f lavfi -i "testsrc2=s=1920x1080:rate=30:duration=3.33" -frames:v 100 \
  -pix_fmt yuv420p10le -c:v libx264 -preset medium -crf 23 \
  -x264opts "bframes=3:ref=2:keyint=250:min-keyint=25" \
  -f h264 bench10_1080p_100f_deblock.h264
```

The other streams add `no-deblock` and the `bframes`/`ref` in the table.
`BENCH_FILE=bench10_1080p_100f_deblock.h264` times the 10-bit stream with
`bench_decode`.

## Results

Frames per second, higher is better. The last two columns show how many
times faster FFmpeg is.

| Stream | rust_h264 serial | rust_h264 4 threads | FFmpeg 1 thread | FFmpeg 4 threads | FFmpeg ÷ ours, 1 thread | FFmpeg ÷ ours, 4 threads |
|--------|-----:|-----:|-----:|-----:|-----:|-----:|
| 720p P-only (testsrc2) | 545 | 1692 | 1010 | 2970 | 1.85x | 1.76x |
| 720p B-frames (testsrc2) | 485 | 1174 | 971 | 2703 | 2.00x | 2.30x |
| 720p P-only (mandelbrot) | 324 | 1115 | 586 | 1887 | 1.81x | 1.69x |
| 720p B-frames (mandelbrot) | 346 | 764 | 647 | 1658 | 1.87x | 2.17x |
| 1080p B-frames (testsrc2) | 216 | 535 | 455 | 1316 | 2.11x | 2.46x |
| 1080p P-only (testsrc2) | 253 | 754 | 490 | 1515 | 1.94x | 2.01x |
| 1080p B-frames (mandelbrot) | 183 | 422 | 376 | 962 | 2.05x | 2.28x |
| 1080p B-frames + deblocking | 190 | 472 | 392 | 1124 | 2.06x | 2.38x |
| **10-bit** 1080p B-frames + deblocking | 64 | 147 | 248 | 690 | 3.86x | 4.68x |

- **8-bit: FFmpeg is 1.8-2.1x faster single-threaded**, and 1.7-2.5x at 4
  threads. Every 8-bit stream decodes above 180 fps at 1080p on one core,
  with deblocking on as well.
- **Frame threading gives 2.2-3.4x at 4 threads**: 3.0-3.4x on the P-only
  streams, where row-level reference sync lets consecutive P-pictures
  overlap, and 2.2-2.5x on the B-frame streams.
- **Deblocking costs about 12%** (1080p B-frames, 216 fps without it on
  the same source, 190 with it). Before this release's deblocking work
  (bS shortcuts, NEON edge filters) it was about a quarter of decode time.
- **10-bit is about 3x slower than 8-bit** on the same content, and 3.9x
  slower than FFmpeg. The 10-bit path is the same decoder compiled for
  16-bit samples, without SIMD: motion compensation, weighted prediction
  and deblocking use the generic scalar code. 10-bit NEON kernels are the
  obvious next step.

## Where the 8-bit time goes

`cargo run --release --features profile --example bench_decode -- 5`
(`bench_1080p_100f_complex`) writes a flamegraph to
`target/profile-1080p-complex.svg`. Most kernels are inlined into the
macroblock decoder, so the self-time ranking (295 samples) is coarse:

| Self time | Share | Contains |
|-----------|------:|----------|
| `decode_cabac_mb` | 28% | CABAC syntax and residual decoding, luma reconstruction, inlined MC dispatch |
| Chroma (`predict_chroma_part`, `decode_chroma_residual_cabac`, `chroma_mc`) | 32% | chroma MC, residual parsing, DC/AC reconstruction |
| Luma MC (`luma_mc_stride`, NEON kernels) | 7%, more inlined | 6-tap filters, bi-prediction |
| Direct mode and skip MBs | 12% | spatial direct derivation, P/B skip |
| `run_slice_px` | 5% | the slice loop, with per-MB metadata finalisation inlined |
| Unresolved symbols | 14% | samples pprof could not attribute to a function |

The remaining gap to FFmpeg is spread across CABAC residual decoding,
chroma, and the per-MB bookkeeping rather than in any single kernel.

## Reproducing

```bash
# rust_h264, serial and 4 threads (all bench streams)
cargo run --release --features dev-internals --example bench_decode -- 1 3
BENCH_THREADS=4 cargo run --release --features dev-internals --example bench_decode -- 1 3

# FFmpeg, per stream
ffmpeg -benchmark -threads 1 -i testdata/bench_1080p_100f_deblock.h264 -f null -
```

## Byte-exactness on other content

`tools/verify_vs_ffmpeg.py` compares our output byte-for-byte with FFmpeg's
on any `.h264` file or container (the H.264 track is extracted with a
bitstream copy, so both decoders see the same Annex B input), in the
stream's own chroma format and bit depth:

```bash
python3 tools/verify_vs_ffmpeg.py clip.mp4 stream.h264
```

It needs `ffmpeg` and `ffprobe` on PATH, and reports the first differing
byte and exits non-zero on a mismatch.

## History

Older figures (FFmpeg 2.6-4x faster, x86-64 numbers from PR #1 before the
SIMD and threading work reached trunk, per-optimisation measurements) are
in the git history of this file. The CHANGELOG lists the performance
changes by release.
