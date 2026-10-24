//! Counter-based RNG with no shared state (DESIGN §12 rule 3). Every random
//! number is a pure function of `(seed, tick, id, stream)`, so any thread can
//! compute any agent's number in any order.

/// SplitMix64 finalizer.
#[inline]
pub const fn mix(mut x: u64) -> u64 {
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// Independent random streams, so adding a roll in one system never shifts
/// the numbers another system sees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
pub enum Stream {
    Think = 1,
    Message = 2,
    Violence = 3,
    Spawn = 4,
    Appearance = 5,
    Police = 6,
    Hazard = 7,
    Body = 8,
    Metro = 9,
    Supply = 10,
    Footage = 11,
    Garble = 12,
    Emotion = 13,
}

#[inline]
pub fn roll(seed: u64, tick: u32, id: u32, s: Stream) -> u64 {
    mix(seed
        ^ mix(((tick as u64) << 32) | id as u64)
        ^ (s as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15))
}

/// Like [`roll`] with an extra key, for several numbers per agent per tick or
/// for things that are not agents (cells, props, squads).
#[inline]
pub fn roll_key(seed: u64, tick: u32, id: u32, key: u32, s: Stream) -> u64 {
    roll(seed ^ mix(0x5851_f42d_4c95_7f2d ^ key as u64), tick, id, s)
}

/// Uniform in [0, 1) from the top 24 bits.
#[inline]
pub fn unit(bits: u64) -> f32 {
    (bits >> 40) as f32 * (1.0 / 16_777_216.0)
}

/// Uniform in [-1, 1).
#[inline]
pub fn signed_unit(bits: u64) -> f32 {
    unit(bits) * 2.0 - 1.0
}

/// Uniform integer in `0..n` (n > 0), from the top 32 bits.
#[inline]
pub fn below(bits: u64, n: u32) -> u32 {
    (((bits >> 32) * n as u64) >> 32) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roll_is_pure_and_streams_differ() {
        let a = roll(7, 100, 42, Stream::Think);
        assert_eq!(a, roll(7, 100, 42, Stream::Think));
        assert_ne!(a, roll(7, 100, 42, Stream::Message));
        assert_ne!(a, roll(7, 101, 42, Stream::Think));
        assert_ne!(a, roll(8, 100, 42, Stream::Think));
        assert_ne!(
            roll_key(7, 1, 2, 3, Stream::Body),
            roll_key(7, 1, 2, 4, Stream::Body)
        );
    }

    #[test]
    fn unit_range_and_mean() {
        let mut sum = 0.0f64;
        for i in 0..100_000u32 {
            let u = unit(roll(1, 0, i, Stream::Spawn));
            assert!((0.0..1.0).contains(&u));
            sum += u as f64;
        }
        let mean = sum / 100_000.0;
        assert!((mean - 0.5).abs() < 0.01, "{mean}");
        for i in 0..1000u32 {
            assert!(below(roll(1, 0, i, Stream::Spawn), 7) < 7);
        }
    }
}
