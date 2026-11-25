//! How a tree's tips are paired with an alignment's rows.
//!
//! A tip matches the rows whose whole name is the tip's name or, when no
//! row's is, the rows whose ID is: the name up to its first whitespace, as
//! in a FASTA header `>seqA Homo sapiens`. Tip names are never cut.

use std::collections::HashMap;

use crate::Alignment;

/// A name up to its first whitespace; a name without any is its own ID.
pub fn sequence_id(name: &str) -> &str {
    name.split(char::is_whitespace).next().unwrap_or(name)
}

/// An alignment's rows by whole name and by ID.
pub struct NameMatch<'a> {
    alignment: &'a Alignment,
    full: HashMap<&'a str, Vec<usize>>,
    id: HashMap<&'a str, Vec<usize>>,
}

impl<'a> NameMatch<'a> {
    pub fn new(alignment: &'a Alignment) -> Self {
        Self::with_ids(alignment, true)
    }

    /// Rows matched by whole name and, if `ids`, by ID; without IDs, as
    /// tips matched rows before IDs were (a replayed older log).
    pub fn with_ids(alignment: &'a Alignment, ids: bool) -> Self {
        let mut full: HashMap<&str, Vec<usize>> = HashMap::with_capacity(alignment.rows());
        let mut id: HashMap<&str, Vec<usize>> = HashMap::new();
        for row in 0..alignment.rows() {
            let name = alignment.name(row);
            full.entry(name).or_default().push(row);
            let short = sequence_id(name);
            if ids && short != name {
                id.entry(short).or_default().push(row);
            }
        }
        NameMatch {
            alignment,
            full,
            id,
        }
    }

    /// The rows the tip named `tip` matches, ascending: those with that
    /// whole name or, if none has it, those with that ID.
    pub fn rows(&self, tip: &str) -> &[usize] {
        self.full
            .get(tip)
            .or_else(|| self.id.get(tip))
            .map_or(&[], Vec::as_slice)
    }

    /// Whether the tip named `tip` matches any row.
    pub fn contains(&self, tip: &str) -> bool {
        !self.rows(tip).is_empty()
    }

    /// The new name of `row`, a row the tip named `tip` matches, when that
    /// tip is renamed `to`: `to` for a row of that whole name, or when `to`
    /// itself has a space; otherwise `to` in place of its ID, its
    /// description kept.
    pub fn renamed(&self, row: usize, tip: &str, to: &str) -> String {
        let name = self.alignment.name(row);
        if name == tip || sequence_id(to) != to {
            return to.to_string();
        }
        format!("{to}{}", &name[sequence_id(name).len()..])
    }

    /// Whether `row` would be matched by its ID alone: it has a
    /// description, IDs are matched, and no row's whole name is its ID.
    pub fn by_id(&self, row: usize) -> bool {
        let name = self.alignment.name(row);
        let id = sequence_id(name);
        id != name && !self.full.contains_key(id) && self.id.contains_key(id)
    }

    /// The ID, if any, that `renames` would give to rows that had different
    /// IDs, when a renamed row keeps a description and so would be matched
    /// by that ID: a tip of that name would then match several rows. Rows
    /// that share an ID may go on sharing one.
    pub fn id_clash(&self, renames: &[(usize, String)]) -> Option<String> {
        let old_id = |row: usize| sequence_id(self.alignment.name(row));
        // Each new ID a renamed row with a description takes, with the old
        // ID of the first row to take it.
        let mut taken: HashMap<&str, &str> = HashMap::new();
        for (row, new) in renames {
            let new_id = sequence_id(new);
            if new_id == new || new_id == old_id(*row) {
                continue;
            }
            if *taken.entry(new_id).or_insert(old_id(*row)) != old_id(*row) {
                return Some(new_id.to_string());
            }
        }
        if taken.is_empty() {
            return None;
        }
        let renamed: std::collections::HashSet<usize> = renames.iter().map(|(r, _)| *r).collect();
        for (new_id, from) in taken {
            let holders = self.full.get(new_id).into_iter().chain(self.id.get(new_id));
            if holders
                .flatten()
                .any(|&other| !renamed.contains(&other) && old_id(other) != from)
            {
                return Some(new_id.to_string());
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Alignment;

    #[test]
    fn an_id_ends_at_any_whitespace() {
        assert_eq!(sequence_id("seqA Homo"), "seqA");
        assert_eq!(sequence_id("seqA\tx"), "seqA");
        assert_eq!(sequence_id("seqA"), "seqA");
    }

    #[test]
    fn a_tip_matches_a_row_by_id() {
        let a = Alignment::from_text(">seqA Homo sapiens\nAC\n>seqB\nAC\n");
        let names = NameMatch::new(&a);
        assert_eq!(names.rows("seqA"), [0]);
        assert_eq!(names.rows("seqB"), [1]);
        assert!(names.rows("Homo").is_empty());
        assert!(names.contains("seqA"));
        assert!(!names.contains("seqC"));
    }

    #[test]
    fn a_full_name_beats_an_id() {
        let a = Alignment::from_text(">x old\nA\n>x\nA\n");
        assert_eq!(NameMatch::new(&a).rows("x"), [1]);
    }

    #[test]
    fn a_whole_name_with_spaces_still_matches() {
        let a = Alignment::from_text(">Homo sapiens\nA\n>Homo neanderthalensis\nA\n");
        let names = NameMatch::new(&a);
        assert_eq!(names.rows("Homo sapiens"), [0]);
        // Tips are never cut, so a tip named `Homo` is the only one to
        // match by ID here.
        assert_eq!(names.rows("Homo"), [0, 1]);
    }

    #[test]
    fn rows_sharing_an_id_all_match() {
        let a = Alignment::from_text(">x a\nA\n>x b\nA\n");
        assert_eq!(NameMatch::new(&a).rows("x"), [0, 1]);
    }

    #[test]
    fn renamed_keeps_the_description() {
        let a = Alignment::from_text(">seqA\tHomo sapiens\nA\n>seqB\nA\n");
        let names = NameMatch::new(&a);
        assert_eq!(names.renamed(0, "seqA", "seqC"), "seqC\tHomo sapiens");
        assert_eq!(names.renamed(1, "seqB", "seqD"), "seqD");
        assert_eq!(
            names.renamed(0, "seqA", "Q new"),
            "Q new",
            "a name with a space is used whole"
        );
    }
}
