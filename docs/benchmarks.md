# Benchmarks

Comparing times for opening files. Jalview for alignments, FigTree for trees and IGV for BAMs. The files are random but
seeded, from 1,000 to 100,000 sequences, 1,000 to 1,000,000 tips, and 10× to 1,000× read depth.

## Machine

- Apple M2, 8 cores, 24 GB of memory, macOS 26.6.2.
- Versions: seqpeeq 0.1.0 (arm64), FigTree 1.4.4, Jalview 2.11.5.1, java (FigTree, Jalview) 11.0.30, IGV 2.19.8, java (IGV) 21.0.13.1.

## Method

**What is timed:** the wall time from launch until the tool has shown the file, and the peak
memory (the largest resident size the operating system reports for the process).

- **Jalview** is run headless with `--open FILE --close`: it reads the alignment in and exits. It is
  timed to its exit. It doesn't save an image, because `--image`
  draws the whole alignment shrunk onto one picture, which is a lot of work.
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
- **Limits:** each run stops after 10 minutes. If a tool runs out of memory, crashes
  or hits the limit, that's recorded as such, with the reason in a note.
- **Build:** seqpeeq is built with `--release`, for the machine's own CPU. All four tools run
  natively.

## Results

Each cell is the median time of its runs and the highest peak memory among them.

| Alignment | File | seqpeeq | Jalview |
|---|---:|---:|---:|
| start-up: 5 × 20 | 150 B | 64 ms · 5 MB | 1.35 s · 283 MB |
| 1,000 × 1,000 | 1 MB | 82 ms · 7 MB | 1.43 s · 304 MB |
| 10,000 × 10,000 | 100 MB | 78 ms · 107 MB | 6.13 s · 1.3 GB |
| 30,000 × 30,000 | 900 MB | 0.13 s · 911 MB | 76 s · 5.2 GB |
| 100,000 × 30,000 | 3.0 GB | 0.65 s · 3.0 GB | 6.5 min · 14.7 GB |

| Tree | File | seqpeeq | FigTree |
|---|---:|---:|---:|
| start-up: 5 tips | 80 B | 61 ms · 5 MB | 1.15 s · 226 MB |
| 1,000 tips | 21 kB | 79 ms · 6 MB | 1.28 s · 254 MB |
| 10,000 tips | 219 kB | 82 ms · 8 MB | 1.60 s · 435 MB |
| 100,000 tips | 2 MB | 74 ms · 31 MB | 3.82 s · 2.3 GB |
| 1,000,000 tips | 24 MB | 0.44 s · 308 MB | 26 s · 11.8 GB |

| BAM | File | seqpeeq | IGV |
|---|---:|---:|---:|
| start-up: 2 kb, 2× | 1 kB | 0.13 s · 6 MB | 3.36 s · 322 MB |
| 1 Mb, 10× | 1 MB | 0.13 s · 6 MB | 3.41 s · 362 MB |
| 1 Mb, 100× | 8 MB | 0.16 s · 8 MB | 1.42 s · 371 MB |
| 1 Mb, 1,000× | 44 MB | 0.16 s · 19 MB | 1.55 s · 388 MB |

## seqpeeq only

seqpeeq-unique features timed inside seqpeeq (`bench_timings_on_the_benchmark_files`).
Each is timed to the frame that shows it. Rows marked *background* are work seqpeeq hands to a
worker thread while the screen stays live: the column counts behind its colouring, and the summary
behind the zoomed-out overview.

**On the 100,000 × 30,000 alignment (3.0 GB):**

| Step | Time |
|---|---:|
| open and first frame | 0.60 s |
| *background:* count the columns | 4.22 s |
| frame after PageDown | 0.5 ms |
| frame after a zoom-out step | 2.5 ms |
| *background:* summary for the overview | 0.62 s |
| delete 1,000 columns | 0.6 ms |
| *background:* count the columns again | 3.89 s |
| delete 1,000 rows | 0.6 ms |
| search names | 7.8 ms |

**On the 1,000,000-tip tree:**

| Step | Time |
|---|---:|
| open and first frame | 0.39 s |
| frame after PageDown | 0.4 ms |
| prune a clade | 0.14 s |
| tip search | 48 ms |

## How to rerun

```sh
conda env create -f bench/env.yml        # FigTree, Jalview, samtools, numpy, pyte
conda env create -f bench/env-igv.yml    # IGV, with Java 21
cargo build --release -p seqpeeq     # on Apple silicon, with an aarch64-apple-darwin toolchain
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
