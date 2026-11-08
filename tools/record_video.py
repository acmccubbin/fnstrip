#!/usr/bin/env python3
"""Turns a fnstrip debug recording (~/.cache/fnstrip-record, see src/record.rs) into a
video: the frames the bar submitted, each shown from its submit time until the next, with
the touch drawn on as a red line (where the finger is at that moment) and a timeline strip.

Usage: python3 tools/record_video.py [--speed 0.5] [--fps 60] [out.mp4]
"""
import argparse
import os
import re
import subprocess
import sys

import numpy as np

WIDTH, HEIGHT = 2170, 60
RECORD = os.path.expanduser("~/.cache/fnstrip-record")


def load():
    submits, touches = [], []
    with open(os.path.join(RECORD, "events.log")) as log:
        for line in log:
            stamp, kind, *rest = line.split(" ", 2)
            t = float(stamp) / 1000.0
            if kind == "submit":
                submits.append(t)
            elif kind == "input":
                found = re.search(r"touches=\[([^\]]*)\]", line)
                points = found.group(1).split() if found else []
                x = int(points[0].split(",")[0]) if points else None
                touches.append((t, x))
    frames = np.memmap(os.path.join(RECORD, "frames.raw"), dtype=np.uint8, mode="r")
    count = min(len(submits), frames.size // (WIDTH * HEIGHT * 4))
    frames = frames[: count * WIDTH * HEIGHT * 4].reshape(count, HEIGHT, WIDTH, 4)
    return submits[:count], touches, frames


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--speed", type=float, default=0.5, help="playback speed (0.5 = half)")
    parser.add_argument("--fps", type=int, default=60)
    parser.add_argument("out", nargs="?", default=os.path.join(RECORD, "recording.mp4"))
    args = parser.parse_args()
    submits, touches, frames = load()
    if not submits:
        sys.exit("no frames recorded")
    start = min(submits[0], touches[0][0] if touches else submits[0])
    end = max(submits[-1], touches[-1][0] if touches else submits[-1]) + 0.5
    step = args.speed / args.fps  # recorded seconds per output frame
    strip = 24
    out_height = HEIGHT * 2 + strip
    ffmpeg = subprocess.Popen(
        ["ffmpeg", "-loglevel", "error", "-y", "-f", "rawvideo", "-pix_fmt", "bgr0",
         "-s", f"{WIDTH}x{out_height}", "-r", str(args.fps), "-i", "-",
         "-vf", "scale=1086:-2", "-pix_fmt", "yuv420p", args.out],
        stdin=subprocess.PIPE,
    )
    frame_index, touch_index, touch_x = 0, 0, None
    submit_marks = np.array(submits)
    t = start
    while t < end:
        while frame_index + 1 < len(submits) and submits[frame_index + 1] <= t:
            frame_index += 1
        while touch_index < len(touches) and touches[touch_index][0] <= t:
            touch_x = touches[touch_index][1]
            touch_index += 1
        canvas = np.zeros((out_height, WIDTH, 4), dtype=np.uint8)
        if submits[0] <= t:
            canvas[:HEIGHT] = frames[frame_index]
            canvas[HEIGHT:2 * HEIGHT] = frames[frame_index]
        if touch_x is not None:
            # Red finger line on the lower copy, so the upper one stays clean.
            x0, x1 = max(touch_x - 3, 0), min(touch_x + 3, WIDTH)
            canvas[HEIGHT:2 * HEIGHT, x0:x1] = (0, 0, 255, 0)
        # Timeline: one second across the strip, ticks for submits (green) in that second.
        recent = submit_marks[(submit_marks > t - 1.0) & (submit_marks <= t)]
        for mark in recent:
            x = int((mark - (t - 1.0)) * (WIDTH - 1))
            canvas[2 * HEIGHT:, x:x + 3] = (0, 200, 0, 0)
        canvas[2 * HEIGHT:, WIDTH - 3:] = (255, 255, 255, 0)
        ffmpeg.stdin.write(canvas.tobytes())
        t += step
    ffmpeg.stdin.close()
    ffmpeg.wait()
    print(f"{args.out}: {len(submits)} frames over {end - start:.1f} s, at {args.speed}x")


main()
