//! The top-down 2D camera (DESIGN §18): map metres ↔ screen pixels.
//!
//! Map coordinates are metres from the map centre with +x east and +y south,
//! which is also "down" on screen, so the camera never flips an axis.

use macroquad::prelude::{Camera2D, Rect, Vec2, vec2};

/// Closest zoom out, in screen pixels per metre (a whole 8 km city fits).
pub const MIN_ZOOM: f32 = 0.05;
/// Closest zoom in, in screen pixels per metre.
pub const MAX_ZOOM: f32 = 160.0;

/// Level of detail for people (DESIGN §18.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lod {
    /// Small quads coloured by state, up to the whole city.
    Far,
    /// One sprite per person.
    Middle,
    /// Full layered paper-doll sprites.
    Close,
}

impl Lod {
    /// Zoom (px/m) at and above which people are drawn as sprites.
    pub const MIDDLE_FROM: f32 = 1.6;
    /// Zoom (px/m) at and above which people are drawn fully layered.
    pub const CLOSE_FROM: f32 = 9.0;

    /// The automatic level of detail for a zoom.
    pub fn for_zoom(zoom: f32) -> Lod {
        if zoom >= Self::CLOSE_FROM {
            Lod::Close
        } else if zoom >= Self::MIDDLE_FROM {
            Lod::Middle
        } else {
            Lod::Far
        }
    }

    /// Lower-case name for the HUD.
    pub fn name(self) -> &'static str {
        match self {
            Lod::Far => "far",
            Lod::Middle => "middle",
            Lod::Close => "close",
        }
    }
}

/// Where the camera looks and how close.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    /// Map point at the centre of the screen (m).
    pub center: Vec2,
    /// Screen pixels per metre.
    pub zoom: f32,
    /// Screen size in pixels.
    pub screen: Vec2,
}

impl Camera {
    /// A camera at `center` (m) and `zoom` (px/m), clamped to the zoom range.
    pub fn new(center: Vec2, zoom: f32, screen: Vec2) -> Camera {
        Camera {
            center,
            zoom: zoom.clamp(MIN_ZOOM, MAX_ZOOM),
            screen,
        }
    }

    /// A camera showing a whole `width × height` map.
    pub fn fit(width: f32, height: f32, screen: Vec2) -> Camera {
        let zoom = (screen.x / width.max(1.0)).min(screen.y / height.max(1.0)) * 0.95;
        Camera::new(Vec2::ZERO, zoom, screen)
    }

    #[inline]
    pub fn world_to_screen(&self, p: Vec2) -> Vec2 {
        (p - self.center) * self.zoom + self.screen * 0.5
    }

    #[inline]
    pub fn screen_to_world(&self, s: Vec2) -> Vec2 {
        (s - self.screen * 0.5) / self.zoom + self.center
    }

    /// The map area on screen, grown by `margin` metres on every side.
    pub fn view_rect(&self, margin: f32) -> Rect {
        let half = self.screen * (0.5 / self.zoom) + Vec2::splat(margin);
        Rect::new(
            self.center.x - half.x,
            self.center.y - half.y,
            half.x * 2.0,
            half.y * 2.0,
        )
    }

    /// Move the view by a screen-space drag of `d` pixels.
    pub fn pan_pixels(&mut self, d: Vec2) {
        self.center -= d / self.zoom;
    }

    /// Multiply the zoom by `factor`, keeping the map point under `anchor`
    /// (screen pixels) where it is.
    pub fn zoom_at(&mut self, factor: f32, anchor: Vec2) {
        let before = self.screen_to_world(anchor);
        self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        let after = self.screen_to_world(anchor);
        self.center += before - after;
    }

    /// Keep the centre inside a map of the given half extents.
    pub fn clamp_to(&mut self, half_w: f32, half_h: f32) {
        self.center.x = self.center.x.clamp(-half_w, half_w);
        self.center.y = self.center.y.clamp(-half_h, half_h);
    }

    /// Shader uniform: `clip = (p − c) · s` as `(cx, cy, sx, sy)`.
    pub fn uniform(&self) -> [f32; 4] {
        [
            self.center.x,
            self.center.y,
            2.0 * self.zoom / self.screen.x.max(1.0),
            -2.0 * self.zoom / self.screen.y.max(1.0),
        ]
    }

    /// The same transform for macroquad's own shape drawing.
    pub fn mq_camera(&self) -> Camera2D {
        Camera2D {
            target: self.center,
            zoom: vec2(
                2.0 * self.zoom / self.screen.x.max(1.0),
                2.0 * self.zoom / self.screen.y.max(1.0),
            ),
            ..Default::default()
        }
    }

    /// Level of detail chosen from the zoom.
    pub fn lod(&self) -> Lod {
        Lod::for_zoom(self.zoom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_zoom_anchor() {
        let mut c = Camera::new(vec2(10.0, -5.0), 4.0, vec2(1280.0, 720.0));
        let p = vec2(37.0, 12.5);
        let s = c.world_to_screen(p);
        assert!((c.screen_to_world(s) - p).length() < 1e-3);
        let anchor = vec2(100.0, 600.0);
        let before = c.screen_to_world(anchor);
        c.zoom_at(2.5, anchor);
        assert!((c.screen_to_world(anchor) - before).length() < 1e-3);
        assert_eq!(c.lod(), Lod::Close);
    }

    #[test]
    fn view_rect_covers_screen() {
        let c = Camera::new(Vec2::ZERO, 2.0, vec2(800.0, 600.0));
        let r = c.view_rect(0.0);
        assert!((r.w - 400.0).abs() < 1e-3 && (r.h - 300.0).abs() < 1e-3);
        assert!((r.x + 200.0).abs() < 1e-3);
    }
}
