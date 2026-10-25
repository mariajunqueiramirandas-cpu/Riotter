//! Input → camera moves, game speed and commands (DESIGN §2.4, §2.8).
//!
//! Devices are polled into [`Verb`]s; [`Input::act`] turns a verb into
//! messages for the sim thread. None of the player's verbs are orders: each
//! becomes a [`CommandKind`] that the sim thread stamps with the tick it is
//! applied on.

use std::collections::HashSet;

use macroquad::prelude::*;
use riot_render::debug::{OBJECTIVE_COLOURS, OBJECTIVE_NAMES, SUPPLY_NAMES};
use riot_render::{Camera, Lod, RenderOptions};
use riot_sim::archetype::Archetype;
use riot_sim::setup::{ObjectiveKind, SupplyKind};
use riot_sim::snapshot::{Snapshot, snap_flag};
use riot_sim::{CommandKind, DebugCommand, Overlay, Stance};

use crate::sim_thread::{SimHandle, ToSim};

/// Game speeds (DESIGN §2.8).
pub const SPEEDS: [u32; 4] = [1, 2, 4, 8];
/// Supplies dropped per key press.
const DROP_AMOUNT: u16 = 20;
/// People spawned by the debug spawn.
const SPAWN_COUNT: u16 = 100;
/// Mouse wheel zoom step.
const ZOOM_STEP: f32 = 1.18;
/// Keyboard pan speed in screen pixels per second.
const PAN_SPEED: f32 = 700.0;

/// Lower-case stance name for the HUD.
pub fn stance_name(s: Stance) -> &'static str {
    match s {
        Stance::Peaceful => "peaceful",
        Stance::Defensive => "defensive",
        Stance::Confrontational => "confrontational",
    }
}

const SUPPLIES: [SupplyKind; 4] = [
    SupplyKind::Water,
    SupplyKind::Medic,
    SupplyKind::Shields,
    SupplyKind::Barricade,
];

/// Everything the player can do, independent of keys and buttons.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Verb {
    TogglePause,
    Speed(u32),
    Faster,
    Slower,
    /// One tick while paused.
    Step,
    Tool(ObjectiveKind),
    PrevSlot,
    NextSlot,
    /// Place the current tool's objective in the current slot.
    Place,
    ClearSlot,
    CycleStance,
    SelectOrganizer,
    Broadcast,
    Chant,
    NextSupply,
    DropSupply,
    Gas,
    Ignite,
    Spawn,
    Threat,
    Escalate,
    CycleOverlay,
    CycleLod,
    ToggleMarkers,
    ToggleRoofs,
    /// Pixel art on or off.
    TogglePixelArt,
    /// Pixel size: the art's own, or twice as big.
    CyclePixelSize,
    /// Time-of-day colour grade on or off.
    ToggleGrade,
    TogglePhases,
    ToggleHelp,
    Quit,
}

/// Keyboard bindings.
const KEYS: &[(KeyCode, Verb)] = &[
    (KeyCode::Space, Verb::TogglePause),
    (KeyCode::Key1, Verb::Speed(1)),
    (KeyCode::Key2, Verb::Speed(2)),
    (KeyCode::Key3, Verb::Speed(4)),
    (KeyCode::Key4, Verb::Speed(8)),
    (KeyCode::Equal, Verb::Faster),
    (KeyCode::KpAdd, Verb::Faster),
    (KeyCode::Minus, Verb::Slower),
    (KeyCode::KpSubtract, Verb::Slower),
    (KeyCode::Period, Verb::Step),
    (KeyCode::M, Verb::Tool(ObjectiveKind::MarchTo)),
    (KeyCode::H, Verb::Tool(ObjectiveKind::Hold)),
    (KeyCode::X, Verb::Tool(ObjectiveKind::Disperse)),
    (KeyCode::LeftBracket, Verb::PrevSlot),
    (KeyCode::RightBracket, Verb::NextSlot),
    (KeyCode::Backspace, Verb::ClearSlot),
    (KeyCode::Delete, Verb::ClearSlot),
    (KeyCode::T, Verb::CycleStance),
    (KeyCode::R, Verb::SelectOrganizer),
    (KeyCode::B, Verb::Broadcast),
    (KeyCode::C, Verb::Chant),
    (KeyCode::Q, Verb::NextSupply),
    (KeyCode::E, Verb::DropSupply),
    (KeyCode::G, Verb::Gas),
    (KeyCode::F, Verb::Ignite),
    (KeyCode::U, Verb::Spawn),
    (KeyCode::Y, Verb::Threat),
    (KeyCode::PageUp, Verb::Escalate),
    (KeyCode::O, Verb::CycleOverlay),
    (KeyCode::L, Verb::CycleLod),
    (KeyCode::Tab, Verb::ToggleMarkers),
    (KeyCode::V, Verb::ToggleRoofs),
    (KeyCode::K, Verb::TogglePixelArt),
    (KeyCode::J, Verb::CyclePixelSize),
    (KeyCode::N, Verb::ToggleGrade),
    (KeyCode::P, Verb::TogglePhases),
    (KeyCode::F1, Verb::ToggleHelp),
    (KeyCode::Escape, Verb::Quit),
];

/// The player's tools and selections, plus viewer settings.
pub struct Input {
    /// Objective kind placed by a left click.
    pub tool: ObjectiveKind,
    /// Objective slot (0–15) that clicks place and broadcasts carry.
    pub slot: u8,
    /// Objective radius for new placements (m).
    pub radius: f32,
    pub stance: Stance,
    /// Stable id of the selected organizer.
    pub selected: Option<u32>,
    /// Organizers this player told to chant.
    pub chanting: HashSet<u32>,
    pub supply: SupplyKind,
    pub overlay: Overlay,
    pub paused: bool,
    pub speed: u32,
    pub show_phases: bool,
    pub help: bool,
    pub escalate_rung: u8,
    /// A short message for the HUD, and when it expires (s).
    pub toast: Option<(String, f64)>,
    pub cursor_world: Vec2,
    pub quit: bool,
    drag: Option<Vec2>,
}

impl Input {
    /// Default tools: march-to in slot 0, peaceful stance, water.
    pub fn new(speed: u32, paused: bool, overlay: Overlay) -> Input {
        Input {
            tool: ObjectiveKind::MarchTo,
            slot: 0,
            radius: 20.0,
            stance: Stance::Peaceful,
            selected: None,
            chanting: HashSet::new(),
            supply: SupplyKind::Water,
            overlay,
            paused,
            speed,
            show_phases: false,
            help: false,
            escalate_rung: 0,
            toast: None,
            cursor_world: Vec2::ZERO,
            quit: false,
            drag: None,
        }
    }

    fn say(&mut self, msg: impl Into<String>, now: f64) {
        self.toast = Some((msg.into(), now + 3.0));
    }

    /// The organizer nearest to `at` in the snapshot.
    pub fn nearest_organizer(snap: &Snapshot, at: Vec2) -> Option<u32> {
        snap.people
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                p.flags & snap_flag::PRESENT != 0 && p.flags & snap_flag::ORGANIZER != 0
            })
            .min_by(|(_, a), (_, b)| {
                let da = vec2(a.x, a.y).distance_squared(at);
                let db = vec2(b.x, b.y).distance_squared(at);
                da.total_cmp(&db)
            })
            .map(|(i, _)| i as u32)
    }

    /// The cursor tool preview for the renderer.
    pub fn tool_preview(&self) -> (Vec2, f32, Color) {
        (
            self.cursor_world,
            self.radius,
            OBJECTIVE_COLOURS[self.tool as usize],
        )
    }

    /// Read this frame's devices: move the camera and act on every verb.
    pub fn update(
        &mut self,
        cam: &mut Camera,
        snap: &Snapshot,
        sim: &SimHandle,
        opts: &mut RenderOptions,
        map_half: Vec2,
        now: f64,
        dt: f32,
    ) {
        let mouse = Vec2::from(mouse_position());
        self.cursor_world = cam.screen_to_world(mouse);
        let shift = is_key_down(KeyCode::LeftShift) || is_key_down(KeyCode::RightShift);

        // Camera: keys, right/middle drag, wheel (Shift+wheel: radius).
        let mut pan = Vec2::ZERO;
        if is_key_down(KeyCode::W) || is_key_down(KeyCode::Up) {
            pan.y += 1.0;
        }
        if is_key_down(KeyCode::S) || is_key_down(KeyCode::Down) {
            pan.y -= 1.0;
        }
        if is_key_down(KeyCode::A) || is_key_down(KeyCode::Left) {
            pan.x += 1.0;
        }
        if is_key_down(KeyCode::D) || is_key_down(KeyCode::Right) {
            pan.x -= 1.0;
        }
        cam.pan_pixels(pan * PAN_SPEED * dt * if shift { 3.0 } else { 1.0 });
        let dragging =
            is_mouse_button_down(MouseButton::Right) || is_mouse_button_down(MouseButton::Middle);
        match (dragging, self.drag) {
            (true, Some(last)) => {
                cam.pan_pixels(mouse - last);
                self.drag = Some(mouse);
            }
            (true, None) => self.drag = Some(mouse),
            (false, _) => self.drag = None,
        }
        let wheel = mouse_wheel().1;
        if wheel != 0.0 {
            if shift {
                self.radius = (self.radius * ZOOM_STEP.powf(wheel.signum())).clamp(3.0, 120.0);
            } else {
                cam.zoom_at(ZOOM_STEP.powf(wheel.signum()), mouse);
            }
        }
        if is_key_pressed(KeyCode::Home) {
            *cam = Camera::fit(map_half.x * 2.0, map_half.y * 2.0, cam.screen);
        }
        cam.clamp_to(map_half.x, map_half.y);
        self.cursor_world = cam.screen_to_world(mouse);

        let mut verbs: Vec<Verb> = KEYS
            .iter()
            .filter(|(k, _)| is_key_pressed(*k))
            .map(|&(_, v)| v)
            .collect();
        if is_mouse_button_pressed(MouseButton::Left) {
            verbs.push(Verb::Place);
        }
        for v in verbs {
            for m in self.act(v, snap, opts, now) {
                sim.send(m);
            }
        }
        if self.toast.as_ref().is_some_and(|(_, until)| *until < now) {
            self.toast = None;
        }
    }

    /// Carry out one verb at the cursor: update the player's state and
    /// return the messages for the sim thread.
    pub fn act(
        &mut self,
        v: Verb,
        snap: &Snapshot,
        opts: &mut RenderOptions,
        now: f64,
    ) -> Vec<ToSim> {
        let at = self.cursor_world;
        let mut out = Vec::new();
        // Forget an organizer who left the map or was arrested.
        if let Some(id) = self.selected
            && snap
                .people
                .get(id as usize)
                .is_none_or(|p| p.flags & snap_flag::PRESENT == 0)
        {
            self.selected = None;
            self.chanting.remove(&id);
            self.say(format!("organizer #{id} is gone"), now);
        }
        let speed_index = SPEEDS.iter().position(|&s| s == self.speed).unwrap_or(0);
        let set_speed = |me: &mut Input, s: u32, out: &mut Vec<ToSim>| {
            me.speed = s;
            out.push(ToSim::Speed(s));
            if me.paused {
                me.paused = false;
                out.push(ToSim::Pause(false));
            }
            me.say(format!("speed {s}×"), now);
        };
        let cmd = |k: CommandKind| ToSim::Cmd(k);
        match v {
            Verb::TogglePause => {
                self.paused = !self.paused;
                out.push(ToSim::Pause(self.paused));
                self.say(if self.paused { "paused" } else { "running" }, now);
            }
            Verb::Speed(s) => set_speed(self, s, &mut out),
            Verb::Faster => set_speed(self, SPEEDS[(speed_index + 1).min(3)], &mut out),
            Verb::Slower => set_speed(self, SPEEDS[speed_index.saturating_sub(1)], &mut out),
            Verb::Step => {
                if self.paused {
                    out.push(ToSim::Step);
                }
            }
            Verb::Tool(kind) => {
                self.tool = kind;
                self.say(format!("tool: {}", OBJECTIVE_NAMES[kind as usize]), now);
            }
            Verb::PrevSlot => self.slot = (self.slot + 15) % 16,
            Verb::NextSlot => self.slot = (self.slot + 1) % 16,
            Verb::Place => {
                out.push(cmd(CommandKind::PlaceObjective {
                    slot: self.slot,
                    kind: self.tool,
                    x: at.x,
                    y: at.y,
                    radius: self.radius,
                }));
                self.say(
                    format!(
                        "objective {}: {} at {:.0},{:.0}",
                        self.slot, OBJECTIVE_NAMES[self.tool as usize], at.x, at.y
                    ),
                    now,
                );
            }
            Verb::ClearSlot => {
                out.push(cmd(CommandKind::ClearObjective { slot: self.slot }));
                self.say(format!("objective {} cleared", self.slot), now);
            }
            Verb::CycleStance => {
                self.stance = Stance::from_u8((self.stance as u8 + 1) % 3);
                out.push(cmd(CommandKind::SetStance(self.stance)));
                self.say(format!("stance: {}", stance_name(self.stance)), now);
            }
            Verb::SelectOrganizer => {
                self.selected = Self::nearest_organizer(snap, at);
                match self.selected {
                    Some(id) => self.say(format!("selected organizer #{id}"), now),
                    None => self.say("no organizer on the map", now),
                }
            }
            Verb::Broadcast => {
                if self.selected.is_none() {
                    self.selected = Self::nearest_organizer(snap, at);
                }
                let active = snap.objectives.iter().any(|o| o.slot == self.slot);
                match self.selected {
                    None => self.say("no organizer to broadcast", now),
                    Some(_) if !active => self.say(
                        format!("objective {} is empty: place it first (click)", self.slot),
                        now,
                    ),
                    Some(id) => {
                        out.push(cmd(CommandKind::Broadcast {
                            organizer: id,
                            objective: self.slot,
                        }));
                        self.say(
                            format!(
                                "organizer #{id} broadcasts objective {} ({})",
                                self.slot,
                                stance_name(self.stance)
                            ),
                            now,
                        );
                    }
                }
            }
            Verb::Chant => match self.selected {
                None => self.say("select an organizer first (R)", now),
                Some(id) => {
                    let on = self.chanting.insert(id);
                    if !on {
                        self.chanting.remove(&id);
                    }
                    out.push(cmd(CommandKind::Chant { organizer: id, on }));
                    let what = if on { "chants" } else { "stops chanting" };
                    self.say(format!("organizer #{id} {what}"), now);
                }
            },
            Verb::NextSupply => {
                let k = SUPPLIES.iter().position(|&s| s == self.supply).unwrap_or(0);
                self.supply = SUPPLIES[(k + 1) % 4];
                self.say(
                    format!("supply: {}", SUPPLY_NAMES[self.supply as usize]),
                    now,
                );
            }
            Verb::DropSupply => {
                out.push(cmd(CommandKind::DropSupplies {
                    kind: self.supply,
                    x: at.x,
                    y: at.y,
                    amount: DROP_AMOUNT,
                }));
                let name = SUPPLY_NAMES[self.supply as usize];
                self.say(format!("drop {DROP_AMOUNT} {name}"), now);
            }
            Verb::Gas => {
                out.push(cmd(CommandKind::Debug(DebugCommand::Gas {
                    x: at.x,
                    y: at.y,
                })));
                self.say("debug: gas canister", now);
            }
            Verb::Ignite => {
                out.push(cmd(CommandKind::Debug(DebugCommand::Ignite {
                    x: at.x,
                    y: at.y,
                })));
                self.say("debug: ignite", now);
            }
            Verb::Spawn => {
                out.push(cmd(CommandKind::Debug(DebugCommand::Spawn {
                    x: at.x,
                    y: at.y,
                    count: SPAWN_COUNT,
                    archetype: Archetype::Student,
                    radius: 10.0,
                })));
                self.say(format!("debug: spawn {SPAWN_COUNT} people"), now);
            }
            Verb::Threat => {
                out.push(cmd(CommandKind::Debug(DebugCommand::Threat {
                    x: at.x,
                    y: at.y,
                    amount: 5.0,
                })));
                self.say("debug: threat", now);
            }
            Verb::Escalate => {
                self.escalate_rung = (self.escalate_rung + 1) % 5;
                out.push(cmd(CommandKind::Debug(DebugCommand::Escalate {
                    rung: self.escalate_rung,
                })));
                let r = self.escalate_rung;
                self.say(format!("debug: force escalation rung {r}"), now);
            }
            Verb::CycleOverlay => {
                let k = Overlay::ALL
                    .iter()
                    .position(|&o| o == self.overlay)
                    .unwrap_or(0);
                self.overlay = Overlay::ALL[(k + 1) % Overlay::ALL.len()];
                out.push(ToSim::Overlay(self.overlay));
                self.say(format!("overlay: {}", self.overlay.name()), now);
            }
            Verb::CycleLod => {
                opts.lod = match opts.lod {
                    None => Some(Lod::Far),
                    Some(Lod::Far) => Some(Lod::Middle),
                    Some(Lod::Middle) => Some(Lod::Close),
                    Some(Lod::Close) => None,
                };
                let name = opts.lod.map_or("auto", Lod::name);
                self.say(format!("level of detail: {name}"), now);
            }
            Verb::ToggleMarkers => opts.debug = !opts.debug,
            Verb::ToggleRoofs => opts.roofs = !opts.roofs,
            Verb::TogglePixelArt => {
                opts.pixel_art = !opts.pixel_art;
                let on = if opts.pixel_art { "on" } else { "off" };
                self.say(format!("pixel art: {on}"), now);
            }
            Verb::CyclePixelSize => {
                opts.chunk = if opts.chunk > 1.5 { 1.0 } else { 2.0 };
                self.say(format!("pixel size: {}×", opts.chunk), now);
            }
            Verb::ToggleGrade => {
                opts.grade = !opts.grade;
                let on = if opts.grade { "on" } else { "off" };
                self.say(format!("time-of-day light: {on}"), now);
            }
            Verb::TogglePhases => self.show_phases = !self.show_phases,
            Verb::ToggleHelp => self.help = !self.help,
            Verb::Quit => self.quit = true,
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use riot_sim::snapshot::{SnapObjective, SnapPerson};

    fn snap_with_organizers() -> Snapshot {
        let mut s = Snapshot::default();
        let person = |x: f32, flags: u8| SnapPerson {
            x,
            y: 0.0,
            flags,
            ..Default::default()
        };
        s.people = vec![
            person(0.0, snap_flag::PRESENT),
            person(10.0, snap_flag::PRESENT | snap_flag::ORGANIZER),
            person(50.0, snap_flag::PRESENT | snap_flag::ORGANIZER),
            person(12.0, snap_flag::ORGANIZER), // not on the map
        ];
        s
    }

    fn cmds(out: &[ToSim]) -> Vec<CommandKind> {
        out.iter()
            .filter_map(|m| match m {
                ToSim::Cmd(k) => Some(*k),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn broadcast_needs_an_organizer_and_a_placed_objective() {
        let mut input = Input::new(1, false, Overlay::None);
        let mut opts = RenderOptions::default();
        let mut snap = snap_with_organizers();
        input.cursor_world = vec2(40.0, 0.0);
        // No objective in slot 0 yet: nothing is sent, but the nearest
        // organizer is selected.
        assert!(input.act(Verb::Broadcast, &snap, &mut opts, 0.0).is_empty());
        assert_eq!(input.selected, Some(2));
        snap.objectives.push(SnapObjective {
            slot: 0,
            active: true,
            ..Default::default()
        });
        let out = input.act(Verb::Broadcast, &snap, &mut opts, 0.0);
        assert_eq!(
            cmds(&out),
            vec![CommandKind::Broadcast {
                organizer: 2,
                objective: 0
            }]
        );
    }

    #[test]
    fn select_chant_and_forget_a_missing_organizer() {
        let mut input = Input::new(1, false, Overlay::None);
        let mut opts = RenderOptions::default();
        let mut snap = snap_with_organizers();
        input.cursor_world = vec2(11.0, 0.0);
        input.act(Verb::SelectOrganizer, &snap, &mut opts, 0.0);
        assert_eq!(input.selected, Some(1), "nearest present organizer");
        let on = input.act(Verb::Chant, &snap, &mut opts, 0.0);
        assert_eq!(
            cmds(&on),
            vec![CommandKind::Chant {
                organizer: 1,
                on: true
            }]
        );
        let off = input.act(Verb::Chant, &snap, &mut opts, 0.0);
        assert_eq!(
            cmds(&off),
            vec![CommandKind::Chant {
                organizer: 1,
                on: false
            }]
        );
        snap.people[1].flags = 0;
        assert!(input.act(Verb::Chant, &snap, &mut opts, 0.0).is_empty());
        assert_eq!(input.selected, None);
    }

    #[test]
    fn tools_slots_stance_supplies_and_speed() {
        let mut input = Input::new(1, true, Overlay::None);
        let mut opts = RenderOptions::default();
        let snap = Snapshot::default();
        input.cursor_world = vec2(5.0, -7.0);
        input.act(Verb::Tool(ObjectiveKind::Hold), &snap, &mut opts, 0.0);
        input.act(Verb::PrevSlot, &snap, &mut opts, 0.0);
        assert_eq!(input.slot, 15);
        let out = input.act(Verb::Place, &snap, &mut opts, 0.0);
        assert_eq!(
            cmds(&out),
            vec![CommandKind::PlaceObjective {
                slot: 15,
                kind: ObjectiveKind::Hold,
                x: 5.0,
                y: -7.0,
                radius: 20.0
            }]
        );
        let out = input.act(Verb::CycleStance, &snap, &mut opts, 0.0);
        assert_eq!(cmds(&out), vec![CommandKind::SetStance(Stance::Defensive)]);
        input.act(Verb::NextSupply, &snap, &mut opts, 0.0);
        let out = input.act(Verb::DropSupply, &snap, &mut opts, 0.0);
        assert!(matches!(
            cmds(&out)[..],
            [CommandKind::DropSupplies {
                kind: SupplyKind::Medic,
                ..
            }]
        ));
        // Changing speed while paused also resumes.
        let out = input.act(Verb::Speed(8), &snap, &mut opts, 0.0);
        assert!(matches!(out[..], [ToSim::Speed(8), ToSim::Pause(false)]));
        let out = input.act(Verb::Faster, &snap, &mut opts, 0.0);
        assert!(matches!(out[..], [ToSim::Speed(8)]));
        let out = input.act(Verb::Slower, &snap, &mut opts, 0.0);
        assert!(matches!(out[..], [ToSim::Speed(4)]));
        let out = input.act(Verb::CycleOverlay, &snap, &mut opts, 0.0);
        assert!(matches!(out[..], [ToSim::Overlay(Overlay::Density)]));
    }
}
