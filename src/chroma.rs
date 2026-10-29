//! Chroma macroblock decoding shared by the CAVLC and CABAC paths: inter
//! prediction of a partition, residual parsing, and reconstruction.

use crate::bitstream::BitstreamReader;
use crate::cabac::CabacReader;
use crate::cavlc::parse_residual_block_cavlc;
use crate::dpb::DecodedPicture;
use crate::error::DecodeError;
use crate::inter_pred;
use crate::neighbor::{cabac_neighbor_nz_chroma, compute_nc_chroma, dequant_4x4_ac_raster};
use crate::residual::{
    chroma_dc_422, chroma_qp, dequant_chroma_dc, inverse_dct_4x4, inverse_hadamard_2x2, ZIGZAG_4X4,
    ZIGZAG_4X4_FIELD,
};
use crate::slice_context::{SliceContext, SliceParams};

/// Chroma residual coefficients of one macroblock, Cb then Cr, as parsed.
#[derive(Default)]
pub(crate) struct ChromaCoeffs {
    /// DC coefficients in parse order (4 for 4:2:0, 8 for 4:2:2)
    pub(crate) dc: [[i32; 8]; 2],
    /// AC coefficients of each 4x4 block, scan positions 1..=15
    pub(crate) ac: [[[i32; 15]; 8]; 2],
}

/// One list's motion for a partition: the reference picture, the ref_idx as
/// coded (field units in an MBAFF field MB, for weights and field choice)
/// and the motion vector.
pub(crate) type PartMotion<'r> = (&'r DecodedPicture, i8, [i16; 2]);

impl SliceContext<'_> {
    /// Chroma inter prediction of the partition at luma offset `(x, y)`,
    /// size `w` x `h`, of the current MB, written to both chroma planes:
    /// motion compensation from each list used, then weighted (explicit
    /// weights for one list, any weights or the average for two).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn predict_chroma_part(
        &mut self,
        mb_idx: usize,
        mb_x: usize,
        mb_y: usize,
        sp: &SliceParams,
        (x, y, w, h): (usize, usize, usize, usize),
        l0: Option<PartMotion<'_>>,
        l1: Option<PartMotion<'_>>,
    ) {
        let sy = self.chroma_shift_y();
        let c422 = self.chroma_422();
        let (cw, ch) = (w / 2, h >> sy);
        let (cx, cy) = (mb_x / 2 + x / 2, y >> sy);
        let n = cw * ch;
        if n == 0 || n > 128 {
            return;
        }
        let weights = sp
            .wctx
            .mb(self.mbaff && self.mb_field_decoding[mb_idx / 2], mb_idx);
        for plane in 0..2 {
            let mc = |ctx: &Self, (pic, ref_idx, mv): PartMotion<'_>, out: &mut [u8]| {
                let (_, _, _, mc_cy, c_stride, c_off) =
                    ctx.mc_params(mb_idx, mb_y, pic.width as usize, ref_idx);
                // Vertical chroma MVs are in 1/8 sample units for 4:2:0
                // (with the field parity offset), 1/4 for 4:2:2 (spec
                // 8.4.1.4, 8.4.2.2.2): doubled, they use the same filter
                let mv_y = if c422 {
                    mv[1] as i32 * 2
                } else {
                    mv[1] as i32 + ctx.chroma_mv_offset(pic, c_off)
                };
                inter_pred::chroma_mc(
                    pic.chroma_ref(plane, c_off),
                    c_stride,
                    (ctx.height >> sy) as usize,
                    cx as i32,
                    (mc_cy + cy) as i32,
                    mv[0] as i32,
                    mv_y,
                    cw,
                    ch,
                    out,
                );
            };
            let mut pred = [0u8; 128];
            match (l0, l1) {
                (Some(m0), Some(m1)) => {
                    let mut p0 = [0u8; 128];
                    let mut p1 = [0u8; 128];
                    mc(self, m0, &mut p0[..n]);
                    mc(self, m1, &mut p1[..n]);
                    weights.apply_bi(
                        &p0[..n],
                        &p1[..n],
                        &mut pred[..n],
                        m0.1 as usize,
                        m1.1 as usize,
                        true,
                        plane,
                    );
                }
                (Some(m), None) | (None, Some(m)) => {
                    mc(self, m, &mut pred[..n]);
                    if sp.use_weight == 1 {
                        let list = usize::from(l0.is_none());
                        weights.apply_uni(&mut pred[..n], list, m.1 as usize, true, plane);
                    }
                }
                (None, None) => return,
            }
            let dst = if plane == 0 {
                &mut self.frame.u
            } else {
                &mut self.frame.v
            };
            for r in 0..ch {
                let o = self.lc_offset + (cy + r) * self.lc_stride + cx;
                dst[o..o + cw].copy_from_slice(&pred[r * cw..(r + 1) * cw]);
            }
        }
    }

    /// Parse the chroma residual of a CAVLC macroblock (spec 7.3.5.3),
    /// recording each AC block's total_coeff for later nC derivation.
    pub(crate) fn decode_chroma_residual_cavlc(
        &mut self,
        reader: &mut BitstreamReader,
        mb_idx: usize,
        cbp_chroma: u8,
    ) -> Result<ChromaCoeffs, DecodeError> {
        let mut c = ChromaCoeffs::default();
        // 4 chroma blocks per plane for 4:2:0 (DC nC -1), 8 for 4:2:2 (-2)
        let c422 = self.chroma_422();
        let (blocks, rows, dc_nc) = if c422 { (8, 4, -2) } else { (4, 2, -1) };
        if cbp_chroma >= 1 {
            for dc in &mut c.dc {
                parse_residual_block_cavlc(reader, &mut dc[..blocks], blocks, dc_nc)?;
            }
        }
        if cbp_chroma >= 2 {
            for (plane, ac) in c.ac.iter_mut().enumerate() {
                for (blk, block) in ac.iter_mut().enumerate().take(blocks) {
                    let nc_arr: &[u8] = if plane == 0 { self.nc_cb } else { self.nc_cr };
                    let nc = compute_nc_chroma(
                        nc_arr,
                        mb_idx,
                        self.mb_width as usize,
                        blk,
                        rows,
                        self.mb_slice_id,
                        self.this_slice_id,
                        self.mbaff,
                        self.mb_field_decoding,
                    );
                    let tc = parse_residual_block_cavlc(reader, block, 15, nc)?;
                    let nc_arr = if plane == 0 {
                        &mut *self.nc_cb
                    } else {
                        &mut *self.nc_cr
                    };
                    nc_arr[mb_idx * 8 + blk] = tc;
                }
            }
        }
        Ok(c)
    }

    /// Parse the chroma residual of a CABAC macroblock (spec 7.3.5.3):
    /// DC then AC blocks, with coded_block_flag contexts from the
    /// neighbours (an unavailable neighbour counts as coded for an intra
    /// macroblock).
    pub(crate) fn decode_chroma_residual_cabac(
        &mut self,
        cr: &mut CabacReader,
        st: &mut [u8; 1024],
        mb_idx: usize,
        cbp_chroma: u8,
        intra: bool,
    ) -> ChromaCoeffs {
        let mut c = ChromaCoeffs::default();
        let field = self.is_field_coded(mb_idx);
        let (blocks, rows) = if self.chroma_422() { (8, 4) } else { (4, 2) };
        if cbp_chroma >= 1 {
            for (plane, dc) in c.dc.iter_mut().enumerate() {
                let bit = 6 + plane;
                let left = self
                    .left_mb(mb_idx)
                    .map_or(intra, |m| (self.mb_cbp[m] >> bit) & 1 != 0);
                let top = self
                    .above_mb(mb_idx)
                    .map_or(intra, |m| (self.mb_cbp[m] >> bit) & 1 != 0);
                if cr.decode_coded_block_flag(st, 3, left, top) {
                    let (coeffs, _) = cr.decode_residual_cabac_field(st, 3, blocks, field);
                    for (p, v) in coeffs {
                        dc[p] = v;
                    }
                    self.mb_cbp[mb_idx] |= 1 << bit;
                }
            }
        }
        if cbp_chroma >= 2 {
            for (plane, ac) in c.ac.iter_mut().enumerate() {
                for (blk, block) in ac.iter_mut().enumerate().take(blocks) {
                    let nc_arr: &[u8] = if plane == 0 { self.nc_cb } else { self.nc_cr };
                    let nz = |is_left: bool| {
                        cabac_neighbor_nz_chroma(
                            nc_arr,
                            mb_idx,
                            self.mb_width as usize,
                            blk,
                            is_left,
                            intra,
                            rows,
                            self.mb_slice_id,
                            self.this_slice_id,
                            self.mbaff,
                            self.mb_field_decoding,
                        )
                    };
                    let (left, top) = (nz(true), nz(false));
                    if cr.decode_coded_block_flag(st, 4, left, top) {
                        let (coeffs, tc) = cr.decode_residual_cabac_field(st, 4, 15, field);
                        let nc_arr = if plane == 0 {
                            &mut *self.nc_cb
                        } else {
                            &mut *self.nc_cr
                        };
                        nc_arr[mb_idx * 8 + blk] = tc;
                        for (p, v) in coeffs {
                            block[p] = v;
                        }
                    }
                }
            }
        }
        c
    }

    /// Add the chroma residual to the prediction already in the frame:
    /// DC transform and scaling, then each 4x4 block's inverse transform
    /// (spec 8.5.11). Scaling lists 1/2 (intra) or 4/5 (inter); Cr takes
    /// the second chroma QP offset.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn add_chroma_residual(
        &mut self,
        c: &ChromaCoeffs,
        cbp_chroma: u8,
        qp_y: i32,
        intra: bool,
        mb_x: usize,
        mb_idx: usize,
        sp: &SliceParams,
    ) {
        if cbp_chroma == 0 {
            return;
        }
        let zigzag = if self.is_field_scan(mb_idx) {
            &ZIGZAG_4X4_FIELD
        } else {
            &ZIGZAG_4X4
        };
        let cx = mb_x / 2;
        for plane in 0..2 {
            let scale = &sp.scaling_list_4x4[if intra { 1 } else { 4 } + plane];
            let offset = if plane == 0 {
                sp.chroma_qp_index_offset
            } else {
                sp.second_chroma_qp_index_offset
            };
            let qp_c = chroma_qp(qp_y, offset);
            let mut dc = c.dc[plane];
            let blocks = if self.chroma_422() {
                chroma_dc_422(&mut dc, qp_c, scale[0]);
                8
            } else {
                let mut dc4 = [dc[0], dc[1], dc[2], dc[3]];
                inverse_hadamard_2x2(&mut dc4);
                dequant_chroma_dc(&mut dc4, qp_c, scale[0]);
                dc[..4].copy_from_slice(&dc4);
                4
            };
            for (blk, &dc) in dc.iter().enumerate().take(blocks) {
                let mut block = [0i32; 16];
                block[0] = dc;
                if cbp_chroma >= 2 {
                    for (i, &v) in c.ac[plane][blk].iter().enumerate() {
                        let (r, col) = zigzag[i + 1];
                        block[r * 4 + col] = v;
                    }
                    dequant_4x4_ac_raster(&mut block, qp_c, scale);
                }
                inverse_dct_4x4(&mut block);
                let dst = if plane == 0 {
                    &mut self.frame.u
                } else {
                    &mut self.frame.v
                };
                let (by, bx) = ((blk / 2) * 4, (blk % 2) * 4);
                for r in 0..4 {
                    let o = self.lc_offset + (by + r) * self.lc_stride + cx + bx;
                    for (k, px) in dst[o..o + 4].iter_mut().enumerate() {
                        *px = (*px as i32 + block[r * 4 + k]).clamp(0, 255) as u8;
                    }
                }
            }
        }
    }
}
