//! Rerooting: on a branch, between an outgroup and the rest, or at the
//! midpoint of the longest path between two tips.

use std::collections::HashSet;

use super::edit::invalid;
use super::{NodeId, Tree, is_support};
use crate::Result;

/// Names for a message: the first three, then `…`.
pub(crate) fn some_names(names: &[&str]) -> String {
    let mut list: Vec<&str> = names.iter().take(3).copied().collect();
    if names.len() > 3 {
        list.push("…");
    }
    list.join(", ")
}

impl Tree {
    /// Put the root on the branch above `n`, `below` from `n` (half way when
    /// `None`). The new root's children are `n`, then the rest of the tree.
    ///
    /// The branch above a child of a two-child root is that child's branch
    /// joined with its sibling's, since the two are one branch of the
    /// unrooted tree. `n` must not be the root.
    pub(crate) fn reroot_above(&mut self, n: NodeId, below: Option<f64>) {
        let parent = self.parent(n).expect("not the root");
        let siblings: Vec<NodeId> = self.children(parent).collect();
        if parent == self.root()
            && let [a, b] = siblings[..]
        {
            // Already on this branch: move the root along it.
            let other = if a == n { b } else { a };
            let whole = match (self.length(n), self.length(other)) {
                (None, None) => None,
                (x, y) => Some(x.unwrap_or(0.0) + y.unwrap_or(0.0)),
            };
            let near = below.or(whole.map(|l| l / 2.0));
            self.set_length(n, near);
            self.set_length(other, whole.zip(near).map(|(l, near)| l - near));
            self.set_children(parent, &[n, other]);
            return;
        }

        // The path from n's parent up to the old root reverses.
        let mut path = vec![parent];
        while let Some(up) = self.parent(*path.last().expect("not empty")) {
            path.push(up);
        }
        let lengths: Vec<Option<f64>> = path.iter().map(|&v| self.length(v)).collect();
        let labels: Vec<Option<std::sync::Arc<str>>> = path
            .iter()
            .map(|&v| self.label.get(v as usize).clone())
            .collect();
        let supports =
            |label: &Option<std::sync::Arc<str>>| label.as_deref().is_some_and(is_support);
        // The old root goes when the path takes one of its two children.
        let old = *path.last().expect("not empty");
        let old_goes = self.children(old).count() <= 2;
        let whole = self.length(n);
        let near = below.or(whole.map(|l| l / 2.0));

        self.detach(n);
        for &v in &path {
            self.detach(v);
        }
        let root = self.push_node();
        self.attach(root, n);
        self.attach(root, parent);
        self.set_length(n, near);
        self.set_length(parent, whole.zip(near).map(|(l, near)| l - near));
        for i in 0..path.len() {
            // A support belongs to the branch above its node, which now
            // hangs the next node on the path; other labels stay put.
            let own = labels[i].clone().filter(|_| !supports(&labels[i]));
            let incoming = if i == 0 {
                None
            } else {
                labels[i - 1].clone().filter(|_| supports(&labels[i - 1]))
            };
            // A support arriving at an old root that is about to go is kept
            // over its own label, so it can pass to the branch it joins.
            let label = if i == path.len() - 1 && old_goes {
                incoming.or(own)
            } else {
                own.or(incoming)
            };
            self.set_label(path[i], label);
            if i > 0 {
                self.attach(path[i - 1], path[i]);
                self.set_length(path[i], lengths[i - 1]);
            }
        }
        self.set_root(root);

        // The old root: removed when left with one child, or none.
        if old != parent {
            match self.children(old).count() {
                0 => self.detach(old),
                1 => self.splice_out(old),
                _ => {}
            }
        }
    }

    /// The node whose branch splits `tips` from the rest, and whether
    /// `tips` are on the other side of it (above rather than below).
    pub(crate) fn outgroup_branch(&self, tips: &[NodeId]) -> Result<(NodeId, bool)> {
        let chosen: HashSet<NodeId> = tips.iter().copied().collect();
        let counts = self.tip_counts();
        let all = self.tips();
        if chosen.len() == all.len() {
            return invalid("the outgroup holds every tip");
        }
        // How many chosen tips each clade holds, in one pass from the tips up.
        let order = self.preorder();
        let mut inside = vec![0u32; self.node_slots()];
        for &n in order.iter().rev() {
            if chosen.contains(&n) {
                inside[n as usize] += 1;
            }
            if let Some(parent) = self.parent(n) {
                inside[parent as usize] += inside[n as usize];
            }
        }
        let wanted = chosen.len() as u32;
        let others = all.len() as u32 - wanted;
        // A clade holding exactly the chosen tips, or exactly the others.
        let side = order[1..].iter().find_map(|&n| {
            let (holds, of) = (inside[n as usize], counts[n as usize]);
            if holds == wanted && of == wanted {
                Some((n, false))
            } else if holds == 0 && of == others {
                Some((n, true))
            } else {
                None
            }
        });
        if let Some(found) = side {
            return Ok(found);
        }
        let names: Vec<&str> = tips
            .iter()
            .map(|&n| self.label(n).unwrap_or_default())
            .collect();
        invalid(format!(
            "{} are not one side of any branch",
            some_names(&names)
        ))
    }

    /// The middle of the longest path between two tips: a node, and how far
    /// up its branch from it.
    pub(crate) fn midpoint(&self) -> Result<(NodeId, f64)> {
        let order = self.preorder();
        let missing = order[1..]
            .iter()
            .filter(|&&n| self.length(n).is_none())
            .count();
        if missing > 0 {
            return invalid(format!(
                "midpoint rooting needs every branch length; {missing} {} missing",
                if missing == 1 { "is" } else { "are" }
            ));
        }
        let Some(&start) = order.iter().find(|&&n| self.is_tip(n)) else {
            unreachable!("a tree has a tip");
        };
        if order.len() < 2 {
            return invalid("a tree of one tip has no branch to root on");
        }
        let (far, _) = self.farthest(start);
        let (other, from) = self.farthest(far);
        // Walk back from `other` to `far` until half the distance is passed.
        let total = from[other as usize].0;
        let half = total / 2.0;
        let mut at = other;
        loop {
            let (dist, previous) = from[at as usize];
            let step = dist - from[previous as usize].0;
            if dist - step <= half {
                // The midpoint is on the edge between `at` and `previous`,
                // `dist - half` from `at`.
                let up_from_at = dist - half;
                return Ok(if self.parent(at) == Some(previous) {
                    (at, up_from_at)
                } else {
                    (previous, step - up_from_at)
                });
            }
            at = previous;
        }
    }

    /// The tip farthest from `start` over the unrooted tree, and for every
    /// node its distance from `start` and the node before it on the way.
    fn farthest(&self, start: NodeId) -> (NodeId, Vec<(f64, NodeId)>) {
        let mut from = vec![(f64::NAN, super::NONE); self.node_slots()];
        from[start as usize] = (0.0, start);
        let mut stack = vec![start];
        let mut best = (start, 0.0);
        while let Some(n) = stack.pop() {
            let dist = from[n as usize].0;
            if self.is_tip(n) && dist > best.1 {
                best = (n, dist);
            }
            let up = self.parent(n).map(|p| (p, self.length(n).unwrap_or(0.0)));
            let down = self.children(n).map(|c| (c, self.length(c).unwrap_or(0.0)));
            for (next, length) in up.into_iter().chain(down) {
                if from[next as usize].0.is_nan() {
                    from[next as usize] = (dist + length, n);
                    stack.push(next);
                }
            }
        }
        (best.0, from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::TreeDoc;
    use crate::tree::edit::tests::cmd;
    use crate::tree::newick::tests::{arb_tree, caterpillar, tree};
    use crate::tree::newick::to_newick;
    use proptest::prelude::*;
    use std::collections::BTreeSet;

    fn rerooted(text: &str, flags: &str) -> String {
        let mut doc = TreeDoc::from_text(text);
        doc.apply(&cmd(&format!("reroot {flags}")))
            .unwrap_or_else(|e| panic!("{flags}: {e}"));
        to_newick(doc.tree(), doc.tree().root())
    }

    fn reroot_err(text: &str, flags: &str) -> String {
        let mut doc = TreeDoc::from_text(text);
        doc.apply(&cmd(&format!("reroot {flags}")))
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn reroot_on_a_branch_reverses_the_path_and_removes_a_two_way_root() {
        assert_eq!(
            rerooted("((A:1,B:2)90:3,(C:4,D:5)80:6)root;", "--tip C"),
            "(C:2,(D:5,(A:1,B:2)90:9):2);\n"
        );
        assert_eq!(
            rerooted("((A:1,B:1):1,C:1,D:1);", "--tip A"),
            "(A:0.5,(B:1,(C:1,D:1):1):0.5);\n",
            "a three-way root stays as a node"
        );
        assert_eq!(
            rerooted("(((A,B)70,C)60,D,E);", "--tip A"),
            "(A,(B,(C,(D,E)60)70));\n",
            "supports stay with their splits; missing lengths stay missing"
        );
        assert_eq!(
            rerooted("((A,B)70/95,(C,D)88);", "--tip A"),
            "(A,(B,(C,D)88));\n",
            "the root's two halves are one branch: the lower support is kept"
        );
        assert_eq!(
            rerooted("((A:1,B:1)x:1,C:1,D:1);", "--tip A"),
            "(A:0.5,(B:1,(C:1,D:1):1)x:0.5);\n",
            "a label that is not a support stays with its node"
        );
    }

    #[test]
    fn a_support_on_the_removed_root_branch_moves_to_its_other_half() {
        assert_eq!(
            rerooted("((A,B)70,(C,D));", "--tip A"),
            "(A,(B,(C,D)70));\n"
        );
        assert_eq!(
            rerooted("((A,B)70,(C,D))root;", "--tip A"),
            "(A,(B,(C,D)70));\n"
        );
        assert_eq!(
            rerooted("((A,B)70,(C,D));", "--tip C"),
            "(C,(D,(A,B)70));\n"
        );
    }

    #[test]
    fn reroot_by_outgroup_and_at_the_midpoint() {
        const T: &str = "((A:1,B:1):1,(C:1,(D:1,E:1):1):1);";
        assert_eq!(
            rerooted(T, "--outgroup D --outgroup E"),
            "((D:1,E:1):0.5,(C:1,(A:1,B:1):2):0.5);\n"
        );
        assert_eq!(
            rerooted(T, "--outgroup C --outgroup D --outgroup E"),
            "((C:1,(D:1,E:1):1):1,(A:1,B:1):1);\n",
            "a child of a two-child root: the middle of the joined branch"
        );
        assert_eq!(
            rerooted(T, "--outgroup A --outgroup B --outgroup C"),
            "((C:1,(A:1,B:1):2):0.5,(D:1,E:1):0.5);\n",
            "the outgroup is the other side of a branch: it comes first"
        );
        assert_eq!(
            rerooted("((A:1,B:1):3,C:1);", "--mrca A --mrca B"),
            "((A:1,B:1):2,C:2);\n"
        );
        assert_eq!(
            reroot_err(T, "--outgroup A --outgroup C"),
            "A, C are not one side of any branch"
        );
        assert_eq!(
            reroot_err(
                T,
                "--outgroup A --outgroup B --outgroup C --outgroup D --outgroup E"
            ),
            "the outgroup holds every tip"
        );
        assert_eq!(
            rerooted("((A:1,B:1):1,C:6);", "--midpoint"),
            "(C:4,(A:1,B:1):3);\n"
        );
        assert_eq!(
            reroot_err("((A,B):1,C:2);", "--midpoint"),
            "midpoint rooting needs every branch length; 2 are missing"
        );
        assert_eq!(reroot_err(T, "--node 1"), "that is already the root");
        for flags in ["--tip A --midpoint", "", "--outgroup A --midpoint"] {
            assert_eq!(
                reroot_err(T, flags),
                "say where to root: --tip, --mrca, --node, --outgroup or --midpoint",
                "{flags}"
            );
        }
    }

    #[test]
    fn reroot_says_where_it_rooted() {
        const T: &str = "((A:1,B:1):1,(C:1,(D:1,E:1):1):1);";
        let mut doc = TreeDoc::from_text(T);
        let mut said = |flags: &str| doc.apply(&cmd(&format!("reroot {flags}"))).unwrap().message;
        assert_eq!(said("--tip A"), "rerooted on the branch above A");
        assert_eq!(
            said("--mrca D --mrca E"),
            "rerooted on the branch above the clade of D and E"
        );
        assert_eq!(said("--node 3"), "rerooted on the branch above node 3");
        assert_eq!(
            said("--outgroup A --outgroup B"),
            "rerooted between A, B and the rest"
        );
        assert_eq!(said("--midpoint"), "rerooted at the midpoint");
    }

    /// Each split as the sorted names on the side without the first name.
    fn splits(t: &Tree) -> BTreeSet<Vec<String>> {
        let all: BTreeSet<String> = t
            .tips()
            .into_iter()
            .map(|n| t.label(n).unwrap().to_string())
            .collect();
        let first = all.iter().next().unwrap().clone();
        let mut out = BTreeSet::new();
        for n in t.preorder() {
            if n == t.root() {
                continue;
            }
            let mut side: BTreeSet<String> = BTreeSet::new();
            let mut stack = vec![n];
            while let Some(m) = stack.pop() {
                if t.is_tip(m) {
                    side.insert(t.label(m).unwrap().to_string());
                }
                stack.extend(t.children(m));
            }
            if side.contains(&first) {
                side = all.difference(&side).cloned().collect();
            }
            out.insert(side.into_iter().collect());
        }
        out
    }

    /// The sum of the lengths given, and of their sizes.
    fn total_length(t: &Tree) -> (f64, f64) {
        let lengths = t
            .preorder()
            .into_iter()
            .filter(|&n| n != t.root())
            .filter_map(|n| t.length(n));
        lengths.fold((0.0, 0.0), |(sum, size), l| (sum + l, size + l.abs()))
    }

    proptest! {
        #[test]
        fn reroot_keeps_the_unrooted_tree(t in arb_tree(true), pick in any::<prop::sample::Index>()) {
            // Same set of splits and the same total of the lengths given.
            // Supports are pinned by the unit tests above.
            let n = *pick.get(&t.preorder()[1..]);
            let mut r = t.clone();
            r.reroot_above(n, None);
            prop_assert_eq!(splits(&r), splits(&t));
            let ((before, size), (after, _)) = (total_length(&t), total_length(&r));
            if size < 1e100 {
                prop_assert!((after - before).abs() <= 1e-9 * size.max(1.0), "{} vs {}", before, after);
            }
        }
    }

    #[test]
    fn an_outgroup_on_a_deep_caterpillar_is_found_in_one_pass() {
        // The rest of the tips is nearly every tip: finding their common
        // ancestor one pair at a time would walk the whole depth for each.
        let mut t = tree(&caterpillar(30_000, ":1"));
        let tips = [
            t.tip_named("t29999").unwrap(),
            t.tip_named("t30000").unwrap(),
        ];
        let started = std::time::Instant::now();
        let (n, other_side) = t.outgroup_branch(&tips).unwrap();
        assert!(
            started.elapsed().as_secs_f64() < 2.0,
            "{:?}",
            started.elapsed()
        );
        assert!(other_side, "the last two tips are not a clade; the rest is");
        t.reroot_above(n, None);
        assert_eq!(t.tip_count(), 30_001);
    }

    #[test]
    fn reroot_on_a_deep_caterpillar_does_not_recurse() {
        let mut t = tree(&caterpillar(200_000, ":1"));
        let t0 = t.tips()[0];
        t.reroot_above(t0, None);
        assert_eq!(t.tip_count(), 200_001);
        let (midpoint, below) = t.midpoint().unwrap();
        t.reroot_above(midpoint, Some(below));
        assert_eq!(t.tip_count(), 200_001);
    }
}
