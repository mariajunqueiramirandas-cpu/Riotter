//! Police (DESIGN §2.5, §14).
//!
//! - **Squads** are a small AoS table. Each runs a doctrine state machine:
//!   hold line, advance, kettle, disperse, snatch arrest, retreat.
//! - **The director** replans every `director_interval` ticks. It clusters
//!   the crowd with union-find over dense cells in the active field chunks,
//!   climbs (or descends) the escalation ladder within the setup's cap, and
//!   assigns squads to clusters.
//! - **Formations**: a line is slots along a segment; a kettle is slots on a
//!   contracting circle around a cluster. Officers steer to their slots with
//!   the shared steering kernel.
//! - **Vehicles** drive along the road graph: vans bring reinforcements from
//!   depots and take arrestees away; water cannon trucks spray.
//!
//! Officers are bodies with mass and shields, so pushing matches between a
//! line and a crowd emerge without special code.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use riot_core::math::{narrow_u8, widen_u8};
use riot_core::rng::{Stream, roll, roll_key, unit};
use riot_core::{SparseSet, StateHasher, Vec2};

use crate::archetype::Archetype;
use crate::fields::{CC, CL, Layer, NO_CHUNK};
use crate::flow::{GOAL_POLICE, MAX_GOALS, NO_GOAL};
use crate::map::Map;
use crate::mapdef::EntryKind;
use crate::people::Spawn;
use crate::physics::Obb;
use crate::setup::{Escalation, NotableKind, RiotSetup, SquadKind, minutes};
use crate::state::{Action, Gait, Item, emo, flag, health, pose, stat};
use crate::steer::OfficerTarget;
use crate::world::{NO_SLOT, SetupError, World, appearance_for};

pub const MAX_SQUAD: usize = 16;
pub const NO_SQUAD: u16 = u16::MAX;
const MAX_CLUSTERS: usize = 32;
/// Most squads committed to one cluster.
const MAX_PICK: usize = 16;
/// Ticks a squad holds its post after its crowd stops being assigned.
const STAND_DOWN_TICKS: u32 = 1200;
/// Crowds smaller than this get squads only when violent.
const MIN_POLICED: f32 = 150.0;
/// How far a snatch squad goes into a crowd for someone (m).
const SNATCH_REACH: f32 = 25.0;
/// How close to the crowd's front a snatch squad must be to go in (m).
const SNATCH_STAGING: f32 = 40.0;
const PATH: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum Doctrine {
    /// Not yet available, or in a van.
    #[default]
    Idle = 0,
    Travel = 1,
    HoldLine = 2,
    Advance = 3,
    Kettle = 4,
    Disperse = 5,
    Snatch = 6,
    Retreat = 7,
    Reserve = 8,
}

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
    pub gas_ready: u32,
    pub aggression: f32,
    forced: Option<Escalation>,
    label: Vec<u32>,
    cells: Vec<(u32, u32, u32)>,
    parent: Vec<u32>,
    road_dist: Vec<f32>,
    road_prev: Vec<u32>,
    road_heap: BinaryHeap<Reverse<(u32, u32)>>,
    pub closed_metro: bool,
    pub kettle_closed: [bool; MAX_CLUSTERS],
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
            gas_ready: 0,
            aggression: setup.police.aggression.clamp(0.0, 1.0),
            forced: None,
            label: vec![u32::MAX; pool * CL],
            cells: Vec::with_capacity(pool * CL),
            parent: Vec::with_capacity(pool * CL),
            road_dist: vec![f32::INFINITY; nodes],
            road_prev: vec![u32::MAX; nodes],
            road_heap: BinaryHeap::with_capacity(nodes * 4 + 16),
            closed_metro: false,
            kettle_closed: [false; MAX_CLUSTERS],
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
        }
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

fn deploy_on_foot(w: &mut World, s: usize) {
    let home = w.police.squads[s].home;
    deploy_at(w, s, home);
}

/// Put squad `s`'s officers on the map around `at`.
fn deploy_at(w: &mut World, s: usize, at: Vec2) {
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

/// Phase 9: the director every `director_interval` ticks; squads, arrests and
/// vehicles every tick.
pub fn director(w: &mut World) {
    let tick = w.tick;
    // Reinforcements that became available.
    for s in 0..w.police.squads.len() {
        let sq = w.police.squads[s];
        if !sq.deployed && sq.van == NO_SQUAD && tick >= sq.available && sq.available > 0 {
            dispatch(w, s);
        }
    }
    let every = w.tun.director_interval.max(1.0) as u32;
    if tick.is_multiple_of(every) {
        clusters(w);
        escalate(w);
        assign(w);
        surveil(w);
        use_force(w);
    }
    update_squads(w);
    arrests(w);
    vehicles(w);
    // Officers who left the map.
    let slot_of = &w.slot_of;
    w.police.on_map.retain(|&s| slot_of[s as usize] != NO_SLOT);
}

/// Send squad `s` from its depot: by van along the roads when there is one.
fn dispatch(w: &mut World, s: usize) {
    let depot = w.police.squads[s].depot;
    let van = w.police.vehicles.iter().position(|v| {
        v.kind == VehicleKind::Van
            && v.depot == depot
            && v.squad == NO_SQUAD
            && v.state == VehicleState::Parked
    });
    let dest = w
        .police
        .clusters
        .first()
        .map(|c| c.centroid)
        .unwrap_or(w.police.squads[s].home);
    match van {
        Some(v) if plan_route(w, v, dest) => {
            w.police.vehicles[v].squad = s as u16;
            w.police.squads[s].van = v as u16;
            w.police.squads[s].doctrine = Doctrine::Travel;
        }
        _ => deploy_on_foot(w, s),
    }
}

/// Road Dijkstra from the vehicle's position to the node nearest `dest`.
fn plan_route(w: &mut World, v: usize, dest: Vec2) -> bool {
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

/// Union-find over dense cells in the active field chunks (DESIGN §14).
fn clusters(w: &mut World) {
    let f = &w.fields;
    let p = &mut w.police;
    p.cells.clear();
    p.parent.clear();
    for slot in 0..f.pool {
        let c = f.slot_chunk[slot];
        let lab = &mut p.label[slot * CL..(slot + 1) * CL];
        if c == NO_CHUNK {
            continue;
        }
        let dens = f.layer(slot, Layer::Dens);
        for (cell, &d) in dens.iter().enumerate() {
            if d >= 1.0 {
                lab[cell] = p.cells.len() as u32;
                p.parent.push(p.cells.len() as u32);
                p.cells.push((slot as u32, cell as u32, c));
            } else {
                lab[cell] = u32::MAX;
            }
        }
    }
    fn find(par: &mut [u32], mut x: u32) -> u32 {
        while par[x as usize] != x {
            par[x as usize] = par[par[x as usize] as usize];
            x = par[x as usize];
        }
        x
    }
    let cdim = f.cdim as i32;
    for k in 0..p.cells.len() {
        let (slot, cell, c) = p.cells[k];
        let (cx, cy) = ((c % f.cdim) as i32, (c / f.cdim) as i32);
        let (lx, ly) = ((cell as usize % CC) as i32, (cell as usize / CC) as i32);
        let (gx, gy) = (cx * CC as i32 + lx, cy * CC as i32 + ly);
        for (dx, dy) in [(1, 0), (0, 1), (1, 1), (-1, 1)] {
            let (nx, ny) = (gx + dx, gy + dy);
            let (ncx, ncy) = (nx.div_euclid(CC as i32), ny.div_euclid(CC as i32));
            if ncx < 0 || ncy < 0 || ncx >= cdim || ncy >= cdim {
                continue;
            }
            let ns = if ncx == cx && ncy == cy {
                Some(slot as usize)
            } else {
                f.slot_of_chunk(ncx, ncy)
            };
            let Some(ns) = ns else { continue };
            let ncell =
                (ny.rem_euclid(CC as i32) as usize) * CC + nx.rem_euclid(CC as i32) as usize;
            let l = p.label[ns * CL + ncell];
            if l == u32::MAX {
                continue;
            }
            let (a, b) = (find(&mut p.parent, k as u32), find(&mut p.parent, l));
            if a != b {
                let (lo, hi) = if a < b { (a, b) } else { (b, a) };
                p.parent[hi as usize] = lo;
            }
        }
    }
    // Accumulate per root, roots in index order.
    let mut acc: [(u32, f64, f64, f64, f64, f64, u32, f64); MAX_CLUSTERS * 4] =
        [(u32::MAX, 0.0, 0.0, 0.0, 0.0, 0.0, 0, 0.0); MAX_CLUSTERS * 4];
    let mut nacc = 0usize;
    for k in 0..p.cells.len() {
        let root = find(&mut p.parent, k as u32);
        let (slot, cell, c) = p.cells[k];
        let d = f.layer(slot as usize, Layer::Dens)[cell as usize] as f64;
        let (cx, cy) = (c % f.cdim, c / f.cdim);
        let (gx, gy) = (
            cx * CC as u32 + cell % CC as u32,
            cy * CC as u32 + cell / CC as u32,
        );
        let (x, y) = f.geom.centre(gx, gy);
        let e = match acc[..nacc].iter().position(|a| a.0 == root) {
            Some(e) => e,
            None if nacc < acc.len() => {
                acc[nacc] = (root, 0.0, 0.0, 0.0, 0.0, 0.0, 0, 0.0);
                nacc += 1;
                nacc - 1
            }
            None => continue,
        };
        let a = &mut acc[e];
        a.1 += d;
        a.2 += x as f64 * d;
        a.3 += y as f64 * d;
        a.4 += f.layer(slot as usize, Layer::Anger)[cell as usize] as f64;
        a.7 += f.layer(slot as usize, Layer::Weight)[cell as usize] as f64;
        a.5 += f.layer(slot as usize, Layer::Threat)[cell as usize] as f64;
        a.6 += 1;
    }
    p.clusters.clear();
    let mut order: [usize; MAX_CLUSTERS * 4] = [0; MAX_CLUSTERS * 4];
    for (k, o) in order.iter_mut().enumerate().take(nacc) {
        *o = k;
    }
    order[..nacc].sort_unstable_by(|&a, &b| acc[b].1.total_cmp(&acc[a].1).then(a.cmp(&b)));
    for &k in order[..nacc].iter() {
        let a = acc[k];
        if a.1 < 40.0 || p.clusters.len() == MAX_CLUSTERS {
            continue;
        }
        let area = a.6 as f32 * 4.0;
        p.clusters.push(Cluster {
            centroid: Vec2::new((a.2 / a.1) as f32, (a.3 / a.1) as f32),
            count: a.1 as f32,
            radius: (area / core::f32::consts::PI).sqrt(),
            anger: (a.4 / a.7.max(1e-3)) as f32,
            threat: a.5 as f32,
            cells: a.6,
        });
    }
}

/// Climb the ladder in response to violence; step down after calm.
fn escalate(w: &mut World) {
    let p = &mut w.police;
    if let Some(f) = p.forced {
        p.escalation = f;
        return;
    }
    let need = w.tun.escalate_threat * (1.25 - 0.5 * p.aggression);
    let angry = p.clusters.iter().any(|c| c.anger > 0.55 && c.count > 300.0);
    let every = w.tun.director_interval.max(1.0) as u32;
    if (p.violence > need || angry && p.escalation < Escalation::LinesAndKettles)
        && p.escalation < p.cap
    {
        p.escalation = Escalation::from_u8(p.escalation as u8 + 1);
        p.violence *= 0.5;
        p.calm_ticks = 0;
        if p.escalation > p.max_reached {
            p.max_reached = p.escalation;
            w.outcome.notable(
                w.tick,
                NotableKind::EscalationRaised,
                Vec2::ZERO,
                p.escalation as u32,
            );
        }
    } else if p.violence < need * 0.1 {
        p.calm_ticks += every;
        if p.calm_ticks as f32 > w.tun.deescalate_ticks && p.escalation > Escalation::Presence {
            p.escalation = Escalation::from_u8(p.escalation as u8 - 1);
            p.calm_ticks = 0;
        }
    } else {
        p.calm_ticks = 0;
    }
    p.violence *= 0.9;
}

/// Assign squads to clusters with a doctrine that fits the rung.
fn assign(w: &mut World) {
    let rung = w.police.escalation;
    let nclusters = w.police.clusters.len();
    let mut taken = [false; 256];
    // Snatch targets handed out this pass: one squad per person.
    let mut targets = [u32::MAX; 256];
    let mut nt = 0;
    // Protected point: the government quarter, else the squads' home.
    let protect = w
        .map
        .areas
        .iter()
        .find(|a| a.tag == "government")
        .map(|a| a.pos);
    for k in 0..w.police.squads.len().min(256) {
        let sq = &w.police.squads[k];
        if !sq.deployed || sq.present == 0 {
            taken[k] = true;
        }
        if sq.morale < 0.3 {
            let sq = &mut w.police.squads[k];
            sq.doctrine = Doctrine::Retreat;
            sq.anchor = sq.home;
            taken[k] = true;
        }
    }
    for ci in 0..nclusters {
        let c = w.police.clusters[ci];
        let violent = c.threat > 6.0 || w.police.violence > 4.0;
        if c.count < MIN_POLICED && c.threat <= 6.0 {
            continue;
        }
        // A kettle once begun is finished, not dropped when the trouble that
        // started it fades.
        let kettled = w.police.squads.iter().any(|sq| {
            sq.doctrine == Doctrine::Kettle && (sq.cluster_at - c.centroid).len() < c.radius + 30.0
        });
        let ring_it =
            rung >= Escalation::LinesAndKettles && (violent || kettled) && c.count < 1500.0;
        // Enough squads for the crowd, and to ring it if it may be kettled:
        // officers at most 1.4 m apart around its packed radius.
        let mut want = (c.count / 350.0).ceil() as usize;
        if ring_it {
            let packed_r = (c.count / (2.5 * core::f32::consts::PI)).sqrt().max(4.0);
            want = want.max((2.0 * core::f32::consts::PI * packed_r / 1.4 / 10.0).ceil() as usize);
        }
        let want = want.clamp(1, MAX_PICK);
        // Nearest free squads, preferring the right kinds.
        let mut picked = [usize::MAX; MAX_PICK];
        let mut np = 0;
        for _ in 0..want {
            let mut best: Option<(f32, usize)> = None;
            for (k, sq) in w.police.squads.iter().enumerate().take(256) {
                if taken[k] {
                    continue;
                }
                let mut d = (sq.pos - c.centroid).len();
                if (sq.cluster_at - c.centroid).len() < c.radius + 30.0 {
                    d *= 0.2;
                }
                if sq.kind == SquadKind::Snatch && rung < Escalation::BatonsAndArrests {
                    d += 5000.0;
                }
                if best.is_none_or(|b| d < b.0) {
                    best = Some((d, k));
                }
            }
            let Some((_, k)) = best else { break };
            taken[k] = true;
            picked[np] = k;
            np += 1;
        }
        let kettle = ring_it && np >= 2 || rung >= Escalation::MassArrests;
        let n_ring = picked[..np]
            .iter()
            .filter(|&&k| w.police.squads[k].kind != SquadKind::Snatch)
            .count()
            .max(1) as u8;
        let mut ring_i = 0u8;
        let away = match protect {
            Some(g) => (g - c.centroid).normalize_or_zero(),
            None => Vec2::new(1.0, 0.0),
        };
        let away = if away == Vec2::ZERO {
            Vec2::new(1.0, 0.0)
        } else {
            away
        };
        // The crowd's front, facing what the police protect.
        let front = c.centroid + away * c.radius;
        for (j, &k) in picked[..np].iter().enumerate() {
            // Snatch squads move up behind the line before they go in.
            let near_front = (w.police.squads[k].pos - front).len() < SNATCH_STAGING;
            let target = if w.police.squads[k].kind == SquadKind::Snatch && near_front {
                pick_snatch_target(w, c, w.police.squads[k].pos, &targets[..nt])
            } else {
                u32::MAX
            };
            if target != u32::MAX && nt < targets.len() {
                targets[nt] = target;
                nt += 1;
            }
            let sq = &mut w.police.squads[k];
            sq.cluster = ci as u16;
            sq.cluster_at = c.centroid;
            sq.assigned = w.tick;
            let side = away.perp() * ((j as f32 - (np as f32 - 1.0) * 0.5) * (sq.width + 2.0));
            let line_anchor = c.centroid + away * (c.radius + 10.0) + side;
            let doctrine = match sq.kind {
                SquadKind::Snatch if rung >= Escalation::BatonsAndArrests && target != u32::MAX => {
                    Doctrine::Snatch
                }
                _ if kettle => Doctrine::Kettle,
                SquadKind::Riot if rung >= Escalation::GasAndWater && violent => Doctrine::Disperse,
                SquadKind::Riot if rung >= Escalation::BatonsAndArrests && violent => {
                    Doctrine::Advance
                }
                _ => Doctrine::HoldLine,
            };
            if doctrine == Doctrine::Kettle {
                if sq.doctrine != Doctrine::Kettle || sq.cluster != ci as u16 {
                    sq.radius = c.radius + 6.0;
                }
                sq.anchor = c.centroid;
                sq.ring = (ring_i, n_ring);
                ring_i += 1;
            } else if doctrine == Doctrine::Snatch {
                sq.target_person = target;
            } else if sq.doctrine != doctrine || (sq.anchor - line_anchor).len() > 30.0 {
                sq.anchor = line_anchor;
            }
            sq.facing = -away;
            sq.doctrine = doctrine;
        }
    }
    for k in 0..w.police.squads.len().min(256) {
        if !taken[k] {
            let tick = w.tick;
            let sq = &mut w.police.squads[k];
            sq.cluster = u16::MAX;
            // A crowd that drops out of one replan is usually still there:
            // hold the post a while before standing down.
            let recent = sq.cluster_at.x != f32::MAX
                && tick.saturating_sub(sq.assigned) < STAND_DOWN_TICKS
                && sq.doctrine != Doctrine::Reserve;
            if recent {
                if sq.doctrine == Doctrine::Snatch || sq.doctrine == Doctrine::Kettle {
                    sq.doctrine = Doctrine::HoldLine;
                    sq.anchor = sq.pos;
                }
            } else {
                sq.doctrine = Doctrine::Reserve;
                sq.anchor = sq.home;
            }
        }
    }
    // Route squads far from their anchor through police flow goals.
    for k in 0..w.police.squads.len() {
        let sq = w.police.squads[k];
        if !sq.deployed
            || (sq.pos - sq.anchor).len() < 25.0 && w.map.ray_blocked(sq.pos, sq.anchor).is_none()
        {
            w.police.squads[k].goal = NO_GOAL;
            continue;
        }
        let g = police_goal(w, sq.anchor);
        w.police.squads[k].goal = g;
    }
}

/// A police flow goal near `p`, reusing one within 30 m or replacing the
/// least recently used.
fn police_goal(w: &mut World, p: Vec2) -> u8 {
    let tick = w.tick;
    let mut lru = (u32::MAX, GOAL_POLICE);
    for g in GOAL_POLICE as usize..MAX_GOALS {
        if w.flow.goals[g].active && (w.police.goal_pos[g] - p).len() < 30.0 {
            w.police.goal_used[g] = tick;
            return g as u8;
        }
        if w.police.goal_used[g] < lru.0 {
            lru = (w.police.goal_used[g], g as u8);
        }
    }
    let g = lru.1;
    w.police.goal_pos[g as usize] = p;
    w.police.goal_used[g as usize] = tick;
    w.flow.set_targets(g, &[p], 6.0, tick);
    g
}

/// Organizers identified by surveillance first, then filmed violent people.
/// Someone for the snatch squad at `from` to arrest: a known organizer, else
/// someone filmed starting violence, nearest the squad and close enough to
/// reach through the edge of the crowd. Never a target another squad has.
fn pick_snatch_target(w: &World, c: Cluster, from: Vec2, taken: &[u32]) -> u32 {
    let reach = c.radius + 40.0;
    let eligible = |s: u32, best: &mut Option<(f32, u32)>| {
        let Some(i) = w.index_of(s) else { return };
        if w.police.arrests.contains(s)
            || taken.contains(&s)
            || Gait::from_u8(w.people.pose[i][pose::GAIT]) == Gait::Restrained
            || (w.people.pos(i) - c.centroid).len() > reach
        {
            return;
        }
        let d = (w.people.pos(i) - from).len();
        if d < SNATCH_REACH && best.is_none_or(|b| d < b.0) {
            *best = Some((d, s));
        }
    };
    let mut best: Option<(f32, u32)> = None;
    for &s in &w.police.organizers {
        if w.index_of(s)
            .is_some_and(|i| w.people.flags(i) & flag::KNOWN != 0)
        {
            eligible(s, &mut best);
        }
    }
    if let Some((_, s)) = best {
        return s;
    }
    for &(s, _) in w.police.watch.entries() {
        // The state does not arrest its own.
        if w.index_of(s).is_some_and(|i| {
            Archetype::from_u8(w.people.stat[i][stat::ARCH]) != Archetype::Provocateur
        }) {
            eligible(s, &mut best);
        }
    }
    best.map_or(u32::MAX, |b| b.1)
}

/// Officers recognise organizers near them; footage identifies people.
fn surveil(w: &mut World) {
    for k in 0..w.police.organizers.len() {
        let s = w.police.organizers[k];
        let Some(i) = w.index_of(s) else { continue };
        if w.people.flags(i) & flag::KNOWN != 0 {
            continue;
        }
        let p = w.people.pos(i);
        let seen = w.police.on_map.iter().any(|&o| {
            w.index_of(o)
                .is_some_and(|j| (w.people.pos(j) - p).len2() < 900.0)
        });
        let filmed = w.fields.sample(Layer::Camera, p.x, p.y) > 2.0;
        if seen || filmed {
            w.people.stat[i][stat::FLAGS] |= flag::KNOWN;
        }
    }
}

/// Gas and water cannons at the top rungs; metro closures.
fn use_force(w: &mut World) {
    let rung = w.police.escalation;
    if rung < Escalation::GasAndWater {
        return;
    }
    let tick = w.tick;
    for ci in 0..w.police.clusters.len().min(4) {
        let c = w.police.clusters[ci];
        let violent = c.threat > 4.0 || w.police.violence > 3.0;
        if !violent {
            continue;
        }
        // Tear gas from the nearest riot officer.
        if tick >= w.police.gas_ready {
            let mut thrower: Option<(f32, u32)> = None;
            for &o in &w.police.on_map {
                let Some(i) = w.index_of(o) else { continue };
                if w.people.stat[i][stat::ARCH] != Archetype::RiotOfficer as u8 {
                    continue;
                }
                let d = (w.people.pos(i) - c.centroid).len();
                if d < 60.0 && thrower.is_none_or(|t| d < t.0) {
                    thrower = Some((d, o));
                }
            }
            if let Some((_, o)) = thrower {
                let i = w.index_of(o).unwrap();
                let from = w.people.pos(i);
                let jitter = Vec2::from_heading(roll(w.seed, tick, o, Stream::Police) as u8)
                    * (c.radius * 0.4);
                let to = c.centroid + jitter;
                w.hazards
                    .throw_canister(from, to, o, w.tun.throw_speed * 1.3);
                w.outcome.police_act(tick, from, to, &w.fields, 1.0, o);
                if !w.outcome.first_gas {
                    w.outcome.first_gas = true;
                    w.outcome.notable(tick, NotableKind::FirstGas, to, o);
                }
                w.police.gas_ready = tick + 200;
            }
        }
        // Water cannon trucks drive to the cluster's edge.
        for v in 0..w.police.vehicles.len() {
            let veh = w.police.vehicles[v];
            if veh.kind != VehicleKind::WaterCannon || veh.state != VehicleState::Parked {
                continue;
            }
            if (veh.pos - c.centroid).len() > c.radius + w.tun.water_range {
                let dest =
                    c.centroid + (veh.pos - c.centroid).normalize_or_zero() * (c.radius + 12.0);
                plan_route(w, v, dest);
            } else {
                w.police.vehicles[v].state = VehicleState::Spraying;
                w.police.vehicles[v].spray = c.centroid;
                if !w.outcome.first_water {
                    w.outcome.first_water = true;
                    w.outcome
                        .notable(tick, NotableKind::FirstWaterCannon, veh.pos, u32::MAX);
                }
            }
        }
    }
    // Close metro stations near big clusters.
    if !w.police.closed_metro && w.police.clusters.iter().any(|c| c.count > 800.0) {
        w.police.closed_metro = true;
        for k in 0..w.map.entries.len() {
            if w.map.entries[k].kind == EntryKind::Metro {
                w.entry_closed[k] = true;
            }
        }
        w.retarget_exits();
        w.outcome
            .notable(tick, NotableKind::MetroClosed, Vec2::ZERO, u32::MAX);
    }
}

/// Formation slots → officer steering targets; squad morale and position.
fn update_squads(w: &mut World) {
    let tick = w.tick;
    let shrink = w.tun.kettle_shrink;
    for k in 0..w.police.squads.len() {
        let sq = w.police.squads[k];
        if !sq.deployed {
            continue;
        }
        // Position, presence and morale from the officers.
        let (mut sum, mut n, mut fear) = (Vec2::ZERO, 0u32, 0.0f32);
        for &o in &sq.officers[..sq.n as usize] {
            if let Some(i) = w.index_of(o) {
                sum += w.people.pos(i);
                fear += widen_u8(w.people.emo[i][emo::FEAR]);
                n += 1;
            }
        }
        let squad = &mut w.police.squads[k];
        squad.present = n as u8;
        if n == 0 {
            continue;
        }
        squad.pos = sum * (1.0 / n as f32);
        let fear = fear / n as f32;
        squad.morale += 0.002 * ((1.0 - fear) * (n as f32 / squad.n as f32) - squad.morale);
        // Doctrine motion.
        match squad.doctrine {
            Doctrine::Advance | Doctrine::Disperse => {
                let target = w
                    .police
                    .clusters
                    .get(squad.cluster as usize)
                    .map(|c| c.centroid);
                if let Some(t) = target {
                    let speed = if squad.doctrine == Doctrine::Disperse {
                        1.0
                    } else {
                        0.5
                    };
                    let to = t - squad.anchor;
                    // Advance only while the line keeps up.
                    if (squad.pos - squad.anchor).len() < 4.0 && to.len() > 1.0 {
                        squad.anchor += to.normalize_or_zero() * (speed * crate::setup::DT);
                    }
                    squad.facing = to.normalize_or_zero();
                }
            }
            Doctrine::Kettle => {
                if let Some(c) = w.police.clusters.get(squad.cluster as usize) {
                    let min_r = (c.count / (2.5 * core::f32::consts::PI)).sqrt().max(4.0);
                    if (squad.pos - squad.anchor).len() < squad.radius + 6.0 {
                        squad.radius = (squad.radius * (1.0 - shrink)).max(min_r);
                    }
                }
            }
            Doctrine::Snatch => {
                let t = squad.target_person;
                match w.slot_of.get(t as usize).copied() {
                    Some(j)
                        if j != NO_SLOT
                            && Gait::from_u8(w.people.pose[j as usize][pose::GAIT])
                                != Gait::Restrained =>
                    {
                        squad.anchor = w.people.pos(j as usize);
                    }
                    _ => {
                        squad.target_person = u32::MAX;
                        squad.doctrine = Doctrine::Reserve;
                        squad.anchor = squad.home;
                    }
                }
            }
            _ => {}
        }
        let sq = w.police.squads[k];
        // Slots.
        let count = sq.n as usize;
        for (slot, &o) in sq.officers[..count].iter().enumerate() {
            let Some(i) = w.index_of(o) else { continue };
            let me = w.people.pos(i);
            let mut t = OfficerTarget {
                pos: sq.anchor,
                goal: sq.goal,
                face: sq.anchor + sq.facing * 10.0,
                run: false,
            };
            match sq.doctrine {
                Doctrine::HoldLine
                | Doctrine::Advance
                | Doctrine::Disperse
                | Doctrine::Reserve
                | Doctrine::Retreat => {
                    let side = sq.facing.perp();
                    let off = (slot as f32 - (count as f32 - 1.0) * 0.5)
                        * (sq.width / count.max(1) as f32);
                    t.pos = sq.anchor + side * off;
                    if sq.doctrine == Doctrine::Reserve || sq.doctrine == Doctrine::Retreat {
                        let row = (slot / 4) as f32;
                        t.pos =
                            sq.anchor + side * (((slot % 4) as f32 - 1.5) * 1.0) - sq.facing * row;
                        t.run = sq.doctrine == Doctrine::Retreat;
                    }
                }
                Doctrine::Kettle => {
                    let (ri, rn) = sq.ring;
                    let total = rn.max(1) as f32 * count as f32;
                    let m = ri as f32 * count as f32 + slot as f32;
                    let h = ((m / total) * 256.0) as u32 as u8;
                    t.pos = sq.anchor + Vec2::from_heading(h) * sq.radius;
                    t.face = sq.anchor;
                    // Run to close the ring; walk once in place.
                    t.run = (me - t.pos).len() > 4.0;
                }
                Doctrine::Snatch => {
                    t.pos = sq.anchor;
                    t.face = sq.anchor;
                    t.run = true;
                    // Straight at them only when no wall is in the way.
                    if (me - sq.anchor).len() < 25.0 && w.map.ray_blocked(me, sq.anchor).is_none() {
                        t.goal = NO_GOAL;
                    }
                }
                Doctrine::Idle | Doctrine::Travel => {}
            }
            if let Some(&target) = w.police.escorting.get(o) {
                // Walk the arrestee to the squad's van, or home.
                let van = (sq.van != NO_SQUAD).then(|| w.police.vehicles[sq.van as usize].pos);
                t.pos = van.unwrap_or(sq.home);
                t.goal = if (me - t.pos).len() > 25.0 {
                    police_goal_ro(w, t.pos)
                } else {
                    NO_GOAL
                };
                let _ = target;
            }
            // A new order wakes a sleeping officer (DESIGN §7.6).
            if (t.pos - me).len2() > 2.25 {
                let st = &mut w.people.stat[i][stat::FLAGS];
                if *st & flag::ASLEEP != 0 {
                    *st = (*st & !flag::ASLEEP) | flag::URGENT;
                }
            }
            w.officer_targets[o as usize] = t;
        }
        let _ = tick;
    }
    // Kettle closure notable.
    for ci in 0..w.police.clusters.len() {
        if w.police.kettle_closed[ci] {
            continue;
        }
        let ring: Vec2 = w.police.clusters[ci].centroid;
        let mut gaps_ok = true;
        let mut any = false;
        for sq in &w.police.squads {
            if sq.doctrine != Doctrine::Kettle || sq.cluster != ci as u16 {
                continue;
            }
            any = true;
            let circ = 2.0 * core::f32::consts::PI * sq.radius;
            let total = sq.ring.1.max(1) as f32 * sq.n as f32;
            if circ / total > 1.6 || (sq.pos - ring).len() > sq.radius + 3.0 {
                gaps_ok = false;
            }
        }
        if any && gaps_ok {
            w.police.kettle_closed[ci] = true;
            w.outcome
                .notable(w.tick, NotableKind::KettleClosed, ring, u32::MAX);
        }
    }
}

/// An existing police goal near `p` (read-only: no new goal is made).
fn police_goal_ro(w: &World, p: Vec2) -> u8 {
    for g in GOAL_POLICE as usize..MAX_GOALS {
        if w.flow.goals[g].active && (w.police.goal_pos[g] - p).len() < 30.0 {
            return g as u8;
        }
    }
    NO_GOAL
}

/// Restrain, escort and remove arrestees.
fn arrests(w: &mut World) {
    let need = w.tun.arrest_ticks.max(1.0) as u16;
    let mut k = 0;
    while k < w.police.arrests.len() {
        let (target, a) = w.police.arrests.entries()[k];
        let (ti, oi) = (w.index_of(target), w.index_of(a.officer));
        let (Some(ti), Some(oi)) = (ti, oi) else {
            w.police.arrests.remove(target);
            continue;
        };
        let d = (w.people.pos(ti) - w.people.pos(oi)).len();
        let e = w.police.arrests.get_mut(target).unwrap();
        if d < 1.3 {
            e.progress += 1;
            e.lost = 0;
        } else {
            e.lost += 1;
        }
        if e.lost > 40 {
            w.police.arrests.remove(target);
            continue;
        }
        if e.progress >= need {
            // Restrained is arrested; walking them to the van is logistics.
            w.police.arrests.remove(target);
            w.people.pose[ti][pose::GAIT] = Gait::Restrained as u8;
            w.people.stat[ti][stat::FLAGS] |= flag::DOWN;
            w.people.stat[ti][stat::FLAGS] &= !flag::ASLEEP;
            w.police.escorting.insert(a.officer, target);
            w.people.stat[oi][stat::ACTION] = Action::Escort as u8;
            w.outcome.arrests += 1;
            if w.people.stat[ti][stat::ARCH] == Archetype::Organizer as u8 {
                let p = w.people.pos(ti);
                w.outcome
                    .notable(w.tick, NotableKind::OrganizerArrested, p, target);
            }
            continue;
        }
        k += 1;
    }
    // Escorts: the arrestee follows; delivered at the van or home.
    let mut k = 0;
    while k < w.police.escorting.len() {
        let (officer, target) = w.police.escorting.entries()[k];
        let (Some(oi), Some(ti)) = (w.index_of(officer), w.index_of(target)) else {
            if let Some(ti) = w.index_of(target) {
                release(w, ti);
            }
            w.police.escorting.remove(officer);
            continue;
        };
        let op = w.people.pos(oi);
        let back = Vec2::from_heading(w.people.pose[oi][pose::HEADING]) * -0.7;
        let tp = op + back;
        if w.map.walkable_at(tp) {
            w.people.set_pos(ti, tp);
        }
        w.people.set_vel(ti, w.people.vel(oi));
        let info = w.police.officers[officer as usize];
        let sq = w.police.squads[info.squad as usize];
        let dest = if sq.van != NO_SQUAD {
            w.police.vehicles[sq.van as usize].pos
        } else {
            sq.home
        };
        if (op - dest).len() < 6.0 {
            if sq.van != NO_SQUAD {
                w.police.vehicles[sq.van as usize].cargo += 1;
            }
            w.remove(ti);
            w.police.escorting.remove(officer);
            w.people.stat[oi][stat::ACTION] = Action::Formation as u8;
            w.people.stat[oi][stat::FLAGS] |= flag::URGENT;
            continue;
        }
        k += 1;
    }
}

fn release(w: &mut World, ti: usize) {
    w.people.pose[ti][pose::GAIT] = Gait::Stand as u8;
    w.people.stat[ti][stat::FLAGS] &= !flag::DOWN;
    w.people.stat[ti][stat::FLAGS] |= flag::URGENT;
}

/// Vans and water cannons.
fn vehicles(w: &mut World) {
    let dt = crate::setup::DT;
    for v in 0..w.police.vehicles.len() {
        let veh = w.police.vehicles[v];
        match veh.state {
            VehicleState::Driving => {
                if veh.path_i >= veh.path_len {
                    arrive(w, v);
                    continue;
                }
                let node = w.map.roads.nodes[veh.path[veh.path_i as usize] as usize];
                let to = node - veh.pos;
                let d = to.len();
                let dir = if d > 1e-3 { to * (1.0 / d) } else { veh.ax };
                let top = if veh.kind == VehicleKind::Van {
                    8.0
                } else {
                    5.0
                };
                // Slow for people, stop for barricades.
                let ahead = veh.pos + dir * (veh.half.x + 3.0);
                let crowd = w.fields.sample(Layer::Dens, ahead.x, ahead.y);
                let blocked = w.obst.near(ahead).any(|o| {
                    let ob = w.obbs.get(o as usize);
                    ob.is_some_and(|ob| ob.kind == 0 && ob.contains(ahead))
                });
                let speed = if blocked {
                    0.0
                } else if crowd > 3.0 {
                    0.3
                } else if crowd > 1.0 {
                    1.5
                } else {
                    top
                };
                let vm = &mut w.police.vehicles[v];
                vm.speed += (speed - vm.speed).clamp(-4.0 * dt, 2.5 * dt);
                vm.ax = dir;
                if blocked {
                    vm.blocked += 1;
                    if vm.blocked > 400 {
                        // Delayed by a barricade: get out here.
                        vm.path_i = vm.path_len;
                    }
                } else {
                    vm.blocked = 0;
                }
                let step = vm.speed * dt;
                if step >= d {
                    vm.pos = node;
                    vm.path_i += 1;
                } else {
                    vm.pos += dir * step;
                }
            }
            VehicleState::Spraying => spray(w, v),
            VehicleState::Parked => {}
        }
    }
}

fn arrive(w: &mut World, v: usize) {
    let veh = w.police.vehicles[v];
    w.police.vehicles[v].state = VehicleState::Parked;
    w.police.vehicles[v].speed = 0.0;
    if veh.squad != NO_SQUAD {
        let s = veh.squad as usize;
        let at = veh.pos - veh.ax * (veh.half.x + 1.5);
        deploy_at(w, s, at);
    }
    if veh.kind == VehicleKind::WaterCannon && w.police.escalation >= Escalation::GasAndWater {
        w.police.vehicles[v].state = VehicleState::Spraying;
        if let Some(c) = w.police.clusters.first() {
            w.police.vehicles[v].spray = c.centroid;
        }
    }
}

/// Water cannon (DESIGN §2.6): a cone-shaped impulse that knocks people down
/// and soaks them.
fn spray(w: &mut World, v: usize) {
    let veh = w.police.vehicles[v];
    if w.police.escalation < Escalation::GasAndWater {
        w.police.vehicles[v].state = VehicleState::Parked;
        return;
    }
    let range = w.tun.water_range;
    let dir = (veh.spray - veh.pos).normalize_or_zero();
    if dir == Vec2::ZERO {
        return;
    }
    let force = w.tun.water_force;
    let nozzle = veh.pos + dir * veh.half.x;
    let tick = w.tick;
    let seed = w.seed;
    let px = riot_core::lanes::flat(&w.people.pos_x);
    let py = riot_core::lanes::flat(&w.people.pos_y);
    let mut hits = [0u32; 256];
    let mut nh = 0usize;
    w.spatial.query_radius(
        px,
        py,
        nozzle.x + dir.x * range * 0.5,
        nozzle.y + dir.y * range * 0.5,
        range * 0.55,
        |j| {
            if nh < hits.len() {
                hits[nh] = j as u32;
                nh += 1;
            }
        },
    );
    let mut hit_any = false;
    for &j in &hits[..nh] {
        let j = j as usize;
        if Archetype::from_u8(w.people.stat[j][stat::ARCH]).is_officer() {
            continue;
        }
        let d = w.people.pos(j) - nozzle;
        let dist = d.len();
        if dist > range || dist < 0.5 || d.dot(dir) < dist * 0.96 {
            continue;
        }
        hit_any = true;
        let k = force * (1.0 - dist / range);
        let nv = w.people.vel(j) + d * (k / dist);
        w.people.set_vel(j, nv);
        let e = &mut w.people.emo[j];
        e[emo::FATIGUE] = e[emo::FATIGUE].saturating_add(4);
        e[emo::FEAR] = e[emo::FEAR].saturating_add(3);
        w.people.stat[j][stat::FLAGS] =
            (w.people.stat[j][stat::FLAGS] & !flag::ASLEEP) | flag::URGENT;
        let stable = w.people.stable_id[j];
        if unit(roll_key(seed, tick, stable, 0x3a7e, Stream::Hazard)) < 0.01 * k {
            w.people.pose[j][pose::GAIT] = Gait::Fallen as u8;
            w.people.stat[j][stat::FLAGS] |= flag::DOWN;
        }
    }
    if hit_any && tick.is_multiple_of(20) {
        w.outcome
            .police_act(tick, veh.pos, veh.spray, &w.fields, 0.5, u32::MAX);
    }
    if !hit_any && tick.is_multiple_of(40) {
        w.police.vehicles[v].state = VehicleState::Parked;
    }
}
