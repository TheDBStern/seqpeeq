//! The state of one open tree's view: which clade is shown, the cursor,
//! the selection, and the rows fitted to the screen.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};

use seqpeeq_core::{LinkSummary, NodeId, Tree, TreeDoc, TreeFacts};

/// One row of the drawn tree: a tip, or a collapsed clade.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Row {
    pub node: NodeId,
    /// False for a tip.
    pub collapsed: bool,
}

/// A search of tip names, as found in one state of the tree.
#[derive(Clone, Debug, PartialEq)]
pub struct TreeSearch {
    pub pattern: String,
    /// The matching tips, top to bottom.
    pub matches: Vec<NodeId>,
    /// For each node number, how many matches its clade holds.
    pub inside: Vec<u32>,
    pub revision: u64,
}

impl TreeSearch {
    /// The tips whose name matches `pattern` (case ignored), each counted
    /// into every clade above it in one pass up the tree.
    pub fn find(doc: &TreeDoc, pattern: &str) -> seqpeeq_core::Result<TreeSearch> {
        let matches = doc.find_tips(pattern)?;
        let tree = doc.tree();
        let preorder = tree.preorder();
        let slots = preorder.iter().max().map_or(0, |&n| n as usize + 1);
        let mut inside = vec![0u32; slots];
        for &m in &matches {
            inside[m as usize] = 1;
        }
        for &n in preorder.iter().rev() {
            if let Some(parent) = tree.parent(n)
                && n != tree.root()
            {
                inside[parent as usize] += inside[n as usize];
            }
        }
        Ok(TreeSearch {
            pattern: pattern.to_string(),
            matches,
            inside,
            revision: doc.revision(),
        })
    }

    /// The match after `from` in drawing order (or before it, going back),
    /// wrapping around; `from` itself counts when `inclusive`. Gives its
    /// index among the matches.
    pub fn next(
        &self,
        facts: &TreeFacts,
        from: NodeId,
        forward: bool,
        inclusive: bool,
    ) -> Option<(usize, NodeId)> {
        if self.matches.is_empty() {
            return None;
        }
        let at = facts.order(from).unwrap_or(0);
        let order = |&m: &NodeId| facts.order(m).unwrap_or(0);
        // The first match at or after `from`.
        let after = self.matches.partition_point(|m| order(m) < at);
        let on = self.matches.get(after).is_some_and(|m| order(m) == at);
        let n = self.matches.len();
        let index = match (forward, inclusive && on) {
            (_, true) => after,
            (true, false) if on => (after + 1) % n,
            (true, false) => after % n,
            (false, false) => (after + n - 1) % n,
        };
        Some((index, self.matches[index]))
    }
}

#[derive(Clone, Debug)]
pub struct TreeView {
    /// The clade chosen with `+ - z`.
    pub zoom: NodeId,
    /// The clade drawn: `zoom`, or lower when the path to the cursor does
    /// not fit.
    pub shown: NodeId,
    pub cursor: NodeId,
    pub select: Option<NodeId>,
    pub opened: HashSet<NodeId>,
    pub closed: HashSet<NodeId>,
    /// Branches drawn with equal lengths.
    pub equal: bool,
    pub search: Option<TreeSearch>,
    pub found: Option<NodeId>,
    /// In drawing order, top to bottom.
    pub rows: Vec<Row>,
    /// The first row on screen.
    pub top: usize,
    /// The rows the screen has for the tree.
    pub height: usize,
    /// The tree's revision the view was last repaired for.
    pub revision: u64,
    pub link: Option<LinkSummary>,
    /// What `link` was worked out for: the tree's revision, the
    /// alignment's dataset id and its revision.
    pub link_key: Option<(u64, u64, u64)>,
    /// The row each open node's junction is drawn on.
    pub junction: HashMap<NodeId, usize>,
    /// The rows of each open node's first and last child.
    pub span: HashMap<NodeId, (usize, usize)>,
    /// The first and last row of each open node's clade.
    pub extent: HashMap<NodeId, (usize, usize)>,
    /// The largest distance and the most branches from the shown clade's
    /// node to a row's node.
    pub farthest: (f32, u32),
    /// Children `←` came up from, most recent last.
    came_from: Vec<NodeId>,
    /// The internal nodes drawn open.
    open: HashSet<NodeId>,
    /// The row the cursor is drawn on.
    at: Option<usize>,
    /// The root when last fitted, to follow a reroot.
    root: NodeId,
}

/// What opening a clade adds.
#[derive(Default)]
struct Expansion {
    opened: Vec<NodeId>,
    /// Collapsed clades that could be opened next.
    candidates: Vec<NodeId>,
    /// Rows added.
    added: usize,
}

/// How many children `n` has.
fn kids(tree: &Tree, n: NodeId) -> usize {
    tree.children(n).count()
}

/// The ancestors of `to` up to and including `from`, lowest first; `None`
/// when `to` is not in `from`'s clade.
fn path_up(tree: &Tree, from: NodeId, to: NodeId) -> Option<Vec<NodeId>> {
    let mut path = Vec::new();
    if to == from {
        return Some(path);
    }
    let mut at = tree.parent(to);
    while let Some(n) = at {
        path.push(n);
        if n == from {
            return Some(path);
        }
        at = tree.parent(n);
    }
    None
}

/// The smallest clade holding both `a` and `b`.
pub fn common_ancestor(tree: &Tree, facts: &TreeFacts, mut a: NodeId, mut b: NodeId) -> NodeId {
    let up = |n: NodeId| tree.parent(n).unwrap_or(n);
    while facts.steps(a) > facts.steps(b) {
        a = up(a);
    }
    while facts.steps(b) > facts.steps(a) {
        b = up(b);
    }
    while a != b && tree.parent(a).is_some() {
        (a, b) = (up(a), up(b));
    }
    a
}

impl TreeView {
    pub fn new(tree: &Tree) -> TreeView {
        let root = tree.root();
        TreeView {
            zoom: root,
            shown: root,
            cursor: root,
            select: None,
            opened: HashSet::new(),
            closed: HashSet::new(),
            equal: false,
            search: None,
            found: None,
            rows: Vec::new(),
            top: 0,
            height: 1,
            revision: 0,
            link: None,
            link_key: None,
            junction: HashMap::new(),
            span: HashMap::new(),
            extent: HashMap::new(),
            farthest: (0.0, 0),
            came_from: Vec::new(),
            open: HashSet::new(),
            at: None,
            root,
        }
    }

    /// Whether `n` is drawn as an open junction.
    pub fn is_open(&self, n: NodeId) -> bool {
        self.open.contains(&n)
    }

    /// Choose what is shown and which clades are open so the rows fit
    /// `height`, then lay out the rows.
    pub fn refit(&mut self, tree: &Tree, facts: &TreeFacts, height: usize) {
        let height = height.max(1);
        self.height = height;
        self.root = tree.root();
        if !facts.contains(self.zoom) {
            self.zoom = tree.root();
        }
        if !facts.contains(self.cursor) {
            self.cursor = self.zoom;
        }
        let path = match path_up(tree, self.zoom, self.cursor) {
            Some(path) => path,
            None => {
                self.zoom = tree.root();
                path_up(tree, self.zoom, self.cursor).unwrap_or_default()
            }
        };
        // The highest node on the path whose own path to the cursor fits;
        // the cursor's parent when none does.
        let mut shown = path.first().copied().unwrap_or(self.zoom);
        let mut keep = path.len().min(1);
        let mut rows = 1;
        for (i, &n) in path.iter().enumerate() {
            rows += kids(tree, n) - 1;
            if rows > height {
                break;
            }
            shown = n;
            keep = i + 1;
        }
        self.shown = shown;
        let path: HashSet<NodeId> = path[..keep].iter().copied().collect();

        self.open.clear();
        let mut rows = 1;
        let mut heap = BinaryHeap::new();
        // Clades holding search matches open before any without, so that
        // as many matches as fit get rows of their own.
        let inside = self
            .search
            .as_ref()
            .filter(|search| search.revision == self.revision)
            .map(|search| search.inside.clone());
        let holds_match = |c: NodeId| {
            inside
                .as_ref()
                .is_some_and(|inside| inside.get(c as usize).is_some_and(|&k| k > 0))
        };
        let take = |view: &mut TreeView, ex: Expansion, heap: &mut BinaryHeap<_>| {
            view.open.extend(ex.opened);
            for c in ex.candidates {
                heap.push((holds_match(c), facts.tips(c), Reverse(facts.order(c)), c));
            }
            ex.added
        };
        if !tree.is_tip(shown) {
            let ex = self
                .expand(tree, shown, &path, usize::MAX)
                .expect("no limit");
            rows += take(self, ex, &mut heap);
        }
        while rows < height
            && let Some((_, _, _, c)) = heap.pop()
        {
            if let Some(ex) = self.expand(tree, c, &path, height - rows) {
                rows += take(self, ex, &mut heap);
            }
        }
        self.lay_out(tree, facts);
    }

    /// Open `start` and the clades below it that are always open, unless
    /// that adds more than `limit` rows.
    fn expand(
        &self,
        tree: &Tree,
        start: NodeId,
        path: &HashSet<NodeId>,
        limit: usize,
    ) -> Option<Expansion> {
        let mut ex = Expansion::default();
        let mut stack = vec![start];
        while let Some(n) = stack.pop() {
            ex.opened.push(n);
            let mut k = 0;
            for c in tree.children(n) {
                k += 1;
                if ex.added + k - 1 > limit {
                    return None;
                }
                if tree.is_tip(c) {
                    continue;
                }
                let closed = self.closed.contains(&c);
                let unary = tree.children(c).nth(1).is_none();
                if path.contains(&c) || (!closed && (self.opened.contains(&c) || unary)) {
                    stack.push(c);
                } else if !closed {
                    ex.candidates.push(c);
                }
            }
            ex.added += k - 1;
        }
        Some(ex)
    }

    /// Rebuild the rows, the junctions and the cursor's row from the open
    /// set, and scroll so the cursor is on screen.
    fn lay_out(&mut self, tree: &Tree, facts: &TreeFacts) {
        self.rows.clear();
        self.junction.clear();
        self.span.clear();
        self.extent.clear();
        let (depth, steps) = (facts.depth(self.shown), facts.steps(self.shown));
        let mut farthest = (0.0f32, 0u32);
        let mut row = |rows: &mut Vec<Row>, node: NodeId| {
            rows.push(Row {
                node,
                collapsed: !tree.is_tip(node),
            });
            farthest.0 = farthest.0.max(facts.depth(node) - depth);
            farthest.1 = farthest.1.max(facts.steps(node).saturating_sub(steps));
            rows.len() - 1
        };
        if !self.open.contains(&self.shown) {
            row(&mut self.rows, self.shown);
        } else {
            // Each frame: a node, its children still to visit, the rows of
            // its first and last child, and the rows before it.
            let mut stack = vec![(self.shown, tree.children(self.shown), None, 0, 0)];
            loop {
                let Some(frame) = stack.last_mut() else {
                    break;
                };
                let y = match frame.1.next() {
                    Some(c) if self.open.contains(&c) => {
                        let before = self.rows.len();
                        stack.push((c, tree.children(c), None, 0, before));
                        continue;
                    }
                    Some(c) => row(&mut self.rows, c),
                    None => {
                        let (n, _, first, last, before) = stack.pop().expect("a frame");
                        let first = first.unwrap_or(last);
                        let y = (first + last) / 2;
                        self.junction.insert(n, y);
                        self.span.insert(n, (first, last));
                        let end = self.rows.len().saturating_sub(1).max(before);
                        self.extent.insert(n, (before, end));
                        y
                    }
                };
                if let Some(frame) = stack.last_mut() {
                    frame.2.get_or_insert(y);
                    frame.3 = y;
                }
            }
        }
        self.farthest = farthest;
        self.at = match self.junction.get(&self.cursor) {
            Some(&y) => Some(y),
            None => self.rows.iter().position(|row| row.node == self.cursor),
        };
        self.scroll();
    }

    /// Scroll just enough to show the cursor's row.
    fn scroll(&mut self) {
        let (n, height) = (self.rows.len(), self.height);
        if n <= height {
            self.top = 0;
            return;
        }
        if let Some(at) = self.at {
            if at < self.top {
                self.top = at;
            } else if at >= self.top + height {
                self.top = at + 1 - height;
            }
        }
        self.top = self.top.min(n - height);
    }

    /// The row the cursor is drawn on.
    #[cfg_attr(not(test), allow(dead_code))] // tests check scrolling with it
    pub fn cursor_row(&self) -> Option<usize> {
        self.at
    }

    /// Move the cursor `by` rows, stopping at the first and last.
    pub fn step(&mut self, tree: &Tree, facts: &TreeFacts, by: isize) {
        if let Some(at) = self.at {
            let to = at.saturating_add_signed(by).min(self.rows.len() - 1);
            let from = self.cursor;
            self.cursor = self.rows[to].node;
            self.came_from.clear();
            // Between two rows of one parent the path, and so the fit,
            // stays the same: only the cursor moves. On a node with a
            // million children this skips laying out a million rows.
            if !self.open.contains(&from)
                && self.rows[at].node == from
                && tree.parent(from) == tree.parent(self.cursor)
            {
                self.at = Some(to);
                return self.scroll();
            }
        }
        self.refit(tree, facts, self.height);
    }

    /// `←`: to the parent, not above the zoomed clade.
    pub fn go_parent(&mut self, tree: &Tree, facts: &TreeFacts) {
        if self.cursor != self.zoom
            && let Some(parent) = tree.parent(self.cursor)
        {
            self.came_from.push(self.cursor);
            self.cursor = parent;
        }
        self.refit(tree, facts, self.height);
    }

    /// `→`: back to the child `←` came from, or the first child.
    pub fn go_child(&mut self, tree: &Tree, facts: &TreeFacts) {
        let back = self
            .came_from
            .last()
            .copied()
            .filter(|&child| tree.parent(child) == Some(self.cursor));
        let to = match back {
            Some(child) => {
                self.came_from.pop();
                Some(child)
            }
            None => {
                self.came_from.clear();
                tree.children(self.cursor).next()
            }
        };
        if let Some(child) = to {
            self.closed.remove(&self.cursor);
            self.cursor = child;
        }
        self.refit(tree, facts, self.height);
    }

    /// Enter: open or close the cursor's clade.
    pub fn toggle(&mut self, tree: &Tree, facts: &TreeFacts) {
        let n = self.cursor;
        if !tree.is_tip(n) && n != self.shown {
            if self.open.contains(&n) {
                self.opened.remove(&n);
                self.closed.insert(n);
            } else {
                self.closed.remove(&n);
                self.opened.insert(n);
            }
        }
        self.refit(tree, facts, self.height);
    }

    /// `+`: show only the cursor's clade (a tip's parent's).
    pub fn zoom_in(&mut self, tree: &Tree, facts: &TreeFacts) {
        let to = if tree.is_tip(self.cursor) {
            tree.parent(self.cursor)
        } else {
            Some(self.cursor)
        };
        if let Some(to) = to {
            self.zoom = to;
            self.closed.remove(&to);
            self.came_from.clear();
        }
        self.refit(tree, facts, self.height);
    }

    /// `-`: show the parent of the zoomed clade.
    pub fn zoom_out(&mut self, tree: &Tree, facts: &TreeFacts) {
        if let Some(parent) = tree.parent(self.zoom) {
            self.zoom = parent;
            self.cursor_into_view(tree);
        }
        self.refit(tree, facts, self.height);
    }

    /// `z`: show the whole tree.
    pub fn zoom_whole(&mut self, tree: &Tree, facts: &TreeFacts) {
        self.zoom = tree.root();
        self.cursor_into_view(tree);
        self.refit(tree, facts, self.height);
    }

    /// Move the cursor up until its path from the zoomed clade fits, so
    /// that the zoomed clade is the one shown.
    fn cursor_into_view(&mut self, tree: &Tree) {
        self.came_from.clear();
        let Some(path) = path_up(tree, self.zoom, self.cursor) else {
            self.cursor = self.zoom;
            return;
        };
        let kids: Vec<usize> = path.iter().map(|&n| kids(tree, n)).collect();
        let mut rows = 1 + kids.iter().map(|k| k - 1).sum::<usize>();
        for (&n, &k) in path.iter().zip(&kids) {
            if rows <= self.height {
                break;
            }
            rows -= k - 1;
            self.cursor = n;
        }
    }

    /// Follow an edit, undo or redo: keep what still exists, drop what is
    /// gone, and fall back to the whole tree.
    pub fn repair(&mut self, tree: &Tree, facts: &TreeFacts, revision: u64) {
        self.revision = revision;
        // A new root turns clades inside out: the zoomed or selected node
        // may hold other tips now. A tip is still itself.
        let rerooted = tree.root() != self.root;
        if rerooted || !facts.contains(self.zoom) {
            self.zoom = tree.root();
        }
        self.select = self
            .select
            .filter(|&n| facts.contains(n) && (!rerooted || tree.is_tip(n)));
        self.found = self.found.filter(|&n| facts.contains(n));
        self.opened.retain(|&n| facts.contains(n));
        self.closed.retain(|&n| facts.contains(n));
        self.came_from.clear();
        if facts.contains(self.cursor) {
            self.refit(tree, facts, self.height);
        } else {
            self.cursor = self.zoom;
            self.refit(tree, facts, self.height);
            self.cursor = self.rows[0].node;
            self.refit(tree, facts, self.height);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use seqpeeq_core::{Command, Session, TreeDoc};

    pub(crate) const T: &str = "((A,B),(C,(D,E)),F);";

    /// A session holding one tree, opened from a temp file. Edits go
    /// through the session, the one path that changes data.
    pub(crate) struct Doc {
        session: Session,
        _dir: tempfile::TempDir,
    }

    impl Doc {
        pub(crate) fn get(&self) -> &TreeDoc {
            self.session.datasets()[0].tree().unwrap()
        }
        pub(crate) fn tree(&self) -> &Tree {
            self.get().tree()
        }
        pub(crate) fn facts(&self) -> &TreeFacts {
            self.get().facts()
        }
        pub(crate) fn revision(&self) -> u64 {
            self.get().revision()
        }
    }

    pub(crate) fn doc(text: &str) -> Doc {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.nwk");
        std::fs::write(&path, text).unwrap();
        let mut session = Session::new(None);
        let line = format!("open '{}'", path.display());
        session
            .execute(Command::parse_line(&line).unwrap())
            .unwrap();
        Doc { session, _dir: dir }
    }

    /// Run a command line, such as `tree prune --tip A` or `undo`.
    pub(crate) fn apply(doc: &mut Doc, line: &str) {
        doc.session
            .execute(Command::parse_line(line).unwrap())
            .unwrap();
    }

    /// The one tip named `name`.
    pub(crate) fn tip(tree: &Tree, name: &str) -> NodeId {
        let mut found = tree
            .tips()
            .into_iter()
            .filter(|&n| tree.label(n) == Some(name));
        let tip = found.next().unwrap();
        assert!(found.next().is_none(), "{name} is shared");
        tip
    }

    /// The rows as names; a collapsed clade as `▸` and its first tip.
    pub(crate) fn names(tree: &Tree, view: &TreeView) -> Vec<String> {
        view.rows
            .iter()
            .map(|row| {
                let mut n = row.node;
                while let Some(child) = tree.children(n).next() {
                    n = child;
                }
                let name = tree.label(n).unwrap_or_default();
                if row.collapsed {
                    format!("▸{name}")
                } else {
                    name.to_string()
                }
            })
            .collect()
    }

    /// The tree, its facts and a fresh view fitted to `height` rows.
    pub(crate) fn view(text: &str, height: usize) -> (Tree, TreeFacts, TreeView) {
        let doc = doc(text);
        let (t, f) = (doc.tree().clone(), doc.facts().clone());
        let mut v = TreeView::new(&t);
        v.refit(&t, &f, height);
        (t, f, v)
    }

    pub(crate) fn fitted(text: &str, height: usize) -> Vec<String> {
        let (t, _, v) = view(text, height);
        names(&t, &v)
    }

    #[test]
    fn clades_open_largest_first_then_top_to_bottom() {
        assert_eq!(fitted(T, 6), ["A", "B", "C", "D", "E", "F"]);
        assert_eq!(fitted(T, 5), ["A", "B", "C", "▸D", "F"]);
        assert_eq!(fitted(T, 4), ["▸A", "C", "▸D", "F"]);
        assert_eq!(fitted(T, 3), ["▸A", "▸C", "F"]);
        assert_eq!(
            fitted(T, 1),
            ["▸A", "▸C", "F"],
            "the shown clade is always open; rows scroll"
        );
    }

    #[test]
    fn clades_opened_or_closed_by_hand_stay_so() {
        let (t, f, mut v) = view(T, 5);
        v.cursor = t.parent(tip(&t, "A")).unwrap();
        v.toggle(&t, &f); // close AB
        assert_eq!(names(&t, &v), ["▸A", "C", "D", "E", "F"]);
        v.toggle(&t, &f); // and open it again
        assert_eq!(names(&t, &v), ["A", "B", "C", "▸D", "F"]);
    }

    #[test]
    fn the_path_to_the_cursor_is_open_and_the_shown_clade_moves_down_when_it_cannot_fit() {
        let (t, f, mut v) = view(T, 4);
        v.cursor = tip(&t, "E");
        v.refit(&t, &f, 4);
        assert_eq!(names(&t, &v), ["C", "D", "E"]);
        assert_eq!(v.shown, t.parent(tip(&t, "C")).unwrap());
        assert_eq!(v.zoom, t.root(), "the zoom chosen by hand is unchanged");
        v.zoom_whole(&t, &f);
        let de = t.parent(tip(&t, "E")).unwrap();
        assert_eq!(
            (v.shown, v.cursor),
            (t.root(), de),
            "the cursor moves up to what fits"
        );
        assert_eq!(names(&t, &v), ["▸A", "C", "▸D", "F"]);
    }

    #[test]
    fn moving_steps_rows_parents_and_back_down() {
        let (t, f, mut v) = view(T, 6);
        v.cursor = tip(&t, "D");
        v.go_parent(&t, &f);
        let de = t.parent(tip(&t, "D")).unwrap();
        assert_eq!(v.cursor, de);
        v.go_child(&t, &f);
        assert_eq!(v.cursor, tip(&t, "D"), "back to the child it came from");
        v.step(&t, &f, 1);
        assert_eq!(v.cursor, tip(&t, "E"));
        v.step(&t, &f, 10);
        assert_eq!(v.cursor, tip(&t, "F"), "stops at the last row");
    }

    #[test]
    fn a_star_too_wide_for_the_screen_scrolls_to_the_cursor() {
        let text = format!(
            "({});",
            (0..1000)
                .map(|i| format!("t{i}"))
                .collect::<Vec<_>>()
                .join(",")
        );
        let (t, f, mut v) = view(&text, 20);
        assert_eq!(v.rows.len(), 1000);
        v.step(&t, &f, 500);
        let row = v.cursor_row().unwrap();
        assert!((v.top..v.top + 20).contains(&row), "{row} {}", v.top);
    }

    #[test]
    fn repair_keeps_the_cursor_tip_and_drops_what_is_gone() {
        // Two tips stay after both prunes: a tree needs at least two.
        let mut doc = doc("((A,B),(C,(D,E)),F,G);");
        let mut v = TreeView::new(doc.tree());
        v.refit(doc.tree(), doc.facts(), 6);
        let (e, ab) = (
            tip(doc.tree(), "E"),
            doc.tree().parent(tip(doc.tree(), "A")).unwrap(),
        );
        v.cursor = e;
        v.select = Some(ab);
        v.opened.insert(ab);
        apply(&mut doc, "tree prune --mrca A --mrca B");
        v.repair(doc.tree(), doc.facts(), doc.revision());
        assert_eq!(
            (v.cursor, v.select, v.opened.contains(&ab)),
            (e, None, false)
        );
        let cde = doc.tree().parent(tip(doc.tree(), "C")).unwrap();
        v.zoom = cde;
        apply(&mut doc, "tree prune --mrca C --mrca E");
        v.repair(doc.tree(), doc.facts(), doc.revision());
        assert_eq!(
            (v.zoom, v.shown),
            (doc.tree().root(), doc.tree().root()),
            "gone: the whole tree"
        );
        assert_eq!(
            v.cursor, v.rows[0].node,
            "the cursor's tip is gone: the first row"
        );
        apply(&mut doc, "undo");
        v.repair(doc.tree(), doc.facts(), doc.revision());
        assert!(doc.facts().contains(v.cursor));
    }

    #[test]
    fn a_reroot_keeps_the_whole_tree_shown() {
        let mut doc = doc(T);
        let mut v = TreeView::new(doc.tree());
        v.refit(doc.tree(), doc.facts(), 6);
        v.cursor = tip(doc.tree(), "D");
        apply(&mut doc, "tree reroot --tip F");
        v.repair(doc.tree(), doc.facts(), doc.revision());
        assert_eq!((v.zoom, v.shown), (doc.tree().root(), doc.tree().root()));
        assert_eq!(v.rows.len(), 6);
        assert_eq!(v.cursor, tip(doc.tree(), "D"));
    }

    #[test]
    fn a_reroot_shows_the_whole_tree_when_the_zoomed_clade_changed() {
        let mut doc = doc(T);
        let mut v = TreeView::new(doc.tree());
        v.refit(doc.tree(), doc.facts(), 6);
        let (c, cde) = (
            tip(doc.tree(), "C"),
            doc.tree().parent(tip(doc.tree(), "C")).unwrap(),
        );
        v.cursor = c;
        v.zoom = cde;
        v.select = Some(cde);
        v.refit(doc.tree(), doc.facts(), 6);
        apply(&mut doc, "tree reroot --tip D");
        v.repair(doc.tree(), doc.facts(), doc.revision());
        assert_eq!(v.zoom, doc.tree().root(), "not the clade's complement");
        assert_eq!(v.select, None, "the selected clade holds other tips now");
        assert_eq!(v.cursor, c, "a tip is still itself");
    }

    /// A balanced tree of `2^depth` tips named t0, t1, ….
    fn balanced(depth: u32) -> String {
        fn clade(depth: u32, next: &mut u32) -> String {
            if depth == 0 {
                *next += 1;
                return format!("t{}", *next - 1);
            }
            format!("({},{})", clade(depth - 1, next), clade(depth - 1, next))
        }
        format!("{};", clade(depth, &mut 0))
    }

    fn searched(doc: &Doc, pattern: &str, height: usize) -> TreeView {
        let mut v = TreeView::new(doc.tree());
        v.revision = doc.revision();
        v.search = Some(TreeSearch::find(doc.get(), pattern).unwrap());
        v.refit(doc.tree(), doc.facts(), height);
        v
    }

    #[test]
    fn a_search_opens_the_clades_that_hold_matches() {
        let doc = doc(&balanced(6));
        let v = searched(&doc, "^t(3|40|62)$", 16);
        assert!(v.rows.len() <= 16);
        let matches = v.search.as_ref().unwrap().matches.clone();
        assert_eq!(matches.len(), 3);
        for m in matches {
            assert!(
                v.rows.iter().any(|r| r.node == m && !r.collapsed),
                "{:?} has its own row",
                doc.tree().label(m)
            );
        }
    }

    #[test]
    fn more_matches_than_rows_still_fit() {
        let doc = doc(&balanced(6));
        let v = searched(&doc, ".", 8);
        assert!(v.rows.len() <= 8, "{}", v.rows.len());
        let inside = &v.search.as_ref().unwrap().inside;
        for r in v.rows.iter().filter(|r| r.collapsed) {
            assert!(inside[r.node as usize] > 0);
        }
    }
}
