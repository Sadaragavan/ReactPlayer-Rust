//! Entry point. All application code lives in the `reaction_player` library so
//! that `cargo test` can exercise it.

fn main() -> eframe::Result {
    reaction_player::app::run()
}
