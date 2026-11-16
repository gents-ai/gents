#[cfg(all(target_os = "macos", feature = "native-e2e", debug_assertions))]
#[path = "browser_data.rs"]
mod browser_data;
#[cfg(all(target_os = "macos", feature = "native-e2e", debug_assertions))]
#[path = "frame_ipc.rs"]
mod frame_ipc;
