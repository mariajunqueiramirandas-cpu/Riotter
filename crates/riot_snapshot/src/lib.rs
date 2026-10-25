//! Render snapshot (DESIGN §18.1). The renderer never reads `World`; it reads
//! these. People are written in `stable_id` order, so snapshot slot *n* is
//! always the same person and the renderer can interpolate between two
//! snapshots even though the simulation reorders agents.

#![deny(unsafe_code)]

use bytemuck::{Pod, Zeroable};
use riot_core::rng::below;
use riot_defs::archetype::Archetype;
use riot_defs::state::Item;

/// 16 bytes per person.
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
#[repr(C)]
pub struct SnapPerson {
    pub x: f32,
    pub y: f32,
    /// 256 steps of about 1.4°; 0 is +x, 64 is +y.
    pub heading: u8,
    /// [`riot_defs::state::Gait`] as `u8`.
    pub gait: u8,
    /// Walking phase: the animation frame, so feet match ground speed.
    pub stride: u8,
    /// [`snap_flag`] bits.
    pub flags: u8,
    /// [`Appearance`] bits.
    pub appearance: u32,
}

/// Bits of [`SnapPerson::flags`].
pub mod snap_flag {
    /// This slot is a person on the map. Unset for people who have not
    /// arrived yet or have left.
    pub const PRESENT: u8 = 1 << 0;
    pub const OFFICER: u8 = 1 << 1;
    pub const INJURED: u8 = 1 << 2;
    /// Holding an item (kind in the appearance).
    pub const ITEM: u8 = 1 << 3;
    pub const SHIELD: u8 = 1 << 4;
    pub const ORGANIZER: u8 = 1 << 5;
    pub const ASLEEP: u8 = 1 << 6;
    /// Identified by cameras or surveillance.
    pub const KNOWN: u8 = 1 << 7;
}

/// Fixed look of a person, derived from identity and archetype (DESIGN §2.3).
///
/// | bits  | field |
/// |-------|-------|
/// | 0–3   | archetype |
/// | 4–5   | body type (4) |
/// | 6–8   | skin tone (8) |
/// | 9–14  | clothes palette (64) |
/// | 15–18 | hair style (16) |
/// | 19–21 | hair colour (8) |
/// | 22–25 | held item ([`riot_defs::state::Item`]) |
/// | 26–29 | accessory (16): 0 none, 1 cap, 2 helmet, 3 mask, 4 backpack, 5 scarf, 6 hi-vis vest, 7 press vest |
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Appearance(pub u32);

impl Appearance {
    pub fn new(
        archetype: u8,
        body: u8,
        skin: u8,
        clothes: u8,
        hair: u8,
        hair_colour: u8,
        item: u8,
        accessory: u8,
    ) -> Self {
        Appearance(
            (archetype as u32 & 15)
                | ((body as u32 & 3) << 4)
                | ((skin as u32 & 7) << 6)
                | ((clothes as u32 & 63) << 9)
                | ((hair as u32 & 15) << 15)
                | ((hair_colour as u32 & 7) << 19)
                | ((item as u32 & 15) << 22)
                | ((accessory as u32 & 15) << 26),
        )
    }
    #[inline]
    pub fn archetype(self) -> u8 {
        (self.0 & 15) as u8
    }
    #[inline]
    pub fn body(self) -> u8 {
        ((self.0 >> 4) & 3) as u8
    }
    #[inline]
    pub fn skin(self) -> u8 {
        ((self.0 >> 6) & 7) as u8
    }
    #[inline]
    pub fn clothes(self) -> u8 {
        ((self.0 >> 9) & 63) as u8
    }
    #[inline]
    pub fn hair(self) -> u8 {
        ((self.0 >> 15) & 15) as u8
    }
    #[inline]
    pub fn hair_colour(self) -> u8 {
        ((self.0 >> 19) & 7) as u8
    }
    #[inline]
    pub fn item(self) -> u8 {
        ((self.0 >> 22) & 15) as u8
    }
    #[inline]
    pub fn accessory(self) -> u8 {
        ((self.0 >> 26) & 15) as u8
    }
    #[inline]
    pub fn with_item(self, item: u8) -> Self {
        Appearance((self.0 & !(15 << 22)) | ((item as u32 & 15) << 22))
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
#[repr(C)]
pub struct SnapVehicle {
    pub x: f32,
    pub y: f32,
    /// Half length and half width (m).
    pub half_len: f32,
    pub half_wid: f32,
    pub heading: u8,
    /// 0 = police van, 1 = water cannon, 2 = ambulance.
    pub kind: u8,
    /// 1 = moving, 2 = spraying, 4 = overturned.
    pub flags: u8,
    pub _pad: u8,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
#[repr(C)]
pub struct SnapProjectile {
    pub x: f32,
    pub y: f32,
    /// 0 = stone/bottle, 1 = molotov, 2 = gas canister.
    pub kind: u8,
    pub _pad: [u8; 3],
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
#[repr(C)]
pub struct SnapProp {
    pub x: f32,
    pub y: f32,
    pub half_x: f32,
    pub half_y: f32,
    /// Rotation as a unit vector.
    pub cos: f32,
    pub sin: f32,
    /// [`riot_defs::mapdef::PropKind`] as `u8`.
    pub kind: u8,
    /// 0..=255 remaining structure.
    pub health: u8,
    /// 1 = burning, 2 = under construction, 4 = destroyed, 8 = overturned.
    pub flags: u8,
    pub _pad: u8,
}

/// Bits of a prop's flags, in the simulation and in [`SnapProp::flags`].
pub mod prop_flag {
    pub const BURNING: u8 = 1;
    pub const BUILDING: u8 = 2;
    pub const DESTROYED: u8 = 4;
    pub const OVERTURNED: u8 = 8;
    /// Gone from the map (police barriers taken down): not drawn.
    pub const REMOVED: u8 = 16;
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SnapObjective {
    pub slot: u8,
    pub active: bool,
    /// [`riot_defs::setup::ObjectiveKind`] as `u8`.
    pub kind: u8,
    pub x: f32,
    pub y: f32,
    pub radius: f32,
    /// Held by protesters right now.
    pub held: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SnapDrop {
    pub x: f32,
    pub y: f32,
    /// [`riot_defs::setup::SupplyKind`] as `u8`.
    pub kind: u8,
    pub amount: u16,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SnapSquad {
    pub x: f32,
    pub y: f32,
    /// Formation target.
    pub tx: f32,
    pub ty: f32,
    /// [`riot_defs::police::Doctrine`] as `u8`.
    pub doctrine: u8,
    /// [`riot_defs::setup::SquadKind`] as `u8`.
    pub kind: u8,
    pub officers: u8,
    pub morale: u8,
}

/// Points kept of a crowd's predicted route.
pub const SNAP_ROUTE: usize = 16;

/// A crowd the police track, what they make of it and where they think it
/// is going (DESIGN §14).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SnapTrack {
    pub x: f32,
    pub y: f32,
    pub count: f32,
    pub radius: f32,
    /// [`riot_defs::police::OpKind`] as `u8`.
    pub op: u8,
    /// Recent violence around it, saturating at 255.
    pub heat: u8,
    /// Threatens a protected site.
    pub site: bool,
    /// Seconds until it reaches the site, or infinity.
    pub eta: f32,
    pub route_n: u8,
    pub route: [[f32; 2]; SNAP_ROUTE],
}

/// A police barricade across a street (DESIGN §14).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SnapCordon {
    /// Centre of the barrier line.
    pub x: f32,
    pub y: f32,
    /// Along the line, and toward the crowd.
    pub axis: [f32; 2],
    pub face: [f32; 2],
    pub half: f32,
    /// [`riot_defs::police::CordonState`] as `u8`.
    pub state: u8,
    /// 0 none, 1 a squad waits in ambush, 2 sprung.
    pub ambush: u8,
    /// Where the ambush waits, and where its side street meets the route.
    pub ambush_at: [f32; 2],
    pub junction: [f32; 2],
}

/// Field layers the renderer can draw (DESIGN §18.2 debug overlays).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Overlay {
    #[default]
    None = 0,
    Density = 1,
    Fear = 2,
    Anger = 3,
    Threat = 4,
    Gas = 5,
    Messages = 6,
    Camera = 7,
    Pressure = 8,
}

impl Overlay {
    pub const ALL: [Overlay; 9] = [
        Overlay::None,
        Overlay::Density,
        Overlay::Fear,
        Overlay::Anger,
        Overlay::Threat,
        Overlay::Gas,
        Overlay::Messages,
        Overlay::Camera,
        Overlay::Pressure,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Overlay::None => "none",
            Overlay::Density => "density",
            Overlay::Fear => "fear",
            Overlay::Anger => "anger",
            Overlay::Threat => "threat",
            Overlay::Gas => "gas",
            Overlay::Messages => "messages",
            Overlay::Camera => "camera",
            Overlay::Pressure => "pressure",
        }
    }
}

/// Active field chunks, quantized to `u8` per 2 m cell.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FieldSnap {
    /// Tick the layers were captured.
    pub tick: u32,
    /// Cells per chunk side (64) and cell size in metres (2).
    pub chunk_cells: u32,
    pub cell: f32,
    /// Half the field grid's size: chunk (cx, cy) covers
    /// `x ∈ [cx·128 − half, (cx+1)·128 − half)`.
    pub half: f32,
    /// Chunk coordinates, one per captured chunk.
    pub chunks: Vec<[u16; 2]>,
    /// Row-major cells of each chunk, `chunk_cells²` bytes per chunk, in
    /// `chunks` order. A layer with nothing in it anywhere is left empty
    /// (length 0) instead: use [`FieldSnap::layer`].
    pub gas: Vec<u8>,
    pub smoke: Vec<u8>,
    pub fire: Vec<u8>,
    pub overlay: Vec<u8>,
    pub overlay_kind: Overlay,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SnapStats {
    pub tick: u32,
    pub present: u32,
    pub protesters: u32,
    pub officers: u32,
    pub asleep: u32,
    pub arrests: u32,
    pub injuries: u32,
    pub deaths: u32,
    pub went_home: u32,
    pub mean_fear: f32,
    pub mean_anger: f32,
    pub mean_morale: f32,
    /// [`riot_defs::setup::Escalation`] as `u8`.
    pub escalation: u8,
    pub messages: u32,
    pub movement_blame: f32,
    pub police_blame: f32,
    /// Minute of the day.
    pub clock_minute: f32,
    pub internet: bool,
    /// The police radio: the latest calls, `radio[(radio_n - 1) % RADIO]`
    /// last ([`riot_defs::police::call_text`] says what each means).
    pub radio: [riot_defs::police::RadioCall; riot_defs::police::RADIO],
    pub radio_n: u32,
}

/// Everything the renderer draws for one tick.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub tick: u32,
    /// Indexed by `stable_id`; `people.len()` is every stable id issued.
    pub people: Vec<SnapPerson>,
    pub vehicles: Vec<SnapVehicle>,
    pub projectiles: Vec<SnapProjectile>,
    pub props: Vec<SnapProp>,
    pub objectives: Vec<SnapObjective>,
    pub drops: Vec<SnapDrop>,
    pub squads: Vec<SnapSquad>,
    pub tracks: Vec<SnapTrack>,
    pub cordons: Vec<SnapCordon>,
    pub fields: FieldSnap,
    pub stats: SnapStats,
}

impl FieldSnap {
    /// The cells of chunk number `k` (in `chunks` order) of a layer, or
    /// `None` when the layer is empty.
    pub fn layer<'a>(&self, layer: &'a [u8], k: usize) -> Option<&'a [u8]> {
        let n = (self.chunk_cells * self.chunk_cells) as usize;
        layer.get(k * n..(k + 1) * n)
    }
}

/// The most a riot can put in each part of a [`Snapshot`].
#[derive(Clone, Copy, Debug, Default)]
pub struct SnapshotSizes {
    pub people: usize,
    pub vehicles: usize,
    pub projectiles: usize,
    pub props: usize,
    pub objectives: usize,
    pub drops: usize,
    pub squads: usize,
    pub tracks: usize,
    pub cordons: usize,
    /// Field cells in all chunks that can be active at once.
    pub field_cells: usize,
    pub field_chunks: usize,
}

impl Snapshot {
    /// Reserve every buffer for the most it can hold, so writing (and
    /// copying) snapshots never allocates once the riot is running.
    pub fn reserve(&mut self, n: &SnapshotSizes) {
        fn to<T>(v: &mut Vec<T>, n: usize) {
            v.reserve_exact(n.saturating_sub(v.len()));
        }
        to(&mut self.people, n.people);
        to(&mut self.vehicles, n.vehicles);
        to(&mut self.projectiles, n.projectiles);
        to(&mut self.props, n.props);
        to(&mut self.objectives, n.objectives);
        to(&mut self.drops, n.drops);
        to(&mut self.squads, n.squads);
        to(&mut self.tracks, n.tracks);
        to(&mut self.cordons, n.cordons);
        let f = &mut self.fields;
        to(&mut f.chunks, n.field_chunks);
        to(&mut f.gas, n.field_cells);
        to(&mut f.smoke, n.field_cells);
        to(&mut f.fire, n.field_cells);
        to(&mut f.overlay, n.field_cells);
    }

    /// Copy `other` into `self`, reusing buffers (for triple buffering).
    pub fn copy_from(&mut self, other: &Snapshot) {
        self.tick = other.tick;
        copy_vec(&mut self.people, &other.people);
        copy_vec(&mut self.vehicles, &other.vehicles);
        copy_vec(&mut self.projectiles, &other.projectiles);
        copy_vec(&mut self.props, &other.props);
        copy_vec(&mut self.objectives, &other.objectives);
        copy_vec(&mut self.drops, &other.drops);
        copy_vec(&mut self.squads, &other.squads);
        copy_vec(&mut self.tracks, &other.tracks);
        copy_vec(&mut self.cordons, &other.cordons);
        let (f, o) = (&mut self.fields, &other.fields);
        f.tick = o.tick;
        f.chunk_cells = o.chunk_cells;
        f.cell = o.cell;
        f.half = o.half;
        f.overlay_kind = o.overlay_kind;
        copy_vec(&mut f.chunks, &o.chunks);
        copy_vec(&mut f.gas, &o.gas);
        copy_vec(&mut f.smoke, &o.smoke);
        copy_vec(&mut f.fire, &o.fire);
        copy_vec(&mut f.overlay, &o.overlay);
        self.stats = other.stats;
    }
}

fn copy_vec<T: Copy>(dst: &mut Vec<T>, src: &[T]) {
    dst.clear();
    dst.extend_from_slice(src);
}
