use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

use crate::cli::args::{ClientConfigCommand, ServeArgs};
use crate::home_state::{read_init_config, resolve_home_dir};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub(crate) enum InterfaceClient {
    #[default]
    Codex,
    Grok,
}

impl InterfaceClient {
    fn name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Grok => "grok",
        }
    }
}

/// The interface a home opens. It lives in a file because `gents` needs it
/// before the runtime is up and the native service definition carries no flags.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClientSettings {
    pub(crate) client: InterfaceClient,
}

fn client_settings_path(home: &Path) -> PathBuf {
    home.join(gents::store::local::home::CLIENT_CONFIG_FILE_NAME)
}

impl ClientSettings {
    pub(crate) fn read(home: &Path) -> Result<Self> {
        let path = client_settings_path(home);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default())
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("reading client settings {}", path.display()))
            }
        };
        serde_json::from_slice(&bytes)
            .with_context(|| format!("decoding client settings {}", path.display()))
    }

    /// Writes the setting, or removes the file for the default. Returns whether
    /// the persisted setting changed. A file that cannot be read is replaced, since
    /// `config client set` is how a person repairs it.
    pub(crate) fn persist(&self, home: &Path) -> Result<bool> {
        if Self::read(home).is_ok_and(|current| current == *self) {
            return Ok(false);
        }
        let path = client_settings_path(home);
        if *self == Self::default() {
            std::fs::remove_file(&path)
                .with_context(|| format!("removing client settings {}", path.display()))?;
        } else {
            let contents = serde_json::to_vec(self).context("encoding client settings")?;
            crate::native_service::atomic_write(&path, &contents)
                .with_context(|| format!("writing client settings {}", path.display()))?;
        }
        Ok(true)
    }
}

pub(crate) fn ensure_client_supported(client: InterfaceClient, unix: bool) -> Result<()> {
    if client == InterfaceClient::Grok && !unix {
        bail!("the grok interface requires Unix; use codex on this platform");
    }
    Ok(())
}

/// Prepared startup keeps the shim off, as `prepare_runtime` set it, so it reads no setting:
/// a corrupt file must not stop a path that would ignore what it says.
pub(crate) fn apply_home_client_setting(
    args: &mut ServeArgs,
    home: &Path,
    prepared: bool,
    unix: bool,
) -> Result<()> {
    if !prepared {
        apply_client_setting(args, &ClientSettings::read(home)?, unix);
    }
    Ok(())
}

/// A home set to Grok serves as if `--grok-shim` were passed.
fn apply_client_setting(args: &mut ServeArgs, settings: &ClientSettings, unix: bool) {
    if settings.client == InterfaceClient::Grok && unix {
        args.grok_shim = true;
    }
}

/// Reads and writes only the home's file, so it works with no runtime and
/// leaves restarting to the operator.
pub(crate) fn config_client(command: ClientConfigCommand) -> Result<()> {
    match command {
        ClientConfigCommand::Set { client, home } => {
            let home = initialized_home(home.as_deref())?;
            ensure_client_supported(client, cfg!(unix))?;
            ClientSettings { client }.persist(&home)?;
            eprintln!(
                "client set to {}; run `gents runtime service restart --home {}` to apply it",
                client.name(),
                crate::shell_word(&home)
            );
        }
        ClientConfigCommand::Show { home } => {
            let home = initialized_home(home.as_deref())?;
            println!("{}", ClientSettings::read(&home)?.client.name());
        }
    }
    Ok(())
}

fn initialized_home(explicit: Option<&Path>) -> Result<PathBuf> {
    let home = resolve_home_dir(explicit);
    if read_init_config(&home)?.is_none() {
        bail!(
            "home {} is not initialized; run `gents --home {}` to set it up",
            home.display(),
            crate::shell_word(&home)
        );
    }
    Ok(home)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Command};
    use clap::Parser;
    use std::fs;

    const GROK: ClientSettings = ClientSettings {
        client: InterfaceClient::Grok,
    };
    const CODEX: ClientSettings = ClientSettings {
        client: InterfaceClient::Codex,
    };

    fn serve_args(extra: &[&str]) -> ServeArgs {
        let mut argv = vec!["gents", "server"];
        argv.extend_from_slice(extra);
        match Cli::try_parse_from(argv)
            .expect("server should parse")
            .command
        {
            Some(Command::Server(args)) => args,
            _ => panic!("expected `server`"),
        }
    }

    #[test]
    fn grok_round_trips_as_exactly_client_grok() {
        let home = tempfile::tempdir().unwrap();
        assert!(GROK.persist(home.path()).unwrap());
        assert_eq!(
            fs::read_to_string(client_settings_path(home.path())).unwrap(),
            r#"{"client":"grok"}"#
        );
        assert_eq!(ClientSettings::read(home.path()).unwrap(), GROK);
        assert!(!GROK.persist(home.path()).unwrap(), "unchanged");
    }

    #[test]
    fn no_file_means_codex() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(ClientSettings::read(home.path()).unwrap(), CODEX);
    }

    #[test]
    fn setting_codex_removes_the_file() {
        let home = tempfile::tempdir().unwrap();
        assert!(!CODEX.persist(home.path()).unwrap(), "already the default");
        GROK.persist(home.path()).unwrap();
        assert!(CODEX.persist(home.path()).unwrap());
        assert!(!client_settings_path(home.path()).exists());
    }

    #[test]
    fn unknown_fields_are_refused() {
        let home = tempfile::tempdir().unwrap();
        fs::write(
            client_settings_path(home.path()),
            br#"{"client":"grok","extra":1}"#,
        )
        .unwrap();
        assert!(ClientSettings::read(home.path()).is_err());
    }

    #[test]
    fn grok_is_refused_off_unix() {
        assert!(ensure_client_supported(InterfaceClient::Grok, false).is_err());
        ensure_client_supported(InterfaceClient::Grok, true).unwrap();
        ensure_client_supported(InterfaceClient::Codex, false).unwrap();
    }

    #[test]
    fn grok_enables_the_shim_on_unix() {
        let mut args = serve_args(&[]);
        apply_client_setting(&mut args, &GROK, false);
        assert!(!args.grok_shim, "the shim needs Unix");
        apply_client_setting(&mut args, &GROK, true);
        assert!(args.grok_shim);
    }

    #[test]
    fn grok_does_not_enable_the_shim_for_prepared_startup() {
        let home = tempfile::tempdir().unwrap();
        GROK.persist(home.path()).unwrap();
        let mut args = serve_args(&[]);
        apply_home_client_setting(&mut args, home.path(), true, true).unwrap();
        assert!(!args.grok_shim);
        apply_home_client_setting(&mut args, home.path(), false, true).unwrap();
        assert!(args.grok_shim);
    }

    #[test]
    fn a_corrupt_file_stops_only_a_path_that_reads_it() {
        let home = tempfile::tempdir().unwrap();
        fs::write(client_settings_path(home.path()), "not json").unwrap();
        let mut args = serve_args(&[]);

        apply_home_client_setting(&mut args, home.path(), true, true)
            .expect("prepared startup ignores the setting");
        assert!(!args.grok_shim);
        assert!(apply_home_client_setting(&mut args, home.path(), false, true).is_err());
    }

    #[test]
    fn set_repairs_a_corrupt_file() {
        let home = tempfile::tempdir().unwrap();
        let path = client_settings_path(home.path());

        fs::write(&path, "not json").unwrap();
        assert!(GROK.persist(home.path()).unwrap());
        assert_eq!(ClientSettings::read(home.path()).unwrap(), GROK);

        fs::write(&path, "not json").unwrap();
        assert!(CODEX.persist(home.path()).unwrap());
        assert!(!path.exists());
    }

    #[test]
    fn an_uninitialized_home_is_named_as_one_shell_word() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path().join("my home");
        let error = initialized_home(Some(&home)).unwrap_err().to_string();
        assert!(
            error.contains(&format!("`gents --home '{}'`", home.display())),
            "{error}"
        );
    }

    #[test]
    fn codex_leaves_grok_shim_unchanged() {
        let mut args = serve_args(&[]);
        apply_client_setting(&mut args, &CODEX, true);
        assert!(!args.grok_shim);

        let mut args = serve_args(&["--grok-shim"]);
        apply_client_setting(&mut args, &CODEX, true);
        assert!(args.grok_shim);
    }
}
