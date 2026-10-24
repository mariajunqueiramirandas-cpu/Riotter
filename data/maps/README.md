# Maps

Riotter maps are drawn in [Tiled](https://www.mapeditor.org) and saved as JSON
(*File → Save As…* with the `.tmj`/`.json` format, or *Export As → JSON map
files*). `riot_data::tiled::load_map` turns one into a `riot_sim::MapDef`
(DESIGN §2.1); the simulation derives its nav tiles and wall lists from that at
load time.

| File            | What it is                                                                   |
|-----------------|------------------------------------------------------------------------------|
| `district.json` | A generated 1 km city (seed 1): the map of the small and standard scenarios. |
| `city.json`     | The generated 8 km city (seed 3): 8192 × 8192 m, about 9.3 MB.               |
| `ground.png`    | The ground tileset image both maps use (one 16 px tile per ground kind).     |

Both maps come from `riot_data::citygen` and can be rebuilt with
`cargo run --release -p riot_data --example gen_data`. A scenario can also ask
for a generated map directly instead of a file:
`generated:city?size=8192&seed=3` is `city.json`, and
`generated:district?seed=1` is `district.json` (see `../scenarios/README.md`).

## Map setup

- **Orthogonal**, not infinite, square tiles. Generated maps use 16 px tiles.
- **Scale**: the map property `metres_per_pixel` (float, default `0.25`). The
  map is `width × tilewidth × metres_per_pixel` metres across, at most 8192 m
  on each side. With 16 px tiles at 0.25 m/px a tile is 4 m.
- **Coordinates**: Tiled measures pixels from the top-left corner with y down.
  Riotter measures metres from the centre of the map, also with y down:
  `x_m = x_px · metres_per_pixel − width_m / 2`.
- **Rotation**: Tiled rotates an object clockwise about its `x, y`: the
  top-left corner of a rectangle or ellipse, the bottom-left corner of a tile
  object. Polygon and polyline points are relative to the object's position
  and rotate with it. All of this is honoured.
- **Layers are found by name**, ignoring case. Group layers are searched
  recursively (their offsets add up), so `north/solids` and `south/solids` both
  count as `solids`. Layers with other names are ignored: use them for notes or
  reference images. Hidden layers are still read. Text objects are ignored;
  object templates must be detached (*Detach* in Tiled's context menu).
- **Object types**: Tiled 1.10+ saves an object's class as `type`, Tiled 1.9 as
  `class`; both are read. Types are case-insensitive.

## Layers

| Layer      | Layer kind | Objects                                                          | Becomes                       |
|------------|------------|------------------------------------------------------------------|-------------------------------|
| `ground`   | tile       | tiles with a `ground` property                                   | ground types (4 m cells)      |
| `solids`   | object     | polygons, rectangles, ellipses; type `building`, `wall`, `water`, `fence` | collision shapes     |
| `overhead` | object     | polygons, rectangles, ellipses; type is free (`canopy`, `arcade`, `bridge`) | drawn above people, walkable |
| `props`    | object     | rectangles (or points); type `car`, `bin`, `barrier`, `bench`, `barricade` | pushable, burnable boxes |
| `roads`    | object     | polylines (a polygon is a closed loop)                           | the road graph vans drive on  |
| `entries`  | object     | named points or ellipses; type `map_edge`, `metro`, `bus`        | where people arrive and leave |
| `depots`   | object     | named points                                                     | police depots                 |
| `staging`  | object     | named points                                                     | police staging points         |
| `areas`    | object     | named points or ellipses with a `tag`                            | places objectives refer to    |

### `ground`

A tile layer the size of the map. Each tileset tile used on it carries a string
property `ground` = `pavement`, `road`, `grass`, `plaza` or `water` (a tile's
type is used when the property is missing). Empty cells and tiles without a
ground are pavement. Tile ids are resolved through each tileset's `firstgid`
and Tiled's flip flags are ignored. Embedded tilesets and external JSON
tilesets (`.tsj`) work; XML tilesets (`.tsx`) do not: embed or re-export them.
Layer data may be CSV or base64, uncompressed, zlib or gzip (not zstd). Several
`ground` layers stack: later layers' non-empty tiles win. Tiles must be square;
the ground cell is one tile.

Ground is what people walk on and how the renderer paints it; it does not
block anything. Water you cannot walk into is a `water` solid.

### `solids`

The exact collision shapes (DESIGN §11.2). A polygon may wind either way but
must not cross itself. Ellipses become 24-sided polygons. An empty type means
`building`. Overlapping solids are fine. Leave at least 0.9 m between solids
wherever people should squeeze through, and keep solids at least 1 m from road
centrelines.

### `overhead`

Arcades, tree canopies and bridges over roads: people walk underneath and the
renderer draws them on top. Ellipses become 16-sided polygons. Purely visual.

### `props`

Parked cars, bins, crowd-control barriers, benches and pre-built barricades.
Each is an oriented box: a rectangle (its size and rotation are used) or a
point, which gets a default size (car 4.4 × 1.8 m, bin 0.7 m, barrier
2.4 × 0.24 m, bench 1.8 × 0.6 m, barricade 3 × 1 m). The simulation keeps the
first `limits.props` of them (8192 by default).

### `roads`

Polylines along carriageway centrelines. Every vertex becomes a graph node and
consecutive vertices an edge. **Vertices closer than 1 m merge**, so draw
crossing roads with a vertex on both at the crossing (Tiled's *Snap to Pixels*
or *Snap to Grid* help). A road that ends within 1 m of another road's edge
splits that edge: a T-junction. Roads that cross without a shared vertex stay
apart (an overpass).

### `entries`, `depots`, `staging`, `areas`

Named markers; scenarios refer to them by name, so names must be unique within
each layer. The simulation moves a marker that sits in a wall to the nearest
walkable spot within 64 m, but keep them in the open.

- **Entries**: type `map_edge` (default), `metro` or `bus`. People spawn within
  `radius` metres: a float property `radius`, else the ellipse's mean radius,
  else 6 m (edge), 4 m (metro) or 3 m (bus). Metro entries also let people
  ride between stations.
- **Depots** and **staging points**: points (an ellipse or rectangle counts
  by its centre). Squads, vans and water cannons start at depots; a squad can
  start already deployed at a staging point.
- **Areas**: a string property `tag` (`square`, `government`, `park`,
  `bridge`, …; the object's type when missing) and a radius: a float property
  `radius` in metres, else the ellipse's mean radius, else 20 m.

## Errors

Loading stops at the first problem and says where it is, for example:

```text
data/maps/city.json: layer "solids": object 812 ("town hall"): unknown solid type "buildng" (expected building, wall, water or fence)
data/maps/city.json: layer "markers": layer "entries": object 9: needs a name (scenarios refer to it)
```

## Generated cities

`riot_data::citygen` writes maps in this convention (`CityParams`: size 512 m
to 8 km, seed, block size, avenue spacing, river, prop budget). From coarse to
fine:

- **Streets** on a grid-ish layout with blocks of about 80–120 m: 32 m avenues
  (16 m carriageway) through the centre and every few blocks, 16 m streets and
  12 m lanes between them, merged superblocks with cul-de-sac stubs.
- **Blocks** lined with buildings around courtyards: rectangles and L-shaped
  corners, 2–4 m alleys between some of them (some dead ends), arcades facing
  squares and avenues, the odd courtyard garden or rear building.
- **The centre**: the government quarter (a parliament behind a fenced
  forecourt, flanked by ministries, barriers on the pavement) facing the big
  central square across the central avenue.
- **A river** across the city in a band with promenades, crossed by a bridge
  at every avenue and some streets, with railings along each deck.
- **Squares** (plaza, trees, benches, a fountain on large ones) and **parks**
  (grass, paths, tree canopies, a pond in large ones).
- **Police depots** (2–6) on a ring at the outskirts: a fenced yard facing the
  centre and a depot building.
- **Props**: parked cars along streets and bins on corners, at most about 6000
  on the 8 km city.

Generated names, which scenarios can rely on:

| Name                     | What                                                                 |
|--------------------------|----------------------------------------------------------------------|
| `edge_n1`, `edge_n2`, …  | map-edge entries at avenue ends on the north edge, west to east; `edge_s*` likewise; `edge_w*` and `edge_e*` north to south. Small maps add streets so every edge has at least three. |
| `metro_1`, `metro_2`, …  | metro stations about 800 m apart, nearest the centre first           |
| `bus_1`, `bus_2`, …      | bus stops on avenues about 1200 m apart                              |
| `depot_north`, …         | police depots, named by compass direction from the centre            |
| `staging_central_square`, `staging_square_1`, … | a staging point at the corner of each square nearest a depot |
| `central_square`         | the big square in front of the government quarter (tag `square`)     |
| `government_quarter`     | the street in front of the parliament (tag `government`)             |
| `square_1`, `square_2`, … | other squares, nearest the centre first (tag `square`)              |
| `park_1`, `park_2`, …    | parks, nearest the centre first (tag `park`)                         |
| `bridge_1`, `bridge_2`, … | bridges, west to east (tag `bridge`)                                |
