//! Local wall-clock time, for chat message stamps.

/// Current local time as hour and minute.
#[cfg(windows)]
pub fn local_hour_minute() -> (u8, u8) {
    // Mirrors the Win32 SYSTEMTIME structure.
    #[repr(C)]
    #[derive(Default)]
    struct SystemTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        milliseconds: u16,
    }

    extern "system" {
        fn GetLocalTime(time: *mut SystemTime);
    }

    let mut now = SystemTime::default();
    // SAFETY: `GetLocalTime` only writes a SYSTEMTIME through the pointer, and
    // `now` is a valid, writable value of exactly that layout.
    unsafe { GetLocalTime(&mut now) };
    (now.hour.min(23) as u8, now.minute.min(59) as u8)
}

/// Without the Windows API the stamp falls back to UTC.
#[cfg(not(windows))]
pub fn local_hour_minute() -> (u8, u8) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    (((secs / 3600) % 24) as u8, ((secs / 60) % 60) as u8)
}

#[cfg(test)]
mod tests {
    use super::local_hour_minute;

    #[test]
    fn the_time_is_a_valid_hour_and_minute() {
        let (hour, minute) = local_hour_minute();
        assert!(hour < 24 && minute < 60);
    }
}
