//! Undo steps, and the tag that ties the two halves of an edit made to a
//! linked tree and alignment together.

/// Marks an undo step as one half of an edit to two files: the same `id`
/// is on the other file's step, and `with` is that file's dataset id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Pair {
    pub id: u64,
    pub with: u64,
}

/// One undo (or redo) step: the state to go back to, the command that
/// replaced it, and its pair tag when it was half of a two-file edit.
#[derive(Debug, Clone)]
pub(crate) struct Step<S> {
    pub state: S,
    pub name: &'static str,
    pub pair: Option<Pair>,
}

impl<S> Step<S> {
    pub fn new(state: S, name: &'static str) -> Step<S> {
        Step {
            state,
            name,
            pair: None,
        }
    }
}

/// The pair-tag questions every document answers the same way from its
/// undo and redo lists.
pub(crate) fn last_pair<S>(steps: &[Step<S>]) -> Option<Pair> {
    steps.last().and_then(|step| step.pair)
}

pub(crate) fn holds<S>(steps: &[Step<S>], id: u64) -> bool {
    steps
        .iter()
        .any(|step| step.pair.is_some_and(|p| p.id == id))
}
