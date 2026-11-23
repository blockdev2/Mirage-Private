//! Closing the window hides it to an icon next to the clock instead of
//! quitting, like Discord: the call goes on. Clicking the icon brings the
//! window back; its right-click menu has "Abrir Mirage" and "Fechar Mirage",
//! the only way to quit while the option is on.
//!
//! The icon lives on its own thread with its own (message-only) window, so it
//! answers while the egui window is hidden and not drawing. It shows and hides
//! the egui window directly by handle; egui is not told, which is fine because
//! a hidden window just stops getting frames.

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::OnceLock;

/// The egui window's handle, set once the icon exists.
static MAIN: AtomicIsize = AtomicIsize::new(0);
/// "Fechar Mirage" was picked: let the next close through.
static QUITTING: AtomicBool = AtomicBool::new(false);
/// "Procurar atualizações" was picked; the app opens that page and looks.
static CHECK_UPDATES: AtomicBool = AtomicBool::new(false);
static CONTEXT: OnceLock<egui::Context> = OnceLock::new();
/// The icon's own window, to take the icon away when the app quits.
static ICON_WINDOW: AtomicIsize = AtomicIsize::new(0);

/// Adds the icon once; `window` is the egui window's handle.
/// The menu's words, in the app's language.
#[derive(Clone, Copy)]
pub struct Labels {
    pub open: &'static str,
    pub updates: &'static str,
    pub quit: &'static str,
}

pub fn start(window: isize, ctx: &egui::Context, labels: Labels) {
    if CONTEXT.set(ctx.clone()).is_err() {
        return;
    }
    MAIN.store(window, Ordering::SeqCst);
    #[cfg(windows)]
    std::thread::Builder::new()
        .name("tray".into())
        .spawn(move || win::run(labels))
        .ok();
    #[cfg(not(windows))]
    let _ = labels;
}

/// True once after "Procurar atualizações" was picked.
pub fn take_update_request() -> bool {
    CHECK_UPDATES.swap(false, Ordering::SeqCst)
}

/// Hides the window to the icon. False when there is no icon or the user is
/// quitting from the menu, so the close should go ahead.
pub fn hide() -> bool {
    let window = MAIN.load(Ordering::SeqCst);
    if window == 0 || QUITTING.load(Ordering::SeqCst) {
        return false;
    }
    #[cfg(windows)]
    win::show(window, false);
    cfg!(windows)
}

/// Takes the icon away; otherwise it lingers until the mouse passes over it.
pub fn remove() {
    #[cfg(windows)]
    win::remove(ICON_WINDOW.swap(0, Ordering::SeqCst));
}

#[cfg(windows)]
mod win {
    use super::{Labels, CHECK_UPDATES, CONTEXT, ICON_WINDOW, MAIN, QUITTING};
    use std::sync::atomic::Ordering;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::Shell::{
        Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
        DispatchMessageW, GetCursorPos, GetMessageW, IsIconic, LoadIconW, LoadImageW, PostMessageW,
        RegisterClassW, SetForegroundWindow, SetMenuItemInfoW, ShowWindow, TrackPopupMenu,
        TranslateMessage, HWND_MESSAGE, IDI_APPLICATION, IMAGE_ICON, LR_DEFAULTSIZE, MENUITEMINFOW,
        MF_GRAYED, MF_SEPARATOR, MF_STRING, MIIM_BITMAP, MSG, SW_HIDE, SW_RESTORE, SW_SHOW,
        TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_APP, WM_CLOSE, WM_LBUTTONUP, WM_RBUTTONUP, WNDCLASSW,
    };

    const CALLBACK: u32 = WM_APP + 1;
    const OPEN: usize = 1;
    const QUIT: usize = 2;
    const UPDATES: usize = 3;
    const TITLE: usize = 4;

    static MENU: std::sync::OnceLock<Labels> = std::sync::OnceLock::new();
    /// The app's icon beside the menu's title (an HBITMAP; 0 without one).
    static BADGE: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(Some(0)).collect()
    }

    pub fn show(window: isize, visible: bool) {
        let window = window as HWND;
        // SAFETY: `window` is the egui window, alive while the app runs.
        unsafe {
            if visible {
                ShowWindow(window, SW_SHOW);
                if IsIconic(window) != 0 {
                    ShowWindow(window, SW_RESTORE);
                }
                SetForegroundWindow(window);
            } else {
                ShowWindow(window, SW_HIDE);
            }
        }
        if let (true, Some(ctx)) = (visible, CONTEXT.get()) {
            ctx.request_repaint();
        }
    }

    unsafe extern "system" fn proc(
        window: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if msg != CALLBACK {
            // SAFETY: default handling for a window this thread owns.
            return unsafe { DefWindowProcW(window, msg, wparam, lparam) };
        }
        let main = MAIN.load(Ordering::SeqCst);
        match lparam as u32 {
            WM_LBUTTONUP => show(main, true),
            WM_RBUTTONUP => {
                let Some(labels) = MENU.get().copied() else {
                    return 0;
                };
                let (open, updates, quit) =
                    (wide(labels.open), wide(labels.updates), wide(labels.quit));
                // SAFETY: plain menu calls on a menu created and destroyed here;
                // the foreground call lets the menu close when clicking away.
                let picked = unsafe {
                    let menu = CreatePopupMenu();
                    // Like Discord's: the app's name as a quiet header, then
                    // the actions, quitting set apart at the bottom.
                    let title = wide(concat!("Mirage ", env!("CARGO_PKG_VERSION")));
                    AppendMenuW(menu, MF_STRING | MF_GRAYED, TITLE, title.as_ptr());
                    let badge = BADGE.load(Ordering::SeqCst);
                    if badge != 0 {
                        let info = MENUITEMINFOW {
                            cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
                            fMask: MIIM_BITMAP,
                            hbmpItem: badge as _,
                            ..std::mem::zeroed()
                        };
                        SetMenuItemInfoW(menu, TITLE as u32, 0, &info);
                    }
                    AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
                    AppendMenuW(menu, MF_STRING, OPEN, open.as_ptr());
                    AppendMenuW(menu, MF_STRING, UPDATES, updates.as_ptr());
                    AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
                    AppendMenuW(menu, MF_STRING, QUIT, quit.as_ptr());
                    let mut at = POINT { x: 0, y: 0 };
                    GetCursorPos(&mut at);
                    SetForegroundWindow(window);
                    let picked = TrackPopupMenu(
                        menu,
                        TPM_RETURNCMD | TPM_RIGHTBUTTON,
                        at.x,
                        at.y,
                        0,
                        window,
                        std::ptr::null(),
                    );
                    DestroyMenu(menu);
                    picked as usize
                };
                match picked {
                    OPEN => show(main, true),
                    UPDATES => {
                        CHECK_UPDATES.store(true, Ordering::SeqCst);
                        show(main, true);
                    }
                    QUIT => {
                        QUITTING.store(true, Ordering::SeqCst);
                        // Shown first: a hidden window may not run the frame
                        // that handles the close.
                        show(main, true);
                        // SAFETY: posting to the egui window, alive.
                        unsafe { PostMessageW(main as HWND, WM_CLOSE, 0, 0) };
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        0
    }

    pub fn remove(window: isize) {
        if window == 0 {
            return;
        }
        // SAFETY: a zeroed struct naming the icon added in `run`.
        unsafe {
            let mut data: NOTIFYICONDATAW = std::mem::zeroed();
            data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            data.hWnd = window as HWND;
            data.uID = 1;
            Shell_NotifyIconW(NIM_DELETE, &data);
        }
    }

    /// Dark context menus to match the app. Windows only offers this through
    /// two unnamed uxtheme.dll exports (135 SetPreferredAppMode, 136
    /// FlushMenuThemes), used by Explorer and most apps since Windows 10 1903;
    /// where they are missing the menu stays light.
    fn dark_menus() {
        use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
        const FORCE_DARK: i32 = 2;
        let name = wide("uxtheme.dll");
        // SAFETY: the exports are looked up by ordinal and called with the
        // signatures they have had since they appeared; null ones are skipped.
        unsafe {
            let module = LoadLibraryW(name.as_ptr());
            if module.is_null() {
                return;
            }
            if let Some(set_mode) = GetProcAddress(module, 135 as _) {
                let set_mode: unsafe extern "system" fn(i32) -> i32 = std::mem::transmute(set_mode);
                set_mode(FORCE_DARK);
            }
            if let Some(flush) = GetProcAddress(module, 136 as _) {
                let flush: unsafe extern "system" fn() = std::mem::transmute(flush);
                flush();
            }
        }
    }

    /// The app's icon as a small 32-bit bitmap with alpha, for the menu title.
    fn badge() -> isize {
        use windows_sys::Win32::Graphics::Gdi::{
            CreateDIBSection, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSMICON};
        // SAFETY: plain metric query.
        let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.clamp(16, 64) as u32;
        let Ok(image) = image::load_from_memory(include_bytes!("../assets/icon.png")) else {
            return 0;
        };
        let image = image
            .resize_exact(size, size, image::imageops::FilterType::Lanczos3)
            .to_rgba8();
        // SAFETY: zeroed plain-data header, then filled.
        let mut info: BITMAPINFO = unsafe { std::mem::zeroed() };
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size as i32,
            biHeight: -(size as i32), // top-down rows
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            // SAFETY: the remaining fields are plain integers.
            ..unsafe { std::mem::zeroed() }
        };
        let mut bits = std::ptr::null_mut();
        // SAFETY: a DIB section of `size`² 32-bit pixels; `bits` points at
        // exactly that many bytes once it succeeds, and is filled right away.
        unsafe {
            let bitmap = CreateDIBSection(
                std::ptr::null_mut(),
                &info,
                DIB_RGB_COLORS,
                &mut bits,
                std::ptr::null_mut(),
                0,
            );
            if bitmap.is_null() || bits.is_null() {
                return 0;
            }
            let out = std::slice::from_raw_parts_mut(bits as *mut u8, (size * size * 4) as usize);
            // Menus want premultiplied BGRA.
            for (pixel, rgba) in out.as_chunks_mut::<4>().0.iter_mut().zip(image.pixels()) {
                let [r, g, b, a] = rgba.0;
                let pre = |c: u8| (u16::from(c) * u16::from(a) / 255) as u8;
                pixel.copy_from_slice(&[pre(b), pre(g), pre(r), a]);
            }
            bitmap as isize
        }
    }

    pub fn run(labels: Labels) {
        let _ = MENU.set(labels);
        dark_menus();
        BADGE.store(badge(), Ordering::SeqCst);
        let class = wide("MirageTray");
        // SAFETY: standard Win32 setup for a message-only window owned by this
        // thread; every pointer passed lives for the call.
        unsafe {
            let module = GetModuleHandleW(std::ptr::null());
            let wc = WNDCLASSW {
                lpfnWndProc: Some(proc),
                hInstance: module,
                lpszClassName: class.as_ptr(),
                ..std::mem::zeroed()
            };
            RegisterClassW(&wc);
            let window = CreateWindowExW(
                0,
                class.as_ptr(),
                class.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                std::ptr::null_mut(),
                module,
                std::ptr::null(),
            );
            if window.is_null() {
                return;
            }
            // Resource 1 is the icon build.rs compiles in; the stock one without it.
            let mut icon = LoadImageW(module, 1 as _, IMAGE_ICON, 0, 0, LR_DEFAULTSIZE);
            if icon.is_null() {
                icon = LoadIconW(std::ptr::null_mut(), IDI_APPLICATION);
            }
            let mut data: NOTIFYICONDATAW = std::mem::zeroed();
            data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            data.hWnd = window;
            data.uID = 1;
            data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
            data.uCallbackMessage = CALLBACK;
            data.hIcon = icon;
            for (slot, unit) in data.szTip.iter_mut().zip("Mirage".encode_utf16()) {
                *slot = unit;
            }
            Shell_NotifyIconW(NIM_ADD, &data);
            ICON_WINDOW.store(window as isize, Ordering::SeqCst);
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            Shell_NotifyIconW(NIM_DELETE, &data);
        }
    }
}
