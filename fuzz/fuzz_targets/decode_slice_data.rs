#![no_main]

//! Slice-data fuzz: damage the slice NALs of a real test stream (see
//! `common/slice_mutation.rs`) and decode it with `OrderedDecoder`, so the
//! macroblock layer (CAVLC, CABAC, prediction, reconstruction) sees damaged
//! data behind valid parameter sets and headers. Must never panic or hang.

use libfuzzer_sys::fuzz_target;
use rust_h264::decoder::OrderedDecoder;

#[path = "../common/slice_mutation.rs"]
mod slice_mutation;

fuzz_target!(|data: &[u8]| {
    let mut decoder = OrderedDecoder::new();
    for nal in &slice_mutation::mutated_nals(data) {
        let _ = decoder.decode_nal(nal);
    }
    let _ = decoder.flush();
});
