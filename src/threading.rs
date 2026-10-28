//! Frame-level multithreaded decoding.
//!
//! The macroblock loop is inherently serial within a picture (CABAC and
//! intra/MV prediction depend on already-decoded neighbours), so parallelism
//! comes from pipelining *pictures*. [`ThreadedDecoder`] runs all
//! header-level work in coded order on the calling thread — slice header
//! parsing, POC computation and reference list construction via
//! [`prepare_slice`] — against a *shadow* DPB that holds placeholder pictures
//! whose pixels have not been decoded yet. Each complete picture is then
//! handed to a worker thread that runs the macroblock loops and deblocking.
//!
//! A worker starts decoding once every picture its slices reference has been
//! committed, and pictures commit strictly in coded order. Because the shadow
//! DPB replays exactly the bookkeeping the serial [`Decoder`] performs
//! (insert, sliding window, MMCO), with the same metadata in the same order,
//! the reference lists are identical by construction and the output is
//! bit-identical to the serial decoder. The reorder buffer reproduces
//! [`OrderedDecoder`] semantics, so the display-order frame sequence is the
//! same as well; the tests decode every multi-frame test stream both ways and
//! compare byte for byte.
//!
//! Dependencies are tracked at picture granularity. On B-frame content this
//! exposes several pictures of parallelism; a pure P chain references its
//! immediate predecessor and gains little.
//!
//! Field pictures and MBAFF are supported.
//!
//! [`Decoder`]: crate::decoder::Decoder
//! [`OrderedDecoder`]: crate::decoder::OrderedDecoder

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::bitstream::BitstreamReader;
use crate::decoder::{
    apply_reference_marking, deblock_picture, finish_output, make_decoded_picture, prepare_slice,
    run_slice, store_parameter_set, Frame, PictureState, SlicePlan,
};
use crate::dpb::{DecodedPicture, Dpb, PictureStructure};
use crate::error::DecodeError;
use crate::nal::{NalUnit, NalUnitType};
use crate::pps::Pps;
use crate::slice::SliceType;
use crate::sps::Sps;

/// Receives the real decoded picture when its frame commits. Workers read a
/// slot only after the commit barrier guarantees it is filled.
type Slot = Arc<OnceLock<Arc<DecodedPicture>>>;

/// A picture in the shadow DPB: metadata-only placeholder plus the slot its
/// pixels will arrive in.
struct Placeholder {
    pic: Arc<DecodedPicture>,
    slot: Slot,
    seq: u64,
}

/// Picture currently accumulating slices on the coordinator.
struct OpenPicture {
    seq: u64,
    slot: Slot,
    slices: Vec<SlicePlan>,
}

/// Per-picture bookkeeping from dispatch until commit.
struct Scheduled {
    slot: Slot,
    /// Reorder-buffer GOP tag (see `push_frame`).
    gop: u32,
    /// Geometry for the stand-in picture published if decoding fails, so
    /// dependent pictures can still be decoded instead of stalling.
    fallback: Arc<DecodedPicture>,
}

struct CommitBarrier {
    /// Number of pictures committed so far; pictures `0..committed` are
    /// decoded and their slots are filled.
    committed: Mutex<u64>,
    cv: Condvar,
}

/// One slice ready for a worker: the plan plus, for every reference list
/// entry, the slot to take the real picture from (`None` keeps the entry).
struct WorkSlice {
    plan: SlicePlan,
    slots: [Vec<Option<Slot>>; 3],
}

struct PictureJob {
    seq: u64,
    /// Commit count required before decoding may start.
    needed: u64,
    slices: Vec<WorkSlice>,
}

/// H.264 decoder that pipelines pictures across worker threads and emits
/// frames in display order: the threaded counterpart of
/// [`OrderedDecoder`](crate::decoder::OrderedDecoder), with byte-identical
/// output.
///
/// ```no_run
/// use rust_h264::threading::ThreadedDecoder;
/// use rust_h264::nal::parse_annex_b;
///
/// let h264 = std::fs::read("input.h264").unwrap();
/// let nals = parse_annex_b(&h264);
/// let mut decoder = ThreadedDecoder::new(4);
/// for nal in &nals {
///     for frame in decoder.decode_nal(nal).unwrap() {
///         println!("POC {}", frame.pic_order_cnt);
///     }
/// }
/// for frame in decoder.flush() {
///     println!("POC {}", frame.pic_order_cnt);
/// }
/// ```
pub struct ThreadedDecoder {
    threads: usize,
    sps_table: BTreeMap<u32, Arc<Sps>>,
    pps_table: BTreeMap<u32, Arc<Pps>>,
    /// Shadow DPB of placeholder pictures.
    dpb: Dpb,
    open: Option<OpenPicture>,
    /// Placeholder identity (Arc pointer) -> slot/seq, for dependency lookup.
    placeholders: HashMap<usize, Placeholder>,
    next_seq: u64,
    commit_next: u64,
    barrier: Arc<CommitBarrier>,
    scheduled: HashMap<u64, Scheduled>,
    results: BTreeMap<u64, Result<PictureState, DecodeError>>,
    done_tx: Sender<(u64, Result<PictureState, DecodeError>)>,
    done_rx: Receiver<(u64, Result<PictureState, DecodeError>)>,
    inflight: VecDeque<JoinHandle<()>>,
    /// First field of a field pair awaiting its complement (serial
    /// `Decoder::pending_field` equivalent; handled at commit time).
    pending_field: Option<Frame>,
    /// Whether the most recent commit produced an output frame.
    last_commit_output: bool,
    first_error: Option<DecodeError>,
    // Display-order reorder buffer with OrderedDecoder semantics.
    buffer: Vec<(u32, Frame)>,
    gop_id: u32,
    max_depth: usize,
}

impl ThreadedDecoder {
    /// Create a decoder that keeps up to `threads` pictures decoding
    /// concurrently (clamped to at least 1), with the default reorder depth.
    pub fn new(threads: usize) -> Self {
        Self::with_max_depth(threads, 16)
    }

    /// Like [`new`](Self::new) with an explicit reorder-buffer depth
    /// (see [`OrderedDecoder::with_max_depth`](crate::decoder::OrderedDecoder::with_max_depth)).
    pub fn with_max_depth(threads: usize, max_depth: usize) -> Self {
        let (done_tx, done_rx) = channel();
        Self {
            threads: threads.max(1),
            sps_table: BTreeMap::new(),
            pps_table: BTreeMap::new(),
            dpb: Dpb::new(0),
            open: None,
            placeholders: HashMap::new(),
            next_seq: 0,
            commit_next: 0,
            barrier: Arc::new(CommitBarrier {
                committed: Mutex::new(0),
                cv: Condvar::new(),
            }),
            scheduled: HashMap::new(),
            results: BTreeMap::new(),
            done_tx,
            done_rx,
            inflight: VecDeque::new(),
            pending_field: None,
            last_commit_output: false,
            first_error: None,
            buffer: Vec::new(),
            gop_id: 0,
            max_depth: max_depth.max(1),
        }
    }

    /// Number of pictures this decoder may decode concurrently.
    pub fn threads(&self) -> usize {
        self.threads
    }

    /// Frame rate from the most recently parsed SPS's VUI timing info, as
    /// `(numerator, denominator)`. See [`Decoder::frame_rate`](crate::decoder::Decoder::frame_rate).
    pub fn frame_rate(&self) -> Option<(u32, u32)> {
        self.sps_table.values().find_map(|s| s.frame_rate())
    }

    /// Frame rate as a single floating-point value.
    pub fn frame_rate_f64(&self) -> Option<f64> {
        let (n, d) = self.frame_rate()?;
        Some(n as f64 / d as f64)
    }

    /// Feed one NAL unit and return any frames that have become ready, in
    /// display order.
    ///
    /// Errors from slice headers are reported immediately; errors from
    /// macroblock decoding on a worker are reported on a later call (the
    /// picture that failed is replaced by a flat stand-in so the stream
    /// keeps decoding), matching how the serial decoder recovers.
    pub fn decode_nal(&mut self, nal: &NalUnit) -> Result<Vec<Frame>, DecodeError> {
        let mut out = Vec::new();
        match nal.nal_unit_type {
            NalUnitType::Sps | NalUnitType::Pps => {
                store_parameter_set(&mut self.sps_table, &mut self.pps_table, &mut self.dpb, nal)?;
            }
            NalUnitType::SliceIdr | NalUnitType::Slice => {
                let mut peek = BitstreamReader::new(&nal.rbsp);
                let first_mb = peek.read_ue().unwrap_or(0);
                let is_new_picture = first_mb == 0;

                if is_new_picture {
                    // Same point at which the serial decoder finalizes the
                    // previous picture.
                    self.close_open_picture();
                    self.pump(&mut out);
                    while self.inflight.len() >= self.threads {
                        self.wait_one_completion();
                        self.pump(&mut out);
                    }
                }

                match prepare_slice(&self.sps_table, &self.pps_table, &mut self.dpb, nal) {
                    Ok(plan) => {
                        if self.open.is_none() {
                            let seq = self.next_seq;
                            self.next_seq += 1;
                            self.open = Some(OpenPicture {
                                seq,
                                slot: Arc::new(OnceLock::new()),
                                slices: Vec::new(),
                            });
                        }
                        self.open.as_mut().unwrap().slices.push(plan);
                    }
                    // The serial decoder swallows a failed continuation slice
                    // when it already holds part of the picture.
                    Err(_) if self.open.is_some() => {}
                    Err(e) => return Err(e),
                }

                // OrderedDecoder bumps the GOP on every IDR NAL after pushing
                // the previous picture's frame; the open picture closed above
                // was tagged with the pre-bump value.
                if nal.nal_unit_type == NalUnitType::SliceIdr {
                    self.gop_id += 1;
                }
            }
            _ => {}
        }
        self.pump(&mut out);
        if let Some(e) = self.first_error.take() {
            return Err(e);
        }
        Ok(out)
    }

    /// Finish all in-flight pictures and return every remaining frame in
    /// display order. Call once after the last `decode_nal`.
    pub fn flush(&mut self) -> Vec<Frame> {
        let closed = self.close_open_picture();
        let mut out = Vec::new();
        self.pump(&mut out);
        // Join workers in coded order, committing between joins: a worker may
        // be waiting on the commit barrier, and commits happen in pump().
        while let Some(handle) = self.inflight.pop_front() {
            let _ = handle.join();
            self.pump(&mut out);
        }
        self.first_error = None;
        // Serial flush: finalize the last picture; if that produced no frame,
        // output a lone pending field as a half-height frame.
        if !closed || !self.last_commit_output {
            if let Some(field) = self.pending_field.take() {
                self.buffer.push((self.gop_id, field));
            }
        }
        self.buffer.sort_by_key(|(g, f)| (*g, f.pic_order_cnt));
        out.extend(self.buffer.drain(..).map(|(_, f)| f));
        out
    }

    /// Dispatch the open picture (if any) to a worker. Returns whether a
    /// picture was dispatched.
    fn close_open_picture(&mut self) -> bool {
        let Some(open) = self.open.take() else {
            return false;
        };
        let Some(last) = open.slices.last() else {
            return false;
        };

        // Reference marking on the shadow DPB with a metadata-only
        // placeholder, exactly as the serial decoder does when it finalizes
        // this picture (using the last slice's header, like the serial path).
        let placeholder = Arc::new(picture_shell(last, Vec::new(), Vec::new()));
        let fallback = Arc::new(picture_shell(
            last,
            vec![128u8; (last.coded_width * last.coded_height) as usize],
            vec![128u8; (last.coded_width * last.coded_height / 4) as usize],
        ));
        apply_reference_marking(
            &mut self.dpb,
            placeholder.clone(),
            last.nal_unit_type,
            last.nal_ref_idc,
            last.header.frame_num,
            last.header.field_pic_flag,
            &last.header.mmco_ops,
            last.header.long_term_reference_flag,
        );
        self.placeholders.insert(
            Arc::as_ptr(&placeholder) as usize,
            Placeholder {
                pic: placeholder,
                slot: open.slot.clone(),
                seq: open.seq,
            },
        );

        // Map every reference list entry to its slot and find the newest
        // picture this one depends on.
        let mut needed = 0u64;
        let mut slices = Vec::with_capacity(open.slices.len());
        for plan in open.slices {
            let mut map = |list: &[Arc<DecodedPicture>]| -> Vec<Option<Slot>> {
                list.iter()
                    .map(|p| {
                        self.placeholders.get(&(Arc::as_ptr(p) as usize)).map(|ph| {
                            needed = needed.max(ph.seq + 1);
                            ph.slot.clone()
                        })
                    })
                    .collect()
            };
            let slots = [
                map(&plan.ref_pic_list),
                map(&plan.ref_pic_list_l0),
                map(&plan.ref_pic_list_l1),
            ];
            slices.push(WorkSlice { plan, slots });
        }

        self.scheduled.insert(
            open.seq,
            Scheduled {
                slot: open.slot,
                gop: self.gop_id,
                fallback,
            },
        );

        let job = PictureJob {
            seq: open.seq,
            needed,
            slices,
        };
        let barrier = Arc::clone(&self.barrier);
        let done_tx = self.done_tx.clone();
        let seq = job.seq;
        let handle = std::thread::spawn(move || {
            let result = catch_unwind(AssertUnwindSafe(|| run_picture(job, &barrier)))
                .unwrap_or(Err(DecodeError::InvalidSyntax("decode worker panicked")));
            let _ = done_tx.send((seq, result));
        });
        self.inflight.push_back(handle);
        true
    }

    /// Collect finished pictures and commit as many as possible in coded
    /// order, emitting display-ready frames into `out`.
    fn pump(&mut self, out: &mut Vec<Frame>) {
        while let Ok((seq, res)) = self.done_rx.try_recv() {
            self.results.insert(seq, res);
        }
        self.inflight.retain(|h| !h.is_finished());

        while let Some(res) = self.results.remove(&self.commit_next) {
            let seq = self.commit_next;
            let sched = self
                .scheduled
                .remove(&seq)
                .expect("scheduled entry for committed picture");
            match res {
                Ok(mut ps) => {
                    let pic = Arc::new(make_decoded_picture(&mut ps));
                    let _ = sched.slot.set(pic);
                    match finish_output(ps, &mut self.pending_field) {
                        Some(frame) => {
                            self.last_commit_output = true;
                            self.push_frame(sched.gop, frame, out);
                        }
                        None => self.last_commit_output = false,
                    }
                }
                Err(e) => {
                    if self.first_error.is_none() {
                        self.first_error = Some(e);
                    }
                    let _ = sched.slot.set(sched.fallback);
                    self.last_commit_output = false;
                }
            }
            {
                let mut committed = self.barrier.committed.lock().unwrap();
                *committed = seq + 1;
                self.barrier.cv.notify_all();
            }
            self.commit_next = seq + 1;
            // Placeholders no longer held by the shadow DPB or any plan can go.
            self.placeholders
                .retain(|_, ph| Arc::strong_count(&ph.pic) > 1);
        }
    }

    /// Block until a worker result arrives or a short timeout passes. The
    /// timeout matters because a result may already have been drained by an
    /// earlier `pump` while its handle still reads as running.
    fn wait_one_completion(&mut self) {
        match self.done_rx.recv_timeout(Duration::from_millis(20)) {
            Ok((seq, res)) => {
                self.results.insert(seq, res);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {}
        }
    }

    /// Reorder-buffer push with `OrderedDecoder` semantics. Commits arrive in
    /// coded order, so the first frame of a newer GOP proves every older GOP
    /// is complete: drain them as one POC-sorted batch (what OrderedDecoder
    /// does when the IDR NAL arrives), then push and bound the depth.
    fn push_frame(&mut self, gop: u32, frame: Frame, out: &mut Vec<Frame>) {
        if self.buffer.iter().any(|(g, _)| *g < gop) {
            let mut completed: Vec<(u32, Frame)> = Vec::new();
            let mut remaining: Vec<(u32, Frame)> = Vec::with_capacity(self.buffer.len());
            for entry in self.buffer.drain(..) {
                if entry.0 < gop {
                    completed.push(entry);
                } else {
                    remaining.push(entry);
                }
            }
            completed.sort_by_key(|(g, f)| (*g, f.pic_order_cnt));
            out.extend(completed.into_iter().map(|(_, f)| f));
            self.buffer = remaining;
        }
        self.buffer.push((gop, frame));
        while self.buffer.len() > self.max_depth {
            let idx = self
                .buffer
                .iter()
                .enumerate()
                .min_by_key(|(_, (g, f))| (*g, f.pic_order_cnt))
                .map(|(i, _)| i)
                .unwrap();
            out.push(self.buffer.remove(idx).1);
        }
    }
}

impl Drop for ThreadedDecoder {
    fn drop(&mut self) {
        // Let workers finish so none is left blocked on the barrier.
        let mut sink = Vec::new();
        while let Some(handle) = self.inflight.pop_front() {
            self.pump(&mut sink);
            let _ = handle.join();
        }
    }
}

/// A `DecodedPicture` carrying the metadata the DPB needs (frame_num, POC,
/// structure, geometry) with the given planes; motion data is empty or
/// zero-sized to match.
fn picture_shell(plan: &SlicePlan, luma: Vec<u8>, chroma: Vec<u8>) -> DecodedPicture {
    let n = if luma.is_empty() {
        0
    } else {
        plan.total_mbs * 16
    };
    DecodedPicture {
        y: luma,
        u: chroma.clone(),
        v: chroma,
        width: plan.coded_width,
        height: plan.coded_height,
        frame_num: plan.header.frame_num,
        pic_order_cnt: plan.current_poc,
        mv_l0: vec![[0i16; 2]; n],
        ref_idx_l0: vec![-1i8; n],
        ref_poc_l0: vec![-1i32; n],
        mv_l1: vec![[0i16; 2]; n],
        ref_idx_l1: vec![-1i8; n],
        mb_width: plan.mb_width,
        is_intra: plan.header.slice_type == SliceType::I,
        structure: if plan.header.field_pic_flag {
            if plan.header.bottom_field_flag {
                PictureStructure::BottomField
            } else {
                PictureStructure::TopField
            }
        } else {
            PictureStructure::Frame
        },
    }
}

/// Worker side: wait for this picture's references, swap the placeholder
/// references for the real pictures, run every slice, then deblock.
fn run_picture(job: PictureJob, barrier: &CommitBarrier) -> Result<PictureState, DecodeError> {
    {
        let mut committed = barrier.committed.lock().unwrap();
        while *committed < job.needed {
            committed = barrier.cv.wait(committed).unwrap();
        }
    }

    let mut pending: Option<PictureState> = None;
    let mut first_error: Option<DecodeError> = None;
    for mut slice in job.slices {
        materialize(&mut slice);
        // Mirror the serial decoder's recovery: a failed continuation slice
        // keeps the already-decoded macroblocks of earlier slices.
        let backup = pending.clone();
        match run_slice(&slice.plan, pending.take()) {
            Ok(ps) => pending = Some(ps),
            Err(e) => {
                pending = backup;
                if first_error.is_none() {
                    first_error = Some(e);
                }
            }
        }
    }
    let mut ps = match pending {
        Some(ps) => ps,
        None => return Err(first_error.unwrap_or(DecodeError::InvalidSyntax("empty picture"))),
    };
    deblock_picture(&mut ps);
    Ok(ps)
}

/// Replace placeholder reference pictures with the committed ones. Slots are
/// guaranteed filled once the barrier has passed; an entry without a slot (or
/// an unexpectedly empty slot) keeps the placeholder.
fn materialize(slice: &mut WorkSlice) {
    let swap = |list: &mut Vec<Arc<DecodedPicture>>, slots: &[Option<Slot>]| {
        for (entry, slot) in list.iter_mut().zip(slots) {
            if let Some(pic) = slot.as_ref().and_then(|s| s.get()) {
                *entry = Arc::clone(pic);
            }
        }
    };
    let WorkSlice { plan, slots } = slice;
    swap(&mut plan.ref_pic_list, &slots[0]);
    swap(&mut plan.ref_pic_list_l0, &slots[1]);
    swap(&mut plan.ref_pic_list_l1, &slots[2]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoder::OrderedDecoder;
    use crate::nal::parse_annex_b;

    fn decode_serial(nals: &[NalUnit]) -> Vec<Frame> {
        let mut dec = OrderedDecoder::new();
        let mut frames = Vec::new();
        for nal in nals {
            if let Ok(f) = dec.decode_nal(nal) {
                frames.extend(f);
            }
        }
        frames.extend(dec.flush());
        frames
    }

    fn decode_threaded(nals: &[NalUnit], threads: usize) -> Vec<Frame> {
        let mut dec = ThreadedDecoder::new(threads);
        let mut frames = Vec::new();
        for nal in nals {
            if let Ok(f) = dec.decode_nal(nal) {
                frames.extend(f);
            }
        }
        frames.extend(dec.flush());
        frames
    }

    fn assert_same(name: &str, a: &[Frame], b: &[Frame]) {
        assert_eq!(a.len(), b.len(), "{name}: frame count differs");
        for (i, (x, y)) in a.iter().zip(b).enumerate() {
            assert_eq!(
                (x.width, x.height, x.pic_order_cnt),
                (y.width, y.height, y.pic_order_cnt),
                "{name}: frame {i} geometry/POC differs"
            );
            assert!(
                x.y == y.y && x.u == y.u && x.v == y.v,
                "{name}: frame {i} pixels differ"
            );
        }
    }

    fn all_streams() -> Vec<std::path::PathBuf> {
        let dir = format!("{}/testdata", env!("CARGO_MANIFEST_DIR"));
        let mut v: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "h264"))
            // The bench corpus is covered by the ignored test below.
            .filter(|p| {
                !p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("bench_")
            })
            .collect();
        v.sort();
        v
    }

    fn compare_all(threads: usize) {
        for path in all_streams() {
            let data = std::fs::read(&path).unwrap();
            let nals = parse_annex_b(&data);
            let serial = decode_serial(&nals);
            let threaded = decode_threaded(&nals, threads);
            assert_same(&path.display().to_string(), &serial, &threaded);
        }
    }

    #[test]
    fn threaded_matches_serial_1_thread() {
        compare_all(1);
    }

    #[test]
    fn threaded_matches_serial_2_threads() {
        compare_all(2);
    }

    #[test]
    fn threaded_matches_serial_4_threads() {
        compare_all(4);
    }

    #[test]
    #[ignore = "slow: decodes the whole bench corpus twice"]
    fn threaded_matches_serial_bench_corpus() {
        let dir = format!("{}/testdata", env!("CARGO_MANIFEST_DIR"));
        for name in [
            "bench_720p_300f_bframes_complex",
            "bench_1080p_100f_complex",
            "bench_1080p_100f_ponly",
        ] {
            let data = std::fs::read(format!("{dir}/{name}.h264")).unwrap();
            let nals = parse_annex_b(&data);
            assert_same(name, &decode_serial(&nals), &decode_threaded(&nals, 4));
        }
    }

    /// Malformed inputs from the fuzz corpus must not hang or panic the
    /// coordinator or its workers.
    #[test]
    fn threaded_survives_fuzz_regressions() {
        let dir = format!("{}/testdata/fuzz_regressions", env!("CARGO_MANIFEST_DIR"));
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            let data = std::fs::read(&path).unwrap();
            let mut dec = ThreadedDecoder::new(3);
            if path.extension().is_some_and(|x| x == "h264") {
                for nal in parse_annex_b(&data) {
                    let _ = dec.decode_nal(&nal);
                }
            } else {
                if data.len() < 2 {
                    continue;
                }
                let split = (data[0] as usize).min(data.len() - 1);
                let Ok(cfg) = crate::nal::parse_avcc_config(&data[1..1 + split]) else {
                    continue;
                };
                for nal in cfg.sps_nals.iter().chain(cfg.pps_nals.iter()) {
                    let _ = dec.decode_nal(nal);
                }
                for nal in crate::nal::parse_avcc(&data[1 + split..], cfg.length_size) {
                    let _ = dec.decode_nal(&nal);
                }
            }
            let _ = dec.flush();
        }
    }
}
