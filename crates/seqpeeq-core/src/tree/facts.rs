//! Facts about every node of one state of a tree, worked out once so that
//! drawing and moving about never walk the whole tree.

use super::{NONE, NodeId, Tree};

/// Per-node facts for one state of a tree. Node numbers not in that state
/// (removed, or from another state) read as empty: no tips, not contained.
#[derive(Clone, Debug, Default)]
pub struct TreeFacts {
    tips: Vec<u32>,
    /// Distance from the root; a missing length counts 0.
    depth: Vec<f32>,
    /// Branches from the root.
    steps: Vec<u32>,
    reach: Vec<f32>,
    reach_steps: Vec<u32>,
    first_tip: Vec<NodeId>,
    last_tip: Vec<NodeId>,
    /// Position in pre-order; `NONE` when not in the tree.
    order: Vec<u32>,
    total_tips: usize,
}

impl TreeFacts {
    /// Work out the facts of `tree`: one pass down and one back up, with
    /// no recursion.
    pub fn of(tree: &Tree) -> TreeFacts {
        let slots = tree.node_slots();
        let preorder = tree.preorder();
        let mut f = TreeFacts {
            tips: vec![0; slots],
            depth: vec![0.0; slots],
            steps: vec![0; slots],
            reach: vec![0.0; slots],
            reach_steps: vec![0; slots],
            first_tip: vec![NONE; slots],
            last_tip: vec![NONE; slots],
            order: vec![NONE; slots],
            total_tips: 0,
        };
        for (i, &n) in preorder.iter().enumerate() {
            let at = n as usize;
            f.order[at] = i as u32;
            if let Some(parent) = tree.parent(n)
                && n != tree.root()
            {
                let p = parent as usize;
                f.depth[at] = f.depth[p] + tree.length(n).unwrap_or(0.0) as f32;
                f.steps[at] = f.steps[p] + 1;
            }
        }
        // Children come after their parent in pre-order, so walking it
        // backwards meets every child before its parent, last child first.
        for &n in preorder.iter().rev() {
            let at = n as usize;
            if tree.is_tip(n) {
                f.tips[at] = 1;
                f.reach[at] = f.depth[at];
                f.reach_steps[at] = f.steps[at];
                f.first_tip[at] = n;
                f.last_tip[at] = n;
                f.total_tips += 1;
            }
            if n == tree.root() {
                continue;
            }
            let Some(parent) = tree.parent(n) else {
                continue;
            };
            let p = parent as usize;
            f.tips[p] += f.tips[at];
            if f.last_tip[p] == NONE {
                f.last_tip[p] = f.last_tip[at];
                f.reach[p] = f.reach[at];
                f.reach_steps[p] = f.reach_steps[at];
            } else {
                f.reach[p] = f.reach[p].max(f.reach[at]);
                f.reach_steps[p] = f.reach_steps[p].max(f.reach_steps[at]);
            }
            f.first_tip[p] = f.first_tip[at];
        }
        f
    }

    /// Tips in the clade at `n`.
    pub fn tips(&self, n: NodeId) -> u32 {
        self.tips.get(n as usize).copied().unwrap_or(0)
    }

    /// Distance from the root, missing lengths counting 0.
    pub fn depth(&self, n: NodeId) -> f32 {
        self.depth.get(n as usize).copied().unwrap_or(0.0)
    }

    /// Branches between the root and `n`.
    pub fn steps(&self, n: NodeId) -> u32 {
        self.steps.get(n as usize).copied().unwrap_or(0)
    }

    /// The largest depth of a tip in the clade at `n`.
    pub fn reach(&self, n: NodeId) -> f32 {
        self.reach.get(n as usize).copied().unwrap_or(0.0)
    }

    /// The most branches between the root and a tip in the clade at `n`.
    pub fn reach_steps(&self, n: NodeId) -> u32 {
        self.reach_steps.get(n as usize).copied().unwrap_or(0)
    }

    /// The clade's top tip as drawn.
    pub fn first_tip(&self, n: NodeId) -> NodeId {
        self.first_tip.get(n as usize).copied().unwrap_or(NONE)
    }

    /// The clade's bottom tip as drawn.
    pub fn last_tip(&self, n: NodeId) -> NodeId {
        self.last_tip.get(n as usize).copied().unwrap_or(NONE)
    }

    /// Position in pre-order, counting from 0; `None` for a node not in
    /// the tree.
    pub fn order(&self, n: NodeId) -> Option<u32> {
        self.order
            .get(n as usize)
            .copied()
            .filter(|&order| order != NONE)
    }

    /// Whether `n` is in the tree.
    pub fn contains(&self, n: NodeId) -> bool {
        self.order(n).is_some()
    }

    pub fn total_tips(&self) -> usize {
        self.total_tips
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::TreeDoc;
    use crate::tree::edit::tests::cmd;
    use crate::tree::newick::tests::{caterpillar, tree};

    #[test]
    fn facts_of_a_small_tree() {
        let t = tree("((A:1,B:2)90:3,(C,D:1):1,E:0.5);");
        let f = TreeFacts::of(&t);
        let n = |name| t.tip_named(name).unwrap();
        let ab = t.parent(n("A")).unwrap();
        assert_eq!((f.tips(t.root()), f.tips(ab), f.total_tips()), (5, 2, 5));
        assert_eq!((f.depth(n("B")), f.steps(n("B"))), (5.0, 2));
        assert_eq!(f.depth(n("C")), 1.0, "a missing length counts 0");
        assert_eq!(
            (f.reach(t.root()), f.reach(ab), f.reach_steps(t.root())),
            (5.0, 5.0, 2)
        );
        assert_eq!(
            (f.first_tip(t.root()), f.last_tip(t.root())),
            (n("A"), n("E"))
        );
        assert_eq!(
            (f.order(t.root()), f.order(ab), f.order(n("A"))),
            (Some(0), Some(1), Some(2))
        );
    }

    #[test]
    fn a_pruned_node_is_no_longer_contained() {
        let mut doc = TreeDoc::from_text("((A,B),C,D);");
        let ab = doc
            .tree()
            .parent(doc.tree().tip_named("A").unwrap())
            .unwrap();
        doc.apply(&cmd("prune --mrca A --mrca B")).unwrap();
        assert!(!doc.facts().contains(ab) && doc.facts().order(ab).is_none());
        assert_eq!(doc.facts().total_tips(), 2);
        doc.undo().unwrap();
        assert!(doc.facts().contains(ab) && doc.facts().total_tips() == 4);
    }

    #[test]
    fn facts_of_a_deep_caterpillar_do_not_recurse() {
        let t = tree(&caterpillar(200_000, ":1"));
        let f = TreeFacts::of(&t);
        assert_eq!(f.tips(t.root()), 200_001);
        assert_eq!(f.reach_steps(t.root()), 200_000);
    }
}
