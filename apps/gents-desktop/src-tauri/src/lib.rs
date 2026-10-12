pub mod browser_data;
mod bridge;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    bridge::run();
}
