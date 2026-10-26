//! Street lamps: the city's static lights, along its roads. The roof mesh
//! ([`crate::map`]) gives each a lamp head; `riot_render::lights` lights the
//! pavement under them at dusk and night.

use riot_defs::mapdef::{Ground, MapDef};

/// A light pool: centre, radius (m), colour and strength.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Light {
    pub pos: [f32; 2],
    pub radius: f32,
    pub color: [f32; 3],
}

/// Street lamps along the roads, every 24 m on both sides.
pub fn street_lamps(map: &MapDef) -> Vec<Light> {
    let warm = [0.72, 0.56, 0.34];
    let mut out = Vec::new();
    let r = &map.roads;
    let painted = map.ground.cell > 0.0 && !map.ground.kinds.is_empty();
    for e in &r.edges {
        let (i, j) = (e[0] as usize, e[1] as usize);
        if i >= r.nodes.len() || j >= r.nodes.len() || i == j {
            continue;
        }
        let (a, b) = (r.nodes[i], r.nodes[j]);
        let d = [b[0] - a[0], b[1] - a[1]];
        let l = (d[0] * d[0] + d[1] * d[1]).sqrt();
        if l < 20.0 {
            continue;
        }
        let u = [d[0] / l, d[1] / l];
        let n = [-u[1], u[0]];
        // Half the road's width, from the painted ground where there is one.
        let half = if painted {
            let mid = [a[0] + d[0] * 0.5, a[1] + d[1] * 0.5];
            let mut h = 0.0;
            while h < 20.0
                && map.ground_at(mid[0] + n[0] * (h + 0.5), mid[1] + n[1] * (h + 0.5))
                    == Ground::Road
            {
                h += 0.5;
            }
            h + 0.5
        } else {
            3.5
        };
        let mut t = 12.0;
        let mut side = 1.0;
        while t < l - 8.0 {
            let o = (half + 0.7) * side;
            out.push(Light {
                pos: [a[0] + u[0] * t + n[0] * o, a[1] + u[1] * t + n[1] * o],
                radius: 7.5,
                color: warm,
            });
            side = -side;
            t += 16.0;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lamps_line_the_district_roads() {
        let map = riot_testkit::district(512.0, 64.0);
        let lamps = street_lamps(&map);
        assert!(lamps.len() > 20);
        // On the pavement, not in the road.
        let on_road = lamps
            .iter()
            .filter(|l| map.ground_at(l.pos[0], l.pos[1]) == Ground::Road)
            .count();
        assert!(on_road * 5 < lamps.len(), "{on_road} of {}", lamps.len());
    }
}
