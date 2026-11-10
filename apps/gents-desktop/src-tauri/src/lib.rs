mod bridge;
pub mod browser_data;
#[cfg(all(target_os = "macos", feature = "native-e2e", debug_assertions))]
pub mod frame_probe;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    bridge::run();
}
