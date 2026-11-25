# Changelog

All notable changes to seqpeeq are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Changed

- A tree links to an alignment whose headers carry a description after the ID: tip `seqA` matches the sequence
  `>seqA Homo sapiens COI` by its ID, the name up to the first space or tab (#17). A sequence whose whole name is
  the tip's still comes first, and tip names are never cut, so `Homo sapiens` still matches only `>Homo sapiens`.
  Linking, the rows' tree order, sorting, `tree prune --seqs`, `tree extract --seqs` and `a` on a clade all match
  this way.
- A linked rename of a sequence matched by its ID changes the ID and keeps the description, from either side:
  `tree rename --seqs` or `msa rename --tree`. A rename that would leave a tip matching two sequences by one ID is
  refused.
- A session logged with 0.4.0 or earlier whose tips matched sequences only by ID now replays with them linked: their
  rows follow the tree, and pruning deletes them.

## [0.4.0] - 2026-11-22

### Added

- Example files in `examples/`: an alignment, its tree, a reference and two BAMs, made up to show each feature.
  `examples/README.md` says where each one is, and
  `cargo run -p seqpeeq-core --features testing --example make_examples -- examples` writes them again.
- The README shows the example files in the interface. The pictures are drawn by the tests, which fail when the
  interface no longer draws them that way.
- `msa sort --by identity`, and `s` on an alignment tab, reorder the sequences by identity to the consensus, to group
  similar sequences and find outliers. It runs in the background, and Esc cancels it. On an alignment linked to one
  tree, it reorders the tree's clades instead and the rows follow the tree.

### Changed

- A linked alignment's rows now follow its tree's order, as the docs said they did: on linking, and after
  ladderizing, re-rooting, pruning, renaming, undo and redo. Unlinking leaves the rows in the order they have.
  Opening a linked pair whose orders differ counts as an unsaved change.
- Session logs gain a `# rows follow linked trees` header line. Logs from 0.3.0 and earlier lack it and replay with a
  linked alignment's rows left in file order, so their row numbers still name the same sequences.

- The keys and commands moved from the README to [docs/keys.md](docs/keys.md) and
  [docs/commands.md](docs/commands.md). The README keeps installation, the quick start and the examples. The release
  archives include both, the README's pictures and `examples/`.
- CI uses actions/checkout v7, which runs on Node.js 24.

### Fixed

- A viewer whose terminal closes (a closed tab, a dropped SSH session) now ends, instead of running at full CPU until
  killed. A job writing a file is cancelled and cleans up first.

## [0.3.0] - 2026-11-20

### Added

- Tab in a BAM's `g` line finishes a contig name. If several contigs match, it opens the contig list, filtered by
  what was typed, and the contig picked goes back on the line.
- Scattered rows or columns of an alignment can be picked: `Space` or right-click on sequence names or column
  numbers picks or unpicks one, and a right-drag picks a run. `d` deletes them and `e` extracts them, as one
  command such as `msa delete-rows --rows 1,3`.
- Several tips or clades of a tree can be picked with `Space` or right-click. `a` then selects all their sequences
  in the linked alignment.
- `:COMMAND -h` (or `--help`) shows a command's help, as in the README: how to write it, what it does, and what its
  options and names mean. `:msa -h`, `:tree -h` and `:bam -h` list a file type's commands, and `:help` lists them
  all. `seqpeeq run "COMMAND -h"` prints the same help and runs nothing.
- `?` names every command for the file in view.

### Changed

- Soft-clipped bases are colored by base at base level, and zoomed-out reads show their clipped ends as gray
  tails. `S` hides clips at both zoom levels.
- A drag across a BAM's reads held past their edge keeps scrolling that way and selecting, so a selection can be
  larger than the screen.
- Right-click on an alignment's names or column numbers, and on a tree, picks or unpicks rather than extending
  the selection. Right-click on residues, and Shift-click, still extend.
- A tree's `a` now also selects the clade's sequences in the alignment, so `d` there deletes them rather than
  gapping the cell under the cursor.

## [0.2.0] - 2026-11-19

### Added

- `O` in a tree re-roots it at the midpoint of its longest path, like `:tree reroot --midpoint`.
- BAMs with the same contigs open as one stack under a shared ruler and reference, moving together (`Tab` moves
  between them; `bam stack`, `bam unstack`).

### Changed

- The help pages are reworded to match the README.
- The alignment and tree help pages show how to rename many names at once, and the README explains it with examples.

### Fixed

- A reference FASTA with blank lines between its sequences, as when genomes are pasted into one file, now opens
  (`--ref`, `bam ref`) and exports (`bam export-ref`). Spaces ending a line are no longer read as bases, a sequence
  with no bases is left out as `samtools faidx` leaves it out, and a file that still cannot be read says which line
  is wrong and how to rewrap it.

## [0.1.0] - 2026-11-18

The first release: one terminal program to view and edit alignments, trees and mapped reads, with every change
logged and replayable, and input files never modified.

### Alignments (aligned FASTA, plain or gzipped)

- Opens multi-GB alignments in seconds by mapping the file rather than copying it. Unaligned FASTA opens too.
- Select rows, columns or blocks with keys or the mouse. Delete, fill with a gap or `N`, overwrite, rename one name or
  many by regex, and extract to a new FASTA file.
- Insert and remove residues in one row, shifting the rest as far as the next gap (`i`, `R`).
- Colour by residue or by conservation. A zoomed-out overview of the whole alignment, with match scores.
- Search names, go to a column, undo and redo.

### Trees (Newick)

- Reads and draws trees of a million tips, collapsing clades to fit the screen. Zoom, open and close clades, branch
  lengths or equal lengths.
- Extract or prune a clade, re-root on a branch, by outgroup or at the midpoint, ladderize, and rename tips one at a
  time or by regex.
- Find tips by name.

### A tree linked to its alignment

- Linked when one file has a sequence for every tip of the other, on open, with `p`, or with `tree link`.
- The alignment's rows follow the tree's order. Pruning a clade deletes its sequences, renames reach both files, and
  one undo reverses both.
- `tree extract --seqs` writes a clade and its sequences together.

### BAMs (coordinate-sorted)

- Reads by region through the index, building a missing index on open. A coverage strip of the whole contig, the
  reference, depth, and reads with mismatches, clips, mates and split parts.
- Jump to the next covered or empty stretch, to a read's mate or other part.
- A read's own view: every place it aligns, laid along its bases. A list of reads that also align elsewhere.
- Export a region with its index, a stretch of the reference, or chosen reads to BAM, FASTA or FASTQ, with mates and
  clipping.

### Sessions

- Every command, opens included, is written to a session log. `seqpeeq replay` runs a log again to make the same
  files, and `seqpeeq run` runs commands with no interface.
- The same commands work after `:` in the interface, in `run`, in a log and in `replay`.
- Results always go to new files; seqpeeq refuses to overwrite an existing file.

### Install

- Builds with `cargo install` into a conda environment or anywhere with Rust.
- Prebuilt programs on each GitHub release: a static Linux x86_64 program and an Apple-silicon macOS program.

[Unreleased]: https://github.com/TheDBStern/seqpeeq/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/TheDBStern/seqpeeq/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/TheDBStern/seqpeeq/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/TheDBStern/seqpeeq/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/TheDBStern/seqpeeq/releases/tag/v0.1.0
