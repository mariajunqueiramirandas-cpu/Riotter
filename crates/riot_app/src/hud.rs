//! The heads-up display: clock and speed, crowd statistics, escalation,
//! the player's tools, per-phase timings and a help line.

use macroquad::prelude::*;
use riot_render::RenderStats;
use riot_render::debug::{OBJECTIVE_COLOURS, OBJECTIVE_NAMES, SUPPLY_NAMES};
use riot_sim::Phase;
use riot_sim::snapshot::{Overlay, Snapshot};
use riot_sim::tactics::{Call, OpKind, RADIO, call_text};
use riot_sim::world::PHASES;

use crate::input::{Input, stance_name};
use crate::sim_thread::SimStatus;

const ESCALATION: [&str; 5] = [
    "presence",
    "lines & kettles",
    "batons & arrests",
    "gas & water cannon",
    "mass arrests",
];

const HELP_LINE: &str = "WASD/arrows/right-drag pan · wheel zoom · Space pause · 1-4 / +- speed · \
     M H X tool · click place · [ ] slot · T stance · R organizer · B broadcast · C chant · \
     Q/E supplies · G gas · F fire · U spawn · O overlay · L lod · Tab police plan · K J pixels · F1 help";

const HELP: [&str; 15] = [
    "Camera   WASD / arrows (Shift = faster), right or middle drag, wheel = zoom, Home = whole map",
    "Time     Space pause · 1 2 3 4 = 1× 2× 4× 8× · + / - faster / slower · . one tick while paused",
    "Plan     M march-to · H hold · X disperse, then left click places the objective in the slot",
    "         [ ] objective slot 0-15 · Shift+wheel objective radius · Backspace clears the slot",
    "Voice    T stance (peaceful / defensive / confrontational) for the next broadcasts",
    "         R select the organizer nearest the cursor · B broadcast the slot's objective",
    "         C chant on/off (raises morale around them, tires them)",
    "Supply   Q choose water / medic / shields / barricade · E drop 20 at the cursor",
    "Debug    G gas canister · F ignite · U spawn 100 people · Y threat · PgUp force escalation",
    "View     O cycle field overlay · L level of detail · Tab markers and the police plan · V roofs · P phase timings",
    "         K pixel art on/off · J pixel size 1× / 2× · N time-of-day light",
    "",
    "Nothing here is an order: objectives only matter to people who hear about them,",
    "and they hear through organizers' broadcasts spreading like rumours.",
    "Esc quits.",
];

/// Frame timing, smoothed.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameTimes {
    /// CPU time of our frame work (ms).
    pub cpu_ms: f32,
    /// Interval between frames (ms).
    pub frame_ms: f32,
}

impl FrameTimes {
    /// Add one frame's measurements (exponential moving average).
    pub fn add(&mut self, cpu_ms: f32, frame_ms: f32) {
        let k = if self.frame_ms == 0.0 { 1.0 } else { 0.1 };
        self.cpu_ms += (cpu_ms - self.cpu_ms) * k;
        self.frame_ms += (frame_ms - self.frame_ms) * k;
    }
}

fn clock(minute: f32) -> String {
    let m = minute.max(0.0) as u32;
    format!("{:02}:{:02}", (m / 60) % 24, m % 60)
}

/// Text with a dark shadow, for legibility over any map.
fn text(s: &str, x: f32, y: f32, size: f32, col: Color) {
    draw_text(s, x + 1.0, y + 1.0, size, Color::new(0.0, 0.0, 0.0, 0.85));
    draw_text(s, x, y, size, col);
}

/// What the police are doing, in one line.
fn police_line(snap: &Snapshot) -> String {
    let (mut up, mut building, mut breached, mut waiting) = (0, 0, 0, 0);
    for c in &snap.cordons {
        match c.state {
            0 | 1 => building += 1,
            2 => up += 1,
            _ => breached += 1,
        }
        if c.ambush == 1 {
            waiting += 1;
        }
    }
    let mut by = [0u32; 7];
    for t in &snap.tracks {
        by[(t.op as usize).min(6)] += 1;
    }
    let mut parts = vec![format!("police: {up} barricades up")];
    if building > 0 {
        parts.push(format!("{building} going up"));
    }
    if breached > 0 {
        parts.push(format!("{breached} breached"));
    }
    if waiting > 0 {
        parts.push(format!(
            "{waiting} ambush{} waiting",
            if waiting == 1 { "" } else { "es" }
        ));
    }
    for (op, n) in by.iter().enumerate().skip(1) {
        if *n > 0 {
            parts.push(format!(
                "{} {} crowd{}",
                OpKind::from_u8(op as u8).name(),
                n,
                if *n == 1 { "" } else { "s" }
            ));
        }
    }
    parts.join("  ·  ")
}

/// The police radio: the commander's latest calls, top right, newest first.
fn draw_radio(snap: &Snapshot, sw: f32, left: f32) {
    let st = &snap.stats;
    let n = st.radio_n as usize;
    if n == 0 {
        return;
    }
    let shown = n.min(RADIO).min(6);
    let mut rows: Vec<(String, Color)> = Vec::with_capacity(shown);
    for k in 0..shown {
        let c = &st.radio[(n - 1 - k) % RADIO];
        let age = snap.tick.saturating_sub(c.tick) as f32 / 20.0;
        let fade = (1.0 - age / 120.0).clamp(0.35, 1.0);
        let col = match c.call {
            Call::AmbushSprung | Call::Breach | Call::LineBroken | Call::Opened => {
                Color::new(1.0, 0.4, 0.35, fade)
            }
            Call::Gas | Call::WaterCannon | Call::Escalate | Call::Warning => {
                Color::new(1.0, 0.7, 0.3, fade)
            }
            Call::Cordon | Call::BarricadeUp | Call::AmbushSet | Call::FallBack => {
                Color::new(1.0, 0.85, 0.45, fade)
            }
            _ => Color::new(0.7, 0.82, 1.0, fade),
        };
        let m = snap.stats.clock_minute - age / 60.0;
        let m = m.max(0.0);
        rows.push((
            format!(
                "{:02}:{:02}:{:02}  {}",
                (m as u32 / 60) % 24,
                m as u32 % 60,
                (m.fract() * 60.0) as u32,
                call_text(c)
            ),
            col,
        ));
    }
    let w = rows
        .iter()
        .map(|(l, _)| measure_text(l, None, 15, 1.0).width)
        .fold(measure_text("POLICE RADIO", None, 15, 1.0).width, f32::max)
        + 20.0;
    let x = (sw - w - 6.0).max(left);
    let h = (rows.len() + 1) as f32 * 17.0 + 12.0;
    draw_rectangle(x, 6.0, w, h, Color::new(0.03, 0.05, 0.1, 0.78));
    text(
        "POLICE RADIO",
        x + 10.0,
        22.0,
        15.0,
        Color::new(0.45, 0.65, 1.0, 1.0),
    );
    for (k, (l, c)) in rows.iter().enumerate() {
        text(l, x + 10.0, 39.0 + k as f32 * 17.0, 15.0, *c);
    }
}

/// Everything the HUD shows.
pub struct HudInput<'a> {
    pub snap: Option<&'a Snapshot>,
    pub status: SimStatus,
    pub input: &'a Input,
    pub render: RenderStats,
    pub frame: FrameTimes,
    pub zoom: f32,
    pub reload: Option<&'a str>,
    pub people_total: usize,
}

/// Draw the HUD in screen space.
pub fn draw(h: &HudInput) {
    let (sw, sh) = (screen_width(), screen_height());
    let white = Color::new(0.95, 0.95, 0.95, 1.0);
    let dim = Color::new(0.75, 0.78, 0.82, 1.0);
    let warn = Color::new(1.0, 0.55, 0.3, 1.0);
    let st = h.status;
    let lines_h = 18.0;

    let mut lines: Vec<(String, Color)> = Vec::new();
    let s = h.snap.map(|s| s.stats).unwrap_or_default();
    let run = if st.fast_forward > 0 {
        format!("fast-forward, {} ticks to go", st.fast_forward)
    } else if st.paused {
        "PAUSED".to_string()
    } else {
        format!("{}×", st.speed)
    };
    lines.push((
        format!(
            "tick {}  ·  {}  ·  {}{}{}",
            st.tick,
            clock(s.clock_minute),
            run,
            if st.behind { "  (sim behind)" } else { "" },
            if st.finished { "  ·  riot over" } else { "" },
        ),
        if st.behind { warn } else { white },
    ));
    lines.push((
        format!(
            "on map {}  ·  protesters {}  ·  officers {}  ·  asleep {}",
            s.present, s.protesters, s.officers, s.asleep
        ),
        dim,
    ));
    lines.push((
        format!(
            "arrests {}  ·  injured {}  ·  deaths {}  ·  went home {}",
            s.arrests, s.injuries, s.deaths, s.went_home
        ),
        dim,
    ));
    lines.push((
        format!(
            "fear {:.2}  ·  anger {:.2}  ·  morale {:.2}  ·  messages {}  ·  internet {}",
            s.mean_fear,
            s.mean_anger,
            s.mean_morale,
            s.messages,
            if s.internet { "on" } else { "OFF" }
        ),
        dim,
    ));
    lines.push((
        format!(
            "escalation: {}  ·  blame: movement {:.1} / police {:.1}",
            ESCALATION[(s.escalation as usize).min(4)],
            s.movement_blame,
            s.police_blame
        ),
        if s.escalation >= 3 { warn } else { dim },
    ));
    if let Some(snap) = h.snap {
        lines.push((police_line(snap), Color::new(0.6, 0.75, 1.0, 1.0)));
    }
    let i = h.input;
    let organizer = match i.selected {
        Some(id) => format!(
            "#{id}{}",
            if i.chanting.contains(&id) {
                " (chanting)"
            } else {
                ""
            }
        ),
        None => "none (R)".into(),
    };
    lines.push((
        format!(
            "tool: {} r{:.0}  ·  slot {}  ·  stance: {}  ·  organizer: {}  ·  supply: {}",
            OBJECTIVE_NAMES[i.tool as usize],
            i.radius,
            i.slot,
            stance_name(i.stance),
            organizer,
            SUPPLY_NAMES[i.supply as usize],
        ),
        OBJECTIVE_COLOURS[i.tool as usize],
    ));
    lines.push((
        format!(
            "overlay: {}  ·  lod: {}  ·  zoom {:.2} px/m{}  ·  drawn {} of {} people{}",
            i.overlay.name(),
            h.render.lod.map_or("-", |l| l.name()),
            h.zoom,
            if h.render.pixel {
                format!(" (art {:.0} px/m)", h.render.density)
            } else {
                String::new()
            },
            h.render.people,
            h.people_total,
            if i.overlay != Overlay::None && st.paused {
                " (overlay refreshes while running)"
            } else {
                ""
            }
        ),
        dim,
    ));
    lines.push((
        format!(
            "frame {:.1} ms ({:.0} fps), cpu {:.1} ms  ·  sim {:.2} ms/tick, {:.0} ticks/s on {} threads{}",
            h.frame.frame_ms,
            1000.0 / h.frame.frame_ms.max(0.01),
            h.frame.cpu_ms,
            st.tick_ms,
            st.ticks_per_sec,
            st.threads,
            if st.queued > 0 {
                format!("  ·  {} commands queued", st.queued)
            } else {
                String::new()
            }
        ),
        dim,
    ));
    if let Some(r) = h.reload {
        lines.push((r.to_string(), dim));
    }
    if h.input.show_phases {
        let mut row = String::from("render cpu ms:");
        for (name, ms) in riot_render::SECTIONS.iter().zip(h.render.sections_ms) {
            row += &format!("  {name} {ms:.2}");
        }
        lines.push((row, dim));
        for k in 0..PHASES.div_ceil(2) {
            let mut row = String::new();
            for p in [k, k + PHASES.div_ceil(2)] {
                if p < PHASES {
                    let ph = Phase::ALL[p];
                    row += &format!(
                        "{:>9} {:>6.3} ms (budget {:.2})     ",
                        ph.name(),
                        st.phase_ms[p],
                        ph.budget_ms()
                    );
                }
            }
            lines.push((row, dim));
        }
    }

    let w = lines
        .iter()
        .map(|(l, _)| measure_text(l, None, 16, 1.0).width)
        .fold(0.0, f32::max)
        + 20.0;
    let ph = lines.len() as f32 * lines_h + 12.0;
    draw_rectangle(6.0, 6.0, w, ph, Color::new(0.05, 0.06, 0.08, 0.72));
    for (k, (l, c)) in lines.iter().enumerate() {
        text(l, 16.0, 24.0 + k as f32 * lines_h, 16.0, *c);
    }
    if let Some(snap) = h.snap {
        draw_radio(snap, sw, w + 16.0);
    }

    // Toast in the middle at the top, help line at the bottom.
    if let Some((msg, _)) = &i.toast {
        let d = measure_text(msg, None, 20, 1.0);
        let x = (sw - d.width) * 0.5;
        draw_rectangle(
            x - 10.0,
            10.0,
            d.width + 20.0,
            30.0,
            Color::new(0.05, 0.06, 0.08, 0.75),
        );
        text(msg, x, 31.0, 20.0, white);
    }
    if i.help {
        let hh = HELP.len() as f32 * 20.0 + 20.0;
        let y0 = (sh - hh) * 0.5;
        draw_rectangle(40.0, y0, sw - 80.0, hh, Color::new(0.04, 0.05, 0.07, 0.9));
        for (k, l) in HELP.iter().enumerate() {
            text(l, 60.0, y0 + 26.0 + k as f32 * 20.0, 17.0, white);
        }
    }
    draw_rectangle(0.0, sh - 24.0, sw, 24.0, Color::new(0.05, 0.06, 0.08, 0.7));
    text(HELP_LINE, 10.0, sh - 8.0, 14.0, dim);
}
