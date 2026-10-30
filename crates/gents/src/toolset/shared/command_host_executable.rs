use std::path::{Path, PathBuf};

/// PATH lookup + canonicalize; admission is side-effect free (rustup shim
/// resolution reads on-disk selection state rather than shelling out, so the
/// spawned/digested path is the actual host toolchain binary, never a shim
/// that exits before initialize under an LSP workspace tempdir).
pub(crate) fn admit_host_executable(
    command: &str,
    tool_root: &Path,
) -> std::result::Result<PathBuf, crate::toolset::denial::DenialReason> {
    use crate::toolset::denial::DenialReason;
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return Err(DenialReason::WorkspaceExecutable);
    }
    let candidate =
        if trimmed.contains('/') || trimmed.contains('\\') || Path::new(trimmed).is_absolute() {
            PathBuf::from(trimmed)
        } else {
            which_on_host_path(trimmed).ok_or(DenialReason::WorkspaceExecutable)?
        };
    let canonical = std::fs::canonicalize(&candidate).unwrap_or(candidate);
    let root = std::fs::canonicalize(tool_root).unwrap_or_else(|_| tool_root.to_path_buf());
    if canonical.starts_with(&root) {
        return Err(DenialReason::WorkspaceExecutable);
    }
    if !canonical.is_file() {
        return Err(DenialReason::WorkspaceExecutable);
    }
    resolve_rustup_proxy_path(trimmed, canonical, &root).ok_or(DenialReason::WorkspaceExecutable)
}

fn resolve_rustup_proxy_path(
    requested: &str,
    admitted: PathBuf,
    tool_root: &Path,
) -> Option<PathBuf> {
    let Some(name) = Path::new(requested)
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.trim_end_matches(".exe"))
    else {
        return Some(admitted);
    };
    if name != "rust-analyzer"
        || admitted
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.trim_end_matches(".exe"))
            != Some("rustup")
    {
        return Some(admitted);
    }

    let rustup_home = std::env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".rustup")))?;
    let settings = read_toml(&rustup_home.join("settings.toml"));
    let channel = std::env::var("RUSTUP_TOOLCHAIN")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| rustup_override_channel(settings.as_ref(), tool_root))
        .or_else(|| rust_toolchain_file_channel(tool_root))
        .or_else(|| {
            settings
                .as_ref()?
                .get("default_toolchain")?
                .as_str()
                .map(ToOwned::to_owned)
        })?;
    let toolchains = rustup_home.join("toolchains");
    let exact = toolchains.join(&channel);
    let toolchain = if exact.is_dir() {
        exact
    } else {
        let mut matches = std::fs::read_dir(&toolchains)
            .ok()?
            .flatten()
            .filter(|entry| entry.path().is_dir())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|value| value.starts_with(&format!("{channel}-")))
            })
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        matches.sort();
        matches.pop()?
    };
    let executable = toolchain.join("bin").join(if cfg!(windows) {
        "rust-analyzer.exe"
    } else {
        name
    });
    let canonical = std::fs::canonicalize(executable).ok()?;
    (!canonical.starts_with(tool_root) && canonical.is_file()).then_some(canonical)
}

fn read_toml(path: &Path) -> Option<toml::Value> {
    std::fs::read_to_string(path).ok()?.parse().ok()
}

fn rustup_override_channel(settings: Option<&toml::Value>, tool_root: &Path) -> Option<String> {
    let overrides = settings?.get("overrides")?.as_table()?;
    tool_root.ancestors().find_map(|directory| {
        let key = directory.to_string_lossy();
        overrides
            .get(key.as_ref())
            .and_then(toml::Value::as_str)
            .map(ToOwned::to_owned)
    })
}

fn rust_toolchain_file_channel(tool_root: &Path) -> Option<String> {
    for directory in tool_root.ancestors() {
        let toml_path = directory.join("rust-toolchain.toml");
        if toml_path.is_file() {
            return read_toml(&toml_path)?
                .get("toolchain")?
                .get("channel")?
                .as_str()
                .map(ToOwned::to_owned);
        }
    }
    None
}

fn which_on_host_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}
