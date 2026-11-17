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

impl<T: Clone> Cols<T> {
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
        Arc::make_mut(&mut self.chunks[i / CHUNK])[i % CHUNK] = value;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

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
