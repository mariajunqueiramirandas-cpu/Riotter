//! Lane storage (DESIGN §6.3): agent arrays padded to a multiple of eight so
//! kernels never need a remainder loop.

/// f32 lanes per vector: one AVX2 register.
pub const LANES: usize = 8;
/// Vectors per parallel task. Fixed, never derived from the thread count,
/// because chunk boundaries define the order of reductions (DESIGN §12).
pub const CHUNK: usize = 64;
/// Agents per parallel task.
pub const CHUNK_AGENTS: usize = LANES * CHUNK;

/// Eight f32s, 32-byte aligned: one AVX2 register.
#[derive(Clone, Copy, Default, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C, align(32))]
pub struct F32x8(pub [f32; LANES]);

/// Number of vectors needed to hold `n` agents.
#[inline]
pub const fn lanes_for(n: usize) -> usize {
    n.div_ceil(LANES)
}

/// `n` rounded up to a whole vector.
#[inline]
pub const fn padded(n: usize) -> usize {
    lanes_for(n) * LANES
}

/// `n` rounded up to a whole parallel chunk.
#[inline]
pub const fn chunk_padded(n: usize) -> usize {
    n.div_ceil(CHUNK_AGENTS) * CHUNK_AGENTS
}

/// Agent `i` lives at `[i / 8].0[i % 8]`.
#[inline]
pub fn get(v: &[F32x8], i: usize) -> f32 {
    v[i / LANES].0[i % LANES]
}

#[inline]
pub fn set(v: &mut [F32x8], i: usize, x: f32) {
    v[i / LANES].0[i % LANES] = x;
}

/// Flat view: agent `i` is element `i`.
#[inline]
pub fn flat(v: &[F32x8]) -> &[f32] {
    bytemuck::cast_slice(v)
}

#[inline]
pub fn flat_mut(v: &mut [F32x8]) -> &mut [f32] {
    bytemuck::cast_slice_mut(v)
}

/// A zeroed lane vector able to hold `n` agents.
pub fn zeroed(n: usize) -> Vec<F32x8> {
    vec![F32x8::default(); lanes_for(n)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexing_matches_flat_view() {
        let mut v = zeroed(20);
        assert_eq!(v.len(), 3);
        for i in 0..20 {
            set(&mut v, i, i as f32);
        }
        let f = flat(&v);
        for i in 0..20 {
            assert_eq!(get(&v, i), i as f32);
            assert_eq!(f[i], i as f32);
        }
        assert_eq!(std::mem::align_of::<F32x8>(), 32);
        assert_eq!(padded(9), 16);
        assert_eq!(chunk_padded(1), CHUNK_AGENTS);
    }
}
