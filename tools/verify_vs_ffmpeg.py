#!/usr/bin/env python3
"""Verify rust_h264 output against FFmpeg, byte for byte.

Usage:
    python tools/verify_vs_ffmpeg.py <input> [input2 ...]

Accepts raw H.264 elementary streams (.h264) or any container FFmpeg can
read (.mp4, .mkv, ...). For containers, the H.264 track is extracted with
a bitstream copy, so both decoders see identical Annex-B input — the
intended check for real content such as PS5 cutscenes pulled from game
.mp4 files.

Both sides decode to raw YUV420P in display order:
  - reference: ffmpeg -pix_fmt yuv420p -f rawvideo
  - rust_h264: cargo run --release --example dump_frames

The two files are compared byte-wise; on mismatch the first differing
offset is reported. Exits non-zero on any mismatch.

Requires ffmpeg/ffprobe on PATH and a rust toolchain.
"""

import os
import shutil
import subprocess
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def run(cmd, **kw):
    return subprocess.run(cmd, capture_output=True, text=True, **kw)


def probe_frames(path):
    r = run([
        "ffprobe", "-v", "error", "-count_frames", "-select_streams", "v:0",
        "-show_entries", "stream=nb_read_frames", "-of", "csv=p=0", path,
    ])
    if r.returncode != 0:
        sys.exit(f"ffprobe failed on {path}:\n{r.stderr}")
    return int(r.stdout.strip())


def extract_h264(path, out):
    r = run([
        "ffmpeg", "-y", "-loglevel", "error", "-i", path,
        "-c:v", "copy", "-bsf:v", "h264_mp4toannexb", "-f", "h264", out,
    ])
    if r.returncode != 0:
        sys.exit(f"ffmpeg extraction failed on {path}:\n{r.stderr}")


def ffmpeg_decode(path, out):
    r = run([
        "ffmpeg", "-y", "-loglevel", "error", "-i", path,
        "-pix_fmt", "yuv420p", "-f", "rawvideo", out,
    ])
    if r.returncode != 0:
        sys.exit(f"ffmpeg decode failed on {path}:\n{r.stderr}")


def rust_decode(path, out):
    r = run([
        "cargo", "build", "--release", "--example", "dump_frames",
    ], cwd=REPO)
    if r.returncode != 0:
        sys.exit(f"cargo build failed:\n{r.stderr}")
    exe = os.path.join(REPO, "target", "release", "examples",
                       "dump_frames.exe" if os.name == "nt" else "dump_frames")
    r = run([exe, path, out])
    if r.returncode != 0:
        sys.exit(f"dump_frames failed on {path}:\n{r.stderr}\n{r.stdout}")
    return r.stderr


def first_diff(a, b):
    chunk = 1 << 20
    with open(a, "rb") as fa, open(b, "rb") as fb:
        off = 0
        while True:
            ca, cb = fa.read(chunk), fb.read(chunk)
            if not ca and not cb:
                return None
            if len(ca) != len(cb):
                return off + min(len(ca), len(cb))
            for x, y in zip(ca, cb):
                if x != y:
                    return off
                off += 1
            off += 0


def verify(path):
    print(f"== {path}")
    with tempfile.TemporaryDirectory() as td:
        ext = os.path.splitext(path)[1].lower()
        h264 = os.path.join(td, "in.h264")
        if ext == ".h264":
            shutil.copyfile(path, h264)
        else:
            extract_h264(path, h264)
            print(f"   extracted H.264 track ({os.path.getsize(h264)} bytes)")

        frames = probe_frames(h264)
        ref = os.path.join(td, "ref.yuv")
        ours = os.path.join(td, "ours.yuv")
        ffmpeg_decode(h264, ref)
        info = rust_decode(h264, ours)
        if info.strip():
            print(f"   decoder: {info.strip()}")

        ref_sz, our_sz = os.path.getsize(ref), os.path.getsize(ours)
        our_frames = None
        for line in info.splitlines():
            if "frames" in line:
                pass
        if ref_sz != our_sz:
            print(f"   FAIL: size mismatch ref={ref_sz} ours={our_sz} "
                  f"(expected {frames} frames)")
            return False
        d = first_diff(ref, ours)
        if d is not None:
            print(f"   FAIL: first differing byte at {d} ({frames} frames, {our_sz} bytes)")
            return False
        print(f"   OK: {frames} frames, {our_sz} bytes, byte-identical")
        return True


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    results = [verify(p) for p in sys.argv[1:]]
    ok = sum(results)
    print(f"\n{ok}/{len(results)} inputs byte-identical with FFmpeg")
    sys.exit(0 if ok == len(results) else 1)


if __name__ == "__main__":
    main()
