# Scenarios

A scenario is everything one riot starts from (DESIGN §17): the map, the crowd
and when it arrives, the police, the escalation cap, comms, weather, the
objectives, and how long it lasts. It is a [RON](https://github.com/ron-rs/ron)
file that `riot_data::load_scenario` turns into a `riot_sim::RiotSetup`, plus
the `MapDef` and `Tunables` it runs with.

| File                      | Map       | People | Police     | Cap              | Length |
|---------------------------|-----------|--------|------------|------------------|--------|
| `small_protest.ron`       | district  | 2,000  | 4 squads   | BatonsAndArrests | 45 min |
| `standard_riot.ron`       | district  | 10,000 | 30 squads  | GasAndWater      | 10 min (the benchmark) |
| `mass_demonstration.ron`  | 8 km city | 50,000 | 60 squads  | GasAndWater      | 2 h    |
| `city_day.ron`            | 8 km city | 30,000 | 42 squads  | MassArrests      | 14 h, internet shutdown at 21:00 |

## An annotated example

Every field is shown; most are optional (defaults in the comments). Comments
start with `//`.

```ron
(
    // Optional, for menus and logs.
    title: "Example",
    description: "Everything a scenario can say.",

    // The map: a Tiled JSON file relative to this file, or a generated one:
    //   "generated:city?size=8192&seed=3"   the procedural city (size 512–8192 m)
    //   "generated:district?seed=1"         the same generator at 1024 m
    //   "generated:testkit?size=1024&block=64"  riot_sim's test district
    // Generator parameters: size, seed, block (street spacing, m),
    // avenue_every (blocks), river (true/false), props (most props).
    map: "../maps/district.json",

    // Tunables file relative to this file (default: built-in defaults), then
    // per-scenario overrides by name.
    tunables: "../tunables.ron",
    tune: { "hop_loss": 5.0 },

    seed: 1,                 // default 1
    start: "14:00",          // time of day at tick 0; or minutes after midnight. Default "12:00".
    duration: 45,            // minutes, or "H:MM". Required.

    // Turnout per crowd archetype: student, worker, elder, radical,
    // provocateur, journalist, bystander. Organizers come from `organizers`,
    // officers from the police squads.
    crowd: { "student": 700, "worker": 700, "elder": 300, "radical": 100 },
    organizers: 6,           // default 0

    // The turnout curve. Each window sends a `share` of everyone (default 1)
    // through `entry` between `from` and `to`. Times are minutes after the
    // start, or "HH:MM" times of day (past midnight wraps to the next day).
    // An entry ending in `*` matches every entry with that prefix and splits
    // the share between them: "metro_*", "edge_n*", "*".
    // `bias` multiplies the share per archetype (default 1): radicals after
    // dark, elders in the morning.
    arrivals: [
        (entry: "edge_*", from: 0, to: 10),
        (entry: "metro_*", from: "14:30", to: "15:30", share: 0.5, bias: { "radical": 3.0 }),
    ],

    // What the movement's logistics delivered (default all 0).
    supplies: (water: 120, medic: 30, shields: 10, barricade: 0),

    police: (
        vans_per_depot: 2,   // default 0: squads then walk
        water_cannons: 0,    // spread over the depots, default 0
        aggression: 0.4,     // 0 cautious … 1 aggressive, default 0
        squads: [
            // kind: Line (lines, kettles), Riot (shields, batons), Snatch (arrests).
            // officers: 1–16, default 10. depot: a depot name, or a prefix
            // with `*`. staging: start there already deployed (optional).
            // available: when the squad can be used (default at once).
            // count: this many alike (default 1); `*` names are dealt
            // round-robin over the matching depots and staging points.
            (kind: Line, depot: "depot_*", staging: "staging_central_square", count: 2),
            (kind: Snatch, officers: 6, depot: "depot_north", available: 15),
        ],
    ),

    // The highest rung the state may use: Presence, LinesAndKettles,
    // BatonsAndArrests (default), GasAndWater, MassArrests.
    escalation_cap: BatonsAndArrests,

    // mode: Phones (default), Degraded or Shutdown. shutdown_at: the state
    // cuts the internet then (optional; minutes or "HH:MM").
    comms: (mode: Phones, shutdown_at: "16:00"),

    // wind: (east, south) m/s, default (1, 0). heat: 0 cool … 1 hot, default
    // 0.3. dusk: when it gets dark, default "20:00".
    weather: (wind: (1.5, 0.5), heat: 0.4, dusk: "20:15"),

    // kind: MarchTo (default), Hold or Disperse. area: a named area of the
    // map, or leave it out and give pos: (x, y) metres from the map centre
    // and radius. radius 0 with an area uses the area's own. weight counts
    // in ground held (default 1). announced: every arriving protester knows
    // it (default false). stance: Peaceful (default), Defensive or
    // Confrontational.
    objectives: [
        (name: "Rally at the square", kind: MarchTo, area: "central_square", announced: true),
        (name: "Hold the corner", kind: Hold, pos: (120.0, -40.0), radius: 15.0, stance: Defensive),
    ],

    // Preallocation sizes (DESIGN §16); anything left out keeps its default:
    // field_chunks 160, flow_tiles 24576, flow_pages 3072, props 8192,
    // projectiles 1024, notable 256.
    limits: (field_chunks: 200),
)
```

## Rules

- Unknown fields, archetypes, tunables, entries, depots, staging points and
  areas are errors, reported with the file, the field and what the map has:

  ```text
  data/scenarios/standard_riot.ron: police.squads[1]: no depot matches `depot_x` (depots in the map: depot_south, depot_north)
  data/scenarios/city_day.ron: line 14, column 5: Unexpected field named `durration` in `ScenarioFile`, expected one of …
  ```
- Enum values (`Line`, `GasAndWater`, `MarchTo`, …) are written as bare names;
  snake_case (`gas_and_water`) works too.
- `Option` fields take the value directly: `staging: "x"`, not `Some("x")`.
- A riot holds at most 65,000 people: crowd, organizers and officers together.
- Map entries, depots, staging points and areas of the generated maps follow
  the names listed in `../maps/README.md`.
