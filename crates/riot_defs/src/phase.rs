//! The phases of a tick and the hooks that time them (DESIGN §3.1, §4.3).

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
