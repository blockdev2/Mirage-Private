//! Mirage native desktop client.
//!
//! A Windows window built with egui. It talks to the local `ts6-bridge`
//! WebSocket service, which owns the TeamSpeak 6 session.

// Hide the console window in release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod bridge;
mod clock;
mod protocol;
mod store;
mod strings;
mod theme;

use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Mirage")
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([1000.0, 640.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Mirage",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}
