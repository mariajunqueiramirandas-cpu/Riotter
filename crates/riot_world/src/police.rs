//! Police state (DESIGN §2.5, §14): squads, officers, vehicles and arrests,
//! and the bookkeeping that puts squads on the map and routes vans along the
//! roads. The director and the squads' doctrines run in `riot_police`, the
//! commander's plans in `riot_tactics`; its state is [`crate::commander`].

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use riot_core::math::narrow_u8;
use riot_core::rng::{Stream, roll};
use riot_core::{SparseSet, StateHasher, Vec2};

use riot_defs::archetype::Archetype;
use riot_defs::police::{Doctrine, NO_CORDON};
use riot_defs::setup::{Escalation, RiotSetup, SquadKind, minutes};
use riot_defs::state::{Action, Item, flag, health};
use riot_defs::targets::OfficerTarget;
use riot_fields::CL;
use riot_flow::{MAX_GOALS, NO_GOAL};
use riot_map::Map;
use riot_people::Spawn;
use riot_snapshot::appearance_for;
use riot_spatial::obstacles::Obb;

use crate::commander::{Commander, NO_TRACK};
use crate::world::{NO_SLOT, SetupError, World};

pub const MAX_SQUAD: usize = 16;
pub const NO_SQUAD: u16 = u16::MAX;
pub const MAX_CLUSTERS: usize = 32;
pub const PATH: usize = 128;

#[derive(Clone, Copy, Debug)]
pub struct Squad {
    pub kind: SquadKind,
    pub doctrine: Doctrine,
    pub officers: [u32; MAX_SQUAD],
    pub n: u8,
    pub depot: u16,
    pub home: Vec2,
    pub available: u32,
    pub deployed: bool,
    /// Formation centre and the direction it faces.
    pub anchor: Vec2,
    pub facing: Vec2,
    pub width: f32,
    /// Kettle radius.
    pub radius: f32,
    pub cluster: u16,
    pub target_person: u32,
    pub goal: u8,
    pub morale: f32,
    pub pos: Vec2,
    pub present: u8,
    pub van: u16,
    /// Index of this squad among the squads kettling the same cluster, and
    /// how many there are.
    pub ring: (u8, u8),
    /// Centroid of the crowd this squad was last sent to: squads stay with
    /// a crowd across replans rather than being dealt out afresh.
    pub cluster_at: Vec2,
    /// Tick this squad was last given a crowd.
    pub assigned: u32,
    /// Persistent id of the crowd this squad works ([`crate::commander::Track`]),
    /// or `NO_TRACK`.
    pub track: u32,
    /// Cordon this squad builds, holds or lies in ambush for, or `NO_CORDON`.
    pub cordon: u8,
    /// Tick the current doctrine began.
    pub since: u32,
    /// Mean fatigue of the officers present.
    pub fatigue: f32,
    /// Where a charge cuts in, or which way a kettle's gate opens (zero
    /// when shut).
    pub sweep: Vec2,
    /// Half its officers have fled their posts.
    pub broken: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct OfficerInfo {
    pub squad: u16,
    pub slot: u8,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Cluster {
    pub centroid: Vec2,
    pub count: f32,
    pub radius: f32,
    pub anger: f32,
    pub threat: f32,
    pub cells: u32,
    /// Bounding box of its cells.
    pub lo: Vec2,
    pub hi: Vec2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum VehicleKind {
    Van = 0,
    WaterCannon = 1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum VehicleState {
    Parked = 0,
    Driving = 1,
    Spraying = 2,
}

#[derive(Clone, Copy, Debug)]
pub struct Vehicle {
    pub kind: VehicleKind,
    pub state: VehicleState,
    pub pos: Vec2,
    pub ax: Vec2,
    pub half: Vec2,
    pub speed: f32,
    pub path: [u32; PATH],
    pub path_len: u8,
    pub path_i: u8,
    pub depot: u16,
    /// Squad riding in this van, or `NO_SQUAD`.
    pub squad: u16,
    pub cargo: u16,
    pub blocked: u16,
    pub spray: Vec2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Arrest {
    pub officer: u32,
    pub progress: u16,
    pub lost: u16,
}

pub struct Police {
    pub squads: Vec<Squad>,
    /// By officer stable id (`0..officers.len()`).
    pub officers: Vec<OfficerInfo>,
    /// Officers on the map, by stable id, sorted.
    pub on_map: Vec<u32>,
    pub organizers: Vec<u32>,
    pub vehicles: Vec<Vehicle>,
    pub escalation: Escalation,
    pub cap: Escalation,
    pub max_reached: Escalation,
    /// Decaying count of recent violence (fed by the outcome accounting).
    pub violence: f32,
    pub calm_ticks: u32,
    pub clusters: Vec<Cluster>,
    /// Being arrested, keyed by target stable id.
    pub arrests: SparseSet<Arrest>,
    /// Officer stable id → stable id of the restrained person they escort.
    pub escorting: SparseSet<u32>,
    /// People the police have identified (filmed, surveilled).
    pub watch: SparseSet<u8>,
    pub goal_pos: [Vec2; MAX_GOALS],
    pub goal_used: [u32; MAX_GOALS],
    pub aggression: f32,
    pub forced: Option<Escalation>,
    /// Scratch for the director's clustering: each active field cell's
    /// dense-cell label, the dense cells and their union-find parents.
    pub label: Vec<u32>,
    pub cells: Vec<(u32, u32, u32)>,
    pub parent: Vec<u32>,
    /// Each union-find root's cluster accumulator (`u32::MAX`: none yet).
    pub acc_of: Vec<u32>,
    /// Each slot's dense cells (`CL` per slot), and how many.
    pub dense: Vec<u16>,
    pub dense_n: Vec<u32>,
    road_dist: Vec<f32>,
    road_prev: Vec<u32>,
    road_heap: BinaryHeap<Reverse<(u32, u32)>>,
    pub closed_metro: bool,
    /// The strategic layer: crowds, cordons, ambushes, what it has learnt.
    pub cmd: Commander,
}

impl Police {
    pub fn new(
        setup: &RiotSetup,
        map: &Map,
        _cap: usize,
        max_people: usize,
    ) -> Result<Police, SetupError> {
        let mut squads = Vec::with_capacity(setup.police.squads.len());
        let mut officers = Vec::new();
        let mut next = 0u32;
        for (k, d) in setup.police.squads.iter().enumerate() {
            let depot = map.depots.iter().position(|p| p.name == d.depot);
            let staging = d
                .staging
                .as_ref()
                .map(|s| map.staging.iter().position(|p| p.name == *s));
            let home = match (staging, depot) {
                (Some(Some(s)), _) => map.staging[s].pos,
                (Some(None), _) => {
                    return Err(SetupError::UnknownDepot(
                        d.staging.clone().unwrap_or_default(),
                    ));
                }
                (None, Some(dp)) => map.depots[dp].pos,
                (None, None) if map.depots.is_empty() && map.staging.is_empty() => {
                    map.entries.first().map_or(Vec2::ZERO, |e| e.pos)
                }
                (None, None) => return Err(SetupError::UnknownDepot(d.depot.clone())),
            };
            let n = d.officers.clamp(1, MAX_SQUAD as u8);
            let mut ids = [u32::MAX; MAX_SQUAD];
            for (s, id) in ids.iter_mut().enumerate().take(n as usize) {
                *id = next;
                officers.push(OfficerInfo {
                    squad: k as u16,
                    slot: s as u8,
                });
                next += 1;
            }
            squads.push(Squad {
                kind: d.kind,
                doctrine: Doctrine::Idle,
                officers: ids,
                n,
                depot: depot.unwrap_or(0) as u16,
                home,
                available: minutes(d.available_min),
                deployed: false,
                anchor: home,
                facing: Vec2::new(1.0, 0.0),
                // Riot squads close up into a shield wall.
                width: n as f32 * if d.kind == SquadKind::Riot { 0.62 } else { 1.1 },
                radius: 0.0,
                cluster: u16::MAX,
                target_person: u32::MAX,
                goal: NO_GOAL,
                morale: 0.8,
                pos: home,
                present: 0,
                van: NO_SQUAD,
                ring: (0, 1),
                cluster_at: Vec2::new(f32::MAX, f32::MAX),
                assigned: 0,
                track: NO_TRACK,
                cordon: NO_CORDON,
                since: 0,
                fatigue: 0.0,
                sweep: Vec2::ZERO,
                broken: false,
            });
        }
        let mut vehicles = Vec::new();
        let roads = !map.roads.nodes.is_empty();
        for (k, dp) in map.depots.iter().enumerate() {
            let mut add = |kind: VehicleKind| {
                vehicles.push(Vehicle {
                    kind,
                    state: VehicleState::Parked,
                    pos: dp.pos,
                    ax: Vec2::new(1.0, 0.0),
                    half: if kind == VehicleKind::Van {
                        Vec2::new(2.6, 1.0)
                    } else {
                        Vec2::new(4.0, 1.25)
                    },
                    speed: 0.0,
                    path: [0; PATH],
                    path_len: 0,
                    path_i: 0,
                    depot: k as u16,
                    squad: NO_SQUAD,
                    cargo: 0,
                    blocked: 0,
                    spray: Vec2::ZERO,
                });
            };
            if roads {
                for _ in 0..setup.police.vans_per_depot {
                    add(VehicleKind::Van);
                }
            }
            let wc = setup.police.water_cannons as usize;
            let nd = map.depots.len().max(1);
            if roads && k < wc % nd + wc / nd * nd && (wc / nd + usize::from(k < wc % nd)) > 0 {
                for _ in 0..(wc / nd + usize::from(k < wc % nd)) {
                    add(VehicleKind::WaterCannon);
                }
            }
        }
        let nodes = map.roads.nodes.len();
        let pool = setup.limits.field_chunks as usize;
        Ok(Police {
            squads,
            on_map: Vec::with_capacity(officers.len()),
            officers,
            organizers: Vec::with_capacity(setup.organizers as usize),
            vehicles,
            escalation: Escalation::Presence,
            cap: setup.escalation_cap,
            max_reached: Escalation::Presence,
            violence: 0.0,
            calm_ticks: 0,
            clusters: Vec::with_capacity(MAX_CLUSTERS),
            arrests: SparseSet::new(max_people.max(1), 512),
            escorting: SparseSet::new(max_people.max(1), 512),
            watch: SparseSet::new(max_people.max(1), 4096),
            goal_pos: [Vec2::ZERO; MAX_GOALS],
            goal_used: [0; MAX_GOALS],
            aggression: setup.police.aggression.clamp(0.0, 1.0),
            forced: None,
            label: vec![u32::MAX; pool * CL],
            cells: Vec::with_capacity(pool * CL),
            parent: Vec::with_capacity(pool * CL),
            acc_of: Vec::with_capacity(pool * CL),
            dense: vec![0; pool * CL],
            dense_n: vec![0; pool],
            road_dist: vec![f32::INFINITY; nodes],
            road_prev: vec![u32::MAX; nodes],
            road_heap: BinaryHeap::with_capacity(nodes * 4 + 16),
            closed_metro: false,
            cmd: Commander::new(map),
        })
    }

    #[inline]
    pub fn officer(&self, stable: u32) -> Option<&OfficerInfo> {
        self.officers.get(stable as usize)
    }

    pub fn force_escalation(&mut self, rung: u8, cap: Escalation) {
        let r = Escalation::from_u8(rung).min(cap);
        self.forced = Some(r);
        self.escalation = r;
        self.max_reached = self.max_reached.max(r);
    }

    /// Vehicles as obstacles for the body phase.
    pub fn vehicle_obbs(&self, out: &mut Vec<Obb>) {
        for (k, v) in self.vehicles.iter().enumerate() {
            if out.len() < out.capacity() {
                out.push(Obb {
                    c: v.pos,
                    ax: v.ax,
                    half: v.half,
                    kind: 1,
                    id: k as u16,
                });
            }
        }
    }

    pub fn hash_into(&self, h: &mut StateHasher) {
        h.write_u32(self.escalation as u32);
        h.write_f32(self.violence);
        for s in &self.squads {
            h.write_u32(s.doctrine as u32 | (s.deployed as u32) << 8 | (s.n as u32) << 16);
            h.write_f32(s.anchor.x);
            h.write_f32(s.anchor.y);
            h.write_f32(s.radius);
            h.write_f32(s.morale);
            h.write_u32(s.target_person);
            h.write_u32(s.track ^ (s.cordon as u32) << 24);
        }
        self.cmd.hash_into(h);
        for v in &self.vehicles {
            h.write_f32(v.pos.x);
            h.write_f32(v.pos.y);
            h.write_u32(v.state as u32 | (v.cargo as u32) << 8);
        }
        for &(k, a) in self.arrests.entries() {
            h.write_u64(k as u64 | (a.officer as u64) << 32);
            h.write_u32(a.progress as u32);
        }
        for &(k, t) in self.escorting.entries() {
            h.write_u64(k as u64 | (t as u64) << 32);
        }
    }
}

// ---------------------------------------------------------------------------

/// Put the squads that start on the scene on the map.
pub fn spawn_initial(w: &mut World) {
    for s in 0..w.police.squads.len() {
        if w.police.squads[s].available == 0 {
            deploy_on_foot(w, s);
        }
    }
}

pub fn deploy_on_foot(w: &mut World, s: usize) {
    let home = w.police.squads[s].home;
    deploy_at(w, s, home);
}

/// Put squad `s`'s officers on the map around `at`.
pub fn deploy_at(w: &mut World, s: usize, at: Vec2) {
    let sq = w.police.squads[s];
    let arch = if sq.kind == SquadKind::Riot {
        Archetype::RiotOfficer
    } else {
        Archetype::Officer
    };
    for k in 0..sq.n as usize {
        let stable = sq.officers[k];
        if w.slot_of[stable as usize] != NO_SLOT {
            continue;
        }
        if w.people.len >= w.people.cap {
            break;
        }
        let row = (k / 4) as f32;
        let col = (k % 4) as f32 - 1.5;
        let p = at + Vec2::new(col * 0.9, row * 0.9);
        let p = w.map.nearest_open(p, 12.0).unwrap_or(at);
        let ap = &w.arch[arch as usize];
        let r = roll(w.seed, w.tick, stable, Stream::Appearance);
        let item = if arch == Archetype::RiotOfficer {
            Item::Shield
        } else {
            Item::Baton
        };
        let mut fl = flag::PHONE | flag::ITEM;
        if arch == Archetype::RiotOfficer {
            fl |= flag::SHIELD;
        }
        let i = w.people.push(Spawn {
            pos: p,
            heading: 0,
            emo: [
                narrow_u8(ap.base_fear),
                narrow_u8(ap.base_anger),
                narrow_u8(ap.base_morale),
                0,
            ],
            health: health::FULL,
            action: Action::Formation as u8,
            archetype: arch as u8,
            flags: fl,
            intent: 0,
            belief: 0,
            stable_id: stable,
        });
        w.slot_of[stable as usize] = i as u32;
        w.issued = w.issued.max(stable + 1);
        let c = &mut w.cold[stable as usize];
        c.appearance = appearance_for(arch, r, item).0;
        c.item = item as u8;
        c.archetype = arch as u8;
        c.arrived = w.tick;
        w.officer_targets[stable as usize] = OfficerTarget {
            pos: p,
            goal: NO_GOAL,
            face: p + Vec2::new(1.0, 0.0),
            run: false,
        };
        if let Err(at) = w.police.on_map.binary_search(&stable) {
            w.police.on_map.insert(at, stable);
        }
    }
    let sq = &mut w.police.squads[s];
    sq.deployed = true;
    sq.doctrine = Doctrine::Reserve;
    sq.anchor = at;
    sq.pos = at;
    w.outcome.officers_deployed += sq.n as u32;
}

/// Road Dijkstra from the vehicle's position to the node nearest `dest`.
pub fn plan_route(w: &mut World, v: usize, dest: Vec2) -> bool {
    let roads = &w.map.roads;
    let (Some(a), Some(b)) = (roads.nearest(w.police.vehicles[v].pos), roads.nearest(dest)) else {
        return false;
    };
    let p = &mut w.police;
    p.road_dist.fill(f32::INFINITY);
    p.road_prev.fill(u32::MAX);
    p.road_heap.clear();
    p.road_dist[a as usize] = 0.0;
    p.road_heap.push(Reverse((0, a)));
    while let Some(Reverse((dq, n))) = p.road_heap.pop() {
        let d = dq as f32 * 0.1;
        if n == b {
            break;
        }
        if d > p.road_dist[n as usize] + 0.05 {
            continue;
        }
        let s0 = roads.start[n as usize] as usize;
        let s1 = roads.start[n as usize + 1] as usize;
        for &(m, len) in &roads.adj[s0..s1] {
            let nd = p.road_dist[n as usize] + len;
            if nd < p.road_dist[m as usize] {
                p.road_dist[m as usize] = nd;
                p.road_prev[m as usize] = n;
                if p.road_heap.len() < p.road_heap.capacity() {
                    p.road_heap.push(Reverse(((nd * 10.0) as u32, m)));
                }
            }
        }
    }
    if p.road_prev[b as usize] == u32::MAX && a != b {
        return false;
    }
    // Walk back from b, then reverse into the fixed path.
    let veh = &mut p.vehicles[v];
    let mut tmp = [0u32; PATH];
    let mut n = 0usize;
    let mut cur = b;
    while n < PATH {
        tmp[n] = cur;
        n += 1;
        if cur == a {
            break;
        }
        cur = p.road_prev[cur as usize];
        if cur == u32::MAX {
            break;
        }
    }
    for k in 0..n {
        veh.path[k] = tmp[n - 1 - k];
    }
    veh.path_len = n as u8;
    veh.path_i = 0;
    veh.state = VehicleState::Driving;
    veh.blocked = 0;
    true
}
