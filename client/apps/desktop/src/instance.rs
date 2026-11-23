//! One Mirage per computer, unless the user asks for more.
//!
//! Two windows would share one identity (the server then rejects the second
//! as a clone) and one local bridge port, so the second window would end up
//! driving the first one's session. A second launch therefore brings the
//! running window forward and exits. `--new-instance` opens another window
//! that is fully separate: its own bridge port, profiles and identity.
//!
//! The guard is an exclusive lock on a file, which the system releases when
//! the process ends, however it ends. A lock that cannot be created never
//! blocks the app from starting.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const NEW_INSTANCE_FLAG: &str = "--new-instance";
/// The port an external `ts6-bridge` also uses by default.
const PRIMARY_PORT: u16 = 8787;
/// Windows opened with `--new-instance` beyond the first.
const MAX_EXTRA: u8 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// The normal window, using the default data and port.
    Primary,
    /// An `--new-instance` window with its own data (numbered from 1).
    Extra(u8),
}

pub enum Outcome {
    /// Start. Keep the value alive for as long as the window is open.
    Run(Instance),
    /// Another Mirage already runs; bring it forward and exit.
    AlreadyRunning,
    /// `--new-instance` with every extra slot taken.
    TooMany,
}

pub struct Instance {
    pub slot: Slot,
    _lock: Option<File>,
}

static PORT: OnceLock<u16> = OnceLock::new();

/// The loopback port of this window's bridge.
pub fn bridge_port() -> u16 {
    *PORT.get().unwrap_or(&PRIMARY_PORT)
}

pub fn window_title(slot: Slot) -> String {
    match slot {
        Slot::Primary => "Mirage".to_string(),
        Slot::Extra(n) => format!("Mirage ({})", u16::from(n) + 1),
    }
}

/// The environment variable the per-user data directory is derived from, and
/// its value. Mirrors where the store and the identity are kept.
fn config_base() -> Option<(&'static str, PathBuf)> {
    if let Some(dir) = std::env::var_os("APPDATA") {
        return Some(("APPDATA", PathBuf::from(dir)));
    }
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
        return Some(("XDG_CONFIG_HOME", PathBuf::from(dir)));
    }
    let home = std::env::var_os("HOME")?;
    Some(("XDG_CONFIG_HOME", PathBuf::from(home).join(".config")))
}

/// Claims this process's place. Must run before anything reads the data
/// directory, because an extra window moves it.
pub fn acquire(new_instance: bool) -> Outcome {
    let Some((variable, base)) = config_base() else {
        return Outcome::Run(Instance {
            slot: Slot::Primary,
            _lock: None,
        });
    };
    let root = base.join("Mirage");
    let outcome = acquire_in(&root, new_instance);
    if let Outcome::Run(instance) = &outcome {
        match instance.slot {
            Slot::Primary => {
                let _ = PORT.set(PRIMARY_PORT);
            }
            Slot::Extra(n) => {
                // The store and the identity follow this variable, so the new
                // window gets its own profiles and its own TeamSpeak identity.
                std::env::set_var(variable, slot_dir(&root, n));
                let _ = PORT.set(free_port().unwrap_or(PRIMARY_PORT + u16::from(n)));
            }
        }
    }
    outcome
}

fn slot_dir(root: &Path, n: u8) -> PathBuf {
    root.join("slots").join(n.to_string())
}

fn acquire_in(root: &Path, new_instance: bool) -> Outcome {
    let slots = std::iter::once(Slot::Primary).chain((1..=MAX_EXTRA).map(Slot::Extra));
    let mut primary_busy = false;
    for slot in slots {
        let dir = match slot {
            Slot::Primary => root.to_path_buf(),
            Slot::Extra(n) => slot_dir(root, n),
        };
        match lock_in(&dir) {
            Ok(Some(file)) => {
                if slot == Slot::Primary {
                    let _ = fs::write(dir.join("instance.pid"), std::process::id().to_string());
                }
                return Outcome::Run(Instance {
                    slot,
                    _lock: Some(file),
                });
            }
            Ok(None) if slot == Slot::Primary && !new_instance => return Outcome::AlreadyRunning,
            Ok(None) => primary_busy |= slot == Slot::Primary,
            // The lock cannot be made here: start rather than refuse.
            Err(_) => {
                return Outcome::Run(Instance {
                    slot: Slot::Primary,
                    _lock: None,
                })
            }
        }
    }
    debug_assert!(primary_busy);
    Outcome::TooMany
}

/// `Ok(Some)` when the lock is ours, `Ok(None)` when someone else holds it.
fn lock_in(dir: &Path) -> io::Result<Option<File>> {
    fs::create_dir_all(dir)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("instance.lock"))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(error)) => Err(error),
    }
}

/// A free loopback port. Another program could take it before the bridge
/// binds it; the window then simply fails to reach its bridge, as it would
/// with any busy port.
fn free_port() -> Option<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).ok()?;
    listener.local_addr().ok().map(|address| address.port())
}

/// Brings the window of the running primary Mirage forward. It looks the
/// window up by the owning process, not by title, so an unrelated window that
/// is also called "Mirage" is never touched. Does nothing if it cannot find it.
pub fn focus_existing() {
    #[cfg(windows)]
    {
        let Some((_, base)) = config_base() else {
            return;
        };
        let Ok(text) = fs::read_to_string(base.join("Mirage").join("instance.pid")) else {
            return;
        };
        if let Ok(pid) = text.trim().parse::<u32>() {
            windows::raise_window_of(pid);
        }
    }
}

#[cfg(windows)]
mod windows {
    use windows_sys::Win32::Foundation::{HWND, LPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
        SetForegroundWindow, ShowWindow, SW_RESTORE, SW_SHOW,
    };

    struct Search {
        pid: u32,
        found: HWND,
    }

    unsafe extern "system" fn visit(window: HWND, parameter: LPARAM) -> i32 {
        // SAFETY: `parameter` is the `Search` that `raise_window_of` passes
        // to the enumeration, alive for its whole duration.
        let search = unsafe { &mut *(parameter as *mut Search) };
        let mut pid = 0u32;
        // SAFETY: `window` is a handle the system just handed to this callback.
        unsafe { GetWindowThreadProcessId(window, &mut pid) };
        if pid == search.pid && (unsafe { IsWindowVisible(window) } != 0 || is_main(window)) {
            search.found = window;
            return 0;
        }
        1
    }

    /// The main window, also while it is hidden next to the clock.
    fn is_main(window: HWND) -> bool {
        let mut text = [0u16; 16];
        // SAFETY: the buffer outlives the call and its length is passed.
        let n = unsafe { GetWindowTextW(window, text.as_mut_ptr(), text.len() as i32) };
        String::from_utf16_lossy(&text[..n.max(0) as usize]) == "Mirage"
    }

    pub fn raise_window_of(pid: u32) {
        let mut search = Search {
            pid,
            found: std::ptr::null_mut(),
        };
        // SAFETY: the callback only reads window handles and writes `search`.
        unsafe { EnumWindows(Some(visit), &mut search as *mut Search as LPARAM) };
        if search.found.is_null() {
            return;
        }
        // SAFETY: `found` was returned by the enumeration a moment ago.
        unsafe {
            ShowWindow(search.found, SW_SHOW);
            if IsIconic(search.found) != 0 {
                ShowWindow(search.found, SW_RESTORE);
            }
            SetForegroundWindow(search.found);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!("mirage-instance-{}-{n}", std::process::id()))
    }

    fn slot_of(outcome: &Outcome) -> Option<Slot> {
        match outcome {
            Outcome::Run(instance) => Some(instance.slot),
            _ => None,
        }
    }

    #[test]
    fn a_second_launch_is_refused_until_the_first_ends() {
        let root = scratch();
        let first = acquire_in(&root, false);
        assert_eq!(slot_of(&first), Some(Slot::Primary));
        assert!(matches!(acquire_in(&root, false), Outcome::AlreadyRunning));
        drop(first);
        assert_eq!(slot_of(&acquire_in(&root, false)), Some(Slot::Primary));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn new_instance_takes_the_next_free_slot_and_slots_are_reused() {
        let root = scratch();
        let primary = acquire_in(&root, false);
        let second = acquire_in(&root, true);
        let third = acquire_in(&root, true);
        assert_eq!(slot_of(&second), Some(Slot::Extra(1)));
        assert_eq!(slot_of(&third), Some(Slot::Extra(2)));
        drop(second);
        assert_eq!(slot_of(&acquire_in(&root, true)), Some(Slot::Extra(1)));
        drop((primary, third));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn new_instance_with_nothing_running_is_the_primary() {
        let root = scratch();
        assert_eq!(slot_of(&acquire_in(&root, true)), Some(Slot::Primary));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn extra_windows_are_capped() {
        let root = scratch();
        let held: Vec<_> = (0..=MAX_EXTRA).map(|_| acquire_in(&root, true)).collect();
        assert!(held.iter().all(|outcome| slot_of(outcome).is_some()));
        assert!(matches!(acquire_in(&root, true), Outcome::TooMany));
        drop(held);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn extra_windows_are_numbered_for_people() {
        assert_eq!(window_title(Slot::Primary), "Mirage");
        assert_eq!(window_title(Slot::Extra(1)), "Mirage (2)");
    }

    #[test]
    fn a_free_port_is_a_real_port() {
        assert!(free_port().is_some_and(|port| port != 0));
    }
}
