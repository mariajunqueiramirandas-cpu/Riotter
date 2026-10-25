//! Sparse sets for rare states (DESIGN §5): an agent is in the table because
//! it has the state, so loops visit only those agents. Keyed by `stable_id`,
//! with fixed capacity so inserts never allocate.

pub const ABSENT: u32 = u32::MAX;

#[derive(Clone, Debug)]
pub struct SparseSet<T: Copy> {
    dense: Vec<(u32, T)>,
    sparse: Vec<u32>,
}

impl<T: Copy> SparseSet<T> {
    /// A set for keys in `0..key_space`, holding at most `capacity` entries.
    pub fn new(key_space: usize, capacity: usize) -> Self {
        SparseSet {
            dense: Vec::with_capacity(capacity),
            sparse: vec![ABSENT; key_space],
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.dense.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.dense.is_empty()
    }

    #[inline]
    pub fn is_full(&self) -> bool {
        self.dense.len() == self.dense.capacity()
    }

    #[inline]
    pub fn contains(&self, key: u32) -> bool {
        self.sparse.get(key as usize).is_some_and(|&s| s != ABSENT)
    }

    #[inline]
    pub fn get(&self, key: u32) -> Option<&T> {
        let s = *self.sparse.get(key as usize)?;
        if s == ABSENT {
            None
        } else {
            Some(&self.dense[s as usize].1)
        }
    }

    #[inline]
    pub fn get_mut(&mut self, key: u32) -> Option<&mut T> {
        let s = *self.sparse.get(key as usize)?;
        if s == ABSENT {
            None
        } else {
            Some(&mut self.dense[s as usize].1)
        }
    }

    /// Insert or overwrite. Returns false (and changes nothing) when full.
    pub fn insert(&mut self, key: u32, value: T) -> bool {
        let s = self.sparse[key as usize];
        if s != ABSENT {
            self.dense[s as usize].1 = value;
            return true;
        }
        if self.is_full() {
            return false;
        }
        self.sparse[key as usize] = self.dense.len() as u32;
        self.dense.push((key, value));
        true
    }

    /// Remove by swapping the last entry into the hole. Deterministic, since
    /// it depends only on the sequence of operations.
    pub fn remove(&mut self, key: u32) -> Option<T> {
        let s = *self.sparse.get(key as usize)?;
        if s == ABSENT {
            return None;
        }
        self.sparse[key as usize] = ABSENT;
        let (_, v) = self.dense.swap_remove(s as usize);
        if let Some(&(moved, _)) = self.dense.get(s as usize) {
            self.sparse[moved as usize] = s;
        }
        Some(v)
    }

    pub fn clear(&mut self) {
        for &(k, _) in &self.dense {
            self.sparse[k as usize] = ABSENT;
        }
        self.dense.clear();
    }

    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = &(u32, T)> {
        self.dense.iter()
    }

    #[inline]
    pub fn entries(&self) -> &[(u32, T)] {
        &self.dense
    }

    #[inline]
    pub fn entries_mut(&mut self) -> &mut [(u32, T)] {
        &mut self.dense
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_remove() {
        let mut s = SparseSet::<u8>::new(10, 3);
        assert!(s.insert(4, 1));
        assert!(s.insert(7, 2));
        assert!(s.insert(1, 3));
        assert!(!s.insert(2, 9));
        assert_eq!(s.remove(4), Some(1));
        assert_eq!(s.get(1), Some(&3));
        assert_eq!(s.get(7), Some(&2));
        assert!(!s.contains(4));
        assert!(s.insert(2, 9));
        assert_eq!(s.len(), 3);
        s.clear();
        assert!(s.is_empty() && !s.contains(7));
    }
}
