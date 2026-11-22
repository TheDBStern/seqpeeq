# Commands

The same commands work in four places:

- after `:` in the interface;
- as arguments to `seqpeeq run`;
- one per line in a session log;
- in `seqpeeq replay`.

Each applies to the current file: the one opened last, or chosen with `use`.

Add `-h` or `--help` to a command after `:` to see its help, e.g. `:msa fill -h`. `:msa -h`, `:tree -h` and `:bam -h`
list each file type's commands, and `:help` lists them all. `seqpeeq run "msa fill -h"` prints the same help.

**Naming things in commands:**

- **Rows** are numbered from 1, e.g. `--rows 3-7,12`.
- **Columns** are numbered from 1, e.g. `--cols 100-250`.
- **Regions** are `contig:start-end`, or a contig's name for all of it.
- **A clade** is `--tip NAME`, or `--mrca A --mrca B` (the most recent common ancestor of two tips), or `--node N`
  (the Nth node in pre-order).

## Files

| Command | What it does |
|---|---|
| `open PATH [--kind msa\|tree\|bam]` | Open a file. The file type is inferred from the extension unless given. |
| `use N` | Make the Nth open file (counting from 1) the one commands apply to. |
| `close` | Close the current file. Nothing is written. |
| `undo` | Take back the last change to the current file. |
| `redo` | Reapply the change that was last undone. |

## Alignments

Rows can be chosen with `--rows`, with `--name NAME` (exact; repeatable), or with `--match REGEX` (searched for in
names).

| Command | Explanation |
|---|---|
| `msa delete-rows --rows … \| --name … \| --match …` | Remove sequences. |
| `msa delete-cols --cols COLS` | Remove columns. |
| `msa fill [rows] [--cols COLS] --with CHAR` | Overwrite a block with one character, such as `-` or `N`. |
| `msa overwrite --row ROW --col COL --seq SEQ` | Replace residues in one row, from a column on, with the characters given. |
| `msa insert --row ROW --col COL --seq SEQ` | Insert characters into one row at a column. The residues after it move right as far as the gaps it uses up. Gap columns are added at the end when the row has too few. |
| `msa remove --row ROW --col COL --count N` | Remove characters from one row at a column. The residues after them move left as far as the next gap, leaving gaps behind. |
| `msa extract [rows] [--cols COLS] [--wrap N] --out FILE` | Write sequences, or parts of them, to a new FASTA file. `--wrap` sets residues per line; the default is one line per sequence. |
| `msa rename [rows] --to NAME \| --regex RE --replace TEXT [--tree]` | Rename one sequence with `--to`, or many with `--regex` and `--replace`. `--tree` also renames the matching tips in the linked tree. |
| `msa sort --by identity` | Reorder the sequences, most like the consensus first: by the share of columns with a majority residue where each carries it. Gaps count as differences. Undo puts the old order back. When the alignment is linked to one tree, the tree's clades are reordered instead and the rows follow the tree. |

## Trees

| Command | Explanation |
|---|---|
| `tree extract [clade] --out FILE [--seqs FILE] [--wrap N]` | Write a clade, or the whole tree with no clade given, to a new file. With `--seqs`, also write its tips' sequences from the linked alignment. |
| `tree prune clade [--seqs]` | Remove a clade. `--seqs` also deletes its sequences from the linked alignment. |
| `tree reroot clade \| --outgroup TIP … \| --midpoint` | Root on the branch above a node, between an outgroup and the rest, or at the midpoint of the longest path between tips. |
| `tree ladderize [--reverse]` | Order every node's children by tip count, fewest first (`--reverse`: most first). |
| `tree rename [clade] --to NAME \| --regex RE --replace TEXT [--seqs]` | Rename one tip with `--to`, or many with `--regex` and `--replace`. `--seqs` also renames the matching sequences in the linked alignment. |
| `tree link FILE` | Pair the tree with an open alignment, matching tip and sequence names. FILE may be the open alignment's path or just its file name. |
| `tree unlink` | Drop the tree's link to an alignment. |

## Renaming many at once

`msa rename` and `tree rename` take `--regex RE --replace TEXT` to rename every name the pattern matches, in one undo
step. Type them after `:` in the interface. The examples here start from names like `Alpha_2019.2`, as in
`examples/aln.fa`.

| Command | Result |
|---|---|
| `msa rename --regex '\.\d+$' --replace ''` | `Alpha_2019`: the version suffix is removed. |
| `msa rename --regex '^([^_]+)_(.+)$' --replace '${2}_$1'` | `2019.2_Alpha`: the two parts swap places. |
| `msa rename --match Delta --regex '^' --replace 'VOC_'` | `VOC_Delta_2021.1`, and only in rows whose names contain `Delta`. |
| `tree rename --regex '_' --replace '-' --seqs` | `Alpha-2019.2`, in the tree and in its linked alignment. |

- The pattern is a [Rust regular expression](https://docs.rs/regex/latest/regex/#syntax), and every match in a name
  is replaced.
- Patterns are case-sensitive, unlike `/` search. Start one with `(?i)` to ignore case.
- In `--replace`, `$1` or `${1}` is the first group, `${name}` a named group and `$$` a dollar sign. Write `${2}_`,
  not `$2_`, when a group is followed by a letter, digit or `_`: seqpeeq refuses `$2_` rather than read it as a group
  called `2_`.
- `msa rename` takes `--rows`, `--name` or `--match` to rename only some rows. `tree rename --regex` renames across
  the whole tree.
- `--tree` (on `msa rename`) renames the matching tips in the linked tree too, and `--seqs` (on `tree rename`) the
  matching sequences in the linked alignment.
- Quote patterns with single quotes. In a shell, as in `seqpeeq run`, put the whole command in double quotes and
  write each `$` as `\$`.
- A rename that would give two tips the same name is refused, as is one that would give two linked sequences the same
  name. An alignment on its own may end up with duplicate names; seqpeeq says how many.

## BAMs

| Command | Explanation |
|---|---|
| `bam ref FILE` | Attach a reference FASTA to the current BAM, or to every BAM in its stack, for reference bases, mismatches and `export-ref`. |
| `bam export-region REGION --out FILE` | Write every record overlapping a region to a new BAM, with its index. |
| `bam export-reads [--region REGION] [--name NAME … \| --names-file FILE] [--with-mates] [--clip] [--everywhere] --out FILE` | Write reads to a new BAM, FASTA or FASTQ, chosen by `--out`'s extension. See below. |
| `bam export-ref REGION --out FILE` | Write a stretch of the reference to a new FASTA. |
| `bam stack FILE` | Stack the current BAM with an open BAM that has the same contigs. |
| `bam unstack` | Split the current BAM's stack back into tabs. |

The options of `bam export-reads`:

- `--with-mates` adds each read's mate and the other parts of split reads.
- `--clip` keeps only the bases inside the region (FASTA or FASTQ).
- `--everywhere` searches the whole BAM for the names instead of reading a region.

## Logs and replay

**The log file:**

- Each session writes `seqpeeq-YYYYMMDD-HHMMSS.log` in the current folder.
  - `--log PATH` to write it to PATH instead.
  - `--no-log` to turn off logging.
  - Logs aren't written if no commands are run.
  - An existing log is never overwritten or amended.
- Every command, opens included, one per line, with timestamps and notes as `#` comments are saved in the log.
  - Scrolling, zooming and selecting are not logged.
  - Undo and redo are logged, so a replay follows exactly what happened.
- Each `open` records the file's full path and size. A replay warns when a file's size has changed since.
- A command naming too many reads to fit on a line gives them in a names file beside the log,
  `<log>.names-N.txt`, which the command points to with `--names-file`.

**Replaying:**

- Since outputs are never overwritten, you should `replay` into a folder where the log's output files don't exist yet. Moving or
  deleting the earlier outputs also works.
- Paths are kept as they were typed, so relative paths are read from the folder you replay in. Copy the inputs
  there, or give full paths when you work.
- Logs from seqpeeq 0.3.0 and earlier replay as they were written: a linked alignment's rows stay in file order
  rather than following the tree, so row numbers name the same sequences. Newer logs say
  `# rows follow linked trees` in their header.

**If the log fails:** when the log can't be written (a full disk, e.g.), seqpeeq refuses further changes rather than
make ones it can't record. `:rescue FILE` still saves the current alignment, so no work is lost.
