//! Notifications whose click the view that posted them hears about, on
//! macOS. The notification plugin hands its notice to the system and keeps
//! nothing, so a click only brings the app forward; here the notice is
//! delivered the same way (`mac-notification-sys`, which the plugin uses
//! through `notify-rust`) but waited on, and a click brings the posting
//! window forward and tells it the notice's id.

use std::sync::atomic::{AtomicUsize, Ordering};

use mac_notification_sys::{Notification, NotificationResponse};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Runtime, WebviewWindow};

/// Carries the clicked notice's id to the window that posted it.
pub const NOTIFICATION_CLICKED_EVENT: &str = "desktop://notification-clicked";

/// The system answers a waited notice only when the person clicks it, closes
/// it or clears it from Notification Center, so each waiting notice holds a
/// thread until then. Past this many, a notice is delivered without waiting
/// and its view falls back to offering it in the app.
const MAX_WAITING: usize = 16;
const MAX_TITLE: usize = 256;
const MAX_BODY: usize = 1024;
const MAX_ID: usize = 64;
const WAITER_STACK: usize = 256 * 1024;

static WAITING: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Posted {
    /// whether a click on this notice will reach its view
    reports_click: bool,
}

fn checked(title: &str, body: Option<&str>, id: &str) -> Result<(), String> {
    if title.trim().is_empty() || title.len() > MAX_TITLE {
        return Err("a notification needs a title of at most 256 bytes".into());
    }
    if body.is_some_and(|body| body.len() > MAX_BODY) {
        return Err("a notification's body is at most 1024 bytes".into());
    }
    if id.is_empty()
        || id.len() > MAX_ID
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("a notification's id is letters, digits, dashes and underscores".into());
    }
    Ok(())
}

/// Takes one of `cap` waiting places, or none when all are taken.
fn reserve(waiting: &AtomicUsize, cap: usize) -> bool {
    waiting
        .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            (n < cap).then_some(n + 1)
        })
        .is_ok()
}

fn release(waiting: &AtomicUsize) {
    waiting.fetch_sub(1, Ordering::AcqRel);
}

/// A click on the notice, or on its action button, opens what it is about;
/// closing it, clearing it or letting it go opens nothing.
fn opens(response: &NotificationResponse) -> bool {
    matches!(
        response,
        NotificationResponse::Click | NotificationResponse::ActionButton(_)
    )
}

/// The identity the system attributes the notice to: the plugin's own
/// choice, so both paths agree whichever posts first. A development binary
/// has no bundle of its own and borrows Terminal's.
fn attribute<R>(app: &AppHandle<R>)
where
    R: Runtime,
{
    let identifier = app.config().identifier.clone();
    let _ = mac_notification_sys::set_application(if tauri::is_dev() {
        "com.apple.Terminal"
    } else {
        &identifier
    });
}

fn bring_forward<R>(app: &AppHandle<R>, label: &str, id: &str)
where
    R: Runtime,
{
    if label == "main" {
        super::show_main_window(app);
    } else if let Some(window) = app.get_webview_window(label) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
    if let Err(error) = app.emit_to(label, NOTIFICATION_CLICKED_EVENT, id) {
        tracing::warn!(%error, "could not tell a view its notification was clicked");
    }
}

fn deliver(title: &str, body: &str, wait: bool) -> Option<NotificationResponse> {
    let mut options = Notification::new();
    if wait {
        options.wait_for_click(true);
    } else {
        options.asynchronous(true);
    }
    match mac_notification_sys::send_notification(title, None, body, Some(&options)) {
        Ok(response) => Some(response),
        Err(error) => {
            tracing::warn!(%error, "notification not delivered");
            None
        }
    }
}

/// Posts a notice and, where its click can be heard, says so. A development
/// build's notices are Terminal's, whose clicks go to Terminal, so it never
/// waits on one.
#[tauri::command]
pub async fn desktop_post_notification<R>(
    app: AppHandle<R>,
    window: WebviewWindow<R>,
    title: String,
    body: Option<String>,
    id: String,
) -> Result<Posted, String>
where
    R: Runtime,
{
    checked(&title, body.as_deref(), &id)?;
    attribute(&app);
    let body = body.unwrap_or_default();
    if tauri::is_dev() || !reserve(&WAITING, MAX_WAITING) {
        // Delivery waits up to two seconds for the system's confirmation.
        tauri::async_runtime::spawn_blocking(move || deliver(&title, &body, false))
            .await
            .map_err(|error| error.to_string())?;
        return Ok(Posted {
            reports_click: false,
        });
    }
    let label = window.label().to_owned();
    let waiter = std::thread::Builder::new()
        .name("gents-notification".into())
        .stack_size(WAITER_STACK)
        .spawn(move || {
            let response = deliver(&title, &body, true);
            release(&WAITING);
            if response.as_ref().is_some_and(opens) {
                bring_forward(&app, &label, &id);
            }
        });
    if let Err(error) = waiter {
        release(&WAITING);
        return Err(format!("could not wait on the notification: {error}"));
    }
    Ok(Posted {
        reports_click: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_click_or_its_button_opens_what_a_notice_is_about() {
        assert!(opens(&NotificationResponse::Click));
        assert!(opens(&NotificationResponse::ActionButton("Open".into())));
        assert!(!opens(&NotificationResponse::None));
        assert!(!opens(&NotificationResponse::CloseButton("Close".into())));
        assert!(!opens(&NotificationResponse::Reply("ok".into())));
    }

    #[test]
    fn waits_on_at_most_the_cap_and_frees_a_place_on_release() {
        let waiting = AtomicUsize::new(0);
        assert!(reserve(&waiting, 2));
        assert!(reserve(&waiting, 2));
        assert!(!reserve(&waiting, 2));
        release(&waiting);
        assert!(reserve(&waiting, 2));
        assert_eq!(waiting.load(Ordering::Acquire), 2);
    }

    #[test]
    fn refuses_an_empty_title_an_oversized_body_and_an_id_that_isnt_plain() {
        assert!(checked("Board: New task", Some("Ship it"), "a1-b2_c3").is_ok());
        assert!(checked("  ", None, "a").is_err());
        assert!(checked(&"t".repeat(MAX_TITLE + 1), None, "a").is_err());
        assert!(checked("t", Some(&"b".repeat(MAX_BODY + 1)), "a").is_err());
        assert!(checked("t", None, "").is_err());
        assert!(checked("t", None, "a/b").is_err());
        assert!(checked("t", None, &"a".repeat(MAX_ID + 1)).is_err());
    }

    #[test]
    fn tells_the_view_whether_its_click_will_be_heard() {
        assert_eq!(
            serde_json::to_value(Posted {
                reports_click: true
            })
            .unwrap(),
            serde_json::json!({ "reportsClick": true })
        );
    }
}
