//! The map as authored (DESIGN §2.1), before any lookup structures are
//! derived from it. `riot_data` fills this from a Tiled JSON map.
//!
//! Coordinates are `f32` metres measured from the **centre of the map**, with
//! +x east and +y south (down on screen, like Tiled). The map covers
//! `x ∈ [-width/2, width/2]`, `y ∈ [-height/2, height/2]`.

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MapDef {
    pub name: String,
    /// Extent in metres. At most 8192 × 8192.
    pub width: f32,
    pub height: f32,
    /// Solid polygons: the exact collision shapes (DESIGN §11.2). Closed
    /// (the last point connects back to the first), any winding, no
    /// self-intersections.
    pub solids: Vec<Polygon>,
    /// Walkable underneath, drawn above people: arcades, tree canopies,
    /// bridges over roads. Render only.
    pub overhead: Vec<Polygon>,
    pub props: Vec<PropDef>,
    pub ground: GroundLayer,
    /// Roads that vehicles drive on.
    pub roads: RoadGraph,
    /// Where people arrive and leave: map edges, metro stations, bus stops.
    pub entries: Vec<EntryDef>,
    /// Police depots: squads, vans and water cannon trucks start here.
    pub depots: Vec<NamedPoint>,
    /// Police staging points.
    pub staging: Vec<NamedPoint>,
    /// Named areas that objectives can refer to: squares, bridges, the
    /// government quarter.
    pub areas: Vec<AreaDef>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum SolidKind {
    #[default]
    Building = 0,
    Wall = 1,
    Water = 2,
    Fence = 3,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Polygon {
    pub kind: SolidKind,
    pub points: Vec<[f32; 2]>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PropKind {
    /// Parked car: burnable, can be rocked.
    #[default]
    Car = 0,
    Bin = 1,
    /// Crowd-control barrier.
    Barrier = 2,
    Bench = 3,
    /// Built by protesters during the riot.
    Barricade = 4,
    /// Steel barriers the police put across a street.
    PoliceBarrier = 5,
}

impl PropKind {
    pub fn from_u8(x: u8) -> PropKind {
        match x {
            0 => PropKind::Car,
            1 => PropKind::Bin,
            2 => PropKind::Barrier,
            3 => PropKind::Bench,
            5 => PropKind::PoliceBarrier,
            _ => PropKind::Barricade,
        }
    }
}

/// An oriented box.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PropDef {
    pub kind: PropKind,
    pub pos: [f32; 2],
    /// Half extents along the prop's own axes.
    pub half: [f32; 2],
    /// Rotation in radians, clockwise on screen (from +x towards +y).
    pub angle: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Ground {
    #[default]
    Pavement = 0,
    Road = 1,
    Grass = 2,
    Plaza = 3,
    Water = 4,
}

/// Painted ground types. Row-major, `kinds[y * w + x]`, cell (0, 0) at the
/// map's top-left corner `(-width/2, -height/2)`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GroundLayer {
    /// Cell size in metres.
    pub cell: f32,
    pub w: u32,
    pub h: u32,
    pub kinds: Vec<u8>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RoadGraph {
    pub nodes: Vec<[f32; 2]>,
    /// Undirected edges between node indices.
    pub edges: Vec<[u32; 2]>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum EntryKind {
    #[default]
    MapEdge = 0,
    Metro = 1,
    Bus = 2,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EntryDef {
    pub name: String,
    pub kind: EntryKind,
    pub pos: [f32; 2],
    pub radius: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct NamedPoint {
    pub name: String,
    pub pos: [f32; 2],
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AreaDef {
    pub name: String,
    /// Free-form tag: "square", "bridge", "government", "park", ...
    pub tag: String,
    pub pos: [f32; 2],
    pub radius: f32,
}

impl MapDef {
    pub fn entry(&self, name: &str) -> Option<usize> {
        self.entries.iter().position(|e| e.name == name)
    }
    pub fn depot(&self, name: &str) -> Option<usize> {
        self.depots.iter().position(|e| e.name == name)
    }
    pub fn staging_point(&self, name: &str) -> Option<usize> {
        self.staging.iter().position(|e| e.name == name)
    }
    pub fn area(&self, name: &str) -> Option<&AreaDef> {
        self.areas.iter().find(|e| e.name == name)
    }

    /// Ground type at a point (pavement outside the layer).
    pub fn ground_at(&self, x: f32, y: f32) -> Ground {
        let g = &self.ground;
        if g.cell <= 0.0 || g.kinds.is_empty() {
            return Ground::Pavement;
        }
        let gx = ((x + self.width * 0.5) / g.cell).floor();
        let gy = ((y + self.height * 0.5) / g.cell).floor();
        if gx < 0.0 || gy < 0.0 || gx >= g.w as f32 || gy >= g.h as f32 {
            return Ground::Pavement;
        }
        match g.kinds[gy as usize * g.w as usize + gx as usize] {
            1 => Ground::Road,
            2 => Ground::Grass,
            3 => Ground::Plaza,
            4 => Ground::Water,
            _ => Ground::Pavement,
        }
    }
}
