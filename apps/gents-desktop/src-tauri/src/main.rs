#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let mut args = std::env::args();
    let _program = args.next();
    if args.next().as_deref() == Some("__native-fs-runner") {
        if let Err(error) = gents_fs_runner::run_stdio_from_args(args) {
            gents_fs_runner::write_stdio_error(&error);
            std::process::exit(1);
        }
        return;
    }

    gents::enable_self_runner();
    gents_desktop_tauri_lib::run()
}
