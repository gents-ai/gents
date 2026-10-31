use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DesktopBuild {
    pub binary: PathBuf,
    pub target_dir: PathBuf,
    pub profile: OsString,
    pub target: Option<OsString>,
}

impl DesktopBuild {
    pub fn for_cli(cli: &Path, build_target: &str, executable_suffix: &str) -> io::Result<Self> {
        let invalid = || {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected an absolute Cargo gents binary path with a profile directory",
            )
        };
        if !cli.is_absolute()
            || cli.file_name() != Some(OsStr::new(&format!("gents{executable_suffix}")))
            || build_target.is_empty()
        {
            return Err(invalid());
        }
        let profile_dir = cli.parent().ok_or_else(invalid)?;
        let directory_name = profile_dir.file_name().ok_or_else(invalid)?;
        let output_root = profile_dir.parent().ok_or_else(invalid)?;
        // Cargo inserts the explicit target between target-dir and profile.
        // Use the invoking binary's build metadata, not a triple-shaped guess.
        let (target_dir, target) = if output_root.file_name() == Some(OsStr::new(build_target)) {
            (
                output_root.parent().ok_or_else(invalid)?,
                Some(OsString::from(build_target)),
            )
        } else {
            (output_root, None)
        };
        let profile = if directory_name == OsStr::new("debug") {
            OsString::from("dev")
        } else {
            directory_name.to_owned()
        };
        Ok(Self {
            binary: profile_dir.join(format!("gents-desktop{executable_suffix}")),
            target_dir: target_dir.to_owned(),
            profile,
            target,
        })
    }

    pub fn cargo_args(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = [
            "build",
            "-p",
            "gents-desktop",
            "--bin",
            "gents-desktop",
            "--locked",
            "--message-format=json",
            "--profile",
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        args.push(self.profile.clone());
        args.push("--target-dir".into());
        args.push(self.target_dir.clone().into_os_string());
        if let Some(target) = &self.target {
            args.push("--target".into());
            args.push(target.clone());
        }
        args
    }

    pub fn reported_binary(&self, reported: Option<&Path>) -> io::Result<PathBuf> {
        let reported = reported.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "Cargo did not report a gents-desktop executable",
            )
        })?;
        let expected = self.binary.canonicalize()?;
        let reported = reported.canonicalize()?;
        if reported != expected || !reported.is_file() {
            return Err(io::Error::new(io::ErrorKind::InvalidData,
                "Cargo reported a desktop executable outside the invoking CLI's profile and target directory"));
        }
        Ok(reported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        std::env::temp_dir().join("gents desktop build layout")
    }

    #[test]
    fn native_profiles_keep_the_invoking_target_directory() {
        for (directory, profile) in [
            ("debug", "dev"),
            ("release", "release"),
            ("fast-dev", "fast-dev"),
            ("custom-profile", "custom-profile"),
        ] {
            let target_dir = root().join("custom-output");
            let cli = target_dir.join(directory).join("gents");
            let build = DesktopBuild::for_cli(&cli, "aarch64-apple-darwin", "").unwrap();
            assert_eq!(build.target_dir, target_dir);
            assert_eq!(build.profile, profile);
            assert_eq!(build.target, None);
            assert_eq!(build.binary, cli.with_file_name("gents-desktop"));
            let args = build.cargo_args();
            assert!(args.contains(&OsString::from("--locked")));
            assert!(args
                .windows(2)
                .any(|pair| pair == [OsString::from("--profile"), OsString::from(profile)]));
            assert!(!args.contains(&OsString::from("--target")));
        }
    }

    #[test]
    fn explicit_target_uses_the_target_directory_above_the_triple() {
        let target_dir = root().join("custom-output");
        let target = "x86_64-unknown-linux-gnu";
        for directory in ["debug", "release", "fast-dev"] {
            let cli = target_dir.join(target).join(directory).join("gents");
            let build = DesktopBuild::for_cli(&cli, target, "").unwrap();
            assert_eq!(build.target_dir, target_dir);
            assert_eq!(build.target, Some(target.into()));
            assert_eq!(build.binary, cli.with_file_name("gents-desktop"));
            assert!(build
                .cargo_args()
                .windows(2)
                .any(|pair| pair == [OsString::from("--target"), OsString::from(target)]));
        }
    }

    #[test]
    fn windows_suffix_and_target_are_retained() {
        let target = "x86_64-pc-windows-msvc";
        let cli = root().join(target).join("release").join("gents.exe");
        let build = DesktopBuild::for_cli(&cli, target, ".exe").unwrap();
        assert_eq!(build.binary, cli.with_file_name("gents-desktop.exe"));
        assert_eq!(build.target, Some(target.into()));
    }

    #[test]
    fn target_directory_names_are_not_guessed_as_triples() {
        let target_dir = root().join("my-target-like-cache");
        let build = DesktopBuild::for_cli(
            &target_dir.join("fast-dev/gents"),
            "aarch64-apple-darwin",
            "",
        )
        .unwrap();
        assert_eq!(build.target_dir, target_dir);
        assert_eq!(build.target, None);
    }

    #[test]
    fn malformed_or_non_cli_paths_are_rejected() {
        for cli in [
            PathBuf::from("target/debug/gents"),
            root().join("debug/not-gents"),
            root().join("debug/gents-deadbeef"),
        ] {
            assert!(DesktopBuild::for_cli(&cli, "aarch64-apple-darwin", "").is_err());
        }
        assert!(DesktopBuild::for_cli(&root().join("debug/gents"), "", "").is_err());
    }

    #[test]
    fn only_the_reported_existing_sibling_is_accepted() {
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = Cleanup(root().join(format!("{}-{stamp}", std::process::id())));
        let build =
            DesktopBuild::for_cli(&temp.0.join("fast-dev/gents"), "aarch64-apple-darwin", "")
                .unwrap();
        std::fs::create_dir_all(build.binary.parent().unwrap()).unwrap();
        std::fs::write(&build.binary, b"existing sibling").unwrap();
        assert_eq!(
            build.reported_binary(None).unwrap_err().kind(),
            io::ErrorKind::NotFound,
            "an old sibling cannot replace a missing Cargo artifact receipt"
        );
        let wrong_profile = temp.0.join("debug/gents-desktop");
        std::fs::create_dir_all(wrong_profile.parent().unwrap()).unwrap();
        std::fs::write(&wrong_profile, b"wrong profile").unwrap();
        assert_eq!(
            build
                .reported_binary(Some(&wrong_profile))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            build.reported_binary(Some(&build.binary)).unwrap(),
            build.binary.canonicalize().unwrap()
        );
        std::fs::remove_file(&build.binary).unwrap();
        assert_eq!(
            build
                .reported_binary(Some(&build.binary))
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
    }
}
