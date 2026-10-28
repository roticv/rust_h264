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

The decoder logic is split across several files for maintainability:

| File | Lines | Content |
|------|-------|---------|
| `src/decoder.rs` | ~2,000 | `Decoder` (raw decode order), `OrderedDecoder` (display-order wrapper), `decode_nal`, `decode_slice` MB loop, DPB/frame management |
| `src/decode_cabac.rs` | ~3,600 | CABAC MB decode: skip detection, mb_type dispatch, residual decode |
| `src/decode_cavlc.rs` | ~2,070 | CAVLC MB decode: P/B inter, intra, residual decode |
| `src/slice_context.rs` | ~920 | `SliceContext`/`SliceParams` structs + shared methods (skip, direct, reconstruct) |
| `src/mv_pred.rs` | ~940 | MV prediction, spatial/temporal direct mode derivation |
| `src/neighbor.rs` | ~470 | CABAC neighbor context helpers, nC computation, dequant helpers |
| `src/threading.rs` | ~600 | `ThreadedDecoder`: frame threading with row-level reference sync. Coordinator plans slices (`prepare_slice`) in coded order and inserts each picture into the DPB as an in-progress `DecodedPicture`; workers run `run_slice` with a `RowSync` hook that deblocks (one unit behind decoding, since intra prediction needs unfiltered samples) and publishes final MB rows; output is committed in coded order; reorder buffer mirrors `OrderedDecoder` |
| `src/picture_sync.rs` | ~170 | `SharedPlane` (single-writer plane, readers see only the published prefix; the only unsafe code for threading) and `RowProgress` (published MB-row count with blocking waits) |
| `src/simd/` | ~450 | Motion-compensation kernel backends: `scalar.rs` (reference), `neon.rs` (aarch64 default), `x86.rs` (x86_64 default, SSE2 + runtime SSSE3/AVX2/AVX-512), `portable.rs` (`core::simd`, nightly `portable-simd` feature). `mod.rs` selects one at compile time; all must be bit-exact with scalar |

`SliceContext` bundles ~25 mutable per-MB arrays; `SliceParams` bundles read-only slice-level parameters. The `make_ctx!()` macro in `decoder.rs` constructs a `SliceContext` from local variables at each call site for zero-cost method dispatch.

## Public API

The crate exposes a minimal surface area:

- `decoder::Decoder` — low-level streaming decoder; `decode_nal` returns one frame at a time in **decode order**.
- `decoder::OrderedDecoder` — wraps `Decoder` with a built-in reorder buffer; `decode_nal` returns 0+ frames in **display order** (sorted by `(gop_id, pic_order_cnt)`). Recommended for most users — handles GOP tracking and the IDR-count timing pitfall internally. It is generic as `OrderedDecoder<M = ()>`; `decode_nal_with_meta`/`flush_with_meta` return `(Frame, M)` pairs so callers can carry per-picture metadata such as PTS timestamps through B-frame reordering while existing `decode_nal`/`flush` callers remain unchanged.
- `threading::ThreadedDecoder` (`std` feature) — same `decode_nal`/`flush` surface as `OrderedDecoder<()>`, decodes pictures on worker threads, output byte-identical to `OrderedDecoder`. Reference reads wait per row: luma/chroma MC (`luma_mc_stride`, `chroma_mc` taking a `ChromaRef`) wait for the rows they read, direct mode waits for the whole co-located picture via `DecodedPicture::motion()`. New reference-plane reads must go through these accessors (`luma()`, `chroma()` return only published rows).
- `decoder::Frame` — decoded YUV 4:2:0 frame with `y`/`u`/`v` planes, `width`, `height`, `pic_order_cnt`.
- `nal::parse_annex_b` — parser for start-code delimited bitstreams.
- `nal::parse_avcc` + `nal::parse_avcc_config` + `nal::AvccConfig` — parser for length-prefixed bitstreams from MP4/MKV containers.
- `nal::NalUnit`, `nal::NalUnitType` — parsed NAL unit types.
- `error::DecodeError` — `UnexpectedEof`, `InvalidSyntax(&'static str)`, `Unsupported(&'static str)`.

Everything else is `pub(crate)` or behind the `dev-internals` feature flag.

`Decoder::decode_nal` has a one-picture pipeline delay: a slice that starts a
new picture finalizes and returns the previous picture. The metadata API on
`OrderedDecoder` stores metadata for the picture currently being built and moves
it to the delayed frame when finalization occurs. Metadata passed for non-slice
NALs (SPS/PPS/SEI/etc.) is ignored, and continuation slices keep the metadata
attached by the first slice of that picture.

## Status

I-frame, P-frame, and B-frame decoding fully functional with both CAVLC and CABAC. High profile 8x8 transform supported for both CAVLC and CABAC (intra and inter). Multi-reference (ref>1) with ref_pic_list_modification supported. Multi-slice frames fully supported for both CABAC and CAVLC (I, P, and B-frames byte-exact). MBAFF (Macroblock-Adaptive Frame-Field) interlaced content fully supported — both frame-coded and field-coded MB pairs byte-exact including I/P/B slices, CABAC/CAVLC, deblocking, High profile 8x8 DCT, implicit weighted bi-prediction, and field-aware MC/intra prediction with doubled stride. 178 unit tests (including 15 MBAFF-specific byte-exact stream tests), x264 `--preset medium` with and without deblocking (320x240, 60 frames, ref=4, bframes=3), multi-slice streams with up to 4 slices per frame, and 1080p streams (1920x1080, 10 frames, CABAC/CAVLC, with/without deblocking). Also verified byte-exact at 720p (1280x720, 300 frames, bframes=3 ref=4). Explicit weighted prediction for P-slices and B-slices, plus implicit weighted bi-prediction for B-slices. NEON SIMD acceleration on aarch64 for luma half-pel filters (~28% speedup) achieving 67 fps at 1080p.

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
- Reference frame storage with `Rc<DecodedPicture>` sharing
- Sliding window marking (spec 8.2.5.3)
- POC computation for types 0, 1, 2
- P-slice L0: short-term refs sorted by descending frame_num
- B-slice L0: refs sorted by POC (before current descending, after ascending)
- B-slice L1: refs sorted by POC (after current ascending, before descending)
- `ref_pic_list_modification` (spec 8.2.4.3) with shift+insert+dedup algorithm,
  including idc=2 for long-term reference reordering
- Long-term reference support: MMCO ops 1-6 (mark ST/LT unused, assign ST→LT,
  set max LT index, clear all, assign current as LT), IDR `long_term_reference_flag`
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
- I4x4 and I16x16 integration: byte-exact output for single-MB, ±1 IDCT tolerance for multi-MB
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
- 2x2 inverse Hadamard (chroma DC)
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
- Applied automatically after slice decode

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

**Test Coverage** (178 tests, all byte-exact against FFmpeg)
- Intra (CAVLC): single_frame, multi_mb_frame, i4x4_frame, deblock_frame,
  mixed_i4x4_frame, gradient_48x32, edges (QP=10/35), smooth_80x48,
  noise_16x16, scaling_test
- P-slice: p_frame_test, p_skip_heavy, p_multi_frame, p_8x8_test, p_multiref
- B-slice: b_l0_l1_test, b_bi_test, b_skip_test (spatial direct),
  b_temporal_test, b_parts_test (16x8/8x16/8x8), b_multi_test, b_hier_test
  (hierarchical B-frames with ref_pic_list_modification)
- CABAC: cabac_i4x4_test, cabac_i16x16_test, cabac_mixed_test,
  cabac_p_test, cabac_p_parts_test (P16x8/8x16/8x8/4x4 sub-partitions),
  cabac_intra_p_test (I16x16-in-P),
  cabac_b_test (B_Skip spatial direct),
  cabac_b_parts_test (B16x16/16x8/8x16/8x8/Direct/Skip with L0/L1/Bi),
  cabac_intra_b_test (I16x16-in-B), cabac_b_temporal_test (temporal direct),
  cabac_high_profile (8x8 inter), cabac_deblock_test (deblocking enabled),
  cabac_i8x8_test (I8x8 intra with chroma),
  cabac_multiref_test (ref=2 with ref_pic_list_modification)
- Deblocking: deblock_frame, deblock_b_test (B-frames + deblock),
  deblock_b_inter_test (B inter + cross-list bS)
- Weighted prediction: weighted_p_test (CAVLC, 100% weighted P, fading),
  weighted_b_test (CABAC, implicit weighted B idc=2)
- High profile: high_profile_test (320x240 CAVLC, 8x8 intra+inter)
- Real-world: realworld_test (320x240 P-only),
  realworld_b_test (320x240 with B-frames),
  preset_medium (320x240, 60 frames, x264 --preset medium, no-deblock),
  preset_medium_deblock (same with deblocking ON)
- Multi-slice: ms_cabac_i_test (32x32, 2-slice CABAC I-frame),
  ms_cabac_i4_test (64x64, 4-slice CABAC I-frame),
  ms_cavlc_i_test (32x32, 2-slice CAVLC I-frame),
  ms_cavlc_p_test (64x64, 5-frame 4-slice CAVLC with P-frames),
  ms_cabac_p_test (64x64, 5-frame 4-slice CABAC with P-frames),
  ms_cabac_b_test (64x64, 4-frame 4-slice CABAC with B-frames),
  b_temporal_direct_test (64x64, 4-frame, preset slower temporal direct 8x8 inference),
  high_p8x8_sub4x4_test (64x64, 6-frame, High profile P_8x8 sub-4x4 + 8x8dct),
  high_b_slower_test (64x64, 10-frame, High profile preset slower ref=2 B-frames 8x8dct),
  high_cavlc_b_test (64x64, 10-frame, CAVLC High profile bframes=2 ref=2 8x8dct),
  ms_deblock_b_cabac_test (64x64, 8-frame, CABAC 4-slice bframes=2 ref=2 deblock),
  ms_cavlc_b_test (64x64, 8-frame, CAVLC 4-slice bframes=2 ref=2),
  cavlc_deblock_pb_test (64x64, 8-frame, CAVLC P+B with deblocking),
  unaligned_100x76_test (100x76, 6-frame, non-16-aligned dimensions),
  cabac_weighted_p_test (64x64, 8-frame, CABAC 100% weighted P fading),
  cavlc_i8x8_test (64x64, 3-frame, CAVLC High profile I8x8 intra),
  high_preset_medium_test (320x240, 30-frame, High profile bframes=3 ref=4 8x8dct no-deblock),
  high_deblock_medium_test (320x240, 30-frame, High profile bframes=3 ref=4 8x8dct deblock ON),
  constrained_intra_test (64x64, 8-frame, CABAC constrained_intra_pred_flag=1 bframes=1 ref=2),
  cabac_b8x8_direct_test (64x64, 8-frame, CABAC B_8x8 with B_Direct_8x8 sub-partitions),
  jm_ltr_cavlc_test (64x64, 8-frame, JM CAVLC long-term reference),
  jm_ltr_cabac_test (64x64, 8-frame, JM CABAC long-term reference),
  jm_weighted_b_explicit_test (64x64, 8-frame, JM CABAC weighted_bipred_idc=1),
  jm_poc_type1_test (64x64, 6-frame, JM CAVLC pic_order_cnt_type=1),
  jm_poc_type2_test (64x64, 6-frame, JM CAVLC pic_order_cnt_type=2),
  jm_ipcm_cavlc_test (32x32, 4-frame, JM CAVLC I_PCM macroblocks QP=0),
  jm_ipcm_cabac_test (32x32, 4-frame, JM CABAC I_PCM macroblocks QP=0)
- 1080p (SHA-256 hash comparison): 1080p_test (1920x1080, 10-frame, CABAC bframes=3 ref=2 no-deblock),
  1080p_deblock_test (same with deblocking ON),
  1080p_cavlc_test (CAVLC, bframes=3 ref=2 no-deblock)
- MBAFF: mbaff_cavlc_test (64x64, 6-frame, CAVLC frame-coded pairs),
  mbaff_p_cavlc_8f_test (64x64, 8-frame, CAVLC P-frames),
  mbaff_high_cavlc_test (64x64, 6-frame, High profile CAVLC 8x8 transform),
  mbaff_cabac_i_test (32x32, 1-frame, CABAC I-frame),
  mbaff_cabac_64_test (64x64, 1-frame, CABAC I-frame),
  mbaff_cabac_p_test (64x64, 5-frame, CABAC P-frames),
  mbaff_cabac_b_test (64x64, 8-frame, CABAC bframes=2 ref=2),
  mbaff_cavlc_b_test (64x64, 8-frame, CAVLC bframes=2 ref=2),
  mbaff_deblock_cavlc_test (64x64, 8-frame, CAVLC Main profile deblocking),
  mbaff_deblock_cabac_test (64x64, 8-frame, CABAC Main profile bframes=2 ref=2 deblocking),
  mbaff_high_deblock_test (64x64, 8-frame, High profile 8x8dct bframes=2 ref=2 deblocking),
  mbaff_field_i_test (64x64, 3-frame, CAVLC all-field-coded I-only),
  mbaff_field_p_test (64x64, 4-frame, CAVLC all-field-coded P-frames),
  mbaff_field_cabac_test (64x64, 4-frame, CABAC all-field-coded Main profile),
  mbaff_field_high_test (64x64, 4-frame, CAVLC all-field-coded High profile 8x8dct)

### Not Yet Implemented

- Anything but 8-bit 4:2:0: monochrome, 4:2:2, 4:4:4, High 10 and higher bit depths, and lossless transform bypass. `check_supported` in `decoder.rs` rejects these with `DecodeError::Unsupported` when a slice activates the SPS, so they never decode into wrong pictures (`testdata/unsupported_*.h264` cover each case)
- SP/SI slice types (parsed but not decoded)
- Slice groups / FMO (returns error)
