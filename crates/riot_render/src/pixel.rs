//! The pixel buffer: the world is drawn into an offscreen buffer whose
//! pixels are the art's pixels, then scaled up to the screen.
//!
//! For a zoom of `z` screen pixels per metre the buffer is drawn at the
//! densest art level `d ≤ z` (48, 24, 12 or 6 px/m, see [`crate::art`]), with
//! the camera centre snapped to whole buffer pixels, so every sheet is
//! sampled texel for pixel and nothing shimmers while panning. The composite
//! scales the buffer by `z / d` (between 1 and 2 at most zooms) with a
//! sharp-bilinear filter: pixels stay square and crisp, and only the one
//! screen pixel where two of them meet is blended, so non-integer scales and
//! sub-pixel scrolling stay smooth. Zoomed out below the coarsest level the
//! buffer matches the screen one to one ("direct").
//!
//! The buffer has a depth attachment, used only by shadows: every shadow is
//! drawn at the same depth with a less-than test, so overlapping shadows
//! never darken twice. A second buffer of the same size holds the light of
//! lamps and fires at night ([`crate::lights`]); the composite lights the
//! scene with it and the time of day's colour grade.

use macroquad::miniquad::*;
use macroquad::prelude::{Camera2D, FilterMode, Rect, RenderTarget, RenderTargetParams, vec2};

use crate::art;
use crate::camera::Camera;

/// How the world is drawn this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    /// The camera into the buffer: centre (snapped in pixel mode), zoom in
    /// buffer pixels per metre, buffer size.
    pub cam: Camera,
    /// Art level whose sheets match the buffer.
    pub level: usize,
    /// Drawn at an art density (pixel art), not straight at the zoom.
    pub pixel: bool,
    /// Screen pixels per buffer pixel.
    pub scale: f32,
    /// The map area the screen shows (the buffer may cover more).
    pub visible: Rect,
}

impl View {
    /// The view for a screen camera. `chunk` ≥ 1 makes pixels that many
    /// times bigger; `pixel_art` off draws straight at the zoom.
    pub fn new(cam: &Camera, pixel_art: bool, chunk: f32) -> View {
        let (w, h) = buffer_size(cam.screen.x, cam.screen.y);
        let want = cam.zoom / chunk.max(1.0);
        let level = (0..art::LEVELS).find(|&k| art::density(k) <= want + 1e-3);
        match level {
            Some(level) if pixel_art => {
                let d = art::density(level);
                let center = vec2(
                    (cam.center.x * d).round() / d,
                    (cam.center.y * d).round() / d,
                );
                View {
                    cam: Camera {
                        center,
                        zoom: d,
                        screen: vec2(w as f32, h as f32),
                    },
                    level,
                    pixel: true,
                    scale: cam.zoom / d,
                    visible: cam.view_rect(0.0),
                }
            }
            _ => View {
                cam: Camera {
                    center: cam.center,
                    zoom: cam.zoom,
                    screen: vec2(w as f32, h as f32),
                },
                level: level.unwrap_or(art::LEVELS - 1),
                pixel: false,
                scale: 1.0,
                visible: cam.view_rect(0.0),
            },
        }
    }

    /// Metres per buffer pixel.
    pub fn px(&self) -> f32 {
        1.0 / self.cam.zoom
    }

    /// macroquad's camera for drawing shapes into `target`.
    pub fn mq_camera(&self, target: &RenderTarget) -> Camera2D {
        Camera2D {
            target: self.cam.center,
            // Render targets are not flipped by macroquad; ours have +y down
            // like the screen, so flip here.
            zoom: vec2(
                2.0 * self.cam.zoom / self.cam.screen.x,
                -2.0 * self.cam.zoom / self.cam.screen.y,
            ),
            render_target: Some(target.clone()),
            ..Default::default()
        }
    }
}

/// The buffer covers the whole screen at scale 1 (even sizes, so the
/// snapped centre falls on a pixel corner).
fn buffer_size(sw: f32, sh: f32) -> (u32, u32) {
    let even = |v: f32| ((v.max(2.0).ceil() as u32) + 1) & !1;
    (even(sw), even(sh))
}

#[repr(C)]
struct Uniforms {
    /// Buffer pixel at the screen's top-left and bottom-right corners.
    src: [f32; 4],
    /// Buffer width, height, screen pixels per buffer pixel, strength of
    /// the light buffer.
    size: [f32; 4],
    /// Colour grade: multiply (rgb) and saturation (a).
    grade: [f32; 4],
}

/// How much the light buffer adds for a colour grade: nothing by day, most
/// of it at night.
pub fn light_strength(grade: [f32; 4]) -> f32 {
    let lum = grade[0] * 0.299 + grade[1] * 0.587 + grade[2] * 0.114;
    ((1.0 - lum) * 2.6).clamp(0.0, 1.0)
}

/// The offscreen buffer and the composite pass.
pub struct PixelBuffer {
    pub target: Option<RenderTarget>,
    /// Light at night ([`crate::lights`]), the same size, no depth.
    pub light: Option<RenderTarget>,
    size: (u32, u32),
    pipeline: Pipeline,
    quad_vb: BufferId,
    quad_ib: BufferId,
}

impl PixelBuffer {
    pub fn new(ctx: &mut dyn RenderingBackend) -> PixelBuffer {
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
                    images: vec!["buf".to_string(), "light".to_string()],
                    uniforms: UniformBlockLayout {
                        uniforms: vec![
                            UniformDesc::new("src", UniformType::Float4),
                            UniformDesc::new("size", UniformType::Float4),
                            UniformDesc::new("grade", UniformType::Float4),
                        ],
                    },
                },
            )
            .unwrap_or_else(|e| panic!("composite shader: {e}"));
        let pipeline = ctx.new_pipeline(
            &[BufferLayout::default()],
            &[VertexAttribute::new("corner", VertexFormat::Float2)],
            shader,
            PipelineParams::default(),
        );
        PixelBuffer {
            target: None,
            light: None,
            size: (0, 0),
            pipeline,
            quad_vb,
            quad_ib,
        }
    }

    /// Make sure the buffer fits `view` (recreated when the screen size
    /// changes).
    pub fn ensure(&mut self, view: &View) -> &RenderTarget {
        let size = (view.cam.screen.x as u32, view.cam.screen.y as u32);
        if self.target.is_none() || self.size != size {
            let rt = macroquad::prelude::render_target_ex(
                size.0,
                size.1,
                RenderTargetParams {
                    sample_count: 1,
                    depth: true,
                },
            );
            rt.texture.set_filter(FilterMode::Linear);
            self.target = Some(rt);
            let light = macroquad::prelude::render_target_ex(
                size.0,
                size.1,
                RenderTargetParams {
                    sample_count: 1,
                    depth: false,
                },
            );
            light.texture.set_filter(FilterMode::Linear);
            self.light = Some(light);
            self.size = size;
        }
        self.target.as_ref().expect("created above")
    }

    /// Draw the buffer onto the default framebuffer for the screen camera
    /// `screen`. Must run inside the default pass.
    pub fn composite(
        &self,
        ctx: &mut dyn RenderingBackend,
        view: &View,
        screen: &Camera,
        grade: [f32; 4],
    ) {
        let (Some(rt), Some(light)) = (&self.target, &self.light) else {
            return;
        };
        // Buffer pixels (y down) of the screen's corners.
        let to_buf = |s: macroquad::prelude::Vec2| {
            let w = screen.screen_to_world(s);
            (w - view.cam.center) * view.cam.zoom + view.cam.screen * 0.5
        };
        let a = to_buf(vec2(0.0, 0.0));
        let b = to_buf(screen.screen);
        let bindings = Bindings {
            vertex_buffers: vec![self.quad_vb],
            index_buffer: self.quad_ib,
            images: vec![
                rt.texture.raw_miniquad_id(),
                light.texture.raw_miniquad_id(),
            ],
        };
        ctx.apply_pipeline(&self.pipeline);
        ctx.apply_bindings(&bindings);
        ctx.apply_uniforms(UniformsSource::table(&Uniforms {
            src: [a.x, a.y, b.x, b.y],
            size: [
                view.cam.screen.x,
                view.cam.screen.y,
                view.scale,
                light_strength(grade),
            ],
            grade,
        }));
        ctx.draw(0, 6, 1);
    }
}

const VERTEX: &str = r#"#version 100
precision highp float;
attribute vec2 corner;
uniform vec4 src;
varying vec2 v_px;
void main() {
    v_px = mix(src.xy, src.zw, corner);
    gl_Position = vec4(corner.x * 2.0 - 1.0, 1.0 - corner.y * 2.0, 0.0, 1.0);
}
"#;

const FRAGMENT: &str = r#"#version 100
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
varying vec2 v_px;
uniform sampler2D buf;
uniform sampler2D light;
uniform vec4 size;
uniform vec4 grade;
void main() {
    // Sharp bilinear: sample texel centres, blending only across the last
    // screen pixel before each texel edge.
    float scale = max(size.z, 1.0);
    vec2 fl = floor(v_px);
    vec2 s = v_px - fl - 0.5;
    float r = 0.5 - 0.5 / scale;
    vec2 f = (s - clamp(s, -r, r)) * scale + 0.5;
    vec2 t = fl + f;
    vec2 uv = vec2(t.x / size.x, 1.0 - t.y / size.y);
    vec3 c = texture2D(buf, uv).rgb;
    float l = dot(c, vec3(0.299, 0.587, 0.114));
    c = mix(vec3(l), c, grade.a);
    // The day's light, plus lamps and fires at night: they light the
    // scene in its own colours and glow a little themselves.
    vec3 lit = texture2D(light, uv).rgb * size.w;
    c = c * (grade.rgb + lit) + lit * 0.1;
    gl_FragColor = vec4(c, 1.0);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_picks_the_densest_level_under_the_zoom() {
        let screen = vec2(1280.0, 720.0);
        let v = View::new(&Camera::new(vec2(3.3, -7.1), 30.0, screen), true, 1.0);
        assert!(v.pixel);
        assert_eq!(v.level, 1);
        assert_eq!(v.cam.zoom, 24.0);
        assert!((v.scale - 1.25).abs() < 1e-5);
        // The buffer's top-left corner is on a pixel boundary.
        let x0 = (v.cam.center.x - v.cam.screen.x * 0.5 / v.cam.zoom) * v.cam.zoom;
        assert!((x0 - x0.round()).abs() < 1e-3);
        let v = View::new(&Camera::new(vec2(0.0, 0.0), 200.0, screen), true, 1.0);
        assert_eq!(v.level, 0);
        let v = View::new(&Camera::new(vec2(0.0, 0.0), 2.0, screen), true, 1.0);
        assert!(!v.pixel);
        assert_eq!(v.cam.zoom, 2.0);
        let v = View::new(&Camera::new(vec2(0.0, 0.0), 30.0, screen), true, 2.0);
        assert_eq!(v.level, 2);
    }
}
