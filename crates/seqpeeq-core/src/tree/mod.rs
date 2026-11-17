//! Phylogenetic trees: the model, Newick reading and writing, and edits.

mod cols;
pub(crate) mod edit;
mod facts;
pub(crate) mod newick;
pub(crate) mod reroot;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cols::Cols;
pub use facts::TreeFacts;
pub use newick::Parsed;

use crate::Result;
use crate::command::tree::{NodeSel, TreeCmd};
use crate::msa::write::write_fasta;
use crate::msa::{Alignment, Applied, UNDO_LEVELS, View, count};
use crate::progress::Progress;
use edit::{invalid, resolve};

/// A node's number within one line of states of a tree. Removed nodes are
/// unlinked, never erased, so a number stays the same node until the tree
/// is rebuilt.
pub type NodeId = u32;

/// No node: the root's parent, a tip's children, the last sibling's next.
pub(crate) const NONE: NodeId = NodeId::MAX;

/// A rooted tree. Each node field is a column of shared chunks, so a copy
/// for undo shares everything an edit does not touch.
#[derive(Clone, Debug)]
pub struct Tree {
    parent: Cols<NodeId>,
    first_child: Cols<NodeId>,
    last_child: Cols<NodeId>,
    next_sibling: Cols<NodeId>,
    prev_sibling: Cols<NodeId>,
    /// Branch length to the parent; NaN when missing.
    length: Cols<f64>,
    label: Cols<Option<Arc<str>>>,
    root: NodeId,
}

/// Whether a label reads as a support value: one or more decimal numbers
/// separated by `/`, such as `81`, `0.95` or `81/95`.
pub(crate) fn is_support(label: &str) -> bool {
    label.split('/').all(|part| {
        let mut dots = 0;
        let mut digits = 0;
        for c in part.chars() {
            match c {
                '0'..='9' => digits += 1,
                '.' => dots += 1,
                _ => return false,
            }
        }
        digits > 0 && dots <= 1
    })
}

fn some(id: NodeId) -> Option<NodeId> {
    (id != NONE).then_some(id)
}

impl Tree {
    /// A tree of one node, which is its root.
    pub(crate) fn new_root() -> Tree {
        let mut tree = Tree {
            parent: Cols::new(),
            first_child: Cols::new(),
            last_child: Cols::new(),
            next_sibling: Cols::new(),
            prev_sibling: Cols::new(),
            length: Cols::new(),
            label: Cols::new(),
            root: 0,
        };
        tree.push_node();
        tree
    }

    fn push_node(&mut self) -> NodeId {
        let id = NodeId::try_from(self.parent.len())
            .ok()
            .filter(|&id| id != NONE)
            .expect("a tree holds fewer than 4 billion nodes");
        for col in [
            &mut self.parent,
            &mut self.first_child,
            &mut self.last_child,
            &mut self.next_sibling,
            &mut self.prev_sibling,
        ] {
            col.push(NONE);
        }
        self.length.push(f64::NAN);
        self.label.push(None);
        id
    }

    pub fn root(&self) -> NodeId {
        self.root
    }

    /// How many node numbers are in use, including removed nodes.
    pub(crate) fn node_slots(&self) -> usize {
        self.parent.len()
    }

    pub fn parent(&self, n: NodeId) -> Option<NodeId> {
        some(*self.parent.get(n as usize))
    }

    pub fn children(&self, n: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        std::iter::successors(some(*self.first_child.get(n as usize)), |&c| {
            some(*self.next_sibling.get(c as usize))
        })
    }

    pub fn is_tip(&self, n: NodeId) -> bool {
        *self.first_child.get(n as usize) == NONE
    }

    pub fn length(&self, n: NodeId) -> Option<f64> {
        let length = *self.length.get(n as usize);
        (!length.is_nan()).then_some(length)
    }

    pub fn label(&self, n: NodeId) -> Option<&str> {
        self.label.get(n as usize).as_deref()
    }

    /// The label of an internal node when it is a support value.
    pub fn support(&self, n: NodeId) -> Option<&str> {
        self.label(n)
            .filter(|label| !self.is_tip(n) && is_support(label))
    }

    /// Every node reachable from the root, parents before children,
    /// children in order.
    pub fn preorder(&self) -> Vec<NodeId> {
        self.preorder_from(self.root)
    }

    /// The clade at `n` in pre-order.
    pub fn preorder_from(&self, n: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = vec![n];
        while let Some(n) = stack.pop() {
            out.push(n);
            let mut child = *self.last_child.get(n as usize);
            while child != NONE {
                stack.push(child);
                child = *self.prev_sibling.get(child as usize);
            }
        }
        out
    }

    /// The tips in pre-order: top to bottom as drawn.
    pub fn tips(&self) -> Vec<NodeId> {
        self.preorder()
            .into_iter()
            .filter(|&n| self.is_tip(n))
            .collect()
    }

    pub fn tip_count(&self) -> usize {
        self.preorder()
            .into_iter()
            .filter(|&n| self.is_tip(n))
            .count()
    }

    /// The tips below each node, indexed by node number; 0 for nodes not
    /// in the tree.
    pub fn tip_counts(&self) -> Vec<u32> {
        let mut counts = vec![0u32; self.node_slots()];
        for n in self.preorder().into_iter().rev() {
            if self.is_tip(n) {
                counts[n as usize] = 1;
            }
            if let Some(parent) = self.parent(n) {
                counts[parent as usize] += counts[n as usize];
            }
        }
        counts
    }

    /// A new node, the last child of `parent`.
    pub(crate) fn add_child(&mut self, parent: NodeId) -> NodeId {
        let n = self.push_node();
        self.attach(parent, n);
        n
    }

    pub(crate) fn set_length(&mut self, n: NodeId, length: Option<f64>) {
        self.length.set(n as usize, length.unwrap_or(f64::NAN));
    }

    pub(crate) fn set_label(&mut self, n: NodeId, label: Option<Arc<str>>) {
        self.label.set(n as usize, label);
    }

    /// Unlink `n` from its parent; it and its clade leave the tree.
    pub(crate) fn detach(&mut self, n: NodeId) {
        let i = n as usize;
        let parent = *self.parent.get(i);
        if parent == NONE {
            return;
        }
        let (prev, next) = (*self.prev_sibling.get(i), *self.next_sibling.get(i));
        if prev == NONE {
            self.first_child.set(parent as usize, next);
        } else {
            self.next_sibling.set(prev as usize, next);
        }
        if next == NONE {
            self.last_child.set(parent as usize, prev);
        } else {
            self.prev_sibling.set(next as usize, prev);
        }
        self.parent.set(i, NONE);
        self.prev_sibling.set(i, NONE);
        self.next_sibling.set(i, NONE);
    }

    /// Make the detached node `n` the last child of `parent`.
    pub(crate) fn attach(&mut self, parent: NodeId, n: NodeId) {
        debug_assert_eq!(*self.parent.get(n as usize), NONE, "{n} is attached");
        let last = *self.last_child.get(parent as usize);
        if last == NONE {
            self.first_child.set(parent as usize, n);
        } else {
            self.next_sibling.set(last as usize, n);
        }
        self.prev_sibling.set(n as usize, last);
        self.next_sibling.set(n as usize, NONE);
        self.last_child.set(parent as usize, n);
        self.parent.set(n as usize, parent);
    }

    /// Put `parent`'s children in the given order. `children` must be
    /// exactly its children.
    pub(crate) fn set_children(&mut self, parent: NodeId, children: &[NodeId]) {
        let mut prev = NONE;
        for &child in children {
            debug_assert_eq!(*self.parent.get(child as usize), parent);
            self.prev_sibling.set(child as usize, prev);
            if prev == NONE {
                self.first_child.set(parent as usize, child);
            } else {
                self.next_sibling.set(prev as usize, child);
            }
            prev = child;
        }
        if prev == NONE {
            self.first_child.set(parent as usize, NONE);
        } else {
            self.next_sibling.set(prev as usize, NONE);
        }
        self.last_child.set(parent as usize, prev);
    }

    /// Whether every node field shares every chunk with `other`.
    #[cfg(test)]
    pub(crate) fn shares_all(&self, other: &Tree) -> bool {
        self.parent.shares_all(&other.parent)
            && self.first_child.shares_all(&other.first_child)
            && self.last_child.shares_all(&other.last_child)
            && self.next_sibling.shares_all(&other.next_sibling)
            && self.prev_sibling.shares_all(&other.prev_sibling)
            && self.length.shares_all(&other.length)
            && self.label.shares_all(&other.label)
    }

    /// Make `n` the root, detaching it from its parent first.
    pub(crate) fn set_root(&mut self, n: NodeId) {
        self.detach(n);
        self.root = n;
    }
}

/// An open tree: its current state, undo history, and what has been written.
#[derive(Debug)]
pub struct TreeDoc {
    tree: Tree,
    /// Facts about `tree`, worked out whenever it changes.
    facts: TreeFacts,
    /// Numbers each state; restored with the state on undo and redo.
    revision: u64,
    next_revision: u64,
    /// The revision last written whole to a file, or as opened.
    written: u64,
    undo: Vec<(Tree, u64, &'static str)>,
    redo: Vec<(Tree, u64, &'static str)>,
    /// Parts of the open message.
    notes: Vec<String>,
}

impl TreeDoc {
    /// Read a tree file.
    pub fn load_with(path: &Path, progress: &Progress) -> Result<TreeDoc> {
        Ok(TreeDoc::new(Tree::load(path, progress)?))
    }

    fn new(parsed: Parsed) -> TreeDoc {
        let Parsed {
            tree,
            skipped,
            comments,
        } = parsed;
        let mut notes = Vec::new();
        if skipped > 0 {
            let trees = if skipped == 1 { "tree" } else { "trees" };
            notes.push(format!("skipped {skipped} more {trees} in the file"));
        }
        if comments {
            notes.push("comments were dropped".to_string());
        }
        let shared = shared_names(&tree);
        if shared > 0 {
            let names = if shared == 1 {
                "1 name is".to_string()
            } else {
                format!("{shared} names are")
            };
            notes.push(format!(
                "{names} shared by more than one tip; name those tips with --node"
            ));
        }
        TreeDoc {
            facts: TreeFacts::of(&tree),
            tree,
            revision: 0,
            next_revision: 1,
            written: 0,
            undo: Vec::new(),
            redo: Vec::new(),
            notes,
        }
    }

    /// A tree from Newick text that must be valid.
    #[cfg(test)]
    pub(crate) fn from_text(text: &str) -> TreeDoc {
        TreeDoc::new(newick::parse(text.as_bytes()).unwrap())
    }

    pub fn tree(&self) -> &Tree {
        &self.tree
    }

    /// Facts about the tree as it now is.
    pub fn facts(&self) -> &TreeFacts {
        &self.facts
    }

    /// The tips whose name matches `pattern`, a regular expression with
    /// letter case ignored, top to bottom as drawn.
    pub fn find_tips(&self, pattern: &str) -> Result<Vec<NodeId>> {
        let re = regex::RegexBuilder::new(pattern)
            .case_insensitive(true)
            .build()
            .or_else(|_| invalid(format!("bad regular expression '{pattern}'")))?;
        Ok(self
            .tree
            .tips()
            .into_iter()
            .filter(|&n| re.is_match(self.tree.label(n).unwrap_or_default()))
            .collect())
    }

    /// Make `tree` the current state, returning the one it replaces. Every
    /// change of state goes through here, so the facts stay current.
    fn set(&mut self, tree: Tree, revision: u64) -> Tree {
        let before = std::mem::replace(&mut self.tree, tree);
        self.revision = revision;
        // Let go of the old facts first: two sets at once is the peak.
        self.facts = TreeFacts::default();
        self.facts = TreeFacts::of(&self.tree);
        before
    }

    /// Changes with every edit, undo and redo.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Whether there are edits not written whole to a file.
    pub fn is_modified(&self) -> bool {
        self.revision != self.written
    }

    pub fn tips(&self) -> usize {
        self.facts.total_tips()
    }

    /// `500 tips`.
    pub fn shape(&self) -> String {
        count(self.tips(), "tip")
    }

    /// What the open message should mention besides the tip count.
    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    pub(crate) fn mark_written(&mut self) {
        self.written = self.revision;
    }

    pub(crate) fn snapshot(&self) -> (Tree, u64) {
        (self.tree.clone(), self.revision)
    }

    /// Put back a snapshot. The undo history is left as it is.
    pub(crate) fn restore(&mut self, (tree, revision): (Tree, u64)) {
        self.set(tree, revision);
    }

    pub(crate) fn apply(&mut self, cmd: &TreeCmd) -> Result<Applied> {
        match cmd {
            TreeCmd::Extract { .. } => unreachable!("the session runs extracts as jobs"),
            TreeCmd::Prune { at } => {
                let n = resolve(&self.tree, at)?;
                self.edit("prune", |tree| {
                    let removed = tree.prune(n)?;
                    Ok(format!(
                        "pruned {}; now {}",
                        count(removed, "tip"),
                        count(tree.tip_count(), "tip")
                    ))
                })
            }
            TreeCmd::Ladderize { reverse } => self.edit("ladderize", |tree| {
                tree.ladderize(*reverse);
                Ok(if *reverse {
                    "ladderized, most tips first".to_string()
                } else {
                    "ladderized".to_string()
                })
            }),
            TreeCmd::Rename {
                at, to: Some(to), ..
            } => {
                if at.is_empty() {
                    return invalid("say which tip: --tip or --node");
                }
                let n = resolve(&self.tree, at)?;
                if !self.tree.is_tip(n) {
                    return match at.node {
                        Some(number) => invalid(format!("--node {number} is not a tip")),
                        None => {
                            invalid("--mrca names a clade; rename one tip with --tip or --node")
                        }
                    };
                }
                let mut was = String::new();
                let mut applied = self.edit("rename", |tree| {
                    was = tree.rename_tip(n, to)?;
                    Ok(format!("renamed '{was}' to '{}'", to.trim()))
                })?;
                applied.note = Some(format!("was: {was}"));
                Ok(applied)
            }
            TreeCmd::Rename {
                at,
                regex: Some(pattern),
                replace: Some(replace),
                ..
            } => {
                if !at.is_empty() {
                    return invalid(
                        "--regex renames every tip; leave out --tip, --mrca and --node",
                    );
                }
                self.edit("rename", |tree| {
                    let changed = tree.rename_matching(pattern, replace)?;
                    Ok(format!(
                        "renamed {changed} of {}",
                        count(tree.tip_count(), "tip")
                    ))
                })
            }
            TreeCmd::Rename { .. } => {
                invalid("say the new name with --to, or use --regex with --replace")
            }
            TreeCmd::Reroot {
                at,
                outgroup,
                midpoint,
            } => self.reroot(at, outgroup, *midpoint),
            other => invalid(format!("tree {} is not built yet", other.name())),
        }
    }

    fn reroot(&mut self, at: &NodeSel, outgroup: &[String], midpoint: bool) -> Result<Applied> {
        let ways =
            usize::from(!at.is_empty()) + usize::from(!outgroup.is_empty()) + usize::from(midpoint);
        if ways != 1 {
            return invalid("say where to root: --tip, --mrca, --node, --outgroup or --midpoint");
        }
        if midpoint {
            let (n, below) = self.tree.midpoint()?;
            return self.edit("reroot", |tree| {
                tree.reroot_above(n, Some(below));
                Ok("rerooted at the midpoint".to_string())
            });
        }
        if !outgroup.is_empty() {
            let tips = outgroup
                .iter()
                .map(|name| self.tree.tip_named(name))
                .collect::<Result<Vec<_>>>()?;
            let (n, other_side) = self.tree.outgroup_branch(&tips)?;
            let names: Vec<&str> = outgroup.iter().map(String::as_str).collect();
            let message = format!(
                "rerooted between {} and the rest",
                reroot::some_names(&names)
            );
            return self.edit("reroot", |tree| {
                tree.reroot_above(n, None);
                if other_side {
                    let root = tree.root();
                    let mut children: Vec<NodeId> = tree.children(root).collect();
                    children.reverse();
                    tree.set_children(root, &children);
                }
                Ok(message)
            });
        }
        let n = resolve(&self.tree, at)?;
        if n == self.tree.root() {
            return invalid("that is already the root");
        }
        // Above a node holding every tip lie only nodes with one child: the
        // branch has nothing on its other side.
        if self.facts.tips(n) as usize == self.facts.total_tips() {
            return invalid(
                "no tip lies outside that clade, so its branch has no other side to root on",
            );
        }
        let above = match (&at.tip, at.mrca.as_slice(), at.node) {
            (Some(tip), _, _) => tip.clone(),
            (_, [a, b], _) => format!("the clade of {a} and {b}"),
            (_, _, Some(number)) => format!("node {number}"),
            _ => unreachable!("resolve named a node"),
        };
        self.edit("reroot", |tree| {
            tree.reroot_above(n, None);
            Ok(format!("rerooted on the branch above {above}"))
        })
    }

    pub(crate) fn undo(&mut self) -> Result<Applied> {
        let Some((tree, revision, name)) = self.undo.pop() else {
            return invalid("nothing to undo");
        };
        let was = self.revision;
        let before = self.set(tree, revision);
        self.redo.push((before, was, name));
        Ok(applied(format!("undid {name}; now {}", self.shape())))
    }

    pub(crate) fn redo(&mut self) -> Result<Applied> {
        let Some((tree, revision, name)) = self.redo.pop() else {
            return invalid("nothing to redo");
        };
        let was = self.revision;
        let before = self.set(tree, revision);
        self.undo.push((before, was, name));
        Ok(applied(format!("redid {name}; now {}", self.shape())))
    }

    /// Change the tree, remembering the state before for undo. A change
    /// that fails leaves the tree as it was.
    pub(crate) fn edit(
        &mut self,
        name: &'static str,
        change: impl FnOnce(&mut Tree) -> Result<String>,
    ) -> Result<Applied> {
        let mut tree = self.tree.clone();
        let message = change(&mut tree)?;
        let was = self.revision;
        let before = self.set(tree, self.next_revision);
        self.next_revision += 1;
        self.undo.push((before, was, name));
        if self.undo.len() > UNDO_LEVELS {
            self.undo.remove(0);
        }
        self.redo.clear();
        Ok(applied(message))
    }

    /// Check a `tree extract` and get it ready to run.
    pub(crate) fn plan_extract(&self, at: &NodeSel, out: &Path) -> Result<TreeExtraction> {
        let from = if at.is_empty() {
            self.tree.root()
        } else {
            resolve(&self.tree, at)?
        };
        Ok(TreeExtraction {
            tree: self.tree.clone(),
            from,
            out: out.to_path_buf(),
            seqs: None,
        })
    }
}

fn applied(message: String) -> Applied {
    Applied {
        message,
        note: None,
        created: Vec::new(),
    }
}

/// How many names are each shared by more than one tip.
fn shared_names(tree: &Tree) -> usize {
    let mut seen: HashMap<&str, u32> = HashMap::new();
    for tip in tree.tips() {
        if let Some(name) = tree.label(tip) {
            *seen.entry(name).or_default() += 1;
        }
    }
    seen.values().filter(|&&n| n > 1).count()
}

/// A `tree extract` checked and ready to write.
pub(crate) struct TreeExtraction {
    tree: Tree,
    from: NodeId,
    out: PathBuf,
    seqs: Option<SeqsOut>,
}

/// The sequences of an extracted clade, from the linked alignment.
struct SeqsOut {
    view: View,
    /// In tree order, top to bottom.
    rows: Vec<usize>,
    wrap: Option<usize>,
    out: PathBuf,
    /// Tips of the clade with no sequence of their name.
    missing: Vec<String>,
    /// Names used that the alignment has more than once.
    repeated: usize,
}

/// The full path, so the log says where a file went whatever the working
/// directory was.
fn full_path(out: &Path) -> PathBuf {
    std::fs::canonicalize(out).unwrap_or_else(|_| out.to_path_buf())
}

impl TreeExtraction {
    pub(crate) fn out(&self) -> &Path {
        &self.out
    }

    /// Whether the whole tree is written.
    pub(crate) fn is_whole(&self) -> bool {
        self.from == self.tree.root()
    }

    /// Also write the clade's sequences from `alignment` (called `name` in
    /// messages) to `out`, whole rows of the alignment as it now is.
    pub(crate) fn add_seqs(
        &mut self,
        alignment: &Alignment,
        name: &str,
        out: &Path,
        wrap: Option<usize>,
    ) -> Result<()> {
        if wrap == Some(0) {
            return invalid("--wrap must be at least 1");
        }
        if out == self.out {
            return invalid("the tree and its sequences need two different files");
        }
        let mut first: HashMap<&str, usize> = HashMap::new();
        let mut repeated_names: HashSet<&str> = HashSet::new();
        // The first row of each name is the one used.
        for row in 0..alignment.rows() {
            let seq = alignment.name(row);
            if first.contains_key(seq) {
                repeated_names.insert(seq);
            } else {
                first.insert(seq, row);
            }
        }
        let (mut rows, mut missing, mut repeated) = (Vec::new(), Vec::new(), 0);
        for tip in self.tree.preorder_from(self.from) {
            if !self.tree.is_tip(tip) {
                continue;
            }
            let label = self.tree.label(tip).unwrap_or_default();
            match first.get(label) {
                Some(&row) => {
                    rows.push(row);
                    repeated += usize::from(repeated_names.contains(label));
                }
                None => missing.push(label.to_string()),
            }
        }
        if rows.is_empty() {
            return invalid(format!("no tip in the clade has a sequence in {name}"));
        }
        self.seqs = Some(SeqsOut {
            view: alignment.view().clone(),
            rows,
            wrap,
            out: out.to_path_buf(),
            missing,
            repeated,
        });
        Ok(())
    }

    /// Refuse output files that exist, before either is written.
    pub(crate) fn check_outputs(&self) -> Result<()> {
        let seqs = self.seqs.as_ref().map(|seqs| seqs.out.as_path());
        for out in std::iter::once(self.out.as_path()).chain(seqs) {
            if out.symlink_metadata().is_ok() {
                return Err(crate::Error::Exists(out.to_path_buf()));
            }
        }
        Ok(())
    }

    /// Write the file, or both files. If the sequences cannot be written
    /// the tree file is removed again.
    pub(crate) fn run(&self, progress: &Progress) -> Result<Applied> {
        if progress.is_cancelled() {
            return Err(crate::Error::Cancelled);
        }
        let size = newick::save_newick(&self.tree, self.from, &self.out, progress)?;
        let tips = self.tree.tip_counts()[self.from as usize] as usize;
        let out = &self.out;
        let mut message = format!("wrote {} ({})", out.display(), count(tips, "tip"));
        let mut note = format!("output {} size={size}", full_path(out).display());
        let mut created = vec![out.to_path_buf()];
        if let Some(seqs) = &self.seqs {
            let written = write_fasta(&seqs.view, &seqs.rows, None, seqs.wrap, &seqs.out, progress);
            let size = match written {
                Ok(size) => size,
                Err(error) => {
                    let _ = std::fs::remove_file(out);
                    return Err(error);
                }
            };
            message.push_str(&format!(
                " and {} ({})",
                seqs.out.display(),
                count(seqs.rows.len(), "sequence")
            ));
            if !seqs.missing.is_empty() {
                let names: Vec<&str> = seqs.missing.iter().map(String::as_str).collect();
                let has = if names.len() == 1 { "has" } else { "have" };
                message.push_str(&format!(
                    "; {} {has} no sequence: {}",
                    count(names.len(), "tip"),
                    reroot::some_names(&names)
                ));
            }
            if seqs.repeated > 0 {
                let is = if seqs.repeated == 1 { "is" } else { "are" };
                message.push_str(&format!(
                    "; {} {is} in the alignment more than once; its first row was used",
                    count(seqs.repeated, "name")
                ));
            }
            note.push_str(&format!(
                "; output {} size={size}",
                full_path(&seqs.out).display()
            ));
            created.push(seqs.out.clone());
        }
        Ok(Applied {
            message,
            note: Some(note),
            created,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tree_is_built_and_walked_in_order() {
        // ((A:1,B:2)90:3,C); built by hand
        let mut t = Tree::new_root();
        let ab = t.add_child(t.root());
        let (a, b) = (t.add_child(ab), t.add_child(ab));
        let c = t.add_child(t.root());
        for (n, name) in [(a, "A"), (b, "B"), (c, "C")] {
            t.set_label(n, Some(name.into()));
        }
        t.set_label(ab, Some("90".into()));
        t.set_length(a, Some(1.0));
        t.set_length(b, Some(2.0));
        t.set_length(ab, Some(3.0));
        assert_eq!(t.preorder(), [t.root(), ab, a, b, c]);
        assert_eq!(t.tips(), [a, b, c]);
        assert_eq!((t.tip_count(), t.tip_counts()[ab as usize]), (3, 2));
        assert_eq!(
            (t.support(ab), t.support(a), t.length(c)),
            (Some("90"), None, None)
        );
        assert_eq!(t.children(t.root()).collect::<Vec<_>>(), [ab, c]);
        t.detach(ab);
        assert_eq!((t.preorder(), t.parent(ab)), (vec![t.root(), c], None));
    }

    #[test]
    fn children_are_reordered_and_a_clade_becomes_the_root() {
        let mut t = Tree::new_root();
        let (a, b, c) = (t.add_child(0), t.add_child(0), t.add_child(0));
        t.set_children(0, &[c, a, b]);
        assert_eq!(t.preorder(), [0, c, a, b]);
        t.set_children(0, &[b, c, a]);
        assert_eq!(t.children(0).collect::<Vec<_>>(), [b, c, a]);
        let d = t.add_child(b);
        t.set_root(b);
        assert_eq!((t.root(), t.parent(b), t.preorder()), (b, None, vec![b, d]));
    }

    #[test]
    fn edits_that_change_nothing_copy_nothing() {
        let mut t = newick::tests::tree(&newick::tests::caterpillar(10_000, ":1"));
        t.ladderize(false);
        let before = t.clone();
        t.ladderize(false);
        assert!(t.shares_all(&before), "ladderizing again copies no chunk");
        let t0 = t.tip_named("t0").unwrap();
        t.reroot_above(t0, None);
        // The new root is added to the last chunk; every other is shared.
        let chunks = before.label.shared_chunks(&before.label);
        assert_eq!(
            t.label.shared_chunks(&before.label),
            chunks - 1,
            "a reroot leaves names alone"
        );
    }

    #[test]
    fn find_tips_ignores_case_and_keeps_drawing_order() {
        let doc = TreeDoc::from_text("((Homo_a,pan_b),(HOMO_c,d));");
        let names: Vec<&str> = doc
            .find_tips("homo")
            .unwrap()
            .iter()
            .map(|&n| doc.tree().label(n).unwrap())
            .collect();
        assert_eq!(names, ["Homo_a", "HOMO_c"]);
        assert_eq!(
            doc.find_tips("(").unwrap_err().to_string(),
            "bad regular expression '('"
        );
    }

    #[test]
    fn support_is_numbers_separated_by_slashes() {
        for (label, support) in [
            ("81", true),
            ("0.95", true),
            ("81/95", true),
            ("100/0.9/88", true),
            ("root", false),
            ("81/", false),
            ("", false),
            ("1e5", false),
        ] {
            assert_eq!(is_support(label), support, "{label}");
        }
    }

    #[test]
    fn a_deep_caterpillar_is_walked_without_recursion() {
        let mut t = Tree::new_root();
        let mut at = t.root();
        for _ in 0..200_000 {
            t.add_child(at);
            at = t.add_child(at);
        }
        assert_eq!((t.preorder().len(), t.tip_count()), (400_001, 200_001));
    }
}
