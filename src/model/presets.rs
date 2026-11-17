//! Layouts and presets.
//!
//! A preset is slot-based: slot 0 is the source, slot 1 the first reaction,
//! slots 2-3 the extras. Applying one maps slots onto layers in order, so the
//! same preset works for any set of videos. Slots carry the full geometry
//! (rect, crop, opacity, fit) and z-order, not just rectangles.

use serde::{Deserialize, Serialize};

use super::geom::{Crop, NormRect};
use super::session::{CanvasAspect, FitMode, Session, now_ms, new_id};

/// Bumped when the preset file shape changes.
pub const PRESET_VERSION: u32 = 1;

/// The layouts offered when starting a session.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LayoutType {
    /// Source left, reactions stacked on the right.
    SideBySide,
    /// Equal cells: 2 stacked, 3 in an L, 4 in a 2x2.
    Grid,
    /// Reaction fills the canvas, the source sits on top as an inset.
    PipReactionOverSource,
    /// Source fills the canvas, the reaction sits on top as an inset.
    PipSourceOverReaction,
    /// Anything the user built by hand; saved presets only.
    Custom,
}

impl LayoutType {
    /// The four that can be chosen when creating a session.
    pub const CHOOSABLE: [Self; 4] = [
        Self::SideBySide,
        Self::Grid,
        Self::PipReactionOverSource,
        Self::PipSourceOverReaction,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::SideBySide => "Side-by-Side Split",
            Self::Grid => "Grid",
            Self::PipReactionOverSource => "Picture-in-Picture (Reaction over Source)",
            Self::PipSourceOverReaction => "Picture-in-Picture (Source over Reaction)",
            Self::Custom => "Custom",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::SideBySide => "Source on the left, every reaction stacked on the right.",
            Self::Grid => "Equal cells: two stacked, three in an L, four in a 2x2.",
            Self::PipReactionOverSource => "The source fills the canvas; the reaction sits on top.",
            Self::PipSourceOverReaction => "The reaction fills the canvas; the source sits on top.",
            Self::Custom => "Saved from a session you arranged by hand.",
        }
    }
}

/// The rectangles a layout gives `count` layers, in slot order.
pub fn layout_rects(layout: LayoutType, count: usize) -> Vec<NormRect> {
    let n = count.clamp(2, 4);
    match layout {
        LayoutType::Grid => grid(n),
        LayoutType::PipReactionOverSource => pip(n, PipOrder::ReactionOnTop),
        LayoutType::PipSourceOverReaction => pip(n, PipOrder::SourceOnTop),
        LayoutType::SideBySide | LayoutType::Custom => split(n),
    }
}

/// The z-order a layout wants, in slot order. Higher is on top.
pub fn layout_z(layout: LayoutType, count: usize) -> Vec<i32> {
    let n = count.clamp(2, 4);
    match layout {
        LayoutType::PipReactionOverSource => {
            let mut z: Vec<i32> = (0..n as i32).collect();
            // Slot 0 is the source; it goes behind the reaction in slot 1.
            z[0] = 0;
            if n > 1 {
                z[1] = 1;
            }
            z
        }
        LayoutType::PipSourceOverReaction => {
            let mut z: Vec<i32> = (0..n as i32).collect();
            if n > 1 {
                z[0] = 1;
                z[1] = 0;
            }
            z
        }
        _ => (0..n as i32).collect(),
    }
}

enum PipOrder {
    SourceOnTop,
    ReactionOnTop,
}

fn split(n: usize) -> Vec<NormRect> {
    let rest = (n - 1) as f32;
    let mut rects = vec![NormRect::new(0.0, 0.0, 0.5, 1.0)];
    for i in 0..(n - 1) {
        rects.push(NormRect::new(0.5, i as f32 / rest, 0.5, 1.0 / rest));
    }
    rects
}

fn grid(n: usize) -> Vec<NormRect> {
    if n == 2 {
        return vec![
            NormRect::new(0.0, 0.0, 1.0, 0.5),
            NormRect::new(0.0, 0.5, 1.0, 0.5),
        ];
    }
    let mut cells = vec![
        NormRect::new(0.0, 0.0, 0.5, 0.5),
        NormRect::new(0.5, 0.0, 0.5, 0.5),
        NormRect::new(0.0, 0.5, 0.5, 0.5),
        NormRect::new(0.5, 0.5, 0.5, 0.5),
    ];
    if n == 3 {
        cells[2] = NormRect::new(0.25, 0.5, 0.5, 0.5);
    }
    cells.truncate(n);
    cells
}

fn pip(n: usize, order: PipOrder) -> Vec<NormRect> {
    let size = 0.32;
    let margin = 0.02;
    let inset = NormRect::new(1.0 - size - margin, 1.0 - size - margin, size, size);
    let mut rects = match order {
        // Slot 0 is the source.
        PipOrder::SourceOnTop => vec![inset, NormRect::FULL],
        PipOrder::ReactionOnTop => vec![NormRect::FULL, inset],
    };
    let extras = [
        NormRect::new(1.0 - size - margin, margin, size, size),
        NormRect::new(margin, margin, size, size),
    ];
    for rect in extras {
        if rects.len() >= n {
            break;
        }
        rects.push(rect);
    }
    rects.truncate(n);
    rects
}

/// One layer's placement inside a preset.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PresetSlot {
    pub rect: NormRect,
    #[serde(default)]
    pub crop: Crop,
    #[serde(default = "one")]
    pub opacity: f32,
    #[serde(default = "default_fit")]
    pub fit: FitMode,
    /// Paint order within the preset.
    #[serde(default)]
    pub z_index: i32,
}

fn one() -> f32 {
    1.0
}

fn default_fit() -> FitMode {
    FitMode::Contain
}

impl PresetSlot {
    pub fn new(rect: NormRect, z_index: i32) -> Self {
        Self {
            rect,
            crop: Crop::ZERO,
            opacity: 1.0,
            fit: FitMode::Contain,
            z_index,
        }
    }
}

/// A saved layout.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Preset {
    pub version: u32,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub built_in: bool,
    pub layout: LayoutType,
    #[serde(default = "default_aspect")]
    pub canvas_aspect: CanvasAspect,
    pub slots: Vec<PresetSlot>,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

fn default_aspect() -> CanvasAspect {
    CanvasAspect::Ratio16x9
}

impl Preset {
    pub fn new(name: impl Into<String>, layout: LayoutType, slots: Vec<PresetSlot>) -> Self {
        let now = now_ms();
        Self {
            version: PRESET_VERSION,
            id: new_id("preset"),
            name: name.into(),
            built_in: false,
            layout,
            canvas_aspect: CanvasAspect::Ratio16x9,
            slots,
            created_at: now,
            updated_at: now,
        }
    }

    /// Snapshot a session's current arrangement as a reusable preset.
    pub fn from_session(name: impl Into<String>, session: &Session) -> Self {
        let paint = session.paint_order();
        let mut slots = Vec::with_capacity(session.layers.len());
        for (z, index) in paint.into_iter().enumerate() {
            let layer = &session.layers[index];
            slots.push(PresetSlot {
                rect: layer.rect,
                crop: layer.crop,
                opacity: layer.opacity,
                fit: layer.fit,
                z_index: z as i32,
            });
        }
        let mut preset = Self::new(name, LayoutType::Custom, slots);
        preset.canvas_aspect = session.canvas_aspect;
        preset
    }

    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }
}

/// The presets that ship with the app.
pub fn builtin_presets() -> Vec<Preset> {
    let mk = |id: &str, name: &str, layout: LayoutType, slots: Vec<PresetSlot>| Preset {
        version: PRESET_VERSION,
        id: (*id).to_owned(),
        name: (*name).to_owned(),
        built_in: true,
        layout,
        canvas_aspect: CanvasAspect::Ratio16x9,
        slots,
        created_at: 0,
        updated_at: 0,
    };
    let from_layout = |layout: LayoutType, n: usize| {
        let rects = layout_rects(layout, n);
        let z = layout_z(layout, n);
        rects
            .into_iter()
            .zip(z)
            .map(|(r, z)| PresetSlot::new(r, z))
            .collect::<Vec<_>>()
    };
    vec![
        mk(
            "std_side",
            "Side by side",
            LayoutType::SideBySide,
            from_layout(LayoutType::SideBySide, 2),
        ),
        mk(
            "std_stack",
            "Stacked",
            LayoutType::Grid,
            from_layout(LayoutType::Grid, 2),
        ),
        mk(
            "std_pip_reaction",
            "Corner, reaction on top",
            LayoutType::PipReactionOverSource,
            from_layout(LayoutType::PipReactionOverSource, 2),
        ),
        mk(
            "std_pip_source",
            "Corner, source on top",
            LayoutType::PipSourceOverReaction,
            from_layout(LayoutType::PipSourceOverReaction, 2),
        ),
        mk(
            "std_three",
            "Source plus two",
            LayoutType::SideBySide,
            from_layout(LayoutType::SideBySide, 3),
        ),
        mk(
            "std_quad",
            "Four-up grid",
            LayoutType::Grid,
            from_layout(LayoutType::Grid, 4),
        ),
    ]
}

/// Apply a preset to a session, mapping slots onto layers in order and
/// preserving offsets, names and volume (a preset is a layout, not a session).
pub fn apply_preset(session: &mut Session, preset: &Preset) {
    if preset.slots.is_empty() || session.layers.is_empty() {
        return;
    }
    let order = session.paint_order();
    for (slot_index, layer_index) in order.clone().into_iter().enumerate() {
        let slot = &preset.slots[slot_index.min(preset.slots.len() - 1)];
        let layer = &mut session.layers[layer_index];
        layer.rect = slot.rect;
        layer.crop = slot.crop;
        layer.opacity = slot.opacity;
        layer.fit = slot.fit;
    }
    // Slot order is the preset's paint order, so z comes from the slots.
    for (slot_index, layer_index) in order.into_iter().enumerate() {
        let slot = &preset.slots[slot_index.min(preset.slots.len() - 1)];
        session.layers[layer_index].z_index = slot.z_index;
    }
    session.normalize_z();
    session.touch();
}

/// Kept for readers of the layout table in the New Session dialog.
pub fn layout_summary(layout: LayoutType) -> &'static str {
    layout.description()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::session::{MediaSource, Role};

    fn session_with(n: usize) -> Session {
        let mut s = Session::new("preset test");
        for i in 0..n {
            s.layers.push(crate::model::session::Layer::new(
                format!("L{i}"),
                if i == 0 { Role::Source } else { Role::Reaction },
                MediaSource::file(format!("/tmp/{i}.mp4")),
                NormRect::FULL,
                i as i32,
            ));
        }
        s
    }

    #[test]
    fn side_by_side_halves_the_canvas() {
        let rects = layout_rects(LayoutType::SideBySide, 2);
        assert_eq!(rects[0], NormRect::new(0.0, 0.0, 0.5, 1.0));
        assert_eq!(rects[1], NormRect::new(0.5, 0.0, 0.5, 1.0));
    }

    #[test]
    fn grid_shapes() {
        let two = layout_rects(LayoutType::Grid, 2);
        assert_eq!(two.len(), 2);
        assert_eq!(two[1], NormRect::new(0.0, 0.5, 1.0, 0.5));
        let three = layout_rects(LayoutType::Grid, 3);
        assert_eq!(three[2], NormRect::new(0.25, 0.5, 0.5, 0.5));
        let four = layout_rects(LayoutType::Grid, 4);
        assert_eq!(four[3], NormRect::new(0.5, 0.5, 0.5, 0.5));
    }

    #[test]
    fn pip_puts_the_source_on_top_or_underneath() {
        let reaction_on_top = layout_rects(LayoutType::PipReactionOverSource, 2);
        assert_eq!(reaction_on_top[0], NormRect::FULL);
        assert!(reaction_on_top[1].w < 0.5);
        let z = layout_z(LayoutType::PipReactionOverSource, 2);
        assert!(z[1] > z[0]);

        let source_on_top = layout_rects(LayoutType::PipSourceOverReaction, 2);
        assert_eq!(source_on_top[1], NormRect::FULL);
        let z = layout_z(LayoutType::PipSourceOverReaction, 2);
        assert!(z[0] > z[1]);
    }

    #[test]
    fn every_layout_covers_the_requested_count() {
        for layout in LayoutType::CHOOSABLE {
            for n in 2..=4 {
                let rects = layout_rects(layout, n);
                assert_eq!(rects.len(), n, "{layout:?} {n}");
                let z = layout_z(layout, n);
                let mut sorted = z.clone();
                sorted.sort_unstable();
                assert_eq!(sorted, (0..n as i32).collect::<Vec<_>>());
            }
        }
    }

    #[test]
    fn builtins_are_all_two_to_four_slots() {
        let presets = builtin_presets();
        assert!(presets.len() >= 4);
        for p in &presets {
            assert!(p.built_in);
            assert!((2..=4).contains(&p.slot_count()), "{}", p.name);
        }
    }

    #[test]
    fn applying_a_preset_keeps_offsets_and_names() {
        let mut s = session_with(2);
        s.layers[0].offset_us = 1_500_000;
        s.layers[1].name = "Reaction 1".into();
        let preset = builtin_presets()
            .into_iter()
            .find(|p| p.id == "std_side")
            .unwrap();
        apply_preset(&mut s, &preset);
        assert_eq!(s.layers[0].offset_us, 1_500_000);
        assert_eq!(s.layers[1].name, "Reaction 1");
        assert_eq!(s.layers[0].rect, NormRect::new(0.0, 0.0, 0.5, 1.0));
        assert_eq!(s.layers[1].rect, NormRect::new(0.5, 0.0, 0.5, 1.0));
    }

    #[test]
    fn preset_round_trip_through_json() {
        let mut s = session_with(3);
        s.layers[1].crop = Crop {
            left: 0.1,
            ..Crop::ZERO
        };
        s.layers[1].opacity = 0.5;
        s.canvas_aspect = CanvasAspect::Ratio9x16;
        let preset = Preset::from_session("mine", &s);
        assert_eq!(preset.slot_count(), 3);
        assert_eq!(preset.canvas_aspect, CanvasAspect::Ratio9x16);
        let json = serde_json::to_string(&preset).unwrap();
        let back: Preset = serde_json::from_str(&json).unwrap();
        assert_eq!(back, preset);
        assert_eq!(back.version, PRESET_VERSION);
    }

    #[test]
    fn preset_from_session_records_paint_order() {
        let mut s = session_with(2);
        s.layers[0].z_index = 7;
        s.layers[1].z_index = 3;
        let preset = Preset::from_session("order", &s);
        // Slot order follows paint order: layer 1 (z=3) first.
        assert_eq!(preset.slots[0].rect, s.layers[1].rect);
        assert_eq!(preset.slots[0].z_index, 0);
        assert_eq!(preset.slots[1].z_index, 1);
    }
}
