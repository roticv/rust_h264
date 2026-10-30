# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **4:2:2 streams** (High 4:2:2 profile, 8-bit) now decode instead of being
  rejected, progressive and interlaced (MBAFF and field pictures).
  `Frame` has a new `chroma_format` field (`ChromaFormat::Yuv420` or
  `Yuv422`; code that builds `Frame` values must set it) and
  `chroma_width()`/`chroma_height()`; 4:2:2 frames have full-height U and V
  planes. Output matches FFmpeg's yuv422p on every 8-bit 4:2:2 JVT
  conformance vector (the other 4:2:2 vectors are 10-bit) and on x264 test
  streams.
- **Monochrome (4:0:0) streams** now decode instead of being rejected. Frames
  keep the 4:2:0 layout with U and V filled with 128, matching FFmpeg's
  output. Both JVT monochrome conformance vectors pass.
- `tools/conformance/run.sh` runs the JVT conformance suites (JVT-AVC_V1,
  JVT-FR-EXT) through Fluster with a new `conformance_decode` example, and
  fails on any result other than the expected unsupported-format vectors.
  Every 8-bit 4:2:0, 4:2:2 and monochrome vector passes: 186 of 204 (up
  from 81 before this release's conformance fixes). The other 18 are
  10-bit, slice groups (FMO) and SP/SI slices, all rejected with
  `DecodeError::Unsupported`.

### Fixed
- **MBAFF streams from x264 (`--interlaced`)**, which failed with "invalid
  coded_block_pattern" or decoded with corrupted macroblocks, now decode
  byte-identical to FFmpeg. Pairs mixing frame and field coding were handled
  with simplified neighbour rules; they now follow spec Table 6-4 everywhere
  (motion vector prediction, CABAC and CAVLC contexts, intra prediction modes
  and availability). Also fixed for MBAFF:
  - the field flag of skipped macroblocks, and of a skipped top macroblock above
    a coded bottom one;
  - reference fields, explicit and implicit weights, and chroma offsets of field
    macroblocks in every inter path, including CAVLC B slices and direct mode;
  - direct mode with a co-located pair coded the other way, and temporal
    direct for field macroblocks;
  - CABAC 8x8 significance contexts and the CAVLC 8x8 coefficient order in
    field macroblocks;
  - deblocking of field macroblocks and of edges between frame and field
    pairs.
- CAVLC macroblocks with the 8x8 transform deblocked the inner edges of their
  8x8 blocks.
- Fixes found with the JVT conformance suites (run through Fluster):
  - POC types 1 and 2 ignored FrameNumOffset, so POC fell back when frame_num
    wrapped; type 1 also mishandled non-reference pictures, and IDR pictures
    of types 0 and 1 took POC 0 instead of their coded values.
  - Pictures with MMCO 5 kept their POC and frame_num instead of restarting
    at 0, and did not start a new output period, so `OrderedDecoder` and
    `ThreadedDecoder` mixed their output with earlier pictures.
  - Gaps in frame_num (`gaps_in_frame_num_value_allowed_flag`) did not insert
    "non-existing" frames, leaving the wrong references in the DPB.
  - Initial reference lists were not cut to `num_ref_idx_lX_active` before
    modification, and modification of long-term references
    (`modification_of_pic_nums_idc` 2) matched the wrong picture.
  - 8x8 scaling matrices were applied in raster instead of zigzag order;
    absent lists did not follow the fall-back rules (default 8x8 lists, PPS
    lists copying the previous PPS list), and "use default" lists were not
    recognised.
  - Cr was dequantized with `chroma_qp_index_offset` instead of
    `second_chroma_qp_index_offset`.
  - After an I_PCM macroblock the next macroblock's QP was predicted from 0,
    and CABAC CBP contexts treated I_PCM as having no coded blocks.
  - Direct and skipped B macroblocks without `direct_8x8_inference_flag`
    predicted chroma with one motion vector per 8x8 block.
  - Slices always used the first PPS received instead of the one their
    header names.
  - Pictures marking references adaptively (MMCO) also ran the sliding
    window, dropping a reference too many.
  - Spatial direct mode applied the co-located zero-motion rule with a
    long-term RefPicList1[0]; temporal direct scaled motion for long-term
    references and implicit weights weighted them.
  - Field reference lists alternated parity frame by frame instead of
    taking the next field of each parity in turn (spec 8.2.4.2.5).
  - PAFF (streams mixing frame and field pictures): field pictures could
    not reference a decoded frame, nor frame pictures a field pair. The DPB
    now provides each as the other on demand, and direct mode reads the
    co-located motion across the two structures (spec Table 8-6).
  - Field pictures whose fields share a POC confused the two fields in
    temporal direct mode and in deblocking.
  - Direct mode between MBAFF frames and field pictures: an MBAFF frame
    over a field pair, and a field picture over the field macroblocks of an
    MBAFF frame, read the wrong co-located block or reference.
  - With constrained intra prediction in MBAFF frames, the left samples of
    a macroblock's upper and lower halves can come from different
    macroblocks; intra prediction now checks each half (spec 8.3.1.2,
    8.3.4).
  - MMCO commands in field pictures read PicNum and LongTermPicNum as
    frame numbers instead of naming a field; one long-term index given to
    both fields of a frame dropped the first; and commands naming a frame
    decoded as two fields changed only one of them.
  - The sliding window counted a long-term frame and a later short-term
    frame with the same frame_num (after it wrapped) as one frame.
- Temporal direct mode used no motion when the co-located block predicted from
  list 1 only, and identified the co-located references of a multi-slice
  picture through the last slice's reference list.

### Changed
- **`Frame` has a new public field, `chroma_format`** (see 4:2:2 above), so
  code that constructs `Frame` values must set it.
- Monochrome and 4:2:2 streams are no longer rejected with
  `DecodeError::Unsupported`; 4:4:4, bit depths above 8 and lossless still
  are.
- `bench_decode` times parsing and decoding only. It used to include
  hashing the output, about as long as decoding itself on the 1080p
  streams, so its fps figures were roughly half the decoder's.
  `BENCH_STREAM=<substring>` runs only matching streams, and a new
  `bench_1080p_100f_deblock` stream has the deblocking filter on (the
  others are encoded without it).

### Performance
Decoding is 1.5-2x faster than before this release's speed work, measured
single-threaded on Apple Silicon (decode-only fps, bench streams):

| Stream | Before | After |
|---|---|---|
| 720p P-only | 171 | 337 |
| 720p P-only complex | 355 | 556 |
| 720p B-frames | 188 | 351 |
| 720p B-frames complex | 327 | 489 |
| 1080p | 96 | 184 |
| 1080p complex | 146 | 218 |
| 1080p P-only | 163 | 258 |

The deblocked 1080p stream went from 159 to 192 fps with the deblocking
changes alone. Output is byte-identical throughout.

- NEON (aarch64) kernels for the quarter-pel and 2D half-pel luma filters,
  chroma bilinear interpolation and bi-prediction averaging, which had
  fallen back to scalar code (+36-43% on motion-heavy streams).
- NEON deblocking: each edge is filtered in one call over its 16 lines with
  per-line strength, vertical edges through 8x8 transposes. bS is worked
  out before the filter thresholds, all-zero edges are skipped, and
  internal edges of macroblocks with one motion skip the motion comparison
  (deblocking went from ~26% to ~10% of decode time).
- Predictions and residuals are written into the frame a row at a time
  instead of one bounds-checked sample at a time; P-skip and B-skip copy
  whole rows, and B-skip motion-compensates per 8x8 instead of per 4x4.
- Branch-free, vectorizable 4x4 and 8x8 dequantization.
- CABAC: no bounds checks on each decoded bin, table-driven significance
  map contexts, and residual coefficients decoded into a reused buffer
  instead of a zeroed 1 KB struct per block.
- Short motion-compensation row copies no longer call `memmove`.
- Every SIMD kernel is checked against the scalar reference on random
  input (`kernels_match_scalar`).

## [0.5.0] - 2026-10-28

Thanks to petitstrawberry, codepaws and Brandon Hoult, whose forks and pull
requests contributed several of these changes.

### Added
- **Field pictures** (`field_pic_flag = 1`, PAFF): top and bottom fields are
  decoded as separate pictures and interleaved into frames for output.
- **`threading::ThreadedDecoder`**: frame-threaded decoding with the same
  `decode_nal`/`flush` surface as `OrderedDecoder` and byte-identical output.
  Pictures wait only for the reference rows their motion vectors read, so P-only
  as well as B-frame streams scale (about 1.4-1.8x at 4 threads; see
  BENCHMARK.md). Requires the `std` feature.
- **Per-picture metadata through reordering**: `OrderedDecoder<M>` with
  `decode_nal_with_meta`/`flush_with_meta` returns `(Frame, M)` pairs, so PTS
  values stay with the right frame through B-frame reordering.
- **`nal::parse_sps_info` / `nal::SpsInfo`**: dimensions, profile, chroma format,
  bit depth, colour description and reorder depth from an SPS without decoding.
- **`no_std` + `alloc` support**: the new default `std` feature can be turned
  off.
- **x86-64 SIMD** motion compensation (SSE2, with SSSE3/AVX2/AVX-512 detected at
  runtime), next to the existing NEON backend; optional `core::simd` backend
  behind the nightly-only `portable-simd` feature.
- Cargo features `minifb` (player example), `profile` (pprof and phase timers)
  and `dev-internals` (internal modules for diagnostics; not a stable API).
- `tools/verify_vs_ffmpeg.py`: compares decoded output with FFmpeg byte for byte
  for `.h264` files or any container FFmpeg reads.
- CI for std and no_std builds, an embedded no_std target, rustfmt and clippy.

### Changed
- **Unsupported formats are rejected**: monochrome, 4:2:2, 4:4:4, bit depths
  above 8 and lossless (transform bypass) streams now fail with
  `DecodeError::Unsupported` on every slice instead of decoding into wrong
  pictures.
- `OrderedDecoder` is generic over a metadata type with default `()`; existing
  `decode_nal`/`flush` callers are unaffected.
- A new `std` feature is on by default. Builds that already disable default
  features now get the `no_std` build, without `std::error::Error` for
  `DecodeError`.
- Rust 1.79 or newer is required (inline `const` blocks, raw slice pointer
  `len`).

### Fixed
- **P-slice reference lists** were ordered by POC; they now follow FrameNumWrap
  (decoding order), which matters once B-pictures are references (x264
  B-pyramid, NVENC).
- **Reference list modification** now finds the named picture among all
  short-term references, not only the list being modified (x264 `weightp`
  duplicates references and pushes entries out).
- **MMCO operations** naming a picture from before frame_num wrapped matched
  nothing, leaving a stale reference.
- **Deblocking** used the cropped display width as the row stride, so streams
  cropped on the right were filtered in the wrong place.
- **Deblocking** now uses each slice's own settings, implements
  `disable_deblocking_filter_idc` 2, and uses `second_chroma_qp_index_offset`
  for Cr.
- **4:2:0 chroma DC dequantization** added a rounding term the spec does not
  have; at chroma QP below 6 this was off by one.
- Field-pair output with mismatched field geometry (found by fuzzing).

  Together these make every 4:2:0 test and benchmark stream byte-identical to
  FFmpeg, including x264 streams with B-pyramid, `weightp` and sliced threads
  that previously drifted.

### Performance
- B-skip and direct macroblocks with direct 8x8 inference derive their motion
  once per macroblock, with a whole-macroblock motion compensation fast path
  (+13-18% on B-frame content).
- CABAC residual decoding no longer allocates (88-97% fewer allocations).
- Fewer allocations in CAVLC partition decoding and output cropping.

## [0.4.0] - 2026-04-19

### Added
- **MBAFF (Macroblock-Adaptive Frame-Field) interlaced support** — the decoder
  now handles interlaced H.264 content encoded with `mb_adaptive_frame_field_flag`.
  Both frame-coded and field-coded MB pairs are fully supported:
  - Pair-based MB addressing with `mb_field_decoding_flag` decode (CABAC contexts
    70-72 and CAVLC 1-bit)
  - MBAFF neighbor derivation per spec Tables 6-3/6-4 with y-coordinate remapping
    for all 4 frame/field mode combinations
  - MVy scaling at cross frame/field boundaries (×2 field→frame, /2 frame→field)
  - All CABAC/CAVLC context functions updated for MBAFF pair-based addressing
  - Field-coded pixel layout with doubled stride and field-line offset
  - Field-aware motion compensation via `luma_mc_stride` with per-field reference
    buffer offset for top/bottom field access
  - Field reference lists: `num_ref_idx` doubled for field-coded MBs, `ref_idx`
    maps same-parity and opposite-parity fields
  - Field-coded CABAC significance/last coefficient contexts (ctxIdx 277+/338+
    per spec Table 9-34)
  - Field-coded CABAC neighbor addressing: bottom field MBs use same-field MB
    from above pair (not top of current pair)
  - MBAFF-aware deblocking filter with pair-based iteration and correct
    left/above neighbor addressing
  - MBAFF-aware POC computation: `min(TopFieldOrderCnt, BottomFieldOrderCnt)`
    for POC types 0 and 1
- 15 new MBAFF-specific byte-exact tests covering CAVLC/CABAC, I/P/B slices,
  frame-coded and field-coded pairs, deblocking, High profile 8x8 DCT, and
  implicit weighted bi-prediction. Total test count: 178.

### Fixed
- POC computation for `pic_order_cnt_type` 0 and 1 now correctly returns
  `min(TopFieldOrderCnt, BottomFieldOrderCnt)` instead of just
  `TopFieldOrderCnt`. Fixes implicit weighted bi-prediction for MBAFF
  B-frames with non-zero `delta_pic_order_cnt_bottom`.
- Above-right MV neighbor for MBAFF bottom MBs no longer reads from
  undecoded pairs due to `mb_slice_id` initialization matching
  `this_slice_id=0`. Falls back to above-left correctly.
- **Fuzzing robustness**: fixed multiple integer overflow panics found by
  fuzzing on malformed bitstreams:
  - IDCT 4x4 and 8x8 add/sub overflows (use wrapping arithmetic)
  - POC type 1 multiplication overflow and MSB computation overflow
  - POC shift overflow (clamp shift amount)
  - Dequant DC multiply overflow
  - SPS scaling list delta add overflow
  - Deblocking filter multiply overflow
  - SPS width overflow
- **Bounds checking**: fixed multiple out-of-bounds panics on malformed
  bitstreams:
  - Chroma MC reference plane boundary checks
  - CABAC I_PCM frame buffer index validation
  - Bitstream reader position validation
  - Empty reference list safe indexing
  - CAVLC coefficient position underflow guard
  - Missing bounds/range validation on values from malformed bitstreams

## [0.3.0] - 2026-04-09

### Added
- `OrderedDecoder` — wraps `Decoder` with a built-in reorder buffer that
  emits frames in display order automatically. Handles IDR boundary tracking
  internally, eliminating a common pitfall where callers would tag the last
  B-frame of a GOP with the wrong GOP id and produce visible glitches at
  scene cuts. Recommended for most users.
- AVCC (length-prefixed) NAL parser for MP4/MKV containers:
  - `nal::parse_avcc(data, length_size)` — parse a length-prefixed sample
  - `nal::parse_avcc_config(avcc_box)` — parse an MP4 `avcC` configuration
    record and extract SPS/PPS NALs
  - `nal::AvccConfig` struct exposing `length_size`, `sps_nals`, `pps_nals`
- NEON SIMD acceleration on aarch64 for luma half-pel filters
  (`half_pel_h`, `half_pel_v`, quarter-pel paths) and chroma bilinear
  interpolation. Requires no opt-in — enabled automatically on aarch64.
- Comprehensive doc comments on the entire public API. Crate-level docs,
  module-level intros, and runnable examples render on docs.rs.
- Test for `OrderedDecoder` validating display-order output against the
  manual sort baseline on a real preset_medium stream.

### Changed
- **Performance**: 1080p decode improved from ~48 fps to ~67 fps (28% speedup)
  on Apple Silicon thanks to NEON luma half-pel intrinsics. 1080p @ 60 fps
  target achieved.
- **Performance**: 720p P-only improved from ~186 fps to ~278 fps;
  720p with B-frames from ~190 fps to ~299 fps.
- README.md restructured to lead with `OrderedDecoder` as the recommended
  entry point. The lower-level `Decoder` is documented as an advanced
  option for callers who need raw decode order.
- `parse_annex_b` and AVCC parsers now share a common `parse_nal_bytes`
  helper for NAL header parsing and emulation-prevention removal.

### Fixed
- Build clean on stable Rust with no clippy warnings.

## [0.2.0] - 2026-04-04

### Added
- Initial release on crates.io. Pure Rust H.264 decoder supporting Baseline,
  Main, and High profile (8-bit 4:2:0 progressive) with both CAVLC and CABAC
  entropy coding.
- Full support for I/P/B-frame decoding, multi-reference prediction with
  `ref_pic_list_modification`, multi-slice frames, weighted prediction
  (explicit and implicit), long-term references, MMCO ops 1-6, deblocking
  filter, intra prediction (I4x4/I8x8/I16x16, all 9 modes for 4x4 and 8x8),
  spatial and temporal direct mode for B-slices.
- Annex B bitstream parser (`nal::parse_annex_b`) with zero-copy emulation
  prevention removal.
- 119 unit tests, including 73 byte-exact stream tests against FFmpeg, with
  coverage up to 1080p (CABAC, CABAC+deblock, CAVLC) using SHA-256 hash
  comparison to keep test data small.
- License: dual MIT / Apache-2.0.

### Performance
- ~48 fps at 1080p (mandelbrot content, x264 `--preset medium` bframes=3 ref=2)
- ~186 fps at 720p P-only / ~190 fps at 720p with B-frames
- See `BENCHMARK.md` for detailed profile breakdown and methodology.

### Not implemented
- Interlaced coding (MBAFF, field pictures)
- High 10 / 4:2:2 / 4:4:4 profiles
- SP/SI slice types
- Slice groups / FMO
