//! Tunables (DESIGN §19 iteration loop). All are `f32`, addressed by name or
//! id, and changed during a riot only through `Command::SetTunable`, so a
//! change is applied at a specific tick and recorded in the log (DESIGN §12
//! rule 9). `riot_data` reads `tunables.ron` into these by name.

macro_rules! tunables {
    ($($name:ident = $default:expr, $doc:literal;)*) => {
        /// Every tunable, with its default. See [`TUNABLES`] for names and docs.
        #[derive(Clone, Copy, Debug, PartialEq)]
        pub struct Tunables {
            $(#[doc = $doc] pub $name: f32,)*
        }

        impl Default for Tunables {
            fn default() -> Self {
                Tunables { $($name: $default,)* }
            }
        }

        /// `(name, doc)` for each tunable, in id order.
        pub const TUNABLES: &[(&str, &str)] = &[$((stringify!($name), $doc),)*];

        impl Tunables {
            pub fn get(&self, id: u16) -> Option<f32> {
                let mut i = 0u16;
                $(if i == id { return Some(self.$name); } i += 1;)*
                let _ = i;
                None
            }

            /// Returns false for an unknown id.
            pub fn set(&mut self, id: u16, value: f32) -> bool {
                let mut i = 0u16;
                $(if i == id { self.$name = value; return true; } i += 1;)*
                let _ = i;
                false
            }
        }
    };
}

tunables! {
    think_interval = 10.0, "Ticks between an agent's rethinks (K).";
    reorder_interval = 8.0, "Ticks between spatial reorders of the agent arrays.";
    contact_iters = 2.0, "Jacobi contact iterations per tick.";
    contact_iters_hot = 4.0, "Contact iterations when the last tick's peak pressure was high.";
    hot_pressure = 0.35, "Peak pressure (0..1) that raises the contact iterations.";
    contact_relax = 0.9, "Jacobi relaxation factor for contact push-out.";
    pressure_scale = 6.0, "Push-out per contact iteration (m) → pressure (0..1).";
    pressure_baseline = 0.02, "Push-out per contact iteration (m) that is just walking into people, not pressure.";
    pressure_smoothing = 0.125, "Per-tick blend of measured into held pressure: a crush needs compression sustained for about half a second.";
    neighbour_skin = 0.15, "Extra reach beyond touching when building neighbour lists (m).";
    sleep_ticks = 40.0, "Ticks of stillness before an agent sleeps.";
    sleep_speed = 0.05, "Speed under which an agent counts as still (m/s).";
    wake_interval = 80.0, "Ticks between a sleeper's slow-timer rethinks.";
    wake_push = 0.01, "Overlap (m) at which an awake body wakes a sleeper.";
    flow_latency = 10.0, "Ticks from requesting a flow tile to swapping it in (L).";
    flow_refresh = 20.0, "Ticks between dynamic cost refreshes per goal.";
    flow_density_cost = 6.0, "Route cost per person per 4 m² field cell.";
    flow_gas_cost = 40.0, "Route cost per unit of gas.";
    flow_fire_cost = 150.0, "Route cost per unit of fire.";
    flow_block_cost = 120.0, "Route cost of a cell across a police barrier: like this many people per cell, so routes go round a barricade when they can.";
    avoid_strength = 1.2, "Anticipatory avoidance strength.";
    avoid_horizon = 1.6, "Time-to-collision horizon for avoidance (s).";
    wall_avoid = 0.8, "Wall avoidance strength.";
    density_avoid = 0.08, "Steering away from the density gradient.";
    hold_radius = 0.6, "Fraction of an objective's radius people hold inside.";
    contagion_fear = 0.01, "Per-tick pull of fear toward the local crowd's mean.";
    contagion_anger = 0.004, "Per-tick pull of anger toward the local crowd's mean (rising; a third of it falling).";
    emotion_relax = 0.002, "Per-tick relaxation of emotions toward archetype baselines.";
    field_blend = 0.25, "How fast the emotion fields follow the people in each cell, per emotion update.";
    field_diffuse = 0.15, "Side weight of the 3-tap emotion field blur.";
    threat_decay = 0.92, "Per-update decay of the threat field.";
    threat_officer = 0.25, "Threat per officer per update.";
    threat_violence = 3.0, "Threat stamped per violent act.";
    threat_fear = 0.006, "Fear per tick at full threat, scaled by timidity.";
    threat_anger = 0.004, "Anger per tick at full threat, scaled by willingness to use violence.";
    gas_decay = 0.995, "Per-tick decay of gas.";
    gas_diffuse = 0.18, "Side weight of the 3-tap gas blur.";
    gas_emit = 6.0, "Gas emitted per canister per tick.";
    gas_fear = 0.12, "Fear per tick per unit of gas.";
    gas_damage = 0.00015, "Health lost per tick per unit of gas (protesters only; gas alone leaves at least 86% health).";
    gas_fatigue = 0.002, "Fatigue per tick per unit of gas.";
    smoke_decay = 0.985, "Per-tick decay of smoke.";
    chant_radius = 12.0, "Radius of an organizer's chant (m).";
    chant_morale = 0.0003, "Morale per tick from a chant at full strength.";
    chant_cost = 0.00004, "Organizer fatigue per tick while chanting.";
    morale_decay = 0.00001, "Per-tick morale loss when nothing lifts it, at moderate fear (fear speeds it up).";
    fatigue_walk = 0.000006, "Fatigue per tick while walking.";
    fatigue_run = 0.00004, "Fatigue per tick while running.";
    fatigue_heat = 0.000004, "Extra fatigue per tick at full heat.";
    rest_recover = 0.0001, "Fatigue recovered per tick while resting.";
    water_recover = 0.25, "Fatigue recovered by drinking.";
    medic_heal = 0.004, "Health restored per tick at a medic post.";
    crush_pressure = 0.7, "Pressure (0..1) above which bodies are crushed.";
    crush_damage = 0.0008, "Health lost per tick at the most excess pressure (fraction of full health).";
    crush_fear = 0.01, "Fear per tick at the most excess pressure.";
    fall_pressure = 0.85, "Pressure (0..1) above which people may fall.";
    fall_chance = 0.01, "Per-tick fall chance at full excess pressure.";
    trip_chance = 0.004, "Per-tick chance of tripping per fallen neighbour when pushed.";
    getup_chance = 0.02, "Per-tick chance a fallen, unpressed person stands up.";
    hop_loss = 6.0, "Fidelity lost per word-of-mouth hop.";
    hear_rate = 0.08, "Per-tick chance of adopting a heard message, before trust.";
    phone_rate = 0.002, "Per-tick chance a phone receives the latest broadcast.";
    phone_fidelity = 0.75, "Fidelity factor of messages received by phone.";
    garble_fidelity = 140.0, "Fidelity below which messages can be garbled.";
    organizer_voice = 10.0, "Radius in which organizers speak at full fidelity (m).";
    violence_base = 0.04, "Base score of violent actions.";
    throw_range = 18.0, "Maximum throwing distance (m).";
    throw_speed = 15.0, "Projectile speed (m/s).";
    throw_cooldown = 60.0, "Ticks between throws.";
    projectile_damage = 0.08, "Health lost to a projectile hit.";
    strike_damage = 0.08, "Health lost to a baton strike.";
    officer_armour = 0.4, "Fraction of a blow or projectile that gets through an officer's helmet and body armour.";
    strike_range = 0.9, "Reach of a strike beyond touching (m).";
    arrest_ticks = 60.0, "Ticks for officers to restrain someone.";
    director_interval = 20.0, "Ticks between police director replans.";
    escalate_threat = 40.0, "Summed threat that makes the director climb a rung.";
    deescalate_ticks = 2400.0, "Calm ticks before the director steps down a rung.";
    officer_break_fear = 0.75, "Officer fear at which formation breaks.";
    kettle_shrink = 0.004, "Kettle radius shrink per tick (fraction).";
    barrier_push = 1.5, "Strength (of 255) a police barrier loses per check (every 10 ticks) per unit of push: pressure × anger × 2, plus anger over 0.4.";
    barrier_brace = 0.3, "Fraction of the crowd's push each officer bracing a barrier from behind takes off (at least 15% always gets through).";
    ambush_violence = 2.0, "Violence at a barricade (severity over the last 20 s) that springs an ambush waiting for it.";
    charge_ticks = 600.0, "Ticks a sprung ambush charges and sweeps before it holds a line.";
    relief_fatigue = 0.45, "Squad fatigue at which a fresh reserve squad takes over its line.";
    water_force = 3.0, "Water cannon impulse (m/s).";
    water_range = 25.0, "Water cannon reach (m).";
    fire_spread = 0.03, "Per-tick chance fire spreads to a neighbouring fuel cell.";
    fire_burn = 2.0, "Fuel burnt per tick per burning cell.";
    fire_damage = 0.0015, "Health lost per tick standing in full fire (nothing below 0.3 of it).";
    metro_delay = 3600.0, "Ticks a metro ride takes.";
    hold_density = 0.4, "Protesters per m² that count as holding an objective.";
    exit_radius = 4.0, "Distance from an exit at which leaving people disappear (m).";
    leave_morale = 0.18, "Morale under which people consider going home.";
    gas_morale = 0.0004, "Morale lost per tick per unit of gas (protesters).";
    hit_morale = 0.05, "Morale lost to each blow or projectile that lands on a protester.";
    camera_radius = 25.0, "How far a camera sees (m).";
}

impl Tunables {
    pub fn id_of(name: &str) -> Option<u16> {
        TUNABLES
            .iter()
            .position(|(n, _)| *n == name)
            .map(|i| i as u16)
    }

    pub fn get_by_name(&self, name: &str) -> Option<f32> {
        Self::id_of(name).and_then(|i| self.get(i))
    }

    pub fn set_by_name(&mut self, name: &str, value: f32) -> bool {
        Self::id_of(name).is_some_and(|i| self.set(i, value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_ids_agree() {
        let mut t = Tunables::default();
        for (i, (name, _)) in TUNABLES.iter().enumerate() {
            assert_eq!(Tunables::id_of(name), Some(i as u16));
            assert!(t.get(i as u16).is_some());
            assert!(t.set(i as u16, 1.5));
            assert_eq!(t.get_by_name(name), Some(1.5));
        }
        assert!(t.get(TUNABLES.len() as u16).is_none());
        assert!(!t.set_by_name("nope", 1.0));
    }
}
