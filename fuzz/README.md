# Fuzz testing

Fuzz targets for `rust_h264` using [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz)
and libFuzzer. The goal is to verify that no input — however malformed —
causes a panic, abort, or undefined behaviour in the parsers or decoder.

## Setup

```sh
cargo install cargo-fuzz
```

cargo-fuzz requires nightly Rust because it uses libFuzzer's runtime. Install
with `rustup toolchain install nightly` if you don't already have it.

## Available targets

| Target | What it fuzzes |
|---|---|
| `parse_annex_b` | The Annex B start-code parser (`nal::parse_annex_b`) |
| `parse_avcc` | The AVCC length-prefixed parser (`nal::parse_avcc`) |
| `parse_avcc_config` | The MP4 `avcC` configuration record parser (`nal::parse_avcc_config`) |
| `decode_annex_b` | End-to-end: parse Annex B + feed every NAL to `Decoder` |
| `decode_avcc` | End-to-end: split input into avcC + sample, feed both to `Decoder` |
| `decode_slice_data` | The macroblock layer: a real test stream with its slice NALs damaged by small edits (byte XORs, truncations) the fuzzer picks, decoded with `OrderedDecoder` |
| `decode_threaded` | The `decode_slice_data` inputs decoded with `ThreadedDecoder` (3 threads) |

Whole-stream byte mutations (`decode_annex_b`) mostly break the SPS, PPS or
slice header and stop there. `decode_slice_data` and `decode_threaded` keep
those valid: their first input byte picks one of the base streams listed in
`common/slice_mutation.rs` (CAVLC and CABAC, B-frames, MBAFF and field
pictures, I_PCM, weighted prediction, long-term references, 10-bit, 4:2:2,
monochrome), and each following 4-byte group is one edit to one of its slices.

## Running

From the repo root:

```sh
cargo +nightly fuzz run parse_annex_b
cargo +nightly fuzz run decode_annex_b
```

Each target runs indefinitely. Press Ctrl-C to stop. Crashes are saved under
`fuzz/artifacts/<target>/` and can be reproduced with:

```sh
cargo +nightly fuzz run <target> fuzz/artifacts/<target>/<crash-id>
```

To minimize a crash to the smallest input that still triggers it:

```sh
cargo +nightly fuzz tmin <target> fuzz/artifacts/<target>/<crash-id>
```

## Seeding the corpus

For faster initial coverage, copy real `.h264` files into the corpus directory:

```sh
mkdir -p fuzz/corpus/decode_annex_b
cp ../testdata/*.h264 fuzz/corpus/decode_annex_b/
```

libFuzzer will mutate these to discover new code paths much faster than
starting from random bytes.

The slice-data targets need no files: one single-byte input per base stream
(byte values 0 to 21) decodes each stream unchanged, and libFuzzer adds edits
from there:

```sh
mkdir -p fuzz/corpus/decode_slice_data
for i in $(seq 0 21); do printf "\\$(printf %03o $i)" > fuzz/corpus/decode_slice_data/seed_$i; done
```

## Time budget

A few minutes per target catches obvious crashes. An overnight run on the
end-to-end targets gives much better coverage.

CI (`.github/workflows/fuzz.yml`) runs every target daily, 3 to 15 minutes
each, and keeps each target's corpus in the Actions cache between runs, so
coverage accumulates instead of restarting from the seeds every time.
