//! Client host layer: Tauri desktop and mobile entrypoint wiring `gents-desktop-bridge`.
//!
//! Client shell and lifecycle models live in `crates/gents/proofs/Proofs/Clients/`.

#[cfg(any(target_os = "ios", all(test, target_os = "macos")))]
mod backup_exclusion;
mod bridge;
pub mod browser_data;
#[cfg(all(target_os = "macos", feature = "native-e2e", debug_assertions))]
pub mod frame_probe;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    bridge::run();
}
