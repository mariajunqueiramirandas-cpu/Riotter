//! Render snapshot (DESIGN §18.1). The renderer never reads `World`; it reads
//! these. People are written in `stable_id` order, so snapshot slot *n* is
//! always the same person and the renderer can interpolate between two
//! snapshots even though the simulation reorders agents.

use bytemuck::{Pod, Zeroable};

/// 16 bytes per person.
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
#[repr(C)]
pub struct SnapPerson {
    pub x: f32,
    pub y: f32,
    /// 256 steps of about 1.4°; 0 is +x, 64 is +y.
    pub heading: u8,
    /// [`crate::state::Gait`] as `u8`.
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
/// | 22–25 | held item ([`crate::state::Item`]) |
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
    /// [`crate::mapdef::PropKind`] as `u8`.
    pub kind: u8,
    /// 0..=255 remaining structure.
    pub health: u8,
    /// 1 = burning, 2 = under construction, 4 = destroyed, 8 = overturned.
    pub flags: u8,
    pub _pad: u8,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SnapObjective {
    pub slot: u8,
    pub active: bool,
    /// [`crate::setup::ObjectiveKind`] as `u8`.
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
    /// [`crate::setup::SupplyKind`] as `u8`.
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
    /// [`crate::police::Doctrine`] as `u8`.
    pub doctrine: u8,
    /// [`crate::setup::SquadKind`] as `u8`.
    pub kind: u8,
    pub officers: u8,
    pub morale: u8,
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
    /// [`crate::setup::Escalation`] as `u8`.
    pub escalation: u8,
    pub messages: u32,
    pub movement_blame: f32,
    pub police_blame: f32,
    /// Minute of the day.
    pub clock_minute: f32,
    pub internet: bool,
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

impl Snapshot {
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
