//! Props and vehicles as obstacles (DESIGN §11.2): oriented boxes, and a
//! coarse hash of them that the body phase and the police vans query.

use riot_core::{Buckets, Vec2};

/// An oriented box people collide with: a prop or a vehicle.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Obb {
    pub c: Vec2,
    /// Unit vector of the box's local x axis.
    pub ax: Vec2,
    pub half: Vec2,
    /// 0 = prop, 1 = vehicle, 2 = police barrier (officers open their own).
    pub kind: u8,
    pub id: u16,
}

impl Obb {
    /// Push a circle out of the box: returns the correction, if touching.
    #[inline]
    pub fn push_out(&self, p: Vec2, r: f32) -> Option<Vec2> {
        let d = p - self.c;
        let ay = self.ax.perp();
        let (lx, ly) = (d.dot(self.ax), d.dot(ay));
        let (hx, hy) = (self.half.x, self.half.y);
        if lx.abs() > hx + r || ly.abs() > hy + r {
            return None;
        }
        let (cx, cy) = (lx.clamp(-hx, hx), ly.clamp(-hy, hy));
        let (ox, oy) = (lx - cx, ly - cy);
        let dist2 = ox * ox + oy * oy;
        if dist2 > 1e-10 {
            if dist2 >= r * r {
                return None;
            }
            let dist = dist2.sqrt();
            let k = (r - dist) / dist;
            return Some(self.ax * (ox * k) + ay * (oy * k));
        }
        // Centre inside the box: leave by the nearest face.
        let (px, py) = (hx - lx.abs() + r, hy - ly.abs() + r);
        Some(if px < py {
            self.ax * (px * lx.signum())
        } else {
            ay * (py * ly.signum())
        })
    }

    pub fn contains(&self, p: Vec2) -> bool {
        let d = p - self.c;
        d.dot(self.ax).abs() <= self.half.x && d.dot(self.ax.perp()).abs() <= self.half.y
    }
}

/// Coarse hash of obstacles over 4 m cells; an obstacle is listed in every
/// cell its expanded bounds overlap, so a point query reads one bucket.
pub struct ObstacleGrid {
    half: f32,
    mask: u32,
    ent_bucket: Vec<u32>,
    ent_obj: Vec<u32>,
    buckets: Buckets,
}

const OBST_CELL: f32 = 4.0;

#[inline]
fn obst_bucket(x: i32, y: i32, mask: u32) -> u32 {
    ((x as u32).wrapping_mul(0x9E37_79B1) ^ (y as u32).wrapping_mul(0x85EB_CA77)) & mask
}

impl ObstacleGrid {
    pub fn new(world_size: f32, max_obstacles: usize) -> Self {
        let cap = max_obstacles * 8 + 64;
        let buckets = (max_obstacles * 4).next_power_of_two().max(256);
        ObstacleGrid {
            half: world_size * 0.5,
            mask: buckets as u32 - 1,
            ent_bucket: Vec::with_capacity(cap),
            ent_obj: Vec::with_capacity(cap),
            buckets: Buckets::new(buckets, cap),
        }
    }

    pub fn rebuild(&mut self, obbs: &[Obb]) {
        self.ent_bucket.clear();
        self.ent_obj.clear();
        // Hash into about four buckets per obstacle present, not per
        // obstacle the scenario allows.
        let n = (obbs.len() * 4)
            .next_power_of_two()
            .clamp(256, self.buckets.buckets());
        self.mask = n as u32 - 1;
        for (k, o) in obbs.iter().enumerate() {
            let ext = o.half.x.max(o.half.y) + 0.6;
            let x0 = ((o.c.x - ext + self.half) / OBST_CELL).floor() as i32;
            let x1 = ((o.c.x + ext + self.half) / OBST_CELL).floor() as i32;
            let y0 = ((o.c.y - ext + self.half) / OBST_CELL).floor() as i32;
            let y1 = ((o.c.y + ext + self.half) / OBST_CELL).floor() as i32;
            for y in y0..=y1 {
                for x in x0..=x1 {
                    if self.ent_bucket.len() == self.ent_bucket.capacity() {
                        return;
                    }
                    self.ent_bucket.push(obst_bucket(x, y, self.mask));
                    self.ent_obj.push(k as u32);
                }
            }
        }
        self.buckets.rebuild_in(&self.ent_bucket, n);
    }

    /// Obstacles listed in the cell containing `p` (may include a few far
    /// ones from hash collisions).
    #[inline]
    pub fn near(&self, p: Vec2) -> impl Iterator<Item = u32> + '_ {
        let x = ((p.x + self.half) / OBST_CELL).floor() as i32;
        let y = ((p.y + self.half) / OBST_CELL).floor() as i32;
        self.buckets
            .bucket(obst_bucket(x, y, self.mask))
            .iter()
            .map(|&e| self.ent_obj[e as usize])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn obb_push_out() {
        let o = Obb {
            c: Vec2::ZERO,
            ax: Vec2::new(1.0, 0.0),
            half: Vec2::new(2.0, 1.0),
            kind: 0,
            id: 0,
        };
        let c = o.push_out(Vec2::new(0.0, 1.1), 0.25).unwrap();
        assert!((c.y - 0.15).abs() < 1e-5 && c.x.abs() < 1e-6);
        assert!(o.push_out(Vec2::new(0.0, 1.3), 0.25).is_none());
        let c = o.push_out(Vec2::new(1.9, 0.0), 0.25).unwrap();
        assert!((c.x - 0.35).abs() < 1e-5);
        // Rotated 90°.
        let o = Obb {
            ax: Vec2::new(0.0, 1.0),
            ..o
        };
        let c = o.push_out(Vec2::new(1.1, 0.0), 0.25).unwrap();
        assert!((c.x - 0.15).abs() < 1e-5, "{c:?}");
    }
}
