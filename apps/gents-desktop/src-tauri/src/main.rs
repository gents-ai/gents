#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// glibc per-thread arenas retained DefraDB query churn at ~2.5x the live heap
/// and memcg-OOM-killed the runtime at 512 MiB (#2034).
#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Debug, thiserror::Error)]
enum MaintenanceError {
    #[error("missing browser clear receipt path")]
    MissingClearReceipt,
    #[error("missing browser check receipt path")]
    MissingCheckReceipt,
    #[error("unexpected browser clear argument")]
    UnexpectedClearArgument,
    #[cfg(all(target_os = "macos", feature = "native-e2e", debug_assertions))]
    #[error("missing fixture receipt path")]
    MissingFixtureReceipt,
    #[cfg(all(target_os = "macos", feature = "native-e2e", debug_assertions))]
    #[error("unexpected fixture argument")]
    UnexpectedFixtureArgument,
    #[error(transparent)]
    BrowserData(#[from] gents_desktop_tauri_lib::browser_data::BrowserDataError),
}

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
        let result = (|| -> Result<(), MaintenanceError> {
            let mut receipt = args.next().ok_or(MaintenanceError::MissingClearReceipt)?;
            let check = receipt == "--check";
            if check {
                receipt = args.next().ok_or(MaintenanceError::MissingCheckReceipt)?;
            }
            if args.next().is_some() {
                return Err(MaintenanceError::UnexpectedClearArgument);
            }
            let receipt = std::path::Path::new(&receipt);
            let result = if check {
                gents_desktop_tauri_lib::browser_data::check_to_receipt(receipt)
            } else {
                gents_desktop_tauri_lib::browser_data::clear_to_receipt(receipt)
            };
            Ok(result?)
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
        let result = (|| -> Result<(), MaintenanceError> {
            let receipt = args.next().ok_or(MaintenanceError::MissingFixtureReceipt)?;
            if args.next().is_some() {
                return Err(MaintenanceError::UnexpectedFixtureArgument);
            }
            let result = gents_desktop_tauri_lib::browser_data::verify_isolated_clear(
                std::path::Path::new(&receipt),
            );
            Ok(result?)
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
