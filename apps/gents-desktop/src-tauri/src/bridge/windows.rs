//! Native macOS tabs keep each webview alive when it moves between windows.
//! Navigation, drafts and in-flight UI work therefore need no transfer protocol.
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use objc2::{
    define_class, msg_send, rc::Retained, runtime::AnyObject, DefinedClass, MainThreadMarker,
    MainThreadOnly,
};
use objc2_app_kit::{
    NSApplication, NSResponder, NSWindow, NSWindowOrderingMode, NSWindowTabbingMode,
};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::{AppHandle, Manager, WebviewWindow, WebviewWindowBuilder};

const TABBING_IDENTIFIER: &str = "com.source-inc.gents.views";
static NEXT_WINDOW: AtomicU64 = AtomicU64::new(1);

struct NativeWindowState {
    setup_complete: AtomicBool,
    new_tab: MenuItem<tauri::Wry>,
    new_window: MenuItem<tauri::Wry>,
}

#[tauri::command]
pub fn desktop_window_setup_complete(window: WebviewWindow) -> Result<(), String> {
    if window.label() != "main" {
        return Err("only the original view can finish window setup".into());
    }
    let app = window.app_handle();
    let state = app.state::<NativeWindowState>();
    state
        .new_tab
        .set_enabled(true)
        .map_err(|error| error.to_string())?;
    state
        .new_window
        .set_enabled(true)
        .map_err(|error| error.to_string())?;
    state.setup_complete.store(true, Ordering::Release);
    Ok(())
}

define_class!(
    // AppKit's tab-bar + button sends newWindowForTab: through the responder
    // chain. Keep this thin adapter next to the native menu's New Tab action.
    #[unsafe(super(NSResponder))]
    #[thread_kind = MainThreadOnly]
    #[ivars = AppHandle]
    struct TabResponder;

    impl TabResponder {
        #[unsafe(method(newWindowForTab:))]
        fn new_window_for_tab(&self, _sender: Option<&AnyObject>) {
            if let Err(error) = create_window(self.ivars(), true) {
                tracing::error!(%error, "native new tab failed");
            }
        }
    }
);

thread_local! {
    // NSResponder.nextResponder does not retain its target. This lives until
    // process exit and is only ever accessed on AppKit's main thread.
    static TAB_RESPONDER: RefCell<Option<Retained<TabResponder>>> = const { RefCell::new(None) };
}

pub fn setup(app: &mut tauri::App) -> tauri::Result<()> {
    let mtm = MainThreadMarker::new().expect("native setup on the main thread");
    let application = NSApplication::sharedApplication(mtm);
    let responder = TabResponder::alloc(mtm).set_ivars(app.handle().clone());
    let responder: Retained<TabResponder> = unsafe { msg_send![super(responder), init] };
    unsafe {
        responder.setNextResponder(application.nextResponder().as_deref());
        application.setNextResponder(Some(&responder));
    }
    TAB_RESPONDER.with(|slot| *slot.borrow_mut() = Some(responder));
    NSWindow::setAllowsAutomaticWindowTabbing(true, mtm);
    let menu = Menu::default(app.handle())?;
    let items = menu.items()?;
    let file = items
        .iter()
        .filter_map(|item| item.as_submenu())
        .find(|item| item.text().ok().as_deref() == Some("File"))
        .expect("default macOS menu has File");
    let new_tab = MenuItem::with_id(app, "new-tab", "New Tab", false, Some("Cmd+T"))?;
    let new_window =
        MenuItem::with_id(app, "new-window", "New Window", false, Some("Cmd+Shift+N"))?;
    file.prepend_items(&[&new_tab, &new_window, &PredefinedMenuItem::separator(app)?])?;
    let window_menu = menu
        .get(tauri::menu::WINDOW_SUBMENU_ID)
        .expect("default macOS menu has Window");
    window_menu.as_submenu_unchecked().append_items(&[
        &PredefinedMenuItem::separator(app)?,
        &MenuItem::with_id(
            app,
            "detach-tab",
            "Move Tab to New Window",
            true,
            None::<&str>,
        )?,
        &MenuItem::with_id(
            app,
            "merge-windows",
            "Merge All Windows",
            true,
            None::<&str>,
        )?,
        &MenuItem::with_id(
            app,
            "previous-tab",
            "Show Previous Tab",
            true,
            Some("Ctrl+Shift+Tab"),
        )?,
        &MenuItem::with_id(app, "next-tab", "Show Next Tab", true, Some("Ctrl+Tab"))?,
    ])?;
    app.set_menu(menu)?;
    app.manage(NativeWindowState {
        setup_complete: AtomicBool::new(false),
        new_tab,
        new_window,
    });
    app.on_menu_event(|app, event| {
        if let Err(error) = handle_menu(app, event.id.as_ref()) {
            tracing::error!(%error, "native window action failed");
        }
    });
    Ok(())
}

fn focused_window(app: &AppHandle) -> Option<WebviewWindow> {
    app.webview_windows()
        .into_values()
        .find(|window| window.is_focused().unwrap_or(false))
}

pub fn handle_menu(app: &AppHandle, action: &str) -> tauri::Result<()> {
    if matches!(action, "new-tab" | "new-window") {
        create_window(app, action == "new-tab")?;
    } else if let Some(window) = focused_window(app) {
        with_native_window(&window, |native| match action {
            "detach-tab" => native.moveTabToNewWindow(None),
            "merge-windows" => native.mergeAllWindows(None),
            "previous-tab" => native.selectPreviousTab(None),
            "next-tab" => native.selectNextTab(None),
            _ => {}
        })?;
    }
    Ok(())
}

pub fn create_window(app: &AppHandle, tab: bool) -> tauri::Result<WebviewWindow> {
    if !app
        .state::<NativeWindowState>()
        .setup_complete
        .load(Ordering::Acquire)
    {
        return Err(tauri::Error::Anyhow(anyhow::anyhow!(
            "finish setup before opening another view"
        )));
    }
    let parent = focused_window(app);
    let mut config = app.config().app.windows[0].clone();
    config.label = format!("gents-view-{}", NEXT_WINDOW.fetch_add(1, Ordering::Relaxed));
    config.visible = false;
    config.tabbing_identifier = Some(TABBING_IDENTIFIER.into());
    let window = WebviewWindowBuilder::from_config(app, &config)?.build()?;
    // Selection is local to each view. A single shared observation scope must
    // not suppress updates or request actions for the other views' agents.
    let state = app.state::<gents_desktop_bridge::state::DesktopAppState>();
    if let Some(core) = gents_desktop_bridge::state::current_core(&state) {
        core.set_selected_agent_did(None);
    }
    with_native_window(&window, |native| {
        native.setTabbingMode(if tab {
            NSWindowTabbingMode::Preferred
        } else {
            NSWindowTabbingMode::Disallowed
        });
    })?;
    if tab {
        if let Some(parent) = parent {
            with_native_window(&parent, |parent| {
                with_native_window(&window, |native| {
                    parent.addTabbedWindow_ordered(native, NSWindowOrderingMode::Above);
                })
            })??;
        }
    }
    window.show()?;
    window.set_focus()?;
    // Explicit New Window bypasses the system's "always prefer tabs" setting;
    // once shown it can participate in dragging and merging like every tab.
    with_native_window(&window, |native| {
        native.setTabbingMode(NSWindowTabbingMode::Automatic)
    })?;
    Ok(window)
}

pub fn detach(window: &WebviewWindow) -> tauri::Result<()> {
    with_native_window(window, |native| native.moveTabToNewWindow(None))
}

fn with_native_window<T>(
    window: &WebviewWindow,
    action: impl FnOnce(&NSWindow) -> T,
) -> tauri::Result<T> {
    // All callers are native setup/menu/window-event callbacks on the main
    // thread. The Tauri window retains its NSWindow throughout this call.
    assert!(objc2::MainThreadMarker::new().is_some());
    let pointer = window.ns_window()?;
    let native = unsafe { &*pointer.cast::<NSWindow>() };
    Ok(action(native))
}
