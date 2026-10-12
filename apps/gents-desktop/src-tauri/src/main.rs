#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// glibc per-thread arenas retained DefraDB query churn at about 2.5x the live
/// heap and memcg-OOM-killed the runtime at 512 MiB (#2034).
#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() {
    let mut args = std::env::args();
    let _program = args.next();
    let command = args.next();
    if command.as_deref() == Some("__native-fs-runner") {
        if let Err(error) = gents_fs_runner::run_stdio_from_args(args) {
            gents_fs_runner::write_stdio_error(&error);
            std::process::exit(1);
        }
        return;
    }

    if command.as_deref() == Some("__clear-browser-data") {
        let _ = tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .try_init();
        let result = (|| -> anyhow::Result<()> {
            let receipt = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("missing browser clear receipt path"))?;
            anyhow::ensure!(args.next().is_none(), "unexpected browser clear argument");
            gents_desktop_tauri_lib::browser_data::clear_to_receipt(std::path::Path::new(&receipt))
        })();
        if let Err(error) = result {
            tracing::error!(%error, "browser data reset failed");
            std::process::exit(1);
        }
        return;
    }

    #[cfg(all(target_os = "macos", feature = "native-e2e", debug_assertions))]
    if command.as_deref() == Some("__verify-browser-clear") {
        let _ = tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .try_init();
        let result = (|| -> anyhow::Result<()> {
            let receipt = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("missing fixture receipt path"))?;
            anyhow::ensure!(args.next().is_none(), "unexpected fixture argument");
            gents_desktop_tauri_lib::browser_data::verify_isolated_clear(std::path::Path::new(
                &receipt,
            ))
        })();
        if let Err(error) = result {
            tracing::error!(%error, "isolated browser data fixture failed");
            std::process::exit(1);
        }
        return;
    }

    gents::enable_self_runner();
    gents_desktop_tauri_lib::run()
}
