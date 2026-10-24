//! State hash (DESIGN §19): a 64-bit `mix` chain over byte views of state.

use crate::rng::mix;

#[derive(Clone, Copy, Debug)]
pub struct StateHasher {
    h: u64,
    n: u64,
}

impl Default for StateHasher {
    fn default() -> Self {
        Self::new()
    }
}

impl StateHasher {
    pub const fn new() -> Self {
        StateHasher {
            h: 0x243f_6a88_85a3_08d3,
            n: 0,
        }
    }

    #[inline]
    pub fn write_u64(&mut self, x: u64) {
        self.n = self.n.wrapping_add(1);
        self.h = mix(self.h ^ x.wrapping_add(self.n.wrapping_mul(0x9e37_79b9_7f4a_7c15)));
    }

    #[inline]
    pub fn write_u32(&mut self, x: u32) {
        self.write_u64(x as u64);
    }

    #[inline]
    pub fn write_f32(&mut self, x: f32) {
        self.write_u64(x.to_bits() as u64);
    }

    pub fn write_bytes(&mut self, bytes: &[u8]) {
        let mut words = bytes.chunks_exact(8);
        for w in &mut words {
            self.write_u64(u64::from_le_bytes([
                w[0], w[1], w[2], w[3], w[4], w[5], w[6], w[7],
            ]));
        }
        let rest = words.remainder();
        if !rest.is_empty() {
            let mut buf = [0u8; 8];
            buf[..rest.len()].copy_from_slice(rest);
            self.write_u64(u64::from_le_bytes(buf) ^ ((rest.len() as u64) << 56));
        }
        self.write_u64(bytes.len() as u64);
    }

    /// Hash any `Pod` slice through its bytes.
    #[inline]
    pub fn write_pod<T: bytemuck::Pod>(&mut self, xs: &[T]) {
        self.write_bytes(bytemuck::cast_slice(xs));
    }

    #[inline]
    pub fn finish(&self) -> u64 {
        mix(self.h ^ self.n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_and_content_matter() {
        let mut a = StateHasher::new();
        a.write_bytes(&[1, 2, 3]);
        let mut b = StateHasher::new();
        b.write_bytes(&[1, 2, 3]);
        assert_eq!(a.finish(), b.finish());
        let mut c = StateHasher::new();
        c.write_bytes(&[1, 2, 3, 0]);
        assert_ne!(a.finish(), c.finish());
        let mut d = StateHasher::new();
        d.write_pod(&[1.0f32, 2.0]);
        let mut e = StateHasher::new();
        e.write_pod(&[2.0f32, 1.0]);
        assert_ne!(d.finish(), e.finish());
    }
}
