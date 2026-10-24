"""Draws the app icon: a pixel-art grass block with a music note.

Writes crisp (nearest-neighbour) desktop icons into src-tauri/icons. Phone
icons come from `npx tauri icon src-tauri/icons/icon-source.png`, run before
this script so these sharper files win.

    python3 scripts/make_icon.py
"""
import random
from pathlib import Path

from PIL import Image

OUT = Path(__file__).resolve().parent.parent / "src-tauri" / "icons"

GRASS = ["#7cc950", "#62b13c", "#4f9a2e"]
DIRT = ["#8b5a2b", "#7a4d24", "#9a6633", "#6a421f"]
NOTE = "#fff6dc"
EDGE = "#2b1a0e"
NOTE_ART = """
.........##.....
.........###....
.........#.##...
.........#..##..
.........#..##..
.........#.##...
.........#......
......####......
.....#####......
.....####.......
......##........
"""
N = 16


def draw() -> Image.Image:
    random.seed(18)
    px = [[random.choices(DIRT, weights=[6, 2, 2, 1])[0] for _ in range(N)] for _ in range(N)]
    drip = [3, 4, 3, 3, 5, 4, 3, 4, 3, 3, 4, 5, 3, 4, 3, 3]
    for x in range(N):
        for y in range(drip[x]):
            px[y][x] = random.choices(GRASS, weights=[3, 4, 2])[0]
        px[drip[x]][x] = "#3e7a24"

    note = set()
    for y, row in enumerate(NOTE_ART.strip().splitlines(), start=3):
        note |= {(x - 1, y) for x, ch in enumerate(row) if ch == "#"}
    outline = {
        (x + dx, y + dy)
        for (x, y) in note
        for dx in (-1, 0, 1)
        for dy in (-1, 0, 1)
        if (x + dx, y + dy) not in note and 0 <= x + dx < N and 0 <= y + dy < N
    }
    for x, y in outline:
        px[y][x] = EDGE
    for x, y in note:
        px[y][x] = NOTE

    img = Image.new("RGBA", (N, N))
    for y in range(N):
        for x in range(N):
            c = px[y][x]
            img.putpixel((x, y), tuple(int(c[i : i + 2], 16) for i in (1, 3, 5)) + (255,))
    return img


def scaled(img: Image.Image, size: int) -> Image.Image:
    return img.resize((size, size), Image.NEAREST)


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    img = draw()
    scaled(img, 1024).save(OUT / "icon-source.png")
    for name, size in [("32x32.png", 32), ("64x64.png", 64), ("128x128.png", 128), ("128x128@2x.png", 256), ("icon.png", 512)]:
        scaled(img, size).save(OUT / name)
    ico_sizes = [16, 24, 32, 48, 64, 128, 256]
    scaled(img, 256).save(OUT / "icon.ico", sizes=[(s, s) for s in ico_sizes], append_images=[scaled(img, s) for s in ico_sizes])
    icns_sizes = [16, 32, 64, 128, 256, 512, 1024]
    scaled(img, 1024).save(OUT / "icon.icns", append_images=[scaled(img, s) for s in icns_sizes])


if __name__ == "__main__":
    main()
