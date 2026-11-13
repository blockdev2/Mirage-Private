//! In-app updates.
//!
//! A worker thread owns everything slow (the network, the download) and the
//! window only reads a small state and sends commands, so the interface never
//! waits. The real backend is Velopack, which exists only for a copy that the
//! installer put on this computer; a portable or development copy reports
//! [`UpdateState::Unavailable`] and keeps working as before.
//!
//! The update source is the public repository's releases. `MIRAGE_UPDATE_URL`
//! overrides it with another repository URL or a local folder, which is how the
//! whole flow is tested without the network. No credential is ever involved.

use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};

/// Public releases of Mirage.
pub const DEFAULT_SOURCE: &str = "https://github.com/3d2faebf4bca96cacf2aa10dc002bf42/Mirage";
/// Overrides [`DEFAULT_SOURCE`]: a repository URL or a folder holding a release.
pub const SOURCE_ENV: &str = "MIRAGE_UPDATE_URL";

/// What the window shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateState {
    /// This copy was not installed by the installer, so it cannot update itself.
    Unavailable,
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    Downloading {
        percent: u8,
    },
    /// Downloaded; the next step restarts into it.
    Ready(Release),
    /// The updater is relaunching the app: the window should close now.
    Restarting,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    /// Release notes as plain markdown, possibly empty.
    pub notes: String,
    pub size_bytes: u64,
}

enum Command {
    Check,
    Download,
    Apply,
}

/// What an update backend must do. Kept small so tests can script it.
pub trait Backend: Send + 'static {
    type Update: Send + 'static;
    fn check(&mut self) -> Result<Option<(Self::Update, Release)>, String>;
    fn download(
        &mut self,
        update: &Self::Update,
        progress: Arc<dyn Fn(u8) + Send + Sync>,
    ) -> Result<(), String>;
    /// Hands over to the updater, which waits for this process to exit.
    fn apply(&mut self, update: &Self::Update) -> Result<(), String>;
}

/// The handle the window keeps.
pub struct Updater {
    state: Arc<Mutex<UpdateState>>,
    commands: Option<Sender<Command>>,
    current_version: String,
}

impl Updater {
    /// The updater for this copy of the app. `notify` is called whenever the
    /// state changes, so the window can repaint.
    pub fn start(notify: impl Fn() + Send + Sync + 'static) -> Self {
        let source = std::env::var(SOURCE_ENV).unwrap_or_else(|_| DEFAULT_SOURCE.to_string());
        #[cfg(debug_assertions)]
        if let Ok(mode) = std::env::var("MIRAGE_DEV_UPDATE") {
            // Debug builds only: a scripted updater to look at every state.
            return Self::with_backend(dev::Scripted::new(&mode), "0.1.0".into(), notify);
        }
        match velopack_backend::open(&source) {
            Ok((backend, version)) => Self::with_backend(backend, version, notify),
            Err(_) => Self {
                state: Arc::new(Mutex::new(UpdateState::Unavailable)),
                commands: None,
                current_version: env!("CARGO_PKG_VERSION").to_string(),
            },
        }
    }

    pub fn with_backend<B: Backend>(
        mut backend: B,
        current_version: String,
        notify: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let state = Arc::new(Mutex::new(UpdateState::Idle));
        let (commands, inbox) = channel::<Command>();
        let notify: Arc<dyn Fn() + Send + Sync> = Arc::new(notify);
        let shared = Arc::clone(&state);
        std::thread::spawn(move || {
            let set = |next: UpdateState| {
                *shared.lock().unwrap_or_else(|e| e.into_inner()) = next;
                notify();
            };
            let mut found: Option<(B::Update, Release)> = None;
            let mut downloaded = false;
            while let Ok(command) = inbox.recv() {
                match command {
                    Command::Check => {
                        set(UpdateState::Checking);
                        downloaded = false;
                        match backend.check() {
                            Ok(Some((update, release))) => {
                                set(UpdateState::Available(release.clone()));
                                found = Some((update, release));
                            }
                            Ok(None) => {
                                found = None;
                                set(UpdateState::UpToDate);
                            }
                            Err(error) => {
                                found = None;
                                set(UpdateState::Failed(error));
                            }
                        }
                    }
                    Command::Download => {
                        let Some((update, release)) = &found else {
                            continue;
                        };
                        set(UpdateState::Downloading { percent: 0 });
                        let progress_state = Arc::clone(&shared);
                        let progress_notify = Arc::clone(&notify);
                        let progress: Arc<dyn Fn(u8) + Send + Sync> = Arc::new(move |percent| {
                            *progress_state.lock().unwrap_or_else(|e| e.into_inner()) =
                                UpdateState::Downloading {
                                    percent: percent.min(100),
                                };
                            progress_notify();
                        });
                        match backend.download(update, progress) {
                            Ok(()) => {
                                downloaded = true;
                                set(UpdateState::Ready(release.clone()));
                            }
                            // The installed version is untouched; offer the
                            // update again.
                            Err(error) => {
                                set(UpdateState::Failed(error));
                            }
                        }
                    }
                    Command::Apply => {
                        let Some((update, _)) = &found else {
                            continue;
                        };
                        if !downloaded {
                            continue;
                        }
                        match backend.apply(update) {
                            Ok(()) => set(UpdateState::Restarting),
                            Err(error) => set(UpdateState::Failed(error)),
                        }
                    }
                }
            }
        });
        Self {
            state,
            commands: Some(commands),
            current_version,
        }
    }

    pub fn state(&self) -> UpdateState {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn current_version(&self) -> &str {
        &self.current_version
    }

    /// Looks for a newer release. Ignored while one is running or not possible.
    pub fn check(&self) {
        if !matches!(
            self.state(),
            UpdateState::Checking | UpdateState::Downloading { .. } | UpdateState::Restarting
        ) {
            self.send(Command::Check);
        }
    }

    pub fn download(&self) {
        if matches!(self.state(), UpdateState::Available(_)) {
            self.send(Command::Download);
        }
    }

    pub fn apply(&self) {
        if matches!(self.state(), UpdateState::Ready(_)) {
            self.send(Command::Apply);
        }
    }

    fn send(&self, command: Command) {
        if let Some(commands) = &self.commands {
            let _ = commands.send(command);
        }
    }
}

/// A scripted backend for photographing the updates page (debug builds).
#[cfg(debug_assertions)]
mod dev {
    use super::{Backend, Release};
    use std::sync::Arc;
    use std::time::Duration;

    pub struct Scripted(String);

    impl Scripted {
        pub fn new(mode: &str) -> Self {
            Self(mode.to_string())
        }
    }

    impl Backend for Scripted {
        type Update = ();

        fn check(&mut self) -> Result<Option<((), Release)>, String> {
            std::thread::sleep(Duration::from_millis(600));
            match self.0.as_str() {
                "checking" => loop {
                    std::thread::sleep(Duration::from_secs(60));
                },
                "uptodate" => Ok(None),
                "failed" => Err("Sem conexão com o GitHub.".to_string()),
                _ => Ok(Some((
                    (),
                    Release {
                        version: "0.2.0".to_string(),
                        notes: "Atualizações dentro do app e um menu de ajustes novo.".to_string(),
                        size_bytes: 14_300_000,
                    },
                ))),
            }
        }

        fn download(
            &mut self,
            _: &(),
            progress: Arc<dyn Fn(u8) + Send + Sync>,
        ) -> Result<(), String> {
            for percent in (0..=100).step_by(5) {
                progress(percent);
                if self.0 == "downloading" && percent == 45 {
                    loop {
                        std::thread::sleep(Duration::from_secs(60));
                    }
                }
                std::thread::sleep(Duration::from_millis(150));
            }
            Ok(())
        }

        fn apply(&mut self, _: &()) -> Result<(), String> {
            Ok(())
        }
    }
}

mod velopack_backend {
    use super::{Backend, Release};
    use std::sync::mpsc;
    use std::sync::Arc;
    use velopack::{sources, UpdateCheck, UpdateInfo, UpdateManager};

    pub struct Real {
        manager: UpdateManager,
    }

    /// A GitHub repository URL or an existing folder; anything else is refused
    /// before the library gets a chance to panic on it.
    fn source(text: &str) -> Result<Box<dyn velopack::sources::UpdateSource>, String> {
        let text = text.trim();
        if std::path::Path::new(text).is_dir() {
            return Ok(Box::new(sources::FileSource::new(text)));
        }
        if text.starts_with("https://github.com/") && text.len() < 200 {
            return Ok(Box::new(sources::GithubSource::new(text, None, false)));
        }
        Err("the update source is not a GitHub repository or a folder".to_string())
    }

    /// Fails when this copy was not installed by the installer.
    pub fn open(text: &str) -> Result<(Real, String), String> {
        let manager = UpdateManager::new_boxed(source(text)?, None, None)
            .map_err(|_| "not installed".to_string())?;
        let version = manager.get_current_version_as_string();
        Ok((Real { manager }, version))
    }

    impl Backend for Real {
        type Update = UpdateInfo;

        fn check(&mut self) -> Result<Option<(UpdateInfo, Release)>, String> {
            match self.manager.check_for_updates() {
                Ok(UpdateCheck::UpdateAvailable(info)) => {
                    let release = Release {
                        version: info.TargetFullRelease.Version.clone(),
                        notes: info.TargetFullRelease.NotesMarkdown.clone(),
                        size_bytes: info.TargetFullRelease.Size,
                    };
                    Ok(Some((*info, release)))
                }
                Ok(_) => Ok(None),
                Err(error) => Err(short(&error.to_string())),
            }
        }

        fn download(
            &mut self,
            update: &UpdateInfo,
            progress: Arc<dyn Fn(u8) + Send + Sync>,
        ) -> Result<(), String> {
            let (sender, receiver) = mpsc::channel::<i16>();
            let forward = std::thread::spawn(move || {
                while let Ok(percent) = receiver.recv() {
                    progress(percent.clamp(0, 100) as u8);
                }
            });
            let result = self
                .manager
                .download_updates(update, Some(sender))
                .map_err(|error| short(&error.to_string()));
            // The sender went into the call and is dropped with it, so the
            // forwarding thread ends by itself.
            let _ = forward.join();
            result
        }

        fn apply(&mut self, update: &UpdateInfo) -> Result<(), String> {
            self.manager
                .wait_exit_then_apply_updates(update, false, true, Vec::<String>::new())
                .map_err(|error| short(&error.to_string()))
        }
    }

    /// One readable line: library errors can carry long paths and URLs.
    fn short(text: &str) -> String {
        let line = text.lines().next().unwrap_or("").trim();
        line.chars().take(160).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Waits until the predicate holds for the state, or fails the test.
    fn wait_for(updater: &Updater, ok: impl Fn(&UpdateState) -> bool) -> UpdateState {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let state = updater.state();
            if ok(&state) {
                return state;
            }
            assert!(Instant::now() < deadline, "stuck in {state:?}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn release(version: &str) -> Release {
        Release {
            version: version.into(),
            notes: "Notas".into(),
            size_bytes: 1_000,
        }
    }

    /// Scripted answers, one per call.
    struct Script {
        checks: Vec<Result<Option<Release>, String>>,
        download: Result<(), String>,
        applied: Arc<Mutex<u32>>,
    }

    impl Backend for Script {
        type Update = Release;
        fn check(&mut self) -> Result<Option<(Release, Release)>, String> {
            self.checks
                .remove(0)
                .map(|found| found.map(|release| (release.clone(), release)))
        }
        fn download(
            &mut self,
            _: &Release,
            progress: Arc<dyn Fn(u8) + Send + Sync>,
        ) -> Result<(), String> {
            progress(40);
            progress(100);
            self.download.clone()
        }
        fn apply(&mut self, _: &Release) -> Result<(), String> {
            *self.applied.lock().unwrap() += 1;
            Ok(())
        }
    }

    fn updater(
        checks: Vec<Result<Option<Release>, String>>,
        download: Result<(), String>,
    ) -> (Updater, Arc<Mutex<u32>>) {
        let applied = Arc::new(Mutex::new(0));
        let backend = Script {
            checks,
            download,
            applied: Arc::clone(&applied),
        };
        (
            Updater::with_backend(backend, "0.1.0".into(), || {}),
            applied,
        )
    }

    #[test]
    fn a_new_release_is_found_downloaded_and_applied_in_order() {
        let (updater, applied) = updater(vec![Ok(Some(release("0.2.0")))], Ok(()));
        assert_eq!(updater.state(), UpdateState::Idle);
        updater.check();
        wait_for(&updater, |s| matches!(s, UpdateState::Available(_)));
        updater.download();
        wait_for(&updater, |s| matches!(s, UpdateState::Ready(_)));
        updater.apply();
        wait_for(&updater, |s| *s == UpdateState::Restarting);
        assert_eq!(*applied.lock().unwrap(), 1);
    }

    #[test]
    fn nothing_is_applied_before_it_is_downloaded() {
        let (updater, applied) = updater(vec![Ok(Some(release("0.2.0")))], Ok(()));
        updater.check();
        wait_for(&updater, |s| matches!(s, UpdateState::Available(_)));
        updater.apply();
        updater.download();
        wait_for(&updater, |s| matches!(s, UpdateState::Ready(_)));
        assert_eq!(*applied.lock().unwrap(), 0);
    }

    #[test]
    fn being_current_and_failing_are_reported_and_a_retry_works() {
        let (updater, _) = updater(vec![Err("sem rede".into()), Ok(None)], Ok(()));
        updater.check();
        assert_eq!(
            wait_for(&updater, |s| matches!(s, UpdateState::Failed(_))),
            UpdateState::Failed("sem rede".into())
        );
        updater.check();
        wait_for(&updater, |s| *s == UpdateState::UpToDate);
    }

    #[test]
    fn a_failed_download_leaves_the_update_available_to_retry() {
        let (updater, applied) = updater(
            vec![Ok(Some(release("0.2.0")))],
            Err("download interrompido".into()),
        );
        updater.check();
        wait_for(&updater, |s| matches!(s, UpdateState::Available(_)));
        updater.download();
        wait_for(&updater, |s| matches!(s, UpdateState::Failed(_)));
        updater.apply();
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(*applied.lock().unwrap(), 0);
    }

    #[test]
    fn a_copy_that_was_not_installed_cannot_update() {
        // Tests run from a build folder, never from an installed location.
        let updater = Updater::start(|| {});
        assert_eq!(updater.state(), UpdateState::Unavailable);
        updater.check();
        updater.download();
        updater.apply();
        assert_eq!(updater.state(), UpdateState::Unavailable);
    }
}
