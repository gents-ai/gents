//! `gents:` links the system hands the app: the one it was launched with, and
//! each one opened while it runs. The main view takes them once it can follow
//! them; a link only names a place, so nothing here acts on one.

use std::sync::Mutex;

use tauri::{AppHandle, Emitter, Manager, Runtime};
use tauri_plugin_deep_link::DeepLinkExt;

/// Tells the main view that links are waiting; it takes them with
/// [`desktop_take_links`].
pub const OPEN_LINK_EVENT: &str = "desktop://open-link";

const SCHEME: &str = "gents:";

#[derive(Default)]
pub struct PendingLinks(Mutex<Vec<String>>);

impl PendingLinks {
    fn links(&self) -> std::sync::MutexGuard<'_, Vec<String>> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn hold(&self, links: impl IntoIterator<Item = String>) -> usize {
        let mut pending = self.links();
        let before = pending.len();
        pending.extend(links.into_iter().filter(|link| is_app_link(link)));
        pending.len() - before
    }

    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.links())
    }
}

fn is_app_link(link: &str) -> bool {
    link.get(..SCHEME.len())
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case(SCHEME))
}

fn receive<R>(app: &AppHandle<R>, links: impl IntoIterator<Item = String>)
where
    R: Runtime,
{
    let Some(pending) = app.try_state::<PendingLinks>() else {
        return;
    };
    let held = pending.hold(links);
    if held == 0 {
        return;
    }
    tracing::info!(count = held, "desktop link received");
    super::show_main_window(app);
    if let Err(error) = app.emit_to("main", OPEN_LINK_EVENT, ()) {
        tracing::warn!(%error, "could not tell the main view about a link");
    }
}

/// Registers the scheme where the platform needs it at run time, and holds
/// the launch link and every later one. With single-instance's `deep-link`
/// feature, a second launch's link arrives through `on_open_url` too.
pub fn setup<R>(app: &tauri::App<R>) -> Result<(), Box<dyn std::error::Error>>
where
    R: Runtime,
{
    app.manage(PendingLinks::default());
    // Installers register the scheme on Windows and Linux; a development
    // build or an AppImage has to register itself.
    #[cfg(any(windows, target_os = "linux"))]
    if cfg!(debug_assertions) || std::env::var_os("APPIMAGE").is_some() {
        if let Err(error) = app.deep_link().register_all() {
            tracing::warn!(%error, "could not register the gents link scheme");
        }
    }
    let handle = app.handle().clone();
    app.deep_link().on_open_url(move |event| {
        receive(&handle, event.urls().into_iter().map(String::from));
    });
    match app.deep_link().get_current() {
        Ok(Some(urls)) => receive(app.handle(), urls.into_iter().map(String::from)),
        Ok(None) => {}
        Err(error) => tracing::warn!(%error, "could not read the launch link"),
    }
    Ok(())
}

#[tauri::command]
pub fn desktop_take_links(pending: tauri::State<'_, PendingLinks>) -> Vec<String> {
    pending.take()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holds_only_gents_links_until_taken() {
        let pending = PendingLinks::default();
        let held = pending.hold([
            "gents://session/abc".to_owned(),
            "https://example.com".to_owned(),
            "GENTS:session/def".to_owned(),
            "gentsx://session/ghi".to_owned(),
        ]);
        assert_eq!(held, 2);
        assert_eq!(
            pending.take(),
            vec![
                "gents://session/abc".to_owned(),
                "GENTS:session/def".to_owned()
            ]
        );
        assert!(pending.take().is_empty());
    }
}
