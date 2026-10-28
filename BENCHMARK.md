# Benchmark: rust_h264 vs FFmpeg

## Test Setup

- **Platform:** Apple Silicon (ARM64), macOS
- **Source:** `testsrc2` (animated test pattern with motion, text, color bars)
- **Streams:**
  - 720p: 1280x720, 300 frames, x264 `--preset medium --no-deblock`, CABAC
    - P-only: `bframes=0 ref=1`
    - B-frames: `bframes=3 ref=4`
  - 1080p: 1920x1080, 100 frames, x264 `--preset medium --no-deblock`, CABAC, `bframes=3 ref=2`
- **FFmpeg:** Single-threaded (`-threads 1`), software decode, compiled with `-O3` + NEON assembly
- **rust_h264:** `cargo build --release`, pure Rust + NEON `half_pel_h` intrinsics

### Stream generation

```bash
# 1080p, 100 frames
ffmpeg -f lavfi -i "testsrc2=s=1920x1080:rate=30:duration=3.33" -frames:v 100 \
  -c:v libx264 -preset medium -crf 23 \
  -x264opts "bframes=3:ref=2:no-deblock:keyint=250:min-keyint=25" \
  -f h264 bench_1080p_100f_complex.h264

# 720p P-only, 300 frames
ffmpeg -f lavfi -i "testsrc2=s=1280x720:rate=30:duration=10" -frames:v 300 \
  -c:v libx264 -preset medium -crf 23 \
  -x264opts "bframes=0:ref=1:no-deblock:keyint=250:min-keyint=25" \
  -f h264 bench_720p_300f_ponly_complex.h264

# 720p B-frames, 300 frames
ffmpeg -f lavfi -i "testsrc2=s=1280x720:rate=30:duration=10" -frames:v 300 \
  -c:v libx264 -preset medium -crf 23 \
  -x264opts "bframes=3:ref=4:no-deblock:keyint=250:min-keyint=25" \
  -f h264 bench_720p_300f_bframes_complex.h264
```

## Results

### 720p (1280x720, 300 frames)

| Decoder | Stream | Time (user) | FPS | Memory |
|---------|--------|-------------|-----|--------|
| FFmpeg | P-only | 0.30s | 1000 | — |
| FFmpeg | B-frames | 0.31s | 968 | — |
| rust_h264 | P-only | 0.77s | 390 | 12 MB |
| rust_h264 | B-frames | 1.18s | 254 | 12 MB |

### 1080p (1920x1080, 100 frames)

| Decoder | Stream | Time (user) | FPS | vs 30fps | vs 60fps |
|---------|--------|-------------|-----|----------|----------|
| FFmpeg | B-frames | 0.22s | 454 | 15.1x | 7.6x |
| rust_h264 | B-frames | 0.89s | 112 | 3.7x | 1.9x |

**FFmpeg is 2.6-4.0x faster.** FFmpeg uses hand-tuned NEON/SSE assembly for all
MC filters, IDCT, and deblocking. rust_h264 uses NEON only for `half_pel_h`.

**1080p @ 60fps target achieved** — 112 fps (1.9x realtime at 60fps).
720p B-frames at 254 fps (8.5x realtime at 30fps).

### Note on synthetic sources

Earlier benchmarks used `mandelbrot` as the video source, which produces
near-static content with mostly skip MBs. This inflated rust_h264 FPS numbers
(67 fps at 1080p) and exaggerated the FFmpeg ratio (reported as 50-110x).
The `testsrc2` source has realistic motion and texture, giving more
representative numbers.

## Profile Breakdown

Sampled with macOS `sample` command on the 720p P-only decode:

| Component | % Time | Description |
|-----------|--------|-------------|
| **Luma MC (half-pel filters)** | **42%** | 6-tap FIR filter for sub-pixel interpolation |
| **CABAC decode overhead** | **25%** | Loop/store overhead (18.7%), arithmetic engine (2.8%), residual/cbp/mvd (3.5%) |
| **Chroma MC** | **18%** | Bilinear interpolation at 1/8-pel precision |
| **P_Skip MC** | **10%** | Combined luma+chroma MC for skip MBs |
| Inverse DCT | 3% | 4x4 integer IDCT |
| finalize_mb_info | 2% | Per-MB metadata copy for deblocking |
| Other | 1% | MV prediction, reconstruction, etc. |

### B-frame profile (720p, bframes=3 ref=4, CABAC)

Sampled with macOS `sample` on 300-frame 720p B-frame decode (1.40s user):

| Component | % Time | Description |
|-----------|--------|-------------|
| **Luma MC** | **42%** | 6-tap FIR half-pel filters (25% in B_Skip, 16% in other inter) |
| **Chroma MC** | **19%** | Bilinear 1/8-pel (13% B_Skip, 6% other inter) |
| **Spatial direct MV** | **9%** | `derive_spatial_direct_blk` per-4x4-block derivation |
| **Bi-pred averaging** | **7%** | L0+L1 pixel averaging in B_Skip |
| **CABAC decode** | **5%** | Residual (2%), syntax elements (2%), neighbor/dequant (1%) |
| Deblock/frame mgmt | 4% | Deblocking filter + DPB management |
| Reconstruct | 2% | Luma/chroma reconstruction from residual |
| Inverse DCT | 2% | 4x4 integer IDCT |
| Other | 10% | MV prediction, malloc, unaccounted |

**Key difference from P-only:** B_Skip dominates (55% of total), with spatial
direct MV derivation (9%) as a new significant cost. CABAC overhead dropped
from 25% to 5% after the `OFFSET_TO_BLOCK` optimization — the reverse lookups
were a major B-slice bottleneck since each B MB required dual-list neighbor
queries.

### 1080p profile (1920x1080, bframes=3 ref=2, CABAC)

Sampled with macOS `sample` on 100-frame 1080p B-frame decode (2.10s user):

| Component | % Time | Description |
|-----------|--------|-------------|
| **Luma MC** | **~55%** | `luma_mc` + `half_pel_h`/`half_pel_v`/`half_pel_hv` |
| **Chroma MC** | **~13%** | Bilinear 1/8-pel interpolation |
| **Spatial direct MV** | **~12%** | `derive_spatial_direct_blk` per-4x4-block |
| **CABAC residual** | **~10%** | `decode_residual_cabac` coefficient parsing |
| **CABAC engine** | **~4%** | `get_cabac` arithmetic decode |
| Other | ~6% | MV prediction, bi-pred, dequant, malloc |

**Detailed leaf-level breakdown** (non-overlapping):

| Function | % Time | Description |
|----------|--------|-------------|
| **`luma_mc` overhead** | **21.5%** | Per-pixel loop, `luma_interp` dispatch, `ref_luma` bounds clamping |
| **`half_pel_h`** | **19.4%** | 6-tap horizontal FIR filter |
| **`chroma_mc`** | **13.8%** | Bilinear 1/8-pel with per-pixel clamping |
| **`decode_residual_cabac`** | **11.3%** | Significance map + coefficient level decode |
| **`half_pel_v`** | **10.5%** | 6-tap vertical FIR filter |
| **`half_pel_hv`** | **8.9%** | 2-pass 6-tap diagonal filter |
| `derive_spatial_direct_blk` | 6.0% | Neighbor MV lookup + co-located check |
| `get_cabac` | 4.6% | Arithmetic decode engine |
| Other | 4.1% | DCT, dequant, weight, mvd, predict_mv |

**Key insight:** `luma_mc` overhead (21.5%) is as expensive as `half_pel_h` (19.4%).
This is the per-pixel `luma_interp` dispatch and `ref_luma` boundary clamping — not
the filter math itself. A row-based approach that processes entire rows with a single
bounds check would cut this significantly even without SIMD.

## Optimization Opportunities

### 1. Luma MC (60% total) — High impact

Luma MC has two bottlenecks: the filter math (39%) and the per-pixel overhead (21%).

**`luma_mc` overhead (21.5%):** The current code calls `luma_interp` -> `half_pel_*`
-> `ref_luma` per pixel. Each `ref_luma` call does bounds clamping. Restructuring to
process entire rows with a single bounds check (is the entire row within bounds?)
would eliminate most of the overhead.

**`half_pel_h`/`half_pel_v`/`half_pel_hv` (38.8%):** The 6-tap FIR filter does
6 multiplications + additions + clipping per pixel.

**Approaches:**
- **Row-based processing:** Process entire rows with one bounds check instead of
  per-pixel clamping. Enables compiler auto-vectorization. Medium effort, no SIMD
  dependency. Expected: **~15-20% overall improvement** (eliminates 21.5% overhead).
- **SIMD (NEON):** Process 8 pixels per instruction with `vmull`/`vmlal`.
  `std::arch::aarch64` intrinsics or `std::simd` (nightly). Expected: **3-5x
  speedup for filter math -> ~1.5-2x overall**.

### 2. Chroma MC (14%) — Medium impact

Same per-pixel overhead pattern as luma: `ref_chroma` boundary clamping per pixel.
Row-based + NEON bilinear would help.

### 3. CABAC decode (16%) — Low impact, hard to optimize

`decode_residual_cabac` (11.3%) and `get_cabac` (4.6%) are bit-serial.
The actual bottleneck is the surrounding code in `decode_cabac_mb`.

**Approaches:**
- ~~**`BLOCK_INDEX_TO_OFFSET` lookup table:**~~ Done — see optimization #6 below.
- **Inline `cabac_neighbor_*` functions:** The neighbor context lookups involve
  multiple function calls with many parameters. `#[inline(always)]` or manual
  inlining would reduce call overhead.
- **Reduce array stores:** MV/ref/MVD stores write to every 4x4 block (16 writes
  for a 16x16 partition). For uniform partitions, a single `memset`-style fill
  would be faster.

Expected improvement: **~10-20% of CABAC time -> ~3-5% overall**

### 3. Chroma MC (18%) — Medium impact, low effort

Chroma MC uses bilinear interpolation at 1/8-pel. Simpler than luma but still
per-pixel with multiplications.

**Approaches:**
- **SIMD:** Same NEON approach as luma MC.
- ~~**Strength reduction:** For full-pel chroma (frac=0), skip interpolation entirely
  and use `copy_from_slice`.~~ Done — see optimization #7 below.

Expected improvement: **2-4x for chroma MC -> ~5-10% overall** (SIMD only; full-pel
fast path already implemented)

### 4. Inverse DCT (3%) — Low impact

Already fast. SIMD could help for 8x8 IDCT in High profile but the 4x4 IDCT
is simple enough that scalar code is nearly optimal.

### 5. Memory allocation (done)

Replaced `Vec` heap allocations with stack arrays in hot paths:
- MC prediction buffers: `vec![0u8; w*h]` -> `[0u8; 256]`
- `b_sub_parts`: `Vec<BSubPart>` -> `[BSubPart; 16]`
- `BSubLayout`: `Vec<BSubLayout>` -> `[BSubLayout; 4]`
- Sub-partition offsets: `vec![...]` -> `&[...]` static slices

**Result: ~4% improvement** (1.70s -> 1.63s)

### 6. OFFSET_TO_BLOCK reverse lookup table (done)

Replaced ~46 O(16) linear scans (`BLOCK_INDEX_TO_OFFSET.iter().position()`) with
O(1) `OFFSET_TO_BLOCK[row][col]` table lookups across `neighbor.rs`,
`decode_cabac.rs`, `decode_cavlc.rs`, and `mv_pred.rs`. These reverse lookups
convert (row, col) grid coordinates to block indices and were called dozens of
times per MB for neighbor context (amvd, ref_idx, coded_block_flag) and MV
prediction.

**Result: ~11% improvement on B-frames** (1.58s -> 1.40s), P-only within noise
(1.60s -> 1.65s). The B-frame gain is larger because B-slices exercise the
reverse lookup much more heavily: dual-list neighbor lookups, direct mode checks,
and spatial/temporal MV derivation.

### 7. Full-pel MC fast path (done)

Added early-exit fast paths in `luma_mc` and `chroma_mc`: when the fractional MV
is zero (integer-pel position), skip the 6-tap FIR / bilinear interpolation and
`copy_from_slice` directly from the reference buffer. Inner-bounds check avoids
per-pixel clamping for blocks fully within the picture.

**Result: ~2% improvement** (P-only 1.65s -> 1.61s, B-frames 1.40s -> 1.37s).
Modest because x264 `--preset medium` (subme=7) produces mostly sub-pel MVs.
Streams with simpler motion estimation or static content would see larger gains.

### 8. Spatial direct MV dedup + inlining (done)

When `direct_8x8_inference_flag` is set, all 4 blocks within each 8x8 group
derive the same spatial/temporal direct MVs. Reduced from 16 derivation calls
per MB to 4 (one per 8x8 group), filling sub-blocks by copy. Also added
`#[inline(always)]` to hot neighbor functions (`cabac_amvd`, `cabac_neighbor_ref`,
`get_mv_neighbor_left/above/above_right/above_left`).

**Result: negligible** (~0.5% at 1080p). The per-call cost was already low after
the `OFFSET_TO_BLOCK` optimization, and LLVM was already inlining the neighbor
functions in release mode.

### 9. Row-based luma MC restructure (done)

Restructured `luma_mc` to dispatch on `(frac_x, frac_y)` once per block instead
of per pixel. In-bounds blocks use direct buffer slicing (`&ref_y[off..off+len]`)
with row-based filter functions (`row_half_pel_h`, `row_half_pel_v`,
`row_half_pel_hv`), eliminating per-pixel `ref_luma` clamping and `luma_interp`
dispatch. Boundary blocks fall back to the original per-pixel path.

**Result: negligible in release** (LLVM was already inlining and optimizing the
per-pixel path to equivalent code at `-O3`). **29% faster in debug mode**
(test suite: 6.38s -> 4.52s), confirming the structural improvement. The new
row-based functions (`row_half_pel_h` etc.) are natural NEON SIMD targets.

### 10. NEON half_pel_h (done)

Replaced the scalar `row_half_pel_h` with NEON intrinsics (`std::arch::aarch64`).
Processes 8 pixels per iteration using `vld1_u8` (6 overlapping loads), `vaddl_u8`
(widen to u16), `vmlaq_n_s16`/`vmlsq_n_s16` (multiply-accumulate with coefficients
20 and -5), `vshrq_n_s16` (right shift by 5), and `vqmovun_s16` (saturating narrow
to u8). Scalar tail handles remaining 0-7 pixels per row.

Key insight: `#[inline(never)]` on the NEON function is critical — without it,
LLVM's inliner absorbs the intrinsics into the enormous caller function and
scalarizes them back. With `#[inline(never)]`, vector instructions are preserved.

**Result: 28-37% improvement** on mandelbrot-source streams.
Now measured at **112 fps (1080p)** and **254-390 fps (720p)** on realistic
`testsrc2` content.

## Realistic Performance Target

**Target: 1080p @ 60 fps — ACHIEVED** (112 fps on testsrc2, 1.9x realtime at 60fps).

### Completed optimizations

1. ~~BLOCK_INDEX_TO_OFFSET lookup table~~ — ~11% B-frame improvement
2. ~~Full-pel MC fast path~~ — ~2% (content-dependent)
3. ~~Spatial direct MV dedup~~ — negligible (per-call cost already low)
4. ~~`#[inline(always)]` on neighbor functions~~ — negligible (LLVM already inlining)
5. ~~Row-based MC processing~~ — no release improvement, but structured for SIMD
6. ~~NEON `half_pel_h`~~ — **28-37% improvement** on simple content

### Further SIMD opportunities

7. **NEON `half_pel_v`** — Same 6-tap filter but vertical. 10.5% of pre-NEON
   1080p time. Needs column gather from 6 rows.

8. **NEON `half_pel_hv`** — 2-pass diagonal filter. 8.9% of pre-NEON time.

9. **NEON chroma MC** — 8-wide bilinear. ~14% of pre-NEON time.

10. **NEON bi-pred averaging** — `vrhadd_u8` does `(a+b+1)>>1` in one instruction.

**720p** is **8.5x realtime** at 30fps (254 fps with B-frames).
**1080p** is **3.7x realtime** at 30fps (112 fps), **1.9x at 60fps**.
Items 7-9 would increase 1080p headroom to ~150+ fps.

## x86-64 results (Windows, SIMD + frame threading) — from PR #1

Numbers reported by the author of PR #1 (codepaws), measured on their
branch. The x86-64 SIMD kernels are being ported to trunk; the frame-threaded
`ThreadedDecoder` is not in trunk yet.

Test machine: x86-64 desktop, rustc 1.95, `cargo build --release` (default
`target-cpu`, no `native`). Streams as above (`testsrc2`, x264 preset
medium, no-deblock). `--threads N` uses `ThreadedDecoder`; `threads=1` is
`OrderedDecoder`.

### Throughput (best of 3)

| Stream | scalar (pre-SIMD) | SIMD, 1 thread | SIMD, 2 threads | SIMD, 4 threads | SIMD, 8 threads |
|--------|------------------:|---------------:|----------------:|----------------:|----------------:|
| 1080p B-frames (100f) | 74 fps | 83 fps | 93 fps | **122 fps** | 121 fps |
| 720p B-frames (300f)  | 196 fps | 227 fps | — | **331 fps** | — |
| 720p P-only (300f)    | 312 fps | 318 fps | — | 339 fps | — |

- **SIMD alone: ~8-9%** end-to-end, even though the hand kernels are
  6.6-8.5x faster than true scalar in microbenchmarks. The reason: LLVM
  already auto-vectorizes the row-based scalar loops with SSE2 at the
  x86-64 baseline (optimization #9 restructured them for exactly that), so
  the hand-written SSE2 mostly matches what the compiler emitted. Gains
  beyond auto-vec require instructions the compiler cannot use in generic
  builds (SSSE3 `pmaddubsw` chroma, AVX2/AVX-512 `pavgb` bi-pred, which
  are gated at runtime).
- **Frame threading: 1.46-1.47x at 4 threads** on B-frame content, and
  ~1x on P-only chains — P-pictures reference their immediate
  predecessor, so the dependency chain serializes regardless of thread
  count. This matches the structure FFmpeg's frame-threaded decoder
  exploits; row-level reference synchronization (decoding a dependent
  picture before its reference is fully finished) is future work.
- Combined effect vs the scalar single-threaded baseline on this machine:
  **74 -> 122 fps at 1080p (1.65x)**.

## Deblocking ON + x86-64 FFmpeg baseline (Windows, same machine) — from PR #1

The streams above disable the in-loop deblocking filter
(`--no-deblock`). Real content (including PS5 cutscenes) uses it, and
deblocking has no SIMD yet. Streams regenerated without `no-deblock`
(`testdata/deb_*.h264`, regenerable with the commands below; not
committed):

```bash
ffmpeg -f lavfi -i "testsrc2=s=1920x1080:rate=30:duration=3.33" -frames:v 100   -c:v libx264 -preset medium -crf 23 -x264opts "bframes=3:ref=2" -f h264 deb_1080p_100f.h264
ffmpeg -f lavfi -i "testsrc2=s=1280x720:rate=30:duration=10" -frames:v 300   -c:v libx264 -preset medium -crf 23 -x264opts "bframes=3:ref=4" -f h264 deb_720p_300f_bframes.h264
ffmpeg -f lavfi -i "testsrc2=s=1280x720:rate=30:duration=10" -frames:v 300   -c:v libx264 -preset medium -crf 23 -x264opts "bframes=0:ref=1" -f h264 deb_720p_300f_ponly.h264
```

| Stream | rust_h264 1t | rust_h264 4t | FFmpeg `-threads 1` | FFmpeg / rust_h264 4t |
|--------|-------------:|-------------:|--------------------:|----------------------:|
| 1080p B + deblock | 70 fps | 95 fps | 321 fps | 3.4x |
| 720p B + deblock  | 159 fps | 241 fps | 620 fps | 2.6x |
| 720p P + deblock  | 262 fps | 233 fps | 800 fps | 3.4x |

- Deblocking costs **16-30% single-threaded** (83->70, 227->159, 318->262).
- With deblocking, the 4-thread P-only case is **slower** than 1 thread:
  deblock runs on the worker but sits on the critical path between
  chained P-pictures (each waits for its reference's full commit), so
  threading only adds overhead there. Row-level reference sync is the
  structural fix.
- **FFmpeg single-threaded is 2.6-3.4x faster than our best threaded
  result on this machine** — the x86 gap is larger than the 2.6-4x
  single-threaded gap on the original author's Mac. The remaining gap
  maps to: deblock SIMD, IDCT/intra-pred SIMD, block-fused MC (per-row
  function dispatch overhead), and row-level threading.

## FFmpeg byte-exactness on real content

`tools/verify_vs_ffmpeg.py` compares our output byte-for-byte against
FFmpeg on any `.h264` or container input (the H.264 track is extracted
with a bitstream copy, so both decoders see identical Annex-B input):

```bash
python3 tools/verify_vs_ffmpeg.py clip.mp4 stream.h264
```

Requires `ffmpeg` and `ffprobe` on PATH. On mismatch it reports the first
differing byte offset and exits non-zero.

## Frame threading on Apple Silicon (trunk)

`ThreadedDecoder` on the bench corpus, 10-core aarch64, NEON backend. Best
of 15 runs per cell (`BENCH_THREADS=N cargo run --release --features
dev-internals --example bench_decode -- 1 5`, repeated in interleaved
rounds); serial is best of 20. Output digests are identical to the serial
decoder in every configuration.

| Stream | serial | picture sync, 4t | row sync, 4t | row sync, 8t |
|--------|-------:|-----------------:|-------------:|-------------:|
| 1080p B-frames complex (100f) | 73.9 fps | 87.8 fps | **102.8 fps** | 93.1 fps |
| 1080p P-only complex (100f)   | 77.4 fps | 76.6 fps | **112.0 fps** | 100.3 fps |
| 720p B-frames complex (300f)  | 166.1 fps | 199.3 fps | **228.0 fps** | 220.3 fps |
| 720p P-only complex (300f)    | 171.8 fps | 167.9 fps | **250.8 fps** | 231.2 fps |
| 1080p B-frames mandelbrot (100f) | 57.1 fps | — | **82.8 fps** | — |
| 720p B-frames mandelbrot (300f)  | 118.5 fps | — | **185.5 fps** | — |
| 720p P-only mandelbrot (300f)    | 111.3 fps | — | **202.2 fps** | — |

- **Row-level synchronization** (current): about **1.4-1.46x over serial at
  4 threads** on the complex streams, for P-only content as much as for
  B-frames. A picture's worker deblocks and publishes each macroblock row
  as it becomes final; motion compensation in a dependent picture waits
  only for the reference rows it reads, so consecutive P-pictures overlap
  instead of running back to back.
- **Picture-level synchronization** (previous version, kept for
  comparison): each picture waited for its references to finish
  completely. That left the I/P chain serial, so P-only streams gained
  nothing and B-frame streams about 1.2x.
- 8 threads is slower than 4 here. Pictures waiting on references hold
  their threads, and with more in flight some run on the efficiency cores,
  which slows the reference chain everyone else waits on.
- The three mandelbrot streams (`bench_1080p_100f`,
  `bench_720p_300f_ponly`, `bench_720p_300f_bframes`) were originally
  encoded as High 4:4:4, which this decoder does not support, so their
  earlier numbers measured decoding of unsupported content. They were
  regenerated as 4:2:0 with the same source and x264 options (`mandelbrot`
  at 60 fps, `--preset medium --crf 23`, `no-deblock`, bframes/ref as
  named) and decode byte-identical to FFmpeg; their rows above (best of 15,
  measured separately from the others) are 1.45-1.82x at 4 threads.
- Single-threaded cost of the row waits (an atomic load per motion
  compensation call on complete pictures) is within noise: best-of-20
  serial throughput is 0.2-0.6% lower than before the change.
