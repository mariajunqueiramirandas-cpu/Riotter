//! Minds' lookup tables (DESIGN §10): the inputs people weigh, the response
//! curves of each civilian action's considerations, and the archetype ×
//! action weights, built once at riot start. Thinking with them is
//! `riot_think`.

use riot_core::mix;

use riot_defs::archetype::ARCHETYPES;
use riot_defs::state::{Action, CIVILIAN_ACTIONS};
use riot_defs::tunables::Tunables;

/// Consideration inputs, each quantized to `u8`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum Input {
    Fear = 0,
    Anger,
    Morale,
    Fatigue,
    Injury,
    /// Threat seen in the field of view.
    Threat,
    Gas,
    Crowding,
    /// Distance to the intended objective (0 = inside, 255 = far or none).
    GoalDist,
    /// Knows a plan (0/255).
    Plan,
    /// Stance of the plan: 0 peaceful, 128 defensive, 255 confrontational.
    Stance,
    /// Cameras watching.
    Camera,
    /// Injured people nearby.
    InjuredNear,
    /// Supplies exist that help (water, medic).
    Supply,
    /// Holding something to throw.
    Armed,
    /// Archetype's willingness to use violence.
    Violence,
    /// The objective asks to hold ground.
    HoldGoal,
    /// Is an organizer and was asked to chant.
    ChantAsked,
    /// Fraction of the riot's duration elapsed.
    Late,
}

pub const INPUTS: usize = 19;

/// A response curve over `0..=255`.
#[derive(Clone, Copy, Debug)]
pub enum Curve {
    /// `lo + (hi - lo) · x^k`.
    Rise { lo: f32, hi: f32, k: f32 },
    /// `hi - (hi - lo) · x^k`.
    Fall { lo: f32, hi: f32, k: f32 },
    /// Smooth step from `lo` to `hi` between `a` and `b`.
    Step { a: f32, b: f32, lo: f32, hi: f32 },
}

impl Curve {
    fn eval(self, x: f32) -> f32 {
        let pow = |x: f32, k: f32| {
            // Integer and half-integer powers only: no powf in the sim.
            let mut r = 1.0;
            let n = k as u32;
            for _ in 0..n {
                r *= x;
            }
            if k - n as f32 > 0.25 { r * x.sqrt() } else { r }
        };
        match self {
            Curve::Rise { lo, hi, k } => lo + (hi - lo) * pow(x, k),
            Curve::Fall { lo, hi, k } => hi - (hi - lo) * pow(x, k),
            Curve::Step { a, b, lo, hi } => {
                let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
                lo + (hi - lo) * t * t * (3.0 - 2.0 * t)
            }
        }
    }

    pub fn table(self) -> [f32; 256] {
        let mut t = [0.0; 256];
        for (k, v) in t.iter_mut().enumerate() {
            *v = self.eval(k as f32 / 255.0);
        }
        t
    }
}

const fn r(lo: f32, hi: f32, k: f32) -> Curve {
    Curve::Rise { lo, hi, k }
}
const fn f(lo: f32, hi: f32, k: f32) -> Curve {
    Curve::Fall { lo, hi, k }
}
const fn s(a: f32, b: f32, lo: f32, hi: f32) -> Curve {
    Curve::Step { a, b, lo, hi }
}

/// Considerations per civilian action, in `Action` order.
fn considerations(a: Action) -> &'static [(Input, Curve)] {
    use Input::*;
    const MARCH: &[(Input, Curve)] = &[
        (Plan, s(0.4, 0.6, 0.0, 1.0)),
        (GoalDist, s(0.0, 0.3, 0.35, 1.0)),
        (Fear, f(0.1, 1.0, 2.0)),
        (Fatigue, f(0.2, 1.0, 2.0)),
        (Morale, r(0.3, 1.0, 1.0)),
    ];
    const HOLD: &[(Input, Curve)] = &[
        (Plan, s(0.4, 0.6, 0.0, 1.0)),
        (GoalDist, f(0.05, 1.0, 1.0)),
        (HoldGoal, r(0.45, 1.0, 1.0)),
        (Stance, r(0.6, 1.0, 1.0)),
        (Fear, f(0.1, 1.0, 2.0)),
        (Morale, r(0.3, 1.0, 1.0)),
    ];
    const FLEE: &[(Input, Curve)] = &[
        (Fear, s(0.45, 0.85, 0.0, 1.6)),
        (Gas, r(0.6, 1.8, 0.5)),
        (Threat, r(0.7, 1.3, 1.0)),
    ];
    const REST: &[(Input, Curve)] = &[
        (Fatigue, s(0.45, 0.9, 0.0, 1.0)),
        (Fear, f(0.0, 1.0, 1.0)),
        (Threat, f(0.1, 1.0, 1.0)),
    ];
    const SEEK_SUPPLY: &[(Input, Curve)] = &[
        (Supply, s(0.4, 0.6, 0.0, 1.0)),
        (Fatigue, s(0.4, 0.85, 0.0, 1.0)),
        (Fear, f(0.2, 1.0, 1.0)),
    ];
    const HELP: &[(Input, Curve)] = &[
        (InjuredNear, s(0.0, 0.3, 0.0, 1.0)),
        (Fear, f(0.0, 1.0, 1.0)),
        (Morale, r(0.3, 1.0, 1.0)),
    ];
    const FILM: &[(Input, Curve)] = &[(Threat, r(0.2, 1.0, 0.5)), (Fear, f(0.1, 1.0, 1.0))];
    const THROW: &[(Input, Curve)] = &[
        (Anger, s(0.45, 0.9, 0.0, 1.0)),
        (Violence, r(0.0, 1.0, 1.0)),
        (Threat, s(0.05, 0.4, 0.0, 1.0)),
        (Stance, r(0.3, 1.0, 1.0)),
        (Armed, r(0.5, 1.0, 1.0)),
        (Fear, f(0.3, 1.0, 1.0)),
    ];
    const BARRICADE: &[(Input, Curve)] = &[
        (Anger, s(0.3, 0.8, 0.0, 1.0)),
        (Threat, s(0.05, 0.3, 0.1, 1.0)),
        (Stance, r(0.2, 1.0, 1.0)),
        (Fear, f(0.2, 1.0, 1.0)),
        (Violence, r(0.2, 1.0, 1.0)),
    ];
    const CHANT: &[(Input, Curve)] = &[
        (ChantAsked, s(0.4, 0.6, 0.0, 1.0)),
        (Fatigue, f(0.0, 1.0, 1.0)),
        (Fear, f(0.2, 1.0, 1.0)),
    ];
    const GO_HOME: &[(Input, Curve)] = &[
        // Replaced when the minds are built by a step down through the
        // `leave_morale` tunable: people go home once their morale breaks.
        (Morale, f(0.0, 1.0, 3.0)),
        (Fatigue, r(0.6, 1.0, 1.0)),
        (Late, r(0.7, 1.0, 1.0)),
        (Injury, r(0.7, 1.0, 1.0)),
    ];
    const IDLE: &[(Input, Curve)] = &[(Plan, f(0.3, 1.0, 1.0))];
    match a {
        Action::March => MARCH,
        Action::Hold => HOLD,
        Action::Flee => FLEE,
        Action::Rest => REST,
        Action::SeekSupply => SEEK_SUPPLY,
        Action::Help => HELP,
        Action::Film => FILM,
        Action::Throw => THROW,
        Action::Barricade => BARRICADE,
        Action::Chant => CHANT,
        Action::GoHome => GO_HOME,
        Action::Idle => IDLE,
        _ => &[],
    }
}

/// Archetype × action weights (DESIGN §10), `Action` order up to `Idle`.
const WEIGHTS: [[f32; CIVILIAN_ACTIONS]; ARCHETYPES] = [
    // march hold flee rest seek help film throw barr chant home idle
    [
        1.00, 0.90, 1.00, 0.30, 0.30, 0.25, 0.10, 0.45, 0.35, 0.00, 1.00, 0.08,
    ], // student
    [
        1.00, 0.85, 1.00, 0.35, 0.30, 0.30, 0.05, 0.35, 0.30, 0.00, 1.00, 0.08,
    ], // worker
    [
        0.90, 0.80, 1.20, 0.60, 0.40, 0.20, 0.02, 0.02, 0.02, 0.00, 1.20, 0.10,
    ], // elder
    [
        1.00, 1.00, 0.80, 0.25, 0.25, 0.15, 0.05, 1.00, 0.80, 0.00, 0.70, 0.05,
    ], // radical
    [
        1.10, 1.00, 0.70, 0.20, 0.20, 0.35, 0.05, 0.00, 0.00, 1.50, 0.30, 0.05,
    ], // organizer
    [
        0.80, 0.70, 0.60, 0.10, 0.10, 0.00, 0.00, 1.50, 0.70, 0.00, 0.05, 0.10,
    ], // provocateur
    [
        0.15, 0.10, 1.00, 0.20, 0.20, 0.10, 1.20, 0.00, 0.00, 0.00, 0.60, 0.30,
    ], // journalist
    [
        0.20, 0.10, 1.30, 0.30, 0.20, 0.15, 0.40, 0.02, 0.01, 0.00, 1.20, 0.60,
    ], // bystander
    [0.0; CIVILIAN_ACTIONS],
    [0.0; CIVILIAN_ACTIONS],
];

const MAX_CONS: usize = 6;

/// Lookup tables and weights, built once at riot start.
pub struct Minds {
    /// `[action][consideration] → (input, table)`.
    pub luts: Vec<[[f32; 256]; MAX_CONS]>,
    pub lut_input: Vec<[u8; MAX_CONS]>,
    pub lut_n: Vec<u8>,
    pub weights: [[f32; CIVILIAN_ACTIONS]; ARCHETYPES],
    pub thought: u64,
}

impl Minds {
    pub fn new(_cap: usize, tun: &Tunables) -> Self {
        let mut luts = Vec::with_capacity(CIVILIAN_ACTIONS);
        let mut lut_input = Vec::with_capacity(CIVILIAN_ACTIONS);
        let mut lut_n = Vec::with_capacity(CIVILIAN_ACTIONS);
        for a in 0..CIVILIAN_ACTIONS {
            let cons = considerations(Action::from_u8(a as u8));
            let mut t = [[1.0f32; 256]; MAX_CONS];
            let mut ins = [0u8; MAX_CONS];
            for (k, &(input, curve)) in cons.iter().enumerate().take(MAX_CONS) {
                t[k] = curve.table();
                ins[k] = input as u8;
            }
            luts.push(t);
            lut_input.push(ins);
            lut_n.push(cons.len().min(MAX_CONS) as u8);
        }
        let mut m = Minds {
            luts,
            lut_input,
            lut_n,
            weights: WEIGHTS,
            thought: 0,
        };
        m.retune(tun);
        m
    }

    /// Rebuild the tables that depend on tunables.
    pub fn retune(&mut self, tun: &Tunables) {
        let lm = tun.leave_morale;
        self.luts[Action::GoHome as usize][0] =
            s(lm + 0.1, (lm - 0.08).max(0.0), 0.02, 1.0).table();
    }

    /// Nothing per dense index lives here, so a reorder changes nothing.
    pub fn on_reorder(&mut self) {}
}

/// Is agent `stable` due to think this tick?
#[inline]
pub fn due(stable: u32, tick: u32, k: u32) -> bool {
    ((mix(stable as u64) as u32).wrapping_add(tick)).is_multiple_of(k)
}
