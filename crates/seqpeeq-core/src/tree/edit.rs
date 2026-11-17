//! Naming nodes in commands, and the edits.

use std::collections::{HashMap, HashSet};

use super::{NodeId, Tree};
use crate::command::tree::NodeSel;
use crate::{Error, Result};

pub(crate) fn invalid<T>(message: impl Into<String>) -> Result<T> {
    Err(Error::Invalid(message.into()))
}

impl Tree {
    /// The one tip named `name`.
    pub(crate) fn tip_named(&self, name: &str) -> Result<NodeId> {
        let mut found = self
            .tips()
            .into_iter()
            .filter(|&n| self.label(n) == Some(name));
        match (found.next(), found.count()) {
            (None, _) => invalid(format!("no tip is named '{name}'")),
            (Some(tip), 0) => Ok(tip),
            (Some(_), more) => invalid(format!(
                "'{name}' names {} tips; name one with --node (see the tree view)",
                more + 1
            )),
        }
    }

    /// The most recent common ancestor of two nodes in the tree.
    pub(crate) fn mrca(&self, a: NodeId, b: NodeId) -> NodeId {
        let mut above_a = HashSet::new();
        let mut at = Some(a);
        while let Some(n) = at {
            above_a.insert(n);
            at = self.parent(n);
        }
        let mut at = b;
        while !above_a.contains(&at) {
            at = self.parent(at).expect("both nodes are in the tree");
        }
        at
    }
}

/// The node a command names.
pub(crate) fn resolve(tree: &Tree, at: &NodeSel) -> Result<NodeId> {
    if let Some(name) = &at.tip {
        return tree.tip_named(name);
    }
    if !at.mrca.is_empty() {
        let [a, b] = at.mrca.as_slice() else {
            return invalid("--mrca takes two tips");
        };
        let a = tree.tip_named(a)?;
        let b = tree.tip_named(b)?;
        return Ok(tree.mrca(a, b));
    }
    if let Some(number) = at.node {
        let order = tree.preorder();
        return match number.checked_sub(1).and_then(|i| order.get(i)) {
            Some(&n) => Ok(n),
            None => invalid(format!(
                "there is no node {number} (the tree has {})",
                order.len()
            )),
        };
    }
    invalid("say which node: --tip, --mrca or --node")
}

/// Two branches joined into one: the sum of the lengths given, missing only
/// when both are.
fn joined(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)),
    }
}

impl Tree {
    /// Remove the node `p`, which has exactly one child, joining its branch
    /// to the child's. The child keeps its label and takes `p`'s place; a
    /// child without a label takes `p`'s support, since the joined branch
    /// is the same split.
    pub(crate) fn splice_out(&mut self, p: NodeId) {
        let child = self.children(p).next().expect("one child");
        let length = joined(self.length(p), self.length(child));
        if self.label(child).is_none()
            && let Some(support) = self.label(p).filter(|label| super::is_support(label))
        {
            let support: std::sync::Arc<str> = support.into();
            self.set_label(child, Some(support));
        }
        match self.parent(p) {
            None => self.set_root(child),
            Some(grand) => {
                let order: Vec<NodeId> = self
                    .children(grand)
                    .map(|n| if n == p { child } else { n })
                    .collect();
                self.detach(child);
                self.detach(p);
                self.attach(grand, child);
                self.set_children(grand, &order);
            }
        }
        self.set_length(child, length);
    }

    /// Remove the clade at `n`. A node left with one child is removed and
    /// its branch joined to the child's. Returns the tips removed.
    pub(crate) fn prune(&mut self, n: NodeId) -> Result<usize> {
        let Some(mut parent) = self.parent(n) else {
            return invalid("cannot prune the whole tree");
        };
        let counts = self.tip_counts();
        let removed = counts[n as usize] as usize;
        let left = counts[self.root() as usize] as usize - removed;
        if left < 2 {
            return invalid(format!(
                "a tree needs at least two tips; this would leave {left}"
            ));
        }
        self.detach(n);
        // A node that held only the pruned clade goes too.
        while self.is_tip(parent) {
            let above = self.parent(parent).expect("tips remain above");
            self.detach(parent);
            parent = above;
        }
        if self.children(parent).nth(1).is_none() {
            self.splice_out(parent);
        }
        Ok(removed)
    }

    /// Order every node's children by tip count, fewest first (most first
    /// with `reverse`). Ties keep their order.
    pub(crate) fn ladderize(&mut self, reverse: bool) {
        let counts = self.tip_counts();
        let mut children = Vec::new();
        for n in self.preorder() {
            children.clear();
            children.extend(self.children(n));
            if children.len() < 2 {
                continue;
            }
            if reverse {
                children.sort_by_key(|&c| std::cmp::Reverse(counts[c as usize]));
            } else {
                children.sort_by_key(|&c| counts[c as usize]);
            }
            self.set_children(n, &children);
        }
    }

    /// Refuse renames that would give a name to more than one tip. Only
    /// names given by the rename count: names already shared stay allowed.
    fn check_clashes(&self, renames: &[(NodeId, String)]) -> Result<()> {
        let renamed: HashSet<NodeId> = renames.iter().map(|(n, _)| *n).collect();
        // Only the new names are counted, so renaming one tip of a large
        // tree builds no map of every name. Per new name: the tips left alone that have it, the tips given
        // it, and the first tip given it, whose old name the others are
        // compared with (16 bytes a name, for renames of a million tips).
        #[derive(Clone, Copy)]
        struct Uses {
            untouched: u32,
            given: u32,
            first: NodeId,
            old_names_differ: bool,
        }
        let mut uses: HashMap<&str, Uses> = HashMap::with_capacity(renames.len());
        for &(tip, ref name) in renames {
            let entry = uses.entry(name.as_str()).or_insert(Uses {
                untouched: 0,
                given: 0,
                first: tip,
                old_names_differ: false,
            });
            entry.given += 1;
            entry.old_names_differ |= self.label(entry.first) != self.label(tip);
        }
        for tip in self.tips() {
            if !renamed.contains(&tip)
                && let Some(name) = self.label(tip)
                && let Some(entry) = uses.get_mut(name)
            {
                entry.untouched += 1;
            }
        }
        // A clash joins tips whose names differed before: a name that
        // several tips already shared may stay shared. Each clash is
        // reported once, in the order the names were given.
        let mut clashes: Vec<(&str, u32)> = Vec::new();
        for (_, name) in renames {
            let entry = uses.get_mut(name.as_str()).expect("counted above");
            if entry.untouched > 0 || entry.old_names_differ {
                clashes.push((name, entry.untouched + entry.given));
                (entry.untouched, entry.old_names_differ) = (0, false);
            }
        }
        match clashes.as_slice() {
            [] => Ok(()),
            [(name, total)] => invalid(format!("that would give {total} tips the name '{name}'")),
            several => {
                let mut list: Vec<String> = several
                    .iter()
                    .take(3)
                    .map(|(name, _)| format!("'{name}'"))
                    .collect();
                if several.len() > 3 {
                    list.push("…".to_string());
                }
                invalid(format!(
                    "that would give more than one tip each of the names {}",
                    list.join(", ")
                ))
            }
        }
    }

    /// Rename the tip `n`, trimming surrounding whitespace. Returns its old
    /// name.
    pub(crate) fn rename_tip(&mut self, n: NodeId, to: &str) -> Result<String> {
        let to = to.trim();
        if to.is_empty() {
            return invalid("a name cannot be empty");
        }
        self.check_clashes(&[(n, to.to_string())])?;
        let was = self.label(n).unwrap_or_default().to_string();
        self.set_label(n, Some(to.into()));
        Ok(was)
    }

    /// Replace matches of `pattern` in every tip name. Returns how many
    /// names changed.
    pub(crate) fn rename_matching(&mut self, pattern: &str, replace: &str) -> Result<usize> {
        let re = crate::msa::regex(pattern)?;
        crate::msa::check_replacement(&re, replace)?;
        let mut matched = false;
        let mut renames = Vec::new();
        for tip in self.tips() {
            let Some(name) = self.label(tip) else {
                continue;
            };
            if !re.is_match(name) {
                continue;
            }
            matched = true;
            let renamed = re.replace_all(name, replace);
            let renamed = renamed.trim();
            if renamed.is_empty() {
                return invalid(format!("a name cannot be empty (tip '{name}')"));
            }
            if renamed != name {
                renames.push((tip, renamed.to_string()));
            }
        }
        if !matched {
            return invalid(format!("no tip name contains a match for '{pattern}'"));
        }
        if renames.is_empty() {
            return invalid("that replacement leaves every name as it is");
        }
        self.check_clashes(&renames)?;
        let changed = renames.len();
        for (tip, name) in renames {
            self.set_label(tip, Some(name.into()));
        }
        Ok(changed)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::command::Command;
    use crate::command::tree::TreeCmd;
    use crate::msa::testing::measure;
    use crate::tree::TreeDoc;
    use crate::tree::newick::tests::{caterpillar, names, tree};
    use crate::tree::newick::to_newick;

    /// `tree <line>` as a command.
    pub(crate) fn cmd(line: &str) -> TreeCmd {
        match Command::parse_line(&format!("tree {line}")).unwrap() {
            Command::Tree(cmd) => cmd,
            other => panic!("{other:?}"),
        }
    }

    fn after(text: &str, edit: impl FnOnce(&mut Tree)) -> String {
        let mut t = tree(text);
        edit(&mut t);
        to_newick(&t, t.root())
    }

    fn tip(t: &Tree, name: &str) -> NodeId {
        t.tip_named(name).unwrap()
    }

    #[test]
    fn prune_joins_the_branches_it_leaves() {
        const T: &str = "((A:1,B:2):3,(C:1,D:1):1);";
        let pruned = after(T, |t| assert_eq!(t.prune(tip(t, "A")).unwrap(), 1));
        assert_eq!(pruned, "(B:5,(C:1,D:1):1);\n");
        let pruned = after(T, |t| {
            let cd = t.parent(tip(t, "C")).unwrap();
            assert_eq!(t.prune(cd).unwrap(), 2);
        });
        assert_eq!(
            pruned, "(A:1,B:2);\n",
            "a root left with one child gives way to it"
        );
        let pruned = after("((A,B)x,C);", |t| {
            t.prune(tip(t, "C")).unwrap();
        });
        assert_eq!(pruned, "(A,B)x;\n");
        let pruned = after("((A,B:2)80,C,(D,E)90:1);", |t| {
            t.prune(tip(t, "A")).unwrap();
        });
        assert_eq!(
            pruned, "(B:2,C,(D,E)90:1);\n",
            "a missing length adds nothing"
        );
        let pruned = after("(((A,B)70:1,C)60:2,D);", |t| {
            t.prune(tip(t, "C")).unwrap();
        });
        assert_eq!(
            pruned, "((A,B)70:3,D);\n",
            "the lower node's support is kept"
        );
        let mut two = tree("(A,B);");
        let a = two.tips()[0];
        assert_eq!(
            two.prune(a).unwrap_err().to_string(),
            "a tree needs at least two tips; this would leave 1"
        );
        assert_eq!(
            two.prune(two.root()).unwrap_err().to_string(),
            "cannot prune the whole tree"
        );
    }

    #[test]
    fn prune_keeps_the_upper_support_when_the_lower_node_has_none() {
        let pruned = after("(((A,B),C)60,D,E);", |t| {
            t.prune(tip(t, "C")).unwrap();
        });
        assert_eq!(pruned, "((A,B)60,D,E);\n");
    }

    #[test]
    fn prune_removes_a_chain_of_nodes_left_without_tips() {
        let pruned = after("(((A)),B,C);", |t| {
            t.prune(tip(t, "A")).unwrap();
        });
        assert_eq!(pruned, "(B,C);\n");
    }

    #[test]
    fn ladderize_orders_children_by_tip_count_keeping_ties() {
        const T: &str = "((A,B),C,(D,(E,F)));";
        assert_eq!(after(T, |t| t.ladderize(false)), "(C,(A,B),(D,(E,F)));\n");
        assert_eq!(after(T, |t| t.ladderize(true)), "(((E,F),D),(A,B),C);\n");
    }

    #[test]
    fn renames_trim_and_refuse_clashes() {
        let mut t = tree("(a_1,a_2,b);");
        let first = t.tips()[0];
        assert_eq!(t.rename_tip(first, "  x y ").unwrap(), "a_1");
        assert_eq!(t.label(first), Some("x y"));
        assert_eq!(
            t.rename_tip(first, "b").unwrap_err().to_string(),
            "that would give 2 tips the name 'b'"
        );
        assert_eq!(
            t.rename_tip(first, "  ").unwrap_err().to_string(),
            "a name cannot be empty"
        );
        assert_eq!(t.rename_matching("_", " ").unwrap(), 1);
        assert_eq!(names(&t), ["x y", "a 2", "b"]);
        assert_eq!(
            t.rename_matching("a 2", "b").unwrap_err().to_string(),
            "that would give 2 tips the name 'b'"
        );
        assert_eq!(
            t.rename_matching("^", "").unwrap_err().to_string(),
            "that replacement leaves every name as it is"
        );
        assert_eq!(
            t.rename_matching("q", "").unwrap_err().to_string(),
            "no tip name contains a match for 'q'"
        );
        assert_eq!(
            t.rename_matching("(.*)", "$2").unwrap_err().to_string(),
            "--replace refers to group '2', which the pattern does not have"
        );
        assert_eq!(
            names(&t),
            ["x y", "a 2", "b"],
            "refused renames change nothing"
        );
    }

    #[test]
    fn a_rename_is_refused_only_for_clashes_it_makes() {
        let mut t = tree("((A,A),b_1);");
        assert_eq!(t.rename_matching("_", " ").unwrap(), 1);
        let mut t = tree("((A_1,A_1),B);");
        assert_eq!(
            t.rename_matching("_", " ").unwrap(),
            2,
            "names already shared stay shared"
        );
        assert_eq!(names(&t), ["A 1", "A 1", "B"]);
        let mut t = tree("((A_1,A_1),'A 1');");
        assert_eq!(
            t.rename_matching("_", " ").unwrap_err().to_string(),
            "that would give 3 tips the name 'A 1'"
        );
        let mut t = tree("(p1,p2,q1,q2);");
        assert_eq!(
            t.rename_matching("[0-9]", "").unwrap_err().to_string(),
            "that would give more than one tip each of the names 'p', 'q'"
        );
    }

    #[test]
    fn tree_doc_commands_say_what_they_did_and_are_undone() {
        let mut doc = TreeDoc::from_text("((A:1,B:2)90:3,C:4,D:1);");
        let message = |doc: &mut TreeDoc, line| doc.apply(&cmd(line)).unwrap().message;
        assert_eq!(
            message(&mut doc, "prune --tip A"),
            "pruned 1 tip; now 3 tips"
        );
        assert_eq!(
            doc.apply(&cmd("prune --mrca C --mrca D"))
                .unwrap_err()
                .to_string(),
            "cannot prune the whole tree"
        );
        assert_eq!(
            message(&mut doc, "prune --tip C"),
            "pruned 1 tip; now 2 tips"
        );
        doc.undo().unwrap();
        assert_eq!(
            message(&mut doc, "ladderize --reverse"),
            "ladderized, most tips first"
        );
        assert_eq!(message(&mut doc, "ladderize"), "ladderized");
        let renamed = doc.apply(&cmd("rename --tip B --to 'Bee one'")).unwrap();
        assert_eq!(renamed.message, "renamed 'B' to 'Bee one'");
        assert_eq!(renamed.note.as_deref(), Some("was: B"));
        assert_eq!(
            message(&mut doc, "rename --regex ' one' --replace ''"),
            "renamed 1 of 3 tips"
        );
        let err = |doc: &mut TreeDoc, line| doc.apply(&cmd(line)).unwrap_err().to_string();
        assert_eq!(
            err(&mut doc, "rename --node 1 --to x"),
            "--node 1 is not a tip"
        );
        assert_eq!(
            err(&mut doc, "rename --mrca C --mrca D --to x"),
            "--mrca names a clade; rename one tip with --tip or --node"
        );
        assert_eq!(
            err(&mut doc, "rename --to x"),
            "say which tip: --tip or --node"
        );
        assert_eq!(
            err(&mut doc, "rename --tip C --regex a --replace b"),
            "--regex renames every tip; leave out --tip, --mrca and --node"
        );
        assert_eq!(
            err(&mut doc, "prune"),
            "say which node: --tip, --mrca or --node"
        );
        while doc.undo().is_ok() {}
        assert_eq!(
            to_newick(doc.tree(), doc.tree().root()),
            "((A:1,B:2)90:3,C:4,D:1);\n"
        );
        assert!(!doc.is_modified());
        doc.redo().unwrap();
        assert!(doc.is_modified());
    }

    #[test]
    fn undo_keeps_twenty_levels() {
        let mut doc = TreeDoc::from_text("(A,B,C);");
        for _ in 0..25 {
            doc.apply(&cmd("ladderize")).unwrap();
        }
        let mut undone = 0;
        while doc.undo().is_ok() {
            undone += 1;
        }
        assert_eq!(undone, 20);
    }

    #[test]
    fn edits_are_undone_and_copy_little() {
        // 100,000 tips (about 11 MB as a tree): renaming one tip copies one
        // chunk plus the walks' node lists, never the tree.
        let text = format!(
            "({});",
            (0..100_000)
                .map(|i| format!("t{i}:1"))
                .collect::<Vec<_>>()
                .join(",")
        );
        let mut doc = TreeDoc::from_text(&text);
        let rename = cmd("rename --tip t5 --to x");
        let (result, peak) = measure::peak_during(|| doc.apply(&rename));
        result.unwrap();
        assert!(peak < 4 << 20, "{peak} bytes");
        doc.undo().unwrap();
        assert_eq!(doc.tree().label(doc.tree().tips()[5]), Some("t5"));
    }

    #[test]
    fn edits_on_a_deep_caterpillar_do_not_recurse() {
        let mut t = tree(&caterpillar(200_000, ""));
        t.ladderize(false);
        let t0 = tip(&t, "t0");
        t.prune(t0).unwrap();
        assert_eq!(t.tip_count(), 200_000);
    }
}
