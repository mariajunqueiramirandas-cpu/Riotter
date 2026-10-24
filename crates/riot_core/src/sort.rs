//! Allocation-free stable sorts (DESIGN §7.2). Buffers are sized once; a
//! rebuild with no more items than the capacity never allocates.

/// Stable counting sort of item indices by bucket, O(items + buckets).
pub struct Buckets {
    /// `buckets + 1` entries: bucket b's items are `order[start[b]..start[b + 1]]`.
    pub start: Vec<u32>,
    /// `buckets` entries of scratch.
    pub cursor: Vec<u32>,
    /// Item indices, sorted by bucket, stable.
    pub order: Vec<u32>,
}

impl Buckets {
    pub fn new(buckets: usize, capacity: usize) -> Self {
        Buckets {
            start: vec![0; buckets + 1],
            cursor: vec![0; buckets],
            order: Vec::with_capacity(capacity),
        }
    }

    #[inline]
    pub fn buckets(&self) -> usize {
        self.cursor.len()
    }

    pub fn rebuild(&mut self, bucket_of: &[u32]) {
        self.rebuild_in(bucket_of, self.buckets());
    }

    /// Like [`Buckets::rebuild`] using only the first `n` buckets (every
    /// item must be below `n`), so a table sized for the worst case costs
    /// only what is in it.
    pub fn rebuild_in(&mut self, bucket_of: &[u32], n: usize) {
        let n = n.min(self.buckets());
        self.order.clear();
        self.order.resize(bucket_of.len(), 0);
        let start = &mut self.start[..n + 1];
        start.fill(0);
        for &b in bucket_of {
            start[b as usize + 1] += 1;
        }
        for b in 1..start.len() {
            start[b] += start[b - 1];
        }
        self.cursor[..n].copy_from_slice(&start[..n]);
        for (i, &b) in bucket_of.iter().enumerate() {
            let slot = &mut self.cursor[b as usize];
            self.order[*slot as usize] = i as u32;
            *slot += 1;
        }
    }

    /// Items in bucket `b`.
    #[inline]
    pub fn bucket(&self, b: u32) -> &[u32] {
        let s = self.start[b as usize] as usize;
        let e = self.start[b as usize + 1] as usize;
        &self.order[s..e]
    }
}

/// Bits per radix pass.
pub const RADIX_BITS: u32 = 12;
pub const RADIX: usize = 1 << RADIX_BITS;

/// Two-pass LSD radix sort of item indices by a 24-bit key: O(items), stable,
/// allocation-free, whatever the key range (DESIGN §7.2 field index).
pub struct RadixSort24 {
    counts: Vec<u32>,
    tmp: Vec<u32>,
    /// Item indices sorted by key, stable.
    pub order: Vec<u32>,
}

impl RadixSort24 {
    pub fn new(capacity: usize) -> Self {
        RadixSort24 {
            counts: vec![0; RADIX + 1],
            tmp: Vec::with_capacity(capacity),
            order: Vec::with_capacity(capacity),
        }
    }

    pub fn sort(&mut self, keys: &[u32]) {
        let n = keys.len();
        self.tmp.clear();
        self.tmp.resize(n, 0);
        self.order.clear();
        self.order.resize(n, 0);
        // Pass 1: low digit, identity order → tmp.
        self.counts.fill(0);
        for &k in keys {
            debug_assert!(k < 1 << 24);
            self.counts[(k as usize & (RADIX - 1)) + 1] += 1;
        }
        for d in 1..=RADIX {
            self.counts[d] += self.counts[d - 1];
        }
        for (i, &k) in keys.iter().enumerate() {
            let slot = &mut self.counts[k as usize & (RADIX - 1)];
            self.tmp[*slot as usize] = i as u32;
            *slot += 1;
        }
        // Pass 2: high digit, tmp order → order.
        self.counts.fill(0);
        for &k in keys {
            self.counts[((k >> RADIX_BITS) as usize & (RADIX - 1)) + 1] += 1;
        }
        for d in 1..=RADIX {
            self.counts[d] += self.counts[d - 1];
        }
        for &i in &self.tmp {
            let k = keys[i as usize];
            let slot = &mut self.counts[(k >> RADIX_BITS) as usize & (RADIX - 1)];
            self.order[*slot as usize] = i;
            *slot += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::{Stream, roll};

    #[test]
    fn buckets_are_stable() {
        let of = [3u32, 1, 3, 0, 1, 3];
        let mut b = Buckets::new(4, of.len());
        b.rebuild(&of);
        assert_eq!(b.order, [3, 1, 4, 0, 2, 5]);
        assert_eq!(b.bucket(3), &[0, 2, 5]);
        assert_eq!(b.bucket(2), &[] as &[u32]);
        // The same items in a table of 64 using only its first 4 buckets.
        let mut big = Buckets::new(64, of.len());
        big.rebuild_in(&of, 4);
        assert_eq!(big.order, b.order);
        assert_eq!(big.bucket(3), &[0, 2, 5]);
    }

    #[test]
    fn radix_matches_std_stable_sort() {
        let keys: Vec<u32> = (0..5000)
            .map(|i| (roll(3, 0, i, Stream::Spawn) as u32) & 0x00ff_ffff & !0xf0)
            .collect();
        let mut r = RadixSort24::new(keys.len());
        r.sort(&keys);
        let mut want: Vec<u32> = (0..keys.len() as u32).collect();
        want.sort_by_key(|&i| keys[i as usize]);
        assert_eq!(r.order, want);
        // Reuse with fewer items allocates nothing new.
        let cap = r.order.capacity();
        r.sort(&keys[..100]);
        assert_eq!(r.order.capacity(), cap);
    }
}
