use std::path::PathBuf;

#[cfg(target_os = "linux")]
pub mod linux;

pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxBackend {
    None,
    MacosSeatbelt,
    LinuxLandlock,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxSandboxConfig {
    pub writable_root: Option<PathBuf>,
    pub allow_network: bool,
    pub deny_hard_links: bool,
    pub denied_paths: Vec<PathBuf>,
    /// True only for Unrestricted commands guarded solely against operator
    /// permission-state tampering (`linux_operator_state_guard`).
    pub allow_writes_outside_denied: bool,
}

pub fn host_sandbox_backend() -> SandboxBackend {
    #[cfg(target_os = "macos")]
    {
        if std::path::Path::new(SANDBOX_EXEC).exists() {
            SandboxBackend::MacosSeatbelt
        } else {
            SandboxBackend::None
        }
    }
    #[cfg(target_os = "linux")]
    {
        if linux::landlock_abi_version().is_some() {
            SandboxBackend::LinuxLandlock
        } else {
            SandboxBackend::None
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        SandboxBackend::None
    }
}

pub fn workspace_write_sandbox_enforced() -> bool {
    host_sandbox_backend() != SandboxBackend::None
}
