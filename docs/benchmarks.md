# Benchmarks

Comparing times for opening files. seqpeeq is compared with other terminal viewers (termal and alv
for alignments, samtools tview and ASCIIGenome for BAMs) and with GUI programs (Jalview for alignments,
FigTree for trees and IGV for BAMs). To my knowledge, there is no interactive terminal tree viewer to compare with,
so trees are compared with FigTree only.

The files are random but seeded, from 1,000 to 100,000 sequences, 1,000 to 1,000,000 tips, and 10×
to 1,000× read depth.

## Machine

- Apple M2, 8 cores, 24 GB of memory, macOS 27.2.1.
- Versions: seqpeeq 0.4.0 (arm64), termal 2.1.0 (arm64), samtools 1.25, alv 1.8.3, FigTree 1.4.4, Jalview 2.11.5.1, java (FigTree, Jalview) 11.0.30, IGV 2.19.8, java (IGV) 21.0.13.1, ASCIIGenome 1.21.1, java (ASCIIGenome) 27.0.1.1.

## Method

**Timing:** the wall time from launch until the tool has shown the file and the peak
memory (the largest resident size the operating system reports for the process).

The terminal tools run in a hidden 160 × 48 terminal, and the screen is read as it is drawn. Each
is timed until the file is on screen, then sent `q` to quit.

- **seqpeeq**:
  - for an alignment or a tree, it is timed until the first screen of the file is drawn;
  - a BAM opens on the whole contig, so it is sent `g c1:1-1000` (the window IGV is sent to) and
    timed until that window's reads have been read and drawn.
- **termal** is timed until its first screen is drawn, down to the help line at its bottom. It
  draws nothing until it has read the whole file, so its time includes reading all of it.
- **alv** prints the whole alignment and exits, so it is timed the way it is used, piped into
  `less -R`, until less has drawn its first screen. alv reads the whole file before it prints,
  so its time includes reading all of it.
- **samtools tview** is opened at `c1:1`, the start of IGV's window, with the reference. It is
  timed until the reads under its ruler and reference are drawn. It shows one screen's width of
  bases, one to a column.
- **ASCIIGenome** is opened on `c1:1-1000` with the reference, and timed until the line naming
  that window is drawn under the reads. Its prompt takes commands, so it is sent `q` and Enter.

The GUI programs are run in their script modes and timed until they exit:

- **Jalview** is run headless with `--open FILE --close`: it reads the alignment in and exits. It is
  timed to its exit. It doesn't save an image, because `--image`
  draws the whole alignment shrunk onto one picture, which is a lot of work.
- **FigTree** draws the tree to a 1200 × 800 PNG (`-graphic PNG`) and exits. It is timed to its exit.
- **IGV** runs a batch script: load the reference and the BAM, go to `c1:1-1000`, save a
  snapshot, exit. It is timed to its exit.

**Fairness:**

- **Memory:** each Java tool is given 16 GB (`-Xmx`). Each runs on Java from its own conda
  environment: Jalview 2.11.5 needs Java below 12, IGV 2.19 needs Java 21 and ASCIIGenome 1.21
  needs Java 17 or later.
- **IGV's settings:** IGV runs with its home folder set to a scratch folder, so it never reads or
  writes anyone's `~/igv`. Its preferences there make the benchmark reference the default genome, so
  it does not download a human genome when it starts.
- **ASCIIGenome** runs its jar on Java directly, not through Bioconda's wrapper script, so the
  Java process is the one measured and the 16 GB limit applies. Its home folder is a scratch folder
  too, so its command history never touches anyone's home.
- **Jalview** is started by its own main class. The jar's entry point is a launcher that starts
  Jalview in a second Java process and exits at once, so timing it would time only the launcher.
  Any run that exits while processes it started are still running is recorded as a failure.
- **Start-up:** the first row of each table is a tiny file, so it shows each tool's start-up cost,
  most of it the Java virtual machine's for the four Java tools.
- **Repeats:** each number is the median of 3 runs, or of 1 run when the first takes over a minute.
- **Limits:** each run stops after 10 minutes. If a tool runs out of memory, crashes
  or hits the limit, that's recorded as such, with the reason in a note.
- **Build:** seqpeeq is built with `--release` and termal with `cargo install`, both with Cargo's
  default release settings and for the machine's own CPU. All eight tools run natively.

## Results

Each cell is the median time of its runs and the highest peak memory among them.
The times are for files already in memory, which the operating system keeps after a file is read
and the repeated runs hit: a first open from disk is slower.

| Alignment | File | seqpeeq | termal | alv | Jalview |
|---|---:|---:|---:|---:|---:|
| start-up: 5 × 20 | 150 B | 62 ms · 6 MB | 16 ms · 4 MB | 0.16 s · 50 MB | 1.34 s · 285 MB |
| 1,000 × 1,000 | 1 MB | 77 ms · 7 MB | 0.22 s · 7 MB | 1.18 s · 52 MB | 1.47 s · 300 MB |
| 10,000 × 10,000 | 100 MB | 77 ms · 108 MB | 4.15 s · 229 MB | 81 s · 163 MB | 6.60 s · 1.3 GB |
| 30,000 × 30,000 | 900 MB | 0.13 s · 911 MB | 71 s · 2.0 GB | timed out (10 min)[^1] | 98 s · 6.7 GB |
| 100,000 × 30,000 | 3.0 GB | 0.69 s · 3.0 GB | failed[^2] | timed out (10 min)[^3] | 6.0 min · 12.7 GB |

| Tree | File | seqpeeq | FigTree |
|---|---:|---:|---:|
| start-up: 5 tips | 80 B | 62 ms · 6 MB | 1.20 s · 234 MB |
| 1,000 tips | 21 kB | 77 ms · 6 MB | 1.29 s · 266 MB |
| 10,000 tips | 219 kB | 82 ms · 8 MB | 1.61 s · 383 MB |
| 100,000 tips | 2 MB | 74 ms · 32 MB | 3.81 s · 1.9 GB |
| 1,000,000 tips | 24 MB | 0.43 s · 309 MB | 27 s · 12.0 GB |

| BAM | File | seqpeeq | samtools tview | ASCIIGenome | IGV |
|---|---:|---:|---:|---:|---:|
| start-up: 2 kb, 2× | 1 kB | 0.12 s · 7 MB | 81 ms · 39 MB | 0.74 s · 185 MB | 3.58 s · 326 MB |
| 1 Mb, 10× | 1 MB | 0.13 s · 7 MB | 75 ms · 39 MB | 0.76 s · 181 MB | 3.43 s · 366 MB |
| 1 Mb, 100× | 8 MB | 0.16 s · 8 MB | 74 ms · 39 MB | 0.90 s · 184 MB | 1.74 s · 367 MB |
| 1 Mb, 1,000× | 44 MB | 0.16 s · 20 MB | 79 ms · 40 MB | 1.00 s · 217 MB | 1.90 s · 394 MB |

### From disk

seqpeeq on the largest files with the file cache emptied (`sudo purge`) before each run, so each
run reads its file from disk. This is mostly the time to read the file, which every tool would
add, so only seqpeeq is timed this way.

| File | Size | From disk | In memory |
|---|---:|---:|---:|
| 30,000 × 30,000 | 900 MB | 1.50 s | 0.13 s |
| 100,000 × 30,000 | 3.0 GB | 5.08 s | 0.69 s |
| 1,000,000 tips | 24 MB | 0.57 s | 0.43 s |

## seqpeeq only

seqpeeq-unique features timed inside seqpeeq (`bench_timings_on_the_benchmark_files`).
Each is timed to the frame that shows it. Rows marked *background* are work seqpeeq hands to a
worker thread while the screen stays live: the column counts behind its colouring, and the summary
behind the zoomed-out overview.

**On the 100,000 × 30,000 alignment (3.0 GB):**

| Step | Time |
|---|---:|
| open and first frame | 0.59 s |
| *background:* count the columns | 4.43 s |
| frame after PageDown | 0.5 ms |
| frame after a zoom-out step | 2.4 ms |
| *background:* summary for the overview | 0.62 s |
| delete 1,000 columns | 0.6 ms |
| *background:* count the columns again | 3.95 s |
| delete 1,000 rows | 0.7 ms |
| search names | 7.7 ms |

**On the 1,000,000-tip tree:**

| Step | Time |
|---|---:|
| open and first frame | 0.38 s |
| frame after PageDown | 0.4 ms |
| prune a clade | 0.14 s |
| tip search | 48 ms |

## How to rerun

```sh
conda env create -f bench/env.yml        # FigTree, Jalview, samtools, alv, numpy, pyte
conda env create -f bench/env-igv.yml    # IGV, with Java 21
conda env create -f bench/env-asciigenome.yml    # ASCIIGenome, with Java 17 or later
cargo install termal-msa@2.1.0 --locked --root bench/tools
cargo build --release -p seqpeeq     # on Apple silicon, with an aarch64-apple-darwin toolchain
PY=$(conda run -n seqpeeq-bench which python)
$PY bench/make_data.py --out bench/data --only all          # about 4 GB
$PY bench/run.py --data bench/data --out bench/results.json --only all
SEQPEEQ_BENCH_DATA=$PWD/bench/data SEQPEEQ_BENCH_OUT=$PWD/bench/seqpeeq_only.json \
    cargo test --release -p seqpeeq -- --ignored bench_timings
sudo -v && $PY bench/run.py --data bench/data --out bench/cold.json --cold
$PY bench/report.py --results bench/results.json --seqpeeq-only bench/seqpeeq_only.json \
    --cold bench/cold.json --out docs/benchmarks.md
```

`run.py` looks for the three environments in `~/miniconda3/envs`; set `SEQPEEQ_BENCH_ENVS` to the
folder that holds them if they are elsewhere.

[^1]: alv on `aln_30k.fa`: after 600 s
[^2]: termal on `aln_100k.fa`: exit 101: thread 'main' (927796) panicked at ~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/termal-msa-2.1.0/src/app.rs:195:45: called `Result::unwrap()` on an `Err` value: TryFromIntError(PosOverflow) note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
[^3]: alv on `aln_100k.fa`: after 600 s
