//! The world and the tick (DESIGN §4.3).
//!
//! [`World`] holds the simulation state, a [`riot_world::World`] it derefs
//! to, and runs the phases over it in tick order. The phases that need the
//! whole world take `&mut riot_world::World`; steering, bodies and flow get
//! only the parts they read and write.

use std::ops::{Deref, DerefMut};

use riot_core::Vec2;
use riot_core::lanes::{self, CHUNK_AGENTS};
use riot_core::rng::{Stream, roll_key, unit};

use riot_defs::command::{Command, CommandKind, DebugCommand};
use riot_defs::harm::Harm;
use riot_defs::mapdef::{MapDef, PropKind};
use riot_defs::setup::{RiotOutcome, RiotSetup, SupplyKind};
use riot_defs::tunables::Tunables;
use riot_fields::{AbsorbParams, CC, Layer, absorb, blur_kernel};
use riot_physics::{BodyCtx, Obb, PhysScratch};
use riot_steer::SteerCtx;

pub use riot_defs::phase::{NoProfiler, PHASES, Phase, Profiler};
pub use riot_defs::targets::{Objective, SupplyDrop};
pub use riot_fields::{field_cell_of, field_cell_pos};
pub use riot_snapshot::{appearance_for, prop_flag};
// Everything else of the state's module; `World` here is the one below.
pub use riot_world::world::*;

pub struct World {
    state: riot_world::World,
    /// Buffers for the body phase, which reads and writes only parts of the
    /// world.
    pub phys: PhysScratch,
}

impl Deref for World {
    type Target = riot_world::World;
    #[inline]
    fn deref(&self) -> &riot_world::World {
        &self.state
    }
}

impl DerefMut for World {
    #[inline]
    fn deref_mut(&mut self) -> &mut riot_world::World {
        &mut self.state
    }
}

impl World {
    pub fn new(setup: &RiotSetup, mapdef: &MapDef, tun: Tunables) -> Result<World, SetupError> {
        let state = riot_world::World::new(setup, mapdef, tun)?;
        let phys = PhysScratch::new(state.people.cap);
        Ok(World { state, phys })
    }

    /// Run one tick (DESIGN §4.3).
    pub fn tick(&mut self, cmds: &[Command]) {
        self.tick_profiled(cmds, &mut NoProfiler);
    }

    pub fn tick_profiled(&mut self, cmds: &[Command], prof: &mut impl Profiler) {
        let w = &mut self.state;
        prof.begin(Phase::Commands);
        apply_commands(w, cmds); //  1  validate + apply, in order
        arrivals(w);
        prof.end(Phase::Commands);

        prof.begin(Phase::Spatial);
        rebuild_spatial(w); //  2  body hash + neighbour lists, field index; reorder every N ticks
        prof.end(Phase::Spatial);

        prof.begin(Phase::Fields);
        update_fields(w); //  3  scatter → diffuse/decay → agents absorb (contagion)
        prof.end(Phase::Fields);

        prof.begin(Phase::Think);
        riot_think::think(w); //  4  1/K of agents re-score their actions
        prof.end(Phase::Think);

        prof.begin(Phase::Messages);
        riot_messages::spread(w); //  5  rumours move
        prof.end(Phase::Messages);

        prof.begin(Phase::Steer);
        steer(w); //  6  desired velocity + anticipatory avoidance + facing
        prof.end(Phase::Steer);

        prof.begin(Phase::Bodies);
        move_bodies(w, &mut self.phys); //  7  predict, contacts (Jacobi), walls, velocity, pressure, pose
        prof.end(Phase::Bodies);

        prof.begin(Phase::Interact);
        riot_interact::interact(w); //  8  emit events in parallel, resolve in order
        prof.end(Phase::Interact);

        prof.begin(Phase::Director);
        riot_police::director(w); //  9  every 20 ticks; squads and vehicles every tick
        prof.end(Phase::Director);

        prof.begin(Phase::Flow);
        w.flow.update(&w.map, &w.fields, &w.tun, w.tick); // 10  swap in finished tiles, queue needed ones
        prof.end(Phase::Flow);

        prof.begin(Phase::Account);
        riot_outcome::account(w); // 11  outcome tallies
        riot_outcome::snapshot_write::write(w); //     render snapshot
        prof.end(Phase::Account);
        w.tick += 1;
    }

    /// Simulate until `setup.duration` and return the outcome (auto-resolve,
    /// DESIGN §17).
    pub fn resolve(&mut self) -> RiotOutcome {
        while !self.finished() {
            self.tick(&[]);
        }
        self.outcome()
    }

    pub fn outcome(&self) -> RiotOutcome {
        riot_outcome::outcome(&self.state)
    }

    /// Field chunk cells per side, for renderers.
    pub const FIELD_CHUNK_CELLS: usize = CC;
}

// -----------------------------------------------------------------------
// Phase 1: commands and arrivals

fn apply_commands(w: &mut riot_world::World, cmds: &[Command]) {
    for c in cmds {
        debug_assert!(
            c.tick == w.tick,
            "command for tick {} applied at {}",
            c.tick,
            w.tick
        );
        if c.tick != w.tick {
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
                    let pos = w.map.nearest_open(p, 32.0).unwrap_or(p);
                    let o = &mut w.objectives[slot as usize];
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
                    w.retarget_objective(slot);
                }
            }
            CommandKind::ClearObjective { slot } => {
                if (slot as usize) < MAX_OBJECTIVES {
                    w.objectives[slot as usize].active = false;
                    w.flow.clear_goal(slot);
                }
            }
            CommandKind::SetStance(s) => w.stance = s,
            CommandKind::Broadcast {
                organizer,
                objective,
            } => {
                riot_messages::broadcast(w, organizer, objective);
            }
            CommandKind::DropSupplies { kind, x, y, amount } => {
                drop_supplies(w, kind, Vec2::new(x, y), amount)
            }
            CommandKind::Chant { organizer, on } => riot_messages::set_chant(w, organizer, on),
            CommandKind::SetTunable { id, value } => {
                w.tun.set(id, value);
                w.minds.retune(&w.tun);
            }
            CommandKind::Debug(d) => debug_command(w, d),
        }
    }
}

fn debug_command(w: &mut riot_world::World, d: DebugCommand) {
    match d {
        DebugCommand::Gas { x, y } => w.hazards.add_gas(Vec2::new(x, y), w.tick, u32::MAX),
        DebugCommand::Ignite { x, y } => {
            w.hazards
                .ignite(Vec2::new(x, y), &mut w.fields, &w.map, &mut w.props)
        }
        DebugCommand::Spawn {
            x,
            y,
            count,
            archetype,
            radius,
        } => {
            for k in 0..count as u32 {
                if w.debug_next >= w.max_people || w.people.len >= w.people.cap {
                    extend_capacity_note(w);
                    break;
                }
                let r = roll_key(w.seed, w.tick, k, 0xdeb0, Stream::Spawn);
                let off = Vec2::from_heading((r >> 8) as u8) * (radius * unit(r).sqrt());
                let p = Vec2::new(x, y) + off;
                let p = w.map.nearest_open(p, 8.0).unwrap_or(p);
                let stable = w.debug_next;
                w.debug_next += 1;
                w.spawn_person(stable, p, archetype as u8, 0);
            }
        }
        DebugCommand::Threat { x, y, amount } => {
            // Trouble, as the police see it.
            let p = Vec2::new(x, y);
            w.fields.stamp(Layer::Threat, p, 6.0, amount, &w.map);
            w.police.cmd.note_violence(w.tick, p, amount / 3.0);
        }
        DebugCommand::Escalate { rung } => w.police.force_escalation(rung, w.setup.escalation_cap),
    }
}

fn extend_capacity_note(w: &mut riot_world::World) {
    w.outcome.spawn_overflow += 1;
}

fn drop_supplies(w: &mut riot_world::World, kind: SupplyKind, p: Vec2, amount: u16) {
    let left = w.supplies_left.get_mut(kind);
    let n = (amount as u32).min(*left) as u16;
    if n == 0 || w.drops.len() == w.drops.capacity() {
        return;
    }
    *left -= n as u32;
    *w.outcome.supplies_used.get_mut(kind) += n as u32;
    let pos = w.map.nearest_open(p, 16.0).unwrap_or(p);
    w.drops.push(SupplyDrop {
        pos,
        kind,
        amount: n,
    });
    match kind {
        SupplyKind::Water => w.retarget_supply(SupplyKind::Water),
        SupplyKind::Medic => w.retarget_supply(SupplyKind::Medic),
        _ => {}
    }
}

fn arrivals(w: &mut riot_world::World) {
    w.arrive_scheduled();
    riot_hazards::metro_arrivals(w);
}

// -----------------------------------------------------------------------
// Phase 2

fn rebuild_spatial(w: &mut riot_world::World) {
    let every = w.tun.reorder_interval.max(1.0) as u32;
    if w.tick.is_multiple_of(every) {
        w.compact();
    }
    w.spatial.rebuild(&w.people, &w.arch);
}

// -----------------------------------------------------------------------
// Phase 3

fn update_fields(w: &mut riot_world::World) {
    let tick = w.tick;
    let t = w.tun;
    w.fields.activate_for_tick(&w.spatial, &w.map);
    // Chunks kept active whatever they hold: police barriers first, then
    // gas emitters and fires (the order they come in assigns pool slots).
    w.fields.pinned.fill(false);
    riot_tactics::pin_barriers(w);
    riot_hazards::pin_emitters(w);
    // Multi-rate layers (DESIGN §8): emotions and threat every 4 ticks,
    // on alternate ticks so their costs do not stack.
    let threat_tick = tick.is_multiple_of(4);
    let emo_tick = tick % 4 == 2;
    w.fields
        .scatter(&w.spatial, &w.people, &w.tun, threat_tick, emo_tick);
    riot_messages::stamp_chants(w);
    if tick.is_multiple_of(10) {
        riot_hazards::stamp_cameras(w);
    }
    if emo_tick {
        w.fields
            .blur(&[Layer::Fear, Layer::Anger, Layer::Weight], t.field_diffuse);
    }
    if threat_tick {
        let tb = blur_kernel(0.2);
        w.fields.filter(Layer::Threat, tb, tb, 1.0, false);
    }
    riot_hazards::update_fields(w);
    let ap = AbsorbParams {
        arch: &w.arch,
        tun: &w.tun,
        heat: w.setup.weather.heat,
        seed: w.seed,
        tick,
    };
    let h = absorb(&w.fields, &mut w.people, &w.spatial, &ap);
    w.outcome.injuries += h.injured;
    w.outcome.officers_injured += h.officers_injured;
    w.outcome.harm[Harm::Gas as usize] += h.gas;
    w.outcome.harm[Harm::Fire as usize] += h.fire;
    if tick.is_multiple_of(20) {
        w.fields.housekeeping(1e-3);
    }
}

// -----------------------------------------------------------------------
// Phase 6

fn steer(w: &mut riot_world::World) {
    let span = w.people.chunks() * CHUNK_AGENTS;
    w.scratch.vx[..span].copy_from_slice(&lanes::flat(&w.people.vel_x)[..span]);
    w.scratch.vy[..span].copy_from_slice(&lanes::flat(&w.people.vel_y)[..span]);
    let mut records = std::mem::take(&mut w.flow.records);
    let cx = SteerCtx {
        map: &w.map,
        flow: &w.flow,
        fields: &w.fields,
        sp: &w.spatial,
        arch: &w.arch,
        tun: &w.tun,
        objectives: &w.objectives,
        drops: &w.drops,
        officer_targets: &w.officer_targets,
        slot_of: &w.slot_of,
        tick: w.tick,
        seed: w.seed,
    };
    riot_steer::steer(
        &mut w.people,
        &w.scratch.vx,
        &w.scratch.vy,
        &mut records,
        &cx,
    );
    w.flow.records = records;
}

// -----------------------------------------------------------------------
// Phase 7

fn move_bodies(w: &mut riot_world::World, phys: &mut PhysScratch) {
    // Props stand still: their grid follows the props, not the clock, so
    // a city full of parked cars costs nothing per tick.
    if w.props_dirty {
        w.obbs.clear();
        for (k, pr) in w.props.iter().enumerate() {
            if pr.flags & prop_flag::DESTROYED == 0 && w.obbs.len() < w.obbs.capacity() {
                w.obbs.push(Obb {
                    c: pr.pos,
                    ax: pr.ax,
                    half: pr.half,
                    kind: if pr.kind == PropKind::PoliceBarrier {
                        2
                    } else {
                        0
                    },
                    id: k as u16,
                });
            }
        }
        w.obst.rebuild(&w.obbs);
        w.props_dirty = false;
    }
    w.veh_obbs.clear();
    w.police.vehicle_obbs(&mut w.veh_obbs);
    w.veh_obst.rebuild(&w.veh_obbs);
    for e in &mut w.events {
        e.clear();
    }
    let cx = BodyCtx {
        map: &w.map,
        sp: &w.spatial,
        arch: &w.arch,
        tun: &w.tun,
        obbs: &w.obbs,
        grid: &w.obst,
        veh_obbs: &w.veh_obbs,
        veh_grid: &w.veh_obst,
    };
    riot_physics::move_bodies(&mut w.people, phys, &mut w.events, &cx);
}
