"""Draws the installer's icon and animated splash from the app logo.

Output: installer/mirage.ico and installer/splash.gif (both committed, so CI
does not need Pillow). Run again only when the logo changes:

    python scripts/make_installer_assets.py

The splash echoes the update page of the app: a hairline horizon and a small
sun that travels along it while the installer works.
"""
import math
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent.parent
LOGO = ROOT / "client/apps/desktop/assets/logo.webp"
OUT = ROOT / "installer"

BG = (16, 18, 22)
SURFACE = (27, 30, 37)
BORDER = (43, 48, 57)
TEXT = (240, 241, 243)
ACCENT = (236, 239, 244)

# The palm-and-sun mark and the word, as boxes of the 2000x667 logo.
MARK_BOX = (370, 195, 825, 490)
WORD_BOX = (850, 225, 1640, 470)


def tint(mask: Image.Image, color) -> Image.Image:
    """A solid layer of `color` shaped by the logo's alpha."""
    layer = Image.new("RGBA", mask.size, color + (0,))
    layer.putalpha(mask.getchannel("A"))
    return layer


def logo_parts():
    logo = Image.open(LOGO).convert("RGBA")
    return logo.crop(MARK_BOX), logo.crop(WORD_BOX)


def make_icon():
    mark, _ = logo_parts()
    sizes = [16, 24, 32, 48, 64, 128, 256]
    big = 1024
    tile = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    draw = ImageDraw.Draw(tile)
    draw.rounded_rectangle((0, 0, big - 1, big - 1), radius=int(big * 0.22), fill=BG)
    draw.rounded_rectangle(
        (6, 6, big - 7, big - 7), radius=int(big * 0.22), outline=BORDER, width=10
    )
    glyph = tint(mark, TEXT)
    scale = (big * 0.66) / glyph.width
    glyph = glyph.resize((int(glyph.width * scale), int(glyph.height * scale)), Image.LANCZOS)
    tile.alpha_composite(glyph, ((big - glyph.width) // 2, (big - glyph.height) // 2 + 6))
    OUT.mkdir(exist_ok=True)
    tile.save(OUT / "mirage.ico", sizes=[(s, s) for s in sizes])


def ease(t: float) -> float:
    return t * t * (3 - 2 * t)


def make_splash():
    width, height = 640, 360
    mark, word = logo_parts()
    mark = tint(mark, TEXT)
    word = tint(word, TEXT)
    mark_h = 150
    mark = mark.resize((int(mark.width * mark_h / mark.height), mark_h), Image.LANCZOS)
    word_h = 92
    word = word.resize((int(word.width * word_h / word.height), word_h), Image.LANCZOS)
    gap = 26
    total = mark.width + gap + word.width
    left = (width - total) // 2
    top = 86
    frames = []
    count = 36
    line_y = 292
    x0, x1 = 120, width - 120
    for i in range(count):
        frame = Image.new("RGBA", (width, height), BG)
        draw = ImageDraw.Draw(frame)
        # Letters and mark settle in over the first third of the loop.
        appear = ease(min(1.0, i / 12))
        for layer, x in ((mark, left), (word, left + mark.width + gap)):
            faded = layer.copy()
            faded.putalpha(faded.getchannel("A").point(lambda v: int(v * appear)))
            frame.alpha_composite(faded, (x, top + int((1 - appear) * 10)))
        # The horizon, and a sun that travels it back and forth.
        draw.line((x0, line_y, x1, line_y), fill=BORDER, width=2)
        phase = (math.sin(i / count * 2 * math.pi - math.pi / 2) + 1) / 2
        sun_x = x0 + (x1 - x0) * phase
        for radius, shade in ((11, 0.14), (6, 1.0)):
            color = tuple(int(BG[k] + (ACCENT[k] - BG[k]) * shade) for k in range(3))
            draw.ellipse(
                (sun_x - radius, line_y - radius, sun_x + radius, line_y + radius),
                fill=color,
            )
        frames.append(frame.convert("RGB"))
    OUT.mkdir(exist_ok=True)
    frames[0].save(
        OUT / "splash.gif",
        save_all=True,
        append_images=frames[1:],
        duration=55,
        loop=0,
        optimize=False,
    )


if __name__ == "__main__":
    make_icon()
    make_splash()
    print("wrote", OUT / "mirage.ico", OUT / "splash.gif")
