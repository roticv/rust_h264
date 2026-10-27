//! SIMD backend dispatch for motion-compensation kernels.
//!
//! Four backends share one function set, selected at compile time:
//! - `portable` (`portable-simd` feature, nightly): `core::simd` kernels that
//!   lower to the target's native SIMD.
//! - `neon` (aarch64, default): hand-written NEON for the luma half-pel
//!   filters; everything else falls through to scalar.
//! - `x86` (x86_64, default): SSE2 baseline with runtime-detected SSSE3 /
//!   AVX2 / AVX-512 tiers (detection needs the `std` feature; without it
//!   the SSE2 baseline is used).
//! - `scalar`: the reference implementation and fallback for all other
//!   targets.
//!
//! All backends must be bit-exact with `scalar`; the golden corpus digest
//! test below is the gate.

#[cfg(all(target_arch = "aarch64", not(feature = "portable-simd")))]
mod neon;
#[cfg(feature = "portable-simd")]
mod portable;
#[allow(dead_code)]
pub(crate) mod scalar;
#[cfg(all(target_arch = "x86_64", not(feature = "portable-simd")))]
mod x86;

#[cfg(all(target_arch = "aarch64", not(feature = "portable-simd")))]
pub use neon::*;
#[cfg(feature = "portable-simd")]
pub use portable::*;
#[cfg(not(any(
    target_arch = "aarch64",
    target_arch = "x86_64",
    feature = "portable-simd"
)))]
pub use scalar::*;
#[cfg(all(target_arch = "x86_64", not(feature = "portable-simd")))]
pub use x86::*;

#[cfg(test)]
mod tests {
    use alloc::format;
    use alloc::string::String;
    use alloc::vec::Vec;

    use crate::decoder::Decoder;
    use crate::nal::parse_annex_b;
    use crate::sha256::sha256_hex;

    const GOLDEN_STREAMS: &[(&str, &str)] = &[
        (
            "bench_720p_300f_ponly",
            "540b6d538b170f21b3c26b120299c34d3d7b71b7fdb653533643d738247d7658",
        ),
        (
            "bench_720p_300f_ponly_complex",
            "4b5cce0664658bf777832652d6d56b727e9a96d88e14e193c409309771e8f525",
        ),
        (
            "bench_720p_300f_bframes",
            "1334e148259f165b5ed6410748eb946af643e1c76fdad84ea01d0a6d2b2bafbe",
        ),
        (
            "bench_720p_300f_bframes_complex",
            "3259ba108ef4b105abba2accbb1cf41565a2908b3042b5276ea389c9a008ae3f",
        ),
        (
            "bench_1080p_100f",
            "ddc43f8e744faba424cea23a6a17922a6ce6d0bbbb0a4cd4bc395536f7ba5d0c",
        ),
        (
            "bench_1080p_100f_complex",
            "9181000ebd22fc2aebbed242937649251bc85cf1aa55a0c404e8feb3d083d9ba",
        ),
        (
            "bench_1080p_100f_ponly",
            "20b52c0de018ea316525119b703489828f471035a226f49ac9f3050c45ad1de3",
        ),
    ];

    fn decode_stream_digest(name: &str) -> String {
        let h264_path = format!("{}/testdata/{}.h264", env!("CARGO_MANIFEST_DIR"), name);
        let h264_data = std::fs::read(&h264_path)
            .unwrap_or_else(|e| panic!("failed to read {}: {}", h264_path, e));
        let nals = parse_annex_b(&h264_data);
        let mut decoder = Decoder::new();
        let mut frames = Vec::new();
        for nal in &nals {
            if let Some(frame) = decoder.decode_nal(nal).unwrap() {
                frames.push(frame);
            }
        }
        if let Some(frame) = decoder.flush() {
            frames.push(frame);
        }
        frames.sort_by_key(|frame| frame.pic_order_cnt);

        let mut output = Vec::new();
        for frame in &frames {
            output.extend_from_slice(&frame.y);
            output.extend_from_slice(&frame.u);
            output.extend_from_slice(&frame.v);
        }
        sha256_hex(&output)
    }

    /// Decodes the full 720p/1080p bench corpus (~67MB of streams) and
    /// checks the active backend against digests recorded from the scalar
    /// reference. Too slow for the default run; execute with
    /// `cargo test --release -- --ignored golden_corpus_digests`.
    #[test]
    #[ignore = "slow: decodes the whole bench corpus; run with --release -- --ignored"]
    fn golden_corpus_digests() {
        for &(name, expected) in GOLDEN_STREAMS {
            assert_eq!(
                decode_stream_digest(name),
                expected,
                "digest mismatch for {name}"
            );
        }
    }
}
