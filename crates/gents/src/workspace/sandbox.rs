use std::path::Path;

pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

#[cfg(target_os = "macos")]
pub fn workspace_write_sandbox_enforced() -> bool {
    Path::new(SANDBOX_EXEC).exists()
}

#[cfg(not(target_os = "macos"))]
pub fn workspace_write_sandbox_enforced() -> bool {
    false
}
