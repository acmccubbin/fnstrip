#!/usr/bin/env python3
"""Regenerates docs/media: renders frames with the real drawing code and made-up data (see
src/demo_media.rs), then builds the pictures. The bar is 36 times wider than tall, so the
animations show it folded onto two lines, which stays readable at web-page width.

Run from the project root: python3 tools/make_demo_media.py  (needs Pillow, NumPy and ffmpeg)
"""
import os
import subprocess
import tempfile

import numpy as np
from PIL import Image, ImageDraw, ImageFont

WIDTH, HEIGHT = 2170, 60
FOLD = 1002  # between plates on every row
SCALE = 0.75
MARGIN, GAP, CAPTION = 14, 10, 34
FPS = 15
OUT = "docs/media"
FONT = ImageFont.truetype("/usr/share/fonts/Adwaita/AdwaitaSans-Regular.ttf", 18)


def frames(directory, name):
    raw = np.fromfile(os.path.join(directory, f"{name}.raw"), dtype=np.uint8)
    return raw.reshape(-1, HEIGHT, WIDTH, 4)[:, :, :, [2, 1, 0]]  # BGRX to RGB


def finger(frame, x):
    """A translucent fingertip, in native bar pixels."""
    image = Image.fromarray(frame).convert("RGBA")
    layer = Image.new("RGBA", image.size)
    ImageDraw.Draw(layer).ellipse((x - 22, 8, x + 22, 52), fill=(255, 90, 80, 150))
    return np.asarray(Image.alpha_composite(image, layer).convert("RGB"))


def fold(frame, caption=""):
    """The bar on two lines, with an optional caption above, on a dark background."""
    left = Image.fromarray(frame[:, :FOLD])
    right = Image.fromarray(frame[:, FOLD:])
    size = lambda part: (round(part.width * SCALE), round(part.height * SCALE))
    left, right = left.resize(size(left), Image.LANCZOS), right.resize(size(right), Image.LANCZOS)
    top = MARGIN + (CAPTION if caption else 0)
    canvas = Image.new("RGB", (right.width + 2 * MARGIN, top + 2 * left.height + GAP + MARGIN), (24, 24, 24))
    canvas.paste(left, (MARGIN, top))
    canvas.paste(right, (MARGIN, top + left.height + GAP))
    if caption:
        ImageDraw.Draw(canvas).text((MARGIN, MARGIN), caption, font=FONT, fill=(235, 235, 235))
    return canvas


def gif(name, images, fps=FPS):
    """Writes a looping GIF with one shared palette."""
    width, height = images[0].size
    width -= width % 2
    height -= height % 2
    command = ["ffmpeg", "-loglevel", "error", "-y", "-f", "rawvideo", "-pix_fmt", "rgb24",
               "-s", f"{width}x{height}", "-r", str(fps), "-i", "-", "-filter_complex",
               "split[a][b];[a]palettegen=max_colors=96:stats_mode=diff[p];"
               "[b][p]paletteuse=dither=none:diff_mode=rectangle",
               "-loop", "0", os.path.join(OUT, f"{name}.gif")]
    ffmpeg = subprocess.Popen(command, stdin=subprocess.PIPE)
    for image in images:
        ffmpeg.stdin.write(image.crop((0, 0, width, height)).tobytes())
    ffmpeg.stdin.close()
    ffmpeg.wait()


def main():
    os.makedirs(OUT, exist_ok=True)
    with tempfile.TemporaryDirectory() as directory:
        subprocess.run(
            ["cargo", "test", "--release", "--lib", "render_demo_frames", "--", "--ignored"],
            check=True, stdout=subprocess.DEVNULL,
            env={**os.environ, "FNSTRIP_DEMO_DIR": directory, "XDG_STATE_HOME": directory},
        )
        controls = frames(directory, "controls")[0]
        function = frames(directory, "function")[0]
        extras = frames(directory, "extras")
        sliders = frames(directory, "sliders")
        test_screen = frames(directory, "test-screen")
        with open(os.path.join(directory, "sliders.txt")) as text:
            fingers = [line.strip() for line in text]

    # Full-width stills.
    Image.fromarray(controls).save(os.path.join(OUT, "controls.png"))
    Image.fromarray(function).save(os.path.join(OUT, "function.png"))
    Image.fromarray(extras[60]).save(os.path.join(OUT, "extras.png"))

    # The three rows, as Fn changes them.
    rows = [(controls, "Normally: Esc, sliders and media"),
            (function, "While Fn is held: Esc and F1-F12"),
            (extras[60], "After a double-tap of Fn: your buttons and live meters")]
    gif("rows", [fold(frame, caption) for frame, caption in rows for _ in range(2)], fps=1 / 1.25)

    images = []
    for frame, x in zip(sliders, fingers):
        images.append(fold(frame if x == "-" else finger(frame, int(x)), "Drag anywhere on a slider; tap the speaker to mute"))
    gif("sliders", images)
    gif("extras", [fold(frame, "Camera, mic spectrum and VPN traffic (sample data), then CPU, memory, disk and network") for frame in extras])
    gif("test-screen", [fold(frame, "The test screen: triple-tap Fn and hold") for frame in test_screen])
    for name in sorted(os.listdir(OUT)):
        print(f"{name}: {os.path.getsize(os.path.join(OUT, name)) // 1024} KiB")


main()
