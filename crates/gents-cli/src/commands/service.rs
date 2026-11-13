use std::path::Path;

use gents_service::native_service::{SERVICE_LABEL, SYSTEMD_UNIT};

use crate::shell_word;

/// What `gents runtime service uninstall` removed and kept. The keychain
/// items are named by the login-keychain services the store key and the
/// `macos-keychain` identity use; no gents command deletes them.
pub(crate) fn uninstall_summary(
    home: &Path,
    definition: &Path,
    removed: bool,
    stderr_log: Option<&Path>,
) -> Vec<String> {
    let macos = cfg!(target_os = "macos");
    let job = if macos { SERVICE_LABEL } else { SYSTEMD_UNIT };
    let mut lines = vec![if removed {
        format!(
            "Removed the Gents service: stopped {job} and deleted {}.",
            definition.display()
        )
    } else {
        format!(
            "No Gents service was installed at {}; nothing was removed.",
            definition.display()
        )
    }];
    lines.push("Kept:".to_owned());
    lines.push(format!(
        "  home      {} (node keys, agents, sessions and data)",
        home.display()
    ));
    if macos {
        lines.push(
            "  keychain  login-keychain items com.source-inc.gents.store-key and com.source-inc.gents.identity, when this home uses them"
                .to_owned(),
        );
    }
    let client = gents_store::local::home::gents_config_dir().ok();
    if let Some(client) = &client {
        lines.push(format!(
            "  client    {} (which home `gents` opens)",
            client.display()
        ));
    }
    if let Some(log) = stderr_log {
        lines.push(format!("  logs      {}", log.display()));
    }
    lines.push("To remove the rest:".to_owned());
    if macos {
        lines.push(format!(
            "  home and desktop app state: gents runtime desktop reset --home {} (previews first; quit the desktop app)",
            shell_word(home)
        ));
        lines.push(
            "  keychain: delete the com.source-inc.gents items in Keychain Access".to_owned(),
        );
    } else {
        lines.push(format!("  home: delete {}", home.display()));
    }
    if let Some(client) = &client {
        lines.push(format!("  client: delete {}", client.display()));
    }
    if let Some(log) = stderr_log.and_then(Path::parent) {
        lines.push(format!("  logs: delete {}", log.display()));
    }
    lines.push(format!(
        "To run the service again: gents runtime service install --home {0}, then gents runtime service start --home {0}",
        shell_word(home)
    ));
    lines
}
