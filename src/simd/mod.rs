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

    use crate::decoder::OrderedDecoder;
    use crate::nal::parse_annex_b;
    use crate::sha256::sha256_hex;

    const GOLDEN_STREAMS: &[(&str, &str)] = &[
        (
            "bench_720p_300f_ponly",
            "2344dcb88dc01d07fd2afa1ed0f7e3413afe04dab40094cd6a70e047e5d8cb8b",
        ),
        (
            "bench_720p_300f_ponly_complex",
            "2e7f958b99c09d9c972ad87e505cdb8fb06fa470c4b07a4e7cb0fe59784c49e0",
        ),
        (
            "bench_720p_300f_bframes",
            "5ff37fa29831240d996febaf69549b59bdf045d691a1b5bd9226f8c25bbb16c7",
        ),
        (
            "bench_720p_300f_bframes_complex",
            "3c5dbbee6379c7b53bc49eb6b67b816cc325b44935ad6c1a4f3b9e905632630f",
        ),
        (
            "bench_1080p_100f",
            "38a94a608ca0af062d2d5654cd7882b74988ab3b59516bb3a91fd97fa156ce32",
        ),
        (
            "bench_1080p_100f_complex",
            "9181000ebd22fc2aebbed242937649251bc85cf1aa55a0c404e8feb3d083d9ba",
        ),
        (
            "bench_1080p_100f_ponly",
            "e17bb157f49784e4e596451b2b84d96714c4c4cbd3e9ac44ed7cb19a385ce7d2",
        ),
    ];

    fn decode_stream_digest(name: &str) -> String {
        let h264_path = format!("{}/testdata/{}.h264", env!("CARGO_MANIFEST_DIR"), name);
        let h264_data = std::fs::read(&h264_path)
            .unwrap_or_else(|e| panic!("failed to read {}: {}", h264_path, e));
        let nals = parse_annex_b(&h264_data);
        // Display order, as ffmpeg outputs it (POC order within each IDR
        // period; a POC sort across IDRs is ambiguous)
        let mut decoder = OrderedDecoder::new();
        let mut frames = Vec::new();
        for nal in &nals {
            frames.extend(decoder.decode_nal(nal).unwrap());
        }
        frames.extend(decoder.flush());

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
