//! The Alignment Studio: capture a frame from each video, compare them with an
//! onion skin or a wipe, and read the exact offset delta between the two
//! timestamps.
//!
//! The studio stores the decoded YUV planes (not RGBA) so the comparisons are
//! uploaded to the GPU at full quality. It keeps the last
//! [`MAX_CAPTURES`](crate::gpu::capture::MAX_CAPTURES) captures *per video*, and
//! the delta it proposes is `offset_B = offset_A + (pts_B - pts_A)`, which the
//! unit tests check against the engine's `target_pts_us` formula.

use std::collections::HashMap;

use eframe::egui::{self, Align, Layout, RichText, Ui};
use eframe::egui_wgpu;

use crate::app::App;
use crate::engine::sync_clock::{alignment_delta, alignment_offset};
use crate::gpu::capture::{Capture, CaptureRing, Comparison, StudioMode, StudioSide};
use crate::gpu::renderer::{CompareUpload, FrameUpload};
use crate::ui::theme;
use crate::ui::widgets;

/// Studio UI state.
#[derive(Debug)]
pub struct StudioState {
    /// Whether the studio panel is open.
    pub open: bool,
    /// Per-layer capture history.
    pub rings: HashMap<u64, CaptureRing>,
    /// Layer A (the reference).
    pub a: Option<u64>,
    /// Layer B (the one being aligned).
    pub b: Option<u64>,
    /// Onion or wipe.
    pub mode: StudioMode,
    /// Mix / split position, `0..1`.
    pub wipe: f32,
}

impl Default for StudioState {
    fn default() -> Self {
        Self {
            open: false,
            rings: HashMap::new(),
            a: None,
            b: None,
            mode: StudioMode::Onion,
            wipe: 0.5,
        }
    }
}

impl StudioState {
    /// Store a capture for `layer_id`, keeping the last eight.
    pub fn push_for(&mut self, layer_id: u64, capture: Capture) {
        self.rings
            .entry(layer_id)
            .or_insert_with(CaptureRing::new)
            .push(capture);
        if self.a.is_none() {
            self.a = Some(layer_id);
        } else if self.b.is_none() && self.a != Some(layer_id) {
            self.b = Some(layer_id);
        }
    }

    /// Switch Onion <-> Wipe.
    pub fn cycle_mode(&mut self) {
        self.mode = match self.mode {
            StudioMode::Onion => StudioMode::Wipe,
            StudioMode::Wipe => StudioMode::Onion,
        };
    }

    /// The newest capture of a layer.
    pub fn newest(&self, layer_id: u64) -> Option<&Capture> {
        self.rings.get(&layer_id).and_then(|r| r.newest())
    }

    /// A short "pts ms" label for the newest A capture.
    pub fn short_a(&self) -> String {
        match self.a.and_then(|id| self.newest(id)) {
            Some(c) => format!("{} ms", c.pts_ms()),
            None => "—".to_string(),
        }
    }

    /// A short "pts ms" label for the newest B capture.
    pub fn short_b(&self) -> String {
        match self.b.and_then(|id| self.newest(id)) {
            Some(c) => format!("{} ms", c.pts_ms()),
            None => "—".to_string(),
        }
    }

    /// The delta between the two newest captures, in microseconds.
    pub fn delta_us(&self) -> Option<i64> {
        let a = self.newest(self.a?)?;
        let b = self.newest(self.b?)?;
        Some(alignment_delta(a.pts_us, b.pts_us))
    }

    /// Build the GPU comparison for the current selection: layer A's id plus
    /// the upload for layer B.
    pub fn comparison_for(&self, app: &App) -> Option<(u64, CompareUpload)> {
        let a_id = self.a?;
        let b_id = self.b?;
        self.newest(a_id)?;
        let b_cap = self.newest(b_id)?;
        let b_layer = app.session.layer(b_id)?;
        Some((
            a_id,
            CompareUpload {
                key: b_id,
                frame: FrameUpload {
                    frame: std::sync::Arc::clone(&b_cap.frame),
                    range: b_cap.range,
                },
                crop: b_layer.crop,
                t: self.wipe,
                kind: self.mode.compare_kind(),
            },
        ))
    }

    /// The comparison as the compositor needs it (A's crop + B's upload).
    pub fn to_comparison(&self, app: &App) -> Option<Comparison> {
        let a_id = self.a?;
        let b_id = self.b?;
        let a_cap = self.newest(a_id)?;
        let b_cap = self.newest(b_id)?;
        let a_side = StudioSide {
            capture: a_cap.clone(),
            crop: app.session.layer(a_id)?.crop,
            layer_id: a_id,
        };
        let b_side = StudioSide {
            capture: b_cap.clone(),
            crop: app.session.layer(b_id)?.crop,
            layer_id: b_id,
        };
        Some(Comparison::new(a_side, b_side, self.mode, self.wipe))
    }

    /// The offset to apply to layer B so its newest capture lines up with A's.
    pub fn suggested_offset(&self, app: &App) -> Option<i64> {
        let a_id = self.a?;
        let b_id = self.b?;
        let a_cap = self.newest(a_id)?;
        let b_cap = self.newest(b_id)?;
        let offset_a = app.session.layer(a_id)?.offset_us;
        Some(alignment_offset(offset_a, a_cap.pts_us, b_cap.pts_us))
    }

    /// Forget every capture (e.g. after relinking media).
    pub fn clear(&mut self) {
        self.rings.clear();
    }
}

/// Draw the studio as a floating panel over the canvas.
pub fn show(app: &mut App, ui: &mut Ui) {
    if !app.studio.open {
        return;
    }
    let mut open = true;
    egui::Window::new("Alignment Studio")
        .id(egui::Id::new("alignment-studio"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(320.0)
        .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-18.0, 78.0))
        .frame(
            egui::Frame::NONE
                .fill(theme::SURFACE)
                .stroke(egui::Stroke::new(1.0, theme::BORDER))
                .inner_margin(egui::Margin::same(12)),
        )
        .show(ui.ctx(), |ui| body(app, ui));
    app.studio.open = open && app.studio.open;
}

fn body(app: &mut App, ui: &mut Ui) {
    widgets::section_label(ui, "Compare");
    ui.horizontal(|ui| {
        for mode in StudioMode::ALL {
            let selected = app.studio.mode == mode;
            let text = RichText::new(mode.label())
                .size(12.0)
                .color(if selected {
                    theme::BG
                } else {
                    theme::TEXT_DIM
                });
            let fill = if selected {
                theme::ACCENT
            } else {
                theme::SURFACE_HOVER
            };
            let button = egui::Button::new(text).fill(fill).corner_radius(0);
            if ui.add(button).clicked() {
                app.studio.mode = mode;
            }
        }
    });

    ui.add_space(6.0);
    widgets::section_label(ui, "Position");
    ui.add(
        egui::Slider::new(&mut app.studio.wipe, 0.0..=1.0)
            .show_value(false)
            .trailing_fill(false),
    );
    ui.horizontal(|ui| {
        if widgets::mini_button(ui, "[") {
            app.studio.wipe =
                crate::gpu::capture::nudge_wipe(app.studio.wipe, -shortcuts_step(ui));
        }
        widgets::value_slot(ui, &format!("{}%", (app.studio.wipe * 100.0).round()), 42.0);
        if widgets::mini_button(ui, "]") {
            app.studio.wipe = crate::gpu::capture::nudge_wipe(app.studio.wipe, shortcuts_step(ui));
        }
        widgets::dim(ui, "(Shift = 10%)");
    });

    ui.add_space(8.0);
    widgets::section_label(ui, "Captures");
    let layer_ids: Vec<u64> = app.session.layers.iter().map(|l| l.id).collect();
    for id in layer_ids {
        let label = app
            .session
            .layer(id)
            .map(|l| l.label.clone())
            .unwrap_or_else(|| format!("Layer {id}"));
        let count = app.studio.rings.get(&id).map(|r| r.len()).unwrap_or(0);
        let newest = app.studio.newest(id).map(|c| c.pts_ms());
        ui.horizontal(|ui| {
            let is_a = app.studio.a == Some(id);
            let is_b = app.studio.b == Some(id);
            let mut a_text = RichText::new("A").size(12.0);
            if is_a {
                a_text = a_text.color(theme::BG);
            }
            let mut b_text = RichText::new("B").size(12.0);
            if is_b {
                b_text = b_text.color(theme::BG);
            }
            if ui
                .add(
                    egui::Button::new(a_text)
                        .fill(if is_a { theme::ACCENT } else { theme::SURFACE_HOVER })
                        .corner_radius(0)
                        .min_size(egui::vec2(22.0, 20.0)),
                )
                .clicked()
            {
                app.studio.a = Some(id);
            }
            if ui
                .add(
                    egui::Button::new(b_text)
                        .fill(if is_b { theme::ACCENT } else { theme::SURFACE_HOVER })
                        .corner_radius(0)
                        .min_size(egui::vec2(22.0, 20.0)),
                )
                .clicked()
            {
                app.studio.b = Some(id);
            }
            ui.label(RichText::new(label).size(12.0).color(theme::TEXT));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                match newest {
                    Some(ms) => {
                        widgets::value_slot(ui, &format!("{ms} ms"), 62.0);
                    }
                    None => {
                        widgets::dim(ui, "no capture");
                    }
                }
                widgets::dim(ui, &format!("{count}/8"));
            });
        });
    }

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if widgets::accent_button(ui, "Capture (C)").clicked() {
            app.capture_frame();
        }
        if widgets::button(ui, "Clear").clicked() {
            app.studio.clear();
        }
    });

    ui.add_space(8.0);
    rules(ui);

    widgets::section_label(ui, "Delta");
    match (app.studio.delta_us(), app.studio.suggested_offset(app)) {
        (Some(delta), Some(target)) => {
            let a_label = app
                .studio
                .a
                .and_then(|id| app.session.layer(id))
                .map(|l| l.label.clone())
                .unwrap_or_else(|| "A".into());
            let b_label = app
                .studio
                .b
                .and_then(|id| app.session.layer(id))
                .map(|l| l.label.clone())
                .unwrap_or_else(|| "B".into());
            ui.label(
                RichText::new(format!("offset_{b_label} = {target} µs"))
                    .font(egui::FontId::monospace(13.0))
                    .color(theme::ACCENT),
            );
            widgets::dim(
                ui,
                format!(
                    "pts_B − pts_A = {:+.3} s relative to {a_label}",
                    delta as f64 / 1_000_000.0
                ),
            );
            if widgets::accent_button(ui, format!("Apply to {b_label}")).clicked() {
                if let Some(id) = app.studio.b
                    && let Some(layer) = app.session.layer_mut(id)
                {
                    layer.offset_us = target;
                }
                if let Some(id) = app.studio.b {
                    app.runtime.set_offset(id, target);
                }
                app.rebuild_union();
                app.save_current_session();
                app.notice(format!("{b_label} offset set to {target} µs"));
            }
        }
        _ => {
            widgets::dim(
                ui,
                "Capture one frame on each side (C) to see the exact offset delta.",
            );
        }
    }
}

/// `[`/`]` inside the slider row: 1 %, or 10 % with Shift.
fn shortcuts_step(ui: &Ui) -> i32 {
    if ui.input(|i| i.modifiers.shift) {
        crate::ui::shortcuts::WIPE_STEP_PCT_LARGE
    } else {
        crate::ui::shortcuts::WIPE_STEP_PCT
    }
}

fn rules(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 1.0),
        egui::Sense::hover(),
    );
    ui.painter().rect_filled(rect, 0.0, theme::BORDER);
}

/// Register the studio's texture cache cleanup (called when layers go away).
pub fn retire(resources: &mut egui_wgpu::CallbackResources, live: &[u64]) {
    crate::gpu::compositor::retire_layers(resources, live);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::hwaccel::ColorRange;
    use crate::engine::sync_clock::target_pts_us;
    use crate::engine::video_frame::{PixelFormat, PlaneBuf, VideoFrame};
    use crate::model::layout::CanvasAspect;
    use crate::model::session::{LayerConfig, LayerRole, Session};
    use crate::model::NormalizedRect;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn frame(pts_us: i64) -> Arc<VideoFrame> {
        Arc::new(VideoFrame {
            format: PixelFormat::Yuv420p,
            width: 8,
            height: 8,
            planes: [PlaneBuf::owned(64), PlaneBuf::owned(16), PlaneBuf::owned(16)],
            strides: [8, 4, 4],
            pts_us,
        })
    }

    fn capture(pts_us: i64) -> Capture {
        Capture::new(pts_us, frame(pts_us), ColorRange::Limited)
    }

    fn session_with_two_layers(offset_a: i64) -> Session {
        let mut s = Session::new("s", "S", CanvasAspect::Widescreen);
        let mut a = LayerConfig::new(
            1,
            "A",
            LayerRole::Source,
            PathBuf::from("/tmp/a.mp4"),
            NormalizedRect::full(),
            0,
        );
        a.offset_us = offset_a;
        let b = LayerConfig::new(
            2,
            "B",
            LayerRole::Reaction1,
            PathBuf::from("/tmp/b.mp4"),
            NormalizedRect::full(),
            1,
        );
        s.layers = vec![a, b];
        s
    }

    #[test]
    fn studio_keeps_eight_captures_per_video() {
        let mut studio = StudioState::default();
        for i in 0..12 {
            studio.push_for(7, capture(i * 1_000));
        }
        assert_eq!(studio.rings[&7].len(), 8);
        assert_eq!(studio.newest(7).unwrap().pts_us, 11_000);
    }

    #[test]
    fn captures_auto_assign_a_and_b() {
        let mut studio = StudioState::default();
        studio.push_for(1, capture(0));
        studio.push_for(2, capture(0));
        assert_eq!(studio.a, Some(1));
        assert_eq!(studio.b, Some(2));
    }

    #[test]
    fn delta_is_b_minus_a() {
        let mut studio = StudioState::default();
        studio.push_for(1, capture(1_000_000));
        studio.push_for(2, capture(2_500_000));
        assert_eq!(studio.delta_us(), Some(1_500_000));
        assert_eq!(studio.short_a(), "1000 ms");
        assert_eq!(studio.short_b(), "2500 ms");
    }

    #[test]
    fn suggested_offset_matches_the_engine_formula() {
        let app_session = session_with_two_layers(0);
        let mut studio = StudioState::default();
        studio.push_for(1, capture(1_000_000));
        studio.push_for(2, capture(2_500_000));

        // Recreate the tiny slice of App that the formula needs.
        let a_offset = app_session.layer(1).unwrap().offset_us;
        let a_pts = studio.newest(1).unwrap().pts_us;
        let b_pts = studio.newest(2).unwrap().pts_us;
        let expected = alignment_offset(a_offset, a_pts, b_pts);
        assert_eq!(expected, 1_500_000);

        // The whole point: after applying the offset, both captures sit at the
        // same point on the master timeline.
        assert_eq!(
            target_pts_us(a_pts, a_offset),
            target_pts_us(b_pts, expected)
        );
    }

    #[test]
    fn suggested_offset_accounts_for_an_existing_offset_on_a() {
        let a_offset = -1_000_000;
        let expected = alignment_offset(a_offset, 1_000_000, 2_500_000);
        assert_eq!(expected, 500_000);
        assert_eq!(
            target_pts_us(1_000_000, a_offset),
            target_pts_us(2_500_000, expected)
        );
    }

    #[test]
    fn negative_delta_means_b_is_earlier() {
        let mut studio = StudioState::default();
        studio.push_for(1, capture(3_000_000));
        studio.push_for(2, capture(1_000_000));
        assert_eq!(studio.delta_us(), Some(-2_000_000));
    }

    #[test]
    fn cycling_mode_toggles() {
        let mut studio = StudioState::default();
        assert_eq!(studio.mode, StudioMode::Onion);
        studio.cycle_mode();
        assert_eq!(studio.mode, StudioMode::Wipe);
        studio.cycle_mode();
        assert_eq!(studio.mode, StudioMode::Onion);
    }

    #[test]
    fn clearing_drops_everything() {
        let mut studio = StudioState::default();
        studio.push_for(1, capture(0));
        studio.clear();
        assert!(studio.rings.is_empty());
        assert!(studio.newest(1).is_none());
    }

    #[test]
    fn delta_needs_two_sides() {
        let mut studio = StudioState::default();
        studio.push_for(1, capture(0));
        assert_eq!(studio.delta_us(), None);
    }
}
