//! Pure Rust H.264/AVC video decoder.
//!
//! A standalone, portable software H.264 decoder. Supports Baseline, Main,
//! High and High 4:2:2 profile (8-bit 4:2:0, 4:2:2 and monochrome,
//! progressive and interlaced), with both CAVLC and CABAC entropy coding,
//! B-frames, multi-reference, weighted prediction, long-term references, and
//! multi-slice frames. NEON SIMD acceleration on aarch64,
//! with an optional `core::simd` backend behind the nightly-only
//! `portable-simd` feature.
//!
//! # Quick start
//!
//! ```no_run
//! use rust_h264::decoder::Decoder;
//! use rust_h264::nal::parse_annex_b;
//!
//! let bitstream = std::fs::read("input.h264").unwrap();
//! let nals = parse_annex_b(&bitstream);
//! let mut decoder = Decoder::new();
//!
//! for nal in &nals {
//!     if let Ok(Some(frame)) = decoder.decode_nal(nal) {
//!         // `frame` has y/u/v planes (4:2:0 or 4:2:2), width, height, pic_order_cnt
//!     }
//! }
//! if let Some(frame) = decoder.flush() {
//!     // final pending frame
//! }
//! ```
//!
//! # Input formats
//!
//! Two parsers are provided in the [`nal`] module:
//!
//! - [`nal::parse_annex_b`] for start-code delimited streams (`.h264` files,
//!   RTP, broadcast TS).
//! - [`nal::parse_avcc`] + [`nal::parse_avcc_config`] for length-prefixed
//!   streams from MP4/MKV containers.
//!
//! Both produce [`nal::NalUnit`] values that can be fed to
//! [`decoder::Decoder::decode_nal`].
//!
//! # Frame ordering
//!
//! [`decoder::Decoder::decode_nal`] returns frames in **decode order**, which
//! differs from display order whenever B-frames are present. To present
//! frames in display order, sort by `pic_order_cnt` within each GOP.
//!
//! Note that `decode_nal` returns the *previous* picture when it sees a new
//! one, so be careful about IDR boundary tracking — increment your GOP
//! counter **after** the call, not before. See `examples/dump_frames.rs` and
//! `examples/play.rs` for working patterns.
//!
//! # Not supported
//!
//! Only 8-bit 4:2:0, 4:2:2 and monochrome (4:0:0, output as 4:2:0 with
//! mid-grey chroma) video is decoded; [`Frame::chroma_format`](decoder::Frame::chroma_format)
//! says which layout the chroma planes have. Streams using 4:4:4 chroma, bit
//! depths above 8 (High 10 and up) or lossless transform
//! bypass are rejected with [`DecodeError::Unsupported`](error::DecodeError::Unsupported)
//! on every slice, rather than decoded into wrong pictures.
//!
//! - SP/SI slice types
//! - Slice groups / FMO

#![cfg_attr(feature = "portable-simd", feature(portable_simd))]
#![cfg_attr(not(feature = "std"), no_std)]

#[macro_use]
extern crate alloc;

// Tests read files and use std collections even when the `std` feature is
// off, so the no_std configuration can be exercised by the full suite.
#[cfg(test)]
extern crate std;

#[allow(dead_code)]
mod bitstream;
mod cabac;
mod cabac_tables;
mod cavlc;
mod chroma;
#[allow(dead_code)]
mod deblock;
mod decode_cabac;
mod decode_cavlc;
pub mod decoder;
#[allow(dead_code)]
mod dpb;
pub mod error;
mod inter_pred;
mod intra_pred;
mod mv_pred;
pub mod nal;
mod neighbor;
mod once;
mod picture_sync;
#[cfg(feature = "dev-internals")]
#[allow(dead_code)]
pub mod pps;
#[cfg(not(feature = "dev-internals"))]
#[allow(dead_code)]
mod pps;
#[cfg(feature = "profile")]
pub mod profile;
// SHA-256 is used by the golden-digest tests and the `bench_decode` example;
// it is not part of the decoder API.
#[cfg(feature = "dev-internals")]
pub mod sha256;
#[cfg(all(test, not(feature = "dev-internals")))]
mod sha256;

#[allow(dead_code)]
mod residual;
#[allow(dead_code)]
mod sei;
mod simd;
#[cfg(feature = "std")]
pub mod threading;

#[cfg(feature = "dev-internals")]
#[allow(dead_code)]
pub mod slice;
#[cfg(not(feature = "dev-internals"))]
#[allow(dead_code)]
mod slice;

mod slice_context;

#[cfg(feature = "dev-internals")]
#[allow(dead_code)]
pub mod sps;
#[cfg(not(feature = "dev-internals"))]
#[allow(dead_code)]
mod sps;
