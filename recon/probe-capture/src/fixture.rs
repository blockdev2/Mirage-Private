//! Creates and controls only the probe's synthetic window.

use anyhow::{Result, bail};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use windows::{
    Win32::{
        Foundation::*, Graphics::Gdi::*, System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::*,
    },
    core::w,
};

static COUNTER: AtomicUsize = AtomicUsize::new(0);
static PAINT_FAILED: AtomicBool = AtomicBool::new(false);
const STYLE: WINDOW_STYLE = WINDOW_STYLE(WS_CAPTION.0 | WS_SYSMENU.0 | WS_OVERLAPPED.0);
const EX_STYLE: WINDOW_EX_STYLE = WS_EX_APPWINDOW;

unsafe extern "system" fn procedure(hwnd: HWND, message: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // SAFETY: Windows calls this procedure for the class registered below. All
    // GDI handles are scoped to this paint; no borrowed Rust pointer is retained.
    unsafe {
        match message {
            WM_TIMER => {
                if !InvalidateRect(Some(hwnd), None, false).as_bool() {
                    PAINT_FAILED.store(true, Ordering::Relaxed);
                }
                LRESULT(0)
            }
            WM_PAINT => {
                let mut paint = PAINTSTRUCT::default();
                let dc = BeginPaint(hwnd, &mut paint);
                let mut rect = RECT::default();
                let green = CreateSolidBrush(COLORREF(0x0050_b414));
                let black = CreateSolidBrush(COLORREF(0));
                let mut ok = GetClientRect(hwnd, &mut rect).is_ok();
                ok &= FillRect(dc, &rect, green) != 0;
                let left = (COUNTER.fetch_add(1, Ordering::Relaxed) % 25) as i32 * 8;
                let marker = RECT {
                    left,
                    top: 0,
                    right: left + 8,
                    bottom: 8,
                };
                ok &= FillRect(dc, &marker, black) != 0;
                ok &= DeleteObject(HGDIOBJ(green.0)).as_bool();
                ok &= DeleteObject(HGDIOBJ(black.0)).as_bool();
                ok &= EndPaint(hwnd, &paint).as_bool();
                if !ok {
                    PAINT_FAILED.store(true, Ordering::Relaxed);
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, message, wp, lp),
        }
    }
}

pub struct Fixture {
    hwnd: HWND,
}

fn outer_size(width: i32, height: i32) -> Result<(i32, i32)> {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: width,
        bottom: height,
    };
    // SAFETY: The output rectangle is live, and the style matches our own window.
    unsafe {
        AdjustWindowRectEx(&mut rect, STYLE, false, EX_STYLE)?;
    }
    Ok((rect.right - rect.left, rect.bottom - rect.top))
}

impl Fixture {
    pub fn create() -> Result<Self> {
        // SAFETY: Static UTF-16 strings and a static callback outlive the window.
        // Only this newly created HWND is ever captured or changed.
        unsafe {
            let instance = HINSTANCE(GetModuleHandleW(None)?.0);
            let class = WNDCLASSW {
                lpfnWndProc: Some(procedure),
                hInstance: instance,
                lpszClassName: w!("MirageNativeCaptureFixture"),
                ..Default::default()
            };
            if RegisterClassW(&class) == 0 {
                bail!("cannot register synthetic fixture window");
            }
            let (width, height) = outer_size(320, 180)?;
            let hwnd = CreateWindowExW(
                EX_STYLE,
                class.lpszClassName,
                w!("Mirage synthetic capture test"),
                STYLE,
                32,
                32,
                width,
                height,
                None,
                None,
                Some(instance),
                None,
            )?;
            let fixture = Self { hwnd };
            let _previous_visibility = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            if SetTimer(Some(hwnd), 1, 50, None) == 0 {
                bail!("cannot start fixture animation timer");
            }
            if !UpdateWindow(hwnd).as_bool() {
                bail!("cannot paint synthetic fixture");
            }
            Ok(fixture)
        }
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    pub fn resize(&self) -> Result<()> {
        let (width, height) = outer_size(480, 270)?;
        // SAFETY: Resize only the window created by this instance, without focus changes.
        unsafe {
            SetWindowPos(
                self.hwnd,
                None,
                0,
                0,
                width,
                height,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            )?;
        }
        Ok(())
    }

    pub fn pump(&self) -> Result<bool> {
        let mut message = MSG::default();
        // SAFETY: The message is stack allocated for the current thread's queue.
        unsafe {
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                if message.message == WM_QUIT {
                    return Ok(false);
                }
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        if PAINT_FAILED.load(Ordering::Relaxed) {
            bail!("synthetic fixture painting failed");
        }
        Ok(true)
    }

    pub fn close(mut self) -> Result<()> {
        // SAFETY: The handle belongs to this instance; invalidate it after destruction.
        unsafe {
            if IsWindow(Some(self.hwnd)).as_bool() {
                KillTimer(Some(self.hwnd), 1)?;
                DestroyWindow(self.hwnd)?;
            }
        }
        self.hwnd = HWND::default();
        Ok(())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // SAFETY: Fallback cleanup is restricted to our still-live owned HWND.
        unsafe {
            if !self.hwnd.0.is_null()
                && IsWindow(Some(self.hwnd)).as_bool()
                && DestroyWindow(self.hwnd).is_err()
            {
                eprintln!("synthetic fixture cleanup failed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires an interactive Windows desktop; creates only its own fixture"]
    fn synthetic_window_is_a_valid_capture_target() {
        let fixture = Fixture::create().expect("create owned fixture");
        let target = windows_capture::window::Window::from_raw_hwnd(fixture.hwnd().0);
        assert!(
            target.is_valid(),
            "fixture must satisfy capture window eligibility"
        );
        fixture.close().expect("close owned fixture");
    }
}
