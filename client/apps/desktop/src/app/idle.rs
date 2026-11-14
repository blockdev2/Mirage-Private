//! Going away by itself after a while without using the computer, and coming
//! back when the person does. The decision is a plain function so it can be
//! tested; only reading the system's idle time is platform specific.

use std::time::Duration;

/// How often the idle time is looked at.
pub const CHECK_EVERY: Duration = Duration::from_secs(5);

/// Choices offered in the settings, in minutes (0 is off).
pub const CHOICES: [u32; 5] = [0, 5, 10, 15, 30];

/// How long since the last keyboard or mouse input anywhere on the computer.
#[cfg(windows)]
pub fn idle_time() -> Option<Duration> {
    use windows_sys::Win32::System::SystemInformation::GetTickCount;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    let mut info = LASTINPUTINFO {
        cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    // SAFETY: `info` is a valid, initialised LASTINPUTINFO with its size set.
    if unsafe { GetLastInputInfo(&mut info) } == 0 {
        return None;
    }
    // SAFETY: GetTickCount takes no arguments and has no preconditions.
    let now = unsafe { GetTickCount() };
    Some(elapsed(now, info.dwTime))
}

#[cfg(not(windows))]
pub fn idle_time() -> Option<Duration> {
    None
}

/// Milliseconds between two tick counts, which wrap around every 49 days.
fn elapsed(now: u32, last: u32) -> Duration {
    Duration::from_millis(u64::from(now.wrapping_sub(last)))
}

/// What to do about the away status.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Nothing,
    GoAway,
    Return,
}

/// `limit_minutes` 0 means off. `away` is the current status and `automatic`
/// whether this program set it (a person who chose it keeps it).
pub fn decide(idle: Duration, limit_minutes: u32, away: bool, automatic: bool) -> Action {
    let limit = Duration::from_secs(u64::from(limit_minutes) * 60);
    match (away, automatic) {
        // Ours: undo it when the person is back, or the option was turned off.
        (true, true) if limit_minutes == 0 || idle < limit => Action::Return,
        (true, _) => Action::Nothing,
        (false, _) if limit_minutes > 0 && idle >= limit => Action::GoAway,
        (false, _) => Action::Nothing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: Duration = Duration::from_secs(60);

    #[test]
    fn it_goes_away_after_the_limit_and_only_when_enabled() {
        assert_eq!(decide(10 * MIN, 10, false, false), Action::GoAway);
        assert_eq!(decide(9 * MIN, 10, false, false), Action::Nothing);
        assert_eq!(decide(999 * MIN, 0, false, false), Action::Nothing);
    }

    #[test]
    fn it_returns_only_from_an_away_it_set_itself() {
        assert_eq!(
            decide(Duration::from_secs(2), 10, true, true),
            Action::Return
        );
        // A status the person chose stays, whatever they do.
        assert_eq!(
            decide(Duration::from_secs(2), 10, true, false),
            Action::Nothing
        );
        // Still idle: stay away.
        assert_eq!(decide(20 * MIN, 10, true, true), Action::Nothing);
        // The option was turned off while away automatically.
        assert_eq!(decide(20 * MIN, 0, true, true), Action::Return);
    }

    #[test]
    fn the_tick_count_wrapping_around_is_handled() {
        assert_eq!(elapsed(1_500, 1_000), Duration::from_millis(500));
        assert_eq!(elapsed(200, u32::MAX - 99), Duration::from_millis(300));
    }
}
