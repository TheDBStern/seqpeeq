"""Make the benchmark's test files: alignments, trees and BAMs of rising size.

Every file comes from a fixed seed, so the same command makes the same bytes.
Files that already exist are kept unless --force is given.

    python bench/make_data.py --out bench/data --only all
"""

import argparse
import shutil
import subprocess
import sys
from pathlib import Path

import numpy as np

ALIGNMENTS = {"aln_1k.fa": (1_000, 1_000), "aln_10k.fa": (10_000, 10_000),
              "aln_30k.fa": (30_000, 30_000), "aln_100k.fa": (100_000, 30_000)}
TREES = {"tree_1k.nwk": 1_000, "tree_10k.nwk": 10_000,
         "tree_100k.nwk": 100_000, "tree_1m.nwk": 1_000_000}
DEPTHS = [10, 100, 1000]
READ_LENGTH = 150
CONTIG = "c1"
GAPS = 0.05


def samtools():
    """samtools from the environment this Python runs in, else from PATH."""
    beside = Path(sys.executable).parent / "samtools"
    found = str(beside) if beside.exists() else shutil.which("samtools")
    if not found:
        raise SystemExit("samtools not found; run from the seqpeeq-bench environment")
    return found


def _residues(rng, rows, cols):
    """Random DNA with about 5% gaps, as bytes, one row per array row."""
    alphabet = np.frombuffer(b"ACGT-", dtype=np.uint8)
    p = [(1 - GAPS) / 4] * 4 + [GAPS]
    return alphabet[rng.choice(5, size=(rows, cols), p=p)]


def write_alignment(path, rows, cols, seed):
    """An aligned FASTA of `rows` sequences s000001… of `cols` columns."""
    rng = np.random.default_rng(seed)
    chunk = max(1, 30_000_000 // cols)
    with open(path, "wb") as out:
        for start in range(0, rows, chunk):
            block = _residues(rng, min(chunk, rows - start), cols)
            for i, row in enumerate(block, start + 1):
                out.write(f">s{i:06d}\n".encode())
                out.write(row.tobytes())
                out.write(b"\n")


def write_tree(path, tips, seed):
    """A Newick tree of tips t1…tN, made by joining random pairs of subtrees.

    Node i < tips is tip t{i+1}; later nodes are joins. Both the joining and
    the writing are iterative, so a million tips need no deep recursion.
    """
    rng = np.random.default_rng(seed)
    nodes = 2 * tips - 1
    children = np.full((nodes, 2), -1, dtype=np.int64)
    lengths = rng.exponential(0.1, size=nodes)
    active = list(range(tips))
    picks = rng.random((tips - 1, 2))
    for k, (a, b) in enumerate(picks):
        pair = []
        for u in (a, b):
            i = int(u * len(active))
            active[i], active[-1] = active[-1], active[i]
            pair.append(active.pop())
        node = tips + k
        children[node] = pair
        active.append(node)
    root = active[0]
    parts = []
    stack = [(root, 0)]
    while stack:
        node, state = stack.pop()
        if node < tips:
            parts.append(f"t{node + 1}:{lengths[node]:.4f}")
        elif state == 0:
            parts.append("(")
            stack.append((node, 1))
            stack.append((int(children[node][0]), 0))
        elif state == 1:
            parts.append(",")
            stack.append((node, 2))
            stack.append((int(children[node][1]), 0))
        else:
            parts.append(")" if node == root else f"):{lengths[node]:.4f}")
    parts.append(";\n")
    Path(path).write_text("".join(parts))


def write_reference(path, length, seed):
    rng = np.random.default_rng(seed)
    bases = np.frombuffer(b"ACGT", dtype=np.uint8)[rng.integers(0, 4, size=length)]
    text = bases.tobytes().decode()
    with open(path, "w") as out:
        out.write(f">{CONTIG}\n")
        for i in range(0, length, 60):
            out.write(text[i:i + 60] + "\n")
    subprocess.run([samtools(), "faidx", str(path)], check=True)
    return text


def write_bam(dir, depth, seed, length=1_000_000, ref="ref.fa", name=None):
    """Reads of 150 bp at `depth`× over one contig, sorted and indexed.

    The reference (`ref`, with its .fai) is written once and reused. Returns
    the BAM's path: `reads_{depth}x.bam` unless `name` is given.
    """
    dir = Path(dir)
    ref_path = dir / ref
    if ref_path.exists() and Path(f"{ref_path}.fai").exists():
        text = "".join(l.strip() for l in ref_path.read_text().splitlines()[1:])
    else:
        text = write_reference(ref_path, length, seed)
    bam = dir / (name or f"reads_{depth}x.bam")
    rng = np.random.default_rng(seed + depth)
    count = depth * length // READ_LENGTH
    starts = np.sort(rng.integers(0, length - READ_LENGTH + 1, size=count))
    reverse = rng.random(count) < 0.5
    qual = "I" * READ_LENGTH
    sort = subprocess.Popen([samtools(), "sort", "-o", str(bam), "-"],
                            stdin=subprocess.PIPE, text=True)
    sort.stdin.write(f"@HD\tVN:1.6\tSO:unsorted\n@SQ\tSN:{CONTIG}\tLN:{length}\n")
    for i, (start, rev) in enumerate(zip(starts, reverse)):
        start = int(start)
        seq = text[start:start + READ_LENGTH]
        sort.stdin.write(f"r{i + 1}\t{16 if rev else 0}\t{CONTIG}\t{start + 1}\t60\t"
                         f"{READ_LENGTH}M\t*\t0\t0\t{seq}\t{qual}\n")
    sort.stdin.close()
    if sort.wait() != 0:
        raise SystemExit(f"samtools sort failed for {bam}")
    subprocess.run([samtools(), "index", str(bam)], check=True)
    return bam


def plan(only):
    """(file name, function making it) for each file in the set `only`."""
    jobs = [("tiny.fa", lambda d: write_alignment(d / "tiny.fa", 5, 20, 1)),
            ("tiny.nwk", lambda d: write_tree(d / "tiny.nwk", 5, 1)),
            ("tiny.bam", lambda d: write_bam(d, 2, 1, length=2000, ref="tiny_ref.fa",
                                             name="tiny.bam"))]
    if only == "tiny":
        return jobs
    alns = list(ALIGNMENTS.items())
    trees = list(TREES.items())
    depths = DEPTHS
    if only == "small":
        alns, trees, depths = alns[:1], trees[:1], depths[:1]
    for name, (rows, cols) in alns:
        jobs.append((name, lambda d, n=name, r=rows, c=cols: write_alignment(d / n, r, c, 1)))
    for name, tips in trees:
        jobs.append((name, lambda d, n=name, t=tips: write_tree(d / n, t, 1)))
    for depth in depths:
        jobs.append((f"reads_{depth}x.bam", lambda d, x=depth: write_bam(d, x, 1)))
    return jobs


def main(argv=None):
    args = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    args.add_argument("--out", required=True, type=Path)
    args.add_argument("--only", choices=["tiny", "small", "all"], default="all")
    args.add_argument("--force", action="store_true", help="make files that already exist again")
    args = args.parse_args(argv)
    args.out.mkdir(parents=True, exist_ok=True)
    for name, make in plan(args.only):
        path = args.out / name
        if path.exists() and not args.force:
            print(f"kept {path}")
            continue
        if args.force and name.endswith(".bam"):
            # The reference comes from the same seed, so an existing one is reused.
            for stale in [path, Path(f"{path}.bai")]:
                stale.unlink(missing_ok=True)
        make(args.out)
        print(f"made {path} ({path.stat().st_size:,} bytes)", flush=True)


if __name__ == "__main__":
    main()
