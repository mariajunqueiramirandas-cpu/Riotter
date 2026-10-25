//! `police`: run a scenario and print the police commander's picture of the
//! riot every so often: the crowds it tracks and what it does about each,
//! its cordons and ambushes, its squads, and its radio calls.

use riot_sim::World;
use riot_sim::police::Doctrine;
use riot_sim::tactics::{CordonState, OpKind, call_text};

use crate::args::Args;
use crate::{default_threads, pool, scenario};

const DOCTRINES: [&str; riot_sim::police::DOCTRINES] = [
    "idle",
    "travel",
    "hold",
    "advance",
    "kettle",
    "disperse",
    "snatch",
    "retreat",
    "reserve",
    "barricade",
    "ambush",
    "charge",
    "escort",
];

pub fn police(args: &Args) -> Result<(), String> {
    let sc = scenario::load(args)?;
    let threads = args.usize("threads").unwrap_or_else(default_threads);
    let every = args.u32("every").unwrap_or(1200).max(1);
    let detail = args.flag("detail");
    let pool = pool(threads);
    let mut w = pool
        .install(|| World::new(&sc.setup, &sc.map, sc.tunables))
        .map_err(|e| e.to_string())?;
    w.snap_cfg.enabled = false;
    let ticks = args.u32("ticks").unwrap_or(w.setup.duration);
    let mut heard = 0u32;
    pool.install(|| {
        for _ in 0..ticks {
            w.tick(&[]);
            let cmd = &w.police.cmd;
            // Radio calls as they come.
            let first = heard.max(cmd.radio_n.saturating_sub(riot_sim::tactics::RADIO as u32));
            for n in first..cmd.radio_n {
                let c = &cmd.radio[n as usize % riot_sim::tactics::RADIO];
                println!(
                    "  [{}] radio: {} at ({:.0}, {:.0})",
                    clock(&w, c.tick),
                    call_text(c),
                    c.pos.x,
                    c.pos.y
                );
            }
            heard = cmd.radio_n;
            if w.tick % every == 0 {
                report(&w, detail);
            }
        }
    });
    report(&w, detail);
    let m = w.police.cmd.memory;
    println!(
        "memory: {} cordons, {} breaches, {} ambushes ({} scattered the crowd, {} did not), \
         {} reliefs, {} warnings, {} gas volleys, {} lines broken",
        m.cordons,
        m.breaches,
        m.ambushes,
        m.ambush_wins,
        m.ambush_losses,
        m.reliefs,
        m.warnings,
        m.volleys,
        m.lines_broken
    );
    let o = &w.outcome;
    let harm: Vec<String> = riot_sim::outcome::Harm::ALL
        .iter()
        .map(|h| {
            format!(
                "{} {:.1} ({} dead)",
                h.name(),
                o.harm[*h as usize] as f32 / 255.0,
                o.deaths_by[*h as usize]
            )
        })
        .collect();
    println!("harm (people's worth of health): {}", harm.join(", "));
    println!("state hash {:016x}", w.state_hash());
    Ok(())
}

fn clock(w: &World, tick: u32) -> String {
    let m = w.setup.start_minute + tick as f32 / 1200.0;
    let s = (m.fract() * 60.0) as u32;
    let m = m as u32;
    format!("{:02}:{:02}:{:02}", (m / 60) % 24, m % 60, s)
}

/// The commander's picture; with `detail`, every crowd's predicted route
/// and every squad.
fn report(w: &World, detail: bool) {
    let p = &w.police;
    let o = &w.outcome;
    println!(
        "== {} tick {}: escalation {:?}, violence {:.1}, {} on map, {} arrests, {} injured \
         (+{} officers), {} deaths",
        clock(w, w.tick),
        w.tick,
        p.escalation,
        p.violence,
        w.present(),
        o.arrests,
        o.injuries,
        o.officers_injured,
        o.deaths
    );
    for t in &p.cmd.tracks {
        if t.count < 100.0 {
            continue;
        }
        let dest = w
            .setup
            .objectives
            .get(t.dest as usize)
            .map_or("-", |o| o.name.as_str());
        println!(
            "   crowd #{:<3} {:>5.0} people at ({:>5.0}, {:>5.0}) moving {:.1} m/s  anger {:.2} \
             heat {:>5.1}  → {:<22} site {:<3} eta {:>5}  op {:<8} cordon {}",
            t.id,
            t.count,
            t.centroid.x,
            t.centroid.y,
            t.vel.len(),
            t.anger,
            t.heat,
            dest,
            if t.site == riot_sim::tactics::NO_SITE {
                "-".to_string()
            } else {
                t.site.to_string()
            },
            if t.eta.is_finite() {
                format!("{:.0}s", t.eta)
            } else {
                "-".into()
            },
            t.op.name(),
            if t.cordon == riot_sim::tactics::NO_CORDON {
                "-".to_string()
            } else {
                t.cordon.to_string()
            },
        );
        if detail {
            let r: Vec<String> = t.route[..t.route_n as usize]
                .iter()
                .map(|q| format!("({:.0},{:.0})", q.x, q.y))
                .collect();
            println!("      route {} front {:.0}", r.join(" "), t.front);
        }
        let _ = OpKind::None;
    }
    for (k, c) in p.cmd.cordons.iter().enumerate() {
        if c.state == CordonState::Gone {
            continue;
        }
        let standing = (0..c.slots as usize)
            .filter(|&s| {
                c.props[s] != u16::MAX
                    && w.props[c.props[s] as usize].flags & riot_sim::world::prop_flag::DESTROYED
                        == 0
            })
            .count();
        let squads: Vec<String> = p
            .squads
            .iter()
            .enumerate()
            .filter(|(_, s)| s.cordon == k as u8)
            .map(|(i, s)| format!("{}:{}", i, DOCTRINES[s.doctrine as usize]))
            .collect();
        println!(
            "   cordon {k} {:?} at ({:.0}, {:.0}) {}/{} barriers, {:.0} m wide, violence {:.1}, \
             ambush {}, squads [{}]",
            c.state,
            c.pos.x,
            c.pos.y,
            standing,
            c.slots,
            c.half * 2.0,
            c.violence,
            match c.ambush {
                None => "none".to_string(),
                Some(a) if a.sprung == 0 => format!("waiting at ({:.0}, {:.0})", a.pos.x, a.pos.y),
                Some(a) => format!("sprung at {}", clock(w, a.sprung)),
            },
            squads.join(" ")
        );
    }
    let mut by = [0u32; riot_sim::police::DOCTRINES];
    for s in &p.squads {
        if s.deployed {
            by[s.doctrine as usize] += 1;
        }
    }
    let list: Vec<String> = by
        .iter()
        .enumerate()
        .filter(|(_, n)| **n > 0)
        .map(|(d, n)| format!("{} {}", n, DOCTRINES[d]))
        .collect();
    println!("   squads: {}", list.join(", "));
    if detail {
        for (k, s) in p.squads.iter().enumerate() {
            if !s.deployed {
                continue;
            }
            println!(
                "      squad {k:>2} {:?} {:<9} at ({:>5.0}, {:>5.0}) → ({:>5.0}, {:>5.0}) {}/{} morale {:.2} fatigue {:.2} goal {}",
                s.kind,
                DOCTRINES[s.doctrine as usize],
                s.pos.x,
                s.pos.y,
                s.anchor.x,
                s.anchor.y,
                s.present,
                s.n,
                s.morale,
                s.fatigue,
                s.goal
            );
        }
    }
    let _ = Doctrine::Idle;
}
