use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DesktopBuild {
    pub binary: PathBuf,
    pub target_dir: PathBuf,
    pub profile: OsString,
    pub target: Option<OsString>,
}

/// What a `--message-format=json` stream said about the desktop binary.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ArtifactMessages {
    pub executable: Option<PathBuf>,
    /// `message.rendered` diagnostics, in stream order, for the caller to relay.
    pub rendered: Vec<String>,
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

    /// The Cargo invocation that rebuilds the desktop sibling of the invoking
    /// CLI, with the inherited build target neutralized for a native build.
    ///
    /// A native build must not inherit `CARGO_BUILD_TARGET`: Cargo would then
    /// place `gents-desktop` under `<target-dir>/<triple>/<profile>` while
    /// `reported_binary` expects the sibling of the running CLI, so the check
    /// would fail closed instead of reproducing the CLI that is already
    /// running.
    ///
    /// This is partial support, not a full neutralization of every config
    /// source. The environment is the only inherited source this workspace
    /// has (`.cargo/config.toml` sets no `build.target`), so removing
    /// `CARGO_BUILD_TARGET` covers it. A `build.target` set by a global or
    /// parent `.cargo/config.toml` is *not* overridden: Cargo offers no usable
    /// override here — `--config build.target=` is rejected ("target was
    /// empty") and `build.target=[]` only merges into an array-valued base —
    /// so such a build lands outside the expected sibling and fails closed in
    /// `reported_binary` rather than silently returning a different artifact.
    ///
    /// A cross build keeps the explicit `--target`, which outranks both the
    /// environment and the config file.
    pub fn cargo_command(&self, workspace_root: &Path) -> Command {
        let mut command = Command::new("cargo");
        command.current_dir(workspace_root).args(self.cargo_args());
        if self.target.is_none() {
            command.env_remove("CARGO_BUILD_TARGET");
        }
        command
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

/// Read a Cargo `--message-format=json` stream: keep the desktop binary's
/// artifact path and the rendered diagnostics, and ignore every other message.
/// A non-JSON line is an error, because Cargo promises one JSON object per line
/// and a stray line means the stream is not the one being parsed.
pub(crate) fn parse_artifact_messages(stdout: &[u8]) -> io::Result<ArtifactMessages> {
    let text = std::str::from_utf8(stdout).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Cargo artifact messages are not UTF-8: {error}"),
        )
    })?;
    let mut messages = ArtifactMessages::default();
    for line in text.lines() {
        let message: serde_json::Value = serde_json::from_str(line).map_err(|error| {
            // Cargo can emit a very long line; keep the diagnostic readable.
            let shown: String = line.chars().take(200).collect();
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("decoding Cargo artifact message {shown:?}: {error}"),
            )
        })?;
        if let Some(rendered) = message
            .pointer("/message/rendered")
            .and_then(|v| v.as_str())
        {
            messages.rendered.push(rendered.to_owned());
        }
        if message["reason"] == "compiler-artifact"
            && message["target"]["name"] == "gents-desktop"
            && message["target"]["kind"]
                .as_array()
                .is_some_and(|kinds| kinds.iter().any(|kind| kind == "bin"))
        {
            if let Some(path) = message["executable"].as_str() {
                if messages.executable.is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Cargo reported multiple gents-desktop executables",
                    ));
                }
                messages.executable = Some(PathBuf::from(path));
            }
        }
    }
    Ok(messages)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        std::env::temp_dir().join("gents desktop build layout")
    }

    fn os_args(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
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
    fn native_cargo_args_are_exact() {
        let target_dir = root().join("custom-output");
        let build = DesktopBuild::for_cli(
            &target_dir.join("fast-dev/gents"),
            "aarch64-apple-darwin",
            "",
        )
        .unwrap();
        assert_eq!(
            build.cargo_args(),
            os_args(&[
                "build",
                "-p",
                "gents-desktop",
                "--bin",
                "gents-desktop",
                "--locked",
                "--message-format=json",
                "--profile",
                "fast-dev",
                "--target-dir",
                target_dir.to_str().unwrap(),
            ])
        );
    }

    #[test]
    fn target_cargo_args_are_exact() {
        let target_dir = root().join("custom-output");
        let target = "x86_64-unknown-linux-gnu";
        let build =
            DesktopBuild::for_cli(&target_dir.join(target).join("release/gents"), target, "")
                .unwrap();
        assert_eq!(
            build.cargo_args(),
            os_args(&[
                "build",
                "-p",
                "gents-desktop",
                "--bin",
                "gents-desktop",
                "--locked",
                "--message-format=json",
                "--profile",
                "release",
                "--target-dir",
                target_dir.to_str().unwrap(),
                "--target",
                target,
            ])
        );
    }

    #[test]
    fn native_build_neutralizes_the_inherited_build_target() {
        let target_dir = root().join("custom-output");
        let native =
            DesktopBuild::for_cli(&target_dir.join("debug/gents"), "aarch64-apple-darwin", "")
                .unwrap();
        let command = native.cargo_command(Path::new("/workspace"));
        assert_eq!(
            command.get_envs().collect::<Vec<_>>(),
            vec![(OsStr::new("CARGO_BUILD_TARGET"), None)],
            "a native rebuild must not inherit CARGO_BUILD_TARGET"
        );
        assert_eq!(command.get_current_dir(), Some(Path::new("/workspace")));
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            native
                .cargo_args()
                .iter()
                .map(OsString::as_os_str)
                .collect::<Vec<_>>()
        );

        let target = "x86_64-unknown-linux-gnu";
        let cross = DesktopBuild::for_cli(&target_dir.join(target).join("debug/gents"), target, "")
            .unwrap();
        let command = cross.cargo_command(Path::new("/workspace"));
        assert_eq!(
            command.get_envs().count(),
            0,
            "an explicit --target already outranks the environment"
        );
        assert!(command
            .get_args()
            .collect::<Vec<_>>()
            .windows(2)
            .any(|pair| pair == [OsStr::new("--target"), OsStr::new(target)]));
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
    fn a_target_directory_named_like_the_triple_still_lands_on_the_sibling() {
        // The derivation cannot tell a custom target dir named exactly like the
        // build target from a real cross layout; both must resolve to the same
        // artifact path, which is what `reported_binary` compares against.
        let target = "aarch64-apple-darwin";
        let parent = root().join("named-like-a-triple");
        let cli = parent.join(target).join("fast-dev/gents");
        let build = DesktopBuild::for_cli(&cli, target, "").unwrap();
        assert_eq!(build.target, Some(target.into()));
        assert_eq!(build.target_dir, parent);
        assert_eq!(build.binary, cli.with_file_name("gents-desktop"));
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
    fn artifact_messages_keep_only_the_desktop_binary_and_rendered_diagnostics() {
        let stream = [
            r#"{"reason":"compiler-artifact","target":{"name":"gents","kind":["bin"]},"executable":"/t/debug/gents"}"#,
            r#"{"reason":"build-script-executed","package_id":"gents-desktop 0.1.0"}"#,
            r#"{"reason":"compiler-message","message":{"rendered":"warning: unused\n"}}"#,
            r#"{"reason":"compiler-artifact","target":{"name":"gents-desktop","kind":["lib"]},"executable":null}"#,
            r#"{"reason":"compiler-artifact","target":{"name":"gents-desktop","kind":["bin"]},"executable":null}"#,
            r#"{"reason":"compiler-artifact","target":{"name":"gents-desktop","kind":["bin"]},"executable":"/t/debug/gents-desktop"}"#,
            r#"{"reason":"build-finished","success":true}"#,
        ]
        .join("\n");
        let messages = parse_artifact_messages(stream.as_bytes()).unwrap();
        assert_eq!(
            messages.executable,
            Some(PathBuf::from("/t/debug/gents-desktop"))
        );
        assert_eq!(messages.rendered, vec!["warning: unused\n".to_owned()]);
    }

    #[test]
    fn artifact_messages_without_a_desktop_executable_report_none() {
        let stream = [
            r#"{"reason":"compiler-artifact","target":{"name":"gents-desktop","kind":["bin"]},"executable":null}"#,
            r#"{"reason":"build-finished","success":true}"#,
        ]
        .join("\n");
        let messages = parse_artifact_messages(stream.as_bytes()).unwrap();
        assert_eq!(messages.executable, None);
        assert!(messages.rendered.is_empty());
        assert_eq!(
            parse_artifact_messages(b"").unwrap(),
            ArtifactMessages::default()
        );
    }

    #[test]
    fn artifact_messages_reject_stray_lines_and_duplicate_executables() {
        let stray = "Compiling gents-desktop v0.1.0\n";
        assert_eq!(
            parse_artifact_messages(stray.as_bytes())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            parse_artifact_messages(&[0xff, 0xfe]).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        let duplicate = [
            r#"{"reason":"compiler-artifact","target":{"name":"gents-desktop","kind":["bin"]},"executable":"/t/a/gents-desktop"}"#,
            r#"{"reason":"compiler-artifact","target":{"name":"gents-desktop","kind":["bin"]},"executable":"/t/b/gents-desktop"}"#,
        ]
        .join("\n");
        assert_eq!(
            parse_artifact_messages(duplicate.as_bytes())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
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
