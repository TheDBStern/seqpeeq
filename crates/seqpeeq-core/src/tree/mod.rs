//! Phylogenetic trees: the model, Newick reading and writing, and edits.

mod cols;

use std::sync::Arc;

use cols::Cols;

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
        let mut out = Vec::new();
        let mut stack = vec![self.root];
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

    /// Make `n` the root, detaching it from its parent first.
    pub(crate) fn set_root(&mut self, n: NodeId) {
        self.detach(n);
        self.root = n;
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
