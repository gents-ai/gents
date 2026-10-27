use std::collections::BTreeSet;
use std::ffi::CStr;
use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
use std::path::{Path, PathBuf};

use anyhow::{ensure, Context as _, Result};

/// These host credential namespaces are excluded even when a workspace contains
/// the user's home. Resolved links are also excluded; existing hard-link aliases
/// refuse launch rather than leaving a second readable name. Host-side namespace
/// replacement after inspection is outside the child sandbox's authority.
const USER_CREDENTIALS: &[&str] = &[
    ".ssh",
    ".aws",
    ".config/gh",
    ".netrc",
    ".gnupg",
    "Library/Keychains",
    ".cargo/credentials",
    ".cargo/credentials.toml",
    ".npmrc",
    ".docker/config.json",
    ".kube",
    ".azure",
    ".config/gcloud",
];

#[derive(Debug)]
pub(super) struct CredentialProtection {
    paths: BTreeSet<PathBuf>,
}

pub(super) async fn capture() -> Result<CredentialProtection> {
    let runtime_home = crate::tool_call_lifecycle::runtime::current_tool_runtime_context()
        .and_then(|context| context.runtime_home);
    let approval_home = crate::plugin::approval::workspace::command_context_home();
    tokio::task::spawn_blocking(move || capture_native(runtime_home, approval_home))
        .await
        .context("credential inspection worker failed")?
}

fn capture_native(
    runtime_home: Option<PathBuf>,
    approval_home: Option<PathBuf>,
) -> Result<CredentialProtection> {
    let mut homes = vec![account_home()?];
    if let Some(home) = dirs::home_dir() {
        homes.push(home);
    }
    let runtimes = runtime_home
        .into_iter()
        .chain(approval_home)
        .collect::<BTreeSet<_>>();
    let mut paths = homes
        .iter()
        .flat_map(|home| USER_CREDENTIALS.iter().map(move |name| home.join(name)))
        .collect::<Vec<_>>();
    // Inactive homes may have a different configuration schema. Their standard
    // credential paths do not require decoding configuration or walking data.
    for home in homes.iter().map(|home| home.join(".gents")).chain(
        std::env::var_os("GENTS_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from),
    ) {
        paths.extend(standard_runtime_credentials(&home));
    }
    if let Some(config) = std::env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        let config = PathBuf::from(config);
        paths.extend([config.join("gh"), config.join("gcloud")]);
    }
    if let Some(cargo) = std::env::var_os("CARGO_HOME").filter(|value| !value.is_empty()) {
        let cargo = PathBuf::from(cargo);
        paths.extend([cargo.join("credentials"), cargo.join("credentials.toml")]);
    }
    for home in runtimes {
        paths.extend(runtime_credentials(&home)?);
    }
    CredentialProtection::capture(paths)
}

fn runtime_credentials(home: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = standard_runtime_credentials(home);
    match std::fs::symlink_metadata(crate::home::init_config_path(home)) {
        Ok(_) => {
            let config = crate::home::read_init_config::<serde_json::Value, serde_json::Value>(home)?
                .context("Runtime identity configuration changed during command isolation. Retry the command.")?;
            paths.extend(config.key_path.map(PathBuf::from));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("inspecting runtime identity configuration"),
    }
    Ok(paths)
}

fn standard_runtime_credentials(home: &Path) -> Vec<PathBuf> {
    vec![
        crate::home::keys_dir(home),
        home.join(crate::home::P2P_SECRET_KEY_FILE_NAME),
        crate::home::init_config_path(home),
    ]
}

impl CredentialProtection {
    fn capture(paths: impl IntoIterator<Item = PathBuf>) -> Result<Self> {
        let mut protection = Self {
            paths: BTreeSet::new(),
        };
        let mut inspected = BTreeSet::new();
        for path in paths {
            let path = std::path::absolute(path).context("resolving credential exclusion")?;
            protection.paths.insert(path.clone());
            let resolved = resolve_missing(&path)?;
            protection.paths.insert(resolved.clone());
            protection
                .inspect(&resolved, &mut inspected)
                .with_context(|| format!("inspecting credential exclusion {}", path.display()))?;
        }
        Ok(protection)
    }

    pub(super) async fn revalidate(&self) -> Result<()> {
        let current = capture().await?;
        ensure!(
            current.paths == self.paths,
            "Credential exclusions changed while the command awaited approval. Retry the command."
        );
        Ok(())
    }

    pub(super) fn protect_launch(&self, args: &mut Vec<String>) -> Result<()> {
        let profile = args
            .get_mut(1)
            .context("command sandbox profile is unavailable")?;
        let mut parameters = Vec::new();
        self.apply(profile, &mut parameters)?;
        let boundary = args
            .iter()
            .position(|arg| arg == "--")
            .context("command sandbox boundary is unavailable")?;
        args.splice(boundary..boundary, parameters);
        Ok(())
    }

    fn inspect(&mut self, path: &Path, inspected: &mut BTreeSet<PathBuf>) -> Result<()> {
        if !inspected.insert(path.to_owned()) {
            return Ok(());
        }
        let metadata = match std::fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error).context("inspecting credential exclusion"),
        };
        if metadata.is_symlink() {
            let target = path.canonicalize().context("resolving credential link")?;
            self.paths.insert(target.clone());
            self.inspect(&target, inspected)?;
        } else if metadata.is_dir() {
            for entry in std::fs::read_dir(path).context("inspecting credential directory")? {
                self.inspect(&entry?.path(), inspected)?;
            }
        } else if metadata.is_file() {
            ensure!(metadata.nlink() == 1, "A credential file has a hard-link alias. Remove the alias outside this session before running a bounded command.");
        }
        Ok(())
    }

    pub(super) fn apply(&self, profile: &mut String, parameters: &mut Vec<String>) -> Result<()> {
        for (index, path) in self.paths.iter().enumerate() {
            profile.push_str(&format!("\n(deny file-read* file-write* file-link (subpath (param \"CREDENTIAL_{index}\")))\n"));
            parameters.push(format!(
                "-DCREDENTIAL_{index}={}",
                path.to_str()
                    .context("credential path is not valid UTF-8")?
            ));
        }
        let parents = self
            .paths
            .iter()
            .flat_map(|path| path.ancestors().skip(1))
            .collect::<BTreeSet<_>>();
        for (index, parent) in parents.iter().enumerate() {
            profile.push_str(&format!(
                "\n(deny file-write-unlink (literal (param \"CREDENTIAL_PARENT_{index}\")))\n"
            ));
            parameters.push(format!(
                "-DCREDENTIAL_PARENT_{index}={}",
                parent
                    .to_str()
                    .context("credential ancestor is not valid UTF-8")?
            ));
        }
        Ok(())
    }
}

fn resolve_missing(path: &Path) -> Result<PathBuf> {
    let mut ancestor = path;
    let mut suffix = Vec::new();
    loop {
        match ancestor.canonicalize() {
            Ok(mut path) => {
                for name in suffix.into_iter().rev() {
                    path.push(name);
                }
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                ensure!(
                    !std::fs::symlink_metadata(ancestor)
                        .is_ok_and(|metadata| metadata.is_symlink()),
                    "A credential exclusion contains an unresolved symbolic link."
                );
                suffix.push(
                    ancestor
                        .file_name()
                        .context("credential path has no existing ancestor")?,
                );
                ancestor = ancestor.parent().context("credential path has no parent")?;
            }
            Err(error) => return Err(error).context("resolving credential namespace"),
        }
    }
}

/// HOME is a child environment setting, not the OS account boundary. Include the
/// account record as well so an inherited or tool-selected HOME cannot expose
/// the account's credentials. getpwuid_r owns only caller-provided storage.
fn account_home() -> Result<PathBuf> {
    let mut size = 16 * 1024;
    loop {
        let mut buffer = vec![0_u8; size];
        let mut record = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut result = std::ptr::null_mut();
        // SAFETY: passwd and buffer are writable for their declared sizes; the
        // returned strings remain valid until this caller-owned buffer drops.
        let status = unsafe {
            libc::getpwuid_r(
                libc::getuid(),
                record.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if status == libc::ERANGE && size < 1024 * 1024 {
            size *= 2;
            continue;
        }
        ensure!(
            status == 0 && !result.is_null(),
            "Cannot resolve the OS account home for command isolation."
        );
        // SAFETY: successful getpwuid_r initialized the passwd and its pw_dir.
        let pointer = unsafe { (*result).pw_dir };
        ensure!(!pointer.is_null(), "OS account home is unavailable.");
        // SAFETY: pw_dir is non-null and points into the live passwd buffer.
        let directory = unsafe { CStr::from_ptr(pointer) };
        let directory = PathBuf::from(std::ffi::OsStr::from_bytes(directory.to_bytes()));
        ensure!(
            directory.is_absolute(),
            "OS account home is not an absolute path."
        );
        return Ok(directory);
    }
}

#[cfg(test)]
#[path = "command_credentials_tests.rs"]
mod tests;
