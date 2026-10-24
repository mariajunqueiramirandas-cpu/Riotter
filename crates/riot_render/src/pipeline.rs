//! The instanced people pipeline (DESIGN §18.2) on macroquad's miniquad
//! backend.
//!
//! Each person is one instance. The two most recent snapshots' records
//! ([`SnapPerson`], 16 bytes) are uploaded as two per-instance vertex
//! buffers in the same order, and the vertex shader blends position and
//! heading with the frame's alpha, decodes the gait, stride, flags and
//! appearance bits, picks the atlas frame and the palette colours, and
//! rotates the quad by heading. The CPU does no per-person work per frame.
//!
//! Each sprite layer (body, accessory, head, item) is one instanced draw.

use macroquad::miniquad::*;
use riot_sim::snapshot::SnapPerson;

use crate::camera::Camera;
use crate::sprites::{self, ATLAS_H, ATLAS_W, Atlas, PAL_H, PAL_W, Palette};

/// What one instanced draw paints. The shader mirrors these numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layer {
    Body = 0,
    Accessory = 1,
    Head = 2,
    Item = 3,
    /// Fallen or carried: lying on the ground.
    Lying = 4,
    /// Middle level of detail: one sprite per person.
    Mid = 5,
    /// Far level of detail: a quad coloured by state.
    Dot = 6,
}

/// Per-instance buffers for one group of people (e.g. the fallen).
struct InstanceSet {
    prev: BufferId,
    cur: BufferId,
    cap: usize,
    len: usize,
    bindings: Bindings,
}

#[repr(C)]
struct Uniforms {
    cam: [f32; 4],
    /// alpha, layer, zoom (px/m), render time (s)
    params: [f32; 4],
    /// columns, u per cell, v per cell, metres per cell
    atlas: [f32; 4],
}

/// GPU state for drawing people.
pub struct PeoplePipeline {
    pipeline: Pipeline,
    quad_vb: BufferId,
    quad_ib: BufferId,
    atlas: TextureId,
    palette: TextureId,
    sets: Vec<InstanceSet>,
    /// False on GL 2 contexts without instancing; the caller falls back.
    pub instancing: bool,
}

impl PeoplePipeline {
    /// Create the pipeline, upload the atlas (with mipmaps) and palettes.
    /// `groups` is the number of independent instance sets.
    pub fn new(
        ctx: &mut dyn RenderingBackend,
        atlas: &Atlas,
        palette: &Palette,
        groups: usize,
    ) -> PeoplePipeline {
        let corners: [[f32; 2]; 4] = [[-0.5, -0.5], [0.5, -0.5], [0.5, 0.5], [-0.5, 0.5]];
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
        let atlas_tex = ctx.new_texture(
            TextureAccess::Static,
            TextureSource::Bytes(&atlas.rgba),
            TextureParams {
                width: ATLAS_W as u32,
                height: ATLAS_H as u32,
                format: TextureFormat::RGBA8,
                min_filter: FilterMode::Linear,
                mag_filter: FilterMode::Linear,
                mipmap_filter: MipmapFilterMode::Linear,
                allocate_mipmaps: true,
                ..Default::default()
            },
        );
        ctx.texture_generate_mipmaps(atlas_tex);
        let palette_tex = ctx.new_texture(
            TextureAccess::Static,
            TextureSource::Bytes(&palette.rgba),
            TextureParams {
                width: PAL_W as u32,
                height: PAL_H as u32,
                format: TextureFormat::RGBA8,
                min_filter: FilterMode::Nearest,
                mag_filter: FilterMode::Nearest,
                ..Default::default()
            },
        );
        let shader = ctx
            .new_shader(
                ShaderSource::Glsl {
                    vertex: VERTEX,
                    fragment: FRAGMENT,
                },
                ShaderMeta {
                    images: vec!["atlas_tex".to_string(), "palette_tex".to_string()],
                    uniforms: UniformBlockLayout {
                        uniforms: vec![
                            UniformDesc::new("cam", UniformType::Float4),
                            UniformDesc::new("params", UniformType::Float4),
                            UniformDesc::new("atlas", UniformType::Float4),
                        ],
                    },
                },
            )
            .unwrap_or_else(|e| panic!("people shader: {e}"));
        let instance = BufferLayout {
            stride: std::mem::size_of::<SnapPerson>() as i32,
            step_func: VertexStep::PerInstance,
            step_rate: 1,
        };
        let pipeline = ctx.new_pipeline(
            &[BufferLayout::default(), instance.clone(), instance],
            &[
                VertexAttribute::with_buffer("corner", VertexFormat::Float2, 0),
                VertexAttribute::with_buffer("pos0", VertexFormat::Float2, 1),
                VertexAttribute::with_buffer("pose0", VertexFormat::Byte4, 1),
                VertexAttribute::with_buffer("pos1", VertexFormat::Float2, 2),
                VertexAttribute::with_buffer("pose1", VertexFormat::Byte4, 2),
                VertexAttribute::with_buffer("app1", VertexFormat::Byte4, 2),
            ],
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
        let instancing = ctx.info().features.instancing;
        let mut p = PeoplePipeline {
            pipeline,
            quad_vb,
            quad_ib,
            atlas: atlas_tex,
            palette: palette_tex,
            sets: Vec::with_capacity(groups),
            instancing,
        };
        for _ in 0..groups {
            let set = p.new_set(ctx, 1024);
            p.sets.push(set);
        }
        p
    }

    fn new_set(&self, ctx: &mut dyn RenderingBackend, cap: usize) -> InstanceSet {
        let mk = |ctx: &mut dyn RenderingBackend| {
            ctx.new_buffer(
                BufferType::VertexBuffer,
                BufferUsage::Stream,
                BufferSource::empty::<SnapPerson>(cap),
            )
        };
        let prev = mk(ctx);
        let cur = mk(ctx);
        InstanceSet {
            prev,
            cur,
            cap,
            len: 0,
            bindings: Bindings {
                vertex_buffers: vec![self.quad_vb, prev, cur],
                index_buffer: self.quad_ib,
                images: vec![self.atlas, self.palette],
            },
        }
    }

    /// Upload one group's instances: `prev[i]` and `cur[i]` are the same
    /// person in the previous and the current snapshot.
    pub fn upload(
        &mut self,
        ctx: &mut dyn RenderingBackend,
        group: usize,
        prev: &[SnapPerson],
        cur: &[SnapPerson],
    ) {
        debug_assert_eq!(prev.len(), cur.len());
        if cur.len() > self.sets[group].cap {
            let old = &self.sets[group];
            ctx.delete_buffer(old.prev);
            ctx.delete_buffer(old.cur);
            let cap = cur.len().next_power_of_two();
            self.sets[group] = self.new_set(ctx, cap);
        }
        let set = &mut self.sets[group];
        set.len = cur.len();
        if set.len > 0 {
            ctx.buffer_update(set.prev, BufferSource::slice(prev));
            ctx.buffer_update(set.cur, BufferSource::slice(cur));
        }
    }

    /// Number of instances in a group.
    pub fn len(&self, group: usize) -> usize {
        self.sets[group].len
    }

    /// Draw one group with the given layers, bottom to top. Must run inside
    /// a render pass.
    pub fn draw(
        &self,
        ctx: &mut dyn RenderingBackend,
        group: usize,
        layers: &[Layer],
        cam: &Camera,
        alpha: f32,
        time: f32,
    ) -> usize {
        let set = &self.sets[group];
        if set.len == 0 || !self.instancing {
            return 0;
        }
        ctx.apply_pipeline(&self.pipeline);
        ctx.apply_bindings(&set.bindings);
        for &layer in layers {
            let u = Uniforms {
                cam: cam.uniform(),
                params: [alpha, layer as u32 as f32, cam.zoom, time],
                atlas: [
                    sprites::COLS as f32,
                    1.0 / sprites::COLS as f32,
                    1.0 / sprites::ROWS as f32,
                    sprites::CELL_M,
                ],
            };
            ctx.apply_uniforms(UniformsSource::table(&u));
            ctx.draw(0, 6, set.len as i32);
        }
        layers.len()
    }
}

/// Vertex shader. GLSL 100 has no integers, so bit fields are decoded with
/// `floor` and `mod` on exact small floats.
const VERTEX: &str = r#"#version 100
precision highp float;

attribute vec2 corner;
attribute vec2 pos0;
attribute vec4 pose0;   // heading, gait, stride, flags (0..255)
attribute vec2 pos1;
attribute vec4 pose1;
attribute vec4 app1;    // appearance bytes, little-endian

uniform vec4 cam;       // clip = (p - cam.xy) * cam.zw
uniform vec4 params;    // alpha, layer, zoom, time
uniform vec4 atlas;     // cols, u per cell, v per cell, metres per cell

varying vec2 v_uv;
varying vec2 v_pa;
varying vec2 v_pb;
varying vec2 v_pc;
varying vec4 v_flat;
varying float v_dot;

float field(float v, float div, float m) { return mod(floor((v + 0.5) / div), m); }
vec2 pal(float col, float row) { return vec2((col + 0.5) / 64.0, (row + 0.5) / 16.0); }

void main() {
    float alpha = params.x;
    float layer = params.y;

    // Interpolate between the previous and the current snapshot. New
    // arrivals and teleports (metro) snap to the current position.
    vec2 p = pos1;
    float h = pose1.x;
    if (field(pose0.w, 1.0, 2.0) > 0.5 && distance(pos0, pos1) < 4.0) {
        p = mix(pos0, pos1, alpha);
        h = pose0.x + (mod(pose1.x - pose0.x + 128.0, 256.0) - 128.0) * alpha;
    }
    float gait = pose1.y;
    float stride = pose1.z;
    float flags = pose1.w;

    // Appearance bits (riot_sim::snapshot::Appearance).
    float arch = field(app1.x, 1.0, 16.0);
    float body = field(app1.x, 16.0, 4.0);
    float skin = field(app1.x, 64.0, 4.0) + field(app1.y, 1.0, 2.0) * 4.0;
    float clothes = field(app1.y, 2.0, 64.0);
    float hair = field(app1.y, 128.0, 2.0) + field(app1.z, 1.0, 8.0) * 2.0;
    float hairc = field(app1.z, 8.0, 8.0);
    float item = field(app1.z, 64.0, 4.0) + field(app1.w, 1.0, 4.0) * 4.0;
    float acc = field(app1.w, 4.0, 16.0);
    float has_item = field(flags, 8.0, 2.0);
    if (has_item < 0.5) item = 0.0;

    // Size: armoured officers are wider; body type widens the shoulders.
    float s = arch > 8.5 ? 1.3 : (arch > 7.5 ? 1.12 : 1.0);
    // Zoomed out, keep a person at least a few pixels wide so crowds stay
    // legible: 4 px as a single sprite, 9 px when fully layered.
    float min_px = (layer > 3.5 && layer < 5.5) ? 4.0 : 9.0;
    s *= max(1.0, min_px / (params.z * 0.5));
    float wide = 0.92 + body * 0.06;
    float anim = floor(mod(params.w * 7.0 + app1.x + app1.y, 4.0));
    float varied = mod(clothes * 7.0 + 13.0, 64.0);
    vec2 headwear = (acc > 0.5 && acc < 1.5) || (acc > 3.5 && acc < 5.5)
        ? pal(varied, 9.0) : pal(acc, 4.0);

    float frame = -1.0;
    vec2 off = vec2(0.0);
    float rot = 0.0;
    vec2 pa = pal(clothes, 0.0);
    vec2 pb = pal(skin, 2.0);
    vec2 pc = pal(clothes, 1.0);
    v_flat = vec4(1.0);
    v_dot = layer > 5.5 ? 1.0 : 0.0;

    if (layer < 0.5) {
        // Body: one strip per gait.
        if (gait < 0.5) frame = 0.0;
        else if (gait < 1.5) frame = 1.0 + floor(stride / 32.0);
        else if (gait < 2.5) frame = 9.0 + floor(stride / 32.0);
        else if (gait < 3.5) frame = 17.0;
        else if (gait < 6.5) frame = 18.0;
        else if (gait < 7.5) frame = 19.0 + anim;
        else if (gait < 8.5) frame = 23.0 + anim;
        else frame = 27.0;
        pa = pal(clothes, 0.0);
        pb = pal(skin, 2.0);
        pc = pal(clothes, 1.0);
    } else if (layer < 1.5) {
        // Body accessories: backpack, scarf, hi-vis vest, press vest.
        if (acc > 3.5 && acc < 7.5) frame = 32.0 + acc - 4.0;
        pa = headwear;
        pb = pal(acc, 5.0);
        pc = pal(0.0, 6.0);
    } else if (layer < 2.5) {
        // Head: hair style, or a cap, helmet or balaclava.
        frame = (acc > 0.5 && acc < 3.5) ? 52.0 + acc - 1.0 : 36.0 + hair;
        if (gait > 1.5 && gait < 2.5) off = vec2(0.04, 0.0);
        if (gait > 2.5 && gait < 3.5) off = vec2(0.08, 0.0);
        pa = pal(hairc, 3.0);
        pb = pal(skin, 2.0);
        pc = headwear;
    } else if (layer < 3.5) {
        // Held item. Throws and strikes move it with the hand.
        if (item > 0.5) frame = 56.0 + item;
        if (gait > 6.5 && gait < 7.5) {
            off = anim < 0.5 ? vec2(-0.38, -0.06) : (anim < 1.5 ? vec2(-0.24, 0.02)
                : (anim < 2.5 ? vec2(0.06, 0.02) : vec2(0.32, -0.1)));
        } else if (gait > 7.5 && gait < 8.5) {
            rot = anim < 0.5 ? 0.9 : (anim < 1.5 ? 0.5 : (anim < 2.5 ? 0.0 : -0.6));
            off = anim < 0.5 ? vec2(-0.01, 0.14) : (anim < 1.5 ? vec2(0.19, 0.1)
                : (anim < 2.5 ? vec2(0.34, -0.1) : vec2(0.3, -0.26)));
        }
        pa = (item > 4.5 && item < 5.5) ? pal(mod(clothes * 11.0 + 5.0, 64.0), 9.0)
                                        : pal(item, 6.0);
        pb = pal(item, 7.0);
        pc = pal(item, 8.0);
    } else if (layer < 4.5) {
        frame = 67.0;
        pa = pal(clothes, 0.0);
        pb = pal(skin, 2.0);
        pc = pal(hairc, 3.0);
    } else if (layer < 5.5) {
        frame = (item > 1.5 && item < 2.5) ? 69.0 : 68.0;
        pa = pal(clothes, 0.0);
        pb = pal(skin, 2.0);
        pc = (acc > 0.5 && acc < 3.5) ? headwear : pal(hairc, 3.0);
    } else {
        // Far: a small quad coloured by state, at least ~2.5 px wide.
        frame = 0.0;
        float officer = field(flags, 2.0, 2.0);
        vec3 c = vec3(1.0, 0.56, 0.16);
        if (arch > 6.5 && arch < 7.5) c = vec3(0.72, 0.72, 0.62);
        if ((arch > 2.5 && arch < 3.5) || (arch > 4.5 && arch < 5.5)) c = vec3(0.78, 0.2, 0.14);
        if (arch > 5.5 && arch < 6.5) c = vec3(0.96, 0.96, 0.96);
        if (field(flags, 32.0, 2.0) > 0.5) c = vec3(1.0, 0.9, 0.1);
        if (officer > 0.5) c = vec3(0.2, 0.42, 1.0);
        if (field(flags, 4.0, 2.0) > 0.5) c = vec3(0.95, 0.2, 0.25);
        if (gait > 3.5 && gait < 5.5) c = vec3(0.55, 0.05, 0.08);
        if (gait > 5.5 && gait < 6.5) c = vec3(0.7, 0.3, 0.85);
        if (field(flags, 64.0, 2.0) > 0.5) c *= 0.7;
        v_flat = vec4(c, 1.0);
        float size = max(0.6, 3.0 / params.z);
        gl_Position = vec4((p + corner * size - cam.xy) * cam.zw, 0.0, 1.0);
        v_uv = vec2(0.0);
        v_pa = pa; v_pb = pb; v_pc = pc;
        return;
    }

    if (frame < 0.0) {
        gl_Position = vec4(2.0, 2.0, 2.0, 1.0);
        v_uv = vec2(0.0);
        v_pa = pa; v_pb = pb; v_pc = pc;
        return;
    }

    // Sprite space: +x forward, +y to the right, metres from the body centre.
    vec2 l = corner * atlas.w;
    if (rot != 0.0) {
        vec2 q = l - vec2(0.06, 0.28);
        float cr = cos(rot), sr = sin(rot);
        l = vec2(q.x * cr - q.y * sr, q.x * sr + q.y * cr) + vec2(0.06, 0.28);
    }
    if (layer > 1.5) wide = 1.0;
    l = (l + off) * vec2(s, s * wide);
    float a = h * (6.2831853 / 256.0);
    float c = cos(a), sn = sin(a);
    vec2 w = p + vec2(l.x * c - l.y * sn, l.x * sn + l.y * c);
    gl_Position = vec4((w - cam.xy) * cam.zw, 0.0, 1.0);

    float col = mod(frame, atlas.x);
    float row = floor(frame / atlas.x);
    v_uv = vec2((col + corner.x + 0.5) * atlas.y, (row + corner.y + 0.5) * atlas.z);
    v_pa = pa;
    v_pb = pb;
    v_pc = pc;
}
"#;

const FRAGMENT: &str = r#"#version 100
precision mediump float;

varying vec2 v_uv;
varying vec2 v_pa;
varying vec2 v_pb;
varying vec2 v_pc;
varying vec4 v_flat;
varying float v_dot;

uniform sampler2D atlas_tex;
uniform sampler2D palette_tex;

void main() {
    if (v_dot > 0.5) {
        gl_FragColor = v_flat;
        return;
    }
    vec4 t = texture2D(atlas_tex, v_uv);
    vec3 a = texture2D(palette_tex, v_pa).rgb;
    vec3 b = texture2D(palette_tex, v_pb).rgb;
    vec3 c = texture2D(palette_tex, v_pc).rgb;
    // Premultiplied: weights already carry coverage.
    gl_FragColor = vec4(t.r * a + t.g * b + t.b * c, t.a) * v_flat.a;
}
"#;
