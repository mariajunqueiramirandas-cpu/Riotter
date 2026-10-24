//! Particles (DESIGN §18.2 effects): flames and embers over burning cells
//! and props, water cannon spray, and smoke trailing gas canisters. Purely
//! cosmetic: they run on the render clock and never feed back.

use macroquad::prelude::*;
use riot_sim::snapshot::Snapshot;

use crate::fields::Burning;

#[derive(Clone, Copy, Debug)]
struct Particle {
    pos: Vec2,
    vel: Vec2,
    age: f32,
    life: f32,
    size: f32,
    kind: Kind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Flame,
    Ember,
    Water,
    Puff,
}

/// The particle system. Bounded: the oldest particles are dropped first.
pub struct Effects {
    parts: Vec<Particle>,
    rng: u64,
    /// Fractional particles carried to the next frame, per emitter type.
    carry: [f32; 3],
}

const MAX_PARTICLES: usize = 6000;

impl Default for Effects {
    fn default() -> Self {
        Self::new()
    }
}

impl Effects {
    /// An empty particle system.
    pub fn new() -> Effects {
        Effects {
            parts: Vec::with_capacity(MAX_PARTICLES),
            rng: 0x9E37_79B9_7F4A_7C15,
            carry: [0.0; 3],
        }
    }

    fn rand(&mut self) -> f32 {
        // xorshift64*: render-side randomness only.
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        ((self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40) as f32) / 16_777_216.0
    }

    fn spawn(&mut self, p: Particle) {
        if self.parts.len() >= MAX_PARTICLES {
            self.parts.swap_remove(0);
        }
        self.parts.push(p);
    }

    /// Advance by `dt` seconds and emit from the snapshot's fire, props,
    /// vehicles and projectiles inside `view`.
    pub fn update(&mut self, dt: f32, snap: &Snapshot, burning: &[Burning], view: Rect) {
        let dt = dt.clamp(0.0, 0.1);
        for p in &mut self.parts {
            p.age += dt;
            p.pos += p.vel * dt;
            match p.kind {
                Kind::Flame => p.vel *= 1.0 - 2.0 * dt,
                Kind::Ember => p.vel *= 1.0 - 0.8 * dt,
                Kind::Water => p.vel *= 1.0 - 1.5 * dt,
                Kind::Puff => p.size += dt * 0.8,
            }
        }
        self.parts.retain(|p| p.age < p.life);

        // Flames over burning field cells and props.
        let mut sources: Vec<(Vec2, f32)> = burning
            .iter()
            .filter(|b| view.contains(vec2(b.x, b.y)))
            .map(|b| (vec2(b.x, b.y), b.heat))
            .collect();
        for pr in &snap.props {
            if pr.flags & 1 != 0 && view.contains(vec2(pr.x, pr.y)) {
                sources.push((vec2(pr.x, pr.y), 1.0));
            }
        }
        if !sources.is_empty() {
            self.carry[0] += dt * 30.0 * (sources.len() as f32).sqrt().min(40.0) * 4.0;
            while self.carry[0] >= 1.0 {
                self.carry[0] -= 1.0;
                let k = ((self.rand() * sources.len() as f32) as usize).min(sources.len() - 1);
                let (c, heat) = sources[k];
                let jitter = vec2(self.rand() - 0.5, self.rand() - 0.5) * 2.2;
                let ember = self.rand() < 0.15;
                let up =
                    vec2(self.rand() - 0.5, -0.4 - self.rand()) * if ember { 2.5 } else { 1.0 };
                let life = if ember {
                    1.2 + self.rand()
                } else {
                    0.4 + self.rand() * 0.5
                };
                let size = if ember {
                    0.06
                } else {
                    0.15 + 0.3 * heat * self.rand()
                };
                self.spawn(Particle {
                    pos: c + jitter,
                    vel: up,
                    age: 0.0,
                    life,
                    size,
                    kind: if ember { Kind::Ember } else { Kind::Flame },
                });
            }
        }

        // Water cannon spray: a cone ahead of spraying trucks.
        for v in &snap.vehicles {
            if v.flags & 2 == 0 || !view.contains(vec2(v.x, v.y)) {
                continue;
            }
            let a = v.heading as f32 * std::f32::consts::TAU / 256.0;
            let dir = vec2(a.cos(), a.sin());
            let nozzle = vec2(v.x, v.y) + dir * v.half_len;
            self.carry[1] += dt * 220.0;
            while self.carry[1] >= 1.0 {
                self.carry[1] -= 1.0;
                let spread = (self.rand() - 0.5) * 0.35;
                let d = vec2(dir.x - dir.y * spread, dir.y + dir.x * spread);
                let speed = 14.0 + self.rand() * 8.0;
                let life = 0.9 + self.rand() * 0.4;
                let size = 0.12 + self.rand() * 0.15;
                self.spawn(Particle {
                    pos: nozzle,
                    vel: d * speed,
                    age: 0.0,
                    life,
                    size,
                    kind: Kind::Water,
                });
            }
        }

        // Smoke puffs trailing gas canisters in flight.
        for pr in &snap.projectiles {
            if pr.kind == 2 && view.contains(vec2(pr.x, pr.y)) {
                self.carry[2] += dt * 25.0;
                while self.carry[2] >= 1.0 {
                    self.carry[2] -= 1.0;
                    let j = vec2(self.rand() - 0.5, self.rand() - 0.5) * 0.4;
                    self.spawn(Particle {
                        pos: vec2(pr.x, pr.y) + j,
                        vel: j,
                        age: 0.0,
                        life: 1.2,
                        size: 0.3,
                        kind: Kind::Puff,
                    });
                }
            }
        }
    }

    /// Draw in map coordinates (a world camera must be set).
    pub fn draw(&self, view: Rect, zoom: f32) {
        let min = 1.2 / zoom.max(1e-3);
        for p in &self.parts {
            if !view.contains(p.pos) {
                continue;
            }
            let t = p.age / p.life;
            let (col, size) = match p.kind {
                Kind::Flame => {
                    let c = if t < 0.25 {
                        Color::new(1.0, 0.85, 0.35, 0.7)
                    } else if t < 0.6 {
                        Color::new(1.0, 0.42, 0.08, 0.6 * (1.0 - t))
                    } else {
                        Color::new(0.25, 0.22, 0.22, 0.45 * (1.0 - t))
                    };
                    (c, p.size * (1.0 + 0.6 * t))
                }
                Kind::Ember => (Color::new(1.0, 0.7, 0.2, 1.0 - t), p.size),
                Kind::Water => (Color::new(0.7, 0.85, 1.0, 0.7 * (1.0 - t)), p.size),
                Kind::Puff => (Color::new(0.85, 0.88, 0.8, 0.45 * (1.0 - t)), p.size),
            };
            draw_circle(p.pos.x, p.pos.y, size.max(min), col);
        }
    }

    /// Live particles.
    pub fn len(&self) -> usize {
        self.parts.len()
    }

    /// No live particles.
    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }
}
