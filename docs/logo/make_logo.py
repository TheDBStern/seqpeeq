"""Draw the seqpeeq logo: seqpeeq-light.svg and seqpeeq-dark.svg, for GitHub's light and dark themes.

A lens over a three-row alignment, beside the name. The lettering is JetBrains Mono, turned into outlines, so the
logo looks the same everywhere and the font is needed only to rebuild it. The output is the same on every run.

To rebuild:

    pip install fonttools
    # Download JetBrainsMono-2.304.zip from https://github.com/JetBrains/JetBrainsMono/releases/tag/v2.304
    # and unzip fonts/ttf/JetBrainsMono-Bold.ttf and fonts/ttf/JetBrainsMono-Regular.ttf.
    python docs/logo/make_logo.py --bold path/to/JetBrainsMono-Bold.ttf \\
        --regular path/to/JetBrainsMono-Regular.ttf
"""

import argparse
from pathlib import Path

from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
from fontTools.ttLib import TTFont

HERE = Path(__file__).resolve().parent

BASES = {"A": "#2da44e", "C": "#3b82f6", "G": "#f59e0b", "T": "#e5484d"}
LETTER = "#ffffff"
THEMES = {
    "light": {"fg": "#1f2328", "muted": "#8c959f", "gap": "#d0d7de", "page": "#ffffff"},
    "dark": {"fg": "#e6edf3", "muted": "#7d8590", "gap": "#30363d", "page": "#0d1117"},
}

# Under the lens, columns 4-6 read GAT / GTT / -AT: a mismatch and a gap.
ROWS = ["TAGCGATCAGTTCA", "TAGCGTTCAGTTCA", "TAGC-ATCAGATCA"]
LENS_COLUMNS = range(4, 7)

NAME = "seqpeeq"
TAGLINE = "alignments · trees · reads"
HEIGHT = 125
LENS_X, LENS_Y, LENS_R = 104, 60, 44
TEXT_X = 236


def num(v):
    """A coordinate with at most 2 decimals and no trailing zeros, so every run writes the same text."""
    s = f"{v:.2f}".rstrip("0").rstrip(".")
    return "0" if s == "-0" else s


def glyph_path(font, text, size, x, baseline):
    """The outline of `text` as one SVG path `d`, left-aligned at `x`, and the run's advance width."""
    glyphs = font.getGlyphSet()
    cmap = font.getBestCmap()
    hmtx = font["hmtx"]
    scale = size / font["head"].unitsPerEm
    pen = SVGPathPen(glyphs, ntos=num)
    advance = 0.0
    for ch in text:
        name = cmap[ord(ch)]
        glyphs[name].draw(TransformPen(pen, (scale, 0, 0, -scale, x + advance, baseline)))
        advance += hmtx[name][0] * scale
    return pen.getCommands(), advance


def rect(x, y, w, h, fill, rx=None):
    corner = f' rx="{num(rx)}"' if rx else ""
    return f'<rect x="{num(x)}" y="{num(y)}" width="{num(w)}" height="{num(h)}"{corner} fill="{fill}"/>'


def small_cell(x, y, base, theme):
    if base == "-":
        return rect(x, y + 3, 9, 2, theme["gap"])
    return rect(x, y, 9, 8, BASES[base], rx=1)


def big_cell(x, y, base, theme, bold):
    w, h = 16, 18
    if base == "-":
        return [rect(x, y + h / 2 - 1.5, w, 3, theme["gap"])]
    size = 13
    scale = size / bold["head"].unitsPerEm
    advance = bold["hmtx"][bold.getBestCmap()[ord(base)]][0] * scale
    cap = bold["OS/2"].sCapHeight * scale
    d, _ = glyph_path(bold, base, size, x + (w - advance) / 2, y + (h + cap) / 2)
    return [rect(x, y, w, h, BASES[base], rx=2), f'<path d="{d}" fill="{LETTER}"/>']


def draw_logo(theme_name, bold, regular):
    """The whole logo for one theme, as an SVG document."""
    theme = THEMES[theme_name]
    out = []
    # The alignment. Cells near the lens are covered by its filled face.
    for r, row in enumerate(ROWS):
        for i, base in enumerate(row):
            out.append(small_cell(18 + i * 11, LENS_Y - 15 + r * 11, base, theme))
    # The lens: a face in the page color, a ring and a handle.
    cx, cy, rr = LENS_X, LENS_Y, LENS_R
    out.append(f'<circle cx="{cx}" cy="{cy}" r="{rr}" fill="{theme["page"]}" stroke="{theme["fg"]}" '
               f'stroke-width="5"/>')
    out.append(f'<path d="M{num(cx + 0.7 * rr)} {num(cy + 0.7 * rr)} L{num(cx + 1.3 * rr)} {num(cy + 1.3 * rr)}" '
               f'stroke="{theme["fg"]}" stroke-width="10" stroke-linecap="round"/>')
    # The magnified columns.
    for r, row in enumerate(ROWS):
        for j, c in enumerate(LENS_COLUMNS):
            out += big_cell(cx - 27 + j * 19, cy - 29 + r * 20, row[c], theme, bold)
    # The name and the tagline.
    name, name_w = glyph_path(bold, NAME, 46, TEXT_X, 68)
    tag, tag_w = glyph_path(regular, TAGLINE, 13.8, TEXT_X, 96.5)
    out.append(f'<path d="{name}" fill="{theme["fg"]}"/>')
    out.append(f'<path d="{tag}" fill="{theme["muted"]}"/>')

    width = num(TEXT_X + max(name_w, tag_w) + 8)
    head = (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} {HEIGHT}" width="{width}" '
            f'height="{HEIGHT}" role="img" aria-label="seqpeeq">')
    return "\n".join([head, "<title>seqpeeq</title>", *out, "</svg>"]) + "\n"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--bold", required=True, help="JetBrainsMono-Bold.ttf")
    parser.add_argument("--regular", required=True, help="JetBrainsMono-Regular.ttf")
    parser.add_argument("--out-dir", default=HERE, type=Path, help="where to write the SVGs (default: here)")
    args = parser.parse_args(argv)
    bold, regular = TTFont(args.bold), TTFont(args.regular)
    for theme in THEMES:
        path = args.out_dir / f"seqpeeq-{theme}.svg"
        path.write_text(draw_logo(theme, bold, regular), newline="\n")
        print(path)


if __name__ == "__main__":
    main()
