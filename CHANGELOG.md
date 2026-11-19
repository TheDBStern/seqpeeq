# Changelog

All notable changes to seqpeeq are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

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

[Unreleased]: https://github.com/TheDBStern/seqpeeq/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/TheDBStern/seqpeeq/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/TheDBStern/seqpeeq/releases/tag/v0.1.0
