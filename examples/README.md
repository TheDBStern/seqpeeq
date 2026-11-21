# Example files

Small, simulated files for trying seqpeeq. Every file is written by
`crates/seqpeeq-core/src/demo/` from fixed seeds.

| File | Description |
|---|---|
| `aln.fa` | 24 sequences × 900 columns in five lineages (`Alpha_2019.1` … `Omicron_2021.5`). Columns 301–309 are a gap in every `Omicron` row, columns 611–622 a gap in every row but `Delta`, and columns 881–900 a gap in every `Beta` and `Gamma` row. |
| `tree.nwk` | The tree the alignment was made from, with branch lengths and some support values. Its tips are the alignment's names, so the two link when both are open. |
| `ref.fa` | The reference for the BAMs: `chr1` (20,000 bp) and `chr2` (5,000 bp). |
| `reads.bam` | 150 bp read pairs at about 20× on `chr1` and 10× on `chr2`, with the features below. |
| `reads2.bam` | Plain pairs at about 8× with one SNP of its own at `chr1:1,033`, for stacking with `reads.bam`. |

What's in `reads.bam` (positions on `ref.fa`):

| Where | What |
|---|---|
| `chr1:1,020` and `chr1:1,071` | a SNP in every read |
| `chr1:1,045` | a SNP in about half the reads |
| after `chr1:1,060` | `TA` inserted in about half the reads |
| `chr1:1,085-1,087` | deleted in about a third of the reads |
| `chr1:1,005-1,020` | four split reads, with their other parts at `chr2:2,001-2,010` |
| `chr1:1,000-1,030` | three pairs whose mates are at `chr2:3,001-3,041` |
| `chr1:1,008-1,038` | three pairs whose mates are 5 kb away, so not proper pairs |
| `chr1:1,015-1,039` | four reads with 20 soft-clipped bases |
| `chr1:1,012-1,050` | dimmed reads: two with mapping quality 0, a duplicate and a secondary |
| `chr1:8,001` and `chr1:8,011` | two spliced reads, each skipping 800 bp |
| `chr1:12,001-12,600` | no reads at all |

To try them, from the top of the repository:

```sh
seqpeeq examples/aln.fa examples/tree.nwk
seqpeeq examples/reads.bam examples/reads2.bam --ref examples/ref.fa
```

To write them again (the output is the same every time):

```sh
cargo run -p seqpeeq-core --features testing --example make_examples -- examples
```
