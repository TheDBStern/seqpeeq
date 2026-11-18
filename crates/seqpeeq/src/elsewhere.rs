//! The list of reads in view that align elsewhere, grouped by where.

use std::collections::{HashMap, HashSet};

use seqpeeq_core::bam::{Read, Region};

use crate::bam_view::{ReadId, holds};
use crate::line::LineEdit;

const PAIRED: u16 = 0x1;
const PROPER: u16 = 0x2;
const UNMAPPED: u16 = 0x4;
const MATE_UNMAPPED: u16 = 0x8;
const SECONDARY: u16 = 0x100;
const SUPPLEMENTARY: u16 = 0x800;

/// How a read was found to go somewhere else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    /// An `SA` part.
    Split,
    Mate,
    /// An `XA` alternative.
    Alternative,
    /// The read in view is a secondary record (only in `?`).
    Secondary,
    /// The name search found a record there.
    Search,
}

impl Via {
    #[allow(dead_code)] // drawn in phase 5 task 7
    pub const ALL: [Via; 5] = [
        Via::Split,
        Via::Mate,
        Via::Alternative,
        Via::Secondary,
        Via::Search,
    ];
}

/// Reads going to one contig, or (`contig: None`) the reads whose other
/// places are not known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub contig: Option<usize>,
    /// The first and last positions the reads go to; (0, 0) for `?`.
    pub span: (u64, u64),
    /// In the order of the reads in view.
    pub reads: Vec<ReadId>,
    /// Reads counted by how they get there, indexed by `Via as usize`.
    pub vias: [usize; 5],
    /// In `?`: reads not yet searched for, and reads searched for with
    /// nothing found.
    pub pending: usize,
    pub none_found: usize,
    /// The names of the reads not yet searched for.
    pub pending_names: Vec<String>,
}

impl Group {
    fn new(contig: Option<usize>) -> Group {
        Group {
            contig,
            span: (u64::MAX, 0),
            reads: Vec::new(),
            vias: [0; 5],
            pending: 0,
            none_found: 0,
            pending_names: Vec::new(),
        }
    }
}

fn overlaps(read: &Read, r: &Region) -> bool {
    read.contig == r.contig && read.start <= r.end && read.end >= r.start
}

/// Where else each read in `view` goes, grouped by contig: largest group
/// first, ties by contig, `?` last. A place on the window's contig and
/// inside it does not count.
pub fn groups(
    reads: &[Read],
    view: &Region,
    window: &Region,
    found: &HashMap<String, Vec<Read>>,
    searched: &HashSet<String>,
) -> Vec<Group> {
    let mut by_contig: HashMap<usize, Group> = HashMap::new();
    let mut question = Group::new(None);
    let away = |contig: usize, start: u64, end: u64| {
        contig != window.contig || end < window.start || start > window.end
    };
    for read in reads.iter().filter(|r| overlaps(r, view)) {
        let id = ReadId::of(read);
        let mut places: Vec<(usize, u64, Via)> = Vec::new();
        let flags = read.flags;
        if let Some(mate) = read.mate
            && flags & PAIRED != 0
            && flags & MATE_UNMAPPED == 0
            && (mate.contig != read.contig || flags & PROPER == 0)
            && away(mate.contig, mate.pos, mate.pos)
        {
            places.push((mate.contig, mate.pos, Via::Mate));
        }
        for part in &read.parts {
            if away(part.place.contig, part.place.pos, part.place.pos) {
                places.push((part.place.contig, part.place.pos, Via::Split));
            }
        }
        for alt in &read.alts {
            if away(alt.place.contig, alt.place.pos, alt.place.pos) {
                places.push((alt.place.contig, alt.place.pos, Via::Alternative));
            }
        }
        if let Some(records) = found.get(&read.name) {
            for r in records.iter().filter(|r| r.flags & UNMAPPED == 0) {
                if away(r.contig, r.start, r.end) {
                    places.push((r.contig, r.start, Via::Search));
                }
            }
        }
        let secondary = flags & SECONDARY != 0;
        let unsure =
            secondary || (flags & SUPPLEMENTARY == 0 && read.mapq == 0 && read.alts.is_empty());
        if unsure && !places.iter().any(|p| p.2 == Via::Search) {
            question.reads.push(id.clone());
            if secondary {
                question.vias[Via::Secondary as usize] += 1;
            }
            if searched.contains(&read.name) {
                question.none_found += 1;
            } else {
                question.pending += 1;
                if !question.pending_names.contains(&read.name) {
                    question.pending_names.push(read.name.clone());
                }
            }
        }
        let mut counted: HashSet<(usize, usize)> = HashSet::new();
        for (contig, pos, via) in places {
            let group = by_contig
                .entry(contig)
                .or_insert_with(|| Group::new(Some(contig)));
            group.span = (group.span.0.min(pos), group.span.1.max(pos));
            if group.reads.last() != Some(&id) {
                group.reads.push(id.clone());
            }
            if counted.insert((contig, via as usize)) {
                group.vias[via as usize] += 1;
            }
        }
    }
    let mut out: Vec<Group> = by_contig.into_values().collect();
    out.sort_by_key(|g| (std::cmp::Reverse(g.reads.len()), g.contig));
    if !question.reads.is_empty() {
        question.span = (0, 0);
        out.push(question);
    }
    out
}

/// What a list was made from, to group again after a search.
#[derive(Debug, Clone)]
pub struct Basis {
    /// The reads in view when it was opened.
    pub reads: Vec<Read>,
    pub view: Region,
    pub window: Region,
}

/// The `A` list: groups, or the reads of the open one.
#[derive(Debug, Clone)]
pub struct ElsewhereList {
    pub basis: Option<Basis>,
    /// `/`: keys go to the filter until Enter or Esc.
    pub typing: bool,
    pub groups: Vec<Group>,
    /// The group whose reads are listed.
    pub open: Option<usize>,
    pub filter: LineEdit,
    /// The row picked, among `rows()`, and the first row shown.
    pub cursor: usize,
    pub top: usize,
}

impl ElsewhereList {
    pub fn new(groups: Vec<Group>) -> ElsewhereList {
        ElsewhereList {
            basis: None,
            typing: false,
            groups,
            open: None,
            filter: LineEdit::new(""),
            cursor: 0,
            top: 0,
        }
    }

    /// Group indexes, or indexes into the open group's reads, whose names
    /// hold the filter (a group shows when any of its reads does).
    pub fn rows(&self) -> Vec<usize> {
        let needle = self.filter.text().to_lowercase();
        let hit = |r: &ReadId| needle.is_empty() || holds(&r.name, &needle);
        match self.open.and_then(|i| self.groups.get(i)) {
            Some(group) => (0..group.reads.len())
                .filter(|&i| hit(&group.reads[i]))
                .collect(),
            None => (0..self.groups.len())
                .filter(|&i| self.groups[i].reads.iter().any(hit))
                .collect(),
        }
    }

    /// The list of `reads` in `view`, grouped.
    pub fn build(
        reads: Vec<Read>,
        view: Region,
        window: Region,
        found: &HashMap<String, Vec<Read>>,
        searched: &HashSet<String>,
    ) -> ElsewhereList {
        let mut list = ElsewhereList::new(groups(&reads, &view, &window, found, searched));
        list.basis = Some(Basis {
            reads,
            view,
            window,
        });
        list
    }

    /// Group again with what the search knows now, keeping the open group
    /// open if it is still there.
    pub fn regroup(&mut self, found: &HashMap<String, Vec<Read>>, searched: &HashSet<String>) {
        let Some(basis) = &self.basis else {
            return;
        };
        let open = self.open.and_then(|i| self.groups.get(i)).map(|g| g.contig);
        self.groups = groups(&basis.reads, &basis.view, &basis.window, found, searched);
        self.open = open.and_then(|c| self.groups.iter().position(|g| g.contig == c));
        if open.is_some() && self.open.is_none() {
            self.cursor = 0;
            self.top = 0;
        }
        self.cursor = self.cursor.min(self.rows().len().saturating_sub(1));
        self.top = self.top.min(self.cursor);
    }

    /// The read in view a group names.
    pub fn read(&self, id: &ReadId) -> Option<&Read> {
        let basis = self.basis.as_ref()?;
        basis.reads.iter().find(|r| ReadId::of(r) == *id)
    }

    /// The names in the open group, or in the group under the cursor, in
    /// order, each once.
    pub fn names(&self) -> Vec<String> {
        let group = match self.open {
            Some(i) => self.groups.get(i),
            None => self
                .rows()
                .get(self.cursor)
                .and_then(|&i| self.groups.get(i)),
        };
        let mut out: Vec<String> = Vec::new();
        for r in group.map(|g| g.reads.as_slice()).unwrap_or_default() {
            if !out.contains(&r.name) {
                out.push(r.name.clone());
            }
        }
        out
    }

    /// Names in `?` not yet searched for.
    pub fn pending_names(&self) -> Vec<String> {
        self.groups
            .iter()
            .filter(|g| g.contig.is_none())
            .flat_map(|g| g.pending_names.iter().cloned())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seqpeeq_core::bam::{BamSource, Region, parse_region};
    use seqpeeq_core::{Command, Session};

    fn source() -> (BamSource, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = seqpeeq_core::bam::testing::multi(dir.path());
        let mut session = Session::new(None);
        session.execute(Command::Open { path, kind: None }).unwrap();
        let source = session.datasets()[0].bam().unwrap().source().clone();
        (source, dir)
    }

    fn region(source: &BamSource, text: &str) -> Region {
        parse_region(text, source.contigs(), "multi.bam").unwrap().0
    }

    fn reads(source: &BamSource, r: &Region) -> Vec<Read> {
        source.reads(r, &seqpeeq_core::Progress::new()).unwrap()
    }

    fn names(g: &Group) -> Vec<&str> {
        g.reads.iter().map(|r| r.name.as_str()).collect()
    }

    fn vias(pairs: &[(Via, usize)]) -> [usize; 5] {
        let mut out = [0; 5];
        for &(via, n) in pairs {
            out[via as usize] = n;
        }
        out
    }

    #[test]
    fn reads_are_grouped_by_where_else_they_go() {
        let (s, _dir) = source();
        let g1 = region(&s, "g1");
        let gs = groups(&reads(&s, &g1), &g1, &g1, &HashMap::new(), &HashSet::new());
        assert_eq!(gs.len(), 3);
        assert_eq!((gs[0].contig, names(&gs[0])), (Some(1), vec!["x1", "z1"]));
        assert_eq!(gs[0].span, (301, 1501));
        assert_eq!(gs[0].vias, vias(&[(Via::Alternative, 1), (Via::Split, 1)]));
        assert_eq!((gs[1].contig, names(&gs[1])), (Some(2), vec!["x1", "p1"]));
        assert_eq!(gs[1].span, (501, 801));
        assert_eq!(gs[1].vias, vias(&[(Via::Alternative, 1), (Via::Mate, 1)]));
        assert_eq!((gs[2].contig, names(&gs[2])), (None, vec!["y1"]));
        assert_eq!((gs[2].pending, gs[2].none_found), (1, 0));
        assert!(
            gs.iter().flat_map(|g| &g.reads).all(|r| r.flags != 147),
            "x1 read 2 is a proper pair with its mate in the window"
        );
    }

    #[test]
    fn a_search_moves_reads_out_of_question() {
        let (s, _dir) = source();
        let g1 = region(&s, "g1");
        let all = s
            .search_names(
                &HashSet::from(["y1".to_string()]),
                &seqpeeq_core::Progress::new(),
            )
            .unwrap();
        assert_eq!(all.len(), 3);
        let found = HashMap::from([("y1".to_string(), all)]);
        let searched = HashSet::from(["y1".to_string()]);
        let gs = groups(&reads(&s, &g1), &g1, &g1, &found, &searched);
        assert_eq!(gs.len(), 2, "no ? group");
        assert_eq!(names(&gs[0]), ["x1", "y1", "z1"]);
        assert_eq!(names(&gs[1]), ["x1", "y1", "p1"]);
        assert_eq!(gs[0].vias[Via::Search as usize], 1);
        assert_eq!(gs[0].span, (301, 1501));

        let gs = groups(&reads(&s, &g1), &g1, &g1, &HashMap::new(), &searched);
        let q = gs.last().unwrap();
        assert_eq!((q.contig, names(q)), (None, vec!["y1"]));
        assert_eq!((q.pending, q.none_found), (0, 1));
    }

    #[test]
    fn a_secondary_record_in_view_is_in_question() {
        let (s, _dir) = source();
        let g2 = region(&s, "g2");
        let gs = groups(&reads(&s, &g2), &g2, &g2, &HashMap::new(), &HashSet::new());
        assert_eq!(gs.len(), 2);
        assert_eq!((gs[0].contig, names(&gs[0])), (Some(0), vec!["z1"]));
        assert_eq!(gs[0].vias, vias(&[(Via::Split, 1)]));
        assert_eq!((gs[1].contig, names(&gs[1])), (None, vec!["y1"]));
        assert_eq!(gs[1].vias, vias(&[(Via::Secondary, 1)]));
        assert_eq!(gs[1].pending, 1);
    }

    #[test]
    fn places_inside_the_window_do_not_count() {
        let (s, _dir) = source();
        let g1 = region(&s, "g1");
        let view = region(&s, "g1:1-500");
        let gs = groups(
            &reads(&s, &g1),
            &view,
            &g1,
            &HashMap::new(),
            &HashSet::new(),
        );
        let all: Vec<&str> = gs.iter().flat_map(names).collect();
        assert_eq!(all, ["x1", "x1", "y1"], "only reads overlapping the view");
        let narrow = region(&s, "g1:1-150");
        let mut improper = reads(&s, &g1);
        improper.iter_mut().for_each(|r| r.flags &= !0x2);
        let gs = groups(
            &improper,
            &narrow,
            &narrow,
            &HashMap::new(),
            &HashSet::new(),
        );
        assert_eq!(
            (gs[0].contig, gs[0].span),
            (Some(0), (401, 401)),
            "a mate off the window"
        );
        assert_eq!(gs[0].vias, vias(&[(Via::Mate, 1)]));
    }

    #[test]
    fn the_list_opens_a_group_and_filters_names() {
        let (s, _dir) = source();
        let g1 = region(&s, "g1");
        let gs = groups(&reads(&s, &g1), &g1, &g1, &HashMap::new(), &HashSet::new());
        let mut list = ElsewhereList::new(gs);
        assert_eq!(list.rows(), [0, 1, 2]);
        assert_eq!(list.pending_names(), ["y1"]);
        list.filter.set("P");
        assert_eq!(list.rows(), [1], "groups holding a matching name");
        list.filter.set("");
        list.open = Some(0);
        assert_eq!(list.rows(), [0, 1]);
        list.filter.set("z");
        assert_eq!(list.rows(), [1]);
    }
}
