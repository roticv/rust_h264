#![no_main]

//! `ThreadedDecoder` fuzz: the slice-data inputs of `decode_slice_data`,
//! decoded on worker threads. Covers the coordinator (slice planning, DPB
//! insertion of in-progress pictures, commit order, reorder buffer) and the
//! row-level reference waits, which must not panic or deadlock on damage.

use libfuzzer_sys::fuzz_target;
use rust_h264::threading::ThreadedDecoder;

#[path = "../common/slice_mutation.rs"]
mod slice_mutation;

fuzz_target!(|data: &[u8]| {
    let mut decoder = ThreadedDecoder::new(3);
    for nal in &slice_mutation::mutated_nals(data) {
        let _ = decoder.decode_nal(nal);
    }
    let _ = decoder.flush();
});
