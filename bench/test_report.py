"""Tests for report.py."""

import unittest

import report

MACHINE = {"chip": "Apple M9", "cores": 8, "memory_bytes": 24 * 2**30, "os": "macOS 99"}
VERSIONS = {"seqpeeq": "0.1.0", "jalview": "2.11.5.1", "igv": "2.19.8", "figtree": "1.4.4"}


def result(tool, kind, file, outcome, seconds, rss, detail=""):
    return {"tool": tool, "kind": kind, "file": file, "file_bytes": 1_000_000,
            "runs": [{"seconds": seconds, "max_rss": rss, "outcome": outcome, "detail": detail}],
            "outcome": outcome, "median_seconds": seconds if outcome == "ok" else None,
            "max_rss": rss}


RESULTS = {
    "machine": MACHINE, "versions": VERSIONS, "timeout_seconds": 600, "memory_limit": "16g",
    "results": [
        result("seqpeeq", "msa", "aln_1k.fa", "ok", 0.21, 9_000_000),
        result("jalview", "msa", "aln_1k.fa", "ok", 2.5, 412_000_000),
        result("seqpeeq", "msa", "aln_100k.fa", "ok", 1.9, 3_100_000_000),
        result("jalview", "msa", "aln_100k.fa", "out of memory", 95.0, 17_000_000_000,
               "java.lang.OutOfMemoryError: Java heap space"),
    ],
}
ONLY = {"alignment": {"open and first frame": 1.86, "delete 1,000 columns": 0.0011},
        "tree": {"prune a clade": 0.153}}


class Format(unittest.TestCase):
    def test_format_seconds(self):
        self.assertEqual(report.format_seconds(0.844), "0.84 s")
        self.assertEqual(report.format_seconds(9.994), "9.99 s")
        self.assertEqual(report.format_seconds(10.0), "10 s")
        self.assertEqual(report.format_seconds(12.4), "12 s")
        self.assertEqual(report.format_seconds(99.4), "99 s")
        self.assertEqual(report.format_seconds(100.0), "1.7 min")
        self.assertEqual(report.format_seconds(192.0), "3.2 min")

    def test_short_times_keep_their_digits(self):
        self.assertEqual(report.format_seconds(0.0011), "1.1 ms")
        self.assertEqual(report.format_seconds(0.053), "53 ms")

    def test_format_bytes(self):
        self.assertEqual(report.format_bytes(412_000_000), "412 MB")
        self.assertEqual(report.format_bytes(999_000_000), "999 MB")
        self.assertEqual(report.format_bytes(3_100_000_000), "3.1 GB")
        self.assertEqual(report.format_bytes(7_000_000), "7 MB")


class Page(unittest.TestCase):
    def setUp(self):
        self.page = report.page(RESULTS, ONLY)

    def test_a_table_per_kind_with_failures_marked(self):
        lines = self.page.splitlines()
        header = next(i for i, line in enumerate(lines) if line.startswith("| Alignment"))
        self.assertIn("seqpeeq", lines[header])
        self.assertIn("Jalview", lines[header])
        rows = [line for line in lines[header + 2:header + 10] if line.startswith("|")]
        self.assertEqual(len(rows), 2, rows)
        self.assertIn("0.21 s", rows[0])
        self.assertIn("412 MB", rows[0])
        self.assertIn("out of memory", rows[1])
        self.assertIn("Java heap space", self.page)

    def test_a_tool_at_its_heap_limit_is_marked(self):
        page = report.page({**RESULTS, "results": [
            result("seqpeeq", "msa", "aln_1k.fa", "ok", 0.21, 9_000_000),
            result("jalview", "msa", "aln_1k.fa", "ok", 436.0, 16_400_000_000)]}, ONLY)
        row = next(line for line in page.splitlines() if line.startswith("| 1,000 × 1,000"))
        self.assertIn("[^", row)
        self.assertIn("16 GB heap limit", page)

    def test_the_uncached_open_comes_from_the_runs(self):
        big = result("seqpeeq", "msa", "aln_100k.fa", "ok", 0.65, 3_000_000_000)
        big["runs"] = [{"seconds": s, "max_rss": 1, "outcome": "ok", "detail": ""}
                       for s in [4.567, 0.65, 0.65]]
        page = report.page({**RESULTS, "results": [big]}, ONLY)
        self.assertIn("first run of `aln_100k.fa`, read from disk, took 4.57 s against 0.65 s", page)

    def test_page_names_the_machine_and_versions(self):
        self.assertIn("Apple M9", self.page)
        for version in VERSIONS.values():
            self.assertIn(version, self.page)

    def test_seqpeeq_only_steps_are_listed(self):
        self.assertIn("delete 1,000 columns", self.page)
        self.assertIn("1.1 ms", self.page)
        self.assertIn("0.15 s", self.page)


if __name__ == "__main__":
    unittest.main()
