//! Naming nodes in commands, and the edits.

use std::collections::HashSet;

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
