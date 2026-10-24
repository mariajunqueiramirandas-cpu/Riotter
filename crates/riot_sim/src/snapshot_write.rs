//! Phase 11b: the render snapshot (DESIGN §18.1). People are gathered
//! through `slot_of` into `stable_id` order, in parallel over output chunks.

use rayon::prelude::*;
use riot_core::lanes;
use riot_core::math::{narrow_u8, widen_u8};

use crate::archetype::Archetype;
use crate::fields::{CC, CL, Layer, NO_CHUNK};
use crate::snapshot::*;
use crate::state::{emo, flag, health, pose, stat};
use crate::world::{NO_SLOT, World, prop_flag};

pub fn write(w: &mut World) {
    if !w.snap_cfg.enabled {
        return;
    }
    let s = &mut w.snapshot;
    s.tick = w.tick;
    let issued = w.issued as usize;
    if s.people.capacity() < w.max_people as usize {
        s.people
            .reserve_exact(w.max_people as usize - s.people.len());
    }
    s.people
        .resize(issued.min(w.max_people as usize), SnapPerson::default());
    let p = &w.people;
    let (px, py) = (lanes::flat(&p.pos_x), lanes::flat(&p.pos_y));
    let slot_of = &w.slot_of;
    let cold = &w.cold;
    s.people
        .par_chunks_mut(512)
        .enumerate()
        .for_each(|(c, out)| {
            for (k, o) in out.iter_mut().enumerate() {
                let sid = c * 512 + k;
                let i = slot_of[sid];
                if i == NO_SLOT {
                    *o = SnapPerson::default();
                    continue;
                }
                let i = i as usize;
                let st = p.stat[i];
                let a = Archetype::from_u8(st[stat::ARCH]);
                let fl = st[stat::FLAGS];
                let mut f = snap_flag::PRESENT;
                if a.is_officer() {
                    f |= snap_flag::OFFICER;
                }
                if st[stat::HEALTH] < health::INJURED {
                    f |= snap_flag::INJURED;
                }
                if fl & flag::ITEM != 0 {
                    f |= snap_flag::ITEM;
                }
                if fl & flag::SHIELD != 0 {
                    f |= snap_flag::SHIELD;
                }
                if a == Archetype::Organizer {
                    f |= snap_flag::ORGANIZER;
                }
                if fl & flag::ASLEEP != 0 {
                    f |= snap_flag::ASLEEP;
                }
                if fl & flag::KNOWN != 0 {
                    f |= snap_flag::KNOWN;
                }
                let ps = p.pose[i];
                *o = SnapPerson {
                    x: px[i],
                    y: py[i],
                    heading: ps[pose::HEADING],
                    gait: ps[pose::GAIT],
                    stride: ps[pose::STRIDE],
                    flags: f,
                    appearance: cold[sid].appearance,
                };
            }
        });

    s.vehicles.clear();
    for v in &w.police.vehicles {
        s.vehicles.push(SnapVehicle {
            x: v.pos.x,
            y: v.pos.y,
            half_len: v.half.x,
            half_wid: v.half.y,
            heading: v.ax.heading(),
            kind: v.kind as u8,
            flags: (v.state == crate::police::VehicleState::Driving) as u8
                | ((v.state == crate::police::VehicleState::Spraying) as u8) << 1,
            _pad: 0,
        });
    }
    s.projectiles.clear();
    for pr in &w.hazards.projectiles {
        s.projectiles.push(SnapProjectile {
            x: pr.pos.x,
            y: pr.pos.y,
            kind: pr.kind as u8,
            _pad: [0; 3],
        });
    }
    s.props.clear();
    for pr in &w.props {
        s.props.push(SnapProp {
            x: pr.pos.x,
            y: pr.pos.y,
            half_x: pr.half.x,
            half_y: pr.half.y,
            cos: pr.ax.x,
            sin: pr.ax.y,
            kind: pr.kind as u8,
            health: pr.health,
            flags: pr.flags,
            _pad: 0,
        });
    }
    let _ = prop_flag::BURNING;
    s.objectives.clear();
    for (k, o) in w.objectives.iter().enumerate() {
        if o.active {
            s.objectives.push(SnapObjective {
                slot: k as u8,
                active: true,
                kind: o.kind as u8,
                x: o.pos.x,
                y: o.pos.y,
                radius: o.radius,
                held: o.held_now,
            });
        }
    }
    s.drops.clear();
    for d in &w.drops {
        s.drops.push(SnapDrop {
            x: d.pos.x,
            y: d.pos.y,
            kind: d.kind as u8,
            amount: d.amount,
        });
    }
    s.squads.clear();
    for sq in &w.police.squads {
        if sq.deployed {
            s.squads.push(SnapSquad {
                x: sq.pos.x,
                y: sq.pos.y,
                tx: sq.anchor.x,
                ty: sq.anchor.y,
                doctrine: sq.doctrine as u8,
                kind: sq.kind as u8,
                officers: sq.present,
                morale: narrow_u8(sq.morale),
            });
        }
    }

    // Field layers every few ticks.
    let every = w.snap_cfg.fields_every;
    if every > 0 && w.tick.is_multiple_of(every) {
        let f = &w.fields;
        let fs = &mut s.fields;
        fs.tick = w.tick;
        fs.chunk_cells = CC as u32;
        fs.cell = crate::spatial::FIELD_CELL;
        fs.half = f.geom.half;
        fs.overlay_kind = w.snap_cfg.overlay;
        fs.chunks.clear();
        fs.gas.clear();
        fs.smoke.clear();
        fs.fire.clear();
        fs.overlay.clear();
        let reserve = f.pool * CL;
        for v in [&mut fs.gas, &mut fs.smoke, &mut fs.fire, &mut fs.overlay] {
            if v.capacity() < reserve {
                v.reserve_exact(reserve - v.len());
            }
        }
        if fs.chunks.capacity() < f.pool {
            fs.chunks.reserve_exact(f.pool);
        }
        let q = |x: f32, scale: f32| narrow_u8(x * scale);
        // Layers with nothing in them stay empty (see `FieldSnap`).
        let (gas, smoke) = (w.hazards.gas_live, w.hazards.smoke_live);
        let fire = smoke;
        let overlay = w.snap_cfg.overlay;
        for slot in 0..f.pool {
            let c = f.slot_chunk[slot];
            if c == NO_CHUNK {
                continue;
            }
            fs.chunks.push([(c % f.cdim) as u16, (c / f.cdim) as u16]);
            if gas {
                fs.gas
                    .extend(f.layer(slot, Layer::Gas).iter().map(|&x| q(x, 0.25)));
            }
            if smoke {
                fs.smoke
                    .extend(f.layer(slot, Layer::Smoke).iter().map(|&x| q(x, 0.25)));
            }
            if fire {
                fs.fire
                    .extend(f.layer(slot, Layer::Fire).iter().map(|&x| q(x, 1.0)));
            }
            let src = match overlay {
                Overlay::None => continue,
                Overlay::Density | Overlay::Pressure => (Layer::Dens, 1.0 / 16.0),
                Overlay::Fear | Overlay::Anger => {
                    // Stored as sums: show the local mean.
                    let l = if overlay == Overlay::Fear {
                        Layer::Fear
                    } else {
                        Layer::Anger
                    };
                    let wt = f.layer(slot, Layer::Weight);
                    fs.overlay.extend(
                        f.layer(slot, l)
                            .iter()
                            .zip(wt)
                            .map(|(&x, &w)| if w > 0.05 { q(x / w, 1.0) } else { 0 }),
                    );
                    continue;
                }
                Overlay::Threat => (Layer::Threat, 0.1),
                Overlay::Gas => (Layer::Gas, 0.25),
                Overlay::Camera => (Layer::Camera, 0.1),
                Overlay::Messages => {
                    fs.overlay.extend(
                        f.msg[slot * CL..(slot + 1) * CL]
                            .iter()
                            .map(|&m| (m >> 8) as u8),
                    );
                    continue;
                }
            };
            fs.overlay
                .extend(f.layer(slot, src.0).iter().map(|&x| q(x, src.1)));
        }
    }

    // Stats.
    let mut st = SnapStats {
        tick: w.tick,
        ..Default::default()
    };
    let (mut fear, mut anger, mut morale, mut n) = (0.0f64, 0.0f64, 0.0f64, 0u32);
    for i in 0..w.people.len {
        let x = w.people.stat[i];
        if x[stat::FLAGS] & flag::DEAD != 0 {
            continue;
        }
        st.present += 1;
        if x[stat::FLAGS] & flag::ASLEEP != 0 {
            st.asleep += 1;
        }
        if Archetype::from_u8(x[stat::ARCH]).is_officer() {
            st.officers += 1;
        } else {
            st.protesters += 1;
            let e = w.people.emo[i];
            fear += widen_u8(e[emo::FEAR]) as f64;
            anger += widen_u8(e[emo::ANGER]) as f64;
            morale += widen_u8(e[emo::MORALE]) as f64;
            n += 1;
        }
    }
    if n > 0 {
        st.mean_fear = (fear / n as f64) as f32;
        st.mean_anger = (anger / n as f64) as f32;
        st.mean_morale = (morale / n as f64) as f32;
    }
    st.arrests = w.outcome.arrests;
    st.injuries = w.outcome.injuries;
    st.deaths = w.outcome.deaths;
    st.went_home = w.outcome.went_home;
    st.escalation = w.police.escalation as u8;
    st.messages = w.msgs.count;
    st.movement_blame = w.outcome.footage.movement.blame;
    st.police_blame = w.outcome.footage.police.blame;
    st.clock_minute = w.clock_minute();
    st.internet = w.internet;
    w.snapshot.stats = st;
}
