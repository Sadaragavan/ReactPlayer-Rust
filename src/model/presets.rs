//! Layout presets.
//!
//! A preset captures *geometry, crops and z-order* for a set of roles. Applying
//! a preset maps the session's layers onto the preset's roles (or, when roles
//! do not match, by order), so a standard "side-by-side" layout can be reused
//! with different media.

use serde::{Deserialize, Serialize};

use super::geom::{CropRect, NormalizedRect};
use super::layout::FitMode;
use super::session::{LayerRole, Session, SCHEMA_VERSION};

/// One entry of a preset: the look of a single role.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PresetRole {
    /// Which role this entry describes.
    pub role: LayerRole,
    /// Where the layer sits on the canvas.
    pub rect: NormalizedRect,
    /// Texture crop.
    #[serde(default)]
    pub crop: CropRect,
    /// Opacity.
    #[serde(default = "one")]
    pub opacity: f32,
    /// Fit/fill.
    #[serde(default)]
    pub fit: FitMode,
    /// Stacking order (dense, ascending).
    pub z_index: i32,
}

fn one() -> f32 {
    1.0
}

/// A named layout preset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    /// Schema version, always written.
    pub version: u32,
    /// Stable identifier (file stem).
    pub id: String,
    /// Display name.
    pub name: String,
    /// True for the built-in presets (not removable).
    #[serde(default)]
    pub builtin: bool,
    /// Role entries.
    pub roles: Vec<PresetRole>,
}

impl Preset {
    /// A preset from a list of role entries.
    pub fn new(id: impl Into<String>, name: impl Into<String>, roles: Vec<PresetRole>) -> Self {
        Self {
            version: SCHEMA_VERSION,
            id: id.into(),
            name: name.into(),
            builtin: false,
            roles,
        }
    }

    /// Capture the geometry of an existing session into a new custom preset.
    pub fn from_session(
        id: impl Into<String>,
        name: impl Into<String>,
        session: &Session,
    ) -> Self {
        let mut s = session.clone();
        s.normalize_z();
        let roles = s
            .layers
            .iter()
            .map(|l| PresetRole {
                role: l.role,
                rect: l.rect,
                crop: l.crop,
                opacity: l.opacity,
                fit: l.fit,
                z_index: l.z_index,
            })
            .collect();
        Self::new(id, name, roles)
    }

    /// Apply the preset to a session in place.
    ///
    /// Matching is done by role first; any remaining preset entries are applied
    /// in order to the remaining layers. Layers with no preset entry keep their
    /// current geometry. Also returns the number of layers updated.
    pub fn apply(&self, session: &mut Session) -> usize {
        let mut used_roles: Vec<bool> = vec![false; self.roles.len()];
        let mut updated = 0usize;
        let layer_ids: Vec<u64> = session.layers.iter().map(|l| l.id).collect();

        // Pass 1: exact role matches.
        for id in &layer_ids {
            let Some(idx) = session.layer_index(*id) else {
                continue;
            };
            let role = session.layers[idx].role;
            if let Some((pi, _)) = self
                .roles
                .iter()
                .enumerate()
                .find(|(pi, r)| r.role == role && !used_roles[*pi])
            {
                used_roles[pi] = true;
                apply_role(&mut session.layers[idx], &self.roles[pi]);
                updated += 1;
            }
        }

        // Pass 2: fill from the remaining entries, in order.
        let mut remaining = self
            .roles
            .iter()
            .enumerate()
            .filter(|(i, _)| !used_roles[*i])
            .map(|(_, r)| r);
        for id in &layer_ids {
            let Some(next) = remaining.next() else { break };
            let Some(idx) = session.layer_index(*id) else {
                continue;
            };
            if used_roles
                .iter()
                .enumerate()
                .any(|(i, used)| !used && self.roles[i].role == session.layers[idx].role)
            {
                continue;
            }
            apply_role(&mut session.layers[idx], next);
            updated += 1;
        }

        session.normalize_z();
        updated
    }
}

fn apply_role(layer: &mut super::session::LayerConfig, entry: &PresetRole) {
    layer.rect = entry.rect.sanitized().clamped_to_canvas();
    layer.crop = entry.crop.sanitized();
    layer.opacity = entry.opacity.clamp(0.0, 1.0);
    layer.fit = entry.fit;
    layer.z_index = entry.z_index;
}

/// The library of built-in plus user presets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PresetLibrary {
    /// Schema version, always written.
    pub version: u32,
    /// All presets (built-ins first).
    pub presets: Vec<Preset>,
}

impl Default for PresetLibrary {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            presets: builtin_presets(),
        }
    }
}

impl PresetLibrary {
    /// Load the built-ins and merge user presets from disk.
    pub fn with_user(user: Vec<Preset>) -> Self {
        let mut lib = Self::default();
        for p in user {
            if p.builtin {
                continue;
            }
            lib.presets.retain(|e| e.id != p.id);
            lib.presets.push(p);
        }
        lib
    }

    /// Find a preset by id.
    pub fn get(&self, id: &str) -> Option<&Preset> {
        self.presets.iter().find(|p| p.id == id)
    }

    /// Add or replace a preset by id.
    pub fn upsert(&mut self, preset: Preset) {
        self.presets.retain(|p| p.id != preset.id);
        self.presets.push(preset);
    }

    /// Remove a user preset. Built-ins cannot be removed.
    pub fn remove(&mut self, id: &str) -> bool {
        if self.get(id).is_some_and(|p| p.builtin) {
            return false;
        }
        let before = self.presets.len();
        self.presets.retain(|p| p.id != id);
        self.presets.len() != before
    }

    /// Only the user presets, for persistence.
    pub fn user_presets(&self) -> Vec<Preset> {
        self.presets
            .iter()
            .filter(|p| !p.builtin)
            .cloned()
            .collect()
    }
}

fn role(
    role: LayerRole,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    z: i32,
) -> PresetRole {
    PresetRole {
        role,
        rect: NormalizedRect::new(x, y, w, h),
        crop: CropRect::none(),
        opacity: 1.0,
        fit: FitMode::Fit,
        z_index: z,
    }
}

/// The standard layouts offered by "New Session" and the Presets screen.
pub fn builtin_presets() -> Vec<Preset> {
    let mut out = Vec::new();

    out.push(Preset {
        builtin: true,
        ..Preset::new(
            "builtin-split",
            "Side-by-Side Split",
            vec![
                role(LayerRole::Source, 0.0, 0.0, 0.5, 1.0, 0),
                role(LayerRole::Reaction1, 0.5, 0.0, 0.5, 1.0, 1),
                role(LayerRole::Reaction2, 0.5, 0.5, 0.5, 0.5, 2),
            ],
        )
    });

    // 2x2 grid.
    out.push(Preset {
        builtin: true,
        ..Preset::new(
            "builtin-grid",
            "Grid",
            vec![
                role(LayerRole::Source, 0.0, 0.0, 0.5, 0.5, 0),
                role(LayerRole::Reaction1, 0.5, 0.0, 0.5, 0.5, 1),
                role(LayerRole::Reaction2, 0.0, 0.5, 0.5, 0.5, 2),
                role(LayerRole::Reaction3, 0.5, 0.5, 0.5, 0.5, 3),
                role(LayerRole::ReactionOther, 0.5, 0.5, 0.5, 0.5, 4),
            ],
        )
    });

    // Reaction picture-in-picture over the source.
    out.push(Preset {
        builtin: true,
        ..Preset::new(
            "builtin-pip-reaction",
            "Picture-in-Picture (Reaction over Source)",
            vec![
                role(LayerRole::Source, 0.0, 0.0, 1.0, 1.0, 0),
                role(LayerRole::Reaction1, 0.66, 0.62, 0.32, 0.32, 1),
                role(LayerRole::Reaction2, 0.34, 0.62, 0.30, 0.30, 2),
                role(LayerRole::Reaction3, 0.02, 0.62, 0.30, 0.30, 3),
            ],
        )
    });

    // Source picture-in-picture over the reaction.
    out.push(Preset {
        builtin: true,
        ..Preset::new(
            "builtin-pip-source",
            "Picture-in-Picture (Source over Reaction)",
            vec![
                role(LayerRole::Reaction1, 0.0, 0.0, 1.0, 1.0, 0),
                role(LayerRole::Reaction2, 0.0, 1.0, 0.0, 0.0, 1),
                role(LayerRole::Source, 0.66, 0.62, 0.32, 0.32, 2),
                role(LayerRole::Reaction3, 0.0, 1.0, 0.0, 0.0, 3),
            ],
        )
    });

    out
}

/// Layout choice in the New Session modal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewSessionLayout {
    /// Side-by-side.
    Split,
    /// 2x2.
    Grid,
    /// Reaction over source.
    PipReactionOverSource,
    /// Source over reaction.
    PipSourceOverReaction,
}

impl NewSessionLayout {
    /// All choices in menu order.
    pub const ALL: [NewSessionLayout; 4] = [
        NewSessionLayout::Split,
        NewSessionLayout::Grid,
        NewSessionLayout::PipReactionOverSource,
        NewSessionLayout::PipSourceOverReaction,
    ];

    /// Menu label.
    pub fn label(self) -> &'static str {
        match self {
            NewSessionLayout::Split => "Side-by-Side Split",
            NewSessionLayout::Grid => "Grid",
            NewSessionLayout::PipReactionOverSource => "Picture-in-Picture (Reaction over Source)",
            NewSessionLayout::PipSourceOverReaction => "Picture-in-Picture (Source over Reaction)",
        }
    }

    /// The built-in preset id backing this layout.
    pub fn preset_id(self) -> &'static str {
        match self {
            NewSessionLayout::Split => "builtin-split",
            NewSessionLayout::Grid => "builtin-grid",
            NewSessionLayout::PipReactionOverSource => "builtin-pip-reaction",
            NewSessionLayout::PipSourceOverReaction => "builtin-pip-source",
        }
    }

    /// Number of layers created by default for this layout, including source.
    pub fn default_layer_count(self) -> usize {
        match self {
            NewSessionLayout::Split => 2,
            NewSessionLayout::Grid => 4,
            NewSessionLayout::PipReactionOverSource => 2,
            NewSessionLayout::PipSourceOverReaction => 2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::session::LayerConfig;

    fn session_with_roles(roles: &[LayerRole]) -> Session {
        let mut s = Session::new("t", "T", crate::model::CanvasAspect::Widescreen);
        for (i, r) in roles.iter().enumerate() {
            s.layers.push(LayerConfig::new(
                (i + 1) as u64,
                r.default_label(),
                *r,
                format!("/tmp/{i}.mp4"),
                NormalizedRect::full(),
                i as i32,
            ));
        }
        s.sanitize()
    }

    #[test]
    fn builtin_presets_are_rectangular_and_ordered() {
        for p in builtin_presets() {
            assert!(!p.roles.is_empty(), "preset {} has no roles", p.id);
            for r in &p.roles {
                assert!(r.rect.is_valid());
                assert!(r.rect.x >= 0.0 && r.rect.y >= 0.0);
                assert!(r.rect.right() <= 1.0001 && r.rect.bottom() <= 1.0001);
            }
        }
    }

    #[test]
    fn split_preset_round_trips_through_session() {
        let lib = PresetLibrary::default();
        let split = lib.get("builtin-split").unwrap();
        let mut s = session_with_roles(&[LayerRole::Source, LayerRole::Reaction1]);
        let n = split.apply(&mut s);
        assert_eq!(n, 2);
        assert_eq!(s.layer(1).unwrap().rect.w, 0.5);
        assert_eq!(s.layer(1).unwrap().rect.x, 0.0);
        assert_eq!(s.layer(2).unwrap().rect.x, 0.5);
        assert_eq!(s.layer(2).unwrap().z_index, 1);
    }

    #[test]
    fn applying_preset_preserves_offsets_and_crops_are_replaced() {
        let lib = PresetLibrary::default();
        let grid = lib.get("builtin-grid").unwrap();
        let mut s = session_with_roles(&[LayerRole::Source, LayerRole::Reaction1]);
        s.layers[0].offset_us = 1_500_000;
        s.layers[0].crop.left = 0.2;
        grid.apply(&mut s);
        assert_eq!(s.layers[0].offset_us, 1_500_000, "offset untouched");
        assert_eq!(s.layers[0].crop, CropRect::none(), "crop replaced");
    }

    #[test]
    fn capture_and_apply_round_trip() {
        let mut s = session_with_roles(&[LayerRole::Source, LayerRole::Reaction1]);
        s.layers[0].rect = NormalizedRect::new(0.1, 0.1, 0.4, 0.6);
        s.layers[1].rect = NormalizedRect::new(0.5, 0.2, 0.4, 0.6);
        s.layers[1].crop = CropRect {
            left: 0.05,
            ..CropRect::none()
        };
        s.normalize_z();
        let p = Preset::from_session("custom-1", "Mine", &s);
        let mut other = session_with_roles(&[LayerRole::Source, LayerRole::Reaction1]);
        p.apply(&mut other);
        assert_eq!(other.layers[0].rect, s.layers[0].rect);
        assert_eq!(other.layers[1].rect, s.layers[1].rect);
        assert_eq!(other.layers[1].crop, s.layers[1].crop);
    }

    #[test]
    fn library_keeps_builtins_when_merging_user_presets() {
        let mut lib = PresetLibrary::with_user(vec![Preset::new("u1", "User", vec![])]);
        assert!(lib.get("builtin-split").is_some());
        assert!(lib.get("u1").is_some());
        assert!(!lib.remove("builtin-split"));
        assert!(lib.remove("u1"));
        assert_eq!(lib.user_presets().len(), 0);
        lib.upsert(Preset::new("u2", "Two", vec![]));
        assert_eq!(lib.user_presets().len(), 1);
    }

    #[test]
    fn new_session_layouts_map_to_presets() {
        for l in NewSessionLayout::ALL {
            let lib = PresetLibrary::default();
            let p = lib.get(l.preset_id()).unwrap();
            assert!(p.roles.len() >= l.default_layer_count());
        }
    }
}
