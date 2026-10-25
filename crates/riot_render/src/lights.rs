//! Light at dusk and night: street lamps, fires, police light bars and
//! molotovs light pools of the city as the day's colour grade darkens it.
//!
//! Lights are drawn additively into a light buffer the size of the pixel
//! buffer, as round pools whose falloff comes in a few steps, dithered on
//! the world's pixel grid like the gas. The composite multiplies the scene
//! by the day's ambient light plus this buffer, so a lamp shows the
//! pavement under it in its own colours. By day nothing is drawn.

use macroquad::miniquad::*;
use riot_sim::mapdef::{Ground, MapDef};
use riot_sim::snapshot::Snapshot;

use crate::fields::Burning;
use crate::pixel::View;

/// A light pool: centre, radius (m), colour and strength.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Light {
    pub pos: [f32; 2],
    pub radius: f32,
    pub color: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct Vertex {
    pos: [f32; 2],
    /// -1..1 across the pool.
    local: [f32; 2],
    color: [f32; 3],
}

#[repr(C)]
struct Uniforms {
    cam: [f32; 4],
    /// metres per buffer pixel, unused ×3
    params: [f32; 4],
}

/// Street lamps along the roads, every 24 m on both sides.
pub fn street_lamps(map: &MapDef) -> Vec<Light> {
    let warm = [0.72, 0.56, 0.34];
    let mut out = Vec::new();
    let r = &map.roads;
    let painted = map.ground.cell > 0.0 && !map.ground.kinds.is_empty();
    for e in &r.edges {
        let (i, j) = (e[0] as usize, e[1] as usize);
        if i >= r.nodes.len() || j >= r.nodes.len() || i == j {
            continue;
        }
        let (a, b) = (r.nodes[i], r.nodes[j]);
        let d = [b[0] - a[0], b[1] - a[1]];
        let l = (d[0] * d[0] + d[1] * d[1]).sqrt();
        if l < 20.0 {
            continue;
        }
        let u = [d[0] / l, d[1] / l];
        let n = [-u[1], u[0]];
        // Half the road's width, from the painted ground where there is one.
        let half = if painted {
            let mid = [a[0] + d[0] * 0.5, a[1] + d[1] * 0.5];
            let mut h = 0.0;
            while h < 20.0
                && map.ground_at(mid[0] + n[0] * (h + 0.5), mid[1] + n[1] * (h + 0.5))
                    == Ground::Road
            {
                h += 0.5;
            }
            h + 0.5
        } else {
            3.5
        };
        let mut t = 12.0;
        let mut side = 1.0;
        while t < l - 8.0 {
            let o = (half + 0.7) * side;
            out.push(Light {
                pos: [a[0] + u[0] * t + n[0] * o, a[1] + u[1] * t + n[1] * o],
                radius: 7.5,
                color: warm,
            });
            side = -side;
            t += 16.0;
        }
    }
    out
}

/// The light buffer, its pipeline and the static lamps.
pub struct LightRenderer {
    pipeline: Pipeline,
    vb: BufferId,
    ib: BufferId,
    cap: usize,
    pub lamps: Vec<Light>,
    frame: Vec<Light>,
    verts: Vec<Vertex>,
    idx: Vec<u32>,
    count: usize,
}

impl LightRenderer {
    pub fn new(ctx: &mut dyn RenderingBackend, map: &MapDef) -> LightRenderer {
        let shader = ctx
            .new_shader(
                ShaderSource::Glsl {
                    vertex: VERTEX,
                    fragment: FRAGMENT,
                },
                ShaderMeta {
                    images: vec![],
                    uniforms: UniformBlockLayout {
                        uniforms: vec![
                            UniformDesc::new("cam", UniformType::Float4),
                            UniformDesc::new("params", UniformType::Float4),
                        ],
                    },
                },
            )
            .unwrap_or_else(|e| panic!("light shader: {e}"));
        let pipeline = ctx.new_pipeline(
            &[BufferLayout::default()],
            &[
                VertexAttribute::new("pos", VertexFormat::Float2),
                VertexAttribute::new("local", VertexFormat::Float2),
                VertexAttribute::new("color", VertexFormat::Float3),
            ],
            shader,
            PipelineParams {
                color_blend: Some(BlendState::new(
                    Equation::Add,
                    BlendFactor::One,
                    BlendFactor::One,
                )),
                ..Default::default()
            },
        );
        let cap = 1024;
        LightRenderer {
            pipeline,
            vb: ctx.new_buffer(
                BufferType::VertexBuffer,
                BufferUsage::Stream,
                BufferSource::empty::<Vertex>(cap * 4),
            ),
            ib: ctx.new_buffer(
                BufferType::IndexBuffer,
                BufferUsage::Stream,
                BufferSource::empty::<u32>(cap * 6),
            ),
            cap,
            lamps: street_lamps(map),
            frame: Vec::new(),
            verts: Vec::new(),
            idx: Vec::new(),
            count: 0,
        }
    }

    /// Collect this frame's lights inside `area` and upload them.
    pub fn update(
        &mut self,
        ctx: &mut dyn RenderingBackend,
        snap: &Snapshot,
        burning: &[Burning],
        area: macroquad::prelude::Rect,
        time: f32,
    ) {
        let inside = |p: [f32; 2], r: f32| {
            p[0] + r >= area.x
                && p[0] - r <= area.x + area.w
                && p[1] + r >= area.y
                && p[1] - r <= area.y + area.h
        };
        self.frame.clear();
        self.frame
            .extend(self.lamps.iter().filter(|l| inside(l.pos, l.radius)));
        let flicker = |x: f32| 0.85 + 0.15 * (time * 9.0 + x).sin();
        for b in burning {
            let p = [b.x, b.y];
            if inside(p, 6.0) {
                self.frame.push(Light {
                    pos: p,
                    radius: 3.0 + 3.0 * b.heat,
                    color: [0.55 * flicker(b.x), 0.28, 0.08].map(|c| c * b.heat.max(0.4)),
                });
            }
        }
        for pr in &snap.props {
            let p = [pr.x, pr.y];
            if pr.flags & 1 != 0 && inside(p, 9.0) {
                self.frame.push(Light {
                    pos: p,
                    radius: 8.0,
                    color: [0.9 * flicker(pr.x), 0.45, 0.12],
                });
            }
        }
        let blink = (time * 4.0) as u32;
        for (i, v) in snap.vehicles.iter().enumerate() {
            let p = [v.x, v.y];
            if v.flags & 3 == 0 || !inside(p, 8.0) {
                continue;
            }
            // Flashing in step with the light bar's two phases.
            let first = (blink + i as u32).is_multiple_of(2);
            self.frame.push(Light {
                pos: p,
                radius: 7.0,
                color: match (first, v.kind == 2) {
                    (true, _) => [0.2, 0.35, 0.95],
                    (false, true) => [0.8, 0.15, 0.12],
                    (false, false) => [0.15, 0.28, 0.85],
                },
            });
        }
        for p in &snap.projectiles {
            if p.kind == 1 && inside([p.x, p.y], 4.0) {
                self.frame.push(Light {
                    pos: [p.x, p.y],
                    radius: 3.5,
                    color: [0.9, 0.5, 0.15],
                });
            }
        }
        self.verts.clear();
        self.idx.clear();
        for l in &self.frame {
            let b = self.verts.len() as u32;
            for c in [[-1.0, -1.0], [1.0, -1.0], [1.0, 1.0], [-1.0, 1.0]] {
                self.verts.push(Vertex {
                    pos: [l.pos[0] + c[0] * l.radius, l.pos[1] + c[1] * l.radius],
                    local: c,
                    color: l.color,
                });
            }
            self.idx
                .extend_from_slice(&[b, b + 1, b + 2, b, b + 2, b + 3]);
        }
        let n = self.frame.len();
        if n > self.cap {
            ctx.delete_buffer(self.vb);
            ctx.delete_buffer(self.ib);
            self.cap = n.next_power_of_two();
            self.vb = ctx.new_buffer(
                BufferType::VertexBuffer,
                BufferUsage::Stream,
                BufferSource::empty::<Vertex>(self.cap * 4),
            );
            self.ib = ctx.new_buffer(
                BufferType::IndexBuffer,
                BufferUsage::Stream,
                BufferSource::empty::<u32>(self.cap * 6),
            );
        }
        if n > 0 {
            ctx.buffer_update(self.vb, BufferSource::slice(&self.verts));
            ctx.buffer_update(self.ib, BufferSource::slice(&self.idx));
        }
        self.count = n;
    }

    /// Draw the lights. Must run inside a pass on the light buffer.
    pub fn draw(&self, ctx: &mut dyn RenderingBackend, view: &View) {
        if self.count == 0 {
            return;
        }
        ctx.apply_pipeline(&self.pipeline);
        ctx.apply_bindings(&Bindings {
            vertex_buffers: vec![self.vb],
            index_buffer: self.ib,
            images: vec![],
        });
        ctx.apply_uniforms(UniformsSource::table(&Uniforms {
            cam: view.cam.uniform(),
            params: [view.px(), 0.0, 0.0, 0.0],
        }));
        ctx.draw(0, (self.count * 6) as i32, 1);
    }

    /// Lights drawn last frame.
    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}

const VERTEX: &str = r#"#version 100
precision highp float;
attribute vec2 pos;
attribute vec2 local;
attribute vec3 color;
uniform vec4 cam;
varying vec2 v_local;
varying vec2 v_world;
varying vec3 v_color;
void main() {
    v_local = local;
    v_world = pos;
    v_color = color;
    gl_Position = vec4((pos - cam.xy) * cam.zw, 0.0, 1.0);
}
"#;

const FRAGMENT: &str = r#"#version 100
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
varying vec2 v_local;
varying vec2 v_world;
varying vec3 v_color;
uniform vec4 params;
float bayer2(vec2 a) {
    a = floor(a);
    return fract(dot(a, vec2(0.5, a.y * 0.75)));
}
float bayer4(vec2 a) {
    return bayer2(0.5 * a) * 0.25 + bayer2(a);
}
void main() {
    float r2 = dot(v_local, v_local);
    if (r2 >= 1.0) discard;
    float f = (1.0 - r2) * (1.0 - r2);
    // Falloff in four steps, dithered between them on the world's pixels.
    f = floor(f * 4.0 + bayer4(floor(v_world / params.x))) / 4.0;
    gl_FragColor = vec4(v_color * f, 1.0);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lamps_line_the_district_roads() {
        let map = riot_sim::testkit::district(512.0, 64.0);
        let lamps = street_lamps(&map);
        assert!(lamps.len() > 20);
        // On the pavement, not in the road.
        let on_road = lamps
            .iter()
            .filter(|l| map.ground_at(l.pos[0], l.pos[1]) == Ground::Road)
            .count();
        assert!(on_road * 5 < lamps.len(), "{on_road} of {}", lamps.len());
    }
}
