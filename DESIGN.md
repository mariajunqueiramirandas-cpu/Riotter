# Riotter: Riot Simulation Design

Status: draft v0.1 (2026-10-24)

This document covers **the real-time riot simulation only**: one district, one protest, thousands of autonomous people against a police force, with the player influencing events but never commanding them. The turn-based campaign, multiplayer, 3D, audio and UI art are out of scope. The campaign shows up only as the boundary: a `RiotSetup` goes in and a `RiotOutcome` comes out (§17).

It has two goals, and they carry equal weight:

1. **The crowd must feel alive.** It should be unpredictable in the moment, yet readable after the fact.
2. **The simulation must be extremely fast.** The main target is 10,000 agents in about 2.5 ms per 20 Hz tick. The stretch target is 50,000. A 10-minute riot must auto-resolve in under 30 seconds.

Everything below is written in service of both goals.

---

## 1. Design pillars

1. **You influence, the crowd decides.** The player never selects units, and no input is ever executed exactly. Every input is a suggestion that has to travel through people, and each person decides what to do with it.
2. **Emergence over scripting.** Big moments (a stampede, a police line breaking, a kettle closing) come out of local rules. Triggers don't create them.
3. **A riot never just wins or loses.** It ends with ground held, arrests, injuries, footage, morale and supplies used, and each of those matters separately.
4. **The simulation is the truth.** It is headless, deterministic and runs on a fixed tick. The renderer only watches.
5. **Performance is a feature.** Bigger crowds and faster auto-resolve are gameplay. Every system has a time budget and a benchmark, and going over budget is a bug.

---

## 2. The riot as the player sees it

### 2.1 The map

- A district of up to 512 × 512 m, containing streets, squares, alleys, buildings, chokepoints, entry/exit points and police staging areas.
- Authored as a bitmap with 1 px = 1 m, where colours mark walls, walkable ground, entries, objectives and staging. Any image editor works.
- Two grids are derived from the bitmap (§7): a 1 m **nav grid** and a 2 m **sim grid**.

### 2.2 Who is in it

| Population | Typical count | Who controls them |
|---|---|---|
| Protesters (students, workers, elders, radicals) | 1k–50k | Nobody. Autonomous, open to influence. |
| Organizers | 5–50 | They carry the player's voice. Still autonomous: they walk, tire and get arrested. |
| Provocateurs | 0–20 | The state, covertly. Hidden among protesters. |
| Journalists and phone cameras | 10–200 | Autonomous. They record events (§2.7). |
| Bystanders | 0–5k | Autonomous. They can join, flee or film. |
| Police officers | 50–2k | Squad AI driven by the state's director (§14). |

Archetypes differ in their defaults: baseline anger, fear threshold, trust in organizers, stamina, willingness to use violence, and speed.

### 2.3 A person

Every person is an agent with four kinds of state:

- **Body:** position, velocity, health, fatigue.
- **Emotions:** fear, anger, morale.
- **Belief:** what they think the plan is (an intent plus how garbled it reached them) and how much they trust the organizers.
- **Action:** what they are doing right now: march, hold, flee, rest, seek water or a medic, help someone injured, film, throw, build a barricade, chant, or go home.

They rethink about twice a second (§10). Each time, they score every possible action against their needs, their emotions and the plan as they understood it.

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
- **Crush:** above roughly 5 people/m² (the crowd-safety literature, e.g. Fruin and Still), people get injured and panic spreads. The player can cause a crush simply by calling everyone to one spot.
- **Violence is an action agents choose.** It is most likely with a confrontational stance, high anger, radicals or provocateurs. Every violent act is an event with an *initiator*, and the initiator is what footage cares about.

### 2.7 Footage and wildcards

- Cameras record the events they can see, each with a severity score. The footage tally tracks **who was seen starting it**. Police charging a peaceful crowd is a large legitimacy win for the movement. Protesters burning a car is a loss.
- Wildcards emerge from the simulation and are never scripted: a provocateur's thrown bottle, a martyr, a video that goes viral.

---

## 3. Performance targets

Reference machine: a 4-core / 8-thread desktop CPU with AVX2 (any mainstream CPU from about 2015 onward).

| Scenario | Agents | Target |
|---|---|---|
| Small protest | 2,000 | ≤ 0.6 ms / tick |
| **Standard riot** | **10,000** | **≤ 2.5 ms / tick** |
| Mass demonstration (stretch, M7) | 50,000 | ≤ 10 ms / tick |
| Auto-resolve, headless | 10,000 for 12,000 ticks (10 min) | ≤ 30 s total |

Hard rules:

- **Zero heap allocations per tick** once the simulation reaches steady state (§16).
- **Hot and warm state stays at or under 40 bytes per agent** (§6.2).
- **No locks and no atomics** in simulation code. Rayon's internal ones are the only exception.
- **The same seed, setup and commands always produce bit-identical state**, whatever the thread count (§12).

Starting point: a throwaway prototype (a naive `Vec<Vec<u32>>` grid, pairwise emotion contagion, and flow-field following) ran 5,000 agents at **0.79 ms/tick** on a 4-core cloud VM using the dev profile in §15. That prototype did a small fraction of the work described here. The architecture below is what lets the full simulation fit inside the budget.

### 3.1 Per-phase budget (10,000 agents)

| # | Phase | Budget (ms) |
|---|---|---|
| 1 | Apply commands | ~0 |
| 2 | Spatial index + periodic reorder | 0.15 |
| 3 | Fields: scatter, diffuse, absorb | 0.35 |
| 4 | Think (1/10 of agents per tick) | 0.30 |
| 5 | Messages | 0.10 |
| 6 | Steering | 0.60 |
| 7 | Integrate + collide | 0.15 |
| 8 | Interactions → events → resolve | 0.30 |
| 9 | Police director (amortized) | 0.10 |
| 10 | Flow fields (background, amortized) | 0.30 |
| 11 | Outcome accounting + render snapshot | 0.10 |
| | **Total** | **2.45** |

These are budgets, not measurements. Each phase gets a benchmark (§19) that is checked against this table.

---

## 4. Architecture

### 4.1 Crates

```
riotter/
├── Cargo.toml              workspace + build profiles (§15)
├── .cargo/config.toml      linker, target CPU
├── crates/
│   ├── riot_core/          math, ids, RNG, lane storage, grid geometry. No dependencies.
│   ├── riot_sim/           world state, systems, tick schedule.
│   │                       deps: riot_core, rayon, bytemuck, libm
│   ├── riot_data/          RON scenario and tunable files → riot_sim types. deps: serde, ron
│   ├── riot_headless/      CLI: batch runs, benchmarks, determinism + allocation checks
│   └── riot_app/           window, input → commands, rendering (macroquad), hot reload
└── data/
    ├── maps/               district bitmaps, 1 px = 1 m
    ├── scenarios/          RiotSetup files
    └── tunables.ron
```

`riot_sim` is the crate we will edit most often, so it deliberately carries no proc macros and no heavy dependencies. Serde lives in `riot_data`, and the dependency points from `riot_data` to `riot_sim`, never the other way. Rebuilding after an edit to the simulation therefore compiles a single crate and then links.

### 4.2 Ownership rules

1. **The simulation knows nothing about rendering.** `riot_sim` has no window, no clock and no I/O.
2. **Single writer.** Every phase declares what it reads and what it writes. Inside a phase, a parallel task writes only to its own chunk of agents or its own range of cells. Anything that affects *another* agent goes through a field (§8) or an event (§11.3). This one rule gives us parallelism without locks, and it also gives us determinism.
3. **Time is a tick counter.** `tick: u32`, and `dt = 1/20 s` is a constant. Wall-clock time never enters the simulation.
4. **All input is a command stamped with a tick.** Player actions, tunable changes and debug actions are all commands. A replay is therefore just seed + setup + command log, and it reproduces everything.

### 4.3 The tick

```rust
pub fn tick(w: &mut World, cmds: &[Command]) {
    apply_commands(w, cmds);     //  1  validate + apply, in order
    rebuild_index(w);            //  2  counting sort by cell; physical reorder every N ticks
    update_fields(w);            //  3  scatter → diffuse/decay → agents absorb (contagion)
    think(w);                    //  4  1/K of agents re-score their actions
    spread_messages(w);          //  5  rumours move
    steer(w);                    //  6  desired velocity + pressure + avoidance → vel
    integrate(w);                //  7  pos += vel·dt, wall collision
    interact(w);                 //  8  emit events in parallel, resolve in order
    police_director(w);          //  9  every 20 ticks
    flow_fields(w);              // 10  swap in finished fields, queue dirty ones
    account_and_snapshot(w);     // 11  outcome tallies + render snapshot
    w.tick += 1;
}
```

| Phase | Reads | Writes (own agent/cell only unless noted) |
|---|---|---|
| index | pos | cell, `CellIndex`; on reorder ticks it permutes all agent arrays |
| fields | agents per cell, emotions, faction | field layers; then each agent's own emotions |
| think | fields, emotions, belief, needs | action, target |
| messages | message layer, belief, trust | belief; message layer |
| steer | pos (own + neighbours), flow fields, density, action | vel |
| integrate | vel, walls | pos |
| interact | pos, action, neighbours | per-chunk event buffers, then sequential resolve |
| director | fields, squads, events | squad orders |

The phase order removes the need for double buffering. `steer` reads positions and writes velocities, and `integrate` reads velocities and writes positions. No phase reads another agent's data from the same array it is writing.

Adjacent element-wise phases may be **fused** into a single pass per chunk (for example, the emotion absorb at the end of phase 3 and gathering inputs for phase 4), but only when the fusion does not break the single-writer rule.

---

## 5. World layout overview

| Table | Rows | Layout | Why |
|---|---|---|---|
| Civilians | 1k–50k | SoA, lane storage | Hot loops over every agent |
| Officers | 50–2k | SoA, same body layout | Same physics, different mind |
| Squads, messages, goals, supply drops, gas emitters, projectiles | ≤ a few hundred | `Vec<Struct>` (AoS) | Small, touched as whole records |
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
| `cell` | u32 (Morton code of the 2 m cell) | 4 |

**Warm**, touched by think, contagion and messages:

| Field | Type | Bytes |
|---|---|---|
| `emotions` | `[u8; 4]`: fear, anger, morale, fatigue | 4 |
| `health`, `action`, `archetype`, `flags` | u8 × 4 | 4 |
| `intent` | u16: goal id + stance | 2 |
| `belief` | u16: message id + fidelity | 2 |
| `stable_id` | u32 | 4 |

**Total: 36 bytes per agent.** That is 360 KB for 10k agents and 1.8 MB for 50k. The entire per-tick working set of a mass demonstration fits in the L3 cache of any desktop CPU.

**Cold** data is touched rarely and accessed through `stable_id`: home and exit points, inventory, group membership, stats, and a name seed.

**Quantization.** Emotions and health are stored as `u8`, because 1/255 resolution is plenty for gameplay. Kernels widen them to f32, do the math, then narrow back with saturating rounding. Because the stored values are already `u8`, they can index 256-entry response-curve tables directly (§10). Positions stay f32: at 512 m, f32 resolution is about 6 × 10⁻⁵ m.

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

## 7. Grids and the spatial index

### 7.1 Two grids

| Grid | Cell size | Cells (512 m map) | Used for |
|---|---|---|---|
| Nav grid | 1 m | 512 × 512 = 262,144 | walls (1-bit walkable bitset, 32 KB), flow fields |
| Sim grid | 2 m | 256 × 256 = 65,536 | spatial index, all fields |

Grid dimensions are powers of two (smaller maps are padded with walls). That way a cell lookup is just shifts and masks, and Morton codes cover the grid exactly.

### 7.2 Index rebuild (every tick)

People walk at about 1.4 m/s, which is 7 cm per tick, so a typical agent stays in the same 2 m cell for about 30 ticks.

1. **Parallel:** `cell[i] = morton(x as u32 >> 1, y as u32 >> 1)`. Morton (Z-order) codes put neighbouring cells close together in memory.
2. **Stable counting sort** by cell, which costs O(agents + cells) and allocates nothing:

```rust
pub struct CellIndex {
    pub start: Vec<u32>,  // cells + 1: cell c's agents are order[start[c]..start[c + 1]]
    pub cursor: Vec<u32>, // cells: scratch
    pub order: Vec<u32>,  // dense agent indices, sorted by cell, stable
}

impl CellIndex {
    pub fn rebuild(&mut self, cell_of: &[u32]) {
        self.start.fill(0);
        for &c in cell_of { self.start[c as usize + 1] += 1; }
        for c in 1..self.start.len() { self.start[c] += self.start[c - 1]; }
        self.cursor.copy_from_slice(&self.start[..self.start.len() - 1]);
        for (i, &c) in cell_of.iter().enumerate() {
            let slot = &mut self.cursor[c as usize];
            self.order[*slot as usize] = i as u32;
            *slot += 1;
        }
    }
}
```

3. **Physical reorder** every N ticks (a tunable that starts at 8): permute every hot and warm array into `order`. Immediately after a reorder, `order` is the identity, and each cell is a contiguous slice of the agent arrays. Neighbour scans and field scatters then read memory sequentially. This copies 36 bytes per agent, which is 360 KB at 10k agents.

The sequential sort costs tens of microseconds at 10k agents. If profiling shows it is needed at 50k, the M7 upgrade is a parallel radix sort with fixed partitions.

### 7.3 Neighbour queries

Only two systems ever need pairwise neighbours: steering avoidance and interactions. Both follow the same rules:

- Scan the 3 × 3 block of sim cells around the agent, which finds every neighbour within 2 m.
- Stop after **K = 8** contacts within range. "First K in index order" is deterministic.
- In **dense cells** (more than about 12 agents, i.e. about 3/m²), skip pairwise checks entirely and use the density-field gradient instead (§11.1).

So the worst case is O(N·K), never O(N²), even in a crush.

---

## 8. Fields

A field replaces "every agent talks to its neighbours" with "every agent writes to the grid, then every agent reads the grid". That turns O(N·k) work into O(N + cells), and the result is the same whether the crowd is sparse or packed.

| Layer | Type | Written by | Read by |
|---|---|---|---|
| density | u16 count | scatter | steering (pressure), crush, think |
| fear, anger | f32 mean | scatter → diffuse | contagion, think |
| threat | f32 | officers, violence events | think, flee steering |
| gas | f32 | emitters → advect + diffuse | think, steering, damage |
| chant | f32 | organizers | morale |
| camera | u8 | journalists, phones | think, footage |
| message | u16 (fidelity << 8 \| id) | carriers | messages (§13) |

Layers are stored row-major on the 256 × 256 sim grid, one `Vec` per layer. Each f32 layer is 256 KB.

- **Scatter is a segmented reduction.** Agents are sorted by cell, so each cell's agents form one contiguous range, and a cell's value is a reduction over that range. The work is parallel over cells, and each cell is written by exactly one task: no atomics, no thread-local grids, and the result is deterministic. Each layer is written in a single streaming pass over all cells (empty cells write their neutral value), so layers never need a separate clear.
- **Diffuse/decay** uses a separable 3-tap blur (one horizontal pass, one vertical pass), parallel over rows, and it vectorizes cleanly. Gas uses semi-Lagrangian advection by the wind, plus diffusion and decay, which stays stable at any `dt`.
- **Absorb (contagion):** each agent does `emotion += rate · (field[cell] − emotion)`. That is an element-wise kernel in which each agent writes only to itself.
- **Multi-rate updates:** fear, anger, density and gas update every tick. Threat updates every 4 ticks and camera every 10 ticks. Each layer is tuned by benchmark.
- **Decay constants** such as `exp(−dt/τ)` are computed once at load time with `libm` and never inside a loop.

---

## 9. Flow fields

- There is one flow field per active goal: an objective, an exit, a rally point or a police target. At most 32 can be active at once.
- Fields are built on the 1 m nav grid. The integration field uses **Dial's algorithm** (a bucket-queue Dijkstra over integer costs 1–255). Cell cost = base + density penalty + gas penalty, so crowds route around crushes and gas clouds.
- Each field is stored as a **direction field**: 1 byte per cell (256 quantized angles, with 0 meaning "no direction"). That is 256 KB per goal and 8 MB at the cap. Integration distances are scratch data and are discarded once the field is built.
- Steering samples the four nearest cells and blends them bilinearly. Angles are decoded through a 256-entry `(cos, sin)` lookup table, so there is no runtime trigonometry and movement doesn't look grid-aligned.
- **Fields are built in the background with fixed latency.** A field requested at tick T is built as a background task on the sim pool and is always swapped in at exactly tick T + L (L = 10 by default). If it isn't finished by then, the simulation waits. Determinism never depends on how fast the task ran.
- Dynamic costs (density, gas) are refreshed once per second for each goal, with goals staggered across ticks.
- **Scaling path (M7):** flow-field tiles (E. Emerson, "Crowd Pathfinding and Steering Using Flow Field Tiles", *Game AI Pro*). These are 16 × 16 sectors connected by a portal graph, and only the sectors on a path get built.

---

## 10. Minds: think

- **Staggered thinking.** Each agent rethinks every K = 10 ticks, i.e. every 0.5 s, which is roughly human reaction time to a changing crowd. An agent's phase within that cycle is `mix(stable_id) % K`, so every tick handles 1/K of the crowd and the per-tick cost stays flat.
- **Emergencies skip the queue.** Being gassed, crushed or hit sets a flag, and the agent thinks on the next tick.
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

Two forces are then added on top:

- **Pressure.** In dense cells, agents are pushed down the density gradient (central differences on the density layer), in the style of continuum crowds (Treuille, Cooper and Popović, SIGGRAPH 2006). This costs one field sample per agent, whatever the density.
- **Avoidance.** In non-dense cells, agents separate from at most K = 8 neighbours.

Finally, `vel = lerp(vel, desired + pressure + avoidance, response)`, clamped to the agent's maximum speed. The `response` factor comes from the archetype; elders react more slowly.

### 11.2 Integrate and collide

- `pos += vel · dt` (the §6.3 kernel).
- Walls are checked against the 1 m walkable bitset. The move is tested one axis at a time (x first, then y); a blocked axis is cancelled, so agents slide along walls.
- **Crush:** cell density above the threshold (about 20 agents in a 2 m cell, i.e. 5/m²) adds crush damage, which produces injury events and sets the panic flag.

### 11.3 Interactions and events

- Interactions only visit agents whose current action interacts: throwing, striking, arresting, helping, picking things up. The per-action lists are built during think.
- Throwing targets come from the officers table (a few hundred rows), not from neighbour scans. Projectiles live in a small AoS table.
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
5. **Simulation logic never iterates a `HashMap`, orders anything by pointer address, reads the wall clock, or uses thread ids.**
6. **Float discipline.**
   - Never enable fast-math. Rust doesn't use it, and we never add it.
   - Never use approximate-reciprocal intrinsics.
   - Never call the std transcendentals (`sin`, `exp`, `powf`, …) inside the simulation. Use lookup tables, polynomials, or `libm`, which is pure Rust and gives identical results everywhere.
   - `+ − × ÷` and `sqrt` are correctly rounded IEEE operations, so they are safe to use anywhere.
   - Rust never fuses a multiply and an add on its own (verified in §6.3, even on x86-64-v3), so AVX2 builds and baseline builds produce identical results.
   - `f32::mul_add` is always correctly rounded and therefore deterministic, but without hardware FMA it falls back to a slow software routine. Keep it out of hot loops unless the build targets FMA.
7. **Background jobs have fixed tick latency** (§9).
8. **Tunable changes are commands**, applied at a specific tick and recorded in the log (§4.2).
9. **Small crowds use the same chunk boundaries.** Below about 2k agents, a phase may run single-threaded, decided by benchmark. It iterates exactly the same chunks in the same order, so the result is identical.

**Thread pool:** the simulation gets a dedicated rayon pool with (cores − 1) threads, leaving the main thread free for input and rendering. Headless runs use every core.

**Verified by tests (§19):** the same scenario run at 1, 2, 4 and 8 threads must give an identical state hash on every tick, and the x86-64-v3 and baseline builds must give identical hashes. Determinism between x86 and ARM is expected but not guaranteed in v1. The rules above keep it achievable if we later want lockstep multiplayer.

---

## 13. Messages (rumour propagation)

**Data:**
- A message table (at most 255 live messages). Each row holds an id, the originating organizer, the tick, the objective, the stance and a base fidelity.
- Per agent, `belief: u16` holding the message id and the fidelity the agent heard it at.
- Per sim cell, a message layer `u16 = fidelity << 8 | id`. Because fidelity is in the high byte, a plain `max` keeps the clearest message, and on a tie the higher (newer) id wins.

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
- **The director** runs every 20 ticks. It reads the threat and density fields and clusters the crowd by running union-find over dense sim cells. It then picks responses within the escalation cap and assigns squads to them.
- **Formations:**
  - A line is a set of slots along a segment.
  - A kettle is a set of slots around a cluster's boundary that contracts over time.
  - Officers steer toward their slots using the same steering kernel, with a slot target in place of a flow field.
- **Officer minds** run a simpler utility AI that weighs staying in formation against breaking, based on fear, anger and fatigue. A broken line is therefore emergent, not scripted.
- **Snatch arrests:** a squad targets one specific person. Organizers are targeted first once cameras or surveillance have identified them.

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
lto = "fat"
codegen-units = 1
panic = "abort"
debug = "line-tables-only"     # readable profiler stacks

[profile.profiling]            # cargo build --profile profiling
inherits = "release"
debug = true

[profile.dist]                 # shipped builds
inherits = "release"
debug = false
strip = true
```

Measured with the prototype on a 4-core cloud VM:
- Rebuilding after a simulation edit takes about 0.5 s.
- With the simulation crate grown to 17k lines, the rebuild takes 2.3 s. An edit to a different crate stays at 0.6 s.
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
- Store indices as `u32` or smaller, never `usize`. That halves the memory traffic.

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
- `riot_sim` depends on `riot_core`, `rayon`, `bytemuck` and `libm`, and nothing else. Any new dependency has to justify its cost in compile time and its risk to determinism.

---

## 17. Boundary with the campaign

```rust
pub struct RiotSetup {
    pub seed: u64,
    pub map: MapId,
    pub crowd: [u32; ARCHETYPES],   // turnout per archetype
    pub organizers: u16,
    pub supplies: Supplies,         // what the movement's logistics delivered
    pub police: PoliceForce,        // squads, doctrine
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

## 18. Rendering boundary

- After every tick, the simulation writes a snapshot into a triple buffer. Per agent, the snapshot holds the position plus one packed `u32` (archetype, action, emotion colour), which is about 12 bytes, or 600 KB at 50k agents.
- The snapshot is **written in `stable_id` order**: it gathers through `slot_of`, in parallel over output chunks. Snapshot slot *n* is therefore always the same person, so the renderer can interpolate between two snapshots even though the simulation reorders agents.
- The renderer interpolates between the two most recent snapshots (using an alpha taken from the frame clock) and draws instanced quads. It never reads `World` directly.
- **Debug overlays** draw field layers (density, fear, gas, messages) as heatmaps. These are the main tool for tuning emergent behaviour.

---

## 19. Testing, benchmarks, tooling

**Tests**
- **Determinism:** run each scenario × seed at 1, 2, 4 and 8 threads, and require an identical state hash on every tick. The state hash is a 64-bit `mix` chain over `bytemuck` views of every array.
- **Build determinism:** the x86-64-v3 and baseline builds must produce the same final hash. This runs in CI.
- **Zero allocation:** the counting-allocator test described in §16.
- **Kernels:** each optimized kernel is property-tested (proptest) against its scalar reference, and the outputs must match bit for bit.
- **Behaviour (headless):** assertions on emergent outcomes. Examples: "a gas canister in a crowd cuts density within 10 m by ≥ 50% within 10 s", and "a kettle closes around a 500-person cluster within 60 s". These catch regressions while we tune.

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
| **M1** Crowd moves | bitmap → nav/sim grids, spatial index, flow fields, steering, collision, renderer + overlays | 10k agents ≤ 1.0 ms/tick |
| **M2** Minds | fields, emotions, contagion, utility AI, staggered think | 10k ≤ 1.6 ms |
| **M3** Influence | organizers, messages, objectives, stance, supply drops, chants | 10k ≤ 1.9 ms |
| **M4** Police | squads, director, lines, kettles, arrests | 10k + 300 officers ≤ 2.2 ms |
| **M5** Hazards & violence | gas, fire, water cannon, injuries, crush, medics | 10k ≤ 2.5 ms |
| **M6** Outcome | events log, footage, `RiotOutcome`, auto-resolve | 10-minute 10k riot ≤ 30 s headless |
| **M7** Scale | flow-field tiles, parallel radix sort, PGO | 50k ≤ 10 ms |

---

## 21. Open questions

- **Fog of war:** does the player see only what their organizers and cameras see? It fits "you don't control everything": you don't see everything either.
- **Reaction speed:** should the think interval vary by archetype (radicals react faster, elders slower)?
- **Fixed-point positions:** only worth it if we commit to cross-platform lockstep multiplayer.
- **Sim cell size:** is 2 m right? Decide from measured neighbour counts in dense crowds.
- **Officer index:** should officers share the civilian spatial index (tagged entries) or keep their own? Decide by benchmark in M4.
- **Map authoring beyond bitmaps:** for example, importing OpenStreetMap street layouts to build fictional districts.
