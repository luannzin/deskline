"""Draws Deskline's mark and everything made from it:

    python tools/logo.py

assets/icon.png (512), assets/icon.ico (16-256), assets/tray.png and assets/tray_alert.png
(32, the tray picks its size), assets/banner.png (the README header).

The mark is 32x32 pixel art, like LanPet's: a monitor with a dark outline, and on its screen
the dusk from luannzin.com's dither strip (violet, plum, ember, gold) in a 4x4 Bayer ordered
dither, with a pointer on it: someone else's screen, and you in it. Pixels are placed one by
one and scaled up with nearest-neighbour, so every size stays crisp.

The banner is the same world at banner size: a field of dots on a grid dithered from the night
down to the dusk glow, the mark, and the name in a 5x7 pixel face drawn here.
"""

import os

from PIL import Image

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "assets")


def rgb(h):
    return tuple(int(h[i : i + 2], 16) for i in (1, 3, 5)) + (255,)


NIGHT = rgb("#110d24")
DUSK = [rgb("#3a2a6b"), rgb("#7a3361"), rgb("#e0703f"), rgb("#ffd180")]
LINE = rgb("#1a1424")  # outlines
BEZEL = rgb("#e8e4f0")
BEZEL_LIT = rgb("#ffffff")
BEZEL_SHADE = rgb("#b4acc8")
INK = rgb("#f5f5f5")
ALERT = rgb("#ef4444")
CLEAR = (0, 0, 0, 0)
BAYER = ((0, 8, 2, 10), (12, 4, 14, 6), (3, 11, 1, 9), (15, 7, 13, 5))


def dither(ramp, t, x, y):
    """Colour of cell (x, y) at position t (0..1) along `ramp`, ordered-dithered."""
    step = t * (len(ramp) - 1) + BAYER[y % 4][x % 4] / 16 - 0.5
    return ramp[max(0, min(len(ramp) - 1, round(step)))]


# A pointer, 7 wide and 10 tall: o = outline, w = white.
POINTER = [
    "o......",
    "oo.....",
    "owo....",
    "owwo...",
    "owwwo..",
    "owwwwo.",
    "owwwwwo",
    "owwoooo",
    "owo....",
    "oo.....",
]


def mark(alert=False):
    """The 32x32 mark."""
    img = Image.new("RGBA", (32, 32), CLEAR)
    px = img.load()

    def rect(x0, y0, x1, y1, c):
        for y in range(y0, y1 + 1):
            for x in range(x0, x1 + 1):
                px[x, y] = c

    # bezel: outline, fill, light from the upper left
    rect(2, 4, 29, 23, LINE)
    rect(3, 5, 28, 22, BEZEL)
    rect(3, 5, 28, 5, BEZEL_LIT)
    rect(3, 5, 3, 22, BEZEL_LIT)
    rect(4, 22, 28, 22, BEZEL_SHADE)
    rect(28, 6, 28, 22, BEZEL_SHADE)
    for x, y in ((2, 4), (29, 4), (2, 23), (29, 23)):  # rounded corners
        px[x, y] = CLEAR
    # the screen: dusk, darkest at the top
    rect(5, 7, 26, 20, LINE)
    for y in range(8, 20):
        for x in range(6, 26):
            px[x, y] = dither(DUSK, (y - 8) / 11, x, y)
    # stand
    rect(13, 24, 18, 25, LINE)
    rect(14, 24, 17, 24, BEZEL_SHADE)
    rect(9, 26, 22, 28, LINE)
    rect(10, 26, 21, 27, BEZEL)
    rect(10, 26, 21, 26, BEZEL_LIT)
    # the pointer, tip on the screen's upper middle
    for dy, row in enumerate(POINTER):
        for dx, c in enumerate(row):
            if c != ".":
                px[15 + dx, 9 + dy] = LINE if c == "o" else INK
    if alert:
        rect(23, 0, 31, 8, LINE)
        rect(24, 1, 30, 7, ALERT)
        for x, y in ((23, 0), (31, 0), (23, 8), (31, 8)):
            px[x, y] = CLEAR
    return img


def scaled(img, size):
    """Nearest-neighbour up to a multiple, then down to `size` with a box filter so odd sizes
    (24, 48) stay sharp-edged without uneven pixels."""
    k = max(1, -(-size // img.width))
    big = img.resize((img.width * k, img.height * k), Image.NEAREST)
    return big if big.width == size else big.resize((size, size), Image.BOX)


# 5x7 pixel face for the banner: one string of 35 cells per glyph, rows top to bottom.
FONT = {
    "A": ".###.#...##...#######...##...##...#",
    "B": "####.#...##...#####.#...##...#####.",
    "C": ".###.#...##....#....#....#...#.###.",
    "D": "####.#...##...##...##...##...#####.",
    "E": "######....#....####.#....#....#####",
    "F": "######....#....####.#....#....#....",
    "G": ".###.#...##....#.####...##...#.####",
    "H": "#...##...##...#######...##...##...#",
    "I": ".###...#....#....#....#....#...###.",
    "K": "#...##..#.#.#..##...#.#..#..#.#...#",
    "L": "#....#....#....#....#....#....#####",
    "M": "#...###.###.#.##.#.##...##...##...#",
    "N": "#...###..##.#.##..###...##...##...#",
    "O": ".###.#...##...##...##...##...#.###.",
    "P": "####.#...##...#####.#....#....#....",
    "R": "####.#...##...#####.#.#..#..#.#...#",
    "S": ".#####....#.....###.....#....#####.",
    "T": "#####..#....#....#....#....#....#..",
    "U": "#...##...##...##...##...##...#.###.",
    "V": "#...##...##...##...##...#.#.#...#..",
    "W": "#...##...##...##.#.##.#.##.#.#.#.#.",
    "X": "#...##...#.#.#...#...#.#.#...##...#",
    "Y": "#...##...#.#.#...#....#....#....#..",
    "Z": "#####....#...#...#...#...#....#####",
    "-": "." * 15 + ".###." + "." * 15,
    "·": "." * 17 + "#" + "." * 17,
    " ": "." * 35,
}


def text(img, s, x, y, scale, color):
    """Draw `s` in the 5x7 face, `scale` image pixels per font pixel, 1 font pixel between
    letters. Returns the x after the last letter."""
    px = img.load()
    for ch in s.upper():
        g = FONT[ch].ljust(35, ".")
        for i, c in enumerate(g):
            if c == "#":
                gx, gy = x + (i % 5) * scale, y + (i // 5) * scale
                for yy in range(gy, gy + scale):
                    for xx in range(gx, gx + scale):
                        px[xx, yy] = color
        x += 6 * scale
    return x


def banner():
    """1600x420: a field of dots on a 4px grid, night at the top dithering down to the dusk
    glow at the bottom right where the mark sits, the name and two lines in the pixel face."""
    w, h, cell = 1600, 420, 4
    img = Image.new("RGBA", (w, h), NIGHT)
    px = img.load()
    ramp = [NIGHT] + DUSK
    glow = (1300, 330)
    for y in range(0, h, cell):
        for x in range(0, w, cell):
            near = max(0.0, 1 - ((x - glow[0]) ** 2 + ((y - glow[1]) * 1.6) ** 2) ** 0.5 / 900)
            t = max(near ** 1.6, 0.08 + 0.25 * (y / h) ** 2)
            c = dither(ramp, t, x // cell, y // cell)
            if c != NIGHT:
                for yy in range(y, y + cell - 1):  # a dot, with a 1px gap: the grid shows
                    for xx in range(x, x + cell - 1):
                        px[xx, yy] = c
    m = scaled(mark(), 256)
    img.alpha_composite(m, (1172, 92))
    text(img, "DESKLINE", 110, 120, 14, INK)
    text(img, "REMOTE DESKTOP WITH NO SERVER TO RUN", 112, 260, 4, INK)
    text(img, "WINDOWS · MACOS · LINUX · END-TO-END ENCRYPTED", 112, 310, 3, rgb("#b2b2b2"))
    return img


def main():
    os.makedirs(ROOT, exist_ok=True)
    art = mark()
    scaled(art, 512).save(os.path.join(ROOT, "icon.png"))
    sizes = [16, 24, 32, 48, 64, 128, 256]
    frames = [scaled(art, s) for s in sizes]
    frames[-1].save(os.path.join(ROOT, "icon.ico"), sizes=[(s, s) for s in sizes], append_images=frames[:-1])
    art.save(os.path.join(ROOT, "tray.png"))
    mark(alert=True).save(os.path.join(ROOT, "tray_alert.png"))
    banner().save(os.path.join(ROOT, "banner.png"))
    old = os.path.join(ROOT, "banner.svg")
    if os.path.exists(old):
        os.remove(old)
    print("wrote", ", ".join(sorted(os.listdir(ROOT))))


if __name__ == "__main__":
    main()
