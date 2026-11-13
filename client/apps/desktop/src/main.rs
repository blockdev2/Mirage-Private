//! Mirage native desktop client.
//!
//! A Windows window built with egui. It talks to the local `ts6-bridge`
//! WebSocket service, which owns the TeamSpeak 6 session.

// Hide the console window in release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod bridge;
mod clock;
mod instance;
mod protocol;
mod store;
mod strings;
mod theme;

use eframe::egui;

fn main() -> eframe::Result {
    let new_instance = std::env::args().any(|arg| arg == instance::NEW_INSTANCE_FLAG);
    let instance = match instance::acquire(new_instance) {
        instance::Outcome::Run(instance) => instance,
        instance::Outcome::AlreadyRunning => {
            instance::focus_existing();
            return Ok(());
        }
        instance::Outcome::TooMany => return Ok(()),
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(instance::window_title(instance.slot))
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([1000.0, 640.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "Mirage",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
    // Held until here: the lock belongs to the window's whole life.
    .inspect(|_| drop(instance))
}
