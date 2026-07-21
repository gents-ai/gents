mod bridge;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    bridge::run();
}
