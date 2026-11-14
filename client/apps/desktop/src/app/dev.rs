//! Debug-only test driver. `MIRAGE_DEV_PLAN` holds steps separated by `;` that
//! run one after another in the real window, so the interface can be
//! exercised and photographed without a person. Compiled out of release
//! builds.
//!
//! Steps: `wait:SECS`, `connect:HOST:PORT`, `chat:TEXT`, `attach:PATH`, `watch` (first other client),
//! `hover:X,Y`, `click:X,Y`, `rclick:X,Y`, `type:TEXT`, `key:Escape`, `shot:NAME`, `quit`.
//! `MIRAGE_DEV_OUT` is the folder the screenshots go to. Run it with an
//! isolated `APPDATA` so the real profile is untouched.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::App;

pub struct Plan {
    steps: VecDeque<String>,
    resume: Instant,
    out: PathBuf,
}

impl Plan {
    pub fn from_env() -> Option<Self> {
        let plan = std::env::var("MIRAGE_DEV_PLAN").ok()?;
        Some(Self {
            steps: plan
                .split(';')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect(),
            resume: Instant::now(),
            out: std::env::var_os("MIRAGE_DEV_OUT")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(".")),
        })
    }
}

fn point(arg: &str) -> Option<egui::Pos2> {
    let (x, y) = arg.split_once(',')?;
    Some(egui::pos2(x.trim().parse().ok()?, y.trim().parse().ok()?))
}

impl App {
    /// Runs the next step of the plan once its wait is over.
    pub(super) fn dev_step(&mut self, ctx: &egui::Context) {
        if self.dev_plan.is_none() {
            return;
        }
        // Before the first-run dialog is decided: a profile always exists.
        if self.store.data.profiles.is_empty() {
            self.store.add_profile("Mirage Dev", true);
        }
        let Some(plan) = self.dev_plan.as_mut() else {
            return;
        };
        ctx.request_repaint_after(Duration::from_millis(100));
        if Instant::now() < plan.resume {
            return;
        }
        let Some(step) = plan.steps.pop_front() else {
            return;
        };
        let (name, arg) = step.split_once(':').unwrap_or((step.as_str(), ""));
        let arg = arg.to_string();
        match name {
            "wait" => {
                plan.resume = Instant::now() + Duration::from_secs_f32(arg.parse().unwrap_or(1.0));
            }
            "connect" => {
                if self.socket != crate::bridge::SocketState::Open {
                    // The bridge is still starting: try this step again.
                    plan.steps.push_front(step);
                    return;
                }
                if self.store.data.profiles.is_empty() {
                    self.store.add_profile("Mirage Dev", true);
                }
                self.select_server(&arg);
                self.start_connect(&arg);
            }
            "chat" => {
                self.chat_draft = arg;
                self.submit_chat();
            }
            "attach" => self.attach_file(PathBuf::from(arg)),
            "token" => self.admin_send(
                super::admin::Purpose::Done("Chave usada."),
                "use_token",
                serde_json::json!({ "token": arg }),
            ),
            "admin" => self.open_admin_window(),
            "watch" => {
                let other = self
                    .snapshot
                    .as_ref()
                    .and_then(|s| s.clients.iter().find(|c| !c.is_self).map(|c| c.id));
                match other {
                    Some(id) => self.watch_stream(id),
                    // Nobody else is here yet: try again.
                    None => plan.steps.push_front(step),
                }
            }
            "hover" => {
                if let Some(pos) = point(&arg) {
                    self.dev_events.push(egui::Event::PointerMoved(pos));
                }
            }
            "click" => {
                if let Some(pos) = point(&arg) {
                    self.dev_events.push(egui::Event::PointerMoved(pos));
                    for pressed in [true, false] {
                        self.dev_events.push(egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        });
                    }
                }
            }
            "type" => self.dev_events.push(egui::Event::Text(arg)),
            "rclick" => {
                if let Some(pos) = point(&arg) {
                    self.dev_events.push(egui::Event::PointerMoved(pos));
                    for pressed in [true, false] {
                        self.dev_events.push(egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Secondary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        });
                    }
                }
            }
            "key" => {
                if arg == "Escape" {
                    for pressed in [true, false] {
                        self.dev_events.push(egui::Event::Key {
                            key: egui::Key::Escape,
                            physical_key: None,
                            pressed,
                            repeat: false,
                            modifiers: egui::Modifiers::NONE,
                        });
                    }
                }
            }
            "shot" => {
                let path = plan.out.join(format!("{arg}.png"));
                let _ = std::fs::create_dir_all(&plan.out);
                // The pixels arrive a frame later; leave time before the next step.
                plan.resume = Instant::now() + Duration::from_millis(600);
                ctx.request_screenshot(move |image| {
                    let pixels: Vec<u8> = image
                        .pixels
                        .iter()
                        .flat_map(|c| [c.r(), c.g(), c.b(), c.a()])
                        .collect();
                    if let Some(png) = image::RgbaImage::from_raw(
                        image.width() as u32,
                        image.height() as u32,
                        pixels,
                    ) {
                        let _ = png.save(path);
                    }
                });
            }
            "quit" => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            other => eprintln!("dev plan: unknown step `{other}`"),
        }
        ctx.request_repaint();
    }
}
