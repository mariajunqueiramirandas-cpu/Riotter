# Riotter: Riot Simulation Design

Status: draft v0.2 (2026-10-24), implemented; §22 records where the code departs from this text and how it measures against §3. v0.2 makes every person an individual body with exact collision, facing and pose, ready for per-person sprites, grows maps to an 8 km city whose size costs nothing per tick, and settles the camera as top-down 2D.

This document covers **the real-time riot simulation only**: one map (up to an 8 km city), one day of protest, thousands of autonomous people against a police force, with the player influencing events but never commanding them. The turn-based campaign, multiplayer, 3D, audio and UI art are out of scope. The campaign shows up only as the boundary: a `RiotSetup` goes in and a `RiotOutcome` comes out (§17).

It has two goals, and they carry equal weight:

1. **The crowd must feel alive.** It should be unpredictable in the moment, yet readable after the fact.
2. **The simulation must be extremely fast.** The main target is 10,000 agents in about 3 ms per 20 Hz tick, on any map size. The stretch target is 50,000. A 10-minute riot must auto-resolve in under 40 seconds, and a simulated hour in under 2 minutes.

Everything below is written in service of both goals.

---

## 1. Design pillars

1. **You influence, the crowd decides.** The player never selects units, and no input is ever executed exactly. Every input is a suggestion that has to travel through people, and each person decides what to do with it.
2. **Emergence over scripting.** Big moments (a stampede, a police line breaking, a kettle closing) come out of local rules. Triggers don't create them.
3. **A riot never just wins or loses.** It ends with ground held, arrests, injuries, footage, morale and supplies used, and each of those matters separately.
4. **The simulation is the truth.** It is headless, deterministic and runs on a fixed tick. The renderer only watches.
5. **Performance is a feature.** Bigger crowds and faster auto-resolve are gameplay. Every system has a time budget and a benchmark, and going over budget is a bug.
6. **Bodies, not cells.** Every person is an individual body with an exact position, size, facing and pose, so every one of them can be simulated, seen and drawn as their own sprite. Grids only speed up lookups; they never decide where anyone stands.
7. **Map size is free.** Cost grows with the number of people and how much is happening, never with the area of the map. A quiet street on the far side of the city costs nothing.

---

## 2. The riot as the player sees it

### 2.1 The map

- A city of up to **8 × 8 km** (64 km², about 250 times v0.1's 512 m district). It has several squares, avenues, side streets, alleys, parks, bridges, metro stations, a government quarter and police depots. Separate protests can happen in different places on the same map, and crowds move between them.
- Authored in **Tiled** (a free map editor) and exported as JSON:
  - Buildings, walls, parked cars, bins and barriers are **polygons**. These polygons are the exact collision shapes (§11.2).
  - Ground types (road, pavement, grass) are painted tile layers. Roads also form a **road graph** that vehicles drive on.
  - Entries (metro stations, bus stops, map edges), objectives, depots and police staging points are named objects.
- At load time, lookup structures are derived from the polygons (§7): a 0.5 m **nav grid** for routes and a 2 m **field grid** for crowd-scale effects. Neither one decides where people can stand; the polygons do. Both are stored in chunks, so a big map costs memory only where it has walls or activity.
- Positions are `f32` metres measured from the **centre of the map**, so no coordinate is ever more than 4,096 m from zero and resolution is about 0.5 mm or better everywhere. Grid lookups add the half-size offset back. A map bigger than 8 km would need positions stored relative to chunks.
- The camera looks **straight down in 2D** (§18). Buildings are seen as roofs, and "overhead" polygons in Tiled (arcades, tree canopies, bridges) are walkable underneath and drawn above people.

### 2.2 Who is in it

| Population | Typical count | Who controls them |
|---|---|---|
| Protesters (students, workers, elders, radicals) | 1k–50k | Nobody. Autonomous, open to influence. |
| Organizers | 5–50 | They carry the player's voice. Still autonomous: they walk, tire and get arrested. |
| Provocateurs | 0–20 | The state, covertly. Hidden among protesters. |
| Journalists and phone cameras | 10–200 | Autonomous. They record events (§2.7). |
| Bystanders | 0–5k | Autonomous. They can join, flee or film. |
| Police officers | 50–2k | Squad AI driven by the state's director (§14). |
| Vehicles (police vans, water cannon trucks, ambulances) | 0–100 | Driven along the road graph by the state's director, or by ambulance logic. |

Archetypes differ in their defaults: baseline anger, fear threshold, trust in organizers, stamina, willingness to use violence, and speed.

### 2.3 A person

Every person is an agent with four kinds of state:

- **Body:** an exact position and velocity, a size and mass (from their archetype and gear: an armoured officer with a shield is wider and heavier than a student), which way they are facing, their pose (standing, walking, running, crouching, fallen, carried, restrained), health and fatigue.
- **Emotions:** fear, anger, morale.
- **Belief:** what they think the plan is (an intent plus how garbled it reached them) and how much they trust the organizers.
- **Action:** what they are doing right now: march, hold, flee, rest, seek water or a medic, help someone injured, film, throw, build a barricade, chant, or go home.

They rethink about twice a second (§10). Each time, they score every possible action against their needs, their emotions and the plan as they understood it.

Each person also has a fixed **appearance** (body type, skin tone, clothes, hair, accessories) derived from their identity and archetype. It changes when they pick something up or drop it, such as a sign, a shield or a bottle. The renderer draws it (§18), and cameras and witnesses can use it to recognise someone.

### 2.4 Player verbs

None of these are orders. They are inputs that the crowd interprets.

| Verb | What actually happens |
|---|---|
| **Place objective** ("march to the square", "hold the bridge", "disperse to exits") | Creates a goal and its flow field. It only matters to agents who have heard about it. |
| **Set stance** (peaceful / defensive / confrontational) | Attached to messages. Agents weigh it against their archetype and their anger. |
| **Broadcast** | An organizer starts a message (objective + stance), which spreads like a rumour (§13). |
| **Drop supplies** (water, medic post, shields, barricade material) | Places supplies where your logistics reached. Agents decide whether to use them. |
| **Chant** | An organizer raises morale in a radius around them, at the cost of their own stamina. |

The player's real constraint is **reach**. Organizers are few and can be arrested. Comms quality (set by the campaign) decides how fast messages travel and how faithfully they arrive.

### 2.5 Police

- Police work in squads of 6–12 officers. Each squad has a doctrine state machine with these states: hold line, advance, kettle, disperse, snatch arrest, retreat.
- A director replans every second. It works from the threat field, its objectives, and the escalation cap given in the setup.
- The escalation ladder is: presence → lines and kettling → batons and arrests → tear gas and water cannon → mass arrests. The setup caps the highest rung. The director climbs the ladder in response to threat and violence.
- Officers are agents too. They tire, get scared and get angry, so a line can break.

### 2.6 Hazards and violence

- **Tear gas** is a diffusing cloud carried by the wind. It raises fear, lowers vision and stamina, and pushes people down its gradient.
- **Fire** is a cellular automaton running on a fuel map, and it produces smoke.
- **Water cannon** is a cone-shaped impulse. It knocks people down and soaks them, which raises fatigue.
- **Crush** comes from real body contact. Every person feels the pressure of the bodies pushing on them (§11.2). Sustained high pressure injures people and spreads panic. People who fall become obstacles that others trip over, which is how real crush cascades happen. The thresholds are tuned against the crowd-safety figure of about 5 people/m² (Fruin, Still). The player can cause a crush simply by calling everyone to one spot.
- **Violence is an action agents choose.** It is most likely with a confrontational stance, high anger, radicals or provocateurs. Every violent act is an event with an *initiator*, and the initiator is what footage cares about.

### 2.7 Footage and wildcards

- Cameras record the events they can see, each with a severity score. The footage tally tracks **who was seen starting it**. Police charging a peaceful crowd is a large legitimacy win for the movement. Protesters burning a car is a loss.
- Wildcards emerge from the simulation and are never scripted: a provocateur's thrown bottle, a martyr, a video that goes viral.

### 2.8 Time and distance

- **Riots take hours, and the city is big.** Crossing an 8 km map on foot takes about an hour and a half. The game has pause and 1×, 2×, 4× and 8× speed. Speed changes how many ticks run per real second, never the tick length, so results are the same at any speed.
- **People arrive over time** through entry points, following a turnout curve from the setup: a trickle in the morning, a surge after work, radicals arriving after dark. Turnout is a curve over time, not a single number.
- **The metro moves people between stations** after a delay, which makes the big map smaller, until the state closes stations.
- **Police reinforcements travel.** Vans drive from depots along the road graph. Where the police are stationed and how long they take to arrive both matter, and a barricade across a road delays them.
- **Distance limits influence.** An organizer only rallies the people around them, and word of mouth crawls across a big map. Phones carry messages across the city until an internet shutdown makes distance matter again.

---

## 3. Performance targets

Reference machine: a 4-core / 8-thread desktop CPU with AVX2 (any mainstream CPU from about 2015 onward).

| Scenario | Agents | Target | Fastest game speed it allows |
|---|---|---|---|
| Small protest | 2,000 | ≤ 0.7 ms / tick | 8× with room to spare |
| **Standard riot** | **10,000** | **≤ 3.0 ms / tick** | **8×** (160 ticks/s needs ≤ 6.25 ms) |
| Mass demonstration (stretch, M7) | 50,000 | ≤ 15 ms / tick | about 3× |
| Auto-resolve, 10-minute riot | 10,000 for 12,000 ticks | ≤ 40 s total | — |
| Auto-resolve, 1 simulated hour | 10,000 for 72,000 ticks | ≤ 2 min total, which needs sleeping (§7.6) | — |

All targets hold on any map up to 8 × 8 km.

Hard rules:

- **Map size is free.** The same crowd costs the same per tick, within 5%, on a 0.5 km map and on an 8 km map (§19).

- **Zero heap allocations per tick** once the simulation reaches steady state (§16).
- **Hot and warm state stays at or under 40 bytes per agent** (§6.2).
- **No locks and no atomics** in simulation code. Rayon's internal ones are the only exception.
- **The same seed, setup and commands always produce bit-identical state**, whatever the thread count (§12).

Starting points, all measured on a 4-core cloud VM with the profiles in §15:

- **Crowd prototype:** a naive `Vec<Vec<u32>>` grid, pairwise emotion contagion and flow-field following ran 5,000 agents at **0.79 ms/tick**.
- **Body prototype:** full body-to-body collision for 10,000 people (spatial hash, neighbour lists, two contact iterations) ran at **0.9–1.2 ms/tick**, and the cost barely changed between 1 and 5 people/m². Details are in §7.5.

Exact collision for every person costs more than v0.1's grid shortcut did, and that is why the targets above are 3 ms rather than 2.5 ms. It buys a crowd where nobody overlaps, pushes travel through bodies, and every person can be drawn as an individual.

### 3.1 Per-phase budget (10,000 agents)

| # | Phase | Budget (ms) |
|---|---|---|
| 1 | Apply commands | ~0 |
| 2 | Body hash, neighbour lists, field index, periodic reorder | 0.85 |
| 3 | Fields: scatter, diffuse, absorb | 0.30 |
| 4 | Think (1/10 of agents per tick) | 0.25 |
| 5 | Messages | 0.10 |
| 6 | Steering and facing | 0.25 |
| 7 | Bodies: contacts, walls, integrate, pose | 0.45 |
| 8 | Interactions → events → resolve | 0.25 |
| 9 | Police director (amortized) | 0.10 |
| 10 | Flow-field tiles (background, amortized) | 0.30 |
| 11 | Outcome accounting + render snapshot | 0.15 |
| | **Total** | **3.00** |

Phases 2 and 7 are backed by the body prototype. The rest are budgets, not measurements. Each phase gets a benchmark (§19) that is checked against this table.

---

## 4. Architecture

### 4.1 Crates

```
riotter/
├── Cargo.toml              workspace + build profiles (§15); members = crates/*
├── .cargo/config.toml      linker, target CPU
├── crates/
│   ├── riot_core/          math, ids, RNG, lane storage, grid geometry. No dependencies.
│   ├── riot_team/          the fork-join thread team the tick runs on (§12)
│   │   simulation, bottom to top:
│   ├── riot_defs/          definitions: archetypes, state layout, setup, tunables, map defs
│   ├── riot_snapshot/      the render snapshot (§18.1)
│   ├── riot_testkit/       test maps and scenarios
│   ├── riot_map/ riot_people/ riot_spatial/ riot_fields/ riot_flow/ riot_steer/ riot_physics/
│   │                       systems over part of the world
│   ├── riot_world/         World and the state of the world-level systems
│   ├── riot_think/ riot_messages/ riot_hazards/ riot_interact/ riot_tactics/ riot_police/ riot_outcome/
│   │                       phases over the whole world
│   ├── riot_sim/           the tick schedule; the facade every user names
│   │   data, depending on riot_defs only:
│   ├── riot_data_core/ riot_data_tiled/ riot_data_citygen/ riot_data_tunables/ riot_data_scenario/
│   ├── riot_data/          facade: RON scenario and tunable files, Tiled maps → riot_defs types
│   ├── riot_headless/      CLI: batch runs, benchmarks, determinism + allocation checks
│   │   rendering, depending on riot_defs and riot_snapshot only:
│   ├── riot_art/ riot_materials/ riot_sheets/ riot_sprites/   procedural pixel art, no GPU
│   ├── riot_mesh/          map meshes on the CPU
│   ├── riot_render/        snapshots → GPU (§18)
│   └── riot_app/           window and input (macroquad), input → commands, hot reload
└── data/
    ├── maps/               Tiled maps (JSON): polygons, tile layers, named objects
    ├── sprites/            layered sprite sheets + palettes
    ├── scenarios/          RiotSetup files
    └── tunables.ron
```

The simulation is split into small crates along its real seams, so an edit to one system rebuilds that crate, the few above it and the link. None of the simulation crates carries proc macros or heavy dependencies. Serde lives in the data crates, and they depend only on `riot_defs`, never on the simulation, so the data layer and the renderer compile in parallel with it and are not rebuilt by a simulation edit. `riot_sim`, `riot_data` and `riot_render` are facades that keep every older module path working.

### 4.2 Ownership rules

1. **The simulation knows nothing about rendering.** `riot_sim` has no window, no clock and no I/O.
2. **Single writer.** Every phase declares what it reads and what it writes. Inside a phase, a parallel task writes only to its own chunk of agents or its own range of cells. Anything that affects *another* agent goes through a field (§8) or an event (§11.3). This one rule gives us parallelism without locks, and it also gives us determinism.
3. **Time is a tick counter.** `tick: u32`, and `dt = 1/20 s` is a constant. Wall-clock time never enters the simulation. Game speed (§2.8) only changes how many ticks run per real second. A `u32` tick count lasts 6.8 years of game time.
4. **All input is a command stamped with a tick.** Player actions, tunable changes and debug actions are all commands. A replay is therefore just seed + setup + command log, and it reproduces everything.

### 4.3 The tick

```rust
pub fn tick(w: &mut World, cmds: &[Command]) {
    apply_commands(w, cmds);     //  1  validate + apply, in order
    rebuild_spatial(w);          //  2  body hash + neighbour lists, field index; reorder every N ticks
    update_fields(w);            //  3  scatter → diffuse/decay → agents absorb (contagion)
    think(w);                    //  4  1/K of agents re-score their actions
    spread_messages(w);          //  5  rumours move
    steer(w);                    //  6  desired velocity + anticipatory avoidance + facing
    move_bodies(w);              //  7  predict, contacts (Jacobi), walls, velocity, pressure, pose
    interact(w);                 //  8  emit events in parallel, resolve in order
    police_director(w);          //  9  every 20 ticks
    flow_fields(w);              // 10  swap in finished tiles, queue needed ones
    account_and_snapshot(w);     // 11  outcome tallies + render snapshot
    w.tick += 1;
}
```

| Phase | Reads | Writes (own agent/cell only unless noted) |
|---|---|---|
| spatial | pos | body hash, neighbour lists, field index; on reorder ticks it permutes all agent arrays |
| fields | agents per field cell, emotions, faction | field layers; then each agent's own emotions |
| think | fields, emotions, belief, needs, what they can see | action, target |
| messages | message layer, belief, trust | belief; message layer |
| steer | pos and vel (own + neighbour list), flow tiles, walls, action | vel, heading |
| move | pos, vel, neighbour lists, walls, props | pos, vel, pose |
| interact | pos, heading, action, neighbour lists | per-chunk event buffers, then sequential resolve |
| director | fields, squads, events | squad orders |

The phase order avoids double buffering almost everywhere. `steer` reads positions and writes velocities, and `move` reads velocities and writes positions. The one exception is inside `move`: each contact iteration reads every neighbour's position from the previous iteration and writes its own into a second buffer, then the buffers swap (§11.2).

Adjacent element-wise phases may be **fused** into a single pass per chunk (for example, the emotion absorb at the end of phase 3 and gathering inputs for phase 4), but only when the fusion does not break the single-writer rule.

---

## 5. World layout overview

| Table | Rows | Layout | Why |
|---|---|---|---|
| Civilians | 1k–50k | SoA, lane storage | Hot loops over every agent |
| Officers | 50–2k | SoA, same body layout | Same physics, different mind |
| Squads, messages, goals, supply drops, gas emitters, projectiles | ≤ a few hundred | `Vec<Struct>` (AoS) | Small, touched as whole records |
| Props (parked cars, bins, barricades) | ≤ a few thousand | `Vec<Struct>` (AoS) | Collide like walls, but can be pushed, built, burned or destroyed |
| Vehicles | ≤ 100 | `Vec<Struct>` (AoS) | Oriented boxes driven along the road graph; people collide with them like props |
| Wall segments | up to a few hundred thousand | SoA segment arrays, listed per 4 m sub-cell of the nav tiles that contain walls | Built once from the map polygons, never written during a riot |
| Field chunks | active ones only | Fixed-size chunks from a pool, plus a chunk table | Fields exist only where something is happening (§8) |
| Rare states (being carried, being arrested, carrying an item) | few | Sparse sets keyed by `stable_id` | Hot loops never check a flag that is almost always false |

Data-oriented does not mean SoA everywhere. Small tables are fine as plain structs. Rare states follow the idea of *existence-based processing* (R. Fabian, *Data-Oriented Design*): an agent belongs to a table *because* it has the state, so loops visit only the agents that actually have it.

---

## 6. Agent data

### 6.1 Roles

Protesters, organizers, provocateurs, journalists and bystanders all share the civilian table. Their role is just their archetype plus some flags. Officers have their own table with the same body layout.

### 6.2 Per-agent layout

**Hot**, touched by most phases every tick:

| Field | Type | Bytes |
|---|---|---|
| `pos_x`, `pos_y` | f32 | 8 |
| `vel_x`, `vel_y` | f32 | 8 |
| `pose` | `[u8; 4]`: heading, gait, stride phase, pressure | 4 |

**Warm**, touched by think, contagion and messages:

| Field | Type | Bytes |
|---|---|---|
| `emotions` | `[u8; 4]`: fear, anger, morale, fatigue | 4 |
| `health`, `action`, `archetype`, `flags` | u8 × 4 | 4 |
| `intent` | u16: goal id + stance | 2 |
| `belief` | u16: message id + fidelity | 2 |
| `stable_id` | u32 | 4 |

**Total: 36 bytes per agent.** That is 360 KB for 10k agents and 1.8 MB for 50k. The entire per-tick working set of a mass demonstration fits in the L3 cache of any desktop CPU.

**The pose bytes:**
- `heading`: which way the person faces, as 256 steps of about 1.4°. It is simulation state, not decoration: it sets what they can see (§10), which way an officer's shield blocks (§14), and what a camera films.
- `gait`: stand, walk, run, crouch, fallen, carried, restrained, throw, strike or film. It follows from the action and the speed.
- `stride`: walking phase, advanced by distance travelled, so a sprite's feet match the ground speed.
- `pressure`: how hard the surrounding bodies push on this person, from the last contact solve (§11.2). It drives crush injuries, falls and panic.

**Per-tick scratch**, rebuilt every tick and not counted as state: hash bucket (u32), field cell (u32), and a neighbour list of up to 8 `u16` indices plus a count. That is 25 bytes per agent, or 250 KB at 10k.

**Cold** data is touched rarely and accessed through `stable_id`: home and exit points, appearance (§2.3), inventory, group membership, stats, and a name seed.

**Per-archetype tables**, not per-agent storage, hold body radius, mass, maximum speed and turn rate. Gear flags add to them; for example, a shield widens an officer.

**Quantization.** Emotions and health are stored as `u8`, because 1/255 resolution is plenty for gameplay. Kernels widen them to f32, do the math, then narrow back with saturating rounding. Because the stored values are already `u8`, they can index 256-entry response-curve tables directly (§10). Positions stay f32, measured from the map centre, which gives about 0.5 mm resolution or better anywhere on an 8 km map (§2.1).

### 6.3 Lane storage

```rust
pub const LANES: usize = 8;
pub const CHUNK: usize = 64; // lanes per parallel task = 512 agents

/// Eight f32s, 32-byte aligned: one AVX2 register.
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C, align(32))]
pub struct F32x8(pub [f32; LANES]);

pub struct Bodies {
    pub pos_x: Vec<F32x8>,
    pub pos_y: Vec<F32x8>,
    pub vel_x: Vec<F32x8>,
    pub vel_y: Vec<F32x8>,
    pub len: usize, // live agents; lanes past `len` are inert padding
}
```

- Arrays are allocated **once**, at riot start, for the maximum population (which is known from the setup), and padded to a multiple of 8. Kernels therefore never need a remainder loop. Padding lanes have zero velocity and a dead flag that masks their results.
- Agent `i` lives at `[i / 8].0[i % 8]`.
- Kernels are plain loops over lanes, which LLVM autovectorizes:

```rust
pub fn integrate(b: &mut Bodies, dt: f32) {
    let Bodies { pos_x, pos_y, vel_x, vel_y, .. } = b;
    pos_x.par_chunks_mut(CHUNK)
        .zip(pos_y.par_chunks_mut(CHUNK))
        .zip(vel_x.par_chunks(CHUNK).zip(vel_y.par_chunks(CHUNK)))
        .for_each(|((px, py), (vx, vy))| {
            for (((px, py), vx), vy) in px.iter_mut().zip(py).zip(vx).zip(vy) {
                for l in 0..LANES {
                    px.0[l] += vx.0[l] * dt;
                    py.0[l] += vy.0[l] * dt;
                }
            }
        });
}
```

Checked on Rust 1.97 with `-C target-cpu=x86-64-v3`: this loop compiles to `ymm` `vmulps`/`vaddps`, with no FMA fusion (§12). `CHUNK` is fixed at 64 lanes (512 agents). That gives about 20 tasks at 10k agents and about 100 at 50k. It is never derived from the thread count, because chunk boundaries define the order of reductions (§12).

### 6.4 Identity and reordering

- The **dense index** is an agent's position in the arrays, and it changes whenever we reorder.
- The **`stable_id`** is assigned at spawn and never changes. It is used for RNG keys, cold data, external references (squad targets, events) and snapshot order.
- `slot_of[stable_id] → dense index` is rebuilt on every reorder.
- **Removal** (arrested, left the map, dead) works by setting a dead flag. The agent is compacted out at the next reorder. There is never a `swap_remove` in the middle of a tick.

---

## 7. Space: bodies, neighbours and grids

### 7.1 People are bodies; grids are lookups

Every person is a circle with a continuous `f32` position, a radius (about 0.23 m for most adults, about 0.32 m for an armoured officer with a shield) and a mass. Nothing snaps to a grid. Grids exist only to answer two kinds of question quickly: "who is near me?" and "what is the gas, fear or route here?". Each grid is sized for its own question:

| Structure | Cell | Size on an 8 km map | Memory and cost follow | Answers |
|---|---|---|---|---|
| Body hash | 0.65 m: one body diameter plus a margin | 2N buckets | number of people | Who is touching me, or about to? |
| Nav grid | 0.5 m, in 16 × 16-cell tiles | 16,384², stored per tile | tiles that contain walls | Which way to my goal? (§9) |
| Field grid | 2 m, in 64 × 64-cell chunks | 4096², only active chunks exist | area where something is happening | Crowd-scale effects: fear, gas, threat, messages (§8) |
| Wall lists | 4 m, static, four per nav tile | only on tiles that contain walls | number of wall segments | Which wall segments should I test? |

Walls themselves are exact polygons (§2.1). The wall lists only say which of their segments are worth testing, and they hang off the mixed nav tiles (§9), so open streets and solid buildings cost nothing. Nothing on this list is updated per tick across the whole map. That is what keeps map size free (pillar 7).

Grid dimensions are powers of two (smaller maps are padded with walls), so cell lookups are shifts and masks, and Morton codes cover the field grid exactly.

### 7.2 Stable bucket sort

The body hash is built with an allocation-free stable counting sort, which costs O(agents + buckets):

```rust
pub struct Buckets {
    pub start: Vec<u32>,  // buckets + 1: bucket b's agents are order[start[b]..start[b + 1]]
    pub cursor: Vec<u32>, // buckets: scratch
    pub order: Vec<u32>,  // dense agent indices, sorted by bucket, stable
}

impl Buckets {
    pub fn rebuild(&mut self, bucket_of: &[u32]) {
        self.start.fill(0);
        for &b in bucket_of { self.start[b as usize + 1] += 1; }
        for b in 1..self.start.len() { self.start[b] += self.start[b - 1]; }
        self.cursor.copy_from_slice(&self.start[..self.start.len() - 1]);
        for (i, &b) in bucket_of.iter().enumerate() {
            let slot = &mut self.cursor[b as usize];
            self.order[*slot as usize] = i as u32;
            *slot += 1;
        }
    }
}
```

**Body hash.** This is a dense spatial hash, the scheme from Matthias Müller's *Ten Minute Physics*. A body cell's coordinates are hashed into a table of 2N buckets (rounded up to a power of two):

```rust
#[inline]
pub fn body_bucket(cx: i32, cy: i32, mask: u32) -> u32 {
    ((cx as u32).wrapping_mul(0x9E37_79B1) ^ (cy as u32).wrapping_mul(0x85EB_CA77)) & mask
}
```

The cost depends on the number of people, not on the map size or the cell size, which is what lets cells be as small as one body. Two different cells that land in the same bucket only add a few extra candidates, and the distance test throws those out.

**Field index.** Each agent's key is the Morton code of its 2 m field cell (24 bits on an 8 km map). A big map has 16.8 million field cells, and a counting sort over every cell would cost more than the agents do. So the field index uses a two-pass LSD radix sort with 12 bits per pass on these keys. That is O(N) whatever the map size, still stable and allocation-free. One scan over the sorted keys then finds where each cell's run of agents starts and ends, and field scatters become segmented reductions over those runs (§8).

### 7.3 Neighbour lists

Once per tick, each agent scans the 3 × 3 body cells around it. It records up to **K = 8** people within reach (twice the largest radius plus a 0.15 m skin) as `u16` indices. Steering, contacts and interactions all read this list, so nothing scans the hash again during the tick.

- **8 is enough.** A circle in a packed crowd touches at most about 6 others (hexagonal packing). The prototype averaged 5.7 neighbours at 5 people/m² and never reached the cap.
- **It is deterministic.** List order is bucket order from a stable sort. If the cap is ever reached, it keeps the first 8 in that order.
- **`u16` indices** cap each population table at 65,535 agents, which is above the 50k stretch target, and halve the list's size.
- **Skip rebuilds when nobody moved far (M7).** Rebuild only once someone has moved more than half the skin since the last rebuild; that check is a deterministic max-reduction. Calm crowds then rebuild every few ticks. A stampede still rebuilds every tick, so the budget assumes every tick.

### 7.4 Spatial reorder

Every N ticks (a tunable that starts at 8), every hot and warm array is permuted into field-cell (Morton) order. People who are close in space then sit close in memory, which makes neighbour gathers and field scatters cache-friendly. In the prototype, sorting this way made the contact solve 15–20% faster. A reorder copies 36 bytes per agent, which is 360 KB at 10k agents.

### 7.5 Measured cost

Body prototype: scalar code, no SIMD, neighbour lists rebuilt every tick, two contact iterations, 4-core cloud VM.

| Crowd | Hash | Neighbour lists | Contacts | Total (ms/tick) | Avg. neighbours |
|---|---|---|---|---|---|
| 10k at 1/m² | 0.25 | 0.47 | 0.20 | 0.91 | 1.3 |
| 10k at 3/m² | 0.25 | 0.60 | 0.33 | 1.17 | 3.8 |
| 10k at 5/m² | 0.22 | 0.66 | 0.26 | 1.14 | 5.7 |
| 50k at 3/m² | 0.78 | 2.89 | 1.25 | 4.92 | 3.8 |
| 50k at 5/m² | 0.74 | 3.32 | 0.95 | 5.01 | 5.8 |

The cost barely changes with density. v0.1's 2 m cells would have meant scanning about 180 candidates per person at 5/m². That is why v0.1 fell back to a density approximation inside crowds and let people overlap, and v0.2 removes both.

### 7.6 Sleeping

On a big map, much of the crowd is standing still at any moment: listening to speeches, waiting at a police line, resting in a park. Physics engines save time by letting resting bodies sleep, and people get the same treatment. The decision is based only on simulation state, never on the camera, so it stays deterministic and auto-resolve gets the same benefit.

- **Falling asleep.** An agent sleeps after about 2 s (40 ticks) of being nearly still (under 0.05 m/s), calm, under low pressure, and with nothing new in its field of view.
- **What a sleeper skips:** steering, its own contact solve, pose updates and scheduled thinking. A sleeper still sits in the body hash as an obstacle, still feeds the fields (it counts towards density and fear), and still absorbs emotions, because those are cheap element-wise passes.
- **Waking up.** Any of these wakes an agent:
  - an awake body pushes into it (the awake agent emits a `Wake` event, resolved in order like every event);
  - a message reaches it;
  - a field in its chunk jumps past a threshold (gas, threat or fear);
  - an interaction targets it;
  - a slow timer wakes it to rethink every 4 seconds or so.
- **Only awake agents are processed.** The heavy phases (steer, move, think) iterate an `awake` index list rebuilt each tick, so sleepers cost nothing there.

How much this saves depends on the scenario: a crowd listening in a square is mostly asleep, a march is mostly awake. The 1-simulated-hour auto-resolve target depends on it, and it is measured in M7.

---

## 8. Fields

Fields carry the things a whole crowd shares: fear spreading through a square, a drifting gas cloud, how threatening an area feels. Bodies and touch are handled exactly in §7 and §11. A field replaces "every agent talks to everyone around them" with "every agent writes to the grid, then every agent reads the grid". That turns O(N·k) work into O(N + cells), and the result is the same whether the crowd is sparse or packed.

| Layer | Type | Written by | Read by |
|---|---|---|---|
| density | u16 count | scatter | route costs (§9), think (perceived crowding) |
| fear, anger | f32 mean | scatter → diffuse | contagion, think |
| threat | f32 | officers, violence events | think, flee steering |
| gas | f32 | emitters → advect + diffuse | think, steering, damage |
| chant | f32 | organizers | morale |
| camera | u8 | journalists, phones | think, footage |
| message | u16 (fidelity << 8 \| id) | carriers | messages (§13) |

**Storage is sparse.** Layers live in chunks of 64 × 64 cells (128 m × 128 m), which is 16 KB per f32 layer per chunk.
- A chunk exists only while it is **active**: it holds people or an emitter (gas, fire), or its values haven't yet decayed to zero.
- Chunks come from a pool allocated at riot start, so activating one allocates nothing. A small chunk table (64 × 64 entries for an 8 km map) maps chunk coordinates to pool slots.
- Diffusion reads a 1-cell halo from neighbouring chunks and activates a neighbour when something spreads into it.
- A chunk that has been empty and decayed for a while goes back to the pool.

Cost and memory therefore follow the area where something is happening, not the size of the map.

- **Sampling is bilinear.** Agents read a blend of the four nearest cells, so values change smoothly as someone walks and never jump in 2 m steps. The renderer draws gas and heatmaps from the same layers with GPU filtering (§18).
- **Scatter is a segmented reduction.** The field index (§7.2) sorts agents by field cell, so each cell's agents form one contiguous range, and a cell's value is a reduction over that range. The work is parallel over cells, and each cell is written by exactly one task: no atomics, no thread-local grids, and the result is deterministic. Each layer is written in a single streaming pass over the cells of every active chunk (empty cells write their neutral value), so layers never need a separate clear.
- **Diffuse/decay** uses a separable 3-tap blur (one horizontal pass, one vertical pass), parallel over rows, and it vectorizes cleanly. Gas uses semi-Lagrangian advection by the wind, plus diffusion and decay, which stays stable at any `dt`.
- **Absorb (contagion):** each agent does `emotion += rate · (field[cell] − emotion)`. That is an element-wise kernel in which each agent writes only to itself.
- **Multi-rate updates:** fear, anger, density and gas update every tick. Threat updates every 4 ticks and camera every 10 ticks. Each layer is tuned by benchmark.
- **Decay constants** such as `exp(−dt/τ)` are computed once at load time with `libm` and never inside a loop.

---

## 9. Flow fields

Flow fields give people their general direction. They never decide exactly where someone can stand; the wall polygons and body contacts do that (§11.2). So the nav grid can be fine enough to route through real doorways without any of its cells showing up as blocky movement.

- **There is one flow field per active goal:** an objective, an exit, a rally point or a police target. At most 32 can be active at once.
- **The nav grid is 0.5 m**, rasterized from the wall polygons with clearance. A nav cell is open only if the smallest body fits at its centre. Gaps narrower than a person close, and a 0.9 m doorway stays open.
- **The nav grid is stored per tile.** A tile is marked fully open, fully blocked or mixed, and only mixed tiles keep a 256-bit walkability bitmap (32 bytes). An 8 km map has 1,048,576 tiles (a 1 MB table of tile kinds), and in a city most of them are fully open or fully blocked. Rasterization runs in parallel per tile at load time.
- **Fields are built as tiles from the start**, following E. Emerson, "Crowd Pathfinding and Steering Using Flow Field Tiles" (*Game AI Pro*).
  - A tile is 16 × 16 nav cells (8 m × 8 m), and tiles connect through a portal graph.
  - **Routes are found on two levels**, as in HPA* (Botea, Müller and Schaeffer, "Near Optimal Hierarchical Path-Finding", 2004). First across 128 m regions (16 × 16 tiles, 4,096 regions on an 8 km map), then across tiles inside the chosen corridor. A 6 km route never searches the whole map.
  - Flow tiles are then built only for tiles on and near that route, when people actually enter them.
  - Each tile is a **direction field** of 256 bytes, one byte per cell holding one of 256 quantized angles (0 means "no direction"). Memory grows with the tiles in use, not with the map. Built tiles are cached per goal with an LRU limit.
- **Tile costs use Dial's algorithm** (a bucket-queue Dijkstra over integer costs). Step cost = base + density penalty + gas penalty + fire penalty, so crowds route around crushes, gas clouds and flames.
- **Routes are any-angle, not grid paths.** A grid Dijkstra measures Manhattan or octile distance, and people following it walk staircases: 37% further than the straight line across open ground at a 30° bearing. Two changes remove that.
  - The tile search is **Theta\***-style (Nash, Daniel, Koenig and Felner, "Theta\*: Any-Angle Path Planning on Grids", 2007). Every tile component keeps an **anchor**, the point its route next turns at (a goal target or the centre of a fully open tile), and a tile that sees its neighbour's anchor takes the straight-line cost to it instead of a grid step. Line of sight is propagated rather than traced: a tile sees an anchor when the tile its centre-to-anchor line enters first is fully open and sees it too, so the test is one lookup. The straight segment is charged each tile's own penalty, so crowds and gas still bend routes, and steps are 8-connected across open ground.
  - A flow tile is solved with the **Fast Marching Method** (Sethian, 1996): the Eikonal equation |∇T| = cell cost over its 16 × 16 cells, bounded by the goal's cells and a ring of cells from the downhill neighbour tiles, each valued by its distance to that neighbour's anchor. Each cell's direction is the field's downhill gradient at any of the 256 angles, and a cell never points across a blocked corner. A fully open, uncrowded tile whose ring heads for one anchor skips the solve: every cell points straight at it.
- **Sampling is bilinear.** Steering blends the four nearest cells, decoding angles through a 256-entry `(cos, sin)` lookup table, so there is no runtime trigonometry and movement doesn't look grid-aligned.
- **While a flow tile is pending,** people follow the tile costs: straight at their tile's anchor on open ground, otherwise across the border into the cheapest neighbouring tile. Only where no tile has a cost yet do they head straight for the target.
- **Tiles are built in the background with fixed latency.** A tile requested at tick T is built as a background task on the sim pool and is always swapped in at exactly tick T + L (L = 10 by default). If it isn't finished by then, the simulation waits. Determinism never depends on how fast the task ran.
- **Dynamic costs** (density, gas) are refreshed once per second for each goal, staggered across ticks.

---

## 10. Minds: think

- **Staggered thinking.** Each agent rethinks every K = 10 ticks, i.e. every 0.5 s, which is roughly human reaction time to a changing crowd. An agent's phase within that cycle is `mix(stable_id) % K`, so every tick handles 1/K of the crowd and the per-tick cost stays flat.
- **Emergencies skip the queue.** Being gassed, crushed or hit sets a flag, and the agent thinks on the next tick.
- **People see what they face.** Perception uses a field of view around `heading`: wide when calm, narrow when panicked. Things behind someone reach them only through fields (noise, fear, gas) and rumours. A police line advancing on a crowd's back is therefore a real surprise.
- **Utility AI.** Each action's score is the product of several *considerations*. The agent picks the highest-scoring action, and deterministic noise from the RNG keeps crowds varied.
- **Considerations are 256-entry lookup tables** indexed directly by the quantized `u8` input. That means one load per consideration, no transcendental functions, no branches and fully deterministic results.
- **Archetype weights** live in a small `[archetype][action][consideration]` table that stays in L1 cache.
- **Action-major batches.** The agents thinking this tick are gathered into a preallocated scratch SoA. Scoring then runs one action at a time across the whole batch, so the inner loops are uniform and vectorize well. A final argmax pass picks each agent's action.

Actions in v1: March (follow the goal), Hold (stay near the goal, face the threat), Flee (move down the threat/gas gradient toward exits), Rest, SeekSupply, Help (carry someone injured), Film, Throw, Barricade, Chant (organizers only), GoHome (leave the map, which is a real loss of turnout).

---

## 11. Movement and interaction

### 11.1 Steering

**Desired velocity:**
- March, Flee, GoHome, SeekSupply: flow-field direction × preferred speed.
- Hold: toward the hold point, damped.
- Everything else: near zero.

**Preferred speed** = archetype base × (1 − fatigue) × injury factor × panic boost.

**Avoidance** reads the neighbour list (§7.3) and is anticipatory. People steer away from whoever they are *about* to bump into, judged by time to collision, not only from whoever they already touch (Karamouzas, Skinner and Guy, "Universal power law governing pedestrian interactions", *Physical Review Letters*, 2014). Wall segments ahead are avoided the same way.

Finally, `vel = lerp(vel, desired + avoidance, response)`, clamped to the agent's maximum speed. The `response` factor comes from the archetype; elders react more slowly.

**Facing.** `heading` turns toward the velocity while moving. When standing, it turns toward what the person is attending to: a police line, an objective, or what they are filming. The turn rate is limited per archetype.

### 11.2 Bodies: contacts, walls, integrate

1. **Predict.** `pos* = pos + vel · dt` (the §6.3 kernel).
2. **Contacts.** Two Jacobi iterations of position-based contact constraints, in the style of Weiss et al., "Position-Based Real-Time Simulation of Large Crowds" (raised to four when the previous tick's peak pressure crossed a threshold).
   - Each agent sums the push-out from every overlapping body in its neighbour list, weighted by mass, so an armoured officer moves a protester more than the other way round.
   - It writes only its own corrected position, into a second buffer.
   - Jacobi, unlike Gauss-Seidel, keeps the single-writer rule, and its results don't depend on order or thread count.
   - In the prototype, no pair stayed more than 20% overlapped at 3/m². At 5/m², 4–20 of 50,000 people did with two iterations.
3. **Walls, props and vehicles.** Circle-versus-segment push-out against the segments in the wall lists (§7.1), plus props (parked cars, bins, barricades) and vehicles (oriented boxes) from their small tables. Vehicles are kinematic: they push people but are not pushed back, unless a crowd rocks or overturns one, which is an interaction event.
4. **Velocity from movement.** `vel = (pos_new − pos) / dt`, so pushes carry momentum and shoves travel through a packed crowd as waves.
5. **Pressure.** The total push-out each person received is stored in `pressure`. Sustained high pressure causes crush injuries, falls and panic. Fallen people become low obstacles that make falls around them more likely, which is the cascade behind real crowd crushes.
6. **Pose.** `gait` follows from the action and the speed, and `stride` advances by distance travelled divided by stride length.

### 11.3 Interactions and events

- Interactions only visit agents whose current action interacts: throwing, striking, arresting, helping, picking things up. The per-action lists are built during think. Melee, arrests and helping use the neighbour list and the attacker's facing.
- Throwing targets come from the officers table (a few hundred rows), not from neighbour scans. Projectiles live in a small AoS table. A bottle thrown at 15 m/s moves about 0.75 m per tick, which is more than a body width, so hits are tested along the whole segment it travelled. Bodies are found through the body hash; shields are tested as oriented segments.
- Events are small `Copy` values (≤ 16 bytes) appended to **per-chunk buffers**. Those buffers are preallocated, cleared and reused every tick, and each one is a separate allocation so chunks never share cache lines. After the parallel pass, the buffers are processed in chunk order and resolved sequentially. Conflicts, such as two officers grabbing the same person, are settled by that order: the first event wins.
- Resolved events update health, flags, arrests and the outcome tallies, and they feed footage (§2.7).

---

## 12. Parallelism and determinism

**The rules:**

1. **Fixed chunking.** Parallel work is split into chunks of `CHUNK` lanes, never by thread count. Each chunk writes only its own slice.
2. **Two-step reductions.** Per-chunk partial results are computed in parallel, each in a fixed lane order. They are then summed sequentially in chunk order. LLVM will not vectorize float reductions by itself (that would reorder the additions), so kernels use explicit 8-lane accumulators, which are both fast and fixed-order:

```rust
/// Sum in a fixed order, independent of thread count. Reuses `partials`' buffer.
pub fn sum(xs: &[F32x8], partials: &mut Vec<f32>) -> f32 {
    xs.par_chunks(CHUNK)
        .map(|chunk| {
            let mut acc = [0.0f32; LANES];
            for v in chunk {
                for l in 0..LANES { acc[l] += v.0[l]; }
            }
            acc.iter().sum::<f32>()
        })
        .collect_into_vec(partials);
    partials.iter().sum()
}
```

3. **Counter-based RNG with no shared state.** Every random number is a pure function of `(seed, tick, stable_id, stream)`. Any thread can compute any agent's number, in any order:

```rust
/// SplitMix64 finalizer.
#[inline]
pub const fn mix(mut x: u64) -> u64 {
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

#[derive(Clone, Copy)]
#[repr(u64)]
pub enum Stream { Think = 1, Message = 2, Violence = 3, Spawn = 4 }

#[inline]
pub fn roll(seed: u64, tick: u32, id: u32, s: Stream) -> u64 {
    mix(seed ^ mix((tick as u64) << 32 | id as u64) ^ (s as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15))
}

/// Uniform in [0, 1) from the top 24 bits.
#[inline]
pub fn unit(bits: u64) -> f32 {
    (bits >> 40) as f32 * (1.0 / 16_777_216.0)
}
```

4. **Events resolve in chunk order** (§11.3).
5. **Contacts are Jacobi iterations** (§11.2): each agent reads the previous iteration's positions and writes only its own. Neighbour lists come from a stable sort and keep the first K in bucket order (§7.3).
6. **Simulation logic never iterates a `HashMap`, orders anything by pointer address, reads the wall clock, or uses thread ids.**
7. **Float discipline.**
   - Never enable fast-math. Rust doesn't use it, and we never add it.
   - Never use approximate-reciprocal intrinsics.
   - Never call the std transcendentals (`sin`, `exp`, `powf`, …) inside the simulation. Use lookup tables, polynomials, or `libm`, which is pure Rust and gives identical results everywhere.
   - `+ − × ÷` and `sqrt` are correctly rounded IEEE operations, so they are safe to use anywhere.
   - Rust never fuses a multiply and an add on its own (verified in §6.3, even on x86-64-v3), so AVX2 builds and baseline builds produce identical results.
   - `f32::mul_add` is always correctly rounded and therefore deterministic, but without hardware FMA it falls back to a slow software routine. Keep it out of hot loops unless the build targets FMA.
8. **Background jobs have fixed tick latency** (§9).
9. **Tunable changes are commands**, applied at a specific tick and recorded in the log (§4.2).
10. **Small crowds use the same chunk boundaries.** Below about 2k agents, a phase may run single-threaded, decided by benchmark. It iterates exactly the same chunks in the same order, so the result is identical.

**Thread pool:** the simulation gets a dedicated rayon pool with (cores − 1) threads, leaving the main thread free for input and rendering. Headless runs use every core.

**Verified by tests (§19):** the same scenario run at 1, 2, 4 and 8 threads must give an identical state hash on every tick, and the x86-64-v3 and baseline builds must give identical hashes. Determinism between x86 and ARM is expected but not guaranteed in v1. The rules above keep it achievable if we later want lockstep multiplayer.

---

## 13. Messages (rumour propagation)

**Data:**
- A message table (at most 255 live messages). Each row holds an id, the originating organizer, the tick, the objective, the stance and a base fidelity.
- Per agent, `belief: u16` holding the message id and the fidelity the agent heard it at.
- Per field cell, a message layer `u16 = fidelity << 8 | id`. Because fidelity is in the high byte, a plain `max` keeps the clearest message, and on a tie the higher (newer) id wins.

**Each tick:**
1. **Carriers scatter** `(fidelity − hop_loss, id)` into the layer, using a segmented max-reduction exactly like the fields.
2. **Word of mouth.** A 3 × 3 max filter dilates the layer by one cell every M ticks, with M set by comms quality. Each agent then reads a single cell.
3. **Adoption.** An agent with no belief, or with an older or less clear one, rolls to adopt the message with probability `hear_rate × trust` (RNG stream `Message`).
4. **Phones.** Each tick, an agent with a phone and no belief rolls a small chance to receive the latest broadcast directly, at reduced fidelity. This models group chats. An internet shutdown (set in the setup) drops that chance to zero, leaving only word of mouth.
5. **Garbling.** At low fidelity, the objective can be swapped for a nearby goal, and the stance drifts toward the archetype's default: radicals hear "confrontational", elders hear "peaceful". The outcome is deterministic via the RNG.
6. **Following is still a choice.** The heard intent is just one consideration in think (§10), weighted by trust and morale.

Organizers rebroadcast at full fidelity around themselves, so **where your organizers stand matters**, and arresting organizers is the police's strongest counter.

---

## 14. Police

- **Squads** are a small AoS table (at most 200 rows). Each row holds the doctrine state, formation, target, officers (as stable ids), morale and stamina.
- **The director** runs every 20 ticks. It reads the threat and density fields and clusters the crowd by running union-find over dense cells in the active field chunks. It then picks responses within the escalation cap and assigns squads to them.
- **Deployment across the city.** Squads start at depots and staging points, and reinforcements travel by van along the road graph. The director has to decide where to commit forces before it knows where the crowd will go, and travel time is part of that decision.
- **Officers are heavier and wider** than protesters. A **shield** is an oriented segment in front of its officer: it blocks projectiles from the front and pushes in contacts. A shield wall advancing into a crowd needs no special code, because it is just bodies with mass, so pushing matches between a line and a crowd emerge on their own.
- **Formations:**
  - A line is a set of slots along a segment.
  - A kettle is a set of slots around a cluster's boundary that contracts over time.
  - Officers steer toward their slots using the same steering kernel, with a slot target in place of a flow field.
- **Officer minds** run a simpler utility AI that weighs staying in formation against breaking, based on fear, anger and fatigue. A broken line is therefore emergent, not scripted.
- **Snatch arrests:** a squad targets one specific person. Organizers are targeted first once cameras or surveillance have identified them.

### 14.1 The commander

Above the squads sits a **commander** (`riot_sim::tactics`), the strategic layer of the director. Every replan it reads the crowds and plans what to do about each; a few cheap checks (barriers going up and wearing down, ambushes, kettles) run every tick.

- **Intelligence.** The director's clusters are matched to **tracked crowds** across replans (by predicted position and by bounding box, since a stream's centroid jumps when clusters merge or split), each with a smoothed velocity, mood and **heat**: the violence logged in and around it over the last 20 s. Where a crowd is going is read from the plan its people follow (the state watches the group chats), falling back to the objective it moves toward. *How* it will get there comes from the crowd's own flow-field corridor: following the tile anchors (§9) gives the turning points of the exact route it will walk, street by street, before any flow tile is built. Each crowd gets an ETA to the **protected site** (map areas tagged `government`) its route threatens.
- **Operations.** Every crowd gets one, from its size, mood, heat, the rung and the site: *watch* (lines at a distance), *escort* (squads walking alongside a march), *cordon* (barricade its route), *protect* (lines between it and the site it reached), *contain* (a kettle) or *disperse* (warn, then gas, water and advancing lines). Violence somewhere else does not make a quiet crowd violent.
- **Cordons.** A crowd that will reach a site within a quarter of an hour is met at the narrowest street on its predicted route that a squad can reach and barricade before the crowd's front gets there (street width comes from rays to the walls on either side, timing from the nearest squad's distance and the crowd's speed), preferring streets 30–60 m out from the site. A squad jogs there and its officers set up steel **police barriers** (a prop kind of their own) wall to wall, one each, never on top of anyone, then hold behind them. Officers pass through their own barriers, and police routes ignore them. Barriers are stamped into a field layer, `Block`, which routes price like a wall that can still be pushed through (`flow_block_cost`, with tile steps allowed up to 16× their length), so a crowd that can go round does; the commander reads its new route and cordons that street too, up to seven barricades per site, until the crowd has to push on one. Pushing wears a barrier down (`barrier_push`: pressure × anger, plus anger, plus throwers and builders at it) and officers bracing it from behind take most of that off (`barrier_brace`); a toppled barrier opens the street, the crowd near it cheers, the cordon is *breached*, and the police fall back to the next street toward the site. Four people crushed against one barrier make the police take it away. A cordon the crowd reaches first is given up; one no crowd threatening its site has come within 300 m of for three minutes is taken down.
- **Ambushes.** Where a side street opens onto the route 18–60 m short of a cordon, out of sight of a crowd coming up the route, a nearby riot squad waits around the corner (at any rung: hiding is not force). Lurking officers crouch and add nothing to the threat field, so the crowd, which sees threat only in its field of view, does not know they are there. Once batons are allowed, when the crowd fights at the barricade (violence at it, a worn or breached barrier) or streams past the corner angry, the squad charges out across the street, cutting the crowd in two, then sweeps the rear part back down the route at a walk while the barricade's officers hold the front; charging officers strike whoever is in reach and grab the people the police have identified. An ambush the crowd stumbles onto charges at once. Whether the barricade's front was clearer half a minute later is remembered.
- **Arrests.** Officers who see a violent act within 25 m identify whoever did it, as a camera would (spotters). Snatch squads wait just behind a line facing the crowd and go for the most valuable target in reach: molotov carriers, then people filmed or spotted starting violence (more so recently), then known organizers (first of all at the mass-arrests rung); lines pull in identified people in front of them now and then.
- **Use of force.** Escalation weighs violence against crowds at the protected site, officers hurt and barricades breached, and climbs at most one rung per 30 s (10 s under heavy pressure); footage of police violence makes the state more careful. Before the first canister a crowd hears a dispersal order (the timid start to leave). Gas comes in volleys of up to three canisters (at most one volley every 5 s force-wide, and none within 30 m of one fired in the last 20 s) from riot officers who can see where they land, aimed at the crowd's violent core, upwind of it so the wind carries the gas on, never on top of officers, at the thinner edge rather than into a packed mass, and never into a kettle. Water cannon spray the violent core, sweeping across it, and back away from fire.
- **Kettles** close as before; one held a minute and calmer (or four minutes anyway) opens a **filter gate** toward the nearest way out away from the site, and officers stand at its posts.
- **Adaptation.** Once barricades have fallen, a crowd at one is enough to send a second squad; ambushes that cleared a barricade's front are used more, ones that did not, less; the most worn-out squad on a line (fatigue past `relief_fatigue`, low morale or half its officers gone) is relieved by a fresh reserve squad.

Everything the commander says it does goes out as a **radio call** (a ring of the last eight, in the snapshot), which the app shows as the police radio and `riot_headless police` prints, with the commander's crowds, cordons and squads, every so often.

---

## 15. Build configuration

```toml
# Cargo.toml (workspace)
[profile.dev]
opt-level = 1                  # our crates: quick to compile, fast enough for 10k agents
debug = "line-tables-only"

[profile.dev.package."*"]
opt-level = 3                  # dependencies: optimized once, then cached
debug = false

[profile.release]
opt-level = 3
lto = "thin"                   # as fast as fat LTO here, and links in parallel
codegen-units = 16
panic = "abort"
debug = "line-tables-only"     # readable profiler stacks

[profile.profiling]            # cargo build --profile profiling
inherits = "release"
debug = true

[profile.dist]                 # shipped builds
inherits = "release"
lto = "fat"
codegen-units = 1
debug = false
strip = true
```

Measured with the prototype on a 4-core cloud VM:
- Rebuilding after a simulation edit takes about 0.5 s.
- With the simulation grown to 17k lines in one crate, a dev rebuild took 1.4 s and a release rebuild 40 s. Split into crates (§4.1) with thin LTO, a steering edit rebuilds in 0.5 s (dev) and 12 s (release).
- The dev profile runs the simulation at release speed, 4.5× faster than Cargo's default dev profile.

- **Target CPU:** release and dist builds target `x86-64-v3` (AVX2, FMA, BMI2), set in `.cargo/config.toml`. A baseline `x86-64` build ships alongside for older CPUs, and the two must produce identical state hashes (§12).
- **Linking:** on x86-64 Linux, Rust already links with LLD by default (verified on Rust 1.97). On Windows, set `linker = "rust-lld.exe"`. The default macOS linker is fine.
- **Later:** profile-guided optimization (PGO) via `cargo-pgo`, trained on the headless benchmark scenarios.
- **Lints:**
  - `riot_sim` sets `#![deny(unsafe_code)]`, with the `kernels` module as the only exception (§16).
  - CI runs clippy with `-D warnings`.

---

## 16. Rust rules for hot code

**Memory**
- Allocate everything at riot start, with sizes taken from the setup. Per-tick scratch space is preallocated `Vec`s that get cleared and never shrunk.
- **Zero allocations per tick.** A test with a counting global allocator runs 1,000 ticks after warm-up and requires exactly zero allocations.
- No `Box<dyn Trait>`, `Rc`/`RefCell`, `String` or `HashMap` in hot paths. Use enums with `match`, and dense arrays indexed by `u32`/`u16`/`u8` ids.
- Store indices as `u32` or smaller, never `usize`. That halves the memory traffic. Neighbour lists use `u16` (§7.3).

**Loops**
- Iterate with `zip` and `chunks_exact` over slices. Put `assert_eq!(a.len(), b.len())` before a loop so LLVM can remove the bounds checks inside it.
- Inner loops must not contain calls that fail to inline. Put `#[inline]` on small cross-crate functions in `riot_core`, because dev builds have no LTO.
- No division inside loops; multiply by a precomputed reciprocal.
- Keep branches light. For cheap cases, compute both sides and select. For expensive per-action work, run separate kernels over the per-action lists.
- Mark rare paths `#[cold]`.

**SIMD**, in order of preference:
1. Autovectorized lane loops (§6.3). Check the assembly with `cargo show-asm` for every kernel in the budget table; the v3 build should use `ymm` registers.
2. The `wide` crate (stable, portable SIMD) for kernels LLVM won't vectorize, such as gathers and compare-and-select patterns.
3. `std::arch` intrinsics behind `#[target_feature]`, with a scalar fallback.

Once `std::simd` is stable, migrate the `wide` code to it.

**Unsafe**
- Unsafe code is allowed only in `riot_sim::kernels`.
- Every unsafe block has a `// SAFETY:` comment.
- Every unsafe kernel has a safe scalar reference implementation, plus a property test that requires bit-identical output from both.
- The scalar paths run under Miri.

**Dependencies**
- The simulation crates depend on `riot_core`, `riot_team`, `rayon`, `bytemuck` and `libm`, and nothing else. Any new dependency has to justify its cost in compile time and its risk to determinism.

---

## 17. Boundary with the campaign

```rust
pub struct RiotSetup {
    pub seed: u64,
    pub map: MapId,
    pub crowd: [u32; ARCHETYPES],   // total turnout per archetype
    pub arrivals: Vec<Arrival>,     // turnout curve per entry point over time
    pub organizers: u16,
    pub supplies: Supplies,         // what the movement's logistics delivered
    pub police: PoliceForce,        // squads, doctrine, depots, vans
    pub escalation_cap: Escalation,
    pub comms: Comms,               // phones / degraded / shutdown
    pub weather: Weather,           // wind for gas, heat for thirst
    pub objectives: Vec<ObjectiveDef>,
}

pub struct RiotOutcome {
    pub ticks: u32,
    pub ground_held: f32,           // objective-weighted fraction of time held
    pub arrests: u32,
    pub injuries: u32,
    pub deaths: u32,
    pub supplies_used: Supplies,
    pub footage: Footage,           // blame and reach, per side
    pub final_morale: f32,
    pub notable: Vec<NotableEvent>, // martyrs, viral moments, organizers arrested
}
```

`Vec` is fine here, because these structs are built once per riot and never touched inside the tick. **Auto-resolve** runs this same simulation headless, as fast as the CPU allows, and returns the same `RiotOutcome`. There is no separate formula that could drift away from the real simulation.

---

## 18. Rendering and sprites

The renderer draws **every person as their own sprite**, from data the simulation already has. It never reads `World` directly; it only reads snapshots.

### 18.1 Snapshot

After every tick, the simulation writes a snapshot into a triple buffer. Each person takes 16 bytes, which is 800 KB at 50k:

| Field | Bytes | Drives |
|---|---|---|
| `pos_x`, `pos_y` (f32) | 8 | where the sprite stands |
| `heading` | 1 | the sprite's rotation |
| `gait` | 1 | which animation (walk, run, crouch, fallen, throw, …) |
| `stride` | 1 | which animation frame, so feet match ground speed |
| `flags` | 1 | injured, carrying, shield raised, filming, … |
| `appearance` (u32) | 4 | body type, skin tone, clothes palette, hair, held item |

The snapshot is **written in `stable_id` order**: it gathers through `slot_of`, in parallel over output chunks. Snapshot slot *n* is therefore always the same person, so the renderer can interpolate between snapshots even though the simulation reorders agents.

### 18.2 Drawing

- **Pixel art at the art's own density.** Every sheet is drawn as pixel art at four densities (48, 24, 12 and 6 px/m). The world is rendered into an offscreen buffer at the densest of these that fits the zoom, with the camera snapped to whole buffer pixels, so textures and sprites are sampled texel for pixel; a sharp-bilinear composite scales the buffer to the screen. Zoomed out past 6 px/m the buffer matches the screen.
- **Interpolation runs on the GPU.** The two most recent snapshots are uploaded as instance buffers, and the vertex shader blends position and heading with the frame's alpha. The CPU does no per-person work per frame.
- **Layered "paper doll" sprites, drawn from above.** Body, clothes, hair and held item are separate sheets. The camera looks straight down, so each sheet is drawn facing one way and the vertex shader rotates it by `heading`. Facing turns smoothly in 1.4° steps, and each gait needs one strip of frames, not 8 or 16 directions. Colours come from palette swaps in the shader, indexed by `appearance`, so a handful of sheets yields thousands of distinct-looking people. Each layer is one instanced draw call.
- **Draw order by layer, no sorting.** Layers draw bottom to top:
  1. ground, decals, shadows and props
  2. fallen people
  3. people's shadows and standing people
  4. vehicles
  5. projectiles
  6. gas and smoke
  7. roofs, rooftop plant and tree canopies

  Bodies never overlap (§11.2), so sprites in the same layer don't need sorting either. Overhead polygons turn see-through where people stand under them.
- **Level of detail.**
  - Far zoom, up to the whole 8 km city: each person is a small quad coloured by state, or the crowd becomes a density texture per field chunk.
  - Middle zoom: single-layer sprites.
  - Close zoom: full layered sprites.
- **Culling.** Each tick, the renderer collects the people in field chunks that overlap the camera, plus a margin, and uploads only those instances. On a big map most people are off-screen at close zoom.
- **Animation timing.** Frames come from `stride`, which is simulation data. Purely cosmetic motion (breathing, flags waving, hair) runs on the render clock and never feeds back.
- **Effects.** Gas is drawn from the gas layer as a smooth texture (GPU bilinear filtering), its translucency quantized and ordered-dithered on the world's pixel grid, plus particles. Fire and water cannon are particles drawn as pixel blobs.
- **Shadows.** Buildings, walls, trees, props, vehicles and people cast shadows towards the south-east. All shadows are drawn at one depth with a less-than test, so where they overlap they darken once.
- **Time of day.** The composite grades colour by the clock: warm at dusk, dark blue at night. At dusk and night street lamps, fires, molotovs and police light bars draw pools of light into a light buffer, and the scene is lit by the day's light plus that buffer.
- **Debug overlays** draw field layers (density, fear, gas, messages) and contact pressure as heatmaps. These are the main tool for tuning emergent behaviour.

### 18.3 Technology

`riot_app` keeps macroquad for the window, input and debug drawing. The sprite pipeline is a custom instanced pipeline on macroquad's own miniquad backend, which supports per-instance vertex buffers. That keeps the dependency list short. We move to `wgpu` only if we need compute shaders, for example GPU culling for very large crowds.

---

## 19. Testing, benchmarks, tooling

**Tests**
- **Determinism:** run each scenario × seed at 1, 2, 4 and 8 threads, and require an identical state hash on every tick. The state hash is a 64-bit `mix` chain over `bytemuck` views of every array.
- **Build determinism:** the x86-64-v3 and baseline builds must produce the same final hash. This runs in CI.
- **Zero allocation:** the counting-allocator test described in §16.
- **Kernels:** each optimized kernel is property-tested (proptest) against its scalar reference, and the outputs must match bit for bit.
- **Behaviour (headless):** assertions on emergent outcomes. These catch regressions while we tune. Examples:
  - "A gas canister in a crowd cuts density within 10 m by ≥ 50% within 10 s."
  - "A kettle closes around a 500-person cluster within 60 s."
  - "After a 5/m² crowd settles, fewer than 0.1% of touching pairs overlap by more than 20%."
  - "A bottle thrown at a shield line never passes through an officer."
- **Map-size independence:** the same 10k crowd scenario runs on a 0.5 km map and on an 8 km map, and the per-tick cost must match within 5%.

**Benchmarks**
- **Per phase:** criterion benches at 2k, 10k and 50k agents on fixed scenarios.
- **Whole tick:** `riot_headless --bench` writes per-phase ms/tick (p50 and p99) to CSV.
- **Budget check:** results are compared against the budget table in §3.1. A regression of more than 10% fails CI.

**Profiling**
- **Built-in timers:** a timer wraps each phase. It lives outside the simulation logic, so it can't affect results, and it feeds an on-screen overlay.
- **Tracy:** integrated via `tracing-tracy` or `profiling`, behind a `profile` feature flag.
- **perf / samply / cargo flamegraph:** run against the `profiling` profile.
- **cargo show-asm and cargo bloat:** for checking vectorization and code size.

**Iteration loop**
- **Tunables hot-reload:** `tunables.ron` reloads while the game runs, via a file watcher in `riot_app`. A change becomes a command applied on the next tick.
- **Code hot reload:** evaluate `subsecond` during M0. The fallback is to snapshot `World` to disk, rebuild, reload the snapshot, and keep watching the same riot. Because the world is made entirely of `Pod` arrays, a snapshot is a single `bytemuck` write.

---

## 20. Milestones

Each milestone ends with a performance gate. All numbers are on the §3 reference machine.

| | Content | Exit gate |
|---|---|---|
| **M0** Skeleton | workspace and profiles, `World`, fixed tick, RNG, lane storage, headless runner, determinism and allocation tests, bench harness | tests green in CI; benches report per phase |
| **M1** Bodies move | Tiled map → polygons, nav tiles with wall lists and two-level routing, body hash, neighbour lists, steering, facing, contacts, pose, arrivals, game speed, debug renderer + overlays | 10k agents ≤ 2.0 ms/tick on an 8 km map |
| **M2** Minds | sparse field chunks, emotions, contagion, field of view, utility AI, staggered think | 10k ≤ 2.5 ms; map-size test passes |
| **M3** Influence | organizers, messages, objectives, stance, supply drops, chants | 10k ≤ 2.6 ms |
| **M4** Police | squads, director, depots, vans on the road graph, lines, kettles, arrests, shields, mass | 10k + 300 officers ≤ 2.8 ms |
| **M5** Hazards & violence | gas, fire, water cannon trucks, projectiles, injuries, crush from pressure, falls, medics | 10k ≤ 3.0 ms |
| **M6** Outcome | events log, footage, `RiotOutcome`, auto-resolve | 10-minute 10k riot ≤ 40 s headless |
| **M7** Scale | sleeping, skip-when-still neighbour lists, SIMD contacts, parallel radix sort, PGO | 50k ≤ 15 ms; 1 simulated hour at 10k ≤ 2 min |
| **M8** Sprites | 16-byte snapshot, instanced layered sprites, palette swaps, level of detail, effects | 50k visible people at 60 fps on a mid-range GPU; 10k on integrated graphics |

---

## 21. Open questions

- **Fog of war:** does the player see only what their organizers and cameras see? It fits "you don't control everything": you don't see everything either.
- **Reaction speed:** should the think interval vary by archetype (radicals react faster, elders slower)?
- **Fixed-point positions:** only worth it if we commit to cross-platform lockstep multiplayer.
- **Field cell size:** 2 m or 1 m for gas and fear? Finer looks better up close but costs 4× per layer.
- **Officer index:** should officers share the civilian body hash (tagged entries) or keep their own? Decide by benchmark in M4.
- **Game speed:** crossing an 8 km map on foot takes about 1.5 hours. Is 8× enough, or should 16× unlock once sleeping (§7.6) makes it affordable? Should the game speed up automatically when nothing is happening near the player's people?
- **Map authoring beyond Tiled:** for example, importing OpenStreetMap street layouts to build fictional city centres. That becomes more attractive now that a map is 8 km across.

---

## 22. Implementation status

What the code does where it departs from or settles something in this document, as of the first implementation (October 2026).

**Settled open questions**
- **Officer index (§21):** officers share the civilian table and body hash, tagged by archetype. One contact solve sees everyone, and police doctrine reads officers through a small per-officer table (squad, slot).
- **Field cell size (§21):** 2 m for every layer.
- **Reaction speed (§21):** one think interval for everyone; emergencies (gas, crush, a blow, a new plan) think on the next tick.

**Deviations**
- **Emotions are dithered (§6.2, §8).** Emotions are bytes, and rounding to nearest after each tick erased every rate below half a step (about 0.002 per tick): relaxation, threat-driven anger and most contagion never moved. `absorb` writes them back with an unbiased random dither from the counter RNG, so rates act as written, on average.
- **Several announced plans (§2.4, §13).** A scenario may announce more than one objective. Each arrival comes for one of them, drawn by objective weight and by how well its stance suits the archetype; announced plans never supersede each other, and phones carry only organizers' broadcasts.
- **Stance (§2.4).** An agent's effective stance is the plan's stance weighted by trust in organizers, the archetype's default for the rest, shifted by anger. Provocateurs decide as if enraged.
- **Harm.** Every health loss is attributed to a cause (baton, blow, projectile, crush, fire, gas) in the outcome. Gas frightens, tires and pushes people but leaves them at 86% health or more; officers wear masks and armour; throws arc over the crowd (only officers, raised shields and the last 4 m of descent can be hit, never someone lying down); routes price fire like gas, and fire burns only inside the flames (above 0.3 of the field); crush damage needs pressure sustained for about half a second.
- **Leaving.** Anyone who has fled all the way to an exit leaves through it. Gas and blows drain morale, and GoHome is gated on the `leave_morale` tunable, so a dispersed crowd goes home rather than marching back for hours.
- **Arrests** count when the person is restrained; walking them to the van is logistics.
- **Any-angle routes (§9).** Tile costs are Theta\*-style with anchors and flow tiles are solved by Fast Marching, so people walk straight lines instead of grid staircases. Against the grid Dijkstra on the same tests, a walker crossing open ground at 31° travels 1.000× the straight line instead of 1.37× (and strays 0.2 m from it instead of 97 m), rounds a building within 0.9% of the geodesic instead of 21%, crosses the test district's streets to its square in 1.13× the straight-line distance instead of 1.27×, and keeps within 2% of the geodesic while its flow tiles are still pending instead of 22%. The tile search costs about 1.2× the grid search per recompute and tile builds about 1.15×: on the standard riot the flow phase's p50 is unchanged (0.27 ms) and its mean rises from 0.71 to 0.89 ms, with recomputes that bunch up reaching 4 ms at p99 instead of 2.8.
- **Police director (§14).** Squads stay with a crowd across replans and hold their post for a minute when it drops out of one; crowds under 150 people draw squads only when violent; a snatch squad moves up behind the line and goes in only for someone within 25 m. The commander (§14.1) plans cordons, ambushes and operations on top of that. On the standard riot (seed 42, 20 minutes) the director without it made no arrests and climbed three rungs, presence to gas, within three seconds at minute 10.5; it ended with 2,429 injuries and 7 deaths. With the commander, barricades go up across the government quarter's approaches from the first minute (14 cordons ordered over the riot, 6 breached, two opened by the police to ease a crush), crowds go round them or push through, two ambushes spring, escalation climbs one rung per 30 s from minute 7.9, crowds are told to disperse 18 times before 47 gas volleys, and the riot ends with 20 arrests, 1,186 injuries (crowd pressure still the largest share) and 3 deaths, all in fires the crowd's molotovs started. The movement held ground 59% of the time instead of 68%: the square 42% instead of 66%, but parliament 68% instead of 70%, so the barricades slow the march on parliament more than they stop it. The scenario is chaotic: while tuning, runs ended with 610–2,463 injuries and 0–4 deaths, all close to or below the director's own.
- **The tick runs on a thread team, not rayon (§12).** On the 4-vCPU cloud VM used for development, a rayon parallel loop costs about 7 µs back to back but 90–120 µs after a stretch of serial work, because idle workers (and the joining thread) sleep and are woken through the OS scheduler; a tick has about 25 such loops. `riot_team` is a small fork-join team whose workers spin for 2 ms after each job before parking (dispatch and join in 1.5–4 µs). Tasks are chunk indices, each run exactly once, so results do not depend on the thread count; reductions must be exact (integer sums, max). Its rayon-shaped API (`par_chunks_mut`, `zip`, `enumerate`, `for_each`, `map`/`reduce`) let the simulation switch by changing one import per module. Its atomics and unsafe code stay in that crate. Rayon still builds maps at load time.
- **Props are static obstacles.** The obstacle grid for props is rebuilt only when a prop is built, finished or destroyed; moving vehicles have their own small grid rebuilt every tick. Without this, the parked cars of a large map made the per-tick cost grow with map area.
- **Dependencies (§4.1).** `riot_core` and `riot_sim` use `bytemuck` with its derive macro for the `Pod` lane and snapshot types. `riot_data` adds `serde_json` and `miniz_oxide` (Tiled's zlib and gzip layers).
- **Sprites (§18).** All art is generated procedurally in `riot_render` at start-up, as pixel art at four densities: the paper-doll atlas, tileable ground and roof materials, decals, props and vehicles. There is no `data/sprites/`. Map meshes are textured and built in layers grouped by material, so a whole city is a few dozen draw batches.
- **Tooling (§19).** Per-phase benchmarks come from `riot_headless bench` (p50, p99, CSV, `--budget`), not criterion. Kernels have unit tests but no proptest suite. Tracy and code hot reload are not integrated; tunables hot reload is.

**Performance against §3 (4-vCPU cloud VM)**

| | Target | Measured |
|---|---|---|
| Small protest, 2,000 agents | ≤ 0.7 ms / tick | 0.95 ms (fields 0.43: a spread-out crowd keeps many field chunks active) |
| Standard riot, 10,309 agents | ≤ 3.0 ms / tick | 3.65 ms p50 (median of three runs) |
| Map-size independence, 10,000 agents, 8 km vs 512 m | within 5% | +0.9% |
| Auto-resolve, 10-minute riot | ≤ 40 s | 37.0 s |
| Auto-resolve, 1 simulated hour | ≤ 2 min | 5 min 41 s: the riot stays active, so few people sleep |

Phases within their §3.1 budgets at 10k: spatial, think, messages, bodies, interactions, director, accounting. Over: fields (0.7 ms against 0.3) and steering (0.8 ms against 0.26). Flow is at its p50 budget but spikes to about 4 ms at p99 when two corridor recomputes or many tile rebuilds land in one tick. The 50k stretch target (M7) has not been benchmarked.

**Known gaps**
- Arrests are rare in the standard riot (0–16 per riot) and injuries high (about 2,000 in a 20-minute riot), mostly crowd pressure during the flight from gas. `injuries` counts every time someone is hurt past the injury line, so a person treated by a medic and hurt again counts twice.
- No ambulances; the overturned-vehicle flag is never set; gas released inside a building has no effect.
- The commander's cordons cost the director about 0.02 ms per tick on average at 10k (route prediction and chokepoint rays every replan), within its budget; barriers changing make flow recompute corridors, which shows in the flow phase's mean.
