//! Small still pictures of a monitor or a window, for the share picker.
//!
//! GDI is enough here: one scaled copy per source, no encoder, no capture
//! session, and the picture never leaves this process.

use crate::config::CaptureSource;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject,
    GetDC, GetMonitorInfoW, ReleaseDC, SelectObject, SetStretchBltMode, StretchBlt, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HALFTONE, HGDIOBJ, HMONITOR, MONITORINFO, SRCCOPY,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
use windows_capture::monitor::Monitor;

/// `PW_RENDERFULLCONTENT`: also draws GPU-composed content (browsers, games).
const PW_RENDERFULLCONTENT: u32 = 2;

/// An RGBA picture, rows top to bottom.
pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// `source` scaled to fit `max_width`×`max_height`, or `None` when it cannot
/// be read (a closed or minimized window, a monitor that went away).
pub fn thumbnail(source: CaptureSource, max_width: u32, max_height: u32) -> Option<Thumbnail> {
    match source {
        CaptureSource::Monitor(index) => {
            let monitor = match index {
                Some(index) => Monitor::enumerate().ok()?.into_iter().nth(index)?,
                None => Monitor::primary().ok()?,
            };
            let mut info = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            // SAFETY: a live monitor handle and a sized out-struct.
            unsafe { GetMonitorInfoW(HMONITOR(monitor.as_raw_hmonitor()), &mut info) }
                .ok()
                .ok()?;
            grab(None, info.rcMonitor, max_width, max_height)
        }
        CaptureSource::Window(handle) => {
            let hwnd = HWND(handle as isize as *mut std::ffi::c_void);
            let mut rect = RECT::default();
            // SAFETY: an invalid handle only makes the call fail.
            unsafe { GetWindowRect(hwnd, &mut rect) }.ok()?;
            grab(Some(hwnd), rect, max_width, max_height)
        }
    }
}

/// Fits `size` inside the bounds, keeping the aspect ratio (at least 1×1).
pub fn fit(size: (u32, u32), max_width: u32, max_height: u32) -> (u32, u32) {
    let scale = f64::min(
        f64::from(max_width) / f64::from(size.0.max(1)),
        f64::from(max_height) / f64::from(size.1.max(1)),
    )
    .min(1.0);
    (
        ((f64::from(size.0) * scale).round() as u32).max(1),
        ((f64::from(size.1) * scale).round() as u32).max(1),
    )
}

/// Copies `rect` (screen coordinates, or the whole window when `window` is
/// set) into a scaled RGBA picture.
fn grab(window: Option<HWND>, rect: RECT, max_width: u32, max_height: u32) -> Option<Thumbnail> {
    let full = (
        u32::try_from(rect.right - rect.left).ok()?,
        u32::try_from(rect.bottom - rect.top).ok()?,
    );
    if full.0 == 0 || full.1 == 0 {
        return None;
    }
    let (width, height) = fit(full, max_width, max_height);
    // SAFETY: every GDI object made here is selected out and freed below, on
    // every path; the DIB pointer is read only while its bitmap is alive.
    unsafe {
        let screen = GetDC(None);
        let result = copy_scaled(screen, window, rect, full, (width, height));
        ReleaseDC(None, screen);
        result
    }
}

/// The GDI work of `grab`, so every object made here is freed on one path.
///
/// # Safety
/// `screen` must be a live screen DC.
unsafe fn copy_scaled(
    screen: windows::Win32::Graphics::Gdi::HDC,
    window: Option<HWND>,
    rect: RECT,
    full: (u32, u32),
    (width, height): (u32, u32),
) -> Option<Thumbnail> {
    // SAFETY: see the caller; every object made here is freed below.
    unsafe {
        {
            let source = CreateCompatibleDC(Some(screen));
            let target = CreateCompatibleDC(Some(screen));
            let source_bitmap = CreateCompatibleBitmap(screen, full.0 as i32, full.1 as i32);
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width as i32,
                    biHeight: -(height as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let target_bitmap =
                CreateDIBSection(Some(target), &info, DIB_RGB_COLORS, &mut bits, None, 0).ok();
            let mut picture = None;
            if let Some(target_bitmap) = target_bitmap {
                let old_source = SelectObject(source, HGDIOBJ(source_bitmap.0));
                let old_target = SelectObject(target, HGDIOBJ(target_bitmap.0));
                let copied = match window {
                    Some(hwnd) => {
                        PrintWindow(hwnd, source, PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT))
                            .as_bool()
                    }
                    None => BitBlt(
                        source,
                        0,
                        0,
                        full.0 as i32,
                        full.1 as i32,
                        Some(screen),
                        rect.left,
                        rect.top,
                        SRCCOPY,
                    )
                    .is_ok(),
                };
                if copied {
                    SetStretchBltMode(target, HALFTONE);
                    let scaled = StretchBlt(
                        target,
                        0,
                        0,
                        width as i32,
                        height as i32,
                        Some(source),
                        0,
                        0,
                        full.0 as i32,
                        full.1 as i32,
                        SRCCOPY,
                    )
                    .as_bool();
                    if scaled && !bits.is_null() {
                        let len = (width * height * 4) as usize;
                        let bgra = std::slice::from_raw_parts(bits.cast::<u8>(), len);
                        picture = Some(Thumbnail {
                            width,
                            height,
                            rgba: bgra_to_rgba(bgra),
                        });
                    }
                }
                SelectObject(source, old_source);
                SelectObject(target, old_target);
                let _ = DeleteObject(HGDIOBJ(target_bitmap.0));
            }
            let _ = DeleteObject(HGDIOBJ(source_bitmap.0));
            let _ = DeleteDC(source);
            let _ = DeleteDC(target);
            picture
        }
    }
}

/// GDI's BGRX to opaque RGBA.
fn bgra_to_rgba(bgra: &[u8]) -> Vec<u8> {
    bgra.as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| [p[2], p[1], p[0], 255])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_keeps_the_aspect_and_never_grows() {
        assert_eq!(fit((1920, 1080), 320, 180), (320, 180));
        assert_eq!(fit((1080, 1920), 320, 180), (101, 180));
        assert_eq!(fit((100, 50), 320, 180), (100, 50));
        assert_eq!(fit((0, 0), 320, 180), (1, 1));
    }

    #[test]
    fn colors_are_swapped_and_opaque() {
        assert_eq!(bgra_to_rgba(&[1, 2, 3, 0]), vec![3, 2, 1, 255]);
    }

    #[test]
    fn the_primary_monitor_has_a_picture() {
        // Headless CI has no monitor; where there is one, it must be readable.
        if crate::list_monitors().is_empty() {
            return;
        }
        let picture = thumbnail(CaptureSource::Monitor(None), 320, 180).expect("thumbnail");
        assert_eq!(
            picture.rgba.len(),
            (picture.width * picture.height * 4) as usize
        );
    }
}
