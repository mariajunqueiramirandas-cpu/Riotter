//! The world and the tick (DESIGN §4.3).

use riot_core::lanes::{self, CHUNK_AGENTS};
use riot_core::math::narrow_u8;
use riot_core::rng::{Stream, below, roll, roll_key, signed_unit, unit};
use riot_core::{StateHasher, Vec2};

use crate::archetype::{
    ARCHETYPE_PARAMS, ARCHETYPES, Archetype, ArchetypeParams, SKIN, Stance, max_radius,
};
use crate::command::{Command, CommandKind, DebugCommand};
use crate::event::Event;
use crate::fields::{CC, Fields, Layer};
use crate::flow::{FlowSystem, GOAL_EXITS, GOAL_MEDIC, GOAL_WATER, OBJECTIVE_GOALS};
use crate::hazards::Hazards;
use crate::map::{Map, MapError};
use crate::mapdef::{MapDef, PropKind};
use crate::messages::Messages;
use crate::outcome::OutcomeAccum;
use crate::people::{Cold, People, Spawn};
use crate::physics::{BodyCtx, Obb, ObstacleGrid, PhysScratch};
use crate::police::Police;
use crate::setup::{ObjectiveKind, RiotOutcome, RiotSetup, SupplyKind, minutes};
use crate::snapshot::{Appearance, Overlay, Snapshot};
use crate::spatial::Spatial;
use crate::state::{Action, Item, aux, belief, flag, health, intent};
use crate::steer::{OfficerTarget, SteerCtx};
use crate::think::Minds;
use crate::tunables::Tunables;

pub const MAX_OBJECTIVES: usize = OBJECTIVE_GOALS;
/// Stable ids kept free for `DebugCommand::Spawn`.
/// Most police vehicles a riot can field, as obstacles.
pub const MAX_VEHICLE_OBSTACLES: usize = 256;

pub const DEBUG_RESERVE: usize = 1024;
pub const NO_SLOT: u32 = u32::MAX;

/// The phases of a tick, for profiling (DESIGN §3.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Phase {
    Commands = 0,
    Spatial = 1,
    Fields = 2,
    Think = 3,
    Messages = 4,
    Steer = 5,
    Bodies = 6,
    Interact = 7,
    Director = 8,
    Flow = 9,
    Account = 10,
}

pub const PHASES: usize = 11;

impl Phase {
    pub const ALL: [Phase; PHASES] = [
        Phase::Commands,
        Phase::Spatial,
        Phase::Fields,
        Phase::Think,
        Phase::Messages,
        Phase::Steer,
        Phase::Bodies,
        Phase::Interact,
        Phase::Director,
        Phase::Flow,
        Phase::Account,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Phase::Commands => "commands",
            Phase::Spatial => "spatial",
            Phase::Fields => "fields",
            Phase::Think => "think",
            Phase::Messages => "messages",
            Phase::Steer => "steer",
            Phase::Bodies => "bodies",
            Phase::Interact => "interact",
            Phase::Director => "director",
            Phase::Flow => "flow",
            Phase::Account => "account",
        }
    }
    /// Budget at 10,000 agents in ms (DESIGN §3.1).
    pub fn budget_ms(self) -> f32 {
        match self {
            Phase::Commands => 0.0,
            Phase::Spatial => 0.85,
            Phase::Fields => 0.30,
            Phase::Think => 0.25,
            Phase::Messages => 0.10,
            Phase::Steer => 0.25,
            Phase::Bodies => 0.45,
            Phase::Interact => 0.25,
            Phase::Director => 0.10,
            Phase::Flow => 0.30,
            Phase::Account => 0.15,
        }
    }
}

/// Phase timing hooks. The timer lives outside the simulation logic, so it
/// cannot affect results (DESIGN §19).
pub trait Profiler {
    fn begin(&mut self, _phase: Phase) {}
    fn end(&mut self, _phase: Phase) {}
}

pub struct NoProfiler;
impl Profiler for NoProfiler {}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Objective {
    pub active: bool,
    pub kind: ObjectiveKind,
    pub pos: Vec2,
    pub radius: f32,
    pub weight: f32,
    pub stance: Stance,
    /// Ticks held by protesters.
    pub held_ticks: u32,
    /// Ticks this objective existed.
    pub live_ticks: u32,
    pub held_now: bool,
    /// Index in `RiotSetup::objectives`, or `u8::MAX` for player-placed ones.
    pub setup_index: u8,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SupplyDrop {
    pub pos: Vec2,
    pub kind: SupplyKind,
    pub amount: u16,
}

/// A prop on the map (DESIGN §5): collides like a wall, but can be pushed,
/// built, burned or destroyed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Prop {
    pub kind: PropKind,
    pub pos: Vec2,
    pub half: Vec2,
    pub ax: Vec2,
    pub health: u8,
    /// 1 = burning, 2 = under construction, 4 = destroyed, 8 = overturned.
    pub flags: u8,
    /// Construction progress (0..=255).
    pub build: u8,
}

pub mod prop_flag {
    pub const BURNING: u8 = 1;
    pub const BUILDING: u8 = 2;
    pub const DESTROYED: u8 = 4;
    pub const OVERTURNED: u8 = 8;
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
    pub phys: PhysScratch,
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
    pub supplies_left: crate::setup::Supplies,
    pub msgs: Messages,
    pub minds: Minds,
    pub police: Police,
    pub hazards: Hazards,
    pub officer_targets: Vec<OfficerTarget>,
    pub outcome: OutcomeAccum,
    pub snapshot: Snapshot,
    pub snap_cfg: SnapshotConfig,
    scratch: Scratch,
    /// Copy of the stat bytes from before think (scratch).
    pub(crate) stat_scratch: Vec<[u8; 4]>,
    schedule: Vec<SpawnRec>,
    next_spawn: usize,
    /// One past the highest stable id ever on the map: the snapshot's length.
    pub issued: u32,
    /// Stable ids planned by the setup (officers, organizers, crowd).
    pub planned: u32,
    /// Next stable id for debug spawns, from a reserve after `planned`.
    debug_next: u32,
    /// Stable ids that can exist: `planned` plus the debug reserve.
    pub max_people: u32,
    pub internet: bool,
    /// Entries the state has closed (metro stations).
    pub entry_closed: Vec<bool>,
    pub metro: crate::hazards::Metro,
    /// Carried person's stable id → carrier's stable id (a rare state).
    pub carried: riot_core::SparseSet<u32>,
}

/// Position of a 24-bit field-cell target (`aux` targets that are places).
#[inline]
pub fn field_cell_pos(f: &Fields, cell: u32) -> Vec2 {
    let dim = f.geom.dim;
    let (cx, cy) = (cell % dim, cell / dim);
    let (x, y) = f.geom.centre(cx, cy);
    Vec2::new(x, y)
}

/// 24-bit field-cell target for a position.
#[inline]
pub fn field_cell_of(f: &Fields, p: Vec2) -> u32 {
    let (cx, cy) = f.geom.cell_of(p.x, p.y);
    (cy * f.geom.dim + cx) & aux::NO_TARGET.wrapping_sub(1)
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
            phys: PhysScratch::new(cap),
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
            minds: Minds::new(cap),
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
            internet: setup.comms.mode != crate::setup::CommsMode::Shutdown,
            entry_closed: vec![false; map.entries.len()],
            metro: crate::hazards::Metro::new(cap),
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
        let sizes = crate::snapshot::SnapshotSizes {
            people: w.max_people as usize,
            vehicles: w.police.vehicles.capacity(),
            projectiles: w.hazards.projectiles.capacity(),
            props: w.props.capacity(),
            objectives: MAX_OBJECTIVES,
            drops: w.drops.capacity(),
            squads: w.police.squads.capacity(),
            field_cells: w.fields.pool * crate::fields::CL,
            field_chunks: w.fields.pool,
        };
        w.snapshot.reserve(&sizes);
        Ok(w)
    }

    /// Run one tick (DESIGN §4.3).
    pub fn tick(&mut self, cmds: &[Command]) {
        self.tick_profiled(cmds, &mut NoProfiler);
    }

    pub fn tick_profiled(&mut self, cmds: &[Command], prof: &mut impl Profiler) {
        prof.begin(Phase::Commands);
        self.apply_commands(cmds); //  1  validate + apply, in order
        self.arrivals();
        prof.end(Phase::Commands);

        prof.begin(Phase::Spatial);
        self.rebuild_spatial(); //  2  body hash + neighbour lists, field index; reorder every N ticks
        prof.end(Phase::Spatial);

        prof.begin(Phase::Fields);
        self.update_fields(); //  3  scatter → diffuse/decay → agents absorb (contagion)
        prof.end(Phase::Fields);

        prof.begin(Phase::Think);
        crate::think::think(self); //  4  1/K of agents re-score their actions
        prof.end(Phase::Think);

        prof.begin(Phase::Messages);
        crate::messages::spread(self); //  5  rumours move
        prof.end(Phase::Messages);

        prof.begin(Phase::Steer);
        self.steer(); //  6  desired velocity + anticipatory avoidance + facing
        prof.end(Phase::Steer);

        prof.begin(Phase::Bodies);
        self.move_bodies(); //  7  predict, contacts (Jacobi), walls, velocity, pressure, pose
        prof.end(Phase::Bodies);

        prof.begin(Phase::Interact);
        crate::interact::interact(self); //  8  emit events in parallel, resolve in order
        prof.end(Phase::Interact);

        prof.begin(Phase::Director);
        crate::police::director(self); //  9  every 20 ticks; squads and vehicles every tick
        prof.end(Phase::Director);

        prof.begin(Phase::Flow);
        self.flow
            .update(&self.map, &self.fields, &self.tun, self.tick); // 10  swap in finished tiles, queue needed ones
        prof.end(Phase::Flow);

        prof.begin(Phase::Account);
        crate::outcome::account(self); // 11  outcome tallies
        crate::snapshot_write::write(self); //     render snapshot
        prof.end(Phase::Account);
        self.tick += 1;
    }

    // -----------------------------------------------------------------------
    // Phase 1: commands and arrivals

    fn apply_commands(&mut self, cmds: &[Command]) {
        for c in cmds {
            debug_assert!(
                c.tick == self.tick,
                "command for tick {} applied at {}",
                c.tick,
                self.tick
            );
            if c.tick != self.tick {
                continue;
            }
            match c.kind {
                CommandKind::PlaceObjective {
                    slot,
                    kind,
                    x,
                    y,
                    radius,
                } => {
                    if (slot as usize) < MAX_OBJECTIVES {
                        let p = Vec2::new(x, y);
                        let pos = self.map.nearest_open(p, 32.0).unwrap_or(p);
                        let o = &mut self.objectives[slot as usize];
                        let fresh = !o.active;
                        o.active = true;
                        o.kind = kind;
                        o.pos = pos;
                        o.radius = radius.clamp(3.0, 120.0);
                        if fresh {
                            o.weight = 1.0;
                            o.setup_index = u8::MAX;
                            o.held_ticks = 0;
                            o.live_ticks = 0;
                        }
                        self.retarget_objective(slot);
                    }
                }
                CommandKind::ClearObjective { slot } => {
                    if (slot as usize) < MAX_OBJECTIVES {
                        self.objectives[slot as usize].active = false;
                        self.flow.clear_goal(slot);
                    }
                }
                CommandKind::SetStance(s) => self.stance = s,
                CommandKind::Broadcast {
                    organizer,
                    objective,
                } => {
                    crate::messages::broadcast(self, organizer, objective);
                }
                CommandKind::DropSupplies { kind, x, y, amount } => {
                    self.drop_supplies(kind, Vec2::new(x, y), amount)
                }
                CommandKind::Chant { organizer, on } => {
                    crate::messages::set_chant(self, organizer, on)
                }
                CommandKind::SetTunable { id, value } => {
                    self.tun.set(id, value);
                }
                CommandKind::Debug(d) => self.debug_command(d),
            }
        }
    }

    fn debug_command(&mut self, d: DebugCommand) {
        match d {
            DebugCommand::Gas { x, y } => {
                self.hazards.add_gas(Vec2::new(x, y), self.tick, u32::MAX)
            }
            DebugCommand::Ignite { x, y } => self.hazards.ignite(
                Vec2::new(x, y),
                &mut self.fields,
                &self.map,
                &mut self.props,
            ),
            DebugCommand::Spawn {
                x,
                y,
                count,
                archetype,
                radius,
            } => {
                for k in 0..count as u32 {
                    if self.debug_next >= self.max_people || self.people.len >= self.people.cap {
                        self.extend_capacity_note();
                        break;
                    }
                    let r = roll_key(self.seed, self.tick, k, 0xdeb0, Stream::Spawn);
                    let off = Vec2::from_heading((r >> 8) as u8) * (radius * unit(r).sqrt());
                    let p = Vec2::new(x, y) + off;
                    let p = self.map.nearest_open(p, 8.0).unwrap_or(p);
                    let stable = self.debug_next;
                    self.debug_next += 1;
                    self.spawn_person(stable, p, archetype as u8, 0);
                }
            }
            DebugCommand::Threat { x, y, amount } => {
                self.fields
                    .stamp(Layer::Threat, Vec2::new(x, y), 6.0, amount, &self.map)
            }
            DebugCommand::Escalate { rung } => self
                .police
                .force_escalation(rung, self.setup.escalation_cap),
        }
    }

    fn extend_capacity_note(&mut self) {
        self.outcome.spawn_overflow += 1;
    }

    fn drop_supplies(&mut self, kind: SupplyKind, p: Vec2, amount: u16) {
        let left = self.supplies_left.get_mut(kind);
        let n = (amount as u32).min(*left) as u16;
        if n == 0 || self.drops.len() == self.drops.capacity() {
            return;
        }
        *left -= n as u32;
        *self.outcome.supplies_used.get_mut(kind) += n as u32;
        let pos = self.map.nearest_open(p, 16.0).unwrap_or(p);
        self.drops.push(SupplyDrop {
            pos,
            kind,
            amount: n,
        });
        match kind {
            SupplyKind::Water => self.retarget_supply(SupplyKind::Water),
            SupplyKind::Medic => self.retarget_supply(SupplyKind::Medic),
            _ => {}
        }
    }

    pub(crate) fn retarget_supply(&mut self, kind: SupplyKind) {
        let goal = if kind == SupplyKind::Water {
            GOAL_WATER
        } else {
            GOAL_MEDIC
        };
        let mut targets = [Vec2::ZERO; crate::flow::MAX_TARGETS];
        let mut n = 0;
        for d in &self.drops {
            if d.kind == kind && d.amount > 0 && n < targets.len() {
                targets[n] = d.pos;
                n += 1;
            }
        }
        self.flow.set_targets(goal, &targets[..n], 3.0, self.tick);
    }

    pub(crate) fn retarget_objective(&mut self, slot: u8) {
        let o = self.objectives[slot as usize];
        self.flow
            .set_targets(slot, &[o.pos], (o.radius * 0.5).max(2.0), self.tick);
    }

    pub(crate) fn retarget_exits(&mut self) {
        let mut targets = [Vec2::ZERO; crate::flow::MAX_TARGETS];
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

    fn arrivals(&mut self) {
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
        crate::hazards::metro_arrivals(self);
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
    pub(crate) fn spawn_person(&mut self, stable: u32, pos: Vec2, archetype: u8, entry: u16) {
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
    pub(crate) fn remove(&mut self, i: usize) {
        let st = &mut self.people.stat[i];
        if st[crate::state::stat::FLAGS] & flag::DEAD != 0 {
            return;
        }
        st[crate::state::stat::FLAGS] = flag::DEAD;
        let stable = self.people.stable_id[i];
        self.slot_of[stable as usize] = NO_SLOT;
        self.cold[stable as usize].left = self.tick;
        self.people.set_vel(i, Vec2::ZERO);
    }

    // -----------------------------------------------------------------------
    // Phase 2

    fn rebuild_spatial(&mut self) {
        let every = self.tun.reorder_interval.max(1.0) as u32;
        if self.tick.is_multiple_of(every) {
            self.compact();
        }
        self.spatial.rebuild(&self.people, &self.arch);
    }

    /// Permute every agent array into field-cell (Morton) order, dropping the
    /// dead (DESIGN §6.4, §7.4).
    fn compact(&mut self) {
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

    // -----------------------------------------------------------------------
    // Phase 3

    fn update_fields(&mut self) {
        let tick = self.tick;
        let t = self.tun;
        self.fields.activate_for_tick(&self.spatial, &self.map);
        crate::hazards::pin_emitters(self);
        // Multi-rate layers (DESIGN §8): emotions and threat every 4 ticks,
        // on alternate ticks so their costs do not stack.
        let threat_tick = tick.is_multiple_of(4);
        let emo_tick = tick % 4 == 2;
        self.fields.scatter(
            &self.spatial,
            &self.people,
            &self.tun,
            threat_tick,
            emo_tick,
        );
        crate::messages::stamp_chants(self);
        if tick.is_multiple_of(10) {
            crate::hazards::stamp_cameras(self);
        }
        if emo_tick {
            self.fields
                .blur(&[Layer::Fear, Layer::Anger, Layer::Weight], t.field_diffuse);
        }
        if threat_tick {
            let tb = crate::fields::blur_kernel(0.2);
            self.fields.filter(Layer::Threat, tb, tb, 1.0, false);
        }
        crate::hazards::update_fields(self);
        let ap = crate::fields::AbsorbParams {
            arch: &self.arch,
            tun: &self.tun,
            heat: self.setup.weather.heat,
            seed: self.seed,
            tick,
        };
        let h = crate::fields::absorb(&self.fields, &mut self.people, &self.spatial, &ap);
        self.outcome.injuries += h.injured;
        self.outcome.officers_injured += h.officers_injured;
        self.outcome.harm[crate::outcome::Harm::Gas as usize] += h.gas;
        self.outcome.harm[crate::outcome::Harm::Fire as usize] += h.fire;
        if tick.is_multiple_of(20) {
            self.fields.housekeeping(1e-3);
        }
    }

    // -----------------------------------------------------------------------
    // Phase 6

    fn steer(&mut self) {
        let span = self.people.chunks() * CHUNK_AGENTS;
        self.scratch.vx[..span].copy_from_slice(&lanes::flat(&self.people.vel_x)[..span]);
        self.scratch.vy[..span].copy_from_slice(&lanes::flat(&self.people.vel_y)[..span]);
        let mut records = std::mem::take(&mut self.flow.records);
        let cx = SteerCtx {
            map: &self.map,
            flow: &self.flow,
            fields: &self.fields,
            sp: &self.spatial,
            arch: &self.arch,
            tun: &self.tun,
            objectives: &self.objectives,
            drops: &self.drops,
            officer_targets: &self.officer_targets,
            slot_of: &self.slot_of,
            tick: self.tick,
            seed: self.seed,
        };
        crate::steer::steer(
            &mut self.people,
            &self.scratch.vx,
            &self.scratch.vy,
            &mut records,
            &cx,
        );
        self.flow.records = records;
    }

    // -----------------------------------------------------------------------
    // Phase 7

    fn move_bodies(&mut self) {
        // Props stand still: their grid follows the props, not the clock, so
        // a city full of parked cars costs nothing per tick.
        if self.props_dirty {
            self.obbs.clear();
            for (k, pr) in self.props.iter().enumerate() {
                if pr.flags & prop_flag::DESTROYED == 0 && self.obbs.len() < self.obbs.capacity() {
                    self.obbs.push(Obb {
                        c: pr.pos,
                        ax: pr.ax,
                        half: pr.half,
                        kind: 0,
                        id: k as u16,
                    });
                }
            }
            self.obst.rebuild(&self.obbs);
            self.props_dirty = false;
        }
        self.veh_obbs.clear();
        self.police.vehicle_obbs(&mut self.veh_obbs);
        self.veh_obst.rebuild(&self.veh_obbs);
        for e in &mut self.events {
            e.clear();
        }
        let cx = BodyCtx {
            map: &self.map,
            sp: &self.spatial,
            arch: &self.arch,
            tun: &self.tun,
            obbs: &self.obbs,
            grid: &self.obst,
            veh_obbs: &self.veh_obbs,
            veh_grid: &self.veh_obst,
        };
        crate::physics::move_bodies(&mut self.people, &mut self.phys, &mut self.events, &cx);
    }

    // -----------------------------------------------------------------------

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

    /// Simulate until `setup.duration` and return the outcome (auto-resolve,
    /// DESIGN §17).
    pub fn resolve(&mut self) -> RiotOutcome {
        while !self.finished() {
            self.tick(&[]);
        }
        self.outcome()
    }

    pub fn finished(&self) -> bool {
        self.tick >= self.setup.duration
    }

    pub fn outcome(&self) -> RiotOutcome {
        crate::outcome::outcome(self)
    }

    /// People currently on the map.
    pub fn present(&self) -> u32 {
        self.spatial.live
    }

    /// Minute of the day.
    pub fn clock_minute(&self) -> f32 {
        self.setup.start_minute + self.tick as f32 / (60.0 * crate::setup::TICK_HZ as f32)
    }

    /// Dense index of a stable id, if present.
    pub fn index_of(&self, stable: u32) -> Option<usize> {
        self.slot_of
            .get(stable as usize)
            .copied()
            .filter(|&s| s != NO_SLOT)
            .map(|s| s as usize)
    }

    /// Field chunk cells per side, for renderers.
    pub const FIELD_CHUNK_CELLS: usize = CC;
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

/// Appearance from identity and archetype (DESIGN §2.3).
pub fn appearance_for(a: Archetype, r: u64, item: Item) -> Appearance {
    let body = below(r, 4) as u8;
    let skin = below(r.rotate_left(7), 8) as u8;
    let clothes = match a {
        Archetype::Officer | Archetype::RiotOfficer => 60 + below(r.rotate_left(11), 2) as u8,
        Archetype::Journalist => 56 + below(r.rotate_left(11), 4) as u8,
        Archetype::Organizer => 52 + below(r.rotate_left(11), 4) as u8,
        Archetype::Radical | Archetype::Provocateur => 44 + below(r.rotate_left(11), 8) as u8,
        _ => below(r.rotate_left(11), 44) as u8,
    };
    let hair = below(r.rotate_left(23), 16) as u8;
    let hair_colour = below(r.rotate_left(29), 8) as u8;
    let accessory = match a {
        Archetype::Officer => 6,
        Archetype::RiotOfficer => 2,
        Archetype::Journalist => 7,
        Archetype::Radical | Archetype::Provocateur => {
            [3, 5, 1, 0][below(r.rotate_left(31), 4) as usize]
        }
        Archetype::Organizer => 6,
        _ => [0, 0, 0, 1, 4, 5][below(r.rotate_left(31), 6) as usize],
    };
    Appearance::new(
        a as u8,
        body,
        skin,
        clothes,
        hair,
        hair_colour,
        item as u8,
        accessory,
    )
}

/// Is the person at dense index `i` an officer?
#[inline]
pub fn is_officer(p: &People, i: usize) -> bool {
    Archetype::from_u8(p.stat[i][crate::state::stat::ARCH]).is_officer()
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
