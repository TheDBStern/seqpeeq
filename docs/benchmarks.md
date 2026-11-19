# Benchmarks

How long seqpeeq takes to open a file and show it, and how much memory it uses, beside the tools it
stands in for: Jalview for alignments, FigTree for trees and IGV for BAMs. The files are random but
seeded, from 1,000 to 100,000 sequences, 1,000 to 1,000,000 tips, and 10× to 1,000× read depth.

seqpeeq does things these tools don't, and they do things seqpeeq doesn't. The comparison shows only
what opening the same file costs in each.

## Machine

- Apple M2, 8 cores, 24 GB of memory, macOS 26.6.2.
- Versions: seqpeeq 0.1.0, FigTree 1.4.4, Jalview 2.11.5.1, java (FigTree, Jalview) 11.0.30, IGV 2.19.8, java (IGV) 21.0.13.1.

## Method

**What is timed:** the wall time from launch until the tool has shown the file, and the peak
memory (the largest resident size the operating system reports for the process).

- **Jalview** runs headless with `--open FILE --close`: it reads the alignment in and exits. It is
  timed to its exit. It saves no image, because `--image`
  draws the whole alignment shrunk onto one picture, far more work than its window does, while
  seqpeeq draws one screen. Skipping the drawing favours Jalview slightly.
- **FigTree** draws the tree to a 1200 × 800 PNG (`-graphic PNG`) and exits. It is timed to its exit.
- **IGV** runs a batch script: load the reference and the BAM, go to `c1:1-1000`, save a
  snapshot, exit. It is timed to its exit.
- **seqpeeq** runs in a hidden 160 × 48 terminal, and the screen is read as it is drawn:
  - for an alignment or a tree, it is timed until the first screen of the file is drawn;
  - a BAM opens on the whole contig, so it is sent `g c1:1-1000` (the window IGV is sent to) and
    timed until that window's reads have been read and drawn.

  It is then sent `q`.

**Fairness:**

- **Memory:** each Java tool is given 16 GB (`-Xmx`). Each runs on Java from its own conda
  environment: Jalview 2.11.5 needs Java below 12 and IGV 2.19 needs Java 21.
- **IGV's settings:** IGV runs with its home folder set to a scratch folder, so it never reads or
  writes anyone's `~/igv`. Its preferences there make the benchmark reference the default genome, so
  it does not download a human genome when it starts.
- **Jalview** is started by its own main class. The jar's entry point is a launcher that starts
  Jalview in a second Java process and exits at once, so timing it would time only the launcher.
  Any run that exits while processes it started are still running is recorded as a failure.
- **Start-up:** the first row of each table is a tiny file, so it shows each tool's start-up cost,
  most of it the Java virtual machine's for the three Java tools.
- **Repeats:** each number is the median of 3 runs, or of 1 run when the first takes over a minute.
- **Limits:** each run stops after 10 minutes. A tool that runs out of memory, crashes
  or hits the limit is recorded as such, with the reason in a note.
- **Build:** seqpeeq is built with `--release`.

## Results

Each cell is the median time of its runs and the highest peak memory among them.

| Alignment | File | seqpeeq | Jalview |
|---|---:|---:|---:|
| start-up: 5 × 20 | 150 B | 69 ms · 7 MB | 1.34 s · 279 MB |
| 1,000 × 1,000 | 1 MB | 88 ms · 8 MB | 1.48 s · 312 MB |
| 10,000 × 10,000 | 100 MB | 83 ms · 108 MB | 6.48 s · 1.4 GB |
| 30,000 × 30,000 | 900 MB | 0.30 s · 912 MB | 1.8 min · 6.9 GB |
| 100,000 × 30,000 | 3.0 GB | 1.44 s · 3.0 GB | 7.3 min · 16.4 GB |

| Tree | File | seqpeeq | FigTree |
|---|---:|---:|---:|
| start-up: 5 tips | 80 B | 76 ms · 7 MB | 1.15 s · 229 MB |
| 1,000 tips | 21 kB | 84 ms · 7 MB | 1.25 s · 253 MB |
| 10,000 tips | 219 kB | 87 ms · 10 MB | 1.63 s · 438 MB |
| 100,000 tips | 2 MB | 80 ms · 32 MB | 3.91 s · 2.3 GB |
| 1,000,000 tips | 24 MB | 0.60 s · 323 MB | 29 s · 12.5 GB |

| BAM | File | seqpeeq | IGV |
|---|---:|---:|---:|
| start-up: 2 kb, 2× | 1 kB | 0.14 s · 7 MB | 3.40 s · 316 MB |
| 1 Mb, 10× | 1 MB | 0.14 s · 8 MB | 3.35 s · 347 MB |
| 1 Mb, 100× | 8 MB | 0.16 s · 9 MB | 1.42 s · 357 MB |
| 1 Mb, 1,000× | 44 MB | 0.17 s · 19 MB | 1.60 s · 385 MB |

## seqpeeq only

Steps with no counterpart above, timed inside seqpeeq (`bench_timings_on_the_benchmark_files`).
Each is timed to the frame that shows it. Rows marked *background* are work seqpeeq hands to a
worker thread while the screen stays live: the column counts behind its colouring, and the summary
behind the zoomed-out overview.

**On the 100,000 × 30,000 alignment (3.0 GB):**

| Step | Time |
|---|---:|
| open and first frame | 1.44 s |
| *background:* count the columns | 6.18 s |
| frame after PageDown | 0.9 ms |
| frame after a zoom-out step | 5.2 ms |
| *background:* summary for the overview | 1.02 s |
| delete 1,000 columns | 1.4 ms |
| *background:* count the columns again | 5.65 s |
| delete 1,000 rows | 1.4 ms |
| search names | 13 ms |

**On the 1,000,000-tip tree:**

| Step | Time |
|---|---:|
| open and first frame | 0.55 s |
| frame after PageDown | 0.7 ms |
| prune a clade | 0.15 s |
| tip search | 58 ms |

## What the numbers do and don't show

- **Opening is not using.** A script that opens a file and exits is not someone scrolling, zooming
  and selecting in a window. The numbers say how soon each tool can show a file, not how it feels
  to work in.
- **seqpeeq maps an alignment rather than copying it.** Its resident memory counts the pages of the
  file it has read, which the operating system can drop and reread. That is why its memory on a
  large alignment is near the file's size, though it never holds a copy.
- **seqpeeq reads a BAM by region.** It reads the window in view, through the index, and a sketch of
  the whole contig in the background. IGV also reads by region. Neither number says how long a
  tool takes to read every read.
- **The file cache.** Each tool's first run of a file may read it from disk; later runs find it
  in memory, so a median of three is a cached run. Runs over a minute are run once and may be
  uncached. Here, seqpeeq's first, uncached open of the 3 GB alignment took 5.4 s against 1.4 s.
- **IGV's start-up varies.** Its runs took either about 1.3 s or about 3.4 s, on any file, with
  nothing in its log to tell them apart. Where two of a cell's three runs were slow, its median is
  the slow one. `results.json` has every run.
- **The start-up rows** are most of the Java tools' times on the smaller files.
- **One machine.** These numbers come from one laptop. The scripts are in `bench/` to rerun anywhere.

## How to rerun

```sh
conda env create -f bench/env.yml        # FigTree, Jalview, samtools, numpy, pyte
conda env create -f bench/env-igv.yml    # IGV, with Java 21
cargo build --release -p seqpeeq
PY=$(conda run -n seqpeeq-bench which python)
$PY bench/make_data.py --out bench/data --only all          # about 4 GB
$PY bench/run.py --data bench/data --out bench/results.json --only all
SEQPEEQ_BENCH_DATA=$PWD/bench/data SEQPEEQ_BENCH_OUT=$PWD/bench/seqpeeq_only.json \
    cargo test --release -p seqpeeq -- --ignored bench_timings
$PY bench/report.py --results bench/results.json --seqpeeq-only bench/seqpeeq_only.json \
    --out docs/benchmarks.md
```

`run.py` looks for the two environments in `~/miniconda3/envs`; set `SEQPEEQ_BENCH_ENVS` to the
folder that holds them if they are elsewhere.
