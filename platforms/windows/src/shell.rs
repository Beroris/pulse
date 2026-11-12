//! Windows' look and feel for tm-app: the system theme and accent, motion and contrast settings,
//! and the DWM attributes that make the title bar and frame match the app.

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use tm_app::{Rgb, Shell};
use windows::Win32::Foundation::{COLORREF, HWND, RECT};
use windows::core::BOOL;
use windows::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWINDOWATTRIBUTE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONULL, MonitorFromRect};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, KEY_READ};
use windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetWindowLongPtrW, SPI_GETCLIENTAREAANIMATION, SPI_GETHIGHCONTRAST, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetWindowLongPtrW, SystemParametersInfoW,
    WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};

use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::core::{PCSTR, w};

use crate::util::{RegKey, RegValue};

pub struct WinShell;

const PERSONALIZE: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";
const ACCENT: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent";

pub fn hwnd(window: &slint::Window) -> Option<HWND> {
    let handle = window.window_handle();
    let raw = handle.window_handle().ok()?.as_raw();
    match raw {
        RawWindowHandle::Win32(h) => Some(HWND(h.hwnd.get() as *mut _)),
        _ => None,
    }
}

fn set_attr<T>(hwnd: HWND, attr: DWMWINDOWATTRIBUTE, value: &T) {
    unsafe {
        let _ = DwmSetWindowAttribute(hwnd, attr, value as *const T as *const _, std::mem::size_of::<T>() as u32);
    }
}

fn colorref(c: Rgb) -> COLORREF {
    COLORREF(c.0 as u32 | (c.1 as u32) << 8 | (c.2 as u32) << 16)
}

impl Shell for WinShell {
    fn system_dark(&self) -> bool {
        // Missing on old builds: those only had light apps.
        RegKey::open(HKEY_CURRENT_USER, PERSONALIZE, KEY_READ).and_then(|k| k.dword("AppsUseLightTheme")).is_some_and(|v| v == 0)
    }

    fn system_accent(&self) -> Option<(Rgb, Rgb)> {
        let k = RegKey::open(HKEY_CURRENT_USER, ACCENT, KEY_READ)?;
        let RegValue::Binary(b) = k.query("AccentPalette")? else { return None };
        if b.len() < 32 {
            return None;
        }
        // Seven shades, lightest first. Windows' own apps use the second for text and lines on dark
        // backgrounds and the fifth on light ones.
        let shade = |i: usize| (b[i * 4], b[i * 4 + 1], b[i * 4 + 2]);
        Some((shade(1), shade(4)))
    }

    fn animations(&self) -> bool {
        let mut on = BOOL(1);
        let ok = unsafe { SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, Some(&mut on as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)) };
        ok.is_err() || on.as_bool()
    }

    fn high_contrast(&self) -> bool {
        let mut hc = HIGHCONTRASTW { cbSize: std::mem::size_of::<HIGHCONTRASTW>() as u32, ..Default::default() };
        let ok = unsafe { SystemParametersInfoW(SPI_GETHIGHCONTRAST, hc.cbSize, Some(&mut hc as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)) };
        ok.is_ok() && hc.dwFlags.contains(HCF_HIGHCONTRASTON)
    }

    fn style_main(&self, window: &slint::Window, dark: bool, background: Rgb) {
        let Some(h) = hwnd(window) else { return };
        set_attr(h, DWMWA_USE_IMMERSIVE_DARK_MODE, &BOOL::from(dark));
        // The title bar takes the page colour, so the window reads as one surface (Windows 11).
        set_attr(h, DWMWA_CAPTION_COLOR, &colorref(background));
        set_attr(h, DWMWA_TEXT_COLOR, &colorref(if dark { (0xe8, 0xe8, 0xea) } else { (0x1a, 0x1a, 0x1c) }));
    }

    fn style_compact(&self, window: &slint::Window, dark: bool) {
        let Some(h) = hwnd(window) else { return };
        set_attr(h, DWMWA_USE_IMMERSIVE_DARK_MODE, &BOOL::from(dark));
        set_attr(h, DWMWA_WINDOW_CORNER_PREFERENCE, &DWMWCP_ROUND);
        set_attr(h, DWMWA_BORDER_COLOR, &colorref(if dark { (0x2c, 0x2c, 0x31) } else { (0xd8, 0xd8, 0xdc) }));
        // A tool window: no taskbar button and not in Alt+Tab.
        unsafe {
            let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
            let want = (ex | WS_EX_TOOLWINDOW.0 as isize) & !(WS_EX_APPWINDOW.0 as isize);
            if want != ex {
                SetWindowLongPtrW(h, GWL_EXSTYLE, want);
            }
        }
    }

    fn set_menu_theme(&self, dark: bool) {
        // uxtheme exports these by ordinal only: SetPreferredAppMode (135) and FlushMenuThemes (136).
        // Windows' own apps use them for dark context menus. Where they're missing, menus stay light.
        type SetMode = unsafe extern "system" fn(i32) -> i32;
        type Flush = unsafe extern "system" fn();
        unsafe {
            let Ok(lib) = LoadLibraryW(w!("uxtheme.dll")) else { return };
            let (Some(set), Some(flush)) = (GetProcAddress(lib, PCSTR(135 as *const u8)), GetProcAddress(lib, PCSTR(136 as *const u8))) else { return };
            let set: SetMode = std::mem::transmute(set);
            let flush: Flush = std::mem::transmute(flush);
            // 2: always dark, 3: always light.
            set(if dark { 2 } else { 3 });
            flush();
        }
    }

    fn on_screen(&self, x: i32, y: i32, width: u32, height: u32) -> bool {
        // The top strip must be on a monitor, so the window can always be grabbed and moved.
        let r = RECT { left: x + 8, top: y, right: x + (width as i32).max(48) - 8, bottom: y + 32.min(height as i32).max(8) };
        !unsafe { MonitorFromRect(&r, MONITOR_DEFAULTTONULL) }.is_invalid()
    }
}
