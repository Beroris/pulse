//! Bero Task Manager's interface (Slint) and the controller behind it.
//!
//! Nothing here calls the operating system directly. Monitoring goes through tm-core's
//! [`Sampler`] and [`MetaResolver`], actions through [`SystemControl`], and the few window
//! touches that only a platform can do (title bar colours, the system accent) through [`Shell`].

slint::include_modules!();

mod ctrl;
mod lists;
mod pages;
mod procs;
mod util;

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use tm_core::platform::{MetaResolver, Sampler, SystemControl};

/// An sRGB colour.
pub type Rgb = (u8, u8, u8);

/// Window and desktop details that differ per platform.
pub trait Shell {
    /// Apps are set to use the dark theme.
    fn system_dark(&self) -> bool;
    /// The system accent colour, as a variant for dark backgrounds and one for light backgrounds.
    fn system_accent(&self) -> Option<(Rgb, Rgb)>;
    /// The user wants animations (Windows' "Animation effects").
    fn animations(&self) -> bool;
    fn high_contrast(&self) -> bool;
    /// Matches the window's frame (title bar, border) to the app's theme. Called after it shows.
    fn style_main(&self, window: &slint::Window, dark: bool, background: Rgb);
    /// Makes the compact monitor a tool window: no taskbar button, rounded corners.
    fn style_compact(&self, window: &slint::Window, dark: bool);
    /// Menus the system draws (context menus, the tray menu) follow the app's theme.
    fn set_menu_theme(&self, dark: bool);
    /// Whether a rectangle in physical pixels is on a connected display.
    fn on_screen(&self, x: i32, y: i32, width: u32, height: u32) -> bool;
}

pub struct Platform {
    pub sampler: Box<dyn Sampler>,
    pub resolver: Box<dyn MetaResolver>,
    pub control: Arc<dyn SystemControl>,
    pub shell: Rc<dyn Shell>,
    pub settings_path: PathBuf,
    /// Started at sign-in: stay in the tray.
    pub start_hidden: bool,
}

/// Runs the app until it quits.
pub fn run(platform: Platform) -> Result<(), slint::PlatformError> {
    ctrl::run(platform)
}

/// A request from another copy of the app ("show" or "quit"). Safe to call from any thread.
pub fn external_command(command: String) {
    let _ = slint::invoke_from_event_loop(move || ctrl::with(|c| c.external(&command)));
}
