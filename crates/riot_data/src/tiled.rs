//! Tiled JSON maps → [`MapDef`] (DESIGN §2.1).
//!
//! The authoring convention is documented in `data/maps/README.md`. In short:
//! an orthogonal map exported as JSON, a map property `metres_per_pixel`
//! (default 0.25), object layers named `solids`, `overhead`, `props`, `roads`,
//! `entries`, `depots`, `staging` and `areas`, and a tile layer named `ground`
//! whose tiles carry a `ground` property. Layer names are case-insensitive,
//! groups are walked recursively, and other layers are ignored.
//!
//! Tiled measures pixels from the top-left corner with y down; a `MapDef`
//! measures metres from the centre of the map with y down, so
//! `x_m = x_px · mpp − width_m / 2`.

use std::path::Path;

use riot_sim::mapdef::{
    AreaDef, EntryDef, EntryKind, Ground, GroundLayer, MapDef, NamedPoint, Polygon, PropDef,
    PropKind, RoadGraph, SolidKind,
};
use serde::de::{self, IgnoredAny, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{DataError, Result};
use crate::math::{seg_dist2, sin_cos_deg};

/// Metres per Tiled pixel when the map has no `metres_per_pixel` property.
pub const DEFAULT_METRES_PER_PIXEL: f64 = 0.25;
/// Largest map side in metres (`riot_sim::map::LARGEST_MAP`).
pub const LARGEST_MAP_M: f64 = 8192.0;
/// Road vertices closer than this (metres) become one graph node.
pub const ROAD_MERGE_M: f32 = 1.0;
/// Tiled's flip and rotation flags in the top bits of a global tile id.
const GID_FLAGS: u32 = 0xF000_0000;

// ---------------------------------------------------------------------------
// The file format. Shared with `citygen`, which writes it: every optional
// field is skipped when empty, so written maps stay small.

/// A Tiled map as JSON (only what Riotter reads or writes).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct TiledMap {
    #[serde(rename = "type")]
    pub kind: String,
    /// Tiled writes these as strings now and as numbers in old versions.
    pub version: serde_json::Value,
    pub tiledversion: serde_json::Value,
    pub orientation: String,
    pub renderorder: String,
    pub infinite: bool,
    pub compressionlevel: i32,
    /// In tiles.
    pub width: u32,
    pub height: u32,
    /// In pixels.
    pub tilewidth: u32,
    pub tileheight: u32,
    pub nextlayerid: u32,
    pub nextobjectid: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub properties: Vec<Property>,
    pub tilesets: Vec<Tileset>,
    pub layers: Vec<Layer>,
}

impl Default for TiledMap {
    fn default() -> Self {
        TiledMap {
            kind: "map".into(),
            version: "1.10".into(),
            tiledversion: "1.11.0".into(),
            orientation: "orthogonal".into(),
            renderorder: "right-down".into(),
            infinite: false,
            compressionlevel: -1,
            width: 0,
            height: 0,
            tilewidth: 0,
            tileheight: 0,
            nextlayerid: 1,
            nextobjectid: 1,
            properties: Vec::new(),
            tilesets: Vec::new(),
            layers: Vec::new(),
        }
    }
}

/// A custom property (`{"name", "type", "value"}`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Property {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub propertytype: String,
    pub value: serde_json::Value,
}

impl Property {
    pub fn float(name: &str, v: f64) -> Self {
        Property {
            name: name.into(),
            kind: "float".into(),
            value: v.into(),
            ..Default::default()
        }
    }
    pub fn int(name: &str, v: i64) -> Self {
        Property {
            name: name.into(),
            kind: "int".into(),
            value: v.into(),
            ..Default::default()
        }
    }
    pub fn string(name: &str, v: &str) -> Self {
        Property {
            name: name.into(),
            kind: "string".into(),
            value: v.into(),
            ..Default::default()
        }
    }
}

/// An embedded tileset, or a reference to an external one (`source`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Tileset {
    pub firstgid: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub tilewidth: u32,
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub tileheight: u32,
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub tilecount: u32,
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub columns: u32,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub image: String,
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub imagewidth: u32,
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub imageheight: u32,
    pub margin: u32,
    pub spacing: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tiles: Vec<TileDef>,
}

/// Per-tile data in a tileset.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct TileDef {
    pub id: u32,
    #[serde(rename = "type", skip_serializing_if = "String::is_empty")]
    pub kind: String,
    /// Tiled 1.9 wrote `class` instead of `type`.
    #[serde(skip_serializing)]
    pub class: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub properties: Vec<Property>,
}

/// Any layer: `tilelayer`, `objectgroup`, `group` or `imagelayer`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Layer {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: u32,
    pub name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub class: String,
    pub visible: bool,
    pub opacity: f64,
    #[serde(serialize_with = "ser_num")]
    pub x: f64,
    #[serde(serialize_with = "ser_num")]
    pub y: f64,
    #[serde(skip_serializing_if = "is_zero_f64", serialize_with = "ser_num")]
    pub offsetx: f64,
    #[serde(skip_serializing_if = "is_zero_f64", serialize_with = "ser_num")]
    pub offsety: f64,
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub width: u32,
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub height: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<TileData>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub encoding: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub compression: String,
    /// Infinite maps store tiles in chunks, which Riotter does not read.
    #[serde(skip_serializing)]
    pub chunks: Option<IgnoredAny>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub draworder: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub objects: Vec<Object>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<Layer>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub properties: Vec<Property>,
}

impl Default for Layer {
    fn default() -> Self {
        Layer {
            kind: String::new(),
            id: 0,
            name: String::new(),
            class: String::new(),
            visible: true,
            opacity: 1.0,
            x: 0.0,
            y: 0.0,
            offsetx: 0.0,
            offsety: 0.0,
            width: 0,
            height: 0,
            data: None,
            encoding: String::new(),
            compression: String::new(),
            chunks: None,
            draworder: String::new(),
            objects: Vec::new(),
            layers: Vec::new(),
            properties: Vec::new(),
        }
    }
}

/// A map object. Its shape is a rectangle unless `point`, `ellipse`,
/// `polygon` or `polyline` says otherwise; a `gid` makes it a tile object.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Object {
    pub id: u32,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(rename = "type", skip_serializing_if = "String::is_empty")]
    pub kind: String,
    /// Tiled 1.9 wrote `class` instead of `type`.
    #[serde(skip_serializing)]
    pub class: String,
    #[serde(serialize_with = "ser_num")]
    pub x: f64,
    #[serde(serialize_with = "ser_num")]
    pub y: f64,
    #[serde(skip_serializing_if = "is_zero_f64", serialize_with = "ser_num")]
    pub width: f64,
    #[serde(skip_serializing_if = "is_zero_f64", serialize_with = "ser_num")]
    pub height: f64,
    /// Degrees, clockwise, about (`x`, `y`).
    #[serde(skip_serializing_if = "is_zero_f64", serialize_with = "ser_num")]
    pub rotation: f64,
    #[serde(skip_serializing_if = "is_false")]
    pub point: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub ellipse: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub polygon: Option<Vec<Pt>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub polyline: Option<Vec<Pt>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gid: Option<u32>,
    #[serde(skip_serializing)]
    pub text: Option<IgnoredAny>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub properties: Vec<Property>,
}

impl Object {
    /// The object's type, or its class in Tiled 1.9 files.
    fn type_name(&self) -> &str {
        if self.kind.is_empty() {
            self.class.trim()
        } else {
            self.kind.trim()
        }
    }
}

/// A polygon or polyline vertex, relative to its object.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Pt {
    #[serde(serialize_with = "ser_num")]
    pub x: f64,
    #[serde(serialize_with = "ser_num")]
    pub y: f64,
}

/// Tile layer data: a plain array of global tile ids, or a base64 string.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum TileData {
    Gids(Vec<u32>),
    Encoded(String),
}

impl Serialize for TileData {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            TileData::Gids(v) => v.serialize(s),
            TileData::Encoded(e) => s.serialize_str(e),
        }
    }
}

impl<'de> Deserialize<'de> for TileData {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = TileData;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an array of tile ids or a base64 string")
            }
            fn visit_str<E: de::Error>(self, s: &str) -> std::result::Result<TileData, E> {
                Ok(TileData::Encoded(s.to_string()))
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<TileData, A::Error> {
                let mut v = Vec::with_capacity(seq.size_hint().unwrap_or(0));
                while let Some(g) = seq.next_element::<u32>()? {
                    v.push(g);
                }
                Ok(TileData::Gids(v))
            }
        }
        d.deserialize_any(V)
    }
}

/// Writes integral values without a fraction (`12`, not `12.0`).
pub(crate) fn ser_num<S: Serializer>(v: &f64, s: S) -> std::result::Result<S::Ok, S::Error> {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        s.serialize_i64(*v as i64)
    } else {
        s.serialize_f64(*v)
    }
}

fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}
fn is_zero_f64(v: &f64) -> bool {
    *v == 0.0
}
fn is_false(v: &bool) -> bool {
    !*v
}

// ---------------------------------------------------------------------------
// Tile data codecs

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding.
pub(crate) fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for k in 0..4 {
            if k <= c.len() {
                out.push(B64[(n >> (18 - 6 * k) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Standard base64; whitespace is skipped, padding is optional.
pub(crate) fn base64_decode(s: &str) -> std::result::Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for (i, ch) in s.bytes().enumerate() {
        let v = match ch {
            b'A'..=b'Z' => ch - b'A',
            b'a'..=b'z' => ch - b'a' + 26,
            b'0'..=b'9' => ch - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return Err(format!("invalid base64 character at offset {i}")),
        };
        acc = acc << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Ok(out)
}

/// Encodes global tile ids the way Tiled does: little-endian u32s, zlib
/// compressed when `compress`, base64 (returns data, encoding, compression).
pub(crate) fn encode_gids(gids: &[u32], compress: bool) -> (TileData, String, String) {
    if !compress {
        return (TileData::Gids(gids.to_vec()), String::new(), String::new());
    }
    let bytes: Vec<u8> = gids.iter().flat_map(|g| g.to_le_bytes()).collect();
    let z = miniz_oxide::deflate::compress_to_vec_zlib(&bytes, 6);
    (
        TileData::Encoded(base64_encode(&z)),
        "base64".into(),
        "zlib".into(),
    )
}

/// The global tile ids of a tile layer, `count` of them.
fn decode_gids(layer: &Layer, count: usize) -> Result<Vec<u32>> {
    let gids = match (&layer.data, layer.encoding.as_str()) {
        (None, _) if layer.chunks.is_some() => {
            return Err(DataError::new(
                "has chunks: infinite maps are not supported (uncheck Map Properties → Infinite)",
            ));
        }
        (None, _) => return Err(DataError::new("has no tile data")),
        (Some(TileData::Gids(v)), "" | "csv") => v.clone(),
        (Some(TileData::Encoded(s)), "base64") => {
            let raw = base64_decode(s).map_err(DataError::new)?;
            let limit = count * 4 + 64;
            let bytes = match layer.compression.as_str() {
                "" => raw,
                "zlib" => miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&raw, limit)
                    .map_err(|e| DataError::new(format!("bad zlib data: {e:?}")))?,
                "gzip" => {
                    miniz_oxide::inflate::decompress_to_vec_with_limit(gzip_body(&raw)?, limit)
                        .map_err(|e| DataError::new(format!("bad gzip data: {e:?}")))?
                }
                other => {
                    return Err(DataError::new(format!(
                        "unsupported tile layer compression `{other}` (use zlib, gzip or none)"
                    )));
                }
            };
            if bytes.len() != count * 4 {
                return Err(DataError::new(format!(
                    "tile data has {} bytes, expected {} for {count} tiles",
                    bytes.len(),
                    count * 4
                )));
            }
            bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|&b| u32::from_le_bytes(b))
                .collect()
        }
        (Some(_), enc) => {
            return Err(DataError::new(format!(
                "unsupported tile layer encoding `{enc}` (use CSV or base64)"
            )));
        }
    };
    if gids.len() != count {
        return Err(DataError::new(format!(
            "has {} tiles, expected {count}",
            gids.len()
        )));
    }
    Ok(gids)
}

/// The raw deflate stream inside a gzip member (RFC 1952).
fn gzip_body(b: &[u8]) -> Result<&[u8]> {
    let bad = || DataError::new("bad gzip header");
    if b.len() < 18 || b[0] != 0x1f || b[1] != 0x8b || b[2] != 8 {
        return Err(bad());
    }
    let flags = b[3];
    let mut at = 10;
    if flags & 4 != 0 {
        let xlen =
            *b.get(at).ok_or_else(bad)? as usize | (*b.get(at + 1).ok_or_else(bad)? as usize) << 8;
        at += 2 + xlen;
    }
    for bit in [8u8, 16] {
        if flags & bit != 0 {
            at += b
                .get(at..)
                .ok_or_else(bad)?
                .iter()
                .position(|&c| c == 0)
                .ok_or_else(bad)?
                + 1;
        }
    }
    if flags & 2 != 0 {
        at += 2;
    }
    b.get(at..b.len().saturating_sub(8)).ok_or_else(bad)
}

// ---------------------------------------------------------------------------
// Loading

/// Loads a Tiled JSON map. The map is named after the file stem
/// (`data/maps/district.json` → `district`). External JSON tilesets
/// (`.tsj`) are resolved relative to the map.
pub fn load_map(path: &Path) -> Result<MapDef> {
    let text = std::fs::read_to_string(path).map_err(|e| DataError::io(path, e))?;
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    parse_in(&text, &name, path.parent()).map_err(|e| e.in_file(path))
}

/// Parses a Tiled JSON map from a string. External tilesets cannot be
/// resolved without a directory; use [`load_map`] for those.
pub fn parse_map(json: &str, name: &str) -> Result<MapDef> {
    parse_in(json, name, None)
}

fn parse_in(json: &str, name: &str, dir: Option<&Path>) -> Result<MapDef> {
    let map: TiledMap = serde_json::from_str(json)
        .map_err(|e| DataError::caused_by(format!("not a valid Tiled JSON map: {e}"), e))?;
    convert(&map, name, dir)
}

/// Converts a parsed Tiled map. `dir` resolves external tilesets.
pub(crate) fn convert(map: &TiledMap, name: &str, dir: Option<&Path>) -> Result<MapDef> {
    if !map.orientation.is_empty() && map.orientation != "orthogonal" {
        return Err(DataError::new(format!(
            "{} maps are not supported; use an orthogonal map",
            map.orientation
        )));
    }
    if map.infinite {
        return Err(DataError::new(
            "infinite maps are not supported (uncheck Map Properties → Infinite)",
        ));
    }
    if map.width == 0 || map.height == 0 || map.tilewidth == 0 || map.tileheight == 0 {
        return Err(DataError::new(
            "map has no size (width, height, tilewidth and tileheight must be set)",
        ));
    }
    let mpp =
        match prop_f64(&map.properties, "metres_per_pixel").map_err(|e| e.at("map property"))? {
            Some(v) if v > 0.0 && v.is_finite() => v,
            Some(v) => {
                return Err(DataError::new(format!(
                    "map property metres_per_pixel must be positive, not {v}"
                )));
            }
            None => DEFAULT_METRES_PER_PIXEL,
        };
    let width = map.width as f64 * map.tilewidth as f64 * mpp;
    let height = map.height as f64 * map.tileheight as f64 * mpp;
    if width.max(height) > LARGEST_MAP_M {
        return Err(DataError::new(format!(
            "map is {width} × {height} m; the largest supported is {LARGEST_MAP_M} m across \
             (check metres_per_pixel)"
        )));
    }
    let mut c = Conv {
        map,
        dir,
        mpp,
        half: [width * 0.5, height * 0.5],
        tilesets: None,
        roads: RoadBuilder::default(),
        def: MapDef {
            name: name.to_string(),
            width: width as f32,
            height: height as f32,
            ..Default::default()
        },
    };
    c.walk(&map.layers, [0.0, 0.0])?;
    let roads = std::mem::take(&mut c.roads).finish();
    c.def.roads = roads;
    check_unique("entry", c.def.entries.iter().map(|e| &e.name))?;
    check_unique("depot", c.def.depots.iter().map(|e| &e.name))?;
    check_unique("staging point", c.def.staging.iter().map(|e| &e.name))?;
    check_unique("area", c.def.areas.iter().map(|e| &e.name))?;
    Ok(c.def)
}

fn check_unique<'a>(what: &str, names: impl Iterator<Item = &'a String>) -> Result<()> {
    let mut v: Vec<&String> = names.collect();
    v.sort();
    for w in v.windows(2) {
        if w[0] == w[1] {
            return Err(DataError::new(format!(
                "two {what}s are named \"{}\"",
                w[0]
            )));
        }
    }
    Ok(())
}

/// A tileset ready for lookups: `firstgid` and the ground kind per local id.
struct GroundTiles {
    firstgid: u32,
    kinds: Vec<(u32, u8)>,
}

struct Conv<'a> {
    map: &'a TiledMap,
    dir: Option<&'a Path>,
    mpp: f64,
    half: [f64; 2],
    /// Resolved on first use, sorted by `firstgid`.
    tilesets: Option<Vec<GroundTiles>>,
    roads: RoadBuilder,
    def: MapDef,
}

/// The object layers Riotter reads.
const OBJECT_LAYERS: [&str; 8] = [
    "solids", "overhead", "props", "roads", "entries", "depots", "staging", "areas",
];

impl Conv<'_> {
    fn walk(&mut self, layers: &[Layer], off: [f64; 2]) -> Result<()> {
        for layer in layers {
            let off = [off[0] + layer.offsetx, off[1] + layer.offsety];
            let name = layer.name.trim().to_ascii_lowercase();
            let ctx = || format!("layer \"{}\"", layer.name);
            match layer.kind.as_str() {
                "group" => self.walk(&layer.layers, off).map_err(|e| e.at(ctx()))?,
                "tilelayer" if name == "ground" => {
                    self.ground(layer, off).map_err(|e| e.at(ctx()))?
                }
                "tilelayer" if OBJECT_LAYERS.contains(&name.as_str()) => {
                    return Err(
                        DataError::new("must be an object layer, not a tile layer").at(ctx())
                    );
                }
                "objectgroup" if name == "ground" => {
                    return Err(
                        DataError::new("must be a tile layer, not an object layer").at(ctx())
                    );
                }
                "objectgroup" => {
                    for o in &layer.objects {
                        if o.text.is_some() {
                            continue; // annotations
                        }
                        self.object(&name, o, off).map_err(|e| {
                            let who = if o.name.is_empty() {
                                format!("object {}", o.id)
                            } else {
                                format!("object {} (\"{}\")", o.id, o.name)
                            };
                            e.at(who).at(ctx())
                        })?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Map pixels (layer offset included) → metres from the map centre.
    fn m(&self, p: [f64; 2]) -> [f32; 2] {
        [
            (p[0] * self.mpp - self.half[0]) as f32,
            (p[1] * self.mpp - self.half[1]) as f32,
        ]
    }

    fn object(&mut self, layer: &str, o: &Object, off: [f64; 2]) -> Result<()> {
        if o.template.is_some() {
            return Err(DataError::new(
                "object templates are not supported (Detach the template in Tiled)",
            ));
        }
        let ty = o.type_name().to_ascii_lowercase();
        match layer {
            "solids" => {
                let kind = match ty.as_str() {
                    "" | "building" => SolidKind::Building,
                    "wall" => SolidKind::Wall,
                    "water" => SolidKind::Water,
                    "fence" => SolidKind::Fence,
                    other => {
                        return Err(DataError::new(format!(
                            "unknown solid type \"{other}\" (expected building, wall, water or fence)"
                        )));
                    }
                };
                let points = self.outline(o, off, 24)?;
                self.def.solids.push(Polygon { kind, points });
            }
            "overhead" => {
                // Render only: the kind is a hint (canopy, arcade, bridge → building).
                let kind = match ty.as_str() {
                    "wall" => SolidKind::Wall,
                    "water" => SolidKind::Water,
                    "fence" => SolidKind::Fence,
                    _ => SolidKind::Building,
                };
                let points = self.outline(o, off, 16)?;
                self.def.overhead.push(Polygon { kind, points });
            }
            "props" => {
                let prop = self.prop(o, off, &ty)?;
                self.def.props.push(prop);
            }
            "roads" => {
                let (pts, closed) = match (&o.polyline, &o.polygon) {
                    (Some(p), _) => (p, false),
                    (None, Some(p)) => (p, true),
                    _ => return Err(DataError::new("roads must be polylines")),
                };
                let (s, c) = sin_cos_deg(o.rotation);
                let pts: Vec<[f32; 2]> = pts
                    .iter()
                    .map(|p| self.m(place(o, off, s, c, p.x, p.y)))
                    .collect();
                self.roads.polyline(&pts, closed);
            }
            "entries" => {
                let kind = match ty.as_str() {
                    "" | "map_edge" | "edge" => EntryKind::MapEdge,
                    "metro" => EntryKind::Metro,
                    "bus" => EntryKind::Bus,
                    other => {
                        return Err(DataError::new(format!(
                            "unknown entry type \"{other}\" (expected map_edge, metro or bus)"
                        )));
                    }
                };
                let default_r = match kind {
                    EntryKind::MapEdge => 6.0,
                    EntryKind::Metro => 4.0,
                    EntryKind::Bus => 3.0,
                };
                let (pos, radius) = self.place_radius(o, off, default_r)?;
                self.def.entries.push(EntryDef {
                    name: required_name(o)?,
                    kind,
                    pos,
                    radius,
                });
            }
            "depots" | "staging" => {
                let (pos, _) = self.place_radius(o, off, 0.0)?;
                let p = NamedPoint {
                    name: required_name(o)?,
                    pos,
                };
                if layer == "depots" {
                    self.def.depots.push(p);
                } else {
                    self.def.staging.push(p);
                }
            }
            "areas" => {
                let (pos, radius) = self.place_radius(o, off, 20.0)?;
                let tag = match prop_str(&o.properties, "tag")? {
                    Some(t) => t.trim().to_string(),
                    None => ty,
                };
                self.def.areas.push(AreaDef {
                    name: required_name(o)?,
                    tag,
                    pos,
                    radius,
                });
            }
            _ => {}
        }
        Ok(())
    }

    /// Closed outline of a polygon, rectangle, ellipse (as a `segments`-gon)
    /// or tile object, in metres.
    fn outline(&self, o: &Object, off: [f64; 2], segments: usize) -> Result<Vec<[f32; 2]>> {
        let (s, c) = sin_cos_deg(o.rotation);
        let local: Vec<[f64; 2]> = if let Some(poly) = &o.polygon {
            if poly.len() < 3 {
                return Err(DataError::new("polygon has fewer than 3 points"));
            }
            poly.iter().map(|p| [p.x, p.y]).collect()
        } else if o.polyline.is_some() {
            return Err(DataError::new("is a polyline; draw a polygon or rectangle"));
        } else if o.point {
            return Err(DataError::new("is a point; draw a polygon or rectangle"));
        } else if !(o.width > 0.0 && o.height > 0.0) {
            return Err(DataError::new("has no width or height"));
        } else if o.ellipse {
            let (rx, ry) = (o.width * 0.5, o.height * 0.5);
            (0..segments)
                .map(|k| {
                    let (sn, cs) = sin_cos_deg(k as f64 * 360.0 / segments as f64);
                    [rx + rx * cs, ry + ry * sn]
                })
                .collect()
        } else {
            // Tile objects are anchored at their bottom-left corner.
            let y0 = if o.gid.is_some() { -o.height } else { 0.0 };
            vec![
                [0.0, y0],
                [o.width, y0],
                [o.width, y0 + o.height],
                [0.0, y0 + o.height],
            ]
        };
        Ok(local
            .iter()
            .map(|p| self.m(place(o, off, s, c, p[0], p[1])))
            .collect())
    }

    /// Position and radius (metres) of a point, ellipse, rectangle or tile
    /// object. A `radius` property (metres) wins over the shape's size.
    fn place_radius(&self, o: &Object, off: [f64; 2], default_r: f32) -> Result<([f32; 2], f32)> {
        if o.polygon.is_some() || o.polyline.is_some() {
            return Err(DataError::new("must be a point or an ellipse"));
        }
        let (s, c) = sin_cos_deg(o.rotation);
        let (pos, shape_r) = if o.point || (o.width == 0.0 && o.height == 0.0) {
            (self.m(place(o, off, s, c, 0.0, 0.0)), None)
        } else {
            let cy = if o.gid.is_some() {
                -o.height * 0.5
            } else {
                o.height * 0.5
            };
            let p = self.m(place(o, off, s, c, o.width * 0.5, cy));
            (p, Some(((o.width + o.height) * 0.25 * self.mpp) as f32))
        };
        let radius = match prop_f64(&o.properties, "radius")? {
            Some(r) if r > 0.0 => r as f32,
            Some(r) => return Err(DataError::new(format!("radius must be positive, not {r}"))),
            None => shape_r.unwrap_or(default_r),
        };
        Ok((pos, radius))
    }

    fn prop(&self, o: &Object, off: [f64; 2], ty: &str) -> Result<PropDef> {
        let (kind, default_half) = match ty {
            "car" => (PropKind::Car, [2.2, 0.9]),
            "bin" => (PropKind::Bin, [0.35, 0.35]),
            "barrier" => (PropKind::Barrier, [1.2, 0.12]),
            "bench" => (PropKind::Bench, [0.9, 0.3]),
            "barricade" => (PropKind::Barricade, [1.5, 0.5]),
            "police_barrier" => (PropKind::PoliceBarrier, [1.25, 0.3]),
            other => {
                return Err(DataError::new(format!(
                    "unknown prop type \"{other}\" (expected car, bin, barrier, bench, barricade or police_barrier)"
                )));
            }
        };
        if o.polygon.is_some() || o.polyline.is_some() || o.ellipse {
            return Err(DataError::new("props must be rectangles or points"));
        }
        let (s, c) = sin_cos_deg(o.rotation);
        let angle = (o.rotation.rem_euclid(360.0) * (std::f64::consts::PI / 180.0)) as f32;
        if o.point || (o.width == 0.0 && o.height == 0.0) {
            return Ok(PropDef {
                kind,
                pos: self.m(place(o, off, s, c, 0.0, 0.0)),
                half: default_half,
                angle,
            });
        }
        let cy = if o.gid.is_some() {
            -o.height * 0.5
        } else {
            o.height * 0.5
        };
        Ok(PropDef {
            kind,
            pos: self.m(place(o, off, s, c, o.width * 0.5, cy)),
            half: [
                (o.width * 0.5 * self.mpp) as f32,
                (o.height * 0.5 * self.mpp) as f32,
            ],
            angle,
        })
    }

    fn ground(&mut self, layer: &Layer, off: [f64; 2]) -> Result<()> {
        let map = self.map;
        if off != [0.0, 0.0] {
            return Err(DataError::new("the ground layer must not be offset"));
        }
        if map.tilewidth != map.tileheight {
            return Err(DataError::new(format!(
                "ground tiles must be square, not {} × {} px",
                map.tilewidth, map.tileheight
            )));
        }
        if (layer.width, layer.height) != (map.width, map.height) {
            return Err(DataError::new(format!(
                "is {} × {} tiles; the map is {} × {}",
                layer.width, layer.height, map.width, map.height
            )));
        }
        let count = map.width as usize * map.height as usize;
        let gids = decode_gids(layer, count)?;
        if self.tilesets.is_none() {
            self.tilesets = Some(self.resolve_tilesets()?);
        }
        let sets = self.tilesets.as_ref().unwrap();
        let g = &mut self.def.ground;
        if g.kinds.is_empty() {
            *g = GroundLayer {
                cell: (map.tilewidth as f64 * self.mpp) as f32,
                w: map.width,
                h: map.height,
                kinds: vec![Ground::Pavement as u8; count],
            };
        }
        // Runs of the same tile are common: remember the last lookup.
        let mut last = (0u32, Ground::Pavement as u8);
        for (k, &raw) in g.kinds.iter_mut().zip(&gids) {
            let gid = raw & !GID_FLAGS;
            if gid == 0 {
                continue;
            }
            if gid != last.0 {
                last = (gid, ground_of(sets, gid)?);
            }
            *k = last.1;
        }
        Ok(())
    }

    fn resolve_tilesets(&self) -> Result<Vec<GroundTiles>> {
        let mut out = Vec::new();
        for ts in &self.map.tilesets {
            let loaded;
            let (t, label) = match &ts.source {
                None => (ts, format!("tileset \"{}\"", ts.name)),
                Some(src) => {
                    let Some(dir) = self.dir else {
                        return Err(DataError::new(format!(
                            "external tileset \"{src}\" needs the map's directory; use load_map"
                        )));
                    };
                    if src.ends_with(".tsx") {
                        return Err(DataError::new(format!(
                            "tileset \"{src}\" is XML; export it as JSON (.tsj) or embed it in the map"
                        )));
                    }
                    let path = dir.join(src);
                    let text =
                        std::fs::read_to_string(&path).map_err(|e| DataError::io(&path, e))?;
                    loaded = serde_json::from_str::<Tileset>(&text).map_err(|e| {
                        DataError::caused_by(format!("not a valid Tiled JSON tileset: {e}"), e)
                            .in_file(&path)
                    })?;
                    (&loaded, format!("tileset \"{src}\""))
                }
            };
            let mut kinds = Vec::new();
            for tile in &t.tiles {
                let named = match prop_str(&tile.properties, "ground")? {
                    Some(v) => Some(v),
                    None if !tile.kind.is_empty() => Some(tile.kind.as_str()),
                    None if !tile.class.is_empty() => Some(tile.class.as_str()),
                    None => None,
                };
                if let Some(v) = named {
                    let g = ground_named(v.trim()).ok_or_else(|| {
                        DataError::new(format!(
                            "{label}, tile {}: unknown ground \"{v}\" \
                             (expected pavement, road, grass, plaza or water)",
                            tile.id
                        ))
                    })?;
                    kinds.push((tile.id, g as u8));
                }
            }
            kinds.sort_unstable();
            out.push(GroundTiles {
                firstgid: ts.firstgid,
                kinds,
            });
        }
        out.sort_by_key(|t| t.firstgid);
        Ok(out)
    }
}

/// Object-local pixel offset → map pixels: rotate clockwise by the object's
/// rotation about its origin, then translate.
fn place(o: &Object, off: [f64; 2], s: f64, c: f64, lx: f64, ly: f64) -> [f64; 2] {
    [
        o.x + off[0] + lx * c - ly * s,
        o.y + off[1] + lx * s + ly * c,
    ]
}

fn required_name(o: &Object) -> Result<String> {
    let n = o.name.trim();
    if n.is_empty() {
        return Err(DataError::new("needs a name (scenarios refer to it)"));
    }
    Ok(n.to_string())
}

fn ground_named(s: &str) -> Option<Ground> {
    Some(match s.to_ascii_lowercase().as_str() {
        "pavement" => Ground::Pavement,
        "road" => Ground::Road,
        "grass" => Ground::Grass,
        "plaza" => Ground::Plaza,
        "water" => Ground::Water,
        _ => return None,
    })
}

/// Ground kind of a global tile id (pavement for tiles without a `ground`).
fn ground_of(sets: &[GroundTiles], gid: u32) -> Result<u8> {
    let i = sets.partition_point(|t| t.firstgid <= gid);
    if i == 0 {
        return Err(DataError::new(format!(
            "tile id {gid} is not in any tileset"
        )));
    }
    let t = &sets[i - 1];
    let local = gid - t.firstgid;
    Ok(match t.kinds.binary_search_by_key(&local, |k| k.0) {
        Ok(j) => t.kinds[j].1,
        Err(_) => Ground::Pavement as u8,
    })
}

fn prop<'a>(props: &'a [Property], name: &str) -> Option<&'a Property> {
    props.iter().find(|p| p.name == name)
}

/// A numeric property, if present.
fn prop_f64(props: &[Property], name: &str) -> Result<Option<f64>> {
    match prop(props, name) {
        None => Ok(None),
        Some(p) => match p.value.as_f64() {
            Some(v) => Ok(Some(v)),
            None => Err(DataError::new(format!(
                "property `{name}` must be a number, not {}",
                p.value
            ))),
        },
    }
}

/// A string property, if present.
fn prop_str<'a>(props: &'a [Property], name: &str) -> Result<Option<&'a str>> {
    match prop(props, name) {
        None => Ok(None),
        Some(p) => match p.value.as_str() {
            Some(v) => Ok(Some(v)),
            None => Err(DataError::new(format!(
                "property `{name}` must be a string, not {}",
                p.value
            ))),
        },
    }
}

// ---------------------------------------------------------------------------
// Road graph

/// Builds the road graph from polylines: vertices within [`ROAD_MERGE_M`]
/// merge into one node, and a dangling end that lands on another road's
/// edge splits that edge (a T-junction).
#[derive(Default)]
struct RoadBuilder {
    nodes: Vec<[f32; 2]>,
    edges: Vec<[u32; 2]>,
    /// Nodes by 1 m cell.
    grid: std::collections::HashMap<(i32, i32), Vec<u32>>,
}

impl RoadBuilder {
    fn node(&mut self, p: [f32; 2]) -> u32 {
        let (cx, cy) = (
            (p[0] / ROAD_MERGE_M).floor() as i32,
            (p[1] / ROAD_MERGE_M).floor() as i32,
        );
        let mut best: Option<(f32, u32)> = None;
        for gy in cy - 1..=cy + 1 {
            for gx in cx - 1..=cx + 1 {
                for &n in self.grid.get(&(gx, gy)).map_or(&[][..], |v| v) {
                    let q = self.nodes[n as usize];
                    let d = (q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2);
                    if d <= ROAD_MERGE_M * ROAD_MERGE_M
                        && best.is_none_or(|b| d < b.0 || (d == b.0 && n < b.1))
                    {
                        best = Some((d, n));
                    }
                }
            }
        }
        if let Some((_, n)) = best {
            return n;
        }
        let n = self.nodes.len() as u32;
        self.nodes.push(p);
        self.grid.entry((cx, cy)).or_default().push(n);
        n
    }

    fn polyline(&mut self, pts: &[[f32; 2]], closed: bool) {
        let ids: Vec<u32> = pts.iter().map(|&p| self.node(p)).collect();
        for w in ids.windows(2) {
            if w[0] != w[1] {
                self.edges.push([w[0], w[1]]);
            }
        }
        if closed && ids.len() > 2 && ids[0] != ids[ids.len() - 1] {
            self.edges.push([ids[ids.len() - 1], ids[0]]);
        }
    }

    fn finish(mut self) -> RoadGraph {
        let mut degree = vec![0u32; self.nodes.len()];
        for e in &self.edges {
            degree[e[0] as usize] += 1;
            degree[e[1] as usize] += 1;
        }
        let r2 = ROAD_MERGE_M * ROAD_MERGE_M;
        for n in 0..self.nodes.len() as u32 {
            if degree[n as usize] != 1 {
                continue;
            }
            let p = self.nodes[n as usize];
            let mut best: Option<(f32, usize)> = None;
            for (k, e) in self.edges.iter().enumerate() {
                if e[0] == n || e[1] == n {
                    continue;
                }
                let (a, b) = (self.nodes[e[0] as usize], self.nodes[e[1] as usize]);
                if p[0] < a[0].min(b[0]) - ROAD_MERGE_M
                    || p[0] > a[0].max(b[0]) + ROAD_MERGE_M
                    || p[1] < a[1].min(b[1]) - ROAD_MERGE_M
                    || p[1] > a[1].max(b[1]) + ROAD_MERGE_M
                {
                    continue;
                }
                let d = seg_dist2(p, a, b);
                if d <= r2 && best.is_none_or(|x| d < x.0) {
                    best = Some((d, k));
                }
            }
            if let Some((_, k)) = best {
                let [a, b] = self.edges[k];
                self.edges[k] = [a, n];
                self.edges.push([n, b]);
                degree[n as usize] += 2;
            }
        }
        for e in &mut self.edges {
            if e[0] > e[1] {
                e.swap(0, 1);
            }
        }
        self.edges.sort_unstable();
        self.edges.dedup();
        RoadGraph {
            nodes: self.nodes,
            edges: self.edges,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map_json(layers: &str, props: &str) -> String {
        format!(
            r#"{{"type":"map","orientation":"orthogonal","width":10,"height":5,
            "tilewidth":16,"tileheight":16,"infinite":false,"properties":[{props}],
            "tilesets":[{{"firstgid":1,"name":"g","tilewidth":16,"tileheight":16,"tilecount":3,
              "tiles":[{{"id":1,"properties":[{{"name":"ground","type":"string","value":"road"}}]}},
                       {{"id":2,"type":"water"}}]}}],
            "layers":[{layers}]}}"#
        )
    }

    #[test]
    fn base64_round_trip() {
        for n in 0..20u8 {
            let bytes: Vec<u8> = (0..n).map(|k| k.wrapping_mul(37)).collect();
            assert_eq!(base64_decode(&base64_encode(&bytes)).unwrap(), bytes);
        }
        assert_eq!(base64_encode(b"Man"), "TWFu");
        assert_eq!(base64_encode(b"Ma"), "TWE=");
        assert_eq!(base64_decode("TW E=\n").unwrap(), b"Ma");
        assert!(base64_decode("TW*=").is_err());
    }

    #[test]
    fn converts_shapes_and_coordinates() {
        // 10 × 5 tiles of 16 px at 0.5 m/px: 80 × 40 m, centre at (40, 20) m.
        let layers = r#"
          {"type":"objectgroup","name":"Solids","objects":[
            {"id":1,"type":"building","x":0,"y":0,"width":20,"height":10},
            {"id":2,"type":"wall","x":100,"y":20,"width":10,"height":2,"rotation":90},
            {"id":3,"class":"water","x":40,"y":40,"polygon":[{"x":0,"y":0},{"x":10,"y":0},{"x":0,"y":10}]}]},
          {"type":"group","name":"markers","offsetx":2,"layers":[
            {"type":"objectgroup","name":"entries","objects":[
              {"id":4,"name":"west","type":"map_edge","x":0,"y":40,"point":true},
              {"id":5,"name":"metro","type":"metro","x":60,"y":40,"width":8,"height":8,"ellipse":true,
               "properties":[{"name":"radius","type":"float","value":3}]}]},
            {"type":"objectgroup","name":"depots","objects":[{"id":6,"name":"d","x":10,"y":10,"point":true}]},
            {"type":"objectgroup","name":"areas","objects":[
              {"id":7,"name":"sq","x":80,"y":20,"width":20,"height":20,"ellipse":true,
               "properties":[{"name":"tag","type":"string","value":"square"}]}]}]},
          {"type":"objectgroup","name":"props","objects":[
            {"id":8,"type":"car","x":20,"y":20,"width":8,"height":4,"rotation":90}]},
          {"type":"tilelayer","name":"ground","width":10,"height":5,"data":
            [0,1,2,3,2147483651,0,0,0,0,0, 0,0,0,0,0,0,0,0,0,0, 0,0,0,0,0,0,0,0,0,0,
             0,0,0,0,0,0,0,0,0,0, 0,0,0,0,0,0,0,0,0,3]}"#;
        let json = map_json(
            layers,
            r#"{"name":"metres_per_pixel","type":"float","value":0.5}"#,
        );
        let m = parse_map(&json, "t").unwrap();
        assert_eq!((m.width, m.height), (80.0, 40.0));
        assert_eq!(m.solids.len(), 3);
        assert_eq!(m.solids[0].kind, SolidKind::Building);
        assert_eq!(
            m.solids[0].points,
            vec![
                [-40.0, -20.0],
                [-30.0, -20.0],
                [-30.0, -15.0],
                [-40.0, -15.0]
            ]
        );
        // Rotated 90° clockwise about (100, 20) px: width runs down the screen.
        assert_eq!(m.solids[1].kind, SolidKind::Wall);
        assert_eq!(
            m.solids[1].points,
            vec![[10.0, -10.0], [10.0, -5.0], [9.0, -5.0], [9.0, -10.0]]
        );
        assert_eq!(m.solids[2].kind, SolidKind::Water);
        assert_eq!(m.solids[2].points[1], [-15.0, 0.0]);
        // Group offset of 2 px = 1 m.
        assert_eq!(m.entries[0].pos, [-39.0, 0.0]);
        assert_eq!(m.entries[0].radius, 6.0);
        assert_eq!(m.entries[1].kind, EntryKind::Metro);
        assert_eq!(m.entries[1].pos, [-7.0, 2.0]);
        assert_eq!(m.entries[1].radius, 3.0);
        assert_eq!(m.depots[0].pos, [-34.0, -15.0]);
        assert_eq!(m.areas[0].tag, "square");
        assert_eq!(m.areas[0].pos, [6.0, -5.0]);
        assert_eq!(m.areas[0].radius, 5.0);
        let p = m.props[0];
        assert_eq!(p.kind, PropKind::Car);
        assert_eq!(p.pos, [-31.0, -8.0]);
        assert_eq!(p.half, [2.0, 1.0]);
        assert!((p.angle - std::f32::consts::FRAC_PI_2).abs() < 1e-6);
        // Ground: 1-based gids, flip flags masked, `type` as a fallback.
        assert_eq!(m.ground.cell, 8.0);
        assert_eq!(&m.ground.kinds[..6], &[0, 0, 1, 4, 4, 0]);
        assert_eq!(m.ground.kinds[49], 4);
        assert_eq!(m.ground_at(-12.0, -19.0), Ground::Water);
        assert_eq!(m.ground_at(-31.0, -19.0), Ground::Pavement);
    }

    #[test]
    fn roads_merge_and_split() {
        let layers = r#"{"type":"objectgroup","name":"roads","objects":[
            {"id":1,"x":0,"y":20,"polyline":[{"x":0,"y":0},{"x":100,"y":0},{"x":200,"y":0}]},
            {"id":2,"x":100.5,"y":-100,"polyline":[{"x":0,"y":0},{"x":0,"y":119.5}]},
            {"id":3,"x":150,"y":100,"polyline":[{"x":0,"y":0},{"x":0,"y":-79}]}]}"#;
        let m = parse_map(&map_json(layers, ""), "t").unwrap();
        // Polyline 2 ends within 1 m of polyline 1's middle vertex; polyline
        // 3 ends 0.25 m short of polyline 1's second edge, which splits.
        assert_eq!(m.roads.nodes.len(), 6);
        assert_eq!(m.roads.edges, vec![[0, 1], [1, 3], [1, 5], [2, 5], [4, 5]]);
    }

    #[test]
    fn compressed_ground_round_trips() {
        let gids: Vec<u32> = (0..50).map(|k| k % 4).collect();
        let (data, enc, comp) = encode_gids(&gids, true);
        let layer = Layer {
            kind: "tilelayer".into(),
            data: Some(data),
            encoding: enc,
            compression: comp,
            ..Default::default()
        };
        assert_eq!(decode_gids(&layer, 50).unwrap(), gids);
        assert!(decode_gids(&layer, 49).is_err());
    }

    #[test]
    fn errors_say_where() {
        let layers = r#"{"type":"objectgroup","name":"solids","objects":[
            {"id":12,"name":"town hall","type":"buildng","x":0,"y":0,"width":4,"height":4}]}"#;
        let e = parse_map(&map_json(layers, ""), "t")
            .unwrap_err()
            .to_string();
        assert!(
            e.contains(
                "layer \"solids\": object 12 (\"town hall\"): unknown solid type \"buildng\""
            ),
            "{e}"
        );
        let layers = r#"{"type":"objectgroup","name":"entries","objects":[
            {"id":1,"x":0,"y":0,"point":true}]}"#;
        let e = parse_map(&map_json(layers, ""), "t")
            .unwrap_err()
            .to_string();
        assert!(e.contains("needs a name"), "{e}");
        let e = parse_map("{\"width\": 3", "t").unwrap_err().to_string();
        assert!(e.contains("not a valid Tiled JSON map"), "{e}");
        let big = r#"{"name":"metres_per_pixel","type":"float","value":100}"#;
        let e = parse_map(&map_json("", big), "t").unwrap_err().to_string();
        assert!(e.contains("largest supported"), "{e}");
    }
}
