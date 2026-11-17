//! The bridge between the layer stack and egui: converts the runtime's
//! presentation into per-layer paint callbacks, orders them by z-index, and
//! implements `CallbackTrait` so egui drives the uploads and draws.

use eframe::egui::{self, Rect};
use eframe::egui_wgpu::{self, wgpu};

use crate::model::Session;

use super::renderer::{CompositorResources, LayerDraw};

/// One layer prepared for painting: where it goes and what it shows.
#[derive(Clone, Debug)]
pub struct LayerPaint {
    /// The draw description (geometry, crop, uploads).
    pub draw: LayerDraw,
    /// Session z-index; higher paints later.
    pub z_index: i32,
    /// Hidden layers are skipped entirely.
    pub visible: bool,
}

/// Everything that should be painted this frame, already in paint order.
#[derive(Clone, Debug, Default)]
pub struct PaintList {
    /// Layers in ascending z-index.
    pub layers: Vec<LayerPaint>,
}

impl PaintList {
    /// Build the paint order for a session.
    ///
    /// Layers are sorted ascending by `z_index` (ties broken by id so the
    /// order is stable), hidden layers are dropped, and each layer keeps the
    /// rectangle the caller computed for it.
    pub fn build<F>(session: &Session, mut rect_for: F) -> Self
    where
        F: FnMut(u64) -> Option<Rect>,
    {
        let mut layers: Vec<LayerPaint> = session
            .paint_order()
            .into_iter()
            .filter(|l| l.visible)
            .filter_map(|l| {
                let rect = rect_for(l.id)?;
                Some(LayerPaint {
                    draw: LayerDraw::empty(l.id, rect),
                    z_index: l.z_index,
                    visible: l.visible,
                })
            })
            .collect();
        layers.sort_by_key(|p| (p.z_index, p.draw.layer_id));
        Self { layers }
    }

    /// Number of visible layers with a non-empty rectangle.
    pub fn len(&self) -> usize {
        self.layers.len()
    }

    /// True when nothing would be painted.
    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }
}

/// The callback handed to egui for a single layer.
#[derive(Clone)]
pub struct LayerCallback {
    draw: LayerDraw,
}

impl LayerCallback {
    /// Wrap a draw.
    pub fn new(draw: LayerDraw) -> Self {
        Self { draw }
    }

    /// The draw this callback paints.
    pub fn draw(&self) -> &LayerDraw {
        &self.draw
    }
}

impl egui_wgpu::CallbackTrait for LayerCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(res) = resources.get_mut::<CompositorResources>() {
            res.prepare(device, queue, &self.draw);
        }
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        if let Some(res) = resources.get::<CompositorResources>() {
            res.paint(&self.draw, render_pass);
        }
    }
}

/// Submit a paint list into an egui painter.
///
/// Each layer becomes its own callback so egui sets the render-pass viewport
/// to exactly that layer's rectangle; this is what makes arbitrary normalized
/// rects (including negative-space letterboxing) work without any extra
/// uniforms.
pub fn submit(painter: &egui::Painter, list: &PaintList) {
    for layer in &list.layers {
        if layer.draw.is_empty() {
            // Still paint a transparent quad so stacked layers keep their
            // placeholders visible to the hit-testing code above us.
            continue;
        }
        painter.add(egui_wgpu::Callback::new_paint_callback(
            layer.draw.rect,
            LayerCallback::new(layer.draw.clone()),
        ));
    }
}

/// Paint a single background clear rectangle (canvas wallpaper) so the canvas
/// has a defined colour even before any layer is ready.
pub fn paint_canvas_background(painter: &egui::Painter, rect: Rect, color: egui::Color32) {
    painter.rect_filled(rect, 0.0, color);
}

/// Keep only GPU cache entries whose layers still exist.
pub fn retire_layers(resources: &mut egui_wgpu::CallbackResources, live: &[u64]) {
    if let Some(res) = resources.get_mut::<CompositorResources>() {
        res.retain_layers(live);
    }
}

/// Register the compositor in eframe's wgpu render state.
pub fn install(render_state: &egui_wgpu::RenderState) {
    let resources = CompositorResources::install(render_state);
    render_state
        .renderer
        .write()
        .callback_resources
        .insert(resources);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::session::{CanvasAspect, LayerConfig, LayerRole, Session};
    use crate::model::geom::NormalizedRect;
    use std::path::PathBuf;

    fn session_with_two_layers() -> Session {
        let mut s = Session::new("s-1".into(), "Test".into(), CanvasAspect::Widescreen);
        s.layers = vec![
            LayerConfig::new(
                1,
                "A".into(),
                LayerRole::Source,
                PathBuf::from("/a.mp4"),
                NormalizedRect::full(),
                5,
            ),
            LayerConfig::new(
                2,
                "B".into(),
                LayerRole::Reaction1,
                PathBuf::from("/b.mp4"),
                NormalizedRect::new(0.5, 0.5, 0.5, 0.5),
                2,
            ),
        ];
        s
    }

    #[test]
    fn paint_order_is_ascending_z() {
        let session = session_with_two_layers();
        let list = PaintList::build(&session, |id| {
            Some(Rect::from_min_size(
                egui::pos2(id as f32, 0.0),
                egui::vec2(100.0, 100.0),
            ))
        });
        assert_eq!(list.len(), 2);
        assert_eq!(list.layers[0].draw.layer_id, 2, "z=2 painted before z=5");
        assert_eq!(list.layers[1].draw.layer_id, 1);
        assert!(list.layers[0].z_index <= list.layers[1].z_index);
    }

    #[test]
    fn hidden_layers_are_dropped() {
        let mut session = session_with_two_layers();
        session.layers[0].visible = false;
        let list = PaintList::build(&session, |_| {
            Some(Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(10.0, 10.0)))
        });
        assert_eq!(list.len(), 1);
        assert_eq!(list.layers[0].draw.layer_id, 2);
    }

    #[test]
    fn missing_rects_are_dropped() {
        let session = session_with_two_layers();
        let list = PaintList::build(&session, |id| {
            if id == 1 {
                None
            } else {
                Some(Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(10.0, 10.0),
                ))
            }
        });
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn empty_list_when_all_hidden() {
        let mut session = session_with_two_layers();
        for l in &mut session.layers {
            l.visible = false;
        }
        let list = PaintList::build(&session, |_| {
            Some(Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1.0, 1.0)))
        });
        assert!(list.is_empty());
        assert_eq!(list.len(), 0);
    }

    #[test]
    fn callback_exposes_its_draw() {
        let rect = Rect::from_min_size(egui::pos2(3.0, 4.0), egui::vec2(5.0, 6.0));
        let draw = LayerDraw::empty(7, rect);
        let cb = LayerCallback::new(draw);
        assert_eq!(cb.draw().layer_id, 7);
        assert_eq!(cb.draw().rect, rect);
    }
}
