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
    let temp = absolute(&std::env::temp_dir());
    let resolved_absolute = absolute(resolved);
    for real in real_gents_homes().map(|path| absolute(&path)) {
        if !real.starts_with(&temp) && resolved_absolute.starts_with(&real) {
            panic!(
                "test build resolved {} inside the operator's real Gents state {}; give this process throwaway homes (set {variable} and HOME to temporary directories)",
                resolved.display(),
                real.display()
            );
        }
    }
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

#[cfg(any(test, feature = "test-home-guard"))]
fn absolute(path: &Path) -> std::path::PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
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
            .find(|path| !absolute(path).starts_with(absolute(&std::env::temp_dir())))
            .expect("an operator home outside the temporary directory");
        refuse_real_gents_home(&real.join("desktop"), "GENTS_DESKTOP_HOME");
    }
}
