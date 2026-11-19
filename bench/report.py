"""Turn bench/results.json and bench/seqpeeq_only.json into docs/benchmarks.md.

    python bench/report.py --results bench/results.json \\
        --seqpeeq-only bench/seqpeeq_only.json --out docs/benchmarks.md
"""

import argparse
import json
from pathlib import Path

KINDS = [("msa", "Alignment"), ("tree", "Tree"), ("bam", "BAM")]
NAMES = {"seqpeeq": "seqpeeq", "jalview": "Jalview", "figtree": "FigTree", "igv": "IGV"}
FILES = {
    "tiny.fa": "start-up: 5 × 20", "aln_1k.fa": "1,000 × 1,000", "aln_10k.fa": "10,000 × 10,000",
    "aln_30k.fa": "30,000 × 30,000", "aln_100k.fa": "100,000 × 30,000",
    "tiny.nwk": "start-up: 5 tips", "tree_1k.nwk": "1,000 tips", "tree_10k.nwk": "10,000 tips",
    "tree_100k.nwk": "100,000 tips", "tree_1m.nwk": "1,000,000 tips",
    "tiny.bam": "start-up: 2 kb, 2×", "reads_10x.bam": "1 Mb, 10×", "reads_100x.bam": "1 Mb, 100×",
    "reads_1000x.bam": "1 Mb, 1,000×",
}


def format_seconds(s):
    if s < 0.01:
        return f"{s * 1000:.1f} ms"
    if s < 0.1:
        return f"{s * 1000:.0f} ms"
    if s < 10:
        return f"{s:.2f} s"
    if s < 100:
        return f"{s:.0f} s"
    return f"{s / 60:.1f} min"


def format_bytes(b):
    if b < 1000:
        return f"{b} B"
    if b < 1_000_000:
        return f"{b / 1e3:.0f} kB"
    if b < 1_000_000_000:
        return f"{b / 1e6:.0f} MB"
    return f"{b / 1e9:.1f} GB"


def _cell(result, timeout, notes):
    if result is None:
        return "–"
    if result["outcome"] == "ok":
        return f"{format_seconds(result['median_seconds'])} · {format_bytes(result['max_rss'])}"
    label = {"timed out": f"timed out ({timeout / 60:g} min)"}.get(result["outcome"], result["outcome"])
    detail = " ".join(result["runs"][-1].get("detail", "").split()) or result["outcome"]
    notes.append(f"{NAMES[result['tool']]} on `{result['file']}`: {detail}".replace("|", "\\|"))
    return f"{label}[^{len(notes)}]"


def tables(results, notes):
    out = []
    timeout = results.get("timeout_seconds", 600)
    for kind, title in KINDS:
        rows = [r for r in results["results"] if r["kind"] == kind]
        if not rows:
            continue
        tools = list(dict.fromkeys(r["tool"] for r in rows))
        files = list(dict.fromkeys(r["file"] for r in rows))
        out.append(f"| {title} | File | " + " | ".join(NAMES[t] for t in tools) + " |")
        out.append("|---|---:|" + "---:|" * len(tools))
        for file in files:
            got = {r["tool"]: r for r in rows if r["file"] == file}
            size = next(r["file_bytes"] for r in rows if r["file"] == file)
            cells = [_cell(got.get(t), timeout, notes) for t in tools]
            out.append(f"| {FILES.get(file, file)} | {format_bytes(size)} | " + " | ".join(cells) + " |")
        out.append("")
    return out


def only_tables(only):
    out = []
    for key, title in [("alignment", "On the 100,000 × 30,000 alignment (3.0 GB)"),
                       ("tree", "On the 1,000,000-tip tree")]:
        if key not in only:
            continue
        out += [f"**{title}:**", "", "| Step | Time |", "|---|---:|"]
        out += [f"| {step} | {format_seconds(s)} |" for step, s in only[key].items()]
        out.append("")
    return out


def page(results, only):
    machine, versions = results["machine"], results["versions"]
    timeout = results.get("timeout_seconds", 600)
    memory = results.get("memory_limit", "16g").upper().replace("G", " GB")
    notes = []
    lines = [
        "# Benchmarks",
        "",
        "How long seqpeeq takes to open a file and show it, and how much memory it uses, beside the tools it",
        "stands in for: Jalview for alignments, FigTree for trees and IGV for BAMs. The files are random but",
        "seeded, from 1,000 to 100,000 sequences, 1,000 to 1,000,000 tips, and 10× to 1,000× read depth.",
        "",
        "seqpeeq does things these tools don't, and they do things seqpeeq doesn't. The comparison shows only",
        "what opening the same file costs in each.",
        "",
        "## Machine",
        "",
        f"- {machine['chip']}, {machine['cores']} cores, {machine['memory_bytes'] / 2**30:.0f} GB of memory,"
        f" {machine['os']}.",
        "- Versions: " + ", ".join(f"{NAMES.get(name, name)} {v}" for name, v in versions.items()) + ".",
        "",
        "## Method",
        "",
        "**What is timed:** the wall time from launch until the tool has shown the file, and the peak",
        "memory (the largest resident size the operating system reports for the process).",
        "",
        "- **Jalview** runs headless with `--open FILE --close`: it reads the alignment in and exits. It is",
        "  timed to its exit. It saves no image, because `--image`",
        "  draws the whole alignment shrunk onto one picture, far more work than its window does, while",
        "  seqpeeq draws one screen. Skipping the drawing favours Jalview slightly.",
        "- **FigTree** draws the tree to a 1200 × 800 PNG (`-graphic PNG`) and exits. It is timed to its exit.",
        f"- **IGV** runs a batch script: load the reference and the BAM, go to `{WINDOW_TEXT}`, save a",
        "  snapshot, exit. It is timed to its exit.",
        "- **seqpeeq** runs in a hidden 160 × 48 terminal, and the screen is read as it is drawn:",
        "  - for an alignment or a tree, it is timed until the first screen of the file is drawn;",
        f"  - a BAM opens on the whole contig, so it is sent `g {WINDOW_TEXT}` (the window IGV is sent to) and",
        "    timed until that window's reads have been read and drawn.",
        "",
        "  It is then sent `q`.",
        "",
        "**Fairness:**",
        "",
        f"- **Memory:** each Java tool is given {memory} (`-Xmx`). Each runs on Java from its own conda",
        "  environment: Jalview 2.11.5 needs Java below 12 and IGV 2.19 needs Java 21.",
        "- **IGV's settings:** IGV runs with its home folder set to a scratch folder, so it never reads or",
        "  writes anyone's `~/igv`. Its preferences there make the benchmark reference the default genome, so",
        "  it does not download a human genome when it starts.",
        "- **Jalview** is started by its own main class. The jar's entry point is a launcher that starts",
        "  Jalview in a second Java process and exits at once, so timing it would time only the launcher.",
        "  Any run that exits while processes it started are still running is recorded as a failure.",
        "- **Start-up:** the first row of each table is a tiny file, so it shows each tool's start-up cost,",
        "  most of it the Java virtual machine's for the three Java tools.",
        "- **Repeats:** each number is the median of 3 runs, or of 1 run when the first takes over a minute.",
        f"- **Limits:** each run stops after {timeout / 60:g} minutes. A tool that runs out of memory, crashes",
        "  or hits the limit is recorded as such, with the reason in a note.",
        "- **Build:** seqpeeq is built with `--release`.",
        "",
        "## Results",
        "",
        "Each cell is the median time of its runs and the highest peak memory among them.",
        "",
    ]
    lines += tables(results, notes)
    lines += [
        "## seqpeeq only",
        "",
        "Steps with no counterpart above, timed inside seqpeeq (`bench_timings_on_the_benchmark_files`).",
        "Each is timed to the frame that shows it. Rows marked *background* are work seqpeeq hands to a",
        "worker thread while the screen stays live: the column counts behind its colouring, and the summary",
        "behind the zoomed-out overview.",
        "",
    ]
    lines += [line.replace("| background: ", "| *background:* ") for line in only_tables(only)]
    lines += [
        "## What the numbers do and don't show",
        "",
        "- **Opening is not using.** A script that opens a file and exits is not someone scrolling, zooming",
        "  and selecting in a window. The numbers say how soon each tool can show a file, not how it feels",
        "  to work in.",
        "- **seqpeeq maps an alignment rather than copying it.** Its resident memory counts the pages of the",
        "  file it has read, which the operating system can drop and reread. That is why its memory on a",
        "  large alignment is near the file's size, though it never holds a copy.",
        "- **seqpeeq reads a BAM by region.** It reads the window in view, through the index, and a sketch of",
        "  the whole contig in the background. IGV also reads by region. Neither number says how long a",
        "  tool takes to read every read.",
        "- **The file cache.** Each tool's first run of a file may read it from disk; later runs find it",
        "  in memory, so a median of three is a cached run. Runs over a minute are run once and may be",
        "  uncached. Here, seqpeeq's first, uncached open of the 3 GB alignment took 5.4 s against 1.4 s.",
        "- **IGV's start-up varies.** Its runs took either about 1.3 s or about 3.4 s, on any file, with",
        "  nothing in its log to tell them apart. Where two of a cell's three runs were slow, its median is",
        "  the slow one. `results.json` has every run.",
        "- **The start-up rows** are most of the Java tools' times on the smaller files.",
        "- **One machine.** These numbers come from one laptop. The scripts are in `bench/` to rerun anywhere.",
        "",
        "## How to rerun",
        "",
        "```sh",
        "conda env create -f bench/env.yml        # FigTree, Jalview, samtools, numpy, pyte",
        "conda env create -f bench/env-igv.yml    # IGV, with Java 21",
        "cargo build --release -p seqpeeq",
        "PY=$(conda run -n seqpeeq-bench which python)",
        "$PY bench/make_data.py --out bench/data --only all          # about 4 GB",
        "$PY bench/run.py --data bench/data --out bench/results.json --only all",
        "SEQPEEQ_BENCH_DATA=$PWD/bench/data SEQPEEQ_BENCH_OUT=$PWD/bench/seqpeeq_only.json \\",
        "    cargo test --release -p seqpeeq -- --ignored bench_timings",
        "$PY bench/report.py --results bench/results.json --seqpeeq-only bench/seqpeeq_only.json \\",
        "    --out docs/benchmarks.md",
        "```",
        "",
        "`run.py` looks for the two environments in `~/miniconda3/envs`; set `SEQPEEQ_BENCH_ENVS` to the",
        "folder that holds them if they are elsewhere.",
    ]
    if notes:
        lines.append("")
        lines += [f"[^{i}]: {note}" for i, note in enumerate(notes, 1)]
    return "\n".join(lines) + "\n"


WINDOW_TEXT = "c1:1-1000"


def main(argv=None):
    args = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    args.add_argument("--results", required=True, type=Path)
    args.add_argument("--seqpeeq-only", required=True, type=Path)
    args.add_argument("--out", required=True, type=Path)
    args = args.parse_args(argv)
    results = json.loads(args.results.read_text())
    only = json.loads(args.seqpeeq_only.read_text())
    args.out.write_text(page(results, only))
    print(f"wrote {args.out}")


if __name__ == "__main__":
    main()
