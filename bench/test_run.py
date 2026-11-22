"""Tests for run.py. They use the tiny files, made into a temporary folder."""

import platform
import subprocess
import tempfile
import time
import unittest
from unittest import mock
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

    def test_a_tool_that_ignores_quit_is_killed(self):
        # Killed 10 s after `q`; killing its group again once only a
        # zombie is left must not crash the harness.
        started = time.monotonic()
        got = run.measure(["/bin/sh", "-c", "echo shown; exec sleep 31.9"], timeout=30,
                          ready=run.screen_shows("shown"))
        self.assertEqual(got.outcome, "ok", got.detail)
        self.assertLess(time.monotonic() - started, 15)

    def test_out_of_memory_is_recorded(self):
        got = run.measure(["/bin/sh", "-c", "echo java.lang.OutOfMemoryError >&2; exit 1"], timeout=5)
        self.assertEqual(got.outcome, "out of memory")

    def test_failed_command_keeps_its_last_output(self):
        got = run.measure(["/bin/sh", "-c", "echo boom; exit 3"], timeout=5)
        self.assertEqual(got.outcome, "failed")
        self.assertIn("boom", got.detail)


class Build(unittest.TestCase):
    def test_seqpeeq_is_built_for_this_machine(self):
        # An x86_64 seqpeeq on Apple silicon runs translated, beside native
        # Java tools.
        self.assertEqual(run.architecture(run.SEQPEEQ), platform.machine())
        self.assertIn(f"({platform.machine()})", run.versions()["seqpeeq"])

    def test_termal_is_built_for_this_machine(self):
        self.assertEqual(run.architecture(run.TERMAL), platform.machine())
        self.assertIn(f"({platform.machine()})", run.versions()["termal"])

    def test_versions_name_the_terminal_peers(self):
        found = run.versions()
        for key in ["termal", "samtools", "asciigenome", "java (ASCIIGenome)"]:
            self.assertIn(key, found)
            self.assertTrue(found[key], key)

    def test_missing_tools_are_named(self):
        everything = ["seqpeeq", "termal", "tview", "asciigenome", "jalview", "figtree", "igv"]
        self.assertEqual(run.missing_tools(everything), [])
        with mock.patch.object(run, "TERMAL", Path("/nonexistent/termal")):
            self.assertEqual(run.missing_tools(["seqpeeq", "termal"]), ["termal"])
        with mock.patch.object(run, "BENCH_ASCII", Path("/nonexistent/env")):
            self.assertEqual(run.missing_tools(["asciigenome", "tview"]), ["asciigenome"])


# samtools tview on tiny.bam at c1:1, as captured: ruler, reference, reads.
TVIEW_READS = """\
1         11        21        31        41        51        61
CGTTAATTACTCCTCCGGAATTTGTCCTACACTACCTAGCATACCCATGTAGCGTCGACTCGCAC
                                                            ..
                                                            ..
"""

# ASCIIGenome on tiny.bam at c1:1-1000, as captured: the coverage track drawn
# before the reads, then the whole window with its footer and prompt.
ASCIIGENOME_COVERAGE = """\
********************250*****************500*****************760*******
tiny.bam#1; ylim[auto auto]; range[1.0 5.0]
                            .::::::             :     :::
                            :::::::.           .:     :::.
"""
ASCIIGENOME_DRAWN = """\
********************250*****************500*****************
tiny.bam#1; ylim[auto auto]; range[1.0 5.0]
          _.::::::::::::::::::::::::::::::::::::::::::::::::
tiny.bam@2; Reads: 18
                            <<<<<<<<<<<<<<<<<<<<<<<<
                                 >>>>>>>>>>>>>>>>>>>>>>>>>
1         65        125       190       250       315
0         .06       .13       .19       .25       .31
c1:1-1000; 1,000 bp; 6.2 bp/char
Enter h for help:
"""


class Ready(unittest.TestCase):
    def test_asciigenome_ready_needs_the_reads_and_the_window(self):
        self.assertFalse(run.asciigenome_ready(""))
        self.assertFalse(run.asciigenome_ready(ASCIIGENOME_COVERAGE))
        self.assertTrue(run.asciigenome_ready(ASCIIGENOME_DRAWN))

    def test_tview_ready_needs_the_reads(self):
        self.assertFalse(run.tview_ready(""))
        # The ruler and the reference alone: no reads drawn yet.
        self.assertFalse(run.tview_ready("\n".join(TVIEW_READS.splitlines()[:2])))
        self.assertTrue(run.tview_ready(TVIEW_READS))

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

    def mid_bam(self):
        """A 300 kb BAM, which opens zoomed out past its reads."""
        bam = self.dir / "mid.bam"
        if not bam.exists():
            make_data.write_bam(self.dir, depth=1, seed=1, length=300_000, ref="ref.fa", name="mid.bam")
        return bam

    def test_seqpeeq_shows_the_reads_of_the_igv_window(self):
        # 300 kb opens zoomed out past the reads, so seqpeeq must be taken
        # to the window IGV is sent to before its reads are drawn.
        bam = self.mid_bam()
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

    def test_tview_shows_the_window(self):
        bam = self.mid_bam()
        argv = run.commands("tview", bam, self.scratch("tv"))
        self.assertIn("c1:1", argv)
        self.assertEqual(argv[-1], str(run._reference(bam).resolve()))
        got = run.measure(argv, timeout=30, ready=run._ready_for("tview", "bam"))
        self.assertEqual(got.outcome, "ok", got.detail)

    def test_asciigenome_runs_java_itself(self):
        scratch = self.scratch("ag").resolve()
        argv = run.commands("asciigenome", self.mid_bam(), scratch)
        self.assertEqual(argv[0], run._java(run.BENCH_ASCII))
        self.assertIn(run.MEMORY, argv)
        self.assertIn(f"-Duser.home={scratch}", argv)
        self.assertEqual(argv[argv.index("-r") + 1], run.WINDOW)

    def test_asciigenome_never_uses_the_home_folder(self):
        def snapshot():
            return {path: path.read_bytes() for path in Path.home().glob(".asciigenome*") if path.is_file()}

        before = snapshot()
        argv = run.commands("asciigenome", self.mid_bam(), self.scratch("agh"))
        got = run.measure(argv, timeout=120, ready=run._ready_for("asciigenome", "bam"),
                          quit=run.QUIT.get("asciigenome", b"q"))
        self.assertEqual(got.outcome, "ok", got.detail)
        self.assertEqual(snapshot(), before)

    def test_asciigenome_quits_when_asked(self):
        # Not killed after the harness's 10 s wait.
        argv = run.commands("asciigenome", self.dir / "tiny.bam", self.scratch("agq"))
        started = time.monotonic()
        got = run.measure(argv, timeout=120, ready=run._ready_for("asciigenome", "bam"),
                          quit=run.QUIT.get("asciigenome", b"q"))
        self.assertEqual(got.outcome, "ok", got.detail)
        self.assertLess(time.monotonic() - started - got.seconds, 3)

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
