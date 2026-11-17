//! The New Session modal: name, canvas aspect, layout preset, and one row per
//! layer with an editable label and z-order.

use eframe::egui::{self, RichText};

use crate::app::{App, Screen};
use crate::model::layout::CanvasAspect;
use crate::model::presets::{NewSessionLayout, PresetLibrary};
use crate::model::session::{LayerConfig, LayerRole, Session};
use crate::model::store::generate_id;
use crate::model::NormalizedRect;
use crate::ui::theme;
use crate::ui::widgets;

/// How many layers the modal allows (matching the four built-in layouts).
pub const MAX_LAYERS: usize = 4;

/// Draft state of the New Session modal.
#[derive(Debug, Clone)]
pub struct NewSessionForm {
    /// Session name.
    pub name: String,
    /// Canvas aspect.
    pub aspect: CanvasAspect,
    /// Chosen layout preset.
    pub layout: NewSessionLayout,
    /// Layer count.
    pub layer_count: usize,
    /// Per-layer editable labels.
    pub labels: Vec<String>,
}

impl Default for NewSessionForm {
    fn default() -> Self {
        let layout = NewSessionLayout::Split;
        let count = layout.default_layer_count();
        Self {
            name: "Untitled session".to_string(),
            aspect: CanvasAspect::Widescreen,
            layout,
            layer_count: count,
            labels: default_labels(count),
        }
    }
}

/// Default labels for `count` layers: Source then Reaction 1..n.
pub fn default_labels(count: usize) -> Vec<String> {
    let mut out = Vec::new();
    for i in 0..count.clamp(1, MAX_LAYERS) {
        if i == 0 {
            out.push(LayerRole::Source.default_label().to_string());
        } else {
            out.push(LayerRole::reaction(i - 1).default_label().to_string());
        }
    }
    out
}

impl NewSessionForm {
    /// Keep `labels` at exactly `layer_count` entries.
    pub fn sync_labels(&mut self) {
        self.layer_count = self.layer_count.clamp(1, MAX_LAYERS);
        let wanted = default_labels(self.layer_count);
        self.labels.resize(self.layer_count, String::new());
        for (i, w) in wanted.into_iter().enumerate() {
            if self.labels[i].trim().is_empty() {
                self.labels[i] = w;
            }
        }
        self.labels.truncate(self.layer_count);
    }

    /// Build the session this form describes.
    ///
    /// The geometry comes from the chosen layout's built-in preset, so a new
    /// session and the matching preset can never disagree.
    pub fn build_session(&self, presets: &PresetLibrary) -> Session {
        let mut count = self.layer_count.clamp(1, MAX_LAYERS);
        let mut session = Session::new(generate_id(), sanitize_title(&self.name), self.aspect);
        session.linked = true;
        let mut layers = Vec::new();
        for i in 0..count {
            let role = if i == 0 {
                LayerRole::Source
            } else {
                LayerRole::reaction(i - 1)
            };
            let label = self
                .labels
                .get(i)
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(|| role.default_label().to_string());
            layers.push(LayerConfig::new(
                (i + 1) as u64,
                label,
                role,
                String::new(),
                NormalizedRect::full(),
                i as i32,
            ));
        }
        session.layers = layers;
        // Applying the preset gives each role its slot. `apply` never touches
        // sources, so the empty paths stay empty until the user picks files.
        if let Some(preset) = presets
            .presets
            .iter()
            .find(|p| p.id == self.layout.preset_id())
        {
            preset.apply(&mut session);
        }
        session.master_audio_layer = Some(1);
        count = session.layers.len();
        session.normalize_z();
        session.sanitize();
        let _ = count;
        session
    }
}

/// Trim a title, falling back to a neutral default.
pub fn sanitize_title(name: &str) -> String {
    let t = name.trim();
    if t.is_empty() {
        "Untitled session".to_string()
    } else {
        t.chars().take(80).collect()
    }
}

/// Draw the modal.
pub fn show(app: &mut App, ctx: &egui::Context) {
    let Some(mut form) = app.new_session.clone() else {
        return;
    };
    let mut keep_open = true;
    let mut created = false;
    let mut cancelled = false;

    egui::Modal::new(egui::Id::new("new-session-modal"))
        .frame(
            egui::Frame::NONE
                .fill(theme::BG)
                .stroke(egui::Stroke::new(1.0, theme::BORDER))
                .inner_margin(egui::Margin::same(18)),
        )
        .show(ctx, |ui| {
            ui.set_width(520.0);
            ui.label(
                RichText::new("New session")
                    .size(18.0)
                    .strong()
                    .color(theme::TEXT),
            );
            ui.add_space(10.0);

            widgets::labelled_text(
                ui,
                egui::Id::new("ns-name"),
                "Name",
                &mut form.name,
                "Session name",
            );
            ui.add_space(8.0);

            widgets::section_label(ui, "Canvas");
            ui.horizontal_wrapped(|ui| {
                for aspect in CanvasAspect::ALL {
                    if widgets::chip(ui, aspect.label(), form.aspect == aspect) {
                        form.aspect = aspect;
                    }
                }
            });
            ui.add_space(8.0);

            widgets::section_label(ui, "Layout");
            ui.horizontal_wrapped(|ui| {
                for layout in NewSessionLayout::ALL {
                    if widgets::chip(ui, short_layout(layout), form.layout == layout) {
                        form.layout = layout;
                        form.layer_count = layout.default_layer_count();
                        form.labels = default_labels(form.layer_count);
                    }
                }
            });
            ui.add_space(10.0);

            widgets::section_label(ui, "Layers");
            ui.horizontal(|ui| {
                widgets::dim(ui, "Count");
                if widgets::mini_button(ui, "−") && form.layer_count > 1 {
                    form.layer_count -= 1;
                    form.sync_labels();
                }
                widgets::value_slot(ui, &form.layer_count.to_string(), 20.0);
                if widgets::mini_button(ui, "+") && form.layer_count < MAX_LAYERS {
                    form.layer_count += 1;
                    form.sync_labels();
                }
                widgets::dim(ui, "Labels are editable; z-order follows the list.");
            });
            ui.add_space(6.0);
            form.sync_labels();
            ui.horizontal(|ui| {
                for i in 0..form.layer_count {
                    let role = if i == 0 {
                        LayerRole::Source
                    } else {
                        LayerRole::reaction(i - 1)
                    };
                    ui.vertical(|ui| {
                        widgets::dim(ui, role.default_label());
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut form.labels[i])
                                .id(egui::Id::new(("ns-label", i)))
                                .desired_width(110.0)
                                .margin(egui::Margin::symmetric(4, 3)),
                        );
                        resp.on_hover_text("Layer label");
                    });
                }
            });

            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if widgets::accent_button(ui, "Create session").clicked() {
                    let session = form.build_session(&app.persisted.presets);
                    app.session = session;
                    app.selected = None;
                    app.rebuild_union();
                    app.save_current_session();
                    app.bind_runtime();
                    app.screen = Screen::Editor;
                    created = true;
                }
                if widgets::button(ui, "Cancel").clicked() {
                    cancelled = true;
                }
                widgets::dim(ui, "Sources can be assigned after creating.");
            });
        });

    if created || cancelled {
        app.new_session = None;
    } else if keep_open {
        form.sync_labels();
        app.new_session = Some(form);
    }
    keep_open = false;
    let _ = keep_open;
}

fn short_layout(layout: NewSessionLayout) -> &'static str {
    match layout {
        NewSessionLayout::Split => "Split",
        NewSessionLayout::Grid => "Grid",
        NewSessionLayout::PipReactionOverSource => "PIP reaction",
        NewSessionLayout::PipSourceOverReaction => "PIP source",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn presets() -> PresetLibrary {
        PresetLibrary::default()
    }

    #[test]
    fn default_form_has_a_source_and_a_reaction() {
        let form = NewSessionForm::default();
        assert_eq!(form.layer_count, 2);
        assert_eq!(form.labels, vec!["Source", "Reaction 1"]);
    }

    #[test]
    fn labels_grow_and_shrink_with_the_count() {
        let mut form = NewSessionForm::default();
        form.layer_count = 4;
        form.sync_labels();
        assert_eq!(form.labels.len(), 4);
        assert_eq!(form.labels[3], "Reaction 3");
        form.layer_count = 1;
        form.sync_labels();
        assert_eq!(form.labels.len(), 1);
        form.layer_count = 99;
        form.sync_labels();
        assert_eq!(form.labels.len(), MAX_LAYERS);
    }

    #[test]
    fn custom_labels_survive_sync() {
        let mut form = NewSessionForm::default();
        form.labels[0] = "My source".to_string();
        form.sync_labels();
        assert_eq!(form.labels[0], "My source");
    }

    #[test]
    fn built_session_uses_per_role_geometry() {
        let form = NewSessionForm::default();
        let session = form.build_session(&presets());
        assert_eq!(session.layers.len(), 2);
        // builtin-split: source on the left, reaction on the right.
        assert!((session.layers[0].rect.x - 0.0).abs() < 1e-6);
        assert!((session.layers[1].rect.x - 0.5).abs() < 1e-6);
        assert_eq!(session.layers[0].z_index, 0);
        assert_eq!(session.layers[1].z_index, 1);
        assert_eq!(session.master_audio_layer, Some(1));
    }

    #[test]
    fn grid_layout_builds_four_layers() {
        let mut form = NewSessionForm::default();
        form.layout = NewSessionLayout::Grid;
        form.layer_count = NewSessionLayout::Grid.default_layer_count();
        form.labels = default_labels(form.layer_count);
        let session = form.build_session(&presets());
        assert_eq!(session.layers.len(), 4);
        // A 2x2 grid: all four quadrants are occupied exactly once.
        let mut seen = Vec::new();
        for l in &session.layers {
            seen.push((l.rect.x, l.rect.y));
        }
        assert!(seen.contains(&(0.0, 0.0)));
        assert!(seen.iter().any(|(x, y)| *x >= 0.49 && *y == 0.0));
        assert!(seen.iter().any(|(x, y)| *x == 0.0 && *y >= 0.49));
        assert!(seen.iter().any(|(x, y)| *x >= 0.49 && *y >= 0.49));
    }

    #[test]
    fn titles_are_trimmed_and_defaulted() {
        assert_eq!(sanitize_title("  Demo  "), "Demo");
        assert_eq!(sanitize_title("   "), "Untitled session");
        assert_eq!(sanitize_title(&"x".repeat(120)).len(), 80);
    }

    #[test]
    fn every_layout_has_a_short_label() {
        for l in NewSessionLayout::ALL {
            assert!(!short_layout(l).is_empty());
            assert!(l.default_layer_count() >= 2);
        }
    }
}
