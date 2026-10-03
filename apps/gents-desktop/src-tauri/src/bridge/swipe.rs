//! Two-finger history swipes come from AppKit's own recogniser so they match
//! Safari's thresholds and tracking; the webview draws the feedback and owns
//! the history, so WebKit's page-sliding animation and back-forward list stay
//! out of it.
//!
//! AppKit's tracker takes over the scroll stream once it starts, so it only
//! starts where the webview has said the content under the pointer cannot
//! scroll further in the gesture's direction. Content scrolls first and the
//! swipe arms at its edge, as in Safari and Chrome.
use std::collections::HashMap;
use std::ptr::NonNull;
use std::sync::Mutex;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool};
use objc2::MainThreadMarker;
use objc2_app_kit::{NSEvent, NSEventMask, NSEventPhase, NSEventSwipeTrackingOptions};
use objc2_core_foundation::CGFloat;
use tauri::{AppHandle, Emitter, Manager, WebviewWindow};

/// Sideways scroll room under each view's pointer: (left, right).
static SCROLL_ROOM: Mutex<Option<HashMap<String, (bool, bool)>>> = Mutex::new(None);

#[tauri::command]
pub fn swipe_scroll_edges(window: WebviewWindow, left: bool, right: bool) {
    let mut room = SCROLL_ROOM
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    room.get_or_insert_with(HashMap::new)
        .insert(window.label().to_owned(), (left, right));
}

fn scroll_room(label: &str) -> (bool, bool) {
    let room = SCROLL_ROOM
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    room.as_ref()
        .and_then(|room| room.get(label).copied())
        .unwrap_or((false, false))
}

#[derive(Clone, serde::Serialize)]
struct SwipeProgress {
    amount: f64,
    phase: &'static str,
}

pub fn setup(app: AppHandle) {
    let monitor: RcBlock<dyn Fn(NonNull<NSEvent>) -> *mut NSEvent> =
        RcBlock::new(move |event: NonNull<NSEvent>| {
            let event = unsafe { event.as_ref() };
            if event.phase() == NSEventPhase::Began
                && event.scrollingDeltaX().abs() > event.scrollingDeltaY().abs()
                && NSEvent::isSwipeTrackingFromScrollEventsEnabled()
            {
                track(&app, event);
            }
            event as *const NSEvent as *mut NSEvent
        });
    // The monitor lives for the process; AppKit retains the block.
    let _handle: Option<Retained<AnyObject>> = unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::ScrollWheel, &monitor)
    };
    std::mem::forget(monitor);
}

fn track(app: &AppHandle, event: &NSEvent) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let Some(native) = event.window(mtm) else {
        return;
    };
    let native_pointer = Retained::as_ptr(&native) as *mut std::ffi::c_void;
    let Some(window) = app
        .webview_windows()
        .into_values()
        .find(|window| matches!(window.ns_window(), Ok(pointer) if pointer == native_pointer))
    else {
        return;
    };
    // A positive delta moves content right, toward its left edge.
    let (left, right) = scroll_room(window.label());
    let delta = event.scrollingDeltaX();
    if (delta > 0.0 && left) || (delta < 0.0 && right) {
        return;
    }
    let handler: RcBlock<dyn Fn(CGFloat, NSEventPhase, Bool, NonNull<Bool>)> = RcBlock::new(
        move |amount: CGFloat, phase: NSEventPhase, _complete: Bool, _stop: NonNull<Bool>| {
            // After Ended AppKit keeps animating the amount on its own; the
            // webview settles its handle from where the fingers left it.
            let phase = match phase {
                NSEventPhase::Began | NSEventPhase::Changed => "moving",
                NSEventPhase::Ended => "ended",
                NSEventPhase::Cancelled => "cancelled",
                _ => return,
            };
            let _ = window.emit(
                "native-swipe",
                SwipeProgress {
                    amount: amount as f64,
                    phase,
                },
            );
        },
    );
    event.trackSwipeEventWithOptions_dampenAmountThresholdMin_max_usingHandler(
        NSEventSwipeTrackingOptions::LockDirection,
        -1.0,
        1.0,
        &handler,
    );
}
