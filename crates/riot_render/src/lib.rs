//! Snapshots → GPU (DESIGN §18): a top-down pixel-art renderer for Riotter.
//!
//! The renderer never reads `World`; it only reads [`Snapshot`]s. Every
//! picture it draws is pixel art generated at start-up ([`art`]): people
//! ([`sprites`]), ground and roof materials ([`materials`]), decals, props
//! and vehicles ([`sheets`]). The world is drawn into an offscreen buffer
//! whose pixels are the art's pixels, then scaled up to the screen
//! ([`pixel`]). Each frame it draws, bottom to top and without sorting:
//!
//! 1. ground, decals and shadows (static mesh from the [`MapDef`]), then
//!    props and their shadows, objective areas and supply drops;
//! 2. fallen people;
//! 3. people's shadows and standing people (layered paper-doll sprites, one
//!    instanced draw per layer);
//! 4. vehicles;
//! 5. projectiles;
//! 6. gas, smoke and fire glow (dithered field textures) and particles;
//! 7. roofs, rooftop plant and tree canopies, then the optional debug
//!    overlay heatmap. At far zoom people are abstract markers wider than
//!    the streets, so they are drawn above the roofs instead of in 2–3.
//!
//! The buffer is then composited to the screen with a colour grade for the
//! time of day, and markers, labels and tools are drawn on top at screen
//! resolution.
//!
//! People are culled on the CPU only when a new snapshot arrives or the view
//! leaves the culled area (with a margin); position and heading are
//! interpolated on the GPU with the frame's alpha.

pub mod art;
pub mod camera;
pub mod debug;
pub mod effects;
pub mod fields;
pub mod map;
pub mod materials;
pub mod mesh;
pub mod pipeline;
pub mod pixel;
pub mod scene;
pub mod sheets;
pub mod sprites;

use macroquad::miniquad::{PassAction, RenderingBackend};
use macroquad::prelude::*;
use riot_sim::mapdef::{EntryDef, MapDef};
use riot_sim::snapshot::{Overlay, SnapPerson, Snapshot, snap_flag};
use riot_sim::state::Gait;

pub use camera::{Camera, Lod};
use mesh::{MeshBuilder, MeshRenderer, StreamMesh};
use pipeline::{Layer, PeoplePipeline};
use pixel::{PixelBuffer, View};
use sheets::{DecalAtlas, ThingAtlas};

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
    /// Draw through the pixel buffer at the art's densities. Off draws
    /// straight at the zoom.
    pub pixel_art: bool,
    /// Pixel size: 1 is the art's own; 2 makes every pixel twice as big.
    pub chunk: f32,
    /// Colour grade by the time of day.
    pub grade: bool,
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions {
            lod: None,
            debug: true,
            roofs: true,
            pixel_art: true,
            chunk: 1.0,
            grade: true,
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
    /// Art density the world was drawn at (px/m), and whether through the
    /// pixel buffer.
    pub density: f32,
    pub pixel: bool,
    /// People instances drawn.
    pub people: usize,
    /// Instanced draw calls for people.
    pub people_draws: usize,
    pub field_chunks: usize,
    pub particles: usize,
    /// Map triangles drawn.
    pub map_triangles: usize,
    /// CPU time spent culling and uploading people and updating field
    /// textures this frame (ms).
    pub gather_ms: f32,
    /// CPU time per section of [`Renderer::draw`] (ms), see [`SECTIONS`].
    pub sections_ms: [f32; SECTIONS.len()],
}

/// Names of the timed sections of a frame, in draw order.
pub const SECTIONS: [&str; 9] = [
    "upload",
    "ground",
    "people",
    "scene",
    "hazards",
    "effects",
    "roofs",
    "composite",
    "markers",
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
    meshes: MeshRenderer,
    map: map::MapRenderer,
    people: PeoplePipeline,
    fields: fields::FieldRenderer,
    effects: effects::Effects,
    pixel: PixelBuffer,
    things: ThingAtlas,
    props: StreamMesh,
    vehicles: StreamMesh,
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

/// Like [`with_gl`], inside a render pass on the pixel buffer.
fn in_buffer<R>(
    pass: macroquad::miniquad::RenderPass,
    action: PassAction,
    f: impl FnOnce(&mut dyn RenderingBackend) -> R,
) -> R {
    with_gl(|ctx| {
        ctx.begin_pass(Some(pass), action);
        let r = f(ctx);
        ctx.end_render_pass();
        r
    })
}

/// Colour grade for a minute of the day: multiply (rgb) and saturation.
fn grade(minute: f32) -> [f32; 4] {
    const KEYS: [(f32, [f32; 4]); 9] = [
        (0.0, [0.6, 0.66, 0.9, 0.7]),
        (5.0, [0.6, 0.66, 0.9, 0.7]),
        (7.0, [1.04, 0.96, 0.9, 0.95]),
        (9.0, [1.0, 1.0, 1.0, 1.0]),
        (16.5, [1.0, 1.0, 1.0, 1.0]),
        (18.5, [1.07, 0.99, 0.88, 1.06]),
        (20.0, [0.86, 0.78, 0.82, 0.9]),
        (21.5, [0.6, 0.66, 0.9, 0.7]),
        (24.0, [0.6, 0.66, 0.9, 0.7]),
    ];
    let h = (minute / 60.0).rem_euclid(24.0);
    for w in KEYS.windows(2) {
        let ((h0, a), (h1, b)) = (w[0], w[1]);
        if h >= h0 && h <= h1 {
            let t = ((h - h0) / (h1 - h0).max(1e-3)).clamp(0.0, 1.0);
            return [0, 1, 2, 3].map(|k| a[k] + (b[k] - a[k]) * t);
        }
    }
    [1.0; 4]
}

impl Renderer {
    /// Build the sheets, palettes, static map meshes and pipelines. Call once
    /// the macroquad window exists.
    pub fn new(map: &MapDef) -> Renderer {
        let atlases: Vec<sprites::Atlas> = (0..art::LEVELS).map(sprites::build_atlas).collect();
        let palette = sprites::build_palette();
        let decals = DecalAtlas::layout();
        let things = ThingAtlas::layout();
        let decal_imgs: Vec<art::Img> = (0..art::LEVELS).map(|l| decals.paint(l).img).collect();
        let thing_imgs: Vec<art::Img> = (0..art::LEVELS).map(|l| things.paint(l).img).collect();
        with_gl(|ctx| {
            let people = PeoplePipeline::new(ctx, &atlases, &palette, GROUPS);
            if !people.instancing {
                eprintln!("riot_render: no GL instancing; people fall back to circles");
            }
            let meshes = MeshRenderer::new(ctx, &decal_imgs, &thing_imgs);
            let map_r = map::MapRenderer::new(ctx, &meshes, map, &decals);
            let props = meshes.new_stream(ctx);
            let vehicles = meshes.new_stream(ctx);
            Renderer {
                map: map_r,
                people,
                fields: fields::FieldRenderer::new(ctx),
                effects: effects::Effects::new(),
                pixel: PixelBuffer::new(ctx),
                meshes,
                things,
                props,
                vehicles,
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
        let opts = self.options;
        let view = View::new(&cam, opts.pixel_art, opts.chunk);
        let lod = opts.lod.unwrap_or(cam.lod());
        // The buffer covers at least the screen (more when scaled up), but
        // only what the screen shows is composited: cull to that.
        let area_view = cam.view_rect(2.0);
        let mut stats = RenderStats {
            lod: Some(lod),
            density: view.cam.zoom,
            pixel: view.pixel,
            ..Default::default()
        };
        self.labels.clear();

        let mut timer = SectionTimer::new();
        // Cull and upload people when the snapshot or the view changed.
        let t0 = std::time::Instant::now();
        if self.gather.needed(f.prev, f.cur, area_view) {
            let margin = (area_view.w.max(area_view.h) * 0.25).max(20.0);
            let area = Rect::new(
                area_view.x - margin,
                area_view.y - margin,
                area_view.w + 2.0 * margin,
                area_view.h + 2.0 * margin,
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
        // Props and vehicles for this frame.
        {
            let mut pm = MeshBuilder::default();
            let mut vm = MeshBuilder::default();
            scene::build(&mut pm, f.cur, area_view, &view, &self.things, f.time);
            scene::build_drops(&mut pm, f.cur, area_view, &view, &self.things);
            // Vehicles are the builder's layers 2–3; split them off so they
            // draw above people.
            scene::build_vehicles(&mut vm, f.cur, area_view, &view, &self.things, f.time);
            let (meshes, props, vehicles) = (&self.meshes, &mut self.props, &mut self.vehicles);
            with_gl(|ctx| {
                meshes.update_stream(ctx, props, pm);
                meshes.update_stream(ctx, vehicles, vm);
            });
        }
        stats.gather_ms = t0.elapsed().as_secs_f32() * 1000.0;
        timer.mark(0);

        let rt = self.pixel.ensure(&view).clone();
        let pass = rt.render_pass.raw_miniquad_id();
        let o = map::OUTSIDE;
        let clear = PassAction::Clear {
            color: Some((
                o[0] as f32 / 255.0,
                o[1] as f32 / 255.0,
                o[2] as f32 / 255.0,
                1.0,
            )),
            depth: Some(1.0),
            stencil: None,
        };

        // 1. Ground, shadows, props, objective areas, supply drops.
        {
            let (map_r, meshes, props) = (&mut self.map, &self.meshes, &self.props);
            in_buffer(pass, clear, |ctx| {
                map_r.draw_ground(ctx, meshes, &view, f.time);
                meshes.draw_stream(ctx, props, &view, f.time);
            });
        }
        set_camera(&view.mq_camera(&rt));
        debug::draw_ground_markers(f.cur, &view.cam, f.highlight_slot, f.time, &mut self.labels);
        timer.mark(1);

        // 2–3. People: fallen, shadows, standing, carried.
        let (lying, standing): (&[Layer], &[Layer]) = match lod {
            Lod::Far => (&[Layer::Dot], &[Layer::Dot]),
            Lod::Middle => (&[Layer::Lying], &[Layer::Shadow, Layer::Mid]),
            Lod::Close => (
                &[Layer::Lying],
                &[
                    Layer::Shadow,
                    Layer::Body,
                    Layer::Accessory,
                    Layer::Head,
                    Layer::Item,
                ],
            ),
        };
        // Far: people are abstract markers, drawn above the roofs instead.
        if lod != Lod::Far {
            self.draw_people(f, &view, pass, lying, standing, &mut stats);
        }
        stats.people = (0..GROUPS).map(|g| self.people.len(g)).sum();
        timer.mark(2);

        // 4–5. Vehicles and projectiles.
        {
            let (meshes, vehicles) = (&self.meshes, &self.vehicles);
            in_buffer(pass, PassAction::Nothing, |ctx| {
                meshes.draw_stream(ctx, vehicles, &view, f.time);
            });
        }
        set_camera(&view.mq_camera(&rt));
        scene::draw_projectiles(f.cur, area_view, view.cam.zoom);
        timer.mark(3);

        // 6. Gas, smoke, fire glow, particles.
        {
            let (fields, fs) = (&mut self.fields, &f.cur.fields);
            in_buffer(pass, PassAction::Nothing, |ctx| {
                fields.draw_hazards(ctx, fs, &view.cam, f.time)
            });
            stats.field_chunks = fields.drawn;
        }
        timer.mark(4);
        self.effects
            .update(f.dt, f.cur, &self.fields.burning, cam.view_rect(10.0));
        set_camera(&view.mq_camera(&rt));
        self.effects.draw(area_view, view.px());
        stats.particles = self.effects.len();
        timer.mark(5);

        // 7. Roofs (their own shadows start from a clear depth), then far
        // people and the overlay heatmap on top.
        if opts.roofs {
            let (map_r, meshes) = (&mut self.map, &self.meshes);
            let clear_depth = PassAction::Clear {
                color: None,
                depth: Some(1.0),
                stencil: None,
            };
            in_buffer(pass, clear_depth, |ctx| {
                map_r.draw_roofs(ctx, meshes, &view, f.time)
            });
        }
        stats.map_triangles = self.map.triangles;
        if lod == Lod::Far {
            self.draw_people(f, &view, pass, lying, standing, &mut stats);
        }
        if f.cur.fields.overlay_kind != Overlay::None {
            let (fields, fs) = (&mut self.fields, &f.cur.fields);
            in_buffer(pass, PassAction::Nothing, |ctx| {
                fields.draw_overlay(ctx, fs, &view.cam, f.time)
            });
        }
        timer.mark(6);

        // Composite to the screen.
        set_default_camera();
        let g = if opts.grade {
            grade(f.cur.stats.clock_minute)
        } else {
            [1.0; 4]
        };
        {
            let px = &self.pixel;
            with_gl(|ctx| {
                ctx.begin_default_pass(PassAction::clear_color(0.0, 0.0, 0.0, 1.0));
                px.composite(ctx, &view, &cam, g);
                ctx.end_render_pass();
            });
        }
        timer.mark(7);

        // Markers, selection and tools at screen resolution.
        set_camera(&cam.mq_camera());
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
        timer.mark(8);
        stats.sections_ms = timer.ms;
        stats
    }

    fn draw_people(
        &mut self,
        f: &Frame,
        view: &View,
        pass: macroquad::miniquad::RenderPass,
        lying: &[Layer],
        standing: &[Layer],
        stats: &mut RenderStats,
    ) {
        if !self.people.instancing {
            self.draw_people_fallback(view);
            return;
        }
        let people = &mut self.people;
        let (alpha, time) = (f.alpha, f.time);
        stats.people_draws = in_buffer(pass, PassAction::Nothing, |ctx| {
            people.draw(ctx, FALLEN, lying, view, alpha, time)
                + people.draw(ctx, STANDING, standing, view, alpha, time)
                + people.draw(ctx, CARRIED, lying, view, alpha, time)
        });
    }

    /// Circles for GL contexts without instancing.
    fn draw_people_fallback(&self, view: &View) {
        let r = (0.25f32).max(1.2 / view.cam.zoom);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grade_is_neutral_by_day_and_dark_at_night() {
        assert_eq!(grade(12.0 * 60.0), [1.0; 4]);
        assert!(grade(23.0 * 60.0)[0] < 0.7);
        let dusk = grade(19.0 * 60.0);
        assert!(dusk[0] < 1.1 && dusk[0] > 0.6);
    }
}
