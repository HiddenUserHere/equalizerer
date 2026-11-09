//! Window behaviour that Slint does not expose: the title bar buttons, the window icon,
//! minimize-to-tray and showing the window again.
//!
//! Slint reaches the native window through its winit accessor. Winit is therefore selected
//! explicitly, and the icon and button set are passed as window attributes. Slint then enables the
//! maximize button again when it creates a resizable window, so the event filter removes it once
//! the window exists.

use slint::ComponentHandle;
use slint::winit_030::winit;
use slint::winit_030::winit::event::WindowEvent;
use slint::winit_030::winit::window::{Icon, WindowButtons};
use slint::winit_030::{EventResult, WinitWindowAccessor};

use super::AppWindow;
use crate::icon;

/// Selects the winit backend and sets the native window attributes: no maximize button, and the
/// application icon. Must run once, before the first window is created.
///
/// # Errors
///
/// Returns the platform error when the backend cannot be selected, for example because another
/// backend was already created.
pub fn select_backend() -> Result<(), slint::PlatformError> {
    slint::BackendSelector::new()
        .backend_name("winit".to_owned())
        .with_winit_window_attributes_hook(|attributes| {
            // Without the maximize button the window cannot be maximized from its title bar. It
            // stays resizable through its borders.
            attributes
                .with_enabled_buttons(WindowButtons::CLOSE | WindowButtons::MINIMIZE)
                .with_window_icon(window_icon())
        })
        .select()
}

/// Shows the window. A request from the tray or from a second launch must make a hidden window
/// visible at once, so it is also un-minimized and brought to the front.
pub fn show(ui: &AppWindow) {
    if let Err(error) = ui.show() {
        tracing::warn!("cannot show the window: {error}");
        return;
    }
    ui.window().with_winit_window(|window| {
        window.set_minimized(false);
        window.focus_window();
    });
}

/// Hides the window. The tray icon is then the only way to show it again.
pub fn hide_to_tray(ui: &AppWindow) {
    if let Err(error) = ui.hide() {
        tracing::warn!("cannot hide the window: {error}");
    }
}

/// Installs the winit event filter of the window. It runs for every native window event:
///
/// - Removes the maximize button on the first event. Slint enables that button again when it
///   creates a resizable native window (a workaround for winit bug 2990), after the window
///   attributes are applied. Every event comes after that creation, so the first one is the right
///   moment. The change is posted to the event loop, because winit's event dispatch should not
///   change the window it is running for.
/// - With `hide_on_minimize`, hides the window when the user minimizes it, so that it leaves the
///   taskbar and stays reachable from the tray icon. Without a tray icon the window must minimize
///   normally, so the flag is then false.
///
/// Winit reports a minimized window through `Resized` or `Occluded`. Hiding is posted to the
/// event loop for the same reason as the button change.
pub fn install_window_filter(ui: &AppWindow, hide_on_minimize: bool) {
    let weak = ui.as_weak();
    let mut buttons_fixed = false;
    ui.window().on_winit_window_event(move |window, event| {
        if !buttons_fixed {
            buttons_fixed = true;
            let weak = weak.clone();
            post_to_ui(move || {
                if let Some(ui) = weak.upgrade() {
                    ui.window().with_winit_window(|winit_window| {
                        winit_window
                            .set_enabled_buttons(WindowButtons::CLOSE | WindowButtons::MINIMIZE);
                    });
                }
            });
        }
        let may_mean_minimized =
            matches!(event, WindowEvent::Resized(_) | WindowEvent::Occluded(_));
        if hide_on_minimize && may_mean_minimized {
            let minimized =
                window.with_winit_window(winit::window::Window::is_minimized) == Some(Some(true));
            if minimized {
                let weak = weak.clone();
                post_to_ui(move || {
                    if let Some(ui) = weak.upgrade() {
                        hide_to_tray(&ui);
                    }
                });
            }
        }
        EventResult::Propagate
    });
}

/// Runs `action` on the UI thread after the current event, outside of winit's event dispatch.
fn post_to_ui(action: impl FnOnce() + Send + 'static) {
    if let Err(error) = slint::invoke_from_event_loop(action) {
        tracing::warn!("cannot update the window: {error}");
    }
}

/// The application icon for the window title bar and taskbar.
fn window_icon() -> Option<Icon> {
    match Icon::from_rgba(icon::rgba(), icon::SIZE, icon::SIZE) {
        Ok(icon) => Some(icon),
        Err(error) => {
            tracing::warn!("cannot build the window icon: {error}");
            None
        }
    }
}
