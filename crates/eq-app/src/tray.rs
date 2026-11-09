//! Tray icon: the way back to the window while it is hidden, and a second way to quit.
//!
//! Windows only for now. Other platforms have no tray yet (see the backlog in `docs/PLAN.md`),
//! so [`start`] returns `None` there and the window behaves as an ordinary window.
//!
//! The tray library delivers its events on the thread that runs the platform message loop. The
//! handlers only forward a [`TrayAction`] through a channel; the UI thread applies it.

use crossbeam_channel::Sender;

/// A request made through the tray icon or its menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    /// Show the window: the menu item "Open Equalizerer", a double click or a left click.
    Open,
    /// Quit the application, the same as closing the window.
    Quit,
}

/// Identifier of the "Open Equalizerer" menu item.
#[cfg(windows)]
const OPEN_ITEM: &str = "open";

/// Identifier of the "Quit" menu item.
#[cfg(windows)]
const QUIT_ITEM: &str = "quit";

/// The live tray icon. Dropping it removes the icon from the notification area.
#[cfg(windows)]
pub type Tray = tray_icon::TrayIcon;

/// No tray icon exists on this platform yet.
#[cfg(not(windows))]
pub type Tray = std::convert::Infallible;

/// Creates the tray icon and its menu, and forwards their requests to `actions`.
///
/// Must run on the thread that runs the window's event loop, after the window exists. The
/// returned value keeps the icon visible; drop it to remove the icon.
///
/// # Errors
///
/// Returns a message for the log when the menu or the icon cannot be created.
#[cfg(windows)]
pub fn start(actions: Sender<TrayAction>) -> Result<Option<Tray>, String> {
    use tray_icon::menu::{Menu, MenuEvent, MenuItem};
    use tray_icon::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    let open = MenuItem::with_id(OPEN_ITEM, "Open Equalizerer", true, None);
    let quit = MenuItem::with_id(QUIT_ITEM, "Quit", true, None);
    let menu = Menu::new();
    menu.append_items(&[&open, &quit])
        .map_err(|error| format!("cannot build the tray menu: {error}"))?;

    let icon =
        tray_icon::Icon::from_rgba(crate::icon::rgba(), crate::icon::SIZE, crate::icon::SIZE)
            .map_err(|error| format!("cannot build the tray icon: {error}"))?;
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        // A left click opens the window; the menu belongs to the right click.
        .with_menu_on_left_click(false)
        .with_tooltip("Equalizerer")
        .with_icon(icon)
        .build()
        .map_err(|error| format!("cannot create the tray icon: {error}"))?;

    let menu_actions = actions.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let action = match event.id.as_ref() {
            OPEN_ITEM => TrayAction::Open,
            QUIT_ITEM => TrayAction::Quit,
            _ => return,
        };
        // A closed channel means the UI has ended; there is nothing left to tell.
        let _ = menu_actions.send(action);
    }));

    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        let opens = matches!(
            event,
            TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } | TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            }
        );
        if opens {
            let _ = actions.send(TrayAction::Open);
        }
    }));

    Ok(Some(tray))
}

/// No tray icon exists on this platform yet; the window then behaves as an ordinary window.
///
/// # Errors
///
/// Never fails on this platform; the signature matches the Windows version.
#[cfg(not(windows))]
pub fn start(_actions: Sender<TrayAction>) -> Result<Option<Tray>, String> {
    Ok(None)
}
