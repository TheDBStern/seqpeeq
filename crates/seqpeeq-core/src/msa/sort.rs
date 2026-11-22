//! Sorting rows by how closely each matches the consensus.

use super::conservation::BLOCK;
use super::{Alignment, Applied, View, count, invalid};
use crate::command::msa::SortKey;
use crate::progress::Progress;
use crate::{Error, Result};

/// A checked `msa sort`, ready to run on any thread.
pub(crate) struct Sorting {
    view: View,
}

/// What a sort found.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Sorted {
    /// Current rows, most like the consensus first.
    pub order: Vec<u32>,
    /// Each current row's identity, from 0 to 1.
    pub scores: Vec<f64>,
    /// The highest and lowest identity, as percentages.
    pub high: f64,
    pub low: f64,
}

impl Alignment {
    /// Check an `msa sort` and get it ready to run.
    pub(crate) fn plan_sort(&self, by: SortKey) -> Result<Sorting> {
        match by {
            SortKey::Identity if !self.is_aligned() => {
                invalid("sorting needs every sequence to be the same length")
            }
            SortKey::Identity => Ok(Sorting {
                view: self.view.clone(),
            }),
        }
    }

    /// Put the rows in the order a sort found, as one undo step; rows
    /// already in that order are left alone, with no step.
    pub(crate) fn commit_sort(&mut self, sorted: &Sorted) -> Applied {
        if !sorted.moves_rows() {
            return Applied::new(sorted.unchanged_message(self.rows()));
        }
        self.record_sort(sorted)
    }

    /// [`Alignment::commit_sort`], always as an undo step: the half of a
    /// sort of linked files that the tree's half is paired with.
    pub(crate) fn record_sort(&mut self, sorted: &Sorted) -> Applied {
        self.edit("sort", |layer, _| layer.reorder(&sorted.order));
        Applied::new(sorted.message(self.rows()))
    }
}

impl Sorted {
    /// Whether the order found differs from the rows' order now.
    pub(crate) fn moves_rows(&self) -> bool {
        self.order
            .iter()
            .enumerate()
            .any(|(at, &row)| at != row as usize)
    }

    /// What the status line says when the sort changed nothing.
    pub(crate) fn unchanged_message(&self, rows: usize) -> String {
        format!(
            "{} already sorted by identity to the consensus: {:.1}% to {:.1}%",
            count(rows, "sequence"),
            self.high,
            self.low
        )
    }

    /// What the status line says of a sort of `rows` sequences.
    pub(crate) fn message(&self, rows: usize) -> String {
        format!(
            "sorted {} by identity to the consensus: {:.1}% to {:.1}%",
            count(rows, "sequence"),
            self.high,
            self.low
        )
    }
}

impl Sorting {
    /// Count the consensus, then score every row against it: two passes
    /// over the residues, holding one row at a time.
    pub(crate) fn run(&self, progress: &Progress) -> Result<Sorted> {
        let view = &self.view;
        let units = view.count_units(BLOCK);
        let blocks = view.cols().div_ceil(BLOCK) as u64;
        progress.set_total(2 * units);
        let majority = view
            .count_majorities(progress, BLOCK)
            .ok_or(Error::Cancelled)?;
        let counted = majority.iter().filter(|&&m| m != 0).count();
        if counted == 0 {
            return invalid(
                "no column has a residue that at least half the sequences share, so there is \
                 nothing to sort by",
            );
        }
        let mut matches = Vec::with_capacity(view.rows());
        let mut residues = Vec::with_capacity(view.cols());
        for row in 0..view.rows() {
            if progress.is_cancelled() {
                return Err(Error::Cancelled);
            }
            residues.clear();
            view.residues(row, 0..view.cols(), &mut residues);
            let same = residues
                .iter()
                .zip(&majority)
                .filter(|&(&residue, &m)| m != 0 && residue.to_ascii_uppercase() == m)
                .count();
            matches.push(same);
            progress.add(blocks);
        }
        let mut order: Vec<u32> = (0..view.rows() as u32).collect();
        order.sort_by_key(|&row| std::cmp::Reverse(matches[row as usize]));
        let percent = |row: u32| matches[row as usize] as f64 * 100.0 / counted as f64;
        Ok(Sorted {
            high: percent(order[0]),
            low: percent(order[order.len() - 1]),
            scores: matches
                .iter()
                .map(|&same| same as f64 / counted as f64)
                .collect(),
            order,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::Error;
    use crate::command::msa::SortKey;
    use crate::msa::Alignment;
    use crate::msa::testing::{SORTABLE, run};
    use crate::progress::Progress;

    fn names(a: &Alignment) -> Vec<&str> {
        (0..a.rows()).map(|row| a.name(row)).collect()
    }

    fn error(result: crate::Result<impl std::fmt::Debug>) -> String {
        match result {
            Err(Error::Invalid(message)) => message,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn rows_go_most_like_the_consensus_first() {
        let mut a = Alignment::from_text(SORTABLE);
        let applied = run(&mut a, "sort --by identity").unwrap();
        assert_eq!(
            applied.message,
            "sorted 3 sequences by identity to the consensus: 100.0% to 75.0%"
        );
        assert_eq!(names(&a), ["r3", "r1", "r2"]);
    }

    #[test]
    fn a_second_sort_changes_nothing() {
        let mut a = Alignment::from_text(SORTABLE);
        run(&mut a, "sort --by identity").unwrap();
        let (revision, stamp) = (a.revision(), a.order_stamp());
        let applied = run(&mut a, "sort --by identity").unwrap();
        assert_eq!(
            applied.message,
            "3 sequences already sorted by identity to the consensus: 100.0% to 75.0%"
        );
        assert_eq!((a.revision(), a.order_stamp()), (revision, stamp));
        a.undo().unwrap();
        assert_eq!(names(&a), ["r1", "r2", "r3"], "one undo step, not two");
    }

    #[test]
    fn equal_scores_keep_their_order() {
        let mut a = Alignment::from_text(">x\nAAAA\n>y\nACGT\n>z\nAAAT\n>w\nAAAA\n");
        run(&mut a, "sort --by identity").unwrap();
        assert_eq!(names(&a), ["x", "w", "z", "y"]);
    }

    #[test]
    fn undo_puts_the_old_order_back() {
        let mut a = Alignment::from_text(SORTABLE);
        run(&mut a, "sort --by identity").unwrap();
        assert_eq!(
            a.undo().unwrap().message,
            "undid sort; now 3 sequences x 5 columns"
        );
        assert_eq!(names(&a), ["r1", "r2", "r3"]);
    }

    #[test]
    fn edits_made_before_count() {
        let mut a = Alignment::from_text(SORTABLE);
        run(&mut a, "fill --rows 3 --cols 1-4 --with N").unwrap();
        run(&mut a, "sort --by identity").unwrap();
        assert_eq!(names(&a), ["r1", "r2", "r3"]);
    }

    #[test]
    fn an_unaligned_file_is_refused() {
        let mut a = Alignment::from_text(">a\nACGT\n>b\nAC\n");
        assert_eq!(
            error(run(&mut a, "sort --by identity")),
            "sorting needs every sequence to be the same length"
        );
    }

    #[test]
    fn no_consensus_is_refused() {
        let mut a = Alignment::from_text(">a\nAC\n>b\nGT\n>c\n--\n");
        let revision = a.revision();
        assert_eq!(
            error(run(&mut a, "sort --by identity")),
            "no column has a residue that at least half the sequences share, so there is \
             nothing to sort by"
        );
        assert_eq!(a.revision(), revision);
    }

    #[test]
    fn a_cancelled_sort_returns_cancelled() {
        let a = Alignment::from_text(SORTABLE);
        let progress = Progress::new();
        progress.cancel();
        let sorting = a.plan_sort(SortKey::Identity).unwrap();
        assert!(matches!(sorting.run(&progress), Err(Error::Cancelled)));
    }

    #[test]
    fn progress_ends_full() {
        let a = Alignment::from_text(SORTABLE);
        let progress = Progress::new();
        a.plan_sort(SortKey::Identity)
            .unwrap()
            .run(&progress)
            .unwrap();
        assert_eq!(progress.fraction(), Some(1.0));
    }

    #[test]
    fn scores_are_per_current_row() {
        let a = Alignment::from_text(SORTABLE);
        let sorted = a
            .plan_sort(SortKey::Identity)
            .unwrap()
            .run(&Progress::new())
            .unwrap();
        assert_eq!(sorted.scores, [0.75, 0.75, 1.0]);
        assert_eq!(sorted.order, [2, 0, 1]);
    }

    #[test]
    fn follow_reorders_without_an_undo_step() {
        let mut a = Alignment::from_text(SORTABLE);
        assert!(a.follow(&[2, 0, 1]));
        assert_eq!(names(&a), ["r3", "r1", "r2"]);
        assert_eq!(error(a.undo()), "nothing to undo");
        let revision = a.revision();
        assert!(!a.follow(&[0, 1, 2]));
        assert_eq!(a.revision(), revision);
    }

    #[test]
    fn order_stamp_moves_only_with_the_order() {
        let mut a = Alignment::from_text(SORTABLE);
        let first = a.order_stamp();
        run(&mut a, "fill --rows 1 --cols 1 --with N").unwrap();
        assert_eq!(a.order_stamp(), first);
        run(&mut a, "sort --by identity").unwrap();
        let sorted = a.order_stamp();
        assert_ne!(sorted, first);
        a.undo().unwrap();
        assert_ne!(a.order_stamp(), sorted);
        run(&mut a, "delete-rows --rows 1").unwrap();
        assert_ne!(a.order_stamp(), first);
    }
}
