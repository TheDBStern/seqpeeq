//! A column of node fields, cut into shared chunks.

use std::sync::Arc;

/// Values per chunk.
pub(crate) const CHUNK: usize = 4096;

/// A growable column of values held in chunks of [`CHUNK`] shared by `Arc`.
/// Cloning copies only the chunk pointers; changing a value copies the one
/// chunk holding it if a clone still shares that chunk. An undo level is
/// then a clone, and costs only what later edits touch.
#[derive(Debug)]
pub(crate) struct Cols<T> {
    chunks: Vec<Arc<Vec<T>>>,
    len: usize,
}

impl<T> Clone for Cols<T> {
    fn clone(&self) -> Self {
        Cols {
            chunks: self.chunks.clone(),
            len: self.len,
        }
    }
}

/// Equality for deciding whether a write changes anything. Lengths compare
/// bit for bit, so a missing length (NaN) equals itself.
pub(crate) trait Same {
    fn same(&self, other: &Self) -> bool;
}

impl Same for u32 {
    fn same(&self, other: &Self) -> bool {
        self == other
    }
}

impl Same for f64 {
    fn same(&self, other: &Self) -> bool {
        self.to_bits() == other.to_bits()
    }
}

impl Same for Option<Arc<str>> {
    fn same(&self, other: &Self) -> bool {
        self == other
    }
}

impl<T: Clone + Same> Cols<T> {
    pub(crate) fn new() -> Self {
        Cols {
            chunks: Vec::new(),
            len: 0,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn push(&mut self, value: T) {
        if self.len.is_multiple_of(CHUNK) {
            self.chunks.push(Arc::new(Vec::with_capacity(CHUNK)));
        }
        let last = self.chunks.last_mut().expect("a chunk was just ensured");
        Arc::make_mut(last).push(value);
        self.len += 1;
    }

    pub(crate) fn get(&self, i: usize) -> &T {
        &self.chunks[i / CHUNK][i % CHUNK]
    }

    pub(crate) fn set(&mut self, i: usize, value: T) {
        assert!(i < self.len, "index {i} past the end ({})", self.len);
        // A chunk shared with an undo level is copied only for a real change.
        if self.get(i).same(&value) {
            return;
        }
        Arc::make_mut(&mut self.chunks[i / CHUNK])[i % CHUNK] = value;
    }

    /// Whether every chunk is shared with `other`.
    #[cfg(test)]
    pub(crate) fn shares_all(&self, other: &Cols<T>) -> bool {
        self.chunks.len() == other.chunks.len() && self.shared_chunks(other) == self.chunks.len()
    }

    /// How many chunks, position by position, are shared with `other`.
    #[cfg(test)]
    pub(crate) fn shared_chunks(&self, other: &Cols<T>) -> usize {
        let pairs = self.chunks.iter().zip(&other.chunks);
        pairs.filter(|(a, b)| Arc::ptr_eq(a, b)).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn a_value_set_to_itself_keeps_its_chunk_shared() {
        let mut ids = Cols::new();
        let mut lengths = Cols::new();
        for i in 0..10u32 {
            ids.push(i);
            lengths.push(f64::NAN);
        }
        let (ids_before, lengths_before) = (ids.clone(), lengths.clone());
        ids.set(3, 3);
        lengths.set(3, f64::NAN);
        assert!(ids.shares_all(&ids_before) && lengths.shares_all(&lengths_before));
        ids.set(3, 4);
        assert!(!ids.shares_all(&ids_before));
    }

    proptest! {
        #[test]
        fn cols_behave_as_a_vec_and_clones_are_independent(
            ops in proptest::collection::vec((0usize..10_000, any::<u32>()), 0..300)
        ) {
            let (mut cols, mut model) = (Cols::new(), Vec::new());
            for i in 0..9_000u32 {
                cols.push(i);
                model.push(i);
            }
            let before = cols.clone();
            for (i, v) in ops {
                let i = i % model.len();
                cols.set(i, v);
                model[i] = v;
            }
            prop_assert_eq!(cols.len(), model.len());
            for (i, v) in model.iter().enumerate() {
                prop_assert_eq!(cols.get(i), v);
            }
            for i in 0..9_000 {
                prop_assert_eq!(*before.get(i), i as u32);
            }
        }
    }
}
