//! The static map meshes on the GPU (DESIGN §18.2 layers 1 and 7): the
//! ground and roofs [`riot_mesh::map`] builds from the [`MapDef`] (its items
//! are re-exported here), detailed and simplified, uploaded once and drawn
//! culled to the view.

use macroquad::miniquad::RenderingBackend;
pub use riot_mesh::map::{DETAIL_ZOOM, OUTSIDE, build_ground, build_roofs};
use riot_sim::mapdef::MapDef;

use crate::mesh::{self, MeshRenderer};
use crate::pixel::View;
use crate::sheets::DecalAtlas;

/// The static map meshes, detailed and simplified.
pub struct MapRenderer {
    ground: Option<mesh::Mesh>,
    roofs: Option<mesh::Mesh>,
    ground_far: Option<mesh::Mesh>,
    roofs_far: Option<mesh::Mesh>,
    /// Map half extents, for the outside-the-map frame.
    pub half: [f32; 2],
    /// Triangles drawn last frame.
    pub triangles: usize,
}

impl MapRenderer {
    /// Build and upload the ground and roof meshes.
    pub fn new(
        ctx: &mut dyn RenderingBackend,
        meshes: &MeshRenderer,
        map: &MapDef,
        decals: &DecalAtlas,
    ) -> MapRenderer {
        MapRenderer {
            ground: meshes.upload(ctx, build_ground(map, true, decals)),
            roofs: meshes.upload(ctx, build_roofs(map, true, decals)),
            ground_far: meshes.upload(ctx, build_ground(map, false, decals)),
            roofs_far: meshes.upload(ctx, build_roofs(map, false, decals)),
            half: [map.width * 0.5, map.height * 0.5],
            triangles: 0,
        }
    }

    /// Layer 1: the ground. Must run inside a render pass.
    pub fn draw_ground(
        &mut self,
        ctx: &mut dyn RenderingBackend,
        meshes: &MeshRenderer,
        view: &View,
        time: f32,
    ) {
        let far = view.cam.zoom < DETAIL_ZOOM;
        let mesh = if far { &self.ground_far } else { &self.ground };
        self.triangles = mesh.as_ref().map_or(0, |m| meshes.draw(ctx, m, view, time));
    }

    /// Layer 7: roofs and canopies. Must run inside a render pass.
    pub fn draw_roofs(
        &mut self,
        ctx: &mut dyn RenderingBackend,
        meshes: &MeshRenderer,
        view: &View,
        time: f32,
    ) {
        let far = view.cam.zoom < DETAIL_ZOOM;
        let mesh = if far { &self.roofs_far } else { &self.roofs };
        self.triangles += mesh.as_ref().map_or(0, |m| meshes.draw(ctx, m, view, time));
    }
}
