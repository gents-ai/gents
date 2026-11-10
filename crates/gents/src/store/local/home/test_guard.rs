use std::path::Path;

/// Refuses, in a test build, a resolved home inside the operator's real Gents
/// state: `$HOME/.gents` or the platform data folder's `gents` directory.
/// Tests own throwaway homes; one that forgot to pass them would otherwise
/// read or replace the operator's runtime, desktop client or CLI identity.
/// Test builds are this crate's unit tests and any build enabling
/// `test-home-guard`, which the workspace's dev-dependencies do, so the
/// binaries those tests spawn are guarded too. A redirected `HOME` inside the
/// temporary directory moves these locations there and is accepted.
#[cfg(any(test, feature = "test-home-guard"))]
pub fn refuse_real_gents_home(resolved: &Path, variable: &str) {
    if let Some(real) = real_home_containing(resolved, &std::env::temp_dir(), real_gents_homes()) {
        panic!(
            "test build resolved {} inside the operator's real Gents state {}; give this process throwaway homes (set {variable} and HOME to temporary directories)",
            resolved.display(),
            real.display()
        );
    }
}

/// The real Gents home outside `temp` that contains `resolved`. Paths are
/// compared with their symlinks resolved: on macOS `temp_dir()` is spelled
/// `/var/folders/...` while a canonicalized temporary `HOME` is
/// `/private/var/folders/...`, the same folder.
#[cfg(any(test, feature = "test-home-guard"))]
fn real_home_containing(
    resolved: &Path,
    temp: &Path,
    mut reals: impl Iterator<Item = std::path::PathBuf>,
) -> Option<std::path::PathBuf> {
    let temp = canonical(temp);
    let resolved = canonical(resolved);
    reals.find(|real| {
        let real = canonical(real);
        !real.starts_with(&temp) && resolved.starts_with(&real)
    })
}

#[cfg(not(any(test, feature = "test-home-guard")))]
#[inline(always)]
pub fn refuse_real_gents_home(_resolved: &Path, _variable: &str) {}

#[cfg(any(test, feature = "test-home-guard"))]
fn real_gents_homes() -> impl Iterator<Item = std::path::PathBuf> {
    [
        dirs::home_dir().map(|home| home.join(".gents")),
        dirs::data_local_dir().map(|data| data.join("gents")),
    ]
    .into_iter()
    .flatten()
}

/// `path` made absolute with symlinks resolved as far as it exists; the part
/// that does not exist yet is appended unchanged.
#[cfg(any(test, feature = "test-home-guard"))]
fn canonical(path: &Path) -> std::path::PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut existing = absolute.as_path();
    let mut missing = Vec::new();
    loop {
        if let Ok(found) = std::fs::canonicalize(existing) {
            return missing
                .iter()
                .rev()
                .fold(found, |path, part| path.join(part));
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                missing.push(name.to_owned());
                existing = parent;
            }
            _ => return absolute,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_temporary_home_is_accepted() {
        refuse_real_gents_home(&std::env::temp_dir().join("gents-home"), "GENTS_HOME");
    }

    #[test]
    #[should_panic(expected = "inside the operator's real Gents state")]
    fn the_real_default_home_is_refused() {
        let real = real_gents_homes()
            .find(|path| !canonical(path).starts_with(canonical(&std::env::temp_dir())))
            .expect("an operator home outside the temporary directory");
        refuse_real_gents_home(&real.join("desktop"), "GENTS_DESKTOP_HOME");
    }

    /// A temporary `HOME` spelled through a symlink (macOS's `/var` to
    /// `/private/var`) is still inside the temporary directory.
    #[cfg(unix)]
    #[test]
    fn a_home_inside_the_temporary_directory_by_another_spelling_is_accepted() {
        let root = tempfile::tempdir().unwrap();
        let temp = root.path().join("temp");
        std::fs::create_dir(&temp).unwrap();
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(&temp, &alias).unwrap();
        let home = temp.join("home");
        let real = home.join(".gents");
        assert_eq!(
            real_home_containing(&real.join("desktop"), &alias, std::iter::once(real.clone())),
            None
        );
        let outside = root.path().join("operator").join(".gents");
        assert_eq!(
            real_home_containing(
                &outside.join("desktop"),
                &alias,
                std::iter::once(outside.clone())
            ),
            Some(outside)
        );
    }
}
