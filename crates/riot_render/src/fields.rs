//! Field layers as textures (DESIGN §18.2 effects and debug overlays).
//!
//! Every active field chunk in the snapshot (64 × 64 cells of 2 m) becomes
//! one RGBA texture: gas, smoke, fire and the selected debug overlay. Each
//! texture carries a one-cell halo copied from its neighbours, so GPU
//! bilinear filtering is seamless across chunk borders. Only chunks near the
//! view are uploaded, and only when the snapshot's field tick changes.

use std::collections::HashMap;

use macroquad::miniquad::*;
use macroquad::prelude::Rect;
use riot_snapshot::FieldSnap;

use crate::camera::Camera;

/// Texels per side: 64 cells plus a one-cell halo on each side.
const TEX: usize = 66;

struct ChunkTex {
    tex: TextureId,
    bindings: Bindings,
    /// Field tick this texture holds, if any.
    tick: Option<u32>,
    /// Something to draw in the hazard (gas/smoke/fire) and overlay passes.
    hazard: bool,
    overlay: bool,
    seen: bool,
}

#[repr(C)]
struct Uniforms {
    cam: [f32; 4],
    /// x0, y0, size, mode (0 hazards, 1 overlay)
    rect: [f32; 4],
    /// time, metres per buffer pixel, unused ×2
    params: [f32; 4],
}

/// A burning field cell, for fire particles.
#[derive(Clone, Copy, Debug)]
pub struct Burning {
    pub x: f32,
    pub y: f32,
    /// 0..1
    pub heat: f32,
}

/// GPU textures of the snapshot's field chunks.
pub struct FieldRenderer {
    pipeline: Pipeline,
    quad_vb: BufferId,
    quad_ib: BufferId,
    chunks: HashMap<[u16; 2], ChunkTex>,
    free: Vec<ChunkTex>,
    index: HashMap<[u16; 2], usize>,
    staging: Vec<u8>,
    /// Field tick of `index` and `burning`.
    seen_tick: Option<u32>,
    /// Burning cells of the last field snapshot.
    pub burning: Vec<Burning>,
    /// Chunks drawn in the last frame.
    pub drawn: usize,
}

impl FieldRenderer {
    /// Create the field pipeline; chunk textures are made on demand.
    pub fn new(ctx: &mut dyn RenderingBackend) -> FieldRenderer {
        let corners: [[f32; 2]; 4] = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        let quad_vb = ctx.new_buffer(
            BufferType::VertexBuffer,
            BufferUsage::Immutable,
            BufferSource::slice(&corners),
        );
        let quad_ib = ctx.new_buffer(
            BufferType::IndexBuffer,
            BufferUsage::Immutable,
            BufferSource::slice(&[0u16, 1, 2, 0, 2, 3]),
        );
        let shader = ctx
            .new_shader(
                ShaderSource::Glsl {
                    vertex: VERTEX,
                    fragment: FRAGMENT,
                },
                ShaderMeta {
                    images: vec!["field_tex".to_string()],
                    uniforms: UniformBlockLayout {
                        uniforms: vec![
                            UniformDesc::new("cam", UniformType::Float4),
                            UniformDesc::new("rect", UniformType::Float4),
                            UniformDesc::new("params", UniformType::Float4),
                        ],
                    },
                },
            )
            .unwrap_or_else(|e| panic!("field shader: {e}"));
        let pipeline = ctx.new_pipeline(
            &[BufferLayout::default()],
            &[VertexAttribute::new("corner", VertexFormat::Float2)],
            shader,
            PipelineParams {
                color_blend: Some(BlendState::new(
                    Equation::Add,
                    BlendFactor::One,
                    BlendFactor::OneMinusValue(BlendValue::SourceAlpha),
                )),
                ..Default::default()
            },
        );
        FieldRenderer {
            pipeline,
            quad_vb,
            quad_ib,
            chunks: HashMap::new(),
            free: Vec::new(),
            index: HashMap::new(),
            staging: vec![0; TEX * TEX * 4],
            seen_tick: None,
            burning: Vec::new(),
            drawn: 0,
        }
    }

    fn chunk_rect(fs: &FieldSnap, c: [u16; 2]) -> Rect {
        let size = fs.chunk_cells as f32 * fs.cell;
        Rect::new(
            c[0] as f32 * size - fs.half,
            c[1] as f32 * size - fs.half,
            size,
            size,
        )
    }

    /// Bring textures of chunks overlapping `view` up to date.
    pub fn update(&mut self, ctx: &mut dyn RenderingBackend, fs: &FieldSnap, view: Rect) {
        let cc = fs.chunk_cells as usize;
        if cc == 0 || fs.chunks.is_empty() {
            for (_, c) in self.chunks.drain() {
                self.free.push(c);
            }
            self.burning.clear();
            self.seen_tick = Some(fs.tick);
            return;
        }
        let cl = cc * cc;
        if self.seen_tick != Some(fs.tick) {
            self.seen_tick = Some(fs.tick);
            self.index.clear();
            self.burning.clear();
            for (k, &c) in fs.chunks.iter().enumerate() {
                self.index.insert(c, k);
                // A layer may be empty when nothing on the map has it.
                let Some(fire) = chunk_layer(&fs.fire, k, cl) else {
                    continue;
                };
                for (i, &f) in fire.iter().enumerate() {
                    if f > 24 {
                        let r = Self::chunk_rect(fs, c);
                        self.burning.push(Burning {
                            x: r.x + ((i % cc) as f32 + 0.5) * fs.cell,
                            y: r.y + ((i / cc) as f32 + 0.5) * fs.cell,
                            heat: f as f32 / 255.0,
                        });
                    }
                }
            }
            // Return textures of chunks that are no longer active.
            let gone: Vec<[u16; 2]> = self
                .chunks
                .keys()
                .filter(|c| !self.index.contains_key(*c))
                .copied()
                .collect();
            for c in gone {
                if let Some(t) = self.chunks.remove(&c) {
                    self.free.push(t);
                }
            }
        }
        for t in self.chunks.values_mut() {
            t.seen = false;
        }
        for k in 0..fs.chunks.len() {
            let c = fs.chunks[k];
            if !Self::chunk_rect(fs, c).overlaps(&view) {
                continue;
            }
            if !self.chunks.contains_key(&c) {
                let t = match self.free.pop() {
                    Some(mut t) => {
                        t.tick = None;
                        t
                    }
                    None => self.new_chunk_tex(ctx),
                };
                self.chunks.insert(c, t);
            }
            let fresh = self.chunks[&c].tick == Some(fs.tick);
            if !fresh {
                let (hazard, overlay) = self.fill_staging(fs, k, cc);
                let t = self.chunks.get_mut(&c).expect("inserted above");
                ctx.texture_update(t.tex, &self.staging);
                t.tick = Some(fs.tick);
                t.hazard = hazard;
                t.overlay = overlay;
            }
            if let Some(t) = self.chunks.get_mut(&c) {
                t.seen = true;
            }
        }
    }

    fn new_chunk_tex(&self, ctx: &mut dyn RenderingBackend) -> ChunkTex {
        let tex = ctx.new_texture(
            TextureAccess::Static,
            TextureSource::Empty,
            TextureParams {
                width: TEX as u32,
                height: TEX as u32,
                format: TextureFormat::RGBA8,
                wrap: TextureWrap::Clamp,
                min_filter: FilterMode::Linear,
                mag_filter: FilterMode::Linear,
                ..Default::default()
            },
        );
        ChunkTex {
            tex,
            bindings: Bindings {
                vertex_buffers: vec![self.quad_vb],
                index_buffer: self.quad_ib,
                images: vec![tex],
            },
            tick: None,
            hazard: false,
            overlay: false,
            seen: false,
        }
    }

    /// Pack chunk `k` with its halo into `staging`: R gas, G smoke, B fire,
    /// A overlay. Returns whether the hazard and overlay channels have
    /// anything to draw.
    fn fill_staging(&mut self, fs: &FieldSnap, k: usize, cc: usize) -> (bool, bool) {
        let cl = cc * cc;
        let c = fs.chunks[k];
        // This chunk and its 8 neighbours: (gas, smoke, fire, overlay),
        // each `None` when the chunk or the layer is absent.
        type Layers<'a> = [Option<&'a [u8]>; 4];
        let mut near: [[Layers; 3]; 3] = [[[None; 4]; 3]; 3];
        for (dy, row) in near.iter_mut().enumerate() {
            for (dx, cell) in row.iter_mut().enumerate() {
                let (nx, ny) = (c[0] as i32 + dx as i32 - 1, c[1] as i32 + dy as i32 - 1);
                let s = if dx == 1 && dy == 1 {
                    Some(k)
                } else if nx < 0 || ny < 0 {
                    None
                } else {
                    self.index.get(&[nx as u16, ny as u16]).copied()
                };
                if let Some(s) = s {
                    *cell = [
                        chunk_layer(&fs.gas, s, cl),
                        chunk_layer(&fs.smoke, s, cl),
                        chunk_layer(&fs.fire, s, cl),
                        chunk_layer(&fs.overlay, s, cl),
                    ];
                }
            }
        }
        let (mut hazard, mut overlay) = (false, false);
        let n = cc as i32;
        for ty in 0..TEX {
            for tx in 0..TEX {
                // Cell coordinates relative to this chunk, -1..=64.
                let (x, y) = (tx as i32 - 1, ty as i32 - 1);
                let (dx, dy) = (x.div_euclid(n), y.div_euclid(n));
                let layers = &near[(dy + 1) as usize][(dx + 1) as usize];
                let i = y.rem_euclid(n) as usize * cc + x.rem_euclid(n) as usize;
                let o = (ty * TEX + tx) * 4;
                let px = &mut self.staging[o..o + 4];
                for (v, l) in px.iter_mut().zip(layers) {
                    *v = l.and_then(|l| l.get(i).copied()).unwrap_or(0);
                }
                hazard |= (px[0] | px[1] | px[2]) > 1;
                overlay |= px[3] > 0;
            }
        }
        (hazard, overlay)
    }

    fn draw_pass(
        &mut self,
        ctx: &mut dyn RenderingBackend,
        fs: &FieldSnap,
        cam: &Camera,
        mode: u32,
        time: f32,
    ) {
        ctx.apply_pipeline(&self.pipeline);
        for (&c, t) in &self.chunks {
            let wanted = if mode == 0 { t.hazard } else { t.overlay };
            if !t.seen || !wanted || t.tick.is_none() {
                continue;
            }
            let r = Self::chunk_rect(fs, c);
            ctx.apply_bindings(&t.bindings);
            ctx.apply_uniforms(UniformsSource::table(&Uniforms {
                cam: cam.uniform(),
                rect: [r.x, r.y, r.w, mode as f32],
                params: [time, 1.0 / cam.zoom.max(1e-3), 0.0, 0.0],
            }));
            ctx.draw(0, 6, 1);
            self.drawn += 1;
        }
    }

    /// Layer 6: gas, smoke and fire glow. Must run inside a render pass.
    pub fn draw_hazards(
        &mut self,
        ctx: &mut dyn RenderingBackend,
        fs: &FieldSnap,
        cam: &Camera,
        time: f32,
    ) {
        self.drawn = 0;
        self.draw_pass(ctx, fs, cam, 0, time);
    }

    /// The debug overlay heatmap. Must run inside a render pass.
    pub fn draw_overlay(
        &mut self,
        ctx: &mut dyn RenderingBackend,
        fs: &FieldSnap,
        cam: &Camera,
        time: f32,
    ) {
        self.draw_pass(ctx, fs, cam, 1, time);
    }
}

/// Chunk `k`'s cells of one layer, or `None` when the layer is empty
/// (nothing anywhere on the map has it) or too short.
fn chunk_layer(layer: &[u8], k: usize, cl: usize) -> Option<&[u8]> {
    layer.get(k * cl..(k + 1) * cl)
}

const VERTEX: &str = r#"#version 100
precision highp float;
attribute vec2 corner;
uniform vec4 cam;
uniform vec4 rect;
uniform vec4 params;
varying vec2 v_uv;
varying vec2 v_world;
varying float v_mode;
varying float v_time;
varying float v_px;
void main() {
    vec2 w = rect.xy + corner * rect.z;
    v_world = w;
    v_mode = rect.w;
    v_time = params.x;
    v_px = params.y;
    // Texel centres: the chunk spans texels 1..65 of 66.
    v_uv = (corner * 64.0 + 1.0) / 66.0;
    gl_Position = vec4((w - cam.xy) * cam.zw, 0.0, 1.0);
}
"#;

const FRAGMENT: &str = r#"#version 100
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
varying vec2 v_uv;
varying vec2 v_world;
varying float v_mode;
varying float v_time;
varying float v_px;
uniform sampler2D field_tex;

// Ordered dithering on a 4×4 Bayer matrix, anchored to the world's pixels.
float bayer2(vec2 a) {
    a = floor(a);
    return fract(dot(a, vec2(0.5, a.y * 0.75)));
}
float bayer4(vec2 a) {
    return bayer2(0.5 * a) * 0.25 + bayer2(a);
}

vec3 ramp(float v) {
    vec3 c0 = vec3(0.10, 0.12, 0.55);
    vec3 c1 = vec3(0.05, 0.60, 0.90);
    vec3 c2 = vec3(0.25, 0.85, 0.35);
    vec3 c3 = vec3(1.00, 0.85, 0.10);
    vec3 c4 = vec3(0.92, 0.12, 0.10);
    float x = clamp(v, 0.0, 1.0) * 4.0;
    if (x < 1.0) return mix(c0, c1, x);
    if (x < 2.0) return mix(c1, c2, x - 1.0);
    if (x < 3.0) return mix(c2, c3, x - 2.0);
    return mix(c3, c4, x - 3.0);
}

void main() {
    vec4 t = texture2D(field_tex, v_uv);
    if (v_mode < 0.5) {
        float time = v_time;
        // Cosmetic billowing on the render clock.
        float n = 0.78 + 0.22 * sin(v_world.x * 0.23 + time * 0.8) * sin(v_world.y * 0.19 - time * 0.6);
        // Thin gas must still read and dense gas must not hide everything.
        // Smoothstep's flat start keeps the lowest quantization steps from
        // drawing hard contours.
        float ga = smoothstep(0.012, 0.3, t.r) * 0.72 * n;
        float sa = smoothstep(0.012, 0.25, t.g) * 0.85 * n;
        vec3 c = vec3(0.86, 0.90, 0.62) * ga;
        float a = ga;
        c = vec3(0.30, 0.30, 0.32) * sa + c * (1.0 - sa);
        a = sa + a * (1.0 - sa);
        float f = clamp(t.b * 1.6, 0.0, 1.0);
        // Pixel art: alpha in four steps, dithered between them, on the
        // world's pixel grid (premultiplied colour follows).
        vec2 cell = floor(v_world / v_px);
        float q = clamp(floor(a * 4.0 + bayer4(cell)) / 4.0, 0.0, 1.0);
        c = a > 0.001 ? c * (q / a) : vec3(0.0);
        a = q;
        // Fire glows: emissive light added on top, in steps too.
        float fl = f * (0.75 + 0.25 * sin(time * 11.0 + floor(v_world.x)));
        fl = floor(fl * 5.0 + bayer4(cell + 2.0)) / 5.0;
        c += vec3(1.0, 0.42, 0.08) * fl;
        a = a + fl * 0.3 * (1.0 - a);
        gl_FragColor = vec4(c, a);
    } else {
        float v = t.a;
        if (v < 0.004) discard;
        v = sqrt(v);
        float a = (0.3 + 0.4 * v) * smoothstep(0.0, 0.12, v);
        gl_FragColor = vec4(ramp(v) * a, a);
    }
}
"#;
