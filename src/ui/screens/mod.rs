//! Screens and panels. Each submodule is one visible region of the app; all of
//! them take `&mut App` and draw into an egui region.

pub mod alignment;
pub mod banner;
pub mod canvas;
pub mod canvas_tools;
pub mod context_menu;
pub mod home;
pub mod layers;
pub mod modals;
pub mod new_session;
pub mod settings;
pub mod toasts;
pub mod top_bar;
pub mod transport;
