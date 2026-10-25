//! Textured triangle meshes on the GPU: the pipelines of the three passes,
//! every material and sheet at every art level, static meshes culled by runs
//! of triangles, and stream meshes rebuilt every frame. The meshes are built
//! on the CPU by [`riot_mesh::mesh`], whose items are re-exported here.

use macroquad::miniquad::*;
pub use riot_mesh::mesh::{
    BatchDesc, MeshBuilder, Part, Pass, Rgba, Run, Tex, Vertex, WHITE, centroid, cross, inset,
    is_convex, point_in_polygon, rgba, runs, signed_area2, triangulate,
};

use crate::art::{self, Img};
use crate::materials::{self, Mat};
use crate::pixel::View;

struct Batch {
    pass: Pass,
    tex: Tex,
    runs: Vec<Run>,
}

/// An uploaded static mesh.
pub struct Mesh {
    vb: BufferId,
    ib: BufferId,
    batches: Vec<Batch>,
}

/// A mesh rebuilt every frame (props and vehicles).
pub struct StreamMesh {
    vb: BufferId,
    ib: BufferId,
    vcap: usize,
    icap: usize,
    batches: Vec<BatchDesc>,
}

#[repr(C)]
struct Uniforms {
    cam: [f32; 4],
    /// metres per buffer pixel, uv scroll x, uv scroll y, unused
    params: [f32; 4],
}

/// Pipelines and every texture of the textured meshes.
pub struct MeshRenderer {
    normal: Pipeline,
    shadow: Pipeline,
    dither: Pipeline,
    /// Per material, per level.
    mats: Vec<Vec<TextureId>>,
    decals: Vec<TextureId>,
    things: Vec<TextureId>,
}

/// Upload one level of a texture. Only the coarsest level, which is also
/// drawn minified when zoomed out past it, keeps mipmaps.
fn texture(ctx: &mut dyn RenderingBackend, img: &Img, level: usize, repeat: bool) -> TextureId {
    let last = level == art::LEVELS - 1;
    let t = ctx.new_texture(
        TextureAccess::Static,
        TextureSource::Bytes(&img.bytes()),
        TextureParams {
            width: img.w as u32,
            height: img.h as u32,
            format: TextureFormat::RGBA8,
            wrap: if repeat {
                TextureWrap::Repeat
            } else {
                TextureWrap::Clamp
            },
            min_filter: if last {
                FilterMode::Linear
            } else {
                FilterMode::Nearest
            },
            mag_filter: FilterMode::Nearest,
            mipmap_filter: if last {
                MipmapFilterMode::Linear
            } else {
                MipmapFilterMode::None
            },
            allocate_mipmaps: last,
            ..Default::default()
        },
    );
    if last {
        ctx.texture_generate_mipmaps(t);
    }
    t
}

impl MeshRenderer {
    /// Generate every material and sheet at every level and upload them.
    pub fn new(ctx: &mut dyn RenderingBackend, decals: &[Img], things: &[Img]) -> MeshRenderer {
        let shader = ctx
            .new_shader(
                ShaderSource::Glsl {
                    vertex: VERTEX,
                    fragment: FRAGMENT,
                },
                ShaderMeta {
                    images: vec!["tex".to_string()],
                    uniforms: UniformBlockLayout {
                        uniforms: vec![
                            UniformDesc::new("cam", UniformType::Float4),
                            UniformDesc::new("params", UniformType::Float4),
                        ],
                    },
                },
            )
            .unwrap_or_else(|e| panic!("mesh shader: {e}"));
        let dither_shader = ctx
            .new_shader(
                ShaderSource::Glsl {
                    vertex: VERTEX,
                    fragment: DITHER_FRAGMENT,
                },
                ShaderMeta {
                    images: vec!["tex".to_string()],
                    uniforms: UniformBlockLayout {
                        uniforms: vec![
                            UniformDesc::new("cam", UniformType::Float4),
                            UniformDesc::new("params", UniformType::Float4),
                        ],
                    },
                },
            )
            .unwrap_or_else(|e| panic!("dither shader: {e}"));
        let layout = [BufferLayout::default()];
        let attrs = [
            VertexAttribute::new("pos", VertexFormat::Float2),
            VertexAttribute::new("uv", VertexFormat::Float2),
            VertexAttribute::new("ext", VertexFormat::Float2),
            VertexAttribute::new("color", VertexFormat::Byte4),
        ];
        let blend = Some(BlendState::new(
            Equation::Add,
            BlendFactor::Value(BlendValue::SourceAlpha),
            BlendFactor::OneMinusValue(BlendValue::SourceAlpha),
        ));
        let normal = ctx.new_pipeline(
            &layout,
            &attrs,
            shader,
            PipelineParams {
                color_blend: blend,
                ..Default::default()
            },
        );
        let shadow = ctx.new_pipeline(
            &layout,
            &attrs,
            shader,
            PipelineParams {
                color_blend: blend,
                depth_test: Comparison::Less,
                depth_write: true,
                ..Default::default()
            },
        );
        let dither = ctx.new_pipeline(
            &layout,
            &attrs,
            dither_shader,
            PipelineParams {
                color_blend: blend,
                ..Default::default()
            },
        );
        // Materials are generated in parallel: a few hundred ms otherwise.
        let imgs: Vec<Vec<Img>> = std::thread::scope(|s| {
            let jobs: Vec<_> = Mat::ALL
                .iter()
                .map(|&m| {
                    s.spawn(move || {
                        (0..art::LEVELS)
                            .map(|l| materials::generate(m, l))
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            jobs.into_iter()
                .map(|j| j.join().expect("material generator"))
                .collect()
        });
        let mats = imgs
            .iter()
            .map(|levels| {
                levels
                    .iter()
                    .enumerate()
                    .map(|(l, img)| texture(ctx, img, l, true))
                    .collect()
            })
            .collect();
        let decals = decals
            .iter()
            .enumerate()
            .map(|(l, img)| texture(ctx, img, l, false))
            .collect();
        let things = things
            .iter()
            .enumerate()
            .map(|(l, img)| texture(ctx, img, l, false))
            .collect();
        MeshRenderer {
            normal,
            shadow,
            dither,
            mats,
            decals,
            things,
        }
    }

    fn texture_for(&self, tex: Tex, level: usize) -> TextureId {
        match tex {
            Tex::Mat(m) => self.mats[m.index()][level],
            Tex::Decals => self.decals[level],
            Tex::Things => self.things[level],
        }
    }

    /// Upload a static mesh.
    pub fn upload(&self, ctx: &mut dyn RenderingBackend, m: MeshBuilder) -> Option<Mesh> {
        let (verts, idx, descs) = m.finish();
        if idx.is_empty() {
            return None;
        }
        let vb = ctx.new_buffer(
            BufferType::VertexBuffer,
            BufferUsage::Immutable,
            BufferSource::slice(&verts),
        );
        let ib = ctx.new_buffer(
            BufferType::IndexBuffer,
            BufferUsage::Immutable,
            BufferSource::slice(&idx),
        );
        let batches = descs
            .iter()
            .map(|d| Batch {
                pass: d.pass,
                tex: d.tex,
                runs: runs(&verts, &idx, d.start, d.count),
            })
            .collect();
        Some(Mesh { vb, ib, batches })
    }

    fn pipeline(&self, pass: Pass) -> &Pipeline {
        match pass {
            Pass::Normal => &self.normal,
            Pass::Shadow => &self.shadow,
            Pass::Dither => &self.dither,
        }
    }

    fn uniforms(&self, view: &View, tex: Tex, time: f32) -> Uniforms {
        let scroll = if tex == Tex::Mat(Mat::Water) {
            [time * 0.011, time * 0.004]
        } else {
            [0.0, 0.0]
        };
        Uniforms {
            cam: view.cam.uniform(),
            params: [view.px(), scroll[0], scroll[1], 0.0],
        }
    }

    /// Draw a static mesh, culled to the view. Returns triangles drawn.
    /// Must run inside a render pass.
    pub fn draw(
        &self,
        ctx: &mut dyn RenderingBackend,
        mesh: &Mesh,
        view: &View,
        time: f32,
    ) -> usize {
        let v = view.visible;
        let mut tris = 0;
        for b in &mesh.batches {
            let visible: Vec<&Run> = b
                .runs
                .iter()
                .filter(|r| {
                    r.max[0] >= v.x
                        && r.min[0] <= v.x + v.w
                        && r.max[1] >= v.y
                        && r.min[1] <= v.y + v.h
                })
                .collect();
            if visible.is_empty() {
                continue;
            }
            ctx.apply_pipeline(self.pipeline(b.pass));
            ctx.apply_bindings(&Bindings {
                vertex_buffers: vec![mesh.vb],
                index_buffer: mesh.ib,
                images: vec![self.texture_for(b.tex, view.level)],
            });
            ctx.apply_uniforms(UniformsSource::table(&self.uniforms(view, b.tex, time)));
            for r in visible {
                ctx.draw(r.start, r.count, 1);
                tris += r.count as usize / 3;
            }
        }
        tris
    }

    pub fn new_stream(&self, ctx: &mut dyn RenderingBackend) -> StreamMesh {
        let (vcap, icap) = (4096, 6144);
        StreamMesh {
            vb: ctx.new_buffer(
                BufferType::VertexBuffer,
                BufferUsage::Stream,
                BufferSource::empty::<Vertex>(vcap),
            ),
            ib: ctx.new_buffer(
                BufferType::IndexBuffer,
                BufferUsage::Stream,
                BufferSource::empty::<u32>(icap),
            ),
            vcap,
            icap,
            batches: Vec::new(),
        }
    }

    /// Replace a stream mesh's contents.
    pub fn update_stream(
        &self,
        ctx: &mut dyn RenderingBackend,
        s: &mut StreamMesh,
        m: MeshBuilder,
    ) {
        let (verts, idx, batches) = m.finish();
        if verts.len() > s.vcap || idx.len() > s.icap {
            ctx.delete_buffer(s.vb);
            ctx.delete_buffer(s.ib);
            s.vcap = verts.len().next_power_of_two();
            s.icap = idx.len().next_power_of_two();
            s.vb = ctx.new_buffer(
                BufferType::VertexBuffer,
                BufferUsage::Stream,
                BufferSource::empty::<Vertex>(s.vcap),
            );
            s.ib = ctx.new_buffer(
                BufferType::IndexBuffer,
                BufferUsage::Stream,
                BufferSource::empty::<u32>(s.icap),
            );
        }
        if !idx.is_empty() {
            ctx.buffer_update(s.vb, BufferSource::slice(&verts));
            ctx.buffer_update(s.ib, BufferSource::slice(&idx));
        }
        s.batches = batches;
    }

    /// Draw a stream mesh. Must run inside a render pass.
    pub fn draw_stream(
        &self,
        ctx: &mut dyn RenderingBackend,
        s: &StreamMesh,
        view: &View,
        time: f32,
    ) {
        for b in &s.batches {
            ctx.apply_pipeline(self.pipeline(b.pass));
            ctx.apply_bindings(&Bindings {
                vertex_buffers: vec![s.vb],
                index_buffer: s.ib,
                images: vec![self.texture_for(b.tex, view.level)],
            });
            ctx.apply_uniforms(UniformsSource::table(&self.uniforms(view, b.tex, time)));
            ctx.draw(b.start as i32, b.count as i32, 1);
        }
    }
}

const VERTEX: &str = r#"#version 100
precision highp float;
attribute vec2 pos;
attribute vec2 uv;
attribute vec2 ext;
attribute vec4 color;
uniform vec4 cam;
uniform vec4 params;
varying vec2 v_uv;
varying vec2 v_world;
varying lowp vec4 v_color;
void main() {
    vec2 p = pos + ext * params.x;
    v_world = p;
    v_uv = uv + params.yz;
    v_color = color / 255.0;
    gl_Position = vec4((p - cam.xy) * cam.zw, 0.0, 1.0);
}
"#;

const FRAGMENT: &str = r#"#version 100
precision mediump float;
varying vec2 v_uv;
varying vec2 v_world;
varying lowp vec4 v_color;
uniform sampler2D tex;
void main() {
    vec4 c = texture2D(tex, v_uv) * v_color;
    // Shadows write depth: fully transparent texels must not.
    if (c.a < 0.004) discard;
    gl_FragColor = c;
}
"#;

const DITHER_FRAGMENT: &str = r#"#version 100
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
varying vec2 v_uv;
varying vec2 v_world;
varying lowp vec4 v_color;
uniform sampler2D tex;
uniform vec4 params;
// Ordered dithering on a 4×4 Bayer matrix, anchored to the world's pixels.
float bayer2(vec2 a) {
    a = floor(a);
    return fract(dot(a, vec2(0.5, a.y * 0.75)));
}
float bayer4(vec2 a) {
    return bayer2(0.5 * a) * 0.25 + bayer2(a);
}
void main() {
    vec4 c = texture2D(tex, v_uv) * v_color;
    if (c.a <= bayer4(v_world / params.x) + 0.03) discard;
    gl_FragColor = vec4(c.rgb, 1.0);
}
"#;
