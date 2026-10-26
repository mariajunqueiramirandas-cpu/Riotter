//! Hazard state (DESIGN §2.6, §2.8): gas canisters, projectiles in flight,
//! whether gas, smoke or fire is left anywhere, and people riding the metro.
//! How hazards spread, burn and hit is in `riot_hazards`.

use riot_core::{StateHasher, Vec2};

use riot_defs::mapdef::PropKind;
use riot_defs::setup::{DT, RiotSetup};
use riot_fields::{Fields, Layer};
use riot_map::Map;
use riot_snapshot::prop_flag;

use crate::world::Prop;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Emitter {
    pub pos: Vec2,
    pub ticks_left: u32,
    pub rate: f32,
    /// Stable id of the officer who fired it, or `u32::MAX`.
    pub owner: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ProjectileKind {
    Stone = 0,
    Molotov = 1,
    Canister = 2,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Projectile {
    pub kind: ProjectileKind,
    pub pos: Vec2,
    pub vel: Vec2,
    /// Lands here if it hits nothing first.
    pub target: Vec2,
    pub owner: u32,
    pub ttl: u16,
}

pub struct Hazards {
    pub emitters: Vec<Emitter>,
    pub projectiles: Vec<Projectile>,
    pub wind: Vec2,
    /// Gas or smoke left anywhere: when neither is, their passes are skipped.
    pub gas_live: bool,
    pub smoke_live: bool,
    /// Something is burning somewhere.
    pub fire_live: bool,
    /// Projectiles stopped by a shield or a body.
    pub blocked: u32,
    pub hits: u32,
}

impl Hazards {
    pub fn new(setup: &RiotSetup) -> Self {
        Hazards {
            emitters: Vec::with_capacity(256),
            projectiles: Vec::with_capacity(setup.limits.projectiles as usize),
            wind: Vec2::new(setup.weather.wind[0], setup.weather.wind[1]),
            gas_live: false,
            smoke_live: false,
            fire_live: false,
            blocked: 0,
            hits: 0,
        }
    }

    /// A canister starts emitting at `p`.
    pub fn add_gas(&mut self, p: Vec2, _tick: u32, owner: u32) {
        if self.emitters.len() < self.emitters.capacity() {
            self.emitters.push(Emitter {
                pos: p,
                ticks_left: 20 * 40,
                rate: 1.0,
                owner,
            });
        }
    }

    pub fn throw_canister(&mut self, from: Vec2, to: Vec2, owner: u32, speed: f32) {
        self.launch(ProjectileKind::Canister, from, to, owner, speed);
    }

    pub fn launch(&mut self, kind: ProjectileKind, from: Vec2, to: Vec2, owner: u32, speed: f32) {
        if self.projectiles.len() == self.projectiles.capacity() {
            return;
        }
        let d = to - from;
        let dist = d.len().max(0.1);
        let ttl = (dist / (speed * DT)).ceil() as u16 + 1;
        self.projectiles.push(Projectile {
            kind,
            pos: from,
            vel: d * (speed / dist),
            target: to,
            owner,
            ttl,
        });
    }

    /// Start a fire at `p`: fuel from props under it, or a little from the
    /// ground.
    pub fn ignite(&mut self, p: Vec2, fields: &mut Fields, map: &Map, props: &mut [Prop]) {
        let mut fuel = 40.0;
        for pr in props.iter_mut() {
            let reach = pr.half.x.max(pr.half.y) + 2.0;
            if pr.flags & prop_flag::DESTROYED != 0 || (pr.pos - p).len2() > reach * reach {
                continue;
            }
            pr.flags |= prop_flag::BURNING;
            fuel += match pr.kind {
                PropKind::Car => 600.0,
                PropKind::Barricade => 400.0,
                PropKind::Bin => 150.0,
                _ => 80.0,
            };
            // Fuel covers the prop's footprint.
            let r = pr.half.x.max(pr.half.y);
            fields.stamp(Layer::Fuel, pr.pos, r + 1.0, fuel * 0.5, map);
        }
        fields.stamp(Layer::Fuel, p, 2.0, fuel, map);
        fields.raise_at(Layer::Fire, p, 1.0, map);
        self.fire_live = true;
    }

    pub fn hash_into(&self, h: &mut StateHasher) {
        for e in &self.emitters {
            h.write_f32(e.pos.x);
            h.write_f32(e.pos.y);
            h.write_u32(e.ticks_left);
        }
        for p in &self.projectiles {
            h.write_f32(p.pos.x);
            h.write_f32(p.pos.y);
            h.write_u32(p.kind as u32 | (p.ttl as u32) << 8);
        }
    }
}

// ---------------------------------------------------------------------------
// The metro moves people between stations after a delay (DESIGN §2.8).

#[derive(Clone, Copy, Debug)]
pub struct Rider {
    pub arrive: u32,
    pub stable: u32,
    pub to: u16,
    pub archetype: u8,
    pub emo: [u8; 4],
    pub health: u8,
    pub flags: u8,
    pub intent: u16,
    pub belief: u16,
}

pub struct Metro {
    pub riders: Vec<Rider>,
    pub rides: u32,
}

impl Metro {
    pub fn new(cap: usize) -> Self {
        Metro {
            riders: Vec::with_capacity(cap.min(16_384)),
            rides: 0,
        }
    }
}
