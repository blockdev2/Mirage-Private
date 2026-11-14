//! The windows a user can share, for a picker.

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::UI::WindowsAndMessaging::IsIconic;
use windows_capture::window::Window;

/// One shareable window. `id` is what `CaptureSource::Window` takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    pub id: i64,
    pub title: String,
    /// Executable name, to tell apart windows with the same title.
    pub process: String,
    pub width: u32,
    pub height: u32,
}

/// Smallest window worth listing: tool tips and helper windows are smaller.
const MIN_SIDE: u32 = 120;

/// Windows that are on the desktop but hidden by the shell (suspended apps,
/// other virtual desktops) report `cloaked`; capture refuses them.
fn hidden(window: &Window) -> bool {
    let handle = HWND(window.as_raw_hwnd());
    let mut cloaked = 0u32;
    // SAFETY: `cloaked` is a valid u32 and the size passed is its size.
    let queried = unsafe {
        DwmGetWindowAttribute(
            handle,
            DWMWA_CLOAKED,
            std::ptr::addr_of_mut!(cloaked).cast(),
            std::mem::size_of::<u32>() as u32,
        )
    };
    // SAFETY: plain query on a window handle.
    queried.is_ok() && cloaked != 0 || unsafe { IsIconic(handle).as_bool() }
}

/// Visible top-level windows with a title, largest-first is left to the caller.
pub fn list_windows() -> Vec<WindowInfo> {
    let Ok(all) = Window::enumerate() else {
        return Vec::new();
    };
    all.iter()
        .filter(|window| !hidden(window))
        .filter_map(|window| {
            let title = window.title().ok()?.trim().to_owned();
            let (width, height) = (
                u32::try_from(window.width().ok()?).ok()?,
                u32::try_from(window.height().ok()?).ok()?,
            );
            // Minimized windows report a tiny or off-screen size and cannot be captured.
            if title.is_empty() || width < MIN_SIDE || height < MIN_SIDE {
                return None;
            }
            Some(WindowInfo {
                id: window.as_raw_hwnd() as isize as i64,
                title,
                process: window.process_name().unwrap_or_default(),
                width,
                height,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listed_windows_have_titles_and_sizes_and_unique_ids() {
        let windows = list_windows();
        let mut ids: Vec<i64> = windows.iter().map(|w| w.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), windows.len());
        for window in &windows {
            assert!(!window.title.is_empty());
            assert!(window.width >= MIN_SIDE && window.height >= MIN_SIDE);
        }
    }
}
