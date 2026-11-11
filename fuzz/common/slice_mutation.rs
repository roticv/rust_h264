//! Structured input for the slice-data fuzz targets.
//!
//! Byte-level mutations of a whole stream mostly break the SPS, PPS or slice
//! header, which the decoder rejects early, so the macroblock layer is rarely
//! reached. These targets start from a real test stream instead and apply
//! small edits to its slice NALs only, so the parameter sets stay valid and
//! most slices still parse their headers before hitting the damage.
//!
//! Input layout:
//! - byte 0: which base stream (modulo the number of streams);
//! - then 4-byte edits `[slice, offset_lo, offset_hi, value]`: `slice`
//!   (low 7 bits, modulo the slice count) picks a slice NAL, and the offset
//!   is taken modulo that slice's RBSP length. With the top bit of `slice`
//!   clear, the byte at the offset is XORed with `value`; with it set, the
//!   slice is truncated at the offset.

use rust_h264::nal::{parse_annex_b, NalUnit, NalUnitType};

/// Small streams covering CAVLC and CABAC, P and B slices, MBAFF and field
/// pictures, I_PCM, weighted prediction, long-term references, 10-bit, 4:2:2
/// and monochrome.
const STREAMS: &[&[u8]] = &[
    include_bytes!("../../testdata/ms_cavlc_p_test.h264"),
    include_bytes!("../../testdata/p_8x8_test.h264"),
    include_bytes!("../../testdata/b_l0_l1_test.h264"),
    include_bytes!("../../testdata/mbaff_cavlc_b_test.h264"),
    include_bytes!("../../testdata/jm_ipcm_cavlc_test.h264"),
    include_bytes!("../../testdata/jm_field_p_test.h264"),
    include_bytes!("../../testdata/weighted_p_test.h264"),
    include_bytes!("../../testdata/mono_x264_cavlc.h264"),
    include_bytes!("../../testdata/cabac_b_test.h264"),
    include_bytes!("../../testdata/cabac_multiref_test.h264"),
    include_bytes!("../../testdata/cabac_high_test.h264"),
    include_bytes!("../../testdata/mbaff_cabac_b_test.h264"),
    include_bytes!("../../testdata/jm_field_cabac_test.h264"),
    include_bytes!("../../testdata/jm_ipcm_cabac_test.h264"),
    include_bytes!("../../testdata/jm_weighted_b_explicit_test.h264"),
    include_bytes!("../../testdata/ms_cabac_b_test.h264"),
    include_bytes!("../../testdata/jm_ltr_cabac_test.h264"),
    include_bytes!("../../testdata/mono_x264_cabac_mbaff.h264"),
    include_bytes!("../../testdata/high10_420_32x32.h264"),
    include_bytes!("../../testdata/high10_x264_cabac_mbaff.h264"),
    include_bytes!("../../testdata/yuv422_x264_cabac.h264"),
    include_bytes!("../../testdata/high10_yuv422_x264_cabac.h264"),
];

/// Build the NAL units of the base stream picked by `data`, with its edits
/// applied to the slice NALs.
pub fn mutated_nals(data: &[u8]) -> Vec<NalUnit<'static>> {
    let Some((&pick, edits)) = data.split_first() else {
        return Vec::new();
    };
    let mut nals = parse_annex_b(STREAMS[pick as usize % STREAMS.len()]);
    let slices: Vec<usize> = (0..nals.len())
        .filter(|&i| {
            matches!(
                nals[i].nal_unit_type,
                NalUnitType::Slice | NalUnitType::SliceIdr
            )
        })
        .collect();
    if slices.is_empty() {
        return nals;
    }
    for edit in edits.chunks_exact(4) {
        let nal = &mut nals[slices[(edit[0] & 0x7f) as usize % slices.len()]];
        let len = nal.rbsp.len();
        if len == 0 {
            continue;
        }
        let offset = u16::from_le_bytes([edit[1], edit[2]]) as usize % len;
        let rbsp = nal.rbsp.to_mut();
        if edit[0] & 0x80 != 0 {
            rbsp.truncate(offset);
        } else {
            rbsp[offset] ^= edit[3];
        }
    }
    nals
}
