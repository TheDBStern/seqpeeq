"""Tests for run.py. They use the tiny files, made into a temporary folder."""

import subprocess
import tempfile
import time
import unittest
from pathlib import Path

import make_data
import run


class Measure(unittest.TestCase):
    def test_a_quick_command_is_ok(self):
        got = run.measure(["/bin/sleep", "0.2"], timeout=5)
        self.assertEqual(got.outcome, "ok", got.detail)
        self.assertGreaterEqual(got.seconds, 0.2)
        self.assertLess(got.seconds, 1.5)
        self.assertGreater(got.max_rss, 0)

    def test_timeout_kills_the_whole_group(self):
        marker = "sleep 31.5"
        started = time.monotonic()
        got = run.measure(["/bin/sh", "-c", f"{marker} & {marker}; wait"], timeout=1)
        self.assertEqual(got.outcome, "timed out")
        self.assertLess(time.monotonic() - started, 3)
        time.sleep(0.2)
        left = subprocess.run(["pgrep", "-f", marker], capture_output=True, text=True)
        self.assertEqual(left.stdout.strip(), "", "a child of the timed-out run is still alive")

    def test_leftover_processes_are_a_failure(self):
        # A launcher that starts the real program and exits would
        # otherwise be timed instead of the program.
        marker = "sleep 31.7"
        got = run.measure(["/bin/sh", "-c", f"{marker} & exit 0"], timeout=5)
        self.assertEqual(got.outcome, "failed")
        self.assertIn("still running", got.detail)
        for _ in range(100):
            left = subprocess.run(["pgrep", "-f", marker], capture_output=True, text=True)
            if not left.stdout.strip():
                break
            time.sleep(0.02)
        self.assertEqual(left.stdout.strip(), "", "the process left behind was not killed")

    def test_out_of_memory_is_recorded(self):
        got = run.measure(["/bin/sh", "-c", "echo java.lang.OutOfMemoryError >&2; exit 1"], timeout=5)
        self.assertEqual(got.outcome, "out of memory")

    def test_failed_command_keeps_its_last_output(self):
        got = run.measure(["/bin/sh", "-c", "echo boom; exit 3"], timeout=5)
        self.assertEqual(got.outcome, "failed")
        self.assertIn("boom", got.detail)


class Ready(unittest.TestCase):
    def test_bam_ready_needs_the_reads_drawn(self):
        ready = run.seqpeeq_ready("bam")
        self.assertFalse(ready("t.bam\nopened t.bam (bam, 1 contig)\nc1:1"))
        self.assertTrue(ready("t.bam\nc1:1  depth 7  1 bp a column"))

    def test_alignment_ready_needs_the_open_message(self):
        ready = run.seqpeeq_ready("msa")
        self.assertFalse(ready("opening aln.fa…"))
        self.assertTrue(ready("opened aln.fa (msa, 5 sequences x 20 columns)"))


class Tools(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls._dir = tempfile.TemporaryDirectory()
        cls.dir = Path(cls._dir.name)
        make_data.main(["--out", str(cls.dir), "--only", "tiny"])

    @classmethod
    def tearDownClass(cls):
        cls._dir.cleanup()

    def scratch(self, name):
        path = self.dir / name
        path.mkdir(exist_ok=True)
        return path

    def test_seqpeeq_shows_the_reads_of_the_igv_window(self):
        # 300 kb opens zoomed out past the reads, so seqpeeq must be taken
        # to the window IGV is sent to before its reads are drawn.
        bam = make_data.write_bam(self.dir, depth=1, seed=1, length=300_000,
                                  ref="mid_ref.fa", name="mid.bam")
        argv = run.commands("seqpeeq", bam, self.scratch("s"))
        got = run.measure(argv, timeout=30, ready=run.seqpeeq_steps("bam"))
        self.assertEqual(got.outcome, "ok", got.detail)
        # Without the go-to, the last step must never hold.
        alone = run.measure(argv, timeout=3, ready=run.seqpeeq_steps("bam")[-1][0])
        self.assertEqual(alone.outcome, "timed out")
        run.commands("igv", bam, self.scratch("i"))
        self.assertIn(f"goto {run.WINDOW}\n", (self.scratch("i") / "batch.txt").read_text())

    def test_the_window_test_needs_the_window_scale(self):
        shown = run.seqpeeq_steps("bam")[-1][0]
        # A small contig shows a mean depth at open, zoomed out.
        self.assertFalse(shown("c1:1  mean depth 2.0  20 bp a column"))
        self.assertTrue(shown("c1:1  mean depth 2.0  10 bp a column"))
        self.assertTrue(shown("c1:1  depth 2  1 bp a column"))

    def test_jalview_never_uses_the_home_folder(self):
        props = Path.home() / ".jalview_properties"
        before = props.read_bytes() if props.exists() else None
        scratch = self.scratch("jh").resolve()
        argv = run.commands("jalview", self.dir / "tiny.fa", scratch)
        self.assertIn(f"-Duser.home={scratch}", argv)
        got = run.measure(argv, timeout=120)
        self.assertEqual(got.outcome, "ok", got.detail)
        self.assertEqual(props.read_bytes() if props.exists() else None, before)

    def test_jalview_is_timed_itself_not_its_launcher(self):
        argv = run.commands("jalview", self.dir / "tiny.fa", self.scratch("j"))
        self.assertIn("jalview.bin.Jalview", argv)
        self.assertNotIn("-jar", argv)

    def test_seqpeeq_shows_an_alignment_and_a_tree(self):
        for name, kind in [("tiny.fa", "msa"), ("tiny.nwk", "tree")]:
            argv = run.commands("seqpeeq", self.dir / name, self.scratch("s"))
            got = run.measure(argv, timeout=30, ready=run.seqpeeq_steps(kind))
            self.assertEqual(got.outcome, "ok", f"{name}: {got.detail}")

    def test_igv_never_uses_the_home_folder(self):
        prefs = Path.home() / "igv" / "prefs.properties"
        before = prefs.read_bytes() if prefs.exists() else None
        scratch = self.scratch("igv").resolve()
        argv = run.commands("igv", self.dir / "tiny.bam", scratch)
        self.assertIn(f"-Duser.home={scratch}", argv)
        got = run.measure(argv, timeout=120)
        self.assertEqual(got.outcome, "ok", got.detail)
        self.assertTrue((scratch / "igv" / "prefs.properties").exists())
        self.assertEqual(prefs.read_bytes() if prefs.exists() else None, before)

    def test_figtree_and_jalview_run(self):
        for tool, name in [("figtree", "tiny.nwk"), ("jalview", "tiny.fa")]:
            argv = run.commands(tool, self.dir / name, self.scratch(tool))
            got = run.measure(argv, timeout=120)
            self.assertEqual(got.outcome, "ok", f"{tool}: {got.detail}")


if __name__ == "__main__":
    unittest.main()
