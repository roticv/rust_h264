//! H.264 deblocking filter (spec section 8.7).
//!
//! Applied after all macroblocks in a slice are decoded. For each MB in raster
//! order, vertical edges are filtered left-to-right, then horizontal edges
//! top-to-bottom. Each edge consists of 4-sample segments filtered independently.

use crate::decoder::Frame;
use crate::residual::chroma_qp;
use crate::slice::SliceHeader;

/// Macroblock type for boundary strength derivation.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum MbType {
    #[default]
    Intra,
    Ipcm,
    Inter,
}

/// Per-macroblock metadata needed by the deblocking filter.
/// One slice's deblocking settings from its header. Each MB's edges are
/// filtered as its own slice says (spec 8.7).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SliceDeblock {
    pub disable_deblocking_filter_idc: u32,
    pub slice_alpha_c0_offset_div2: i32,
    pub slice_beta_offset_div2: i32,
}

#[derive(Clone, Default)]
pub struct MbInfo {
    pub mb_type: MbType,
    pub qp_y: i32,
    /// Per-4x4-block motion vectors for L0 (quarter-pel). 16 entries in raster scan.
    pub mv_l0: [[i16; 2]; 16],
    /// Per-4x4-block motion vectors for L1 (quarter-pel). 16 entries in raster scan.
    pub mv_l1: [[i16; 2]; 16],
    /// Per-4x4-block reference indices for L0. -1 = unused.
    pub ref_idx_l0: [i8; 16],
    /// Per-4x4-block reference indices for L1. -1 = unused.
    pub ref_idx_l1: [i8; 16],
    /// Per-4x4-block reference picture POC for L0. Used for cross-list comparison.
    pub ref_poc_l0: [i32; 16],
    /// Per-4x4-block reference picture POC for L1.
    pub ref_poc_l1: [i32; 16],
    /// Per-4x4-block non-zero coefficient count. True if any coefficients were coded.
    pub nnz: [bool; 16],
    /// Number of reference lists used (1 for P-slice, 2 for B-slice, 0 for I-slice).
    pub list_count: u8,
    /// True if this MB uses 8x8 transform (High profile transform_size_8x8_flag).
    pub is_8x8dct: bool,
}

// H.264 Table 8-16a: alpha threshold indexed by indexA = clamp(0..51, QPavg + offset_a)
#[rustfmt::skip]
const ALPHA_TABLE: [i32; 52] = [
     0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,
     4,  4,  5,  6,  7,  8,  9, 10, 12, 13, 15, 17, 20, 22, 25, 28,
    32, 36, 40, 45, 50, 56, 63, 71, 80, 90,101,113,127,144,162,182,
   203,226,255,255,
];

// H.264 Table 8-16b: beta threshold
#[rustfmt::skip]
const BETA_TABLE: [i32; 52] = [
     0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,
     2,  2,  2,  3,  3,  3,  3,  4,  4,  4,  6,  6,  7,  7,  8,  8,
     9,  9, 10, 10, 11, 11, 12, 12, 13, 13, 14, 14, 15, 15, 16, 16,
    17, 17, 18, 18,
];

// H.264 Table 8-16c: tc0 indexed by [indexA][bS-1] for bS=1..3
#[rustfmt::skip]
const TC0_TABLE: [[i32; 3]; 52] = [
    [ 0, 0, 0], [ 0, 0, 0], [ 0, 0, 0], [ 0, 0, 0], [ 0, 0, 0], [ 0, 0, 0],
    [ 0, 0, 0], [ 0, 0, 0], [ 0, 0, 0], [ 0, 0, 0], [ 0, 0, 0], [ 0, 0, 0],
    [ 0, 0, 0], [ 0, 0, 0], [ 0, 0, 0], [ 0, 0, 0], [ 0, 0, 0], [ 0, 0, 1],
    [ 0, 0, 1], [ 0, 0, 1], [ 0, 0, 1], [ 0, 1, 1], [ 0, 1, 1], [ 1, 1, 1],
    [ 1, 1, 1], [ 1, 1, 1], [ 1, 1, 1], [ 1, 1, 2], [ 1, 1, 2], [ 1, 1, 2],
    [ 1, 1, 2], [ 1, 2, 3], [ 1, 2, 3], [ 2, 2, 3], [ 2, 2, 4], [ 2, 3, 4],
    [ 2, 3, 4], [ 3, 3, 5], [ 3, 4, 6], [ 3, 4, 6], [ 4, 5, 7], [ 4, 5, 8],
    [ 4, 6, 9], [ 5, 7,10], [ 6, 8,11], [ 6, 8,13], [ 7,10,14], [ 8,11,16],
    [ 9,12,18], [10,13,20], [11,15,23], [13,17,25],
];

/// Apply the deblocking filter to a fully decoded frame.
pub fn filter_frame(
    frame: &mut Frame,
    mb_info: &[MbInfo],
    mb_width: usize,
    _mb_height: usize,
    header: &SliceHeader,
    chroma_qp_index_offset: i32,
) {
    filter_frame_params(
        frame,
        mb_info,
        mb_width,
        header.disable_deblocking_filter_idc,
        header.slice_alpha_c0_offset_div2,
        header.slice_beta_offset_div2,
        chroma_qp_index_offset,
    );
}

/// Apply deblocking with explicit parameters (no SliceHeader needed).
#[allow(clippy::needless_range_loop)]
pub fn filter_frame_params(
    frame: &mut Frame,
    mb_info: &[MbInfo],
    mb_width: usize,
    disable_deblocking_filter_idc: u32,
    slice_alpha_c0_offset_div2: i32,
    slice_beta_offset_div2: i32,
    chroma_qp_index_offset: i32,
) {
    filter_frame_inner(
        frame,
        mb_info,
        mb_width,
        &vec![0; mb_info.len()],
        &[SliceDeblock {
            disable_deblocking_filter_idc,
            slice_alpha_c0_offset_div2,
            slice_beta_offset_div2,
        }],
        chroma_qp_index_offset,
        chroma_qp_index_offset,
        Interlace::default(),
        0..mb_info.len(),
    );
}

/// Apply deblocking with MBAFF and field picture support. `mb_slice_id`
/// gives each MB's slice, an index into `slices`; Cr uses the second chroma
/// QP offset.
#[allow(clippy::too_many_arguments)]
pub(crate) fn filter_frame_mbaff(
    frame: &mut Frame,
    mb_info: &[MbInfo],
    mb_width: usize,
    mb_slice_id: &[u16],
    slices: &[SliceDeblock],
    chroma_qp_index_offset: i32,
    second_chroma_qp_index_offset: i32,
    interlace: Interlace,
) {
    filter_frame_inner(
        frame,
        mb_info,
        mb_width,
        mb_slice_id,
        slices,
        chroma_qp_index_offset,
        second_chroma_qp_index_offset,
        interlace,
        0..mb_info.len(),
    );
}

/// Deblock only macroblocks `mbs` (in decoding order). Filtering is a single
/// raster-order pass where each MB filters its left and top edges, so
/// calling this on consecutive ranges gives exactly the result of
/// [`filter_frame_mbaff`], provided each range's MBs and their left/top
/// neighbours are final when it runs.
#[allow(clippy::too_many_arguments)]
pub(crate) fn filter_mb_range(
    frame: &mut Frame,
    mb_info: &[MbInfo],
    mb_width: usize,
    mb_slice_id: &[u16],
    slices: &[SliceDeblock],
    chroma_qp_index_offset: i32,
    second_chroma_qp_index_offset: i32,
    interlace: Interlace,
    mbs: core::ops::Range<usize>,
) {
    filter_frame_inner(
        frame,
        mb_info,
        mb_width,
        mb_slice_id,
        slices,
        chroma_qp_index_offset,
        second_chroma_qp_index_offset,
        interlace,
        mbs,
    );
}

#[allow(clippy::needless_range_loop, clippy::too_many_arguments)]
fn filter_frame_inner(
    frame: &mut Frame,
    mb_info: &[MbInfo],
    mb_width: usize,
    mb_slice_id: &[u16],
    slices: &[SliceDeblock],
    chroma_qp_index_offset: i32,
    second_chroma_qp_index_offset: i32,
    interlace: Interlace,
    mbs: core::ops::Range<usize>,
) {
    if let Some(mb_field) = interlace.mbaff_field {
        for m in mbs.start..mbs.end.min(mb_info.len()) {
            filter_mb_mbaff(
                frame,
                mb_info,
                mb_width,
                mb_slice_id,
                slices,
                [chroma_qp_index_offset, second_chroma_qp_index_offset],
                mb_field,
                m,
            );
        }
        return;
    }
    // Field pictures: intra horizontal MB edges get bS 3 and vertical MV
    // differences are in field units (spec 8.7.2.1)
    let mvy_limit = if interlace.field_pic { 2 } else { 4 };
    // The frame is still at its coded size here (cropping happens on output),
    // so a row is mb_width MBs long whatever frame.width says
    let stride_y = mb_width * 16;
    let stride_c = mb_width * 8;

    // 4x4 block index layout within an MB (raster scan):
    //  0  1  4  5
    //  2  3  6  7
    //  8  9 12 13
    // 10 11 14 15
    // For vertical edges: blocks to the left of edge column
    // For horizontal edges: blocks above edge row

    for mb_idx in mbs.start..mbs.end.min(mb_info.len()) {
        // Compute spatial position and column/row for neighbor checks
        let (mb_col, mb_row) = (mb_idx % mb_width, mb_idx / mb_width);
        let (mb_x, mb_y) = (mb_col * 16, mb_row * 16);
        let mb_q = &mb_info[mb_idx];

        // This MB's edges are filtered as its own slice's header says
        let slice_id = mb_slice_id.get(mb_idx).copied().unwrap_or(0);
        let slice = slices
            .get(usize::from(slice_id))
            .or(slices.last())
            .copied()
            .unwrap_or_default();
        if slice.disable_deblocking_filter_idc == 1 {
            continue;
        }
        let filter_offset_a = slice.slice_alpha_c0_offset_div2.wrapping_mul(2);
        let filter_offset_b = slice.slice_beta_offset_div2.wrapping_mul(2);
        // With 2, edges shared with an MB of another slice aren't filtered
        let other_slice = |neighbour: usize| {
            slice.disable_deblocking_filter_idc == 2
                && mb_slice_id.get(neighbour).copied().unwrap_or(slice_id) != slice_id
        };

        // -- Vertical edges (left to right) --
        for edge in 0..4 {
            let edge_x = mb_x + edge * 4;
            if edge == 0 && mb_col == 0 {
                continue;
            }

            let is_mb_edge = edge == 0;

            // 8x8 transform: skip internal odd edges (spec 8.7.2.1).
            // Edges 1 and 3 fall inside 8x8 transform blocks, so no filtering.
            if !is_mb_edge && (edge & 1) != 0 && mb_q.is_8x8dct {
                continue;
            }

            // Left neighbor MB
            let left_mb_idx = if is_mb_edge { mb_idx - 1 } else { mb_idx };
            if is_mb_edge && other_slice(left_mb_idx) {
                continue;
            }
            let mb_p = &mb_info[left_mb_idx];

            let qp_q = mb_q.qp_y;
            let qp_p = mb_p.qp_y;
            let qp_avg = (qp_p.wrapping_add(qp_q).wrapping_add(1)) >> 1;
            let index_a = (qp_avg + filter_offset_a).clamp(0, 51) as usize;
            let index_b = (qp_avg + filter_offset_b).clamp(0, 51) as usize;
            let alpha = ALPHA_TABLE[index_a];
            let beta = BETA_TABLE[index_b];

            // Chroma QP (computed once per edge, used for chroma segments).
            // Cb and Cr each take their own QP offset.
            let chroma_params = |offset: i32| {
                let qp_c_avg = (chroma_qp(qp_p, offset) + chroma_qp(qp_q, offset) + 1) >> 1;
                let c_index_a = (qp_c_avg + filter_offset_a).clamp(0, 51) as usize;
                let c_index_b = (qp_c_avg + filter_offset_b).clamp(0, 51) as usize;
                (c_index_a, ALPHA_TABLE[c_index_a], BETA_TABLE[c_index_b])
            };
            let cb = chroma_params(chroma_qp_index_offset);
            let cr = chroma_params(second_chroma_qp_index_offset);

            // Compute bS for all 4 segments first
            let mut seg_bs = [0i32; 4];
            for seg in 0..4 {
                let blk_q = blk_idx(edge, seg);
                let blk_p = if is_mb_edge {
                    blk_idx(3, seg)
                } else {
                    blk_idx(edge - 1, seg)
                };
                let intra_bs = if is_mb_edge { 4 } else { 3 };
                seg_bs[seg] = derive_bs(mb_p, mb_q, blk_p, blk_q, intra_bs, mvy_limit);
            }
            // Luma: filter each segment independently
            for seg in 0..4 {
                if seg_bs[seg] == 0 {
                    continue;
                }
                let tc0 = if seg_bs[seg] < 4 {
                    TC0_TABLE[index_a][(seg_bs[seg] - 1) as usize]
                } else {
                    0
                };
                let y = mb_y + seg * 4;
                filter_edge_v(
                    &mut frame.y,
                    stride_y,
                    edge_x,
                    y,
                    4,
                    seg_bs[seg],
                    alpha,
                    beta,
                    tc0,
                );
            }

            // Chroma: per-pixel bS from all 4 luma segments
            if edge % 2 == 0 {
                let c_edge_x = mb_col * 8 + (edge / 2) * 4;
                for cseg in 0..2 {
                    let cy = mb_y / 2 + cseg * 4;
                    let c_bs = [
                        seg_bs[cseg * 2],
                        seg_bs[cseg * 2],
                        seg_bs[cseg * 2 + 1],
                        seg_bs[cseg * 2 + 1],
                    ];
                    for (plane, (c_index_a, c_alpha, c_beta)) in
                        [(&mut frame.u, cb), (&mut frame.v, cr)]
                    {
                        for i in 0..4 {
                            let pbs = c_bs[i];
                            if pbs == 0 {
                                continue;
                            }
                            let ptc0 = if pbs < 4 {
                                TC0_TABLE[c_index_a][(pbs - 1) as usize]
                            } else {
                                0
                            };
                            filter_edge_v_chroma(
                                plane,
                                stride_c,
                                c_edge_x,
                                cy + i,
                                1,
                                pbs,
                                c_alpha,
                                c_beta,
                                ptc0,
                            );
                        }
                    }
                }
            }
        }

        // -- Horizontal edges (top to bottom) --
        for edge in 0..4 {
            let edge_y = mb_y + edge * 4;

            let is_mb_edge = edge == 0;

            // Check if there's an above neighbor
            if is_mb_edge && mb_row == 0 {
                continue;
            }

            // 8x8 transform: skip internal odd edges (spec 8.7.2.1).
            if !is_mb_edge && (edge & 1) != 0 && mb_q.is_8x8dct {
                continue;
            }

            // Above neighbor MB
            let above_mb_idx = if is_mb_edge {
                mb_idx - mb_width
            } else {
                mb_idx
            };
            if is_mb_edge && other_slice(above_mb_idx) {
                continue;
            }
            let mb_p = &mb_info[above_mb_idx];

            let qp_q = mb_q.qp_y;
            let qp_p = mb_p.qp_y;
            let qp_avg = (qp_p.wrapping_add(qp_q).wrapping_add(1)) >> 1;
            let index_a = (qp_avg + filter_offset_a).clamp(0, 51) as usize;
            let index_b = (qp_avg + filter_offset_b).clamp(0, 51) as usize;
            let alpha = ALPHA_TABLE[index_a];
            let beta = BETA_TABLE[index_b];

            // Cb and Cr each take their own QP offset
            let chroma_params = |offset: i32| {
                let qp_c_avg = (chroma_qp(qp_p, offset) + chroma_qp(qp_q, offset) + 1) >> 1;
                let c_index_a = (qp_c_avg + filter_offset_a).clamp(0, 51) as usize;
                let c_index_b = (qp_c_avg + filter_offset_b).clamp(0, 51) as usize;
                (c_index_a, ALPHA_TABLE[c_index_a], BETA_TABLE[c_index_b])
            };
            let cb = chroma_params(chroma_qp_index_offset);
            let cr = chroma_params(second_chroma_qp_index_offset);

            // Compute bS for all 4 segments first
            let mut seg_bs = [0i32; 4];
            for seg in 0..4 {
                let blk_q = blk_idx(seg, edge);
                let blk_p = if is_mb_edge {
                    blk_idx(seg, 3)
                } else {
                    blk_idx(seg, edge - 1)
                };
                let intra_bs = if is_mb_edge && !interlace.field_pic {
                    4
                } else {
                    3
                };
                seg_bs[seg] = derive_bs(mb_p, mb_q, blk_p, blk_q, intra_bs, mvy_limit);
            }

            // Luma: filter each segment independently
            for seg in 0..4 {
                if seg_bs[seg] == 0 {
                    continue;
                }
                let tc0 = if seg_bs[seg] < 4 {
                    TC0_TABLE[index_a][(seg_bs[seg] - 1) as usize]
                } else {
                    0
                };
                let x = mb_x + seg * 4;
                filter_edge_h(
                    &mut frame.y,
                    stride_y,
                    x,
                    edge_y,
                    4,
                    seg_bs[seg],
                    alpha,
                    beta,
                    tc0,
                );
            }

            // Chroma: per-pixel bS from all 4 luma segments
            if edge % 2 == 0 {
                let c_edge_y = mb_y / 2 + (edge / 2) * 4;
                for cseg in 0..2 {
                    let cx = mb_x / 2 + cseg * 4;
                    let c_bs = [
                        seg_bs[cseg * 2],
                        seg_bs[cseg * 2],
                        seg_bs[cseg * 2 + 1],
                        seg_bs[cseg * 2 + 1],
                    ];
                    for (plane, (c_index_a, c_alpha, c_beta)) in
                        [(&mut frame.u, cb), (&mut frame.v, cr)]
                    {
                        for i in 0..4 {
                            let pbs = c_bs[i];
                            if pbs == 0 {
                                continue;
                            }
                            let ptc0 = if pbs < 4 {
                                TC0_TABLE[c_index_a][(pbs - 1) as usize]
                            } else {
                                0
                            };
                            filter_edge_h_chroma(
                                plane,
                                stride_c,
                                cx + i,
                                c_edge_y,
                                1,
                                pbs,
                                c_alpha,
                                c_beta,
                                ptc0,
                            );
                        }
                    }
                }
            }
        }
    }
}

/// Interlaced structure of the picture being deblocked.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Interlace<'a> {
    /// MBAFF frame: `mb_field_decoding_flag` of each macroblock pair.
    pub(crate) mbaff_field: Option<&'a [bool]>,
    /// Field picture: horizontal MB edges next to intra MBs get bS 3 and
    /// vertical MV differences count from 2 (field units).
    pub(crate) field_pic: bool,
}

#[inline]
fn is_intra(mb: &MbInfo) -> bool {
    mb.mb_type == MbType::Intra || mb.mb_type == MbType::Ipcm
}

/// Sample layout of one macroblock: rows `y0 + r` at line step `ls`
/// starting at column offset `x0` (the line step and offset place an MBAFF
/// field MB on every other frame line), and likewise for chroma.
#[derive(Clone, Copy)]
struct MbGeom {
    x0: usize,
    y0: usize,
    ls: usize,
    cx0: usize,
    cy0: usize,
    cls: usize,
}

impl MbGeom {
    fn mbaff(mb_idx: usize, mb_width: usize, field: bool) -> Self {
        let (stride_y, stride_c) = (mb_width * 16, mb_width * 8);
        let pair = mb_idx / 2;
        let (col, prow, bottom) = (pair % mb_width, pair / mb_width, mb_idx % 2);
        if field {
            MbGeom {
                x0: bottom * stride_y + col * 16,
                y0: prow * 16,
                ls: 2 * stride_y,
                cx0: bottom * stride_c + col * 8,
                cy0: prow * 8,
                cls: 2 * stride_c,
            }
        } else {
            MbGeom {
                x0: col * 16,
                y0: prow * 32 + 16 * bottom,
                ls: stride_y,
                cx0: col * 8,
                cy0: prow * 16 + 8 * bottom,
                cls: stride_c,
            }
        }
    }
}

/// Edge filter parameters for luma and both chroma planes.
#[derive(Clone, Copy)]
struct EdgeQp {
    luma: (usize, i32, i32),
    chroma: [(usize, i32, i32); 2],
}

impl EdgeQp {
    fn new(qp_p: i32, qp_q: i32, off_a: i32, off_b: i32, cqp_offset: [i32; 2]) -> Self {
        let params = |qp: i32| {
            let index_a = (qp + off_a).clamp(0, 51) as usize;
            let index_b = (qp + off_b).clamp(0, 51) as usize;
            (index_a, ALPHA_TABLE[index_a], BETA_TABLE[index_b])
        };
        let chroma = cqp_offset.map(|o| params((chroma_qp(qp_p, o) + chroma_qp(qp_q, o) + 1) >> 1));
        EdgeQp {
            luma: params((qp_p.wrapping_add(qp_q).wrapping_add(1)) >> 1),
            chroma,
        }
    }
}

#[inline]
fn tc0_for(index_a: usize, bs: i32) -> i32 {
    if bs < 4 {
        TC0_TABLE[index_a][(bs - 1) as usize]
    } else {
        0
    }
}

/// Filter the vertical edge at luma column `ex` (chroma `ex / 2` when even)
/// of an MB with geometry `g`; `bs[k]` covers luma rows `4k..4k + 4`.
fn filter_v_edge(frame: &mut Frame, g: MbGeom, ex: usize, bs: [i32; 4], qp: EdgeQp) {
    let (index_a, alpha, beta) = qp.luma;
    for (seg, &b) in bs.iter().enumerate() {
        if b != 0 {
            let tc0 = tc0_for(index_a, b);
            filter_edge_v(
                &mut frame.y,
                g.ls,
                g.x0 + ex,
                g.y0 + seg * 4,
                4,
                b,
                alpha,
                beta,
                tc0,
            );
        }
    }
    if ex.is_multiple_of(8) {
        for (plane, (c_index_a, c_alpha, c_beta)) in
            [(&mut frame.u, qp.chroma[0]), (&mut frame.v, qp.chroma[1])]
        {
            for cy in 0..8 {
                let b = bs[cy / 2];
                if b != 0 {
                    let tc0 = tc0_for(c_index_a, b);
                    filter_edge_v_chroma(
                        plane,
                        g.cls,
                        g.cx0 + ex / 2,
                        g.cy0 + cy,
                        1,
                        b,
                        c_alpha,
                        c_beta,
                        tc0,
                    );
                }
            }
        }
    }
}

/// Filter the horizontal edge at luma row `ey` of an MB with geometry `g`;
/// `bs[k]` covers luma columns `4k..4k + 4`.
fn filter_h_edge(frame: &mut Frame, g: MbGeom, ey: usize, bs: [i32; 4], qp: EdgeQp) {
    let (index_a, alpha, beta) = qp.luma;
    for (seg, &b) in bs.iter().enumerate() {
        if b != 0 {
            let tc0 = tc0_for(index_a, b);
            filter_edge_h(
                &mut frame.y,
                g.ls,
                g.x0 + seg * 4,
                g.y0 + ey,
                4,
                b,
                alpha,
                beta,
                tc0,
            );
        }
    }
    if ey.is_multiple_of(8) {
        for (plane, (c_index_a, c_alpha, c_beta)) in
            [(&mut frame.u, qp.chroma[0]), (&mut frame.v, qp.chroma[1])]
        {
            for cx in 0..8 {
                let b = bs[cx / 2];
                if b != 0 {
                    let tc0 = tc0_for(c_index_a, b);
                    filter_edge_h_chroma(
                        plane,
                        g.cls,
                        g.cx0 + cx,
                        g.cy0 + ey / 2,
                        1,
                        b,
                        c_alpha,
                        c_beta,
                        tc0,
                    );
                }
            }
        }
    }
}

/// bS of the four segments of an edge between `p` and `q` (blocks `blk_p`
/// / `blk_q` per segment). `intra_bs` is used when either MB is intra;
/// `mixed` marks a frame/field MB edge, which gets at least 1.
#[allow(clippy::too_many_arguments)]
fn edge_bs(
    p: &MbInfo,
    q: &MbInfo,
    blk_p: impl Fn(usize) -> usize,
    blk_q: impl Fn(usize) -> usize,
    intra_bs: i32,
    mixed: bool,
    mvy_limit: i32,
) -> [i32; 4] {
    if is_intra(p) || is_intra(q) {
        return [intra_bs; 4];
    }
    core::array::from_fn(|seg| {
        let (bp, bq) = (blk_p(seg), blk_q(seg));
        if p.nnz[bp] || q.nnz[bq] {
            2
        } else if mixed || check_mv_diff(p, q, bp, bq, mvy_limit) {
            1
        } else {
            0
        }
    })
}

/// Deblock macroblock `m` of an MBAFF frame (spec 8.7 with MbaffFrameFlag,
/// following FFmpeg's `ff_h264_filter_mb`). Field MBs are filtered on their
/// own field lines; edges between a frame and a field pair use the sample
/// geometry of spec Table 6-4:
/// - a left edge to a pair of the other kind takes, per row, the left MB and
///   QP that hold the neighbouring sample (bS 4 next to intra, else 1 or 2);
/// - the top edge of a frame MB below a field pair is filtered twice, once
///   against each field of that pair;
/// - the top edge of a field MB uses the same-parity lines of the pair above.
#[allow(clippy::too_many_arguments)]
fn filter_mb_mbaff(
    frame: &mut Frame,
    mb_info: &[MbInfo],
    mb_width: usize,
    mb_slice_id: &[u16],
    slices: &[SliceDeblock],
    cqp_offset: [i32; 2],
    mb_field: &[bool],
    m: usize,
) {
    let pair = m / 2;
    let (col, prow, bottom) = (pair % mb_width, pair / mb_width, m % 2 == 1);
    let Some(&field) = mb_field.get(pair) else {
        return;
    };
    let g = MbGeom::mbaff(m, mb_width, field);
    let q = &mb_info[m];
    let slice_id = mb_slice_id.get(m).copied().unwrap_or(0);
    let slice = slices
        .get(usize::from(slice_id))
        .or(slices.last())
        .copied()
        .unwrap_or_default();
    if slice.disable_deblocking_filter_idc == 1 {
        return;
    }
    let off_a = slice.slice_alpha_c0_offset_div2.wrapping_mul(2);
    let off_b = slice.slice_beta_offset_div2.wrapping_mul(2);
    let other_slice = |n: usize| {
        slice.disable_deblocking_filter_idc == 2
            && mb_slice_id.get(n).copied().unwrap_or(slice_id) != slice_id
    };
    let mvy_limit = if field { 2 } else { 4 };
    let pair_field = |mb: usize| mb_field.get(mb / 2).copied().unwrap_or(false);

    // -- Left MB edge --
    if col > 0 && !other_slice((pair - 1) * 2) {
        let lp = pair - 1;
        if pair_field(lp * 2) == field {
            let p = &mb_info[lp * 2 + bottom as usize];
            let bs = edge_bs(
                p,
                q,
                |s| blk_idx(3, s),
                |s| blk_idx(0, s),
                4,
                false,
                mvy_limit,
            );
            filter_v_edge(
                frame,
                g,
                0,
                bs,
                EdgeQp::new(p.qp_y, q.qp_y, off_a, off_b, cqp_offset),
            );
        } else {
            filter_mixed_left_edge(
                frame, mb_info, g, q, lp, field, bottom, off_a, off_b, cqp_offset,
            );
        }
    }
    // -- Internal vertical edges --
    for edge in 1..4 {
        if (edge & 1) != 0 && q.is_8x8dct {
            continue;
        }
        let bs = edge_bs(
            q,
            q,
            |s| blk_idx(edge - 1, s),
            |s| blk_idx(edge, s),
            3,
            false,
            mvy_limit,
        );
        filter_v_edge(
            frame,
            g,
            edge * 4,
            bs,
            EdgeQp::new(q.qp_y, q.qp_y, off_a, off_b, cqp_offset),
        );
    }

    // -- Top MB edge --
    let above = if !field && bottom {
        Some(m - 1)
    } else if prow > 0 {
        let ap = pair - mb_width;
        Some(if field && !bottom && pair_field(ap * 2) {
            ap * 2
        } else {
            ap * 2 + 1
        })
    } else {
        None
    };
    if let Some(pm) = above.filter(|&pm| !other_slice(pm)) {
        if !field && !bottom && pair_field(pm) {
            // Frame MB below a field pair: filter against each field
            let ap = pm / 2;
            for j in 0..2 {
                let p = &mb_info[ap * 2 + j];
                let bs = if is_intra(p) || is_intra(q) {
                    [3; 4]
                } else {
                    core::array::from_fn(|s| {
                        1 + (q.nnz[blk_idx(s, 0)] || p.nnz[blk_idx(s, 3)]) as i32
                    })
                };
                let gj = MbGeom {
                    x0: j * g.ls + g.x0,
                    y0: g.y0 / 2,
                    ls: 2 * g.ls,
                    cx0: j * g.cls + g.cx0,
                    cy0: g.cy0 / 2,
                    cls: 2 * g.cls,
                };
                filter_h_edge(
                    frame,
                    gj,
                    0,
                    bs,
                    EdgeQp::new(p.qp_y, q.qp_y, off_a, off_b, cqp_offset),
                );
            }
        } else {
            let p = &mb_info[pm];
            let p_field = pair_field(pm);
            let intra_bs = if field || p_field { 3 } else { 4 };
            let bs = edge_bs(
                p,
                q,
                |s| blk_idx(s, 3),
                |s| blk_idx(s, 0),
                intra_bs,
                p_field != field,
                mvy_limit,
            );
            filter_h_edge(
                frame,
                g,
                0,
                bs,
                EdgeQp::new(p.qp_y, q.qp_y, off_a, off_b, cqp_offset),
            );
        }
    }
    // -- Internal horizontal edges --
    for edge in 1..4 {
        if (edge & 1) != 0 && q.is_8x8dct {
            continue;
        }
        let bs = edge_bs(
            q,
            q,
            |s| blk_idx(s, edge - 1),
            |s| blk_idx(s, edge),
            3,
            false,
            mvy_limit,
        );
        filter_h_edge(
            frame,
            g,
            edge * 4,
            bs,
            EdgeQp::new(q.qp_y, q.qp_y, off_a, off_b, cqp_offset),
        );
    }
}

/// Left MB edge of `q` next to left pair `lp` coded the other way (frame vs
/// field). Each of the 16 rows of `q` borders the left MB holding the
/// sample beside it: for a field `q` the left frame pair's top MB for rows
/// 0-7 and bottom MB for rows 8-15, for a frame `q` the left field pair's
/// MB of the row's parity. bS is 4 next to an intra MB, else 1, or 2 with
/// coefficients on either side; the QP is that of the row's left MB.
#[allow(clippy::too_many_arguments)]
fn filter_mixed_left_edge(
    frame: &mut Frame,
    mb_info: &[MbInfo],
    g: MbGeom,
    q: &MbInfo,
    lp: usize,
    field: bool,
    bottom: bool,
    off_a: i32,
    off_b: i32,
    cqp_offset: [i32; 2],
) {
    let left = [&mb_info[lp * 2], &mb_info[lp * 2 + 1]];
    // bS per pair of rows (index i), as FFmpeg orders them
    let bs8: [i32; 8] = core::array::from_fn(|i| {
        let l = left[if field { i >> 2 } else { i & 1 }];
        if is_intra(q) || is_intra(l) {
            return 4;
        }
        let l_row = if field {
            i & 3
        } else {
            (i >> 2) + if bottom { 2 } else { 0 }
        };
        1 + (q.nnz[blk_idx(0, i >> 1)] || l.nnz[blk_idx(3, l_row)]) as i32
    });
    let qps = left.map(|l| EdgeQp::new(l.qp_y, q.qp_y, off_a, off_b, cqp_offset));
    for r in 0..16 {
        let (i, j) = if field {
            (r >> 1, r >> 3)
        } else {
            (2 * (r >> 2) + (r & 1), r & 1)
        };
        let (index_a, alpha, beta) = qps[j].luma;
        let b = bs8[i];
        filter_edge_v(
            &mut frame.y,
            g.ls,
            g.x0,
            g.y0 + r,
            1,
            b,
            alpha,
            beta,
            tc0_for(index_a, b),
        );
    }
    for (c, plane) in [&mut frame.u, &mut frame.v].into_iter().enumerate() {
        // Chroma row cy takes the bS of row pair cy (as FFmpeg)
        for (cy, &b) in bs8.iter().enumerate() {
            let j = if field { cy >> 2 } else { cy & 1 };
            let (c_index_a, c_alpha, c_beta) = qps[j].chroma[c];
            filter_edge_v_chroma(
                plane,
                g.cls,
                g.cx0,
                g.cy0 + cy,
                1,
                b,
                c_alpha,
                c_beta,
                tc0_for(c_index_a, b),
            );
        }
    }
}

/// Convert (column, row) in 4x4-block units to raster-scan 4x4 block index.
/// Layout: col 0-1 = left 8x8, col 2-3 = right 8x8; row 0-1 = top 8x8, row 2-3 = bottom.
/// Raster: 0,1,4,5 / 2,3,6,7 / 8,9,12,13 / 10,11,14,15
fn blk_idx(col: usize, row: usize) -> usize {
    let x8 = col / 2; // 8x8 block column (0 or 1)
    let y8 = row / 2; // 8x8 block row (0 or 1)
    let x4 = col % 2; // position within 8x8
    let y4 = row % 2;
    (y8 * 2 + x8) * 4 + y4 * 2 + x4
}

/// Derive boundary strength for an edge between two 4x4 blocks (spec 8.7.2.1).
///
/// `mb_p`/`mb_q`: macroblock info for each side of the edge.
/// `blk_p`/`blk_q`: 4x4 block index (0-15 raster) within each MB.
/// `intra_bs`: bS when either MB is intra (4 on most MB edges, else 3).
/// `mvy_limit`: vertical MV difference that counts as different motion, 4
/// quarter samples, or 2 for field MBs whose MVs are in field units.
fn derive_bs(
    mb_p: &MbInfo,
    mb_q: &MbInfo,
    blk_p: usize,
    blk_q: usize,
    intra_bs: i32,
    mvy_limit: i32,
) -> i32 {
    if is_intra(mb_p) || is_intra(mb_q) {
        return intra_bs;
    }

    // bS=2: either side has non-zero coded coefficients
    if mb_p.nnz[blk_p] || mb_q.nnz[blk_q] {
        return 2;
    }

    // bS=1 or 0: check reference indices and motion vectors
    if check_mv_diff(mb_p, mb_q, blk_p, blk_q, mvy_limit) {
        return 1;
    }

    0
}

/// Check if two 4x4 blocks have different motion (spec 8.7.2.1 conditions for bS=1).
/// Returns true if reference pictures differ or the MVs differ by 4 or more
/// horizontally or `mvy_limit` or more vertically.
fn check_mv_diff(mb_p: &MbInfo, mb_q: &MbInfo, blk_p: usize, blk_q: usize, mvy_limit: i32) -> bool {
    let list_count = mb_p.list_count.max(mb_q.list_count);

    if list_count <= 1 {
        // P-slice: single list comparison by picture identity (POC).
        // With ref_pic_list_modification, different ref_idx values can map
        // to the same reference picture (e.g., [POC4, POC4, POC0]).
        let rp = mb_p.ref_idx_l0[blk_p];
        let rq = mb_q.ref_idx_l0[blk_q];
        if rp < 0 && rq < 0 {
            return false; // both unused
        }
        if (rp < 0) != (rq < 0) {
            return true; // one used, one not
        }
        if mb_p.ref_poc_l0[blk_p] != mb_q.ref_poc_l0[blk_q] {
            return true;
        }
        if mv_differs(mb_p.mv_l0[blk_p], mb_q.mv_l0[blk_q], mvy_limit) {
            return true;
        }
        return false;
    }

    // B-slice: two-list comparison by actual picture identity (POC).
    // Spec 8.7.2.1: compare reference pictures (not list indices) and MVs.
    // First check straight (p_L0 vs q_L0, p_L1 vs q_L1).
    // If not, check swapped (p_L0 vs q_L1, p_L1 vs q_L0).
    let straight_match = refs_and_mvs_match(
        mb_p.ref_idx_l0[blk_p],
        mb_p.ref_poc_l0[blk_p],
        mb_p.mv_l0[blk_p],
        mb_q.ref_idx_l0[blk_q],
        mb_q.ref_poc_l0[blk_q],
        mb_q.mv_l0[blk_q],
        mvy_limit,
    ) && refs_and_mvs_match(
        mb_p.ref_idx_l1[blk_p],
        mb_p.ref_poc_l1[blk_p],
        mb_p.mv_l1[blk_p],
        mb_q.ref_idx_l1[blk_q],
        mb_q.ref_poc_l1[blk_q],
        mb_q.mv_l1[blk_q],
        mvy_limit,
    );

    if straight_match {
        return false;
    }

    // Try swapped: p_L0 vs q_L1 and p_L1 vs q_L0
    let swapped_match = refs_and_mvs_match(
        mb_p.ref_idx_l0[blk_p],
        mb_p.ref_poc_l0[blk_p],
        mb_p.mv_l0[blk_p],
        mb_q.ref_idx_l1[blk_q],
        mb_q.ref_poc_l1[blk_q],
        mb_q.mv_l1[blk_q],
        mvy_limit,
    ) && refs_and_mvs_match(
        mb_p.ref_idx_l1[blk_p],
        mb_p.ref_poc_l1[blk_p],
        mb_p.mv_l1[blk_p],
        mb_q.ref_idx_l0[blk_q],
        mb_q.ref_poc_l0[blk_q],
        mb_q.mv_l0[blk_q],
        mvy_limit,
    );

    !swapped_match
}

/// Check if reference picture and MV match between two blocks.
/// Compares by POC (picture identity) rather than list index.
fn refs_and_mvs_match(
    ref_a: i8,
    poc_a: i32,
    mv_a: [i16; 2],
    ref_b: i8,
    poc_b: i32,
    mv_b: [i16; 2],
    mvy_limit: i32,
) -> bool {
    if ref_a < 0 && ref_b < 0 {
        return true; // both unused
    }
    if ref_a < 0 || ref_b < 0 {
        return false; // one used, one not
    }
    if poc_a != poc_b {
        return false; // different pictures
    }
    !mv_differs(mv_a, mv_b, mvy_limit)
}

/// Returns true if the MVs differ by 4 or more horizontally or `mvy_limit`
/// or more vertically (quarter-pel).
#[inline]
fn mv_differs(mv_a: [i16; 2], mv_b: [i16; 2], mvy_limit: i32) -> bool {
    (mv_a[0] as i32 - mv_b[0] as i32).abs() >= 4
        || (mv_a[1] as i32 - mv_b[1] as i32).abs() >= mvy_limit
}

/// Filter one 4-sample segment of a vertical edge.
/// `x` is the column of q0; p samples are at x-1, x-2, x-3. q samples at x, x+1, x+2.
/// `y` is the row of the first of `count` consecutive rows to filter.
#[allow(clippy::too_many_arguments)]
fn filter_edge_v(
    plane: &mut [u8],
    stride: usize,
    x: usize,
    y: usize,
    count: usize,
    bs: i32,
    alpha: i32,
    beta: i32,
    tc0: i32,
) {
    filter_edge_v_inner(plane, stride, x, y, count, bs, alpha, beta, tc0, false);
}

#[allow(clippy::too_many_arguments)]
fn filter_edge_v_chroma(
    plane: &mut [u8],
    stride: usize,
    x: usize,
    y: usize,
    count: usize,
    bs: i32,
    alpha: i32,
    beta: i32,
    tc0: i32,
) {
    filter_edge_v_inner(plane, stride, x, y, count, bs, alpha, beta, tc0, true);
}

#[allow(clippy::too_many_arguments)]
fn filter_edge_v_inner(
    plane: &mut [u8],
    stride: usize,
    x: usize,
    y: usize,
    count: usize,
    bs: i32,
    alpha: i32,
    beta: i32,
    tc0: i32,
    is_chroma: bool,
) {
    for i in 0..count {
        let row = y + i;
        let idx_q0 = row * stride + x;
        // Guard against frame buffer overrun from malformed streams
        if x < 3 || idx_q0 + 3 >= plane.len() {
            continue;
        }
        let idx_p0 = idx_q0 - 1;

        let p0 = plane[idx_p0] as i32;
        let p1 = plane[idx_p0 - 1] as i32;
        let q0 = plane[idx_q0] as i32;
        let q1 = plane[idx_q0 + 1] as i32;

        if !should_filter(p0, p1, q0, q1, alpha, beta) {
            continue;
        }

        let p2 = plane[idx_p0 - 2] as i32;
        let q2 = plane[idx_q0 + 2] as i32;

        if bs == 4 {
            if is_chroma {
                // Spec 8.7.2.4: chroma strong filter only modifies p0 and q0
                plane[idx_p0] = ((2 * p1 + p0 + q1 + 2) >> 2) as u8;
                plane[idx_q0] = ((2 * q1 + q0 + p1 + 2) >> 2) as u8;
            } else {
                let (np0, np1, np2, nq0, nq1, nq2) = strong_filter(
                    p0,
                    p1,
                    p2,
                    plane[idx_p0 - 3] as i32,
                    q0,
                    q1,
                    q2,
                    plane[idx_q0 + 3] as i32,
                    alpha,
                    beta,
                );
                plane[idx_p0] = np0 as u8;
                plane[idx_p0 - 1] = np1 as u8;
                plane[idx_p0 - 2] = np2 as u8;
                plane[idx_q0] = nq0 as u8;
                plane[idx_q0 + 1] = nq1 as u8;
                plane[idx_q0 + 2] = nq2 as u8;
            }
        } else if is_chroma {
            // Spec 8.7.2.3: chroma normal filter — tc = tc0 + 1, only p0/q0 modified
            let tc = tc0 + 1;
            let delta = ((((q0 - p0) << 2) + (p1 - q1) + 4) >> 3).clamp(-tc, tc);
            plane[idx_p0] = (p0 + delta).clamp(0, 255) as u8;
            plane[idx_q0] = (q0 - delta).clamp(0, 255) as u8;
        } else {
            let (np0, np1, nq0, nq1) = normal_filter(p0, p1, p2, q0, q1, q2, tc0, beta);
            plane[idx_p0] = np0 as u8;
            plane[idx_p0 - 1] = np1 as u8;
            plane[idx_q0] = nq0 as u8;
            plane[idx_q0 + 1] = nq1 as u8;
        }
    }
}

/// Filter one 4-sample segment of a horizontal edge.
/// `y` is the row of q0; p samples are at y-1, y-2, y-3. q samples at y, y+1, y+2.
#[allow(clippy::too_many_arguments)]
fn filter_edge_h(
    plane: &mut [u8],
    stride: usize,
    x: usize,
    y: usize,
    count: usize,
    bs: i32,
    alpha: i32,
    beta: i32,
    tc0: i32,
) {
    filter_edge_h_inner(plane, stride, x, y, count, bs, alpha, beta, tc0, false);
}

#[allow(clippy::too_many_arguments)]
fn filter_edge_h_chroma(
    plane: &mut [u8],
    stride: usize,
    x: usize,
    y: usize,
    count: usize,
    bs: i32,
    alpha: i32,
    beta: i32,
    tc0: i32,
) {
    filter_edge_h_inner(plane, stride, x, y, count, bs, alpha, beta, tc0, true);
}

#[allow(clippy::too_many_arguments)]
fn filter_edge_h_inner(
    plane: &mut [u8],
    stride: usize,
    x: usize,
    y: usize,
    count: usize,
    bs: i32,
    alpha: i32,
    beta: i32,
    tc0: i32,
    is_chroma: bool,
) {
    for i in 0..count {
        let col = x + i;
        let idx_q0 = y * stride + col;
        // Guard against frame buffer overrun from malformed streams
        if y < 3 || idx_q0 + 3 * stride >= plane.len() {
            continue;
        }
        let idx_p0 = idx_q0 - stride;

        let p0 = plane[idx_p0] as i32;
        let p1 = plane[idx_p0 - stride] as i32;
        let q0 = plane[idx_q0] as i32;
        let q1 = plane[idx_q0 + stride] as i32;

        if !should_filter(p0, p1, q0, q1, alpha, beta) {
            continue;
        }

        let p2 = plane[idx_p0 - 2 * stride] as i32;
        let q2 = plane[idx_q0 + 2 * stride] as i32;

        if bs == 4 {
            if is_chroma {
                // Spec 8.7.2.4: chroma strong filter only modifies p0 and q0
                plane[idx_p0] = ((2 * p1 + p0 + q1 + 2) >> 2) as u8;
                plane[idx_q0] = ((2 * q1 + q0 + p1 + 2) >> 2) as u8;
            } else {
                let (np0, np1, np2, nq0, nq1, nq2) = strong_filter(
                    p0,
                    p1,
                    p2,
                    plane[idx_p0 - 3 * stride] as i32,
                    q0,
                    q1,
                    q2,
                    plane[idx_q0 + 3 * stride] as i32,
                    alpha,
                    beta,
                );
                plane[idx_p0] = np0 as u8;
                plane[idx_p0 - stride] = np1 as u8;
                plane[idx_p0 - 2 * stride] = np2 as u8;
                plane[idx_q0] = nq0 as u8;
                plane[idx_q0 + stride] = nq1 as u8;
                plane[idx_q0 + 2 * stride] = nq2 as u8;
            }
        } else if is_chroma {
            // Spec 8.7.2.3: chroma normal filter modifies only p0 and q0
            // with tc = tc0 + 1 (no ap/aq adjustment)
            let tc = tc0 + 1;
            let delta = ((((q0 - p0) << 2) + (p1 - q1) + 4) >> 3).clamp(-tc, tc);
            plane[idx_p0] = (p0 + delta).clamp(0, 255) as u8;
            plane[idx_q0] = (q0 - delta).clamp(0, 255) as u8;
        } else {
            let (np0, np1, nq0, nq1) = normal_filter(p0, p1, p2, q0, q1, q2, tc0, beta);
            plane[idx_p0] = np0 as u8;
            plane[idx_p0 - stride] = np1 as u8;
            plane[idx_q0] = nq0 as u8;
            plane[idx_q0 + stride] = nq1 as u8;
        }
    }
}

/// Check whether filtering should be applied (spec 8.7.2.3 condition).
#[inline]
fn should_filter(p0: i32, p1: i32, q0: i32, q1: i32, alpha: i32, beta: i32) -> bool {
    (p0 - q0).abs() < alpha && (p1 - p0).abs() < beta && (q1 - q0).abs() < beta
}

#[inline]
fn clip(v: i32) -> i32 {
    v.clamp(0, 255)
}

/// Normal filter for bS=1..3 (H.264 spec 8.7.2.3).
/// Returns (p0', p1', q0', q1').
#[allow(clippy::too_many_arguments)]
fn normal_filter(
    p0: i32,
    p1: i32,
    p2: i32,
    q0: i32,
    q1: i32,
    q2: i32,
    tc0: i32,
    beta: i32,
) -> (i32, i32, i32, i32) {
    let ap = (p2 - p0).abs();
    let aq = (q2 - q0).abs();
    let avg = (p0 + q0 + 1) >> 1;

    // p1' and q1' are computed first; tc is incremented as a side effect
    let mut tc = tc0;

    let new_p1 = if ap < beta {
        tc += 1;
        if tc0 != 0 {
            p1 + (((p2 + avg) >> 1) - p1).clamp(-tc0, tc0)
        } else {
            p1
        }
    } else {
        p1
    };

    let new_q1 = if aq < beta {
        tc += 1;
        if tc0 != 0 {
            q1 + (((q2 + avg) >> 1) - q1).clamp(-tc0, tc0)
        } else {
            q1
        }
    } else {
        q1
    };

    // p0' and q0' use the final tc (after increments)
    let delta = ((((q0 - p0) << 2) + (p1 - q1) + 4) >> 3).clamp(-tc, tc);
    let new_p0 = clip(p0 + delta);
    let new_q0 = clip(q0 - delta);

    (new_p0, new_p1, new_q0, new_q1)
}

/// Strong filter for bS=4 (spec 8.7.2.4).
/// Returns (p0', p1', p2', q0', q1', q2').
#[allow(clippy::too_many_arguments)]
fn strong_filter(
    p0: i32,
    p1: i32,
    p2: i32,
    p3: i32,
    q0: i32,
    q1: i32,
    q2: i32,
    q3: i32,
    alpha: i32,
    beta: i32,
) -> (i32, i32, i32, i32, i32, i32) {
    let ap = (p2 - p0).abs();
    let aq = (q2 - q0).abs();
    let small_gap = (p0 - q0).abs() < ((alpha >> 2) + 2);

    // p-side
    let (np0, np1, np2) = if small_gap && ap < beta {
        (
            (p2 + 2 * p1 + 2 * p0 + 2 * q0 + q1 + 4) >> 3,
            (p2 + p1 + p0 + q0 + 2) >> 2,
            (2 * p3 + 3 * p2 + p1 + p0 + q0 + 4) >> 3,
        )
    } else {
        ((2 * p1 + p0 + q1 + 2) >> 2, p1, p2)
    };

    // q-side (mirror)
    let (nq0, nq1, nq2) = if small_gap && aq < beta {
        (
            (q2 + 2 * q1 + 2 * q0 + 2 * p0 + p1 + 4) >> 3,
            (q2 + q1 + q0 + p0 + 2) >> 2,
            (2 * q3 + 3 * q2 + q1 + q0 + p0 + 4) >> 3,
        )
    } else {
        ((2 * q1 + q0 + p1 + 2) >> 2, q1, q2)
    };

    (np0, np1, np2, nq0, nq1, nq2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_filter() {
        // Large difference → filter
        assert!(should_filter(100, 100, 200, 200, 255, 18));
        // p0-q0 difference >= alpha → no filter
        assert!(!should_filter(0, 0, 255, 255, 100, 18));
        // p1-p0 difference >= beta → no filter
        assert!(!should_filter(100, 80, 110, 110, 255, 5));
    }

    #[test]
    fn test_strong_filter_uniform() {
        // Uniform block: all 128. Filter should not change anything meaningful.
        let (p0, p1, p2, q0, q1, q2) =
            strong_filter(128, 128, 128, 128, 128, 128, 128, 128, 255, 18);
        assert_eq!((p0, p1, p2, q0, q1, q2), (128, 128, 128, 128, 128, 128));
    }

    #[test]
    fn test_strong_filter_step_edge() {
        // Sharp step: p-side=0, q-side=255
        let (p0, _p1, _p2, q0, _q1, _q2) = strong_filter(0, 0, 0, 0, 255, 255, 255, 255, 255, 18);
        // small_gap = |0-255| < (255/4 + 2) = 65 → false
        // ap = |0 - 0| = 0 < 18 → true, but small_gap is false
        // So weak path: p0' = (2*0 + 0 + 255 + 2) >> 2 = 64
        //               q0' = (2*255 + 255 + 0 + 2) >> 2 = 192 (actually 191)
        assert_eq!(p0, 64);
        // q0' = (510 + 255 + 0 + 2) >> 2 = 767 >> 2 = 191
        assert_eq!(q0, 191);
    }

    #[test]
    fn test_normal_filter() {
        // Small step across edge with bS=3, moderate QP
        let (p0, _p1, q0, _q1) = normal_filter(120, 120, 120, 130, 130, 130, 1, 18);
        // ap = 0 < 18, aq = 0 < 18 → tc = 1 + 1 + 1 = 3
        // delta = ((10*4 + 120-130 + 4) >> 3) clamped to [-3,3]
        //       = (40 - 10 + 4) >> 3 = 34 >> 3 = 4, clamped to 3
        assert_eq!(p0, 123);
        assert_eq!(q0, 127);
    }

    #[test]
    fn test_filter_vertical_edge() {
        // 8x4 plane with a step at column 4
        let mut plane = vec![0u8; 8 * 4];
        let stride = 8;
        for row in 0..4 {
            for col in 0..4 {
                plane[row * stride + col] = 100;
            }
            for col in 4..8 {
                plane[row * stride + col] = 200;
            }
        }

        // bS=3, generous thresholds so filtering occurs
        filter_edge_v(&mut plane, stride, 4, 0, 4, 3, 255, 255, 3);

        // After filtering, the step should be smoothed
        // p0 (col 3) should increase, q0 (col 4) should decrease
        for row in 0..4 {
            assert!(plane[row * stride + 3] > 100, "p0 should increase");
            assert!(plane[row * stride + 4] < 200, "q0 should decrease");
        }
    }

    /// Two macroblocks side by side, coded 32 columns wide, with a step at x = 16,
    /// that say they're `width` wide once cropped.
    fn two_mb_frame(width: u32) -> Frame {
        let mut y = vec![0u8; 32 * 16];
        for row in 0..16 {
            for col in 0..32 {
                y[row * 32 + col] = if col < 16 { 100 } else { 120 };
            }
        }
        Frame {
            width,
            height: 16,
            y,
            u: vec![128; 16 * 8],
            v: vec![128; 16 * 8],
            pic_order_cnt: 0,
        }
    }

    #[test]
    fn test_cropped_frame_is_filtered_at_its_coded_width() {
        // Cropping happens on output: until then the planes are laid out at the
        // coded width, so a frame cropped to 22 columns must be filtered exactly
        // like the uncropped one.
        let mb = MbInfo {
            mb_type: MbType::Intra,
            qp_y: 40,
            ..Default::default()
        };
        let mbs = [mb.clone(), mb];
        let mut whole = two_mb_frame(32);
        let mut cropped = two_mb_frame(22);
        filter_frame_params(&mut whole, &mbs, 2, 0, 0, 0, 0);
        filter_frame_params(&mut cropped, &mbs, 2, 0, 0, 0, 0);
        assert_ne!(
            whole.y,
            two_mb_frame(32).y,
            "the edge between the MBs is filtered"
        );
        assert_eq!(cropped.y, whole.y);
    }

    /// Two intra MBs side by side whose luma and both chroma planes step up
    /// across the edge between them.
    fn two_mb_steps() -> Frame {
        let mut frame = two_mb_frame(32);
        for row in 0..8 {
            for col in 0..16 {
                let level = if col < 8 { 100 } else { 120 };
                frame.u[row * 16 + col] = level;
                frame.v[row * 16 + col] = level;
            }
        }
        frame
    }

    fn intra(qp_y: i32) -> MbInfo {
        MbInfo {
            mb_type: MbType::Intra,
            qp_y,
            ..Default::default()
        }
    }

    fn deblock(slice_ids: [u16; 2], slices: &[SliceDeblock], cb: i32, cr: i32) -> Frame {
        let mut frame = two_mb_steps();
        filter_frame_mbaff(
            &mut frame,
            &[intra(40), intra(40)],
            2,
            &slice_ids,
            slices,
            cb,
            cr,
            Interlace::default(),
        );
        frame
    }

    const FILTER: SliceDeblock = SliceDeblock {
        disable_deblocking_filter_idc: 0,
        slice_alpha_c0_offset_div2: 0,
        slice_beta_offset_div2: 0,
    };
    const OFF: SliceDeblock = SliceDeblock {
        disable_deblocking_filter_idc: 1,
        ..FILTER
    };
    const NOT_ACROSS_SLICES: SliceDeblock = SliceDeblock {
        disable_deblocking_filter_idc: 2,
        ..FILTER
    };

    #[test]
    fn test_idc_2_leaves_edges_between_slices_alone() {
        let unfiltered = two_mb_steps();
        // In one slice the edge between the two MBs is filtered
        let one_slice = deblock([0, 0], &[NOT_ACROSS_SLICES], 0, 0);
        assert_ne!(one_slice.y, unfiltered.y);
        // In two slices it's left alone (spec 8.7: filterLeftMbEdgeFlag)
        let two_slices = deblock([0, 1], &[NOT_ACROSS_SLICES, NOT_ACROSS_SLICES], 0, 0);
        assert_eq!(two_slices.y, unfiltered.y);
        assert_eq!(two_slices.u, unfiltered.u);
    }

    #[test]
    fn test_each_mb_filters_as_its_own_slice_says() {
        let unfiltered = two_mb_steps();
        // The edge between the MBs belongs to the right-hand one, so its
        // slice decides: filtered when that slice says so...
        let filtered = deblock([0, 1], &[OFF, FILTER], 0, 0);
        assert_ne!(filtered.y, unfiltered.y);
        // ...and not when it says not to, whatever the first slice says
        let left_alone = deblock([0, 1], &[FILTER, OFF], 0, 0);
        assert_eq!(left_alone.y, unfiltered.y);
    }

    #[test]
    fn test_cr_uses_the_second_chroma_qp_offset() {
        // At QP 40 an offset of -12 lowers Cr's QP enough that its 20-level
        // step is above alpha and isn't filtered, while Cb's is
        let unfiltered = two_mb_steps();
        let frame = deblock([0, 0], &[FILTER], 0, -12);
        assert_ne!(frame.u, unfiltered.u);
        assert_eq!(frame.v, unfiltered.v);
    }

    #[test]
    fn test_filter_horizontal_edge() {
        // 4x8 plane with a step at row 4
        let mut plane = vec![0u8; 4 * 8];
        let stride = 4;
        for row in 0..4 {
            for col in 0..4 {
                plane[row * stride + col] = 100;
            }
        }
        for row in 4..8 {
            for col in 0..4 {
                plane[row * stride + col] = 200;
            }
        }

        filter_edge_h(&mut plane, stride, 0, 4, 4, 3, 255, 255, 3);

        for col in 0..4 {
            assert!(plane[3 * stride + col] > 100, "p0 should increase");
            assert!(plane[4 * stride + col] < 200, "q0 should decrease");
        }
    }
}
