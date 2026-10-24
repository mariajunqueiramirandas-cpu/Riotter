//! Snapshots → GPU (DESIGN §18): a top-down renderer for Riotter.
//!
//! The renderer never reads `World`; it only reads [`Snapshot`]s. Each frame
//! it draws, bottom to top and without sorting:
//!
//! 1. ground and decals (static mesh from the [`MapDef`]), props, objective
//!    areas and supply drops;
//! 2. fallen people;
//! 3. standing people (layered paper-doll sprites, one instanced draw per
//!    layer);
//! 4. vehicles;
//! 5. projectiles;
//! 6. gas, smoke and fire glow (field textures) and particles;
//! 7. roofs and tree canopies, then the optional debug overlay heatmap and
//!    markers. At far zoom people are abstract markers wider than the
//!    streets, so they are drawn above the roofs instead of in 2–3.
//!
//! People are culled on the CPU only when a new snapshot arrives or the view
//! leaves the culled area (with a margin); position and heading are
//! interpolated on the GPU with the frame's alpha.
//!
//! Modules: [`camera`] (view and level of detail), [`sprites`] (atlas and
//! palette generation), [`pipeline`] (the instanced shader), [`map`] (static
//! geometry), [`fields`] (field textures), [`scene`] (props, vehicles,
//! projectiles), [`effects`] (particles) and [`debug`] (markers).

pub mod camera;
pub mod debug;
pub mod effects;
pub mod fields;
pub mod map;
pub mod pipeline;
pub mod scene;
pub mod sprites;

use macroquad::miniquad::{PassAction, RenderingBackend};
use macroquad::prelude::*;
use riot_sim::mapdef::{EntryDef, MapDef};
use riot_sim::snapshot::{Overlay, SnapPerson, Snapshot, snap_flag};
use riot_sim::state::Gait;

pub use camera::{Camera, Lod};
use pipeline::{Layer, PeoplePipeline};

/// Instance groups, drawn in this order.
const FALLEN: usize = 0;
const STANDING: usize = 1;
const CARRIED: usize = 2;
const GROUPS: usize = 3;

/// Viewer settings that never touch the simulation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderOptions {
    /// Force a level of detail; `None` picks it from the zoom.
    pub lod: Option<Lod>,
    /// Squads, entries and other debug markers.
    pub debug: bool,
    /// Draw roofs (turn off to see the map's collision shapes underneath).
    pub roofs: bool,
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions {
            lod: None,
            debug: true,
            roofs: true,
        }
    }
}

/// Everything one frame needs.
pub struct Frame<'a> {
    /// The snapshot before `cur`, if any.
    pub prev: Option<&'a Snapshot>,
    pub cur: &'a Snapshot,
    /// Interpolation between `prev` (0) and `cur` (1).
    pub alpha: f32,
    pub camera: Camera,
    /// Seconds since the last frame and since start (render clock).
    pub dt: f32,
    pub time: f32,
    /// Stable id of the selected organizer.
    pub selected: Option<u32>,
    pub chanting: bool,
    /// Objective slot to highlight.
    pub highlight_slot: Option<u8>,
    /// Tool preview at the cursor: position, radius, colour.
    pub tool: Option<(Vec2, f32, Color)>,
}

/// What the last frame drew, for the HUD.
#[derive(Clone, Copy, Debug, Default)]
pub struct RenderStats {
    pub lod: Option<Lod>,
    /// People instances drawn.
    pub people: usize,
    /// Instanced draw calls for people.
    pub people_draws: usize,
    pub field_chunks: usize,
    pub particles: usize,
    /// CPU time spent culling and uploading people and updating field
    /// textures this frame (ms).
    pub gather_ms: f32,
    /// CPU time per section of [`Renderer::draw`] (ms), see [`SECTIONS`].
    pub sections_ms: [f32; SECTIONS.len()],
}

/// Names of the timed sections of a frame, in draw order.
pub const SECTIONS: [&str; 8] = [
    "upload", "ground", "people", "scene", "hazards", "effects", "roofs", "markers",
];

/// Splits a frame's CPU time into sections.
struct SectionTimer {
    last: std::time::Instant,
    ms: [f32; SECTIONS.len()],
}

impl SectionTimer {
    fn new() -> Self {
        SectionTimer {
            last: std::time::Instant::now(),
            ms: [0.0; SECTIONS.len()],
        }
    }

    /// Charge the time since the last mark to section `k`.
    fn mark(&mut self, k: usize) {
        let now = std::time::Instant::now();
        self.ms[k] += (now - self.last).as_secs_f32() * 1000.0;
        self.last = now;
    }
}

/// CPU side of culling: the visible people of both snapshots, in the same
/// order, split into groups.
#[derive(Default)]
struct Gather {
    prev: [Vec<SnapPerson>; GROUPS],
    cur: [Vec<SnapPerson>; GROUPS],
    /// Snapshot ticks and the map area the last gather covered.
    key: Option<(u32, Option<u32>)>,
    area: Rect,
}

impl Gather {
    fn needed(&self, prev: Option<&Snapshot>, cur: &Snapshot, view: Rect) -> bool {
        let key = (cur.tick, prev.map(|p| p.tick));
        self.key != Some(key)
            || !(self.area.contains(view.point()) && self.area.contains(view.point() + view.size()))
    }

    fn run(&mut self, prev: Option<&Snapshot>, cur: &Snapshot, area: Rect) {
        self.key = Some((cur.tick, prev.map(|p| p.tick)));
        self.area = area;
        for g in 0..GROUPS {
            self.prev[g].clear();
            self.cur[g].clear();
        }
        let prev_people = prev.map(|p| p.people.as_slice()).unwrap_or(&[]);
        let (x0, y0, x1, y1) = (area.x, area.y, area.x + area.w, area.y + area.h);
        for (i, c) in cur.people.iter().enumerate() {
            if c.flags & snap_flag::PRESENT == 0 || c.x < x0 || c.x > x1 || c.y < y0 || c.y > y1 {
                continue;
            }
            let g = match c.gait {
                g if g == Gait::Fallen as u8 => FALLEN,
                g if g == Gait::Carried as u8 => CARRIED,
                _ => STANDING,
            };
            // Without a previous record the shader snaps to the current one.
            let p = prev_people.get(i).copied().unwrap_or_default();
            self.prev[g].push(p);
            self.cur[g].push(*c);
        }
    }
}

/// The renderer: owns every GPU resource.
pub struct Renderer {
    map: map::MapRenderer,
    people: PeoplePipeline,
    fields: fields::FieldRenderer,
    effects: effects::Effects,
    gather: Gather,
    labels: Vec<debug::Label>,
    entries: Vec<EntryDef>,
    pub options: RenderOptions,
}

/// Run raw miniquad drawing between macroquad's batched shapes.
fn with_gl<R>(f: impl FnOnce(&mut dyn RenderingBackend) -> R) -> R {
    // SAFETY: macroquad's context lives for the whole program and is only
    // touched from the main thread, inside a frame. `flush` first submits the
    // shapes macroquad has batched so far, which keeps the layer order.
    let mut gl = unsafe { get_internal_gl() };
    gl.flush();
    f(gl.quad_context)
}

/// Like [`with_gl`], inside a render pass on the default framebuffer.
fn in_pass<R>(f: impl FnOnce(&mut dyn RenderingBackend) -> R) -> R {
    with_gl(|ctx| {
        ctx.begin_default_pass(PassAction::Nothing);
        let r = f(ctx);
        ctx.end_render_pass();
        r
    })
}

impl Renderer {
    /// Build the atlas, palettes, static map meshes and pipelines. Call once
    /// the macroquad window exists.
    pub fn new(map: &MapDef) -> Renderer {
        let atlas = sprites::build_atlas();
        let palette = sprites::build_palette();
        with_gl(|ctx| {
            let people = PeoplePipeline::new(ctx, &atlas, &palette, GROUPS);
            if !people.instancing {
                eprintln!("riot_render: no GL instancing; people fall back to circles");
            }
            Renderer {
                map: map::MapRenderer::new(ctx, map),
                people,
                fields: fields::FieldRenderer::new(ctx),
                effects: effects::Effects::new(),
                gather: Gather::default(),
                labels: Vec::new(),
                entries: map.entries.clone(),
                options: RenderOptions::default(),
            }
        })
    }

    /// GL version string, for logs.
    pub fn gl_info() -> String {
        with_gl(|ctx| ctx.info().gl_version_string)
    }

    /// Where a person is drawn this frame (interpolated), if present.
    pub fn person_pos(
        prev: Option<&Snapshot>,
        cur: &Snapshot,
        alpha: f32,
        id: u32,
    ) -> Option<Vec2> {
        let c = cur.people.get(id as usize)?;
        if c.flags & snap_flag::PRESENT == 0 {
            return None;
        }
        let c = vec2(c.x, c.y);
        match prev.and_then(|p| p.people.get(id as usize)) {
            Some(p) if p.flags & snap_flag::PRESENT != 0 => {
                let p = vec2(p.x, p.y);
                Some(if p.distance(c) < 4.0 {
                    p.lerp(c, alpha)
                } else {
                    c
                })
            }
            _ => Some(c),
        }
    }

    /// Draw one frame. Clears the screen; leaves the default (screen)
    /// camera set for the HUD.
    pub fn draw(&mut self, f: &Frame) -> RenderStats {
        let cam = f.camera;
        let lod = self.options.lod.unwrap_or(cam.lod());
        let view = cam.view_rect(2.0);
        let mut stats = RenderStats {
            lod: Some(lod),
            ..Default::default()
        };
        self.labels.clear();

        let mut timer = SectionTimer::new();
        // Cull and upload people when the snapshot or the view changed.
        let t0 = std::time::Instant::now();
        if self.gather.needed(f.prev, f.cur, view) {
            let margin = (view.w.max(view.h) * 0.25).max(20.0);
            let area = Rect::new(
                view.x - margin,
                view.y - margin,
                view.w + 2.0 * margin,
                view.h + 2.0 * margin,
            );
            self.gather.run(f.prev, f.cur, area);
            let (people, gather) = (&mut self.people, &self.gather);
            with_gl(|ctx| {
                for g in 0..GROUPS {
                    people.upload(ctx, g, &gather.prev[g], &gather.cur[g]);
                }
            });
        }
        let fields_view = cam.view_rect(16.0);
        {
            let fields = &mut self.fields;
            with_gl(|ctx| fields.update(ctx, &f.cur.fields, fields_view));
        }
        stats.gather_ms = t0.elapsed().as_secs_f32() * 1000.0;
        timer.mark(0);

        // 1. Ground, props, objective areas, supply drops.
        clear_background(Color::from_rgba(
            map::OUTSIDE[0],
            map::OUTSIDE[1],
            map::OUTSIDE[2],
            255,
        ));
        set_camera(&cam.mq_camera());
        in_pass(|ctx| self.map.draw_ground(ctx, &cam));
        scene::draw_props(f.cur, view, cam.zoom);
        debug::draw_ground_markers(f.cur, &cam, f.highlight_slot, f.time, &mut self.labels);
        timer.mark(1);

        // 2–3. People: fallen, standing, carried.
        let (lying, standing): (&[Layer], &[Layer]) = match lod {
            Lod::Far => (&[Layer::Dot], &[Layer::Dot]),
            Lod::Middle => (&[Layer::Lying], &[Layer::Mid]),
            Lod::Close => (
                &[Layer::Lying],
                &[Layer::Body, Layer::Accessory, Layer::Head, Layer::Item],
            ),
        };
        // Far: people are abstract markers, drawn above the roofs instead.
        if lod != Lod::Far {
            self.draw_people(f, &cam, lying, standing, &mut stats);
        }
        stats.people = (0..GROUPS).map(|g| self.people.len(g)).sum();
        timer.mark(2);

        // 4–5. Vehicles and projectiles.
        scene::draw_vehicles(f.cur, view, f.time);
        scene::draw_projectiles(f.cur, view, cam.zoom);
        timer.mark(3);

        // 6. Gas, smoke, fire glow, particles.
        {
            let (fields, fs) = (&mut self.fields, &f.cur.fields);
            in_pass(|ctx| fields.draw_hazards(ctx, fs, &cam, f.time));
            stats.field_chunks = fields.drawn;
        }
        timer.mark(4);
        self.effects
            .update(f.dt, f.cur, &self.fields.burning, cam.view_rect(10.0));
        self.effects.draw(view, cam.zoom);
        stats.particles = self.effects.len();
        timer.mark(5);

        // 7. Roofs, then the overlay heatmap and markers on top.
        if self.options.roofs {
            in_pass(|ctx| self.map.draw_roofs(ctx, &cam));
        }
        if lod == Lod::Far {
            self.draw_people(f, &cam, lying, standing, &mut stats);
        }
        if f.cur.fields.overlay_kind != Overlay::None {
            let (fields, fs) = (&mut self.fields, &f.cur.fields);
            in_pass(|ctx| fields.draw_overlay(ctx, fs, &cam, f.time));
        }
        timer.mark(6);
        if self.options.debug {
            debug::draw_debug_markers(f.cur, &self.entries, &cam, &mut self.labels);
        }
        if let Some(id) = f.selected
            && let Some(p) = Self::person_pos(f.prev, f.cur, f.alpha, id)
        {
            debug::draw_selection(p, cam.zoom, f.time, f.chanting);
        }
        if let Some((at, r, col)) = f.tool {
            debug::draw_tool_preview(at, r, cam.zoom, col);
        }

        set_default_camera();
        debug::draw_labels(&self.labels, &cam);
        // Submit macroquad's batch now so its cost lands in this section.
        with_gl(|_| ());
        timer.mark(7);
        stats.sections_ms = timer.ms;
        stats
    }

    fn draw_people(
        &self,
        f: &Frame,
        cam: &Camera,
        lying: &[Layer],
        standing: &[Layer],
        stats: &mut RenderStats,
    ) {
        if !self.people.instancing {
            self.draw_people_fallback(cam);
            return;
        }
        let people = &self.people;
        let (alpha, time) = (f.alpha, f.time);
        stats.people_draws = in_pass(|ctx| {
            people.draw(ctx, FALLEN, lying, cam, alpha, time)
                + people.draw(ctx, STANDING, standing, cam, alpha, time)
                + people.draw(ctx, CARRIED, lying, cam, alpha, time)
        });
    }

    /// Circles for GL contexts without instancing.
    fn draw_people_fallback(&self, cam: &Camera) {
        let r = (0.25f32).max(1.2 / cam.zoom);
        for g in 0..GROUPS {
            for p in &self.gather.cur[g] {
                let col = if p.flags & snap_flag::OFFICER != 0 {
                    Color::new(0.2, 0.42, 1.0, 1.0)
                } else if p.flags & snap_flag::INJURED != 0 {
                    Color::new(0.95, 0.2, 0.25, 1.0)
                } else {
                    Color::new(1.0, 0.56, 0.16, 1.0)
                };
                draw_circle(p.x, p.y, r, col);
            }
        }
    }
}
