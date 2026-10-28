# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

A pure Rust H.264 video decoder library. Aims to be a standalone, portable software H.264 decoder (unlike OpenH264 which only supports baseline profile, or FFmpeg's decoder which isn't available as a separate library). Part of the broader rust_media ecosystem.

## Build Commands

This is a Rust project using Cargo:

- **Build:** `cargo build`
- **Test:** `cargo test`
- **Run single test:** `cargo test <test_name>`
- **Lint:** `cargo clippy`
- **Format:** `cargo fmt`
- **Check:** `cargo check`
- **no_std check:** `cargo test --no-default-features` (the `std` feature is on by default; without it the crate is `#![no_std]` + `alloc`)
- **Golden corpus digests:** `cargo test --release -- --ignored golden_corpus_digests` (decodes the full 720p/1080p bench corpus; ignored by default because it is slow)
- **Benchmark:** `cargo run --release --features dev-internals --example bench_decode` (timing, allocation count, and digest per bench stream)
- **Compare against FFmpeg:** `python3 tools/verify_vs_ffmpeg.py testdata/*.h264` (byte-for-byte; every 4:2:0 test and bench stream should match, only the `unsupported_*` streams fail)
- **Profile:** `cargo run --release --features profile --example bench_decode -- <iterations>` (pprof flamegraph at `target/profile-1080p-complex.svg`, per-phase timers, top self-time functions; the `profile` feature implies `std` and `dev-internals`)

## Milestones

1. Get simple decoder test case working
2. Finish implementation of decoder
3. Compare performance of decoder against ffmpeg

## Design Decisions

- **Input format:** Both Annex B (start code delimited) and AVCC (length-prefixed, MP4/MKV) are supported via `parse_annex_b` and `parse_avcc`/`parse_avcc_config` in `src/nal.rs`. The decoder itself accepts `NalUnit` values; the choice of parser determines the input format.
- **Streaming API:** The decoder API is streaming — callers feed NAL units incrementally and receive decoded frames as they become available. No requirement to buffer an entire stream upfront.
- **Performance:** The decoder should be fast. Prefer efficient algorithms, minimize allocations, and avoid unnecessary copies. Performance relative to ffmpeg's software decoder is a key benchmark.

## Code Structure

The decoder logic is split across several files:

| File | Content |
|------|---------|
| `src/decoder.rs` | `Decoder` (raw decode order), `OrderedDecoder` (display-order wrapper). Slice decoding is split into `prepare_slice` (header, POC, reference lists, weights, geometry; produces a `SlicePlan`) and `run_slice` (PictureState creation/continuation and the MB loop). Finalization helpers: `deblock_picture`, `apply_reference_marking`, `finish_output`. `check_supported` rejects non-8-bit-4:2:0 streams. `RowSync` is the frame-threading hook in the MB loop |
| `src/decode_cabac.rs` | CABAC MB decode: skip detection, mb_type dispatch, residual decode |
| `src/decode_cavlc.rs` | CAVLC MB decode: P/B inter, intra, residual decode |
| `src/slice_context.rs` | `SliceContext`/`SliceParams` structs + shared methods (skip, direct, reconstruct, `finalize_mb_info_range`) |
| `src/mv_pred.rs` | MV prediction, spatial/temporal direct mode derivation |
| `src/neighbor.rs` | CABAC neighbor context helpers, nC computation, dequant helpers |
| `src/dpb.rs` | DPB, reference list construction and modification, reference marking, POC. `DecodedPicture` keeps its planes and motion data private: `luma()`/`chroma()` return published rows, `wait_luma`/`wait_chroma`/`motion()` block until data is final (instant for complete pictures) |
| `src/threading.rs` | `ThreadedDecoder`: frame threading with row-level reference sync. Coordinator plans slices (`prepare_slice`) in coded order and inserts each picture into the DPB as an in-progress `DecodedPicture`; workers run `run_slice` with a `RowSync` hook that deblocks (one unit behind decoding, since intra prediction needs unfiltered samples) and publishes final MB rows; output is committed in coded order; reorder buffer mirrors `OrderedDecoder` |
| `src/picture_sync.rs` | `SharedPlane` (single-writer plane, readers see only the published prefix; the only unsafe code for threading) and `RowProgress` (published MB-row count with blocking waits) |
| `src/simd/` | Motion-compensation kernel backends: `scalar.rs` (reference), `neon.rs` (aarch64 default), `x86.rs` (x86_64 default, SSE2 + runtime SSSE3/AVX2/AVX-512), `portable.rs` (`core::simd`, nightly `portable-simd` feature). `mod.rs` selects one at compile time; all must be bit-exact with scalar |

`SliceContext` bundles ~25 mutable per-MB arrays; `SliceParams` bundles read-only slice-level parameters. The `make_ctx!()` macro in `decoder.rs` constructs a `SliceContext` from local variables at each call site for zero-cost method dispatch.

## Public API

The crate exposes a minimal surface area:

- `decoder::Decoder` — low-level streaming decoder; `decode_nal` returns one frame at a time in **decode order**.
- `decoder::OrderedDecoder` — wraps `Decoder` with a built-in reorder buffer; `decode_nal` returns 0+ frames in **display order** (sorted by `(gop_id, pic_order_cnt)`). Recommended for most users — handles GOP tracking and the IDR-count timing pitfall internally. It is generic as `OrderedDecoder<M = ()>`; `decode_nal_with_meta`/`flush_with_meta` return `(Frame, M)` pairs so callers can carry per-picture metadata such as PTS timestamps through B-frame reordering; `OrderedDecoder<()>` has plain `decode_nal`/`flush`.
- `threading::ThreadedDecoder` (`std` feature) — same `decode_nal`/`flush` surface as `OrderedDecoder<()>`, decodes pictures on worker threads, output byte-identical to `OrderedDecoder`. Reference reads wait per row: luma/chroma MC (`luma_mc_stride`, `chroma_mc` taking a `ChromaRef`) wait for the rows they read, direct mode waits for the whole co-located picture via `DecodedPicture::motion()`. New reference-plane reads must go through these accessors (`luma()`, `chroma()` return only published rows).
- `decoder::Frame` — decoded YUV 4:2:0 frame with `y`/`u`/`v` planes, `width`, `height`, `pic_order_cnt`.
- `nal::parse_annex_b` — parser for start-code delimited bitstreams.
- `nal::parse_avcc` + `nal::parse_avcc_config` + `nal::AvccConfig` — parser for length-prefixed bitstreams from MP4/MKV containers.
- `nal::NalUnit`, `nal::NalUnitType` — parsed NAL unit types.
- `nal::parse_sps_info` + `nal::SpsInfo` — stream metadata (dimensions, profile, chroma format, bit depth, colour description, reorder depth) without decoding; works for unsupported formats too.
- `error::DecodeError` — `UnexpectedEof`, `InvalidSyntax(&'static str)`, `Unsupported(&'static str)`.

Everything else is `pub(crate)` or behind the `dev-internals` feature flag.

`Decoder::decode_nal` has a one-picture pipeline delay: a slice that starts a
new picture finalizes and returns the previous picture. The metadata API on
`OrderedDecoder` stores metadata for the picture currently being built and moves
it to the delayed frame when finalization occurs. Metadata passed for non-slice
NALs (SPS/PPS/SEI/etc.) is ignored, and continuation slices keep the metadata
attached by the first slice of that picture.

## Status

Baseline, Main and High profile 8-bit 4:2:0 decoding is complete: I/P/B slices with CAVLC and CABAC, 4x4 and 8x8 transforms with scaling matrices, all partition types, spatial and temporal direct mode, multiple and long-term references with list modification and MMCO, explicit and implicit weighted prediction, multi-slice pictures with per-slice deblocking, and interlaced content (MBAFF and field pictures). Output is byte-identical to FFmpeg on every 4:2:0 test and bench stream (check with `tools/verify_vs_ffmpeg.py`); unit tests pin this with SHA-256 or YUV comparisons against FFmpeg output. Other formats are rejected with `DecodeError::Unsupported`. Motion compensation has NEON and x86 SIMD backends, and `ThreadedDecoder` adds frame threading with row-level reference sync. Current performance numbers live in BENCHMARK.md.

### Completed

**Slice Header Parsing** (`src/slice.rs`)
- Full slice header parsing: slice_type, frame_num, pic_order_cnt, slice_qp_delta
- Decoded reference picture marking for IDR and non-IDR slices
- Deblocking filter parameter parsing
- P-slice fields: num_ref_idx_l0_active, ref_pic_list_modification, dec_ref_pic_marking
- B-slice fields: num_ref_idx_l1_active, direct_spatial_mv_pred_flag, L1 ref_pic_list_modification, pred_weight_table (consumed)

**Intra Macroblock Decoding** (`src/decode_cabac.rs`, `src/decode_cavlc.rs`)
- I4x4 macroblocks with all 9 prediction modes
- I16x16 macroblocks with all 4 prediction modes (vertical, horizontal, DC, plane)
- I_PCM macroblocks (raw pixel data, both CAVLC and CABAC with engine reinit)
- Coded Block Pattern (CBP) handling for luma and chroma
- Per-macroblock QP delta
- Intra MBs within P-slices

**Inter Macroblock Decoding** (`src/decode_cabac.rs`, `src/decode_cavlc.rs`)
- P_Skip macroblocks (MV = median predictor, no residual)
- P_L0_16x16 (single 16x16 partition with ref_idx, MVD, residual)
- P_L0_L0_16x8 and P_L0_L0_8x16 (two-partition modes)
- MV prediction with median and directional (match_count) logic
- Inter CBP table, inter scaling lists (indices 3-5)
- P_8x8 with all sub-partition types (8x8, 8x4, 4x8, 4x4) and P_8x8ref0
- B_Skip (spatial/temporal direct mode, per-4x4-block MV derivation, no residual)
- B_Direct_16x16 (spatial/temporal direct mode, per-4x4-block MV derivation + residual)
- B_L0_16x16, B_L1_16x16 (uni-directional), B_Bi_16x16 (bi-directional)
- Dual MV/ref_idx storage (L0 + L1) for B-slice support
- Spatial direct mode: min-positive ref_idx from neighbors, median MV prediction,
  per-4x4-block co-located zero-MV refinement with L0→L1 fallback per spec 8.4.1.2.2
- Temporal direct mode: per-4x4-block co-located MV scaling by POC distance (dist_scale_factor),
  `direct_8x8_inference_flag` support (one MV per 8x8 group from co-located picture)
- Bi-prediction averaging for luma and chroma

**Motion Compensation** (`src/inter_pred.rs`)
- Luma: 6-tap FIR filter for half-pel, bilinear averaging for quarter-pel (all 16 positions per spec Table 8-12)
- Chroma: bilinear interpolation at eighth-pel precision
- Bi-prediction: `bi_pred_avg` pixel averaging of L0 and L1 predictions
- Weighted prediction: `weighted_uni` (explicit P/B), `weighted_bi` (explicit B),
  `weighted_bi_implicit` (implicit B with POC-distance weights)
- Boundary clipping per spec 8.4.2.2.1

**Decoded Picture Buffer** (`src/dpb.rs`)
- Reference picture storage with `Arc<DecodedPicture>` sharing (pictures may still be decoding when inserted by `ThreadedDecoder`)
- Sliding window marking (spec 8.2.5.3)
- POC computation for types 0, 1, 2
- P-slice L0: short-term refs sorted by descending FrameNumWrap (decoding order, not POC; spec 8.2.4.2.1)
- B-slice L0: refs sorted by POC (before current descending, after ascending)
- B-slice L1: refs sorted by POC (after current ascending, before descending)
- `ref_pic_list_modification` (spec 8.2.4.3) with shift+insert+dedup algorithm,
  including idc=2 for long-term reference reordering; the named picture is looked up
  among all short-term references, not just the list being modified
- Long-term reference support: MMCO ops 1-6 (mark ST/LT unused, assign ST→LT,
  set max LT index, clear all, assign current as LT), IDR `long_term_reference_flag`;
  MMCO PicNums from before a frame_num wrap map back to frame_num modulo MaxFrameNum
- Long-term refs appended to reference lists after short-term refs (spec 8.2.4.2)
- Co-located picture MV/ref storage (L0 + L1) for spatial and temporal direct mode

**CAVLC Entropy Decoding** (`src/cavlc.rs`)
- Complete coeff_token VLC tables (nC 0-2, 2-4, 4-8, 8+, chroma DC)
- Trailing ones and level parsing with suffix length adaptation
- Total zeros and run-before VLC tables
- O(1) VLC decode via flat peek-indexed lookup tables (built once via `OnceLock`)

**CABAC Entropy Decoding** (`src/cabac.rs`, `src/cabac_tables.rs`)
- Binary arithmetic decoder: `get_cabac`, `get_cabac_bypass`, `get_cabac_terminate`
- Context state initialization from QP with 1024 contexts (I-slice + 3 P/B variants)
- Syntax element decoders: mb_type, skip, CBP, pred modes, ref_idx, MVD, sub_mb_type, QP delta
- Residual coefficient decoder: significance map + coefficient levels with 8-node state machine
- Per-MB neighbor tracking: CBF (luma LEFT[16]/TOP[16] + chroma), CBP (u16 with DC coded flags),
  chroma pred mode, I16x16 flag — all with proper unavailable-intra defaults (0x7CF)
- P-slice CABAC: P_Skip, P_L0_16x16/16x8/8x16, P_8x8 (all sub-partition types), intra-in-P
- B-slice CABAC: B_Skip (spatial/temporal direct), B_Direct_16x16, B_L0/L1/Bi_16x16,
  B 16x8/8x16 (18 partition variants), B_8x8 (13 sub_mb_types including B_Direct_8x8),
  intra-in-B (I4x4 and I16x16)
- Dual MVD stores (L0 + L1) for B-slice CABAC amvd context
- Category 5 (8x8 luma): no coded_block_flag (CBP bit sufficient), per-position context offsets
- `transform_size_8x8_flag` context: `399 + neighbor_transform_size` with `mb_is_8x8dct` tracking

**NAL Unit Parsing** (`src/nal.rs`)
- Annex B start code detection (3-byte and 4-byte) via `parse_annex_b`
- AVCC length-prefixed parsing (1/2/4-byte length) via `parse_avcc`
- `avcC` MP4 configuration record parsing via `parse_avcc_config`
- Emulation prevention byte removal with zero-copy fast path (`Cow::Borrowed`)
- forbidden_zero_bit validation
- Shared `parse_nal_bytes()` helper used by both Annex B and AVCC paths

**Bitstream Reader** (`src/bitstream.rs`)
- MSB-first bit reading with `read_bit`, `read_bits`, `read_ue`, `read_se`, `read_te`
- Non-consuming `peek_bits(n)` and position-advancing `skip_bits(n)`
- Padded buffer for bounds-check-free `read_bit`

**Intra Prediction** (`src/intra_pred.rs`)
- I16x16: vertical, horizontal, DC, plane (4 modes)
- I4x4: all 9 modes with above-right availability checks
- I8x8: all 9 modes at 8×8 granularity with low-pass filtered reference samples
- Chroma 8x8: DC (per-4x4-quadrant), horizontal, vertical, plane (4 modes)

**Transform & Quantization** (`src/residual.rs`)
- 4x4 inverse integer DCT
- 8x8 inverse integer DCT (High profile)
- 4x4 inverse Hadamard (I16x16 luma DC)
- 2x2 inverse Hadamard (chroma DC); 4:2:0 chroma DC dequant floors with no rounding term (spec 8.5.11.2)
- Dequantization with 4x4 and 8x8 scaling list support (SPS/PPS, fallback to default matrices)

**Deblocking Filter** (`src/deblock.rs`)
- Strong filter (bS=4) and normal filter (bS=1-3) with proper luma/chroma distinction
- Chroma: strong filter modifies only p0/q0 (spec 8.7.2.4); normal filter uses tc=tc0+1 (spec 8.7.2.3)
- Full spec 8.7.2.1 per-4x4-block boundary strength derivation:
  bS=4 (intra MB edge), bS=3 (intra internal), bS=2 (non-zero coefficients),
  bS=1 (different refs or |MV_diff|>=4), bS=0 (none). B-slice dual-list
  straight+swapped comparison.
- 8x8 transform: internal odd edges (positions 4 and 12 within MB) skipped
  per spec 8.7.2.1 — they fall inside 8x8 transform blocks
- MBAFF-aware: pair-based MB position, left/above neighbor addressing for MB pairs
- Each MB uses its own slice's settings (`SliceDeblock` by slice ID), including
  disable_deblocking_filter_idc 2 (no filtering across slice edges); Cr uses
  second_chroma_qp_index_offset
- Row stride is the coded width (planes are cropped only on output)
- Single raster-order pass, so it can run on MB ranges (`filter_mb_range`) for
  incremental deblocking in `ThreadedDecoder`; applied at picture finalization

**Multi-Slice Frame Support** (`src/decoder.rs`, `src/decode_cabac.rs`, `src/decode_cavlc.rs`)
- `PictureState` accumulates decoded MBs across multiple slices of the same picture
- Frame finalization (deblocking, DPB insert) on next picture's first slice or `flush()`
- `mb_slice_id` array tracks which slice each MB belongs to
- All CABAC neighbor context functions check slice boundaries (skip, mb_type, CBP,
  chroma pred, 8x8dct, ref_idx, MVD, coded_block_flag)
- Intra prediction sample availability gated on same-slice membership (spec 6.4.1):
  cross-slice neighbors treated as unavailable for I4x4, I8x8, I16x16 luma and chroma
  prediction in all code paths (CABAC I-slice, CABAC intra-in-P/B, CAVLC I-slice,
  CAVLC intra-in-P/B)
- CAVLC `compute_nc` checks slice boundaries for cross-MB nC derivation
- CAVLC continuation slice error recovery: backup/restore of `PictureState`

**MBAFF (Macroblock-Adaptive Frame-Field)** (`src/decoder.rs`, `src/mv_pred.rs`, `src/neighbor.rs`, `src/deblock.rs`, `src/inter_pred.rs`)
- Slice header: `field_pic_flag`, `bottom_field_flag`, `mbaff_frame_flag` parsing
- MB-pair addressing: `mb_idx = pair_addr * 2 + {0=top, 1=bottom}`, pair-based pixel coordinates
- `mb_field_decoding_flag`: per-pair flag decoded via CABAC (contexts 70-72) and CAVLC (1 bit),
  with correct skip/non-skip ordering for I/P/B slices
- MBAFF neighbor derivation (spec Tables 6-3, 6-4): `mbaff_left_neighbor`, `mbaff_above_neighbor`
  with y-coordinate remapping for all 4 frame/field mode combinations
- All 4 MV neighbor functions (left, above, above-right, above-left) MBAFF-aware
  with MVy scaling (×2 field→frame, /2 frame→field) at cross-mode boundaries
- All CABAC/CAVLC context functions MBAFF-aware: `cabac_amvd`, `cabac_neighbor_ref`,
  `cabac_neighbor_nz_luma/chroma`, `predict_i4x4_mode`, `compute_nc`
- Field-coded MB pair support: per-MB `ly_stride`/`ly_offset`/`lc_stride`/`lc_offset` for
  doubled-stride field pixel layout; field-aware MC via `luma_mc_stride` with `ref_y_offset`
  for top/bottom field reference access; field-aware intra prediction neighbor reads
- Field reference list: `effective_num_ref` doubles ref count for field MBs (each frame ref
  becomes same-parity + opposite-parity field entries); `frame_ref_idx` maps field ref_idx
  to frame-level reference list index; `mc_params` determines field offset from ref_idx parity
- Field-coded CABAC contexts: significance/last coefficient flag offsets switch to field
  tables (ctxIdx 277+/338+) per spec Table 9-34; field-coded bottom MB "above" neighbor
  uses same-field MB from above pair (not top of current pair)
- POC computation: `min(TopFieldOrderCnt, BottomFieldOrderCnt)` for types 0 and 1
- Deblocking: pair-based iteration with correct left/above neighbor addressing
- CABAC end-of-slice: `cabac_terminate` after bottom MBs only

**Error Handling** (`src/error.rs`)
- `DecodeError` enum with `UnexpectedEof`, `InvalidSyntax`, `Unsupported` variants
- Prediction functions use graceful fallback instead of panicking

**Test Coverage**
- Unit tests for each stage (bitstream, CAVLC/CABAC, transforms, intra/inter prediction,
  deblocking, DPB) plus whole-stream tests that compare decoded output against FFmpeg
  (YUV or SHA-256) for every stream in `testdata/`: synthetic CAVLC/CABAC clips, x264
  presets with and without deblocking, High profile, multi-slice, weighted prediction,
  long-term references and POC types from the JM encoder, MBAFF and field pictures,
  non-16-aligned sizes, up to 1080p
- `testdata/unsupported_*.h264`: one tiny stream per rejected format
- `testdata/fuzz_regressions/`: crash inputs replayed to check they don't panic
- Frame threading: every test stream decoded serially and at 1, 2 and 4 threads must match
- Ignored (slow) tests over the bench corpus: golden digests per SIMD backend, and
  threaded vs serial

### Not Yet Implemented

- Anything but 8-bit 4:2:0: monochrome, 4:2:2, 4:4:4, High 10 and higher bit depths, and lossless transform bypass. `check_supported` in `decoder.rs` rejects these with `DecodeError::Unsupported` when a slice activates the SPS, so they never decode into wrong pictures (`testdata/unsupported_*.h264` cover each case)
- SP/SI slice types (parsed but not decoded)
- Slice groups / FMO (returns error)
