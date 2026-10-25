//! The world (DESIGN §4.3): all simulation state, and the bookkeeping that
//! only touches state: building it from a setup, spawning, removing and
//! reordering people, flow targets and the state hash. The tick that runs
//! the phases over it is `riot_sim::World::tick`.

use riot_core::lanes::{self, CHUNK_AGENTS};
use riot_core::math::narrow_u8;
use riot_core::rng::{Stream, roll, roll_key, signed_unit, unit};
use riot_core::{StateHasher, Vec2};

use riot_defs::archetype::{
    ARCHETYPE_PARAMS, ARCHETYPES, Archetype, ArchetypeParams, SKIN, Stance, max_radius,
};
use riot_defs::event::Event;
use riot_defs::mapdef::{MapDef, PropKind};
use riot_defs::setup::{CommsMode, RiotSetup, Supplies, SupplyKind, TICK_HZ, minutes};
use riot_defs::state::{Action, Item, belief, flag, health, intent, stat};
use riot_defs::targets::{Objective, OfficerTarget, SupplyDrop};
use riot_defs::tunables::Tunables;
use riot_fields::{CL, Fields};
use riot_flow::{FlowSystem, GOAL_EXITS, GOAL_MEDIC, GOAL_WATER, MAX_TARGETS, OBJECTIVE_GOALS};
use riot_map::{Map, MapError};
use riot_people::{Cold, People, Spawn};
use riot_snapshot::{Overlay, Snapshot, SnapshotSizes, appearance_for};
use riot_spatial::Spatial;
use riot_spatial::obstacles::{Obb, ObstacleGrid};

use crate::commander::{MAX_CORDONS, MAX_TRACKS};
use crate::hazards::{Hazards, Metro};
use crate::messages::Messages;
use crate::minds::Minds;
use crate::outcome::OutcomeAccum;
use crate::police::Police;

pub const MAX_OBJECTIVES: usize = OBJECTIVE_GOALS;
/// Stable ids kept free for `DebugCommand::Spawn`.
/// Most police vehicles a riot can field, as obstacles.
pub const MAX_VEHICLE_OBSTACLES: usize = 256;

pub const DEBUG_RESERVE: usize = 1024;
pub const NO_SLOT: u32 = u32::MAX;

/// A prop on the map (DESIGN §5): collides like a wall, but can be pushed,
/// built, burned or destroyed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Prop {
    pub kind: PropKind,
    pub pos: Vec2,
    pub half: Vec2,
    pub ax: Vec2,
    pub health: u8,
    /// 1 = burning, 2 = under construction, 4 = destroyed, 8 = overturned,
    /// 16 = taken away.
    pub flags: u8,
    /// Construction progress (0..=255).
    pub build: u8,
}

#[derive(Clone, Copy, Debug)]
struct SpawnRec {
    tick: u32,
    entry: u16,
    archetype: u8,
    stable: u32,
}

/// What the snapshot captures. Not simulation state: it never changes results.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SnapshotConfig {
    pub enabled: bool,
    /// Capture field layers every this many ticks (0 = never).
    pub fields_every: u32,
    pub overlay: Overlay,
}

impl Default for SnapshotConfig {
    fn default() -> Self {
        SnapshotConfig {
            enabled: true,
            fields_every: 4,
            overlay: Overlay::None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SetupError {
    Map(MapError),
    UnknownEntry(String),
    UnknownDepot(String),
    UnknownArea(String),
    TooManyPeople(usize),
    NoEntries,
}

impl std::fmt::Display for SetupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SetupError::Map(e) => write!(f, "map: {e}"),
            SetupError::UnknownEntry(n) => write!(f, "unknown entry `{n}`"),
            SetupError::UnknownDepot(n) => write!(f, "unknown depot or staging point `{n}`"),
            SetupError::UnknownArea(n) => write!(f, "unknown area `{n}`"),
            SetupError::TooManyPeople(n) => {
                write!(f, "{n} people is more than the 65,000 one riot can hold")
            }
            SetupError::NoEntries => write!(f, "the setup has arrivals but the map has no entries"),
        }
    }
}

impl std::error::Error for SetupError {}

impl From<MapError> for SetupError {
    fn from(e: MapError) -> Self {
        SetupError::Map(e)
    }
}

/// Copies of per-agent arrays some phases need from before they run.
pub struct Scratch {
    pub vx: Vec<f32>,
    pub vy: Vec<f32>,
}

pub struct World {
    pub tick: u32,
    pub seed: u64,
    pub setup: RiotSetup,
    pub tun: Tunables,
    pub arch: [ArchetypeParams; ARCHETYPES],
    pub map: Map,
    pub people: People,
    back: People,
    /// Cold data by stable id.
    pub cold: Vec<Cold>,
    /// Dense index of each stable id, or `NO_SLOT`.
    pub slot_of: Vec<u32>,
    pub spatial: Spatial,
    pub fields: Fields,
    pub flow: FlowSystem,
    /// Props as obstacles, rebuilt only when a prop changes.
    pub obst: ObstacleGrid,
    pub obbs: Vec<Obb>,
    /// A prop was built, grew or was destroyed since `obst` was built.
    pub props_dirty: bool,
    /// Police vehicles as obstacles, rebuilt every tick (they move).
    pub veh_obst: ObstacleGrid,
    pub veh_obbs: Vec<Obb>,
    pub props: Vec<Prop>,
    /// Per-chunk event buffers (DESIGN §11.3).
    pub events: Vec<Vec<Event>>,
    pub objectives: [Objective; MAX_OBJECTIVES],
    pub drops: Vec<SupplyDrop>,
    /// The stance the player attaches to broadcasts.
    pub stance: Stance,
    pub supplies_left: Supplies,
    pub msgs: Messages,
    pub minds: Minds,
    pub police: Police,
    pub hazards: Hazards,
    pub officer_targets: Vec<OfficerTarget>,
    pub outcome: OutcomeAccum,
    pub snapshot: Snapshot,
    pub snap_cfg: SnapshotConfig,
    pub scratch: Scratch,
    /// Copy of the stat bytes from before think (scratch).
    pub stat_scratch: Vec<[u8; 4]>,
    schedule: Vec<SpawnRec>,
    next_spawn: usize,
    /// One past the highest stable id ever on the map: the snapshot's length.
    pub issued: u32,
    /// Stable ids planned by the setup (officers, organizers, crowd).
    pub planned: u32,
    /// Next stable id for debug spawns, from a reserve after `planned`.
    pub debug_next: u32,
    /// Stable ids that can exist: `planned` plus the debug reserve.
    pub max_people: u32,
    pub internet: bool,
    /// Entries the state has closed (metro stations).
    pub entry_closed: Vec<bool>,
    pub metro: Metro,
    /// Carried person's stable id → carrier's stable id (a rare state).
    pub carried: riot_core::SparseSet<u32>,
}

impl World {
    pub fn new(setup: &RiotSetup, mapdef: &MapDef, tun: Tunables) -> Result<World, SetupError> {
        let arch = ARCHETYPE_PARAMS;
        let rmax = max_radius(&arch);
        let map = Map::build(mapdef, rmax + 0.15)?;
        let seed = setup.seed;

        // --- who will be in this riot, by stable id: officers, organizers, crowd
        let officers: usize = setup
            .police
            .squads
            .iter()
            .map(|s| s.officers.clamp(1, 16) as usize)
            .sum();
        let organizers = setup.organizers as usize;
        let mut crowd = [0u32; ARCHETYPES];
        for a in Archetype::ALL {
            if a.is_crowd() {
                crowd[a as usize] = setup.crowd[a as usize];
            }
        }
        let crowd_total: usize = crowd.iter().map(|&c| c as usize).sum();
        let planned = officers + organizers + crowd_total;
        if planned + DEBUG_RESERVE > 65_000 {
            return Err(SetupError::TooManyPeople(planned));
        }
        let max_people = planned + DEBUG_RESERVE;
        let cap_people = max_people.max(1) + 64;

        // --- arrival schedule (DESIGN §2.8): turnout is a curve, not a number
        let mut arrivals: Vec<(u16, u32, u32, f32, [f32; ARCHETYPES])> = Vec::new();
        for a in &setup.arrivals {
            let e = map
                .entry(&a.entry)
                .ok_or_else(|| SetupError::UnknownEntry(a.entry.clone()))?;
            arrivals.push((
                e as u16,
                minutes(a.from_min),
                minutes(a.to_min.max(a.from_min)),
                a.share.max(0.0),
                a.bias,
            ));
        }
        if arrivals.is_empty() && crowd_total + organizers > 0 {
            if map.entries.is_empty() {
                return Err(SetupError::NoEntries);
            }
            for e in 0..map.entries.len() {
                arrivals.push((e as u16, 0, minutes(2.0), 1.0, [1.0; ARCHETYPES]));
            }
        }
        let mut recs: Vec<SpawnRec> = Vec::new();
        for (ai, &count) in crowd.iter().enumerate() {
            if count == 0 {
                continue;
            }
            let weights: Vec<f32> = arrivals.iter().map(|w| w.3 * w.4[ai].max(0.0)).collect();
            let per = largest_remainder(count, &weights);
            for (k, &n) in per.iter().enumerate() {
                let (entry, t0, t1, _, _) = arrivals[k];
                for j in 0..n {
                    let u = unit(roll_key(
                        seed,
                        0,
                        ai as u32,
                        (k as u32) << 20 | j,
                        Stream::Spawn,
                    ));
                    let t = t0 + (((j as f32 + u) / n as f32) * (t1 - t0) as f32) as u32;
                    recs.push(SpawnRec {
                        tick: t,
                        entry,
                        archetype: ai as u8,
                        stable: 0,
                    });
                }
            }
        }
        // Organizers arrive with the first wave, spread over the entries used.
        for j in 0..organizers {
            let (entry, t0, _, _, _) = arrivals[j % arrivals.len().max(1)];
            recs.push(SpawnRec {
                tick: t0 + j as u32 % 40,
                entry,
                archetype: Archetype::Organizer as u8,
                stable: 0,
            });
        }
        recs.sort_by_key(|r| (r.tick, r.entry, r.archetype));
        let first_civ = officers as u32;
        for (k, r) in recs.iter_mut().enumerate() {
            r.stable = first_civ + k as u32;
        }

        let people = People::new(cap_people);
        let cap = people.cap;
        let chunks = cap / CHUNK_AGENTS;
        let spatial = Spatial::new(cap, map.size, rmax, SKIN);
        let fields = Fields::new(map.size, setup.limits.field_chunks as usize);
        let flow = FlowSystem::new(
            &map,
            setup.limits.flow_tiles as usize,
            setup.limits.flow_pages as usize,
            chunks,
        );
        let props: Vec<Prop> = map
            .props
            .iter()
            .take(setup.limits.props as usize)
            .map(|d| Prop {
                kind: d.kind,
                pos: Vec2::new(d.pos[0], d.pos[1]),
                half: Vec2::new(d.half[0].max(0.05), d.half[1].max(0.05)),
                ax: Vec2::new(libm::cosf(d.angle), libm::sinf(d.angle)),
                health: 255,
                flags: 0,
                build: 255,
            })
            .collect();
        let max_obst = setup.limits.props as usize + 256;
        let mut w = World {
            tick: 0,
            seed,
            setup: setup.clone(),
            tun,
            arch,
            back: people.clone(),
            people,
            cold: vec![
                Cold {
                    arrived: u32::MAX,
                    left: u32::MAX,
                    ..Default::default()
                };
                max_people.max(1)
            ],
            slot_of: vec![NO_SLOT; max_people.max(1)],
            spatial,
            fields,
            flow,
            obst: ObstacleGrid::new(map.size, max_obst),
            obbs: Vec::with_capacity(max_obst),
            props_dirty: true,
            veh_obst: ObstacleGrid::new(map.size, MAX_VEHICLE_OBSTACLES),
            veh_obbs: Vec::with_capacity(MAX_VEHICLE_OBSTACLES),
            props: Vec::with_capacity(setup.limits.props as usize),
            events: (0..chunks).map(|_| Vec::with_capacity(4096)).collect(),
            objectives: [Objective::default(); MAX_OBJECTIVES],
            drops: Vec::with_capacity(256),
            stance: Stance::Peaceful,
            supplies_left: setup.supplies,
            msgs: Messages::new(),
            minds: Minds::new(cap, &tun),
            police: Police::new(setup, &map, cap, max_people)?,
            hazards: Hazards::new(setup),
            officer_targets: vec![OfficerTarget::default(); officers.max(1)],
            outcome: OutcomeAccum::new(setup),
            snapshot: Snapshot::default(),
            snap_cfg: SnapshotConfig::default(),
            scratch: Scratch {
                vx: vec![0.0; cap],
                vy: vec![0.0; cap],
            },
            stat_scratch: vec![[0; 4]; cap],
            schedule: recs,
            next_spawn: 0,
            issued: 0,
            planned: planned as u32,
            debug_next: planned as u32,
            max_people: max_people as u32,
            internet: setup.comms.mode != CommsMode::Shutdown,
            entry_closed: vec![false; map.entries.len()],
            metro: Metro::new(cap),
            carried: riot_core::SparseSet::new(max_people.max(1), 1024),
            map,
        };
        w.props.extend_from_slice(&props);

        // --- objectives and goals
        for (k, o) in setup.objectives.iter().enumerate().take(MAX_OBJECTIVES) {
            let (pos, radius) = match &o.area {
                Some(name) => {
                    let a = w
                        .map
                        .area(name)
                        .ok_or_else(|| SetupError::UnknownArea(name.clone()))?;
                    (a.pos, if o.radius > 0.0 { o.radius } else { a.radius })
                }
                None => (Vec2::new(o.pos[0], o.pos[1]), o.radius.max(3.0)),
            };
            let pos = w.map.nearest_open(pos, 64.0).unwrap_or(pos);
            w.objectives[k] = Objective {
                active: true,
                kind: o.kind,
                pos,
                radius,
                weight: o.weight.max(0.0),
                stance: o.stance,
                setup_index: k as u8,
                ..Default::default()
            };
            w.retarget_objective(k as u8);
            if o.announced {
                w.msgs.announce(k as u8, o.stance, o.weight.max(0.0), 0);
            }
        }
        w.retarget_exits();
        crate::police::spawn_initial(&mut w);
        w.outcome.start(&w.objectives);
        let sizes = SnapshotSizes {
            people: w.max_people as usize,
            vehicles: w.police.vehicles.capacity(),
            projectiles: w.hazards.projectiles.capacity(),
            props: w.props.capacity(),
            objectives: MAX_OBJECTIVES,
            drops: w.drops.capacity(),
            squads: w.police.squads.capacity(),
            tracks: MAX_TRACKS,
            cordons: MAX_CORDONS,
            field_cells: w.fields.pool * CL,
            field_chunks: w.fields.pool,
        };
        w.snapshot.reserve(&sizes);
        Ok(w)
    }

    pub fn retarget_supply(&mut self, kind: SupplyKind) {
        let goal = if kind == SupplyKind::Water {
            GOAL_WATER
        } else {
            GOAL_MEDIC
        };
        let mut targets = [Vec2::ZERO; MAX_TARGETS];
        let mut n = 0;
        for d in &self.drops {
            if d.kind == kind && d.amount > 0 && n < targets.len() {
                targets[n] = d.pos;
                n += 1;
            }
        }
        self.flow.set_targets(goal, &targets[..n], 3.0, self.tick);
    }

    pub fn retarget_objective(&mut self, slot: u8) {
        let o = self.objectives[slot as usize];
        self.flow
            .set_targets(slot, &[o.pos], (o.radius * 0.5).max(2.0), self.tick);
    }

    pub fn retarget_exits(&mut self) {
        let mut targets = [Vec2::ZERO; MAX_TARGETS];
        let mut n = 0;
        for (k, e) in self.map.entries.iter().enumerate() {
            if !self.entry_closed[k] && n < targets.len() {
                targets[n] = e.pos;
                n += 1;
            }
        }
        self.flow
            .set_targets(GOAL_EXITS, &targets[..n], self.tun.exit_radius, self.tick);
    }

    /// Phase 1: put on the map everyone the arrival schedule brings by this
    /// tick (DESIGN §2.8).
    pub fn arrive_scheduled(&mut self) {
        while self.next_spawn < self.schedule.len()
            && self.schedule[self.next_spawn].tick <= self.tick
        {
            let r = self.schedule[self.next_spawn];
            self.next_spawn += 1;
            if self
                .entry_closed
                .get(r.entry as usize)
                .copied()
                .unwrap_or(false)
            {
                // Station closed: they try the nearest open entry instead.
                if let Some(alt) = (0..self.map.entries.len()).find(|&k| !self.entry_closed[k]) {
                    self.spawn_at_entry(r.stable, alt as u16, r.archetype);
                }
                continue;
            }
            self.spawn_at_entry(r.stable, r.entry, r.archetype);
        }
    }

    fn spawn_at_entry(&mut self, stable: u32, entry: u16, archetype: u8) {
        let e = &self.map.entries[entry as usize];
        let r = roll(self.seed, self.tick, stable, Stream::Spawn);
        let off = Vec2::from_heading((r >> 8) as u8) * (e.radius * unit(r).sqrt());
        let p = e.pos + off;
        let p = if self.map.walkable_at(p) { p } else { e.pos };
        self.spawn_person(stable, p, archetype, entry);
    }

    /// Put a civilian on the map.
    pub fn spawn_person(&mut self, stable: u32, pos: Vec2, archetype: u8, entry: u16) {
        if self.people.len >= self.people.cap {
            self.compact();
            if self.people.len >= self.people.cap {
                self.outcome.spawn_overflow += 1;
                return;
            }
        }
        let a = Archetype::from_u8(archetype);
        let p = &self.arch[archetype as usize];
        let r = roll(self.seed, self.tick, stable, Stream::Appearance);
        let jitter = |k: u32| signed_unit(roll_key(self.seed, 0, stable, k, Stream::Spawn)) * 0.08;
        let emo = [
            narrow_u8(p.base_fear + jitter(1)),
            narrow_u8(p.base_anger + jitter(2)),
            narrow_u8(p.base_morale + jitter(3)),
            narrow_u8(0.05 + jitter(4).abs()),
        ];
        let mut flags = 0u8;
        if unit(roll_key(self.seed, 0, stable, 5, Stream::Spawn)) < p.phone {
            flags |= flag::PHONE;
        }
        // What they know when they arrive: the announced plan.
        let (intent_v, belief_v) = self.msgs.arrival_belief(stable, a, self.seed, &self.arch);
        let action = match a {
            Archetype::Journalist => Action::Film,
            Archetype::Bystander => Action::Idle,
            _ if intent::objective(intent_v) == intent::NO_GOAL => Action::Idle,
            _ => Action::March,
        };
        let item = initial_item(a, r);
        if item != Item::None {
            flags |= flag::ITEM;
        }
        let i = self.people.push(Spawn {
            pos,
            heading: (r >> 16) as u8,
            emo,
            health: health::FULL,
            action: action as u8,
            archetype,
            flags,
            intent: intent_v,
            belief: belief_v,
            stable_id: stable,
        });
        self.slot_of[stable as usize] = i as u32;
        self.issued = self.issued.max(stable + 1);
        let c = &mut self.cold[stable as usize];
        c.appearance = appearance_for(a, r, item).0;
        c.home = entry;
        c.item = item as u8;
        c.archetype = archetype;
        c.arrived = self.tick;
        c.left = u32::MAX;
        self.outcome.arrived(a);
        if a == Archetype::Organizer {
            let list = &mut self.police.organizers;
            if !list.contains(&stable) && list.len() < list.capacity() {
                list.push(stable);
            }
        }
    }

    /// Take someone off the map. They are compacted out at the next reorder.
    pub fn remove(&mut self, i: usize) {
        let st = &mut self.people.stat[i];
        if st[stat::FLAGS] & flag::DEAD != 0 {
            return;
        }
        st[stat::FLAGS] = flag::DEAD;
        let stable = self.people.stable_id[i];
        self.slot_of[stable as usize] = NO_SLOT;
        self.cold[stable as usize].left = self.tick;
        self.people.set_vel(i, Vec2::ZERO);
    }

    /// Permute every agent array into field-cell (Morton) order, dropping the
    /// dead (DESIGN §6.4, §7.4).
    pub fn compact(&mut self) {
        self.spatial.compute_field_keys(&self.people);
        let live = self.spatial.sort_field(self.people.len);
        let order = &self.spatial.fsort.order[..live];
        self.back.permute_from(&self.people, order);
        std::mem::swap(&mut self.people, &mut self.back);
        for (k, &s) in self.people.stable_id[..live].iter().enumerate() {
            self.slot_of[s as usize] = k as u32;
        }
        self.minds.on_reorder();
    }

    /// 64-bit hash of all simulation state (DESIGN §19).
    pub fn state_hash(&self) -> u64 {
        let mut h = StateHasher::new();
        let p = &self.people;
        let n = p.len;
        h.write_u32(self.tick);
        h.write_u64(n as u64);
        h.write_pod(&lanes::flat(&p.pos_x)[..n]);
        h.write_pod(&lanes::flat(&p.pos_y)[..n]);
        h.write_pod(&lanes::flat(&p.vel_x)[..n]);
        h.write_pod(&lanes::flat(&p.vel_y)[..n]);
        h.write_pod(&p.pose[..n]);
        h.write_pod(&p.emo[..n]);
        h.write_pod(&p.stat[..n]);
        h.write_pod(&p.intent[..n]);
        h.write_pod(&p.belief[..n]);
        h.write_pod(&p.stable_id[..n]);
        h.write_pod(&p.aux[..n]);
        h.write_pod(&self.fields.data);
        h.write_pod(&self.fields.msg);
        h.write_pod(&self.fields.table);
        for g in &self.flow.goals {
            h.write_u32(g.version);
            h.write_u32(g.generation);
        }
        h.write_pod(&self.flow.dirs);
        for pr in &self.props {
            h.write_f32(pr.pos.x);
            h.write_f32(pr.pos.y);
            h.write_u32(pr.health as u32 | (pr.flags as u32) << 8 | (pr.build as u32) << 16);
        }
        self.police.hash_into(&mut h);
        self.hazards.hash_into(&mut h);
        self.msgs.hash_into(&mut h);
        self.outcome.hash_into(&mut h);
        h.finish()
    }

    pub fn finished(&self) -> bool {
        self.tick >= self.setup.duration
    }

    /// People currently on the map.
    pub fn present(&self) -> u32 {
        self.spatial.live
    }

    /// Minute of the day.
    pub fn clock_minute(&self) -> f32 {
        self.setup.start_minute + self.tick as f32 / (60.0 * TICK_HZ as f32)
    }

    /// Dense index of a stable id, if present.
    #[inline]
    pub fn index_of(&self, stable: u32) -> Option<usize> {
        self.slot_of
            .get(stable as usize)
            .copied()
            .filter(|&s| s != NO_SLOT)
            .map(|s| s as usize)
    }
}

/// Largest-remainder split of `total` by `weights`.
fn largest_remainder(total: u32, weights: &[f32]) -> Vec<u32> {
    let sum: f64 = weights.iter().map(|&w| w as f64).sum();
    if sum <= 0.0 || weights.is_empty() {
        let mut v = vec![0; weights.len()];
        if let Some(f) = v.first_mut() {
            *f = total;
        }
        return v;
    }
    let exact: Vec<f64> = weights
        .iter()
        .map(|&w| w as f64 / sum * total as f64)
        .collect();
    let mut out: Vec<u32> = exact.iter().map(|&e| e.floor() as u32).collect();
    let mut rest = total - out.iter().sum::<u32>();
    let mut idx: Vec<usize> = (0..weights.len()).collect();
    idx.sort_by(|&a, &b| {
        (exact[b] - exact[b].floor())
            .total_cmp(&(exact[a] - exact[a].floor()))
            .then(a.cmp(&b))
    });
    for &k in &idx {
        if rest == 0 {
            break;
        }
        out[k] += 1;
        rest -= 1;
    }
    out
}

fn initial_item(a: Archetype, r: u64) -> Item {
    let u = unit(r.rotate_left(17));
    match a {
        Archetype::Journalist => Item::Camera,
        Archetype::Organizer => Item::Megaphone,
        Archetype::Radical if u < 0.25 => Item::Bottle,
        Archetype::Radical if u < 0.35 => Item::Flag,
        Archetype::Provocateur if u < 0.6 => Item::Bottle,
        Archetype::Provocateur => Item::Molotov,
        Archetype::Student | Archetype::Worker | Archetype::Elder if u < 0.3 => Item::Sign,
        Archetype::Student | Archetype::Worker if u < 0.36 => Item::Flag,
        Archetype::Officer => Item::Baton,
        Archetype::RiotOfficer => Item::Shield,
        _ => Item::None,
    }
}

/// Is the person at dense index `i` an officer?
#[inline]
pub fn is_officer(p: &People, i: usize) -> bool {
    Archetype::from_u8(p.stat[i][stat::ARCH]).is_officer()
}

/// Belief helper: does this person know a plan?
#[inline]
pub fn knows_plan(p: &People, i: usize) -> bool {
    belief::id(p.belief[i]) != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remainder_split() {
        assert_eq!(largest_remainder(10, &[1.0, 1.0, 1.0]), vec![4, 3, 3]);
        assert_eq!(largest_remainder(7, &[0.0, 2.0]), vec![0, 7]);
        assert_eq!(largest_remainder(5, &[]), Vec::<u32>::new());
    }
}
