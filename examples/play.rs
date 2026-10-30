/// Decode an H.264 bitstream and display frames in a window.
///
/// Usage: cargo run --release --features minifb --example play -- <input.h264> [--fps N] [--loop]
///
/// Streams decode: feeds NALs incrementally, reorders by POC for correct
/// display order, then displays each frame at the specified rate.
/// Press Escape to quit. With --loop, playback repeats from the beginning.
use minifb::{Key, Window, WindowOptions};
use rust_h264::decoder::{Decoder, Frame};
use rust_h264::nal::{parse_annex_b, NalUnitType};
use std::time::{Duration, Instant};

/// Convert a YUV 4:2:0 or 4:2:2 frame to ARGB pixel buffer for display.
fn yuv_to_argb(f: &rust_h264::decoder::Frame, width: usize, height: usize) -> Vec<u32> {
    let (y, u, v) = (&f.y, &f.u, &f.v);
    let cw = width / 2;
    // Chroma rows per luma row: 1 for 4:2:2, 1/2 for 4:2:0
    let c_row = |row: usize| row * f.chroma_height() as usize / f.height.max(1) as usize;
    // Samples scaled to 8 bits for display
    let shift = f.bit_depth - 8;
    let s = |plane: &[u8], i: usize| (f.sample(plane, i) >> shift) as i32;
    let mut argb = vec![0u32; width * height];
    for row in 0..height {
        for col in 0..width {
            let y_val = s(y, row * width + col);
            let u_val = s(u, c_row(row) * cw + col / 2) - 128;
            let v_val = s(v, c_row(row) * cw + col / 2) - 128;

            let r = (y_val + ((v_val * 359 + 128) >> 8)).clamp(0, 255) as u32;
            let g = (y_val - ((u_val * 88 + v_val * 183 + 128) >> 8)).clamp(0, 255) as u32;
            let b = (y_val + ((u_val * 454 + 128) >> 8)).clamp(0, 255) as u32;

            argb[row * width + col] = (r << 16) | (g << 8) | b;
        }
    }
    argb
}

/// Reorder buffer: collects decoded frames and emits them in display order (by POC).
struct ReorderBuffer {
    buf: Vec<(u32, Frame)>, // (idr_count, frame)
    max_depth: usize,
}

impl ReorderBuffer {
    fn new(max_depth: usize) -> Self {
        Self {
            buf: Vec::new(),
            max_depth,
        }
    }

    /// Push a decoded frame into the buffer.
    fn push(&mut self, idr_count: u32, frame: Frame) {
        self.buf.push((idr_count, frame));
    }

    /// If the buffer has enough frames, pop the lowest-POC one for display.
    fn pop_if_ready(&mut self) -> Option<Frame> {
        if self.buf.len() > self.max_depth {
            self.pop_lowest()
        } else {
            None
        }
    }

    /// Pop the frame with the lowest (idr_count, poc).
    fn pop_lowest(&mut self) -> Option<Frame> {
        if self.buf.is_empty() {
            return None;
        }
        let min_idx = self
            .buf
            .iter()
            .enumerate()
            .min_by_key(|(_, (idr, f))| (*idr, f.pic_order_cnt))
            .map(|(i, _)| i)
            .unwrap();
        Some(self.buf.remove(min_idx).1)
    }

    /// Flush all remaining frames in display order.
    fn flush(&mut self) -> Vec<Frame> {
        self.buf.sort_by_key(|(idr, f)| (*idr, f.pic_order_cnt));
        self.buf.drain(..).map(|(_, f)| f).collect()
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: play <input.h264> [--fps N] [--loop]");
        std::process::exit(1);
    }
    let input_path = &args[1];

    let mut fps_override: Option<f64> = None;
    let mut do_loop = false;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--fps" => {
                i += 1;
                fps_override = args[i].parse().ok();
            }
            "--loop" => do_loop = true,
            _ => eprintln!("Unknown option: {}", args[i]),
        }
        i += 1;
    }

    let h264_data = match std::fs::read(input_path) {
        Ok(data) => data,
        Err(e) => {
            eprintln!("Error: cannot read '{}': {}", input_path, e);
            std::process::exit(1);
        }
    };
    let nals = parse_annex_b(&h264_data);

    // Decode enough frames to get dimensions and detect max B-frame depth
    let mut decoder = Decoder::new();
    let mut idr_count: u32 = 0;
    let mut reorder_buf = ReorderBuffer::new(4); // initial depth, will adjust
    let mut first_frame_argb = None;
    let mut width = 0;
    let mut height = 0;
    let mut nal_idx = 0;

    // Feed NALs until we get the first displayable frame
    while nal_idx < nals.len() {
        let nal = &nals[nal_idx];
        let is_idr = nal.nal_unit_type == NalUnitType::SliceIdr;
        match decoder.decode_nal(nal) {
            Ok(Some(f)) => {
                width = f.width as usize;
                height = f.height as usize;
                reorder_buf.push(idr_count, f);
            }
            Ok(None) => {}
            Err(e) => {
                eprintln!("Error decoding: {:?}", e);
                std::process::exit(1);
            }
        }
        if is_idr {
            idr_count += 1;
        }
        if let Some(display_frame) = reorder_buf.pop_if_ready() {
            first_frame_argb = Some(yuv_to_argb(&display_frame, width, height));
            nal_idx += 1;
            break;
        }
        nal_idx += 1;
    }

    // If reorder buffer hasn't emitted yet, flush to get first frame
    if first_frame_argb.is_none() {
        let flushed = reorder_buf.flush();
        if let Some(f) = flushed.into_iter().next() {
            width = f.width as usize;
            height = f.height as usize;
            first_frame_argb = Some(yuv_to_argb(&f, width, height));
        }
    }

    let first_frame_argb = first_frame_argb.unwrap_or_else(|| {
        eprintln!("No frames decoded.");
        std::process::exit(1);
    });

    // Pick frame rate: explicit --fps wins, then SPS VUI timing, then 30 fps.
    let (fps, fps_source) = if let Some(rate) = fps_override {
        (rate, "user override")
    } else if let Some(rate) = decoder.frame_rate_f64() {
        (rate, "SPS VUI timing")
    } else {
        (30.0, "default (no VUI timing)")
    };

    eprintln!(
        "Playing {} ({}x{}) at {} fps ({}) — streaming decode, display-order",
        input_path, width, height, fps, fps_source
    );

    // Create window
    let scale = if width <= 128 && height <= 128 {
        4
    } else if width <= 320 && height <= 240 {
        2
    } else {
        1
    };

    let mut window = Window::new(
        &format!("rust_h264 — {} ({}x{})", input_path, width, height),
        width * scale,
        height * scale,
        WindowOptions {
            resize: true,
            scale_mode: minifb::ScaleMode::AspectRatioStretch,
            ..WindowOptions::default()
        },
    )
    .expect("failed to create window");

    let frame_duration = Duration::from_secs_f64(1.0 / fps);
    let mut last_frame_time = Instant::now();
    let mut frame_count = 0u64;

    // Display first frame
    window
        .update_with_buffer(&first_frame_argb, width, height)
        .expect("failed to update window");
    frame_count += 1;
    let mut current_argb;
    let _ = first_frame_argb; // already displayed
    let mut flushing = false;
    let mut flush_queue: Vec<Frame> = Vec::new();

    'outer: loop {
        // Get the next display-order frame
        let mut display_frame = None;

        // First drain any flush queue
        if !flush_queue.is_empty() {
            display_frame = Some(flush_queue.remove(0));
        }

        // Feed NALs until reorder buffer emits a frame
        while display_frame.is_none() && !flushing {
            if nal_idx > nals.len() {
                // Already flushed decoder, now flush reorder buffer
                flush_queue = reorder_buf.flush();
                flushing = true;
                if !flush_queue.is_empty() {
                    display_frame = Some(flush_queue.remove(0));
                }
                break;
            }

            let is_idr =
                nal_idx < nals.len() && nals[nal_idx].nal_unit_type == NalUnitType::SliceIdr;

            let frame = if nal_idx < nals.len() {
                match decoder.decode_nal(&nals[nal_idx]) {
                    Ok(f) => f,
                    Err(e) => {
                        eprintln!("Error decoding NAL {}: {:?}", nal_idx, e);
                        nal_idx += 1;
                        continue;
                    }
                }
            } else {
                // Flush decoder
                decoder.flush()
            };
            nal_idx += 1;

            // Push returned frame with CURRENT idr_count (it belongs to the
            // previous picture, before the IDR boundary)
            if let Some(f) = frame {
                reorder_buf.push(idr_count, f);
                if display_frame.is_none() {
                    if let Some(df) = reorder_buf.pop_if_ready() {
                        display_frame = Some(df);
                    }
                }
            }

            // AFTER pushing the previous frame, handle IDR: increment count
            // and flush the reorder buffer for the old GOP
            if is_idr {
                idr_count += 1;
                let idr_flushed = reorder_buf.flush();
                if !idr_flushed.is_empty() {
                    flush_queue.extend(idr_flushed);
                    if display_frame.is_none() {
                        display_frame = Some(flush_queue.remove(0));
                    }
                }
            }
        }

        // If flushing and no more frames, end of stream
        if display_frame.is_none() {
            if do_loop {
                decoder = Decoder::new();
                nal_idx = 0;
                idr_count = 0;
                flushing = false;
                flush_queue.clear();
                reorder_buf = ReorderBuffer::new(4);
                eprintln!("Looping... ({} frames played)", frame_count);
                continue;
            }
            // Show last frame until window closed
            while window.is_open() && !window.is_key_down(Key::Escape) {
                window.update();
                std::thread::sleep(Duration::from_millis(10));
            }
            break;
        }

        let f = display_frame.unwrap();
        current_argb = yuv_to_argb(&f, width, height);
        frame_count += 1;

        // Wait for frame timing
        loop {
            if !window.is_open() || window.is_key_down(Key::Escape) {
                break 'outer;
            }
            let now = Instant::now();
            if now.duration_since(last_frame_time) >= frame_duration {
                last_frame_time = now;
                break;
            }
            window.update();
            std::thread::sleep(Duration::from_millis(1));
        }

        window
            .update_with_buffer(&current_argb, width, height)
            .expect("failed to update window");
    }

    eprintln!("Played {} frames", frame_count);
}
