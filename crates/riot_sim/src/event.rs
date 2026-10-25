//! Interaction events (DESIGN §11.3): small `Copy` values appended to
//! per-chunk buffers during parallel passes, then resolved sequentially in
//! chunk order, so conflicts are settled by that order: the first event wins.
//! Indices are dense indices, valid for the tick that emitted them.

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Event {
    /// An awake body pushed into a sleeper.
    Wake {
        target: u32,
    },
    /// Melee: a baton strike or a punch.
    Strike {
        by: u32,
        target: u32,
    },
    /// An officer grabs someone to arrest them.
    Grab {
        by: u32,
        target: u32,
    },
    /// Throw the held item (or a stone) at a point.
    Throw {
        by: u32,
        x: f32,
        y: f32,
    },
    /// Lift an injured person to carry them to a medic.
    Lift {
        by: u32,
        target: u32,
    },
    /// Take one unit from a supply drop.
    Pickup {
        by: u32,
        drop: u32,
    },
    /// Work on a barricade (`prop == u32::MAX` starts a new one).
    Build {
        by: u32,
        prop: u32,
    },
    /// Crushed by surrounding bodies.
    Crush {
        who: u32,
        amount: u8,
    },
    Fall {
        who: u32,
    },
    GetUp {
        who: u32,
    },
    Die {
        who: u32,
    },
    /// Reached an exit or a metro station.
    Leave {
        who: u32,
        entry: u32,
    },
}

const _: () = assert!(std::mem::size_of::<Event>() <= 16);
