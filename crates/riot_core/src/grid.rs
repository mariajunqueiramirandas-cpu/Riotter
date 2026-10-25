//! Grid geometry (DESIGN §7.1). Grid dimensions are powers of two, so cell
//! lookups are shifts and masks and Morton codes cover a grid exactly.

/// Spread the low 16 bits of `v` to the even bit positions.
#[inline]
pub const fn part1by1(v: u32) -> u32 {
    let mut x = v & 0x0000_ffff;
    x = (x | (x << 8)) & 0x00ff_00ff;
    x = (x | (x << 4)) & 0x0f0f_0f0f;
    x = (x | (x << 2)) & 0x3333_3333;
    x = (x | (x << 1)) & 0x5555_5555;
    x
}

/// Gather the even bits of `v` into the low 16 bits.
#[inline]
pub const fn compact1by1(v: u32) -> u32 {
    let mut x = v & 0x5555_5555;
    x = (x | (x >> 1)) & 0x3333_3333;
    x = (x | (x >> 2)) & 0x0f0f_0f0f;
    x = (x | (x >> 4)) & 0x00ff_00ff;
    x = (x | (x >> 8)) & 0x0000_ffff;
    x
}

/// Morton (Z-order) code of a cell, x in the even bits.
#[inline]
pub const fn morton2(x: u32, y: u32) -> u32 {
    part1by1(x) | (part1by1(y) << 1)
}

#[inline]
pub const fn demorton2(m: u32) -> (u32, u32) {
    (compact1by1(m), compact1by1(m >> 1))
}

/// A square power-of-two grid laid over the map, whose origin is the map
/// centre (DESIGN §2.1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridGeom {
    /// Cells per side, a power of two.
    pub dim: u32,
    pub shift: u32,
    pub cell: f32,
    pub inv_cell: f32,
    /// Half the grid's size in metres, added to positions before lookup.
    pub half: f32,
}

impl GridGeom {
    pub fn new(world_size: f32, cell: f32) -> Self {
        let dim = (world_size / cell + 0.5) as u32;
        assert!(
            dim.is_power_of_two(),
            "grid dim {dim} must be a power of two"
        );
        GridGeom {
            dim,
            shift: dim.trailing_zeros(),
            cell,
            inv_cell: 1.0 / cell,
            half: world_size * 0.5,
        }
    }

    /// Continuous cell coordinates (not clamped).
    #[inline]
    pub fn to_cell_f(&self, x: f32, y: f32) -> (f32, f32) {
        (
            (x + self.half) * self.inv_cell,
            (y + self.half) * self.inv_cell,
        )
    }

    /// Cell containing a point, clamped to the grid.
    #[inline]
    pub fn cell_of(&self, x: f32, y: f32) -> (u32, u32) {
        let (fx, fy) = self.to_cell_f(x, y);
        let max = (self.dim - 1) as f32;
        (fx.clamp(0.0, max) as u32, fy.clamp(0.0, max) as u32)
    }

    #[inline]
    pub fn index(&self, cx: u32, cy: u32) -> usize {
        ((cy << self.shift) | cx) as usize
    }

    /// Centre of a cell in map coordinates.
    #[inline]
    pub fn centre(&self, cx: u32, cy: u32) -> (f32, f32) {
        (
            (cx as f32 + 0.5) * self.cell - self.half,
            (cy as f32 + 0.5) * self.cell - self.half,
        )
    }

    #[inline]
    pub fn cells(&self) -> usize {
        (self.dim as usize) * (self.dim as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn morton_roundtrip() {
        for x in [0u32, 1, 2, 63, 64, 4095, 1234] {
            for y in [0u32, 1, 5, 4095, 777] {
                assert_eq!(demorton2(morton2(x, y)), (x, y));
            }
        }
        assert_eq!(morton2(1, 0), 1);
        assert_eq!(morton2(0, 1), 2);
        assert_eq!(morton2(4095, 4095), (1 << 24) - 1);
    }

    #[test]
    fn grid_lookup() {
        let g = GridGeom::new(512.0, 2.0);
        assert_eq!(g.dim, 256);
        assert_eq!(g.cell_of(-256.0, -256.0), (0, 0));
        assert_eq!(g.cell_of(255.9, 255.9), (255, 255));
        assert_eq!(g.cell_of(1000.0, -1000.0), (255, 0));
        assert_eq!(g.centre(128, 128), (1.0, 1.0));
    }
}
