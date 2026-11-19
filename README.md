<h1>
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/logo/seqpeeq-dark.svg">
    <img alt="seqpeeq" src="docs/logo/seqpeeq-light.svg" width="420">
  </picture>
</h1>

seqpeeq views and edits three kinds of file in the terminal:

- **multiple sequence alignments** (aligned FASTA);
- **phylogenetic trees** (Newick);
- **mapped reads** (sorted BAM).

You can rename, delete, overwrite and extract, and link a tree to its alignment so that edits to one follow in the
other. For BAMs you can export reads, regions or reference, and follow a read to every place it aligns.

It is built for large files:

- An alignment is mapped, not copied, so a several-GB file opens in seconds.
- A tree of a million tips collapses itself to fit the screen.
- A BAM is read by region through its index.

It runs the same on a laptop and over SSH on a cluster, with nothing to set up.

**Two promises:**

- **Your input files are never changed.** Every result is written to a new file, and seqpeeq refuses to overwrite a
  file that already exists.
- **Every change is logged.** Each command is written to a session log, which `seqpeeq replay` runs again to make the
  same files.

## Install

### Conda environment

On a Linux server or a Mac, build seqpeeq into its own conda environment:

```sh
conda create -n seqpeeq -c conda-forge rust
conda activate seqpeeq
git clone https://github.com/TheDBStern/seqpeeq && cd seqpeeq
cargo install --path crates/seqpeeq --root "$CONDA_PREFIX"
```

- The program goes in the environment's `bin`, so `seqpeeq` is there whenever the environment is active.
- To update, run `git pull` and the last line again.
- The build downloads its libraries from crates.io, so run it where there is internet, usually a login node.
- All dependencies are pure Rust, so no C libraries are needed. Linking uses the system's `cc`, which Linux servers
  almost always have; if cargo says ``linker `cc` not found``, load or install gcc first.

### With Rust

If you have a Rust toolchain already:

```sh
cargo install --git https://github.com/TheDBStern/seqpeeq seqpeeq
```

### Prebuilt program

Each release on GitHub has:

- `seqpeeq-<version>-x86_64-unknown-linux-musl.tar.gz`, a static Linux program that runs on any x86_64 Linux,
  however old;
- `seqpeeq-<version>-aarch64-apple-darwin.tar.gz`, for Apple-silicon Macs.

Each comes with a `.sha256` file. Download, check and unpack one in a terminal:

```sh
v=v0.1.0
t=aarch64-apple-darwin           # on Linux: t=x86_64-unknown-linux-musl
f=seqpeeq-$v-$t.tar.gz
curl -fLO https://github.com/TheDBStern/seqpeeq/releases/download/$v/$f
curl -fLO https://github.com/TheDBStern/seqpeeq/releases/download/$v/$f.sha256
shasum -a 256 -c $f.sha256       # on Linux: sha256sum -c $f.sha256
tar xzf $f
```

Then move `seqpeeq-$v-$t/seqpeeq` to a folder on your `PATH`, such as `~/.local/bin`.

**On a Mac, download with `curl` as above, not with a browser.** The program is not signed by Apple, and macOS refuses
to open an unsigned program that a browser or Mail downloaded. A program fetched with `curl`, built with `cargo` or
installed with conda is not affected. If you did use a browser, clear the flag once:

```sh
xattr -d com.apple.quarantine seqpeeq
```

## First session

```sh
seqpeeq aln.fa          # open an alignment (also: tree.nwk, reads.bam, or several at once)
```

1. Move with the arrows or `hjkl`.
2. Press `V` to select whole rows, and `j` to extend the selection down a row.
3. Press `d` to delete the selected rows.
4. Press `w` to save the whole alignment to a new file. Type its name at the prompt and press Enter.
5. Press `q` to quit.

The session's log is in the current folder as `seqpeeq-YYYYMMDD-HHMMSS.log`. Run it again, without the interface:

```sh
seqpeeq replay seqpeeq-20261118-101500.log
```

Other ways in:

- `seqpeeq reads.bam --ref genome.fa` opens a BAM with its reference.
- `seqpeeq run 'open aln.fa' 'msa delete-cols --cols 1-100' 'msa extract --out trimmed.fa'` runs commands with no
  interface, as a script would.
- In the interface, `?` shows the keys for the tab in view, and `:` takes any command below.

## Keys

`?` shows these pages in the program, for the kind of file in view.

### Alignment

```
Move      arrows hjkl   Shift+arrows a screen
          PgUp PgDn   Home End   g go to
          / search names   n N next, previous
Select    drag   right-click extends   v V C   Esc clear
Edit      d delete or gap   f fill   r rename
          i insert (shifts)   R type over   Esc stop
Zoom      - out   + in   z whole alignment and back
          u U undo redo
          w save the whole alignment
          e extract the selection
          c colours   [ ] name width
          msa delete-rows  delete-cols  fill  overwrite
          msa insert  remove  extract  rename
```

### Tree

```
Tree      jk rows  h parent  l back  Enter open/close
          + - z zoom  b lengths  / n N find  v select
          d prune  r rename  o re-root
          w save the whole tree   e extract the clade
          s ladderize  u U undo redo  a to the alignment
          p link to an open alignment
```

**Linking:** a tree is linked to an alignment when one has a sequence for every tip of the other. This happens on its
own when you open the second file, or with `p` or `:tree link`. Once linked:

- the alignment's rows follow the tree's order;
- pruning a clade in the tree also deletes its sequences;
- renaming a tip or a sequence renames it in both.

`:tree unlink` undoes the link.

### BAM

```
Move      arrows hjkl   Shift+arrows a screen
          PgUp PgDn   Home End
BAM       g go to   ] [ covered   } { no coverage
          m mate   s other part   ' back   c contigs
          - + z zoom   F filtered   S clips
          click, drag select   Esc clear
          Space add the read under the cursor
                to the selection
          drag the column numbers, v v  a stretch
          e export reads   E reference   to a file
          a read's places   A reads aligning elsewhere
          in those: S search the BAM   g go   / filter
```

### Every tab

```
View      Tab next file   L log   ? help   q quit
Commands  :  open FILE   close   use N   undo   redo
          :rescue FILE if the log has failed
          Tab completes a file name after :tree link
Mouse     wheel on the column numbers scrolls sideways
          hold Shift (Option on macOS) to select text
          with the terminal itself
          in tmux: set -g mouse on
```

## Commands

The same commands work in four places:

- after `:` in the interface;
- as arguments to `seqpeeq run`;
- one per line in a session log;
- in `seqpeeq replay`.

Each applies to the current file: the one opened last, or chosen with `use`.

**Naming things in commands:**

- **Rows** are numbered from 1, e.g. `--rows 3-7,12`.
- **Columns** are numbered from 1, e.g. `--cols 100-250`.
- **Regions** are `contig:start-end`, or a contig's name for all of it.
- **A clade** is `--tip NAME`, or `--mrca A --mrca B` (the most recent common ancestor of two tips), or `--node N`
  (the Nth node in pre-order).

### Files

| Command | What it does |
|---|---|
| `open PATH [--kind msa\|tree\|bam]` | Open a file. The kind is taken from the extension unless given. |
| `use N` | Make the Nth open file (counting from 1) the one commands apply to. |
| `close` | Close the current file. Nothing is written. |
| `undo` | Take back the last change to the current file. |
| `redo` | Reapply the change that was last undone. |

### Alignments

Rows can be chosen with `--rows`, with `--name NAME` (exact; repeatable), or with `--match REGEX` (searched for in
names).

| Command | What it does |
|---|---|
| `msa delete-rows --rows … \| --name … \| --match …` | Remove sequences. |
| `msa delete-cols --cols COLS` | Remove columns. |
| `msa fill [rows] [--cols COLS] --with CHAR` | Overwrite a block with one character, such as `-` or `N`. |
| `msa overwrite --row ROW --col COL --seq SEQ` | Replace residues in one row, from a column on, with the characters given. |
| `msa insert --row ROW --col COL --seq SEQ` | Insert characters into one row at a column. The residues after it move right as far as the gaps it uses up. Gap columns are added at the end when the row has too few. |
| `msa remove --row ROW --col COL --count N` | Remove characters from one row at a column. The residues after them move left as far as the next gap, leaving gaps behind. |
| `msa extract [rows] [--cols COLS] [--wrap N] --out FILE` | Write sequences, or parts of them, to a new FASTA file. `--wrap` sets residues per line; the default is one line per sequence. |
| `msa rename [rows] --to NAME \| --regex RE --replace TEXT [--tree]` | Rename one sequence with `--to`, or many with `--regex` and `--replace`. `--tree` also renames the matching tips in the linked tree. |

### Trees

| Command | What it does |
|---|---|
| `tree extract [clade] --out FILE [--seqs FILE] [--wrap N]` | Write a clade, or the whole tree with no clade given, to a new file. With `--seqs`, also write its tips' sequences from the linked alignment. |
| `tree prune clade [--seqs]` | Remove a clade. `--seqs` also deletes its sequences from the linked alignment. |
| `tree reroot clade \| --outgroup TIP … \| --midpoint` | Root on the branch above a node, between an outgroup and the rest, or at the midpoint of the longest path between tips. |
| `tree ladderize [--reverse]` | Order every node's children by tip count, fewest first (`--reverse`: most first). |
| `tree rename [clade] --to NAME \| --regex RE --replace TEXT [--seqs]` | Rename one tip with `--to`, or many with `--regex` and `--replace`. `--seqs` also renames the matching sequences in the linked alignment. |
| `tree link FILE` | Pair the tree with an open alignment, matching tip and sequence names. FILE may be the open alignment's path or just its file name. |
| `tree unlink` | Drop the tree's link to an alignment. |

### BAMs

| Command | What it does |
|---|---|
| `bam ref FILE` | Attach a reference FASTA to the current BAM, for reference bases, mismatches and `export-ref`. |
| `bam export-region REGION --out FILE` | Write every record overlapping a region to a new BAM, with its index. |
| `bam export-reads [--region REGION] [--name NAME … \| --names-file FILE] [--with-mates] [--clip] [--everywhere] --out FILE` | Write reads to a new BAM, FASTA or FASTQ, chosen by `--out`'s extension. See below. |
| `bam export-ref REGION --out FILE` | Write a stretch of the reference to a new FASTA. |

The options of `bam export-reads`:

- `--with-mates` adds each read's mate and the other parts of split reads.
- `--clip` keeps only the bases inside the region (FASTA or FASTQ).
- `--everywhere` searches the whole BAM for the names instead of reading a region.

## Logs and replay

**The log file:**

- **Where it goes:** each session writes `seqpeeq-YYYYMMDD-HHMMSS.log` in the current folder.
  - `--log PATH` puts it elsewhere.
  - `--no-log` writes none.
  - A session that runs no command leaves no log.
  - An existing log is never overwritten or added to.
- **What it holds:** every command, opens included, one per line, with timestamps and notes as `#` comments.
  - Scrolling, zooming and selecting are not logged.
  - Undo and redo are, so a replay follows exactly what happened.
- **Inputs:** each `open` records the file's full path and size. A replay warns when a file's size has changed since.
- **Names files:** a command naming too many reads to fit on a line gives them in a names file beside the log,
  `<log>.names-N.txt`, which the command points to with `--names-file`.

**Replaying:**

- `seqpeeq replay LOG` reads every line before running any, so a log it cannot understand changes nothing.
- A command that fails stops the replay there.
- Since outputs are never overwritten, replay into a folder where the log's output files don't exist yet. Moving or
  deleting the earlier outputs also works.
- Paths are kept as they were typed, so relative paths are read from the folder you replay in. Copy the inputs
  there, or give full paths when you work.

**If the log fails:** when the log can't be written (a full disk, say), seqpeeq refuses further changes rather than
make ones it can't record. `:rescue FILE` still saves the current alignment, so no work is lost.

## Limits

- **Alignments:** FASTA, plain or gzipped. Unaligned FASTA (rows of different lengths) opens too.
- **Trees:** Newick. Nexus is not read.
- **BAMs:** coordinate-sorted BAMs only. A missing index is built when the file is opened. CRAM is not read.
- **Out of scope:** realigning, tree inference and variant calling.

## Benchmarks

See [docs/benchmarks.md](docs/benchmarks.md) for how seqpeeq compares with IGV, FigTree and Jalview on files of rising
size.
For example, on an Apple M2 laptop:

- **A 100,000 × 30,000 alignment (3 GB):** seqpeeq shows it in 0.65 s; Jalview takes 6.5 minutes and 15 GB,
  near the 16 GB it was given.
- **A tree of a million tips:** seqpeeq draws it in 0.44 s; FigTree takes 26 s.

## Changes

See [CHANGELOG.md](CHANGELOG.md).

## License

seqpeeq is licensed under either of

- the Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)), or
- the MIT license ([LICENSE-MIT](LICENSE-MIT)),

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in seqpeeq by you, as
defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
