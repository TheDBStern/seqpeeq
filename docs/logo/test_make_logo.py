"""Tests for make_logo.py.

    SEQPEEQ_LOGO_FONTS=/path/to/fonts python -m unittest discover -s docs/logo -p 'test_*.py'

The folder must hold JetBrainsMono-Bold.ttf and JetBrainsMono-Regular.ttf (release 2.304). Without it, or without
fonttools, the tests are skipped.
"""

import os
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path

try:
    from fontTools.ttLib import TTFont
except ImportError:
    TTFont = None

HERE = Path(__file__).resolve().parent
FONTS = os.environ.get("SEQPEEQ_LOGO_FONTS")
SVG = "{http://www.w3.org/2000/svg}"
SKIP = "set SEQPEEQ_LOGO_FONTS to a folder holding JetBrainsMono-Bold.ttf and -Regular.ttf"
PAGES = {"light": "#ffffff", "dark": "#0d1117"}


@unittest.skipIf(not FONTS or TTFont is None, SKIP)
class Logo(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        import make_logo

        cls.make_logo = make_logo
        cls.bold = TTFont(Path(FONTS) / "JetBrainsMono-Bold.ttf")
        cls.regular = TTFont(Path(FONTS) / "JetBrainsMono-Regular.ttf")
        cls.svgs = {t: make_logo.draw_logo(t, cls.bold, cls.regular) for t in PAGES}

    def test_no_text_elements(self):
        for theme, svg in self.svgs.items():
            with self.subTest(theme=theme):
                self.assertNotIn("<text", svg)
                ET.fromstring(svg)

    def test_theme_colors(self):
        light, dark = self.svgs["light"], self.svgs["dark"]
        self.assertIn("#1f2328", light)
        self.assertNotIn("#e6edf3", light)
        self.assertIn("#e6edf3", dark)
        self.assertNotIn("#1f2328", dark)
        for svg in (light, dark):
            for color in ("#2da44e", "#3b82f6", "#f59e0b", "#e5484d", "#ffffff"):
                self.assertIn(color, svg)

    def test_lens_filled_with_page_color(self):
        for theme, svg in self.svgs.items():
            with self.subTest(theme=theme):
                root = ET.fromstring(svg)
                circles = root.findall(f".//{SVG}circle")
                self.assertEqual(len(circles), 1)
                self.assertEqual(circles[0].get("fill"), PAGES[theme])
                _, _, width, height = root.get("viewBox").split()
                for rect in root.iter(f"{SVG}rect"):
                    self.assertFalse(rect.get("width") == width and rect.get("height") == height,
                                     "a background rect fills the whole logo")

    def test_every_character_has_a_glyph(self):
        for font, text in ((self.bold, "seqpeeqACGT"), (self.regular, "alignments · trees · reads")):
            cmap = font.getBestCmap()
            for ch in set(text):
                with self.subTest(ch=ch):
                    self.assertIn(ord(ch), cmap)
                    self.assertNotEqual(cmap[ord(ch)], ".notdef")

    def test_output_is_stable(self):
        for theme in PAGES:
            with self.subTest(theme=theme):
                again = self.make_logo.draw_logo(theme, self.bold, self.regular)
                self.assertEqual(again, self.svgs[theme])

    def test_committed_files_match(self):
        for theme in PAGES:
            with self.subTest(theme=theme):
                committed = (HERE / f"seqpeeq-{theme}.svg").read_text()
                self.assertEqual(committed, self.svgs[theme])


if __name__ == "__main__":
    unittest.main()
