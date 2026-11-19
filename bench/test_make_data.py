"""Tests for make_data.py. Run from the repo root:

    /path/to/envs/seqpeeq-bench/bin/python -m unittest discover -s bench -p 'test_*.py'
"""

import re
import subprocess
import tempfile
import time
import unittest
from pathlib import Path

import make_data

ROOT = Path(__file__).resolve().parent.parent
SEQPEEQ = ROOT / "target" / "release" / "seqpeeq"


def records(path):
    """(name, sequence) for each record of a FASTA file."""
    out = []
    for line in Path(path).read_text().splitlines():
        if line.startswith(">"):
            out.append([line[1:], ""])
        else:
            out[-1][1] += line
    return out


class MakeData(unittest.TestCase):
    def setUp(self):
        self._dir = tempfile.TemporaryDirectory()
        self.dir = Path(self._dir.name)

    def tearDown(self):
        self._dir.cleanup()

    def test_alignment_has_the_rows_and_columns(self):
        path = self.dir / "a.fa"
        make_data.write_alignment(path, rows=50, cols=40, seed=1)
        got = records(path)
        self.assertEqual(len(got), 50)
        self.assertTrue(all(len(seq) == 40 for _, seq in got))
        self.assertTrue(all(set(seq) <= set("ACGT-") for _, seq in got))
        self.assertEqual(got[0][0], "s000001")

    def test_tree_has_the_tips(self):
        path = self.dir / "t.nwk"
        make_data.write_tree(path, tips=1000, seed=1)
        text = path.read_text().strip()
        self.assertTrue(text.endswith(";"))
        tips = re.findall(r"[(,](t\d+):", text)
        self.assertEqual(sorted(tips), sorted(f"t{i}" for i in range(1, 1001)))

    def test_same_seed_same_bytes(self):
        for write, size in [(make_data.write_alignment, dict(rows=30, cols=30)),
                            (make_data.write_tree, dict(tips=300))]:
            a, b = self.dir / "a", self.dir / "b"
            write(a, seed=7, **size)
            write(b, seed=7, **size)
            self.assertEqual(a.read_bytes(), b.read_bytes(), write.__name__)

    def test_existing_files_are_kept(self):
        make_data.main(["--out", str(self.dir), "--only", "tiny"])
        files = sorted(self.dir.iterdir())
        self.assertTrue(files)
        before = {p: p.stat().st_mtime_ns for p in files}
        time.sleep(0.05)
        make_data.main(["--out", str(self.dir), "--only", "tiny"])
        self.assertEqual({p: p.stat().st_mtime_ns for p in files}, before)
        make_data.main(["--out", str(self.dir), "--only", "tiny", "--force"])
        fa = self.dir / "tiny.fa"
        self.assertNotEqual(fa.stat().st_mtime_ns, before[fa])

    def test_seqpeeq_opens_the_tiny_files(self):
        self.assertTrue(SEQPEEQ.exists(), "build it first: cargo build --release -p seqpeeq")
        make_data.main(["--out", str(self.dir), "--only", "tiny"])
        for i, name in enumerate(["tiny.fa", "tiny.nwk", "tiny.bam"]):
            done = subprocess.run(
                [str(SEQPEEQ), "--log", str(self.dir / f"s{i}.log"), "run", f"open {self.dir / name}"],
                capture_output=True, text=True,
            )
            self.assertEqual(done.returncode, 0, done.stderr)
            self.assertIn("opened", done.stdout + done.stderr, name)

    def test_bam_is_sorted_and_indexed(self):
        bam = make_data.write_bam(self.dir, depth=2, seed=1, length=5000)
        self.assertTrue(Path(f"{bam}.bai").exists())
        count = subprocess.run([make_data.samtools(), "view", "-c", str(bam)],
                               capture_output=True, text=True, check=True)
        self.assertGreater(int(count.stdout), 0)


if __name__ == "__main__":
    unittest.main()
