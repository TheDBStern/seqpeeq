"""Turn bench/results.json, bench/seqpeeq_only.json and bench/cold.json into docs/benchmarks.md.

    python bench/report.py --results bench/results.json \\
        --seqpeeq-only bench/seqpeeq_only.json --cold bench/cold.json --out docs/benchmarks.md
"""

import argparse
import json
from pathlib import Path

KINDS = [("msa", "Alignment"), ("tree", "Tree"), ("bam", "BAM")]
NAMES = {"seqpeeq": "seqpeeq", "termal": "termal", "alv": "alv", "tview": "samtools tview", "asciigenome": "ASCIIGenome",
         "samtools": "samtools", "jalview": "Jalview", "figtree": "FigTree", "igv": "IGV"}
# The columns of each table: seqpeeq, then the terminal tools, then the GUIs.
ORDER = ["seqpeeq", "termal", "alv", "tview", "asciigenome", "jalview", "figtree", "igv"]
# The tools run on Java, which grow toward the heap limit they are given.
JAVA = {"jalview", "figtree", "igv", "asciigenome"}
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


def _heap_bytes(limit):
    return int(limit.lower().removesuffix("g")) * 2**30


def _cell(result, timeout, notes, limit="16g"):
    if result is None:
        return "–"
    if result["outcome"] == "ok":
        cell = f"{format_seconds(result['median_seconds'])} · {format_bytes(result['max_rss'])}"
        if result["tool"] in JAVA and result["max_rss"] >= 0.95 * _heap_bytes(limit):
            notes.append(f"{NAMES[result['tool']]} on `{result['file']}` reached its "
                         f"{limit.upper().replace('G', ' GB')} heap limit. A Java program's memory grows "
                         "toward the limit it is given, so this is not what it needs, and its time "
                         "likely includes collecting garbage near the limit.")
            cell += f"[^{len(notes)}]"
        return cell
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
        tools = sorted(dict.fromkeys(r["tool"] for r in rows), key=ORDER.index)
        files = list(dict.fromkeys(r["file"] for r in rows))
        out.append(f"| {title} | File | " + " | ".join(NAMES[t] for t in tools) + " |")
        out.append("|---|---:|" + "---:|" * len(tools))
        for file in files:
            got = {r["tool"]: r for r in rows if r["file"] == file}
            size = next(r["file_bytes"] for r in rows if r["file"] == file)
            cells = [_cell(got.get(t), timeout, notes, results.get("memory_limit", "16g"))
                     for t in tools]
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


def cold_table(results, cold):
    """seqpeeq's times from disk, each beside its time with the file in memory."""
    warm = {r["file"]: r for r in results["results"] if r["tool"] == "seqpeeq"}
    out = ["### From disk", "",
           "seqpeeq on the largest files with the file cache emptied (`sudo purge`) before each run, so each",
           "run reads its file from disk. This is mostly the time to read the file, which every tool would",
           "add, so only seqpeeq is timed this way.", "",
           "| File | Size | From disk | In memory |", "|---|---:|---:|---:|"]
    for r in cold["results"]:
        in_memory = warm.get(r["file"], {}).get("median_seconds")
        cells = [format_seconds(s) if s is not None else r["outcome"]
                 for s in (r["median_seconds"], in_memory)]
        out.append(f"| {FILES.get(r['file'], r['file'])} | {format_bytes(r['file_bytes'])} | "
                   + " | ".join(cells) + " |")
    out.append("")
    return out


def page(results, only, cold=None):
    machine, versions = results["machine"], results["versions"]
    timeout = results.get("timeout_seconds", 600)
    memory = results.get("memory_limit", "16g").upper().replace("G", " GB")
    notes = []
    termal = versions.get("termal", "").split()[:1]
    termal_install = "cargo install termal-msa" + "".join(f"@{v}" for v in termal)
    lines = [
        "# Benchmarks",
        "",
        "Comparing times for opening files. seqpeeq is compared with other terminal viewers (termal and alv",
        "for alignments, samtools tview and ASCIIGenome for BAMs) and with GUI programs (Jalview for alignments,",
        "FigTree for trees and IGV for BAMs). To my knowledge, there is no interactive terminal tree viewer to compare with,",
        "so trees are compared with FigTree only.",
        "",
        "The files are random but seeded, from 1,000 to 100,000 sequences, 1,000 to 1,000,000 tips, and 10×",
        "to 1,000× read depth.",
        "",
        "## Machine",
        "",
        f"- {machine['chip']}, {machine['cores']} cores, {machine['memory_bytes'] / 2**30:.0f} GB of memory,"
        f" {machine['os']}.",
        "- Versions: " + ", ".join(f"{NAMES.get(name, name)} {v}" for name, v in versions.items()) + ".",
        "",
        "## Method",
        "",
        "**Timing:** the wall time from launch until the tool has shown the file and the peak",
        "memory (the largest resident size the operating system reports for the process).",
        "",
        "The terminal tools run in a hidden 160 × 48 terminal, and the screen is read as it is drawn. Each",
        "is timed until the file is on screen, then sent `q` to quit.",
        "",
        "- **seqpeeq**:",
        "  - for an alignment or a tree, it is timed until the first screen of the file is drawn;",
        f"  - a BAM opens on the whole contig, so it is sent `g {WINDOW_TEXT}` (the window IGV is sent to) and",
        "    timed until that window's reads have been read and drawn.",
        "- **termal** is timed until its first screen is drawn, down to the help line at its bottom. It",
        "  draws nothing until it has read the whole file, so its time includes reading all of it.",
        "- **alv** prints the whole alignment and exits, so it is timed the way it is used, piped into",
        "  `less -R`, until less has drawn its first screen. alv reads the whole file before it prints,",
        "  so its time includes reading all of it.",
        f"- **samtools tview** is opened at `{WINDOW_TEXT.split('-')[0]}`, the start of IGV's window, with the reference. It is",
        "  timed until the reads under its ruler and reference are drawn. It shows one screen's width of",
        "  bases, one to a column.",
        f"- **ASCIIGenome** is opened on `{WINDOW_TEXT}` with the reference, and timed until the line naming",
        "  that window is drawn under the reads. Its prompt takes commands, so it is sent `q` and Enter.",
        "",
        "The GUI programs are run in their script modes and timed until they exit:",
        "",
        "- **Jalview** is run headless with `--open FILE --close`: it reads the alignment in and exits. It is",
        "  timed to its exit. It doesn't save an image, because `--image`",
        "  draws the whole alignment shrunk onto one picture, which is a lot of work.",
        "- **FigTree** draws the tree to a 1200 × 800 PNG (`-graphic PNG`) and exits. It is timed to its exit.",
        f"- **IGV** runs a batch script: load the reference and the BAM, go to `{WINDOW_TEXT}`, save a",
        "  snapshot, exit. It is timed to its exit.",
        "",
        "**Fairness:**",
        "",
        f"- **Memory:** each Java tool is given {memory} (`-Xmx`). Each runs on Java from its own conda",
        "  environment: Jalview 2.11.5 needs Java below 12, IGV 2.19 needs Java 21 and ASCIIGenome 1.21",
        "  needs Java 17 or later.",
        "- **IGV's settings:** IGV runs with its home folder set to a scratch folder, so it never reads or",
        "  writes anyone's `~/igv`. Its preferences there make the benchmark reference the default genome, so",
        "  it does not download a human genome when it starts.",
        "- **ASCIIGenome** runs its jar on Java directly, not through Bioconda's wrapper script, so the",
        f"  Java process is the one measured and the {memory} limit applies. Its home folder is a scratch folder",
        "  too, so its command history never touches anyone's home.",
        "- **Jalview** is started by its own main class. The jar's entry point is a launcher that starts",
        "  Jalview in a second Java process and exits at once, so timing it would time only the launcher.",
        "  Any run that exits while processes it started are still running is recorded as a failure.",
        "- **Start-up:** the first row of each table is a tiny file, so it shows each tool's start-up cost,",
        "  most of it the Java virtual machine's for the four Java tools.",
        "- **Repeats:** each number is the median of 3 runs, or of 1 run when the first takes over a minute.",
        f"- **Limits:** each run stops after {timeout / 60:g} minutes. If a tool runs out of memory, crashes",
        "  or hits the limit, that's recorded as such, with the reason in a note.",
        "- **Build:** seqpeeq is built with `--release` and termal with `cargo install`, both with Cargo's",
        "  default release settings and for the machine's own CPU. All eight tools run natively.",
        "",
        "## Results",
        "",
        "Each cell is the median time of its runs and the highest peak memory among them.",
        "The times are for files already in memory, which the operating system keeps after a file is read",
        "and the repeated runs hit: a first open from disk is slower.",
        "",
    ]
    lines += tables(results, notes)
    if cold:
        lines += cold_table(results, cold)
    lines += [
        "## seqpeeq only",
        "",
        "seqpeeq-unique features timed inside seqpeeq (`bench_timings_on_the_benchmark_files`).",
        "Each is timed to the frame that shows it. Rows marked *background* are work seqpeeq hands to a",
        "worker thread while the screen stays live: the column counts behind its colouring, and the summary",
        "behind the zoomed-out overview.",
        "",
    ]
    lines += [line.replace("| background: ", "| *background:* ") for line in only_tables(only)]
    lines += [
        "## How to rerun",
        "",
        "```sh",
        "conda env create -f bench/env.yml        # FigTree, Jalview, samtools, alv, numpy, pyte",
        "conda env create -f bench/env-igv.yml    # IGV, with Java 21",
        "conda env create -f bench/env-asciigenome.yml    # ASCIIGenome, with Java 17 or later",
        f"{termal_install} --locked --root bench/tools",
        "cargo build --release -p seqpeeq     # on Apple silicon, with an aarch64-apple-darwin toolchain",
        "PY=$(conda run -n seqpeeq-bench which python)",
        "$PY bench/make_data.py --out bench/data --only all          # about 4 GB",
        "$PY bench/run.py --data bench/data --out bench/results.json --only all",
        "SEQPEEQ_BENCH_DATA=$PWD/bench/data SEQPEEQ_BENCH_OUT=$PWD/bench/seqpeeq_only.json \\",
        "    cargo test --release -p seqpeeq -- --ignored bench_timings",
        "sudo -v && $PY bench/run.py --data bench/data --out bench/cold.json --cold",
        "$PY bench/report.py --results bench/results.json --seqpeeq-only bench/seqpeeq_only.json \\",
        "    --cold bench/cold.json --out docs/benchmarks.md",
        "```",
        "",
        "`run.py` looks for the three environments in `~/miniconda3/envs`; set `SEQPEEQ_BENCH_ENVS` to the",
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
    args.add_argument("--cold", type=Path)
    args.add_argument("--out", required=True, type=Path)
    args = args.parse_args(argv)
    results = json.loads(args.results.read_text())
    only = json.loads(args.seqpeeq_only.read_text())
    cold = json.loads(args.cold.read_text()) if args.cold else None
    args.out.write_text(page(results, only, cold))
    print(f"wrote {args.out}")


if __name__ == "__main__":
    main()
