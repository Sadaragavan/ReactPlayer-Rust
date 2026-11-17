//! The `eframe` application. One struct holds the session, the engine, the
//! store and every piece of transient UI state; screens are pure functions over
//! it.

use eframe::egui;

use crate::gpu::renderer::Compositor;
use crate::ui::theme;

/// Set up on the first frame and owned by `eframe` for the life of the window.
pub struct App {}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);
        if let Some(render_state) = cc.wgpu_render_state.as_ref() {
            Compositor::install(render_state);
        }
        Self {}
    }
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 1.0]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::BG))
            .show(ctx, |ui| {
                ui.add_space(80.0);
                ui.vertical_centered(|ui| {
                    ui.label(egui::RichText::new("Reaction Player").size(48.0));
                });
            });
    }
}

/// Open the window and run until it closes.
pub fn run() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Reaction Player")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([960.0, 600.0])
            .with_app_id("reaction-player"),
        renderer: eframe::Renderer::Wgpu,
        vsync: true,
        multisampling: 0,
        depth_buffer: 0,
        stencil_buffer: 0,
        ..Default::default()
    };
    eframe::run_native(
        "Reaction Player",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}
