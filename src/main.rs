//! reaction-player: a frame-accurate multi-stream reaction player.
//!
//! One window, up to four stacked videos, per-layer offsets to the
//! millisecond, and a union timeline that starts whenever any layer starts.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]
#![warn(unsafe_op_in_unsafe_fn)]

mod app;
mod engine;
mod error;
mod gpu;
mod model;
mod ui;

use crate::app::App;

fn main() -> eframe::Result<()> {
    // FFmpeg's registration is idempotent; calling it before any window
    // exists means a broken FFmpeg build fails loudly at startup instead of
    // half-way through the first frame.
    //
    // `ffmpeg_next::Error` and `eframe::Error` are unrelated types, so the
    // failure is carried in the variant eframe reserves for user code.
    if let Err(e) = ffmpeg_next::init() {
        eprintln!("reaction-player: FFmpeg could not be initialised: {e}");
        return Err(eframe::Error::AppCreation(Box::new(e)));
    }

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("reaction-player")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([960.0, 600.0])
            .with_app_id("reaction-player"),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };

    eframe::run_native(
        "reaction-player",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}
