//! Session document: the persistent description of a multi-stream reaction
//! layout.

use serde::{Deserialize, Serialize};

use super::geom::{CropRect, NormalizedRect};
use super::layout::{CanvasAspect, FitMode};

/// Current schema version written into every session file.
pub const SCHEMA_VERSION: u32 = 1;

/// The role a layer plays, used when applying presets to new media.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LayerRole {
    /// The original video being reacted to.
    Source,
    /// First reaction clip.
    Reaction1,
    /// Second reaction clip.
    Reaction2,
    /// Third reaction clip.
    Reaction3,
    /// Fourth (and further) reaction clips all map here.
    ReactionOther,
}

impl LayerRole {
    /// Default display label.
    pub fn default_label(self) -> &'static str {
        match self {
            LayerRole::Source => "Source",
            LayerRole::Reaction1 => "Reaction 1",
            LayerRole::Reaction2 => "Reaction 2",
            LayerRole::Reaction3 => "Reaction 3",
            LayerRole::ReactionOther => "Reaction",
        }
    }

    /// Role for the n-th reaction layer (0-based).
    pub fn reaction(index: usize) -> Self {
        match index {
            0 => LayerRole::Reaction1,
            1 => LayerRole::Reaction2,
            2 => LayerRole::Reaction3,
            _ => LayerRole::ReactionOther,
        }
    }
}

/// Hardware acceleration preference (see [`crate::engine::hwaccel`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum HardwareAccel {
    /// Try hardware, fall back to software on any failure.
    #[default]
    Auto,
    /// Try hardware but prefer it even when the hardware path is slower to
    /// initialise; still falls back on failure.
    HardwarePreferred,
    /// Never touch hardware.
    SoftwareOnly,
}

impl HardwareAccel {
    /// All variants in menu order.
    pub const ALL: [HardwareAccel; 3] = [
        HardwareAccel::Auto,
        HardwareAccel::HardwarePreferred,
        HardwareAccel::SoftwareOnly,
    ];

    /// Label for the settings screen.
    pub fn label(self) -> &'static str {
        match self {
            HardwareAccel::Auto => "Auto",
            HardwareAccel::HardwarePreferred => "Hardware Preferred",
            HardwareAccel::SoftwareOnly => "Software Only",
        }
    }

    /// Description for the settings screen.
    pub fn description(self) -> &'static str {
        match self {
            HardwareAccel::Auto => "Decode on the GPU when it works, silently fall back to CPU.",
            HardwareAccel::HardwarePreferred => "Prefer VAAPI/D3D11VA/VideoToolbox; fall back if unsupported.",
            HardwareAccel::SoftwareOnly => "Always decode on the CPU (most compatible).",
        }
    }
}

/// One video layer of a session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerConfig {
    /// Stable identifier, unique within the session.
    pub id: u64,
    /// User-visible label ("Source", "Reaction 1", ...).
    pub label: String,
    /// Semantic role.
    pub role: LayerRole,
    /// Absolute path or FFmpeg-supported URL.
    pub source: String,
    /// Geometry in normalized canvas space.
    pub rect: NormalizedRect,
    /// Z-order. Painted ascending; hit-tested descending.
    pub z_index: i32,
    /// Texture crop window.
    #[serde(default)]
    pub crop: CropRect,
    /// Opacity in `0..=1`.
    #[serde(default = "one")]
    pub opacity: f32,
    /// Hidden layers are not painted and not hit-tested.
    #[serde(default)]
    pub visible: bool,
    /// Locked layers are not hit-tested or draggable, but still painted.
    #[serde(default)]
    pub locked: bool,
    /// `target_pts_us = master_clock_us + offset_us` (positive = later media).
    #[serde(default)]
    pub offset_us: i64,
    /// Per-track volume in `0..=2`.
    #[serde(default = "one")]
    pub volume: f32,
    /// Per-track mute.
    #[serde(default)]
    pub muted: bool,
    /// Fit or fill inside the layer rectangle.
    #[serde(default)]
    pub fit: FitMode,
}

fn one() -> f32 {
    1.0
}

impl LayerConfig {
    /// Create a layer with sensible defaults: visible, unlocked, unit opacity.
    pub fn new(
        id: u64,
        label: impl Into<String>,
        role: LayerRole,
        source: impl Into<String>,
        rect: NormalizedRect,
        z_index: i32,
    ) -> Self {
        Self {
            id,
            label: label.into(),
            role,
            source: source.into(),
            rect,
            z_index,
            crop: CropRect::none(),
            opacity: 1.0,
            visible: true,
            locked: false,
            offset_us: 0,
            volume: 1.0,
            muted: false,
            fit: FitMode::Fit,
        }
    }

    /// Normalize persisted values that could be out of range.
    pub fn sanitized(mut self) -> Self {
        self.rect = self.rect.sanitized().clamped_to_canvas();
        self.crop = self.crop.sanitized();
        self.opacity = self.opacity.clamp(0.0, 1.0);
        self.volume = self.volume.clamp(0.0, 2.0);
        self
    }
}

/// A saved session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    /// Schema version, always written.
    pub version: u32,
    /// Stable identifier (also the file stem on disk).
    pub id: String,
    /// Display name.
    pub name: String,
    /// Canvas aspect.
    pub aspect: CanvasAspect,
    /// Layers (order is irrelevant; `z_index` is authoritative).
    pub layers: Vec<LayerConfig>,
    /// Which layer owns the master audio track.
    pub master_audio_layer: Option<u64>,
    /// Per-layer offsets linked to the master timeline.
    #[serde(default)]
    pub linked: bool,
    /// Creation time, seconds since the Unix epoch.
    pub created_unix: u64,
    /// Last modification time, seconds since the Unix epoch.
    pub modified_unix: u64,
    /// Thumbnail file name inside the session data dir, if generated.
    #[serde(default)]
    pub thumbnail: Option<String>,
}

impl Session {
    /// Create a new empty session.
    pub fn new(id: impl Into<String>, name: impl Into<String>, aspect: CanvasAspect) -> Self {
        let now = now_unix();
        Self {
            version: SCHEMA_VERSION,
            id: id.into(),
            name: name.into(),
            aspect,
            layers: Vec::new(),
            master_audio_layer: None,
            linked: true,
            created_unix: now,
            modified_unix: now,
            thumbnail: None,
        }
    }

    /// Normalize z-indices to a dense `0..n-1` range, keeping relative order.
    ///
    /// The list is not reordered: only the `z_index` fields change. Ties are
    /// broken by the current position in `layers`.
    pub fn normalize_z(&mut self) {
        let mut order: Vec<usize> = (0..self.layers.len()).collect();
        order.sort_by_key(|&i| (self.layers[i].z_index, i));
        for (rank, &i) in order.iter().enumerate() {
            self.layers[i].z_index = rank as i32;
        }
    }

    /// Layer indices sorted by ascending `z_index` (paint order).
    pub fn paint_order(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.layers.len()).collect();
        order.sort_by_key(|&i| (self.layers[i].z_index, i));
        order
    }

    /// Highest `z_index` layer that is visible, used for top-down hit-testing.
    pub fn hit_order(&self) -> Vec<usize> {
        let mut order = self.paint_order();
        order.reverse();
        order
    }

    /// Find a layer by id.
    pub fn layer(&self, id: u64) -> Option<&LayerConfig> {
        self.layers.iter().find(|l| l.id == id)
    }

    /// Find a layer by id, mutably.
    pub fn layer_mut(&mut self, id: u64) -> Option<&mut LayerConfig> {
        self.layers.iter_mut().find(|l| l.id == id)
    }

    /// Index of a layer by id.
    pub fn layer_index(&self, id: u64) -> Option<usize> {
        self.layers.iter().position(|l| l.id == id)
    }

    /// Next unused layer id.
    pub fn next_layer_id(&self) -> u64 {
        self.layers.iter().map(|l| l.id).max().unwrap_or(0) + 1
    }

    /// Bring `id` to the front (topmost).
    ///
    /// Returns true when the layer actually moved, so callers can skip a
    /// pointless save.
    pub fn bring_to_front(&mut self, id: u64) -> bool {
        let Some(idx) = self.layer_index(id) else {
            return false;
        };
        self.normalize_z();
        // Already the topmost? Then there is nothing to save.
        if let Some(&top_i) = self.paint_order().last()
            && top_i == idx
        {
            return false;
        }
        let top = self
            .layers
            .iter()
            .map(|l| l.z_index)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        self.layers[idx].z_index = top;
        self.normalize_z();
        true
    }

    /// Send `id` to the back (bottom-most). Returns true if moved.
    pub fn send_to_back(&mut self, id: u64) -> bool {
        let Some(idx) = self.layer_index(id) else {
            return false;
        };
        self.normalize_z();
        if let Some(&bottom_i) = self.paint_order().first()
            && bottom_i == idx
        {
            return false;
        }
        let bottom = self
            .layers
            .iter()
            .map(|l| l.z_index)
            .min()
            .unwrap_or(0)
            .saturating_sub(1);
        self.layers[idx].z_index = bottom;
        self.normalize_z();
        true
    }

    /// Move `id` one step forward (towards the front). Returns true if moved.
    pub fn bring_forward(&mut self, id: u64) -> bool {
        self.swap_with_neighbour(id, true)
    }

    /// Move `id` one step backward (towards the back). Returns true if moved.
    pub fn send_backward(&mut self, id: u64) -> bool {
        self.swap_with_neighbour(id, false)
    }

    fn swap_with_neighbour(&mut self, id: u64, forward: bool) -> bool {
        let Some(idx) = self.layer_index(id) else {
            return false;
        };
        self.normalize_z();
        let z = self.layers[idx].z_index;
        let target_z = if forward { z + 1 } else { z - 1 };
        let Some(other) = self.layers.iter().position(|l| l.z_index == target_z) else {
            return false;
        };
        self.layers[idx].z_index = target_z;
        self.layers[other].z_index = z;
        self.normalize_z();
        true
    }

    /// Top-most visible, unlocked layer at normalized canvas point `(x, y)`.
    ///
    /// Hit-testing runs strictly descending by z-index and skips hidden and
    /// locked layers.
    pub fn hit_test(&self, x: f32, y: f32) -> Option<u64> {
        for i in self.hit_order() {
            let l = &self.layers[i];
            if !l.visible || l.locked {
                continue;
            }
            if l.rect.contains(x, y) {
                return Some(l.id);
            }
        }
        None
    }

    /// Move a layer to a new position in the ordered list for drag-and-drop
    /// reordering in the Layers panel.
    ///
    /// `from_slot` and `to_slot` are positions in *paint order* (0 = back).
    pub fn reorder_slots(&mut self, from_slot: usize, to_slot: usize) {
        self.normalize_z();
        let order = self.paint_order();
        if from_slot >= order.len() || to_slot >= order.len() {
            return;
        }
        let moving = order[from_slot];
        let mut seq: Vec<usize> = order
            .iter()
            .copied()
            .filter(|&i| i != moving)
            .collect();
        seq.insert(to_slot.min(seq.len()), moving);
        for (rank, &i) in seq.iter().enumerate() {
            self.layers[i].z_index = rank as i32;
        }
        self.normalize_z();
    }

    /// Fix up invariants after loading: dense z-indices, sanitized geometry,
    /// a valid master audio layer.
    pub fn sanitize(&mut self) {
        self.layers = std::mem::take(&mut self.layers)
            .into_iter()
            .map(LayerConfig::sanitized)
            .collect();
        self.normalize_z();
        if let Some(master) = self.master_audio_layer
            && self.layer(master).is_none()
        {
            self.master_audio_layer = None;
        }
        if self.master_audio_layer.is_none() {
            self.master_audio_layer = self
                .layers
                .iter()
                .min_by_key(|l| l.z_index)
                .map(|l| l.id);
        }
    }

    /// Thumbnail path relative to the session's data directory.
    pub fn thumbnail_file(&self) -> String {
        format!("{}.png", self.id)
    }

    /// A short summary for session cards.
    pub fn summary(&self) -> SessionSummary {
        SessionSummary {
            id: self.id.clone(),
            name: self.name.clone(),
            layer_count: self.layers.len(),
            modified_unix: self.modified_unix,
            thumbnail: self.thumbnail.clone(),
        }
    }
}

/// Lightweight description of a session for the session browser.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionSummary {
    /// Session id (file stem).
    pub id: String,
    /// Display name.
    pub name: String,
    /// Number of layers.
    pub layer_count: usize,
    /// Last modification time.
    pub modified_unix: u64,
    /// Thumbnail file name, if any.
    pub thumbnail: Option<String>,
}

/// Current Unix time in seconds.
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_session() -> Session {
        let mut s = Session::new("s1", "Test", CanvasAspect::Widescreen);
        s.layers.push(LayerConfig::new(
            1,
            "Source",
            LayerRole::Source,
            "/tmp/a.mp4",
            NormalizedRect::new(0.0, 0.0, 0.5, 1.0),
            0,
        ));
        s.layers.push(LayerConfig::new(
            2,
            "Reaction 1",
            LayerRole::Reaction1,
            "/tmp/b.mp4",
            NormalizedRect::new(0.5, 0.0, 0.5, 1.0),
            1,
        ));
        s.layers.push(LayerConfig::new(
            3,
            "PiP",
            LayerRole::Reaction2,
            "/tmp/c.mp4",
            NormalizedRect::new(0.6, 0.6, 0.3, 0.3),
            2,
        ));
        s
    }

    #[test]
    fn z_indices_normalize_densely() {
        let mut s = sample_session();
        s.layers[1].z_index = 10;
        s.layers[2].z_index = 20;
        s.normalize_z();
        let mut zs: Vec<i32> = s.layers.iter().map(|l| l.z_index).collect();
        zs.sort();
        assert_eq!(zs, vec![0, 1, 2]);
        // Relative order preserved: layer 1 stays bottom.
        assert_eq!(s.layer(1).unwrap().z_index, 0);
        assert_eq!(s.layer(2).unwrap().z_index, 1);
        assert_eq!(s.layer(3).unwrap().z_index, 2);
    }

    #[test]
    fn paint_order_is_ascending_and_hit_order_descending() {
        let mut s = sample_session();
        s.bring_to_front(1);
        let paint: Vec<u64> = s.paint_order().iter().map(|&i| s.layers[i].id).collect();
        let hit: Vec<u64> = s.hit_order().iter().map(|&i| s.layers[i].id).collect();
        assert_eq!(paint, vec![2, 3, 1]);
        let mut rev = paint.clone();
        rev.reverse();
        assert_eq!(hit, rev);
    }

    #[test]
    fn hit_test_returns_topmost_overlapping_layer() {
        let mut s = sample_session();
        // Point inside the PiP (layer 3) which also sits over layer 2 & 1? It does not;
        // layer 3 covers 0.6..0.9 x 0.6..0.9 which is inside layer 2.
        assert_eq!(s.hit_test(0.7, 0.7), Some(3));
        s.send_to_back(3);
        assert_eq!(s.hit_test(0.7, 0.7), Some(2));
    }

    #[test]
    fn hit_test_skips_hidden_and_locked_layers() {
        let mut s = sample_session();
        s.layer_mut(3).unwrap().visible = false;
        assert_eq!(s.hit_test(0.7, 0.7), Some(2));
        s.layer_mut(3).unwrap().visible = true;
        s.layer_mut(3).unwrap().locked = true;
        assert_eq!(s.hit_test(0.7, 0.7), Some(2));
        s.layer_mut(2).unwrap().locked = true;
        // Only layer 1 covers it now (0..0.5 x) -- it does not, so None.
        assert_eq!(s.hit_test(0.7, 0.7), None);
    }

    #[test]
    fn bring_and_send_walk_the_stack() {
        let mut s = sample_session();
        s.bring_forward(1);
        assert_eq!(s.layer(1).unwrap().z_index, 1);
        s.send_backward(1);
        assert_eq!(s.layer(1).unwrap().z_index, 0);
        s.send_to_back(1);
        assert_eq!(s.layer(1).unwrap().z_index, 0);
        s.bring_to_front(1);
        assert_eq!(s.layer(1).unwrap().z_index, 2);
    }

    #[test]
    fn reorder_slots_moves_a_layer() {
        let mut s = sample_session();
        // Paint order [1, 2, 3]; move slot 0 (layer 1) to slot 2.
        s.reorder_slots(0, 2);
        assert_eq!(s.layer(1).unwrap().z_index, 2);
        assert_eq!(s.layer(2).unwrap().z_index, 0);
        assert_eq!(s.layer(3).unwrap().z_index, 1);
    }

    #[test]
    fn sanitize_repairs_master_audio_and_geometry() {
        let mut s = sample_session();
        s.master_audio_layer = Some(99);
        s.layers[0].opacity = 5.0;
        s.layers[0].rect = NormalizedRect::new(f32::NAN, 0.0, 1.0, 1.0);
        s.sanitize();
        assert_eq!(s.master_audio_layer, Some(1));
        assert_eq!(s.layers[0].opacity, 1.0);
        assert_eq!(s.layers[0].rect, NormalizedRect::full());
    }

    #[test]
    fn session_json_round_trip() {
        let s = sample_session();
        let json = serde_json::to_string_pretty(&s).unwrap();
        assert!(json.contains("\"version\": 1"));
        let back: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn missing_optional_fields_get_defaults() {
        let json = r#"{
            "version": 1,
            "id": "x",
            "name": "X",
            "aspect": "Widescreen",
            "layers": [{
                "id": 1, "label": "A", "role": "Source", "source": "/a.mp4",
                "rect": {"x":0.0,"y":0.0,"w":1.0,"h":1.0}, "z_index": 0,
                "opacity": 1.0, "visible": true
            }],
            "master_audio_layer": 1,
            "linked": true,
            "created_unix": 0, "modified_unix": 0
        }"#;
        let s: Session = serde_json::from_str(json).unwrap();
        assert_eq!(s.layers[0].crop, CropRect::none());
        assert!(!s.layers[0].muted);
        assert_eq!(s.layers[0].offset_us, 0);
        assert_eq!(s.layers[0].fit, FitMode::Fit);
        assert!(s.thumbnail.is_none());
    }

    #[test]
    fn next_layer_id_skips_used_ids() {
        let s = sample_session();
        assert_eq!(s.next_layer_id(), 4);
    }

    #[test]
    fn summary_counts_layers() {
        let mut s = sample_session();
        s.sanitize();
        let sum = s.summary();
        assert_eq!(sum.layer_count, 3);
        assert_eq!(sum.id, "s1");
    }
}
