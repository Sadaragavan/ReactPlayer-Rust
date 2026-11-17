//! The session: a canvas, a stack of layers and the offsets that line them up.
//!
//! Everything here is plain data and pure functions, so it can be tested
//! without a decoder, a GPU or a window. Z-order is explicit (`z_index`), any
//! layer may be on top, and paint order is strictly ascending while hit-testing
//! is strictly descending.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use super::geom::{Crop, NormRect, Point, Size};
use super::timeline;

/// Bumped whenever the on-disk shape changes in a way old builds cannot read.
pub const SESSION_VERSION: u32 = 1;

/// Milliseconds since the Unix epoch, as stored in session timestamps.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// A unique, sortable id for a session, preset or layer.
pub fn new_id(prefix: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{:x}-{:x}", now_ms(), n)
}

/// What a layer is for. Only used for labels and preset mapping; any layer can
/// be anywhere in the stack.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Source,
    Reaction,
    Extra,
}

impl Role {
    /// Default name for the nth layer of this role.
    pub fn default_name(self, index: usize) -> String {
        match self {
            Self::Source => "Source".to_owned(),
            Self::Reaction => format!("Reaction {}", index + 1),
            Self::Extra => format!("Extra {}", index + 1),
        }
    }
}

/// The canvas shape. A fixed ratio keeps a layout pixel-exact at any window
/// size; `Fill` follows the window instead.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CanvasAspect {
    Ratio16x9,
    Ratio9x16,
    Ratio4x3,
    Ratio1x1,
    Ratio21x9,
    Fill,
}

impl CanvasAspect {
    pub const ALL: [Self; 6] = [
        Self::Ratio16x9,
        Self::Ratio9x16,
        Self::Ratio4x3,
        Self::Ratio1x1,
        Self::Ratio21x9,
        Self::Fill,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Ratio16x9 => "16:9",
            Self::Ratio9x16 => "9:16",
            Self::Ratio4x3 => "4:3",
            Self::Ratio1x1 => "1:1",
            Self::Ratio21x9 => "21:9",
            Self::Fill => "Fill window",
        }
    }

    pub fn ratio(self) -> Option<f32> {
        match self {
            Self::Ratio16x9 => Some(16.0 / 9.0),
            Self::Ratio9x16 => Some(9.0 / 16.0),
            Self::Ratio4x3 => Some(4.0 / 3.0),
            Self::Ratio1x1 => Some(1.0),
            Self::Ratio21x9 => Some(21.0 / 9.0),
            Self::Fill => None,
        }
    }

    /// The canvas rectangle that fits inside `available`.
    pub fn size_in(self, available: Size) -> Size {
        match self.ratio() {
            None => available,
            Some(ratio) => {
                let by_width = Size::new(available.w, available.w / ratio);
                if by_width.h <= available.h {
                    by_width
                } else {
                    Size::new(available.h * ratio, available.h)
                }
            }
        }
    }
}

/// How the picture fills its layer rectangle.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FitMode {
    /// Whole frame visible, letterboxed.
    Contain,
    /// Fill the rectangle, cropping the overflow.
    Cover,
}

impl FitMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Contain => "Fit",
            Self::Cover => "Fill",
        }
    }

    pub fn toggled(self) -> Self {
        match self {
            Self::Contain => Self::Cover,
            Self::Cover => Self::Contain,
        }
    }
}

/// Where the media comes from: a local file or an FFmpeg-supported URL.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum MediaSource {
    File { path: PathBuf },
    Url { url: String },
}

impl MediaSource {
    pub fn file(path: impl Into<PathBuf>) -> Self {
        Self::File { path: path.into() }
    }

    pub fn url(url: impl Into<String>) -> Self {
        Self::Url { url: url.into() }
    }

    /// What FFmpeg should be handed.
    pub fn location(&self) -> String {
        match self {
            Self::File { path } => path.display().to_string(),
            Self::Url { url } => url.clone(),
        }
    }

    /// Short label for the UI.
    pub fn display(&self) -> String {
        match self {
            Self::File { path } => path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string()),
            Self::Url { url } => url.clone(),
        }
    }

    /// The local path, when this is a file.
    pub fn path(&self) -> Option<&std::path::Path> {
        match self {
            Self::File { path } => Some(path),
            Self::Url { .. } => None,
        }
    }

    /// A local file that has since moved breaks playback; a URL is assumed to
    /// work until FFmpeg says otherwise.
    pub fn exists(&self) -> bool {
        match self {
            Self::File { path } => path.exists(),
            Self::Url { .. } => true,
        }
    }
}

/// One video on the canvas.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Layer {
    pub id: String,
    pub name: String,
    pub role: Role,
    pub source: MediaSource,

    /// Duration in microseconds, filled in after probing.
    #[serde(default)]
    pub duration_us: i64,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    /// Exact frame rate as a rational, straight from the container.
    #[serde(default = "default_fps_num")]
    pub fps_num: u32,
    #[serde(default = "default_fps_den")]
    pub fps_den: u32,
    #[serde(default)]
    pub has_audio: bool,
    #[serde(default)]
    pub audio_channels: u16,
    #[serde(default)]
    pub audio_rate: u32,

    /// Higher is painted later, i.e. on top.
    pub z_index: i32,
    /// `target_pts_us = master_clock_us + offset_us`. Positive means this layer
    /// shows later material of its own file at the same timeline time.
    pub offset_us: i64,

    #[serde(default = "default_true")]
    pub visible: bool,
    #[serde(default)]
    pub locked: bool,

    pub rect: NormRect,
    #[serde(default)]
    pub crop: Crop,
    #[serde(default = "default_opacity")]
    pub opacity: f32,
    #[serde(default = "default_fit")]
    pub fit: FitMode,
    /// Pixel aspect ratio to hold while resizing, when locked.
    #[serde(default)]
    pub aspect_lock: Option<f32>,

    #[serde(default = "default_volume")]
    pub volume: f32,
    #[serde(default)]
    pub muted: bool,
}

fn default_fps_num() -> u32 {
    30
}

fn default_fps_den() -> u32 {
    1
}

fn default_true() -> bool {
    true
}

fn default_opacity() -> f32 {
    1.0
}

fn default_volume() -> f32 {
    1.0
}

fn default_fit() -> FitMode {
    FitMode::Contain
}

impl Layer {
    pub fn new(name: impl Into<String>, role: Role, source: MediaSource, rect: NormRect, z: i32) -> Self {
        Self {
            id: new_id("layer"),
            name: name.into(),
            role,
            source,
            duration_us: 0,
            width: 0,
            height: 0,
            fps_num: 30,
            fps_den: 1,
            has_audio: false,
            audio_channels: 0,
            audio_rate: 0,
            z_index: z,
            offset_us: 0,
            visible: true,
            locked: false,
            rect,
            crop: Crop::ZERO,
            opacity: 1.0,
            fit: FitMode::Contain,
            aspect_lock: None,
            volume: 1.0,
            muted: false,
        }
    }

    /// One frame, in microseconds.
    pub fn frame_us(&self) -> i64 {
        if self.fps_num == 0 {
            return 33_333;
        }
        1_000_000 * i64::from(self.fps_den.max(1)) / i64::from(self.fps_num.max(1))
    }

    /// Where this layer starts on the union timeline.
    pub fn start_us(&self) -> i64 {
        -self.offset_us
    }

    /// Where it ends.
    pub fn end_us(&self) -> i64 {
        self.duration_us - self.offset_us
    }

    /// Whether the layer has footage at union time `t`.
    pub fn is_active_at(&self, t_us: i64) -> bool {
        t_us >= self.start_us() && t_us < self.end_us()
    }

    /// Source aspect ratio, falling back to 16:9 before probing.
    pub fn source_aspect(&self) -> f32 {
        if self.width == 0 || self.height == 0 {
            return 16.0 / 9.0;
        }
        self.width as f32 / self.height as f32
    }

    /// The crop as a shader-ready UV rectangle.
    pub fn uv_crop(&self) -> [f32; 4] {
        self.crop.uv()
    }
}

/// A saved reaction session.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Session {
    pub version: u32,
    pub id: String,
    pub name: String,
    pub canvas_aspect: CanvasAspect,
    pub layers: Vec<Layer>,
    /// The layer whose sound is the master, by id.
    #[serde(default)]
    pub master_audio: Option<String>,
    #[serde(default = "default_true")]
    pub is_linked: bool,
    /// Lend the master's audio to the topmost active layer when the master has
    /// no footage at the current time.
    #[serde(default = "default_true")]
    pub keep_sound_going: bool,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
    /// File name of the thumbnail inside the data directory.
    #[serde(default)]
    pub thumbnail: Option<String>,
}

impl Session {
    pub fn new(name: impl Into<String>) -> Self {
        let now = now_ms();
        Self {
            version: SESSION_VERSION,
            id: new_id("session"),
            name: name.into(),
            canvas_aspect: CanvasAspect::Ratio16x9,
            layers: Vec::new(),
            master_audio: None,
            is_linked: true,
            keep_sound_going: true,
            created_at: now,
            updated_at: now,
            thumbnail: None,
        }
    }

    pub fn layer(&self, id: &str) -> Option<&Layer> {
        self.layers.iter().find(|l| l.id == id)
    }

    pub fn layer_index(&self, id: &str) -> Option<usize> {
        self.layers.iter().position(|l| l.id == id)
    }

    pub fn layer_mut(&mut self, id: &str) -> Option<&mut Layer> {
        self.layers.iter_mut().find(|l| l.id == id)
    }

    /// Indices of layers in paint order: strictly ascending `z_index`, ties
    /// broken by insertion order so the result is deterministic.
    pub fn paint_order(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.layers.len()).collect();
        order.sort_by_key(|i| (self.layers[*i].z_index, *i));
        order
    }

    /// Topmost visible, unlocked layer under `p`, or `None`.
    pub fn hit_test(&self, p: Point) -> Option<usize> {
        let mut order = self.paint_order();
        order.reverse();
        order
            .into_iter()
            .find(|i| {
                let layer = &self.layers[*i];
                layer.visible && !layer.locked && layer.rect.contains(p)
            })
    }

    /// Rewrite `z_index` as a dense `0..n-1` range, preserving relative order.
    pub fn normalize_z(&mut self) {
        let order = self.paint_order();
        for (z, i) in order.into_iter().enumerate() {
            self.layers[i].z_index = z as i32;
        }
    }

    pub fn z_of(&self, id: &str) -> Option<i32> {
        self.layer(id).map(|l| l.z_index)
    }

    /// Move a layer to the top of the stack.
    pub fn bring_to_front(&mut self, id: &str) {
        if let Some(i) = self.layer_index(id) {
            let top = self.layers.iter().map(|l| l.z_index).max().unwrap_or(0);
            self.layers[i].z_index = top + 1;
            self.normalize_z();
        }
    }

    pub fn send_to_back(&mut self, id: &str) {
        if let Some(i) = self.layer_index(id) {
            let bottom = self.layers.iter().map(|l| l.z_index).min().unwrap_or(0);
            self.layers[i].z_index = bottom - 1;
            self.normalize_z();
        }
    }

    /// One step up. A no-op when the layer is already on top.
    pub fn bring_forward(&mut self, id: &str) {
        let order = self.paint_order();
        let Some(pos) = order.iter().position(|i| self.layers[*i].id == id) else {
            return;
        };
        if pos + 1 >= order.len() {
            return;
        }
        let above = order[pos + 1];
        let me = order[pos];
        // Swap z so the pair exchanges places, then re-densify.
        let a = self.layers[me].z_index;
        let b = self.layers[above].z_index;
        if a == b {
            self.layers[above].z_index = b + 1;
        } else {
            self.layers[me].z_index = b;
            self.layers[above].z_index = a;
        }
        self.normalize_z();
    }

    /// One step down. A no-op when the layer is already at the bottom.
    pub fn send_backward(&mut self, id: &str) {
        let order = self.paint_order();
        let Some(pos) = order.iter().position(|i| self.layers[*i].id == id) else {
            return;
        };
        if pos == 0 {
            return;
        }
        let below = order[pos - 1];
        let me = order[pos];
        let a = self.layers[me].z_index;
        let b = self.layers[below].z_index;
        if a == b {
            self.layers[below].z_index = b - 1;
        } else {
            self.layers[me].z_index = b;
            self.layers[below].z_index = a;
        }
        self.normalize_z();
    }

    /// The layers with footage at union time `t`, topmost first.
    pub fn active_at(&self, t_us: i64) -> Vec<usize> {
        let mut order = self.paint_order();
        order.reverse();
        order
            .into_iter()
            .filter(|i| self.layers[*i].is_active_at(t_us))
            .collect()
    }

    /// The layer that should carry the master's sound at `t` when the master
    /// itself has no footage: the highest active layer.
    pub fn lend_master(&self, t_us: i64) -> Option<&Layer> {
        self.active_at(t_us)
            .into_iter()
            .map(|i| &self.layers[i])
            .find(|l| l.has_audio)
    }

    /// The master audio layer, if it still exists.
    pub fn master(&self) -> Option<&Layer> {
        self.master_audio
            .as_deref()
            .and_then(|id| self.layer(id))
            .or_else(|| self.default_master())
    }

    /// Source first, then the first layer that has audio.
    pub fn default_master(&self) -> Option<&Layer> {
        self.layers
            .iter()
            .find(|l| l.role == Role::Source && l.has_audio)
            .or_else(|| self.layers.iter().find(|l| l.has_audio))
    }

    /// The whole union timeline.
    pub fn union(&self) -> timeline::Union {
        timeline::union_of(&self.layers)
    }

    /// Layers whose media file is gone.
    pub fn missing_media(&self) -> Vec<usize> {
        (0..self.layers.len())
            .filter(|i| !self.layers[*i].source.exists())
            .collect()
    }

    /// Point a layer at a new file, keeping its offset and geometry.
    pub fn relink(&mut self, id: &str, source: MediaSource) -> bool {
        match self.layer_mut(id) {
            Some(layer) => {
                layer.source = source;
                true
            }
            None => false,
        }
    }

    /// Copy of this session with fresh ids so it can be edited independently.
    pub fn duplicated(&self) -> Self {
        let mut copy = self.clone();
        copy.id = new_id("session");
        copy.name = format!("{} copy", self.name);
        copy.created_at = now_ms();
        copy.updated_at = copy.created_at;
        copy.thumbnail = None;
        let mut master_index = None;
        for (i, layer) in copy.layers.iter_mut().enumerate() {
            let old = layer.id.clone();
            layer.id = new_id("layer");
            if self.master_audio.as_deref() == Some(old.as_str()) {
                master_index = Some(i);
            }
        }
        copy.master_audio = master_index.map(|i| copy.layers[i].id.clone());
        copy
    }

    /// Bump the modification time; called before every save.
    pub fn touch(&mut self) {
        self.updated_at = now_ms();
    }

    /// The canvas rectangle for an available area.
    pub fn canvas_size(&self, available: Size) -> Size {
        self.canvas_aspect.size_in(available)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(name: &str, z: i32) -> Layer {
        Layer::new(
            name,
            Role::Reaction,
            MediaSource::file(format!("/tmp/{name}.mp4")),
            NormRect::FULL,
            z,
        )
    }

    #[test]
    fn paint_order_is_ascending_and_hit_test_descending() {
        let mut s = Session::new("t");
        s.layers.push(layer("a", 0));
        s.layers.push(layer("b", 2));
        s.layers.push(layer("c", 1));
        let names: Vec<&str> = s
            .paint_order()
            .into_iter()
            .map(|i| s.layers[i].name.as_str())
            .collect();
        assert_eq!(names, ["a", "c", "b"]);
        // All three cover the whole canvas: the topmost wins.
        let hit = s.hit_test(Point { x: 0.5, y: 0.5 }).unwrap();
        assert_eq!(s.layers[hit].name, "b");
    }

    #[test]
    fn hit_test_skips_hidden_and_locked_layers() {
        let mut s = Session::new("t");
        s.layers.push(layer("bottom", 0));
        s.layers.push(layer("top", 1));
        s.layers[1].visible = false;
        assert_eq!(s.layers[s.hit_test(Point { x: 0.5, y: 0.5 }).unwrap()].name, "bottom");
        s.layers[1].visible = true;
        s.layers[1].locked = true;
        assert_eq!(s.layers[s.hit_test(Point { x: 0.5, y: 0.5 }).unwrap()].name, "bottom");
        s.layers[1].locked = false;
        assert_eq!(s.layers[s.hit_test(Point { x: 0.5, y: 0.5 }).unwrap()].name, "top");
        assert!(s.hit_test(Point { x: 4.0, y: 4.0 }).is_none());
    }

    #[test]
    fn reordering_normalizes_to_a_dense_range() {
        let mut s = Session::new("t");
        for (i, name) in ["a", "b", "c", "d"].iter().enumerate() {
            s.layers.push(layer(name, (i as i32) * 10));
        }
        s.normalize_z();
        assert_eq!(
            s.layers.iter().map(|l| l.z_index).collect::<Vec<_>>(),
            [0, 1, 2, 3]
        );

        let id = s.layers[1].id.clone();
        s.bring_to_front(&id);
        assert_eq!(s.z_of(&id), Some(3));
        assert_eq!(
            s.paint_order()
                .into_iter()
                .map(|i| s.layers[i].name.clone())
                .collect::<Vec<_>>(),
            ["a", "c", "d", "b"]
        );

        s.send_to_back(&id);
        assert_eq!(s.z_of(&id), Some(0));

        s.bring_forward(&id);
        assert_eq!(s.z_of(&id), Some(1));
        s.send_backward(&id);
        assert_eq!(s.z_of(&id), Some(0));
        // Already at the bottom: stays put.
        s.send_backward(&id);
        assert_eq!(s.z_of(&id), Some(0));
    }

    #[test]
    fn bring_forward_is_a_no_op_at_the_top() {
        let mut s = Session::new("t");
        s.layers.push(layer("a", 0));
        s.layers.push(layer("b", 1));
        let top = s.layers[1].id.clone();
        s.bring_forward(&top);
        assert_eq!(s.z_of(&top), Some(1));
    }

    #[test]
    fn offsets_move_the_layer_on_the_union_timeline() {
        let mut l = layer("a", 0);
        l.duration_us = 10_000_000;
        assert_eq!(l.start_us(), 0);
        assert_eq!(l.end_us(), 10_000_000);
        l.offset_us = 1_500_000;
        assert_eq!(l.start_us(), -1_500_000);
        assert_eq!(l.end_us(), 8_500_000);
        assert!(l.is_active_at(0));
        assert!(!l.is_active_at(-1_600_000));
    }

    #[test]
    fn master_falls_back_to_the_first_layer_with_audio() {
        let mut s = Session::new("t");
        let mut a = layer("a", 0);
        a.role = Role::Source;
        a.has_audio = false;
        let mut b = layer("b", 1);
        b.has_audio = true;
        s.layers.push(a);
        s.layers.push(b);
        assert_eq!(s.default_master().map(|l| l.name.clone()), Some("b".into()));
        s.master_audio = Some(s.layers[0].id.clone());
        assert_eq!(s.master().map(|l| l.name.clone()), Some("a".into()));
        s.master_audio = Some("gone".into());
        assert_eq!(s.master().map(|l| l.name.clone()), Some("b".into()));
    }

    #[test]
    fn duplicate_regenerates_ids_and_keeps_the_master() {
        let mut s = Session::new("original");
        let mut a = layer("a", 0);
        a.has_audio = true;
        s.layers.push(a);
        s.master_audio = Some(s.layers[0].id.clone());
        let copy = s.duplicated();
        assert_ne!(copy.id, s.id);
        assert_ne!(copy.layers[0].id, s.layers[0].id);
        assert_eq!(copy.master_audio.as_deref(), Some(copy.layers[0].id.as_str()));
        assert_eq!(copy.name, "original copy");
    }

    #[test]
    fn canvas_aspect_fits_the_available_area() {
        let avail = Size::new(1000.0, 1000.0);
        let s = CanvasAspect::Ratio16x9.size_in(avail);
        assert!((s.w - 1000.0).abs() < 0.01);
        assert!((s.h - 562.5).abs() < 0.01);
        let tall = CanvasAspect::Ratio9x16.size_in(avail);
        assert!((tall.h - 1000.0).abs() < 0.01);
        assert!((tall.w - 562.5).abs() < 0.01);
        assert_eq!(CanvasAspect::Fill.size_in(avail), avail);
        assert_eq!(CanvasAspect::Fill.ratio(), None);
    }

    #[test]
    fn lensed_audio_goes_to_the_topmost_active_layer() {
        let mut s = Session::new("t");
        let mut low = layer("low", 0);
        low.has_audio = true;
        low.duration_us = 10_000_000;
        let mut high = layer("high", 5);
        high.has_audio = true;
        high.duration_us = 10_000_000;
        let mut silent = layer("silent", 9);
        silent.duration_us = 10_000_000;
        s.layers.push(low);
        s.layers.push(high);
        s.layers.push(silent);
        assert_eq!(s.lend_master(1_000_000).map(|l| l.name.clone()), Some("high".into()));
    }
}
