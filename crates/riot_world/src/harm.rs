//! Hurting people: the health a blow, a projectile or a crush takes, and
//! death (DESIGN §2.6). Shared by the interaction phase and the hazards.

use riot_core::math::narrow_u8;

use riot_defs::archetype::Archetype;
use riot_defs::harm::Harm;
use riot_defs::setup::NotableKind;
use riot_defs::state::{Gait, emo, flag, health, pose, stat};
use riot_fields::Layer;

use crate::world::World;

/// Health loss from a blow, a projectile or a crush. `by` is the attacker's
/// stable id, or `u32::MAX` (a crush, which armour does not help with).
pub fn damage(w: &mut World, i: usize, amount: u8, by: u32, cause: Harm) {
    let st = &mut w.people.stat[i];
    let officer = Archetype::from_u8(st[stat::ARCH]).is_officer();
    let amount = if officer && by != u32::MAX {
        (amount as f32 * w.tun.officer_armour).round() as u8
    } else {
        amount
    };
    let before = st[stat::HEALTH];
    let after = before.saturating_sub(amount);
    st[stat::HEALTH] = after;
    st[stat::FLAGS] = (st[stat::FLAGS] & !flag::ASLEEP) | flag::URGENT;
    w.outcome.harm[cause as usize] += (before - after) as u32;
    if before >= health::INJURED && after < health::INJURED {
        w.outcome.injured(officer);
    }
    let s = w.people.stable_id[i] as usize;
    w.cold[s].hits = w.cold[s].hits.saturating_add(1);
    w.cold[s].last_harm = cause as u8;
    if !officer && by != u32::MAX {
        let m = &mut w.people.emo[i][emo::MORALE];
        *m = m.saturating_sub(narrow_u8(w.tun.hit_morale));
    }
    if by != u32::MAX && (by as usize) < w.police.officers.len() {
        // Hit by the police: anger and fear.
        let e = &mut w.people.emo[i];
        e[emo::ANGER] = e[emo::ANGER].saturating_add(30);
        e[emo::FEAR] = e[emo::FEAR].saturating_add(20);
    }
    if after < health::INCAPACITATED && !Gait::from_u8(w.people.pose[i][pose::GAIT]).is_down() {
        w.people.pose[i][pose::GAIT] = Gait::Fallen as u8;
        w.people.stat[i][stat::FLAGS] |= flag::DOWN;
    }
    if after == 0 {
        die(w, i);
    }
}

/// Someone dies: counted, by cause, and taken off the map.
pub fn die(w: &mut World, i: usize) {
    let p = w.people.pos(i);
    let s = w.people.stable_id[i];
    let a = Archetype::from_u8(w.people.stat[i][stat::ARCH]);
    w.outcome.deaths += 1;
    // Fire kills in absorb, which leaves no record: being in it says enough.
    let cause = if w.fields.sample(Layer::Fire, p.x, p.y) > 0.05 {
        Harm::Fire
    } else {
        Harm::from_u8(w.cold[s as usize].last_harm)
    };
    w.outcome.deaths_by[cause as usize] += 1;
    if !a.is_officer() && w.cold[s as usize].hits > 0 {
        w.outcome.notable(w.tick, NotableKind::Martyr, p, s);
    }
    // Whoever carried them stops.
    w.carried.remove(s);
    w.remove(i);
}
