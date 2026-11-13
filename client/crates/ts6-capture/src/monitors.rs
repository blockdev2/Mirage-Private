//! The monitors a user can share, for a picker.

use windows_capture::monitor::Monitor;

/// One monitor. `index` is what `CaptureConfig::monitor` takes (zero-based).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorInfo {
    pub index: usize,
    /// Windows' friendly name, or the device name when it has none.
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

/// Every attached monitor, in the order `CaptureConfig::monitor` counts them.
pub fn list_monitors() -> Vec<MonitorInfo> {
    let primary = Monitor::primary().and_then(|m| m.device_name()).ok();
    let Ok(all) = Monitor::enumerate() else {
        return Vec::new();
    };
    all.iter()
        .enumerate()
        .filter_map(|(index, monitor)| {
            let device = monitor.device_name().ok()?;
            Some(MonitorInfo {
                index,
                name: monitor.name().unwrap_or_else(|_| device.clone()),
                width: monitor.width().ok()?,
                height: monitor.height().ok()?,
                primary: primary.as_deref() == Some(device.as_str()),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn at_most_one_monitor_is_primary_and_indexes_are_dense() {
        // Headless CI has no monitor: an empty list is fine, a bad one is not.
        let monitors = list_monitors();
        assert!(monitors.iter().filter(|m| m.primary).count() <= 1);
        for (position, monitor) in monitors.iter().enumerate() {
            assert_eq!(monitor.index, position);
            assert!(monitor.width > 0 && monitor.height > 0);
        }
    }
}
