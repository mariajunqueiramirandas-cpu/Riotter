//! The police commander's state (DESIGN §14.1): the crowds it tracks, the
//! protected sites, its cordons and ambushes, what it has learnt, and the
//! police radio. What the commander decides is in `riot_tactics`.

use riot_core::{StateHasher, Vec2};

use riot_defs::police::{Call, CordonState, NO_CORDON, NO_SITE, OpKind, RADIO, RadioCall};
use riot_flow::NO_GOAL;
use riot_map::Map;
use riot_snapshot::prop_flag;

use crate::police::Cluster;
use crate::world::Prop;

pub const MAX_TRACKS: usize = 24;
/// Turning points kept of a predicted route.
pub const ROUTE: usize = 32;
pub const MAX_CORDONS: usize = 12;
/// Barriers in one cordon: a 40 m street.
pub const MAX_SLOTS: usize = 16;
pub const NO_TRACK: u32 = u32::MAX;
pub const NO_PROP: u16 = u16::MAX;
pub const NO_CLUSTER: u16 = u16::MAX;
const VLOG: usize = 256;
/// How long violence counts toward a crowd's heat (ticks).
const HEAT_TICKS: u32 = 400;
/// One steel barrier: its length and half its depth (m).
pub const BARRIER_LEN: f32 = 2.5;
pub const BARRIER_DEPTH: f32 = 0.3;
/// Officers hold this far behind their barriers (m).
const BEHIND: f32 = 1.3;

/// A place the state protects: the government quarter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Site {
    pub pos: Vec2,
    pub radius: f32,
}

/// A crowd the police follow from one replan to the next.
#[derive(Clone, Copy, Debug)]
pub struct Track {
    pub id: u32,
    pub centroid: Vec2,
    /// Smoothed velocity of the centroid (m/s).
    pub vel: Vec2,
    pub count: f32,
    pub radius: f32,
    /// Bounding box of its dense cells.
    pub lo: Vec2,
    pub hi: Vec2,
    pub anger: f32,
    pub threat: f32,
    /// Recent violence in and around it (severity, decaying over 20 s).
    pub heat: f32,
    /// Where that violence happened, or the centroid.
    pub hot: Vec2,
    /// This replan's cluster, or `NO_CLUSTER`.
    pub cluster: u16,
    pub seen: u32,
    pub born: u32,
    /// Objective slot its people follow, or `NO_GOAL`.
    pub dest: u8,
    /// Protected site it threatens, or `NO_SITE`.
    pub site: u8,
    pub op: OpKind,
    pub cordon: u8,
    /// Predicted route: turning points from the centroid on.
    pub route: [Vec2; ROUTE],
    pub route_n: u8,
    /// How far its front is ahead of the centroid, along the route (m).
    pub front: f32,
    /// Seconds until its front reaches the protected site.
    pub eta: f32,
    pub gas_ready: u32,
    /// Tick it was told to disperse, or 0.
    pub warned: u32,
    /// Tick its kettle closed, or 0.
    pub kettled: u32,
    /// Direction the kettle's filter gate opens, or zero.
    pub gate: Vec2,
}

impl Track {
    pub fn new(id: u32, c: &Cluster, tick: u32) -> Track {
        Track {
            id,
            centroid: c.centroid,
            vel: Vec2::ZERO,
            count: 0.0,
            radius: c.radius,
            lo: c.lo,
            hi: c.hi,
            anger: c.anger,
            threat: 0.0,
            heat: 0.0,
            hot: c.centroid,
            cluster: NO_CLUSTER,
            seen: tick,
            born: tick,
            dest: NO_GOAL,
            site: NO_SITE,
            op: OpKind::None,
            cordon: NO_CORDON,
            route: [Vec2::ZERO; ROUTE],
            route_n: 0,
            front: 0.0,
            eta: f32::INFINITY,
            gas_ready: 0,
            warned: 0,
            kettled: 0,
            gate: Vec2::ZERO,
        }
    }

    /// Violent enough for force: violence in and around it lately, not
    /// trouble somewhere else.
    #[inline]
    pub fn violent(&self) -> bool {
        self.heat > 2.0
    }
}

/// A squad waiting around a corner for the crowd.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ambush {
    /// Where the squad waits.
    pub pos: Vec2,
    /// Where its side street meets the route.
    pub junction: Vec2,
    /// Tick it was sprung, or 0.
    pub sprung: u32,
    /// The crowd at the barricade when it was sprung (people per cell).
    pub at_spring: f32,
    pub scored: bool,
}

/// A line of police barriers across a street on a crowd's route.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cordon {
    pub state: CordonState,
    /// Centre of the barrier line.
    pub pos: Vec2,
    /// Along the line, wall to wall.
    pub axis: Vec2,
    /// Toward the crowd.
    pub face: Vec2,
    /// Half the street's width.
    pub half: f32,
    pub site: u8,
    pub created: u32,
    /// Last tick a crowd's route ran through it or a crowd stood at it.
    pub needed: u32,
    pub slots: u8,
    /// Prop index of each slot's barrier, or `NO_PROP`.
    pub props: [u16; MAX_SLOTS],
    pub progress: [u8; MAX_SLOTS],
    /// Barrier strength left, with fractions the prop's byte cannot hold.
    pub wear: [f32; MAX_SLOTS],
    pub breached: u32,
    /// Barriers put back up after a breach.
    pub rebuilt: u8,
    pub ambush: Option<Ambush>,
    /// Violence at the barricade (severity over the last 20 s).
    pub violence: f32,
    pub track: u32,
}

impl Cordon {
    /// Centre of barrier slot `s`.
    #[inline]
    pub fn slot_pos(&self, s: usize) -> Vec2 {
        let seg = 2.0 * self.half / self.slots.max(1) as f32;
        self.pos + self.axis * (-self.half + seg * (s as f32 + 0.5))
    }

    /// Where the officer setting up slot `s` stands.
    #[inline]
    pub fn slot_post(&self, s: usize) -> Vec2 {
        self.slot_pos(s) - self.face * (BARRIER_DEPTH + 0.65)
    }

    /// The line the cordon's squads hold, behind the barriers.
    #[inline]
    pub fn line(&self) -> Vec2 {
        self.pos - self.face * BEHIND
    }

    #[inline]
    pub fn live(&self) -> bool {
        self.state != CordonState::Gone
    }

    #[inline]
    pub fn standing(&self, props: &[Prop], s: usize) -> bool {
        let k = self.props[s];
        k != NO_PROP && props[k as usize].flags & prop_flag::DESTROYED == 0
    }
}

/// What the commander has learnt in this riot.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Memory {
    pub cordons: u32,
    pub breaches: u32,
    pub ambushes: u32,
    pub ambush_wins: u32,
    pub ambush_losses: u32,
    pub reliefs: u32,
    pub warnings: u32,
    pub volleys: u32,
    pub lines_broken: u32,
}

#[derive(Clone, Copy, Debug, Default)]
struct Violence {
    tick: u32,
    pos: Vec2,
    severity: f32,
}

pub struct Commander {
    pub sites: Vec<Site>,
    pub tracks: Vec<Track>,
    pub cordons: Vec<Cordon>,
    pub next_id: u32,
    vlog: [Violence; VLOG],
    vhead: usize,
    pub memory: Memory,
    pub radio: [RadioCall; RADIO],
    /// Radio calls ever made; the latest is `radio[(radio_n - 1) % RADIO]`.
    pub radio_n: u32,
    /// Field cells holding barriers in `Layer::Block`, by chunk.
    pub stamped: Vec<(u32, u16)>,
    /// A barrier went up or came down since the last stamp.
    pub dirty: bool,
    pub last_raise: u32,
    pub seen_injured: u32,
    pub seen_breaches: u32,
    /// Streets where a cordon could not be finished, and when: not tried
    /// again for a few minutes.
    pub abandoned: [(Vec2, u32); 8],
    /// Where and when crowds were told to disperse.
    pub warnings: [(Vec2, u32); 8],
    /// No volley before this tick, force-wide: gas is rationed.
    pub gas_next: u32,
    /// Where and when the last volleys landed: none again there for 20 s,
    /// whatever the crowd there is called now.
    pub volleys: [(Vec2, u32); 8],
}

impl Commander {
    pub fn new(map: &Map) -> Commander {
        let sites = map
            .areas
            .iter()
            .filter(|a| a.tag == "government")
            .map(|a| Site {
                pos: a.pos,
                radius: a.radius.max(10.0),
            })
            .collect();
        Commander {
            sites,
            tracks: Vec::with_capacity(MAX_TRACKS),
            cordons: Vec::with_capacity(MAX_CORDONS),
            next_id: 0,
            vlog: [Violence::default(); VLOG],
            vhead: 0,
            memory: Memory::default(),
            radio: [RadioCall::default(); RADIO],
            radio_n: 0,
            stamped: Vec::with_capacity(MAX_CORDONS * MAX_SLOTS * 12),
            dirty: false,
            last_raise: 0,
            seen_injured: 0,
            seen_breaches: 0,
            abandoned: [(Vec2::ZERO, 0); 8],
            warnings: [(Vec2::ZERO, 0); 8],
            gas_next: 0,
            volleys: [(Vec2::ZERO, 0); 8],
        }
    }

    /// A violent act at `pos` (a throw, a blow, a projectile on an officer).
    pub fn note_violence(&mut self, tick: u32, pos: Vec2, severity: f32) {
        self.vlog[self.vhead] = Violence {
            tick: tick.max(1),
            pos,
            severity,
        };
        self.vhead = (self.vhead + 1) % VLOG;
    }

    /// Report on the radio, unless the same call came from about here in
    /// the last half minute.
    pub fn call(&mut self, tick: u32, call: Call, pos: Vec2, n: u32) {
        let repeat = self.radio.iter().any(|r| {
            r.call == call
                && tick.wrapping_sub(r.tick) < 600
                && (r.pos - pos).len2() < 60.0 * 60.0
                && (call != Call::Escalate && call != Call::Deescalate || r.n == n)
        });
        if repeat && self.radio_n > 0 {
            return;
        }
        self.radio[self.radio_n as usize % RADIO] = RadioCall { tick, call, pos, n };
        self.radio_n += 1;
    }

    /// When a crowd within `r` of `p` was last told to disperse, within
    /// the last `ticks`.
    pub fn warned_near(&self, tick: u32, p: Vec2, r: f32, ticks: u32) -> Option<u32> {
        self.warnings
            .iter()
            .filter(|&&(q, t)| t != 0 && tick.wrapping_sub(t) < ticks && (q - p).len2() < r * r)
            .map(|&(_, t)| t)
            .max()
    }

    /// Recent violence within `r` of `p`: summed severity (fading over
    /// 20 s) and its weighted centre.
    pub fn violence_near(&self, tick: u32, p: Vec2, r: f32) -> (f32, Vec2) {
        let (mut sum, mut at) = (0.0f32, Vec2::ZERO);
        for v in &self.vlog {
            let age = tick.wrapping_sub(v.tick);
            if v.tick == 0 || age >= HEAT_TICKS || (v.pos - p).len2() > r * r {
                continue;
            }
            let wt = v.severity * (1.0 - age as f32 / HEAT_TICKS as f32);
            sum += wt;
            at += v.pos * wt;
        }
        if sum > 0.0 {
            (sum, at * (1.0 / sum))
        } else {
            (0.0, p)
        }
    }

    #[inline]
    pub fn track(&self, id: u32) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    #[inline]
    pub fn track_index(&self, id: u32) -> Option<usize> {
        self.tracks.iter().position(|t| t.id == id)
    }

    pub fn hash_into(&self, h: &mut StateHasher) {
        for t in &self.tracks {
            h.write_u32(t.id);
            h.write_f32(t.vel.x);
            h.write_f32(t.vel.y);
            h.write_u32(t.op as u32 | (t.dest as u32) << 8 | (t.cordon as u32) << 16);
            h.write_u32(t.kettled ^ t.warned.rotate_left(16));
        }
        for c in &self.cordons {
            h.write_f32(c.pos.x);
            h.write_f32(c.pos.y);
            h.write_u32(c.state as u32 | (c.slots as u32) << 8 | (c.rebuilt as u32) << 16);
            for &w in &c.wear[..c.slots as usize] {
                h.write_f32(w);
            }
            if let Some(a) = c.ambush {
                h.write_u32(a.sprung);
            }
        }
        h.write_u32(self.radio_n);
        h.write_u32(self.last_raise);
        h.write_u32(self.gas_next);
    }
}
