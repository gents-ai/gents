use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

use crate::cli::args::ClientConfigCommand;
use crate::home_state::{read_init_config, resolve_home_dir};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub(crate) enum InterfaceClient {
    /// Built into `gents`, so there is nothing to install.
    #[default]
    Shell,
    Codex,
}

impl InterfaceClient {
    fn name(self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::Codex => "codex",
        }
    }
}

/// The interface a home opens. It lives in a file because `gents` needs it
/// before the runtime is up.
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
        serde_json::from_slice(&bytes).with_context(|| {
            format!(
                "decoding client settings {}; `gents config client set shell --home {}` replaces it",
                path.display(),
                crate::shell_word(home)
            )
        })
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

/// Reads and writes only the home's file, so it works with no runtime.
pub(crate) fn config_client(command: ClientConfigCommand) -> Result<()> {
    match command {
        ClientConfigCommand::Set { client, home } => {
            let home = initialized_home(home.as_deref())?;
            ClientSettings { client }.persist(&home)?;
            eprintln!(
                "client set to {}; the next `gents --home {}` opens it",
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
    use std::fs;

    const CODEX: ClientSettings = ClientSettings {
        client: InterfaceClient::Codex,
    };
    /// What a home set to the removed Grok interface still holds.
    const GROK_FILE: &str = r#"{"client":"grok"}"#;

    const SHELL: ClientSettings = ClientSettings {
        client: InterfaceClient::Shell,
    };

    #[test]
    fn no_file_means_the_shell() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(ClientSettings::read(home.path()).unwrap(), SHELL);
    }

    #[test]
    fn setting_the_shell_removes_the_file() {
        let home = tempfile::tempdir().unwrap();
        assert!(!SHELL.persist(home.path()).unwrap(), "already the default");
        CODEX.persist(home.path()).unwrap();
        assert!(SHELL.persist(home.path()).unwrap());
        assert!(!client_settings_path(home.path()).exists());
    }

    #[test]
    fn codex_round_trips_as_exactly_client_codex() {
        let home = tempfile::tempdir().unwrap();
        assert!(CODEX.persist(home.path()).unwrap());
        assert_eq!(
            fs::read_to_string(client_settings_path(home.path())).unwrap(),
            r#"{"client":"codex"}"#
        );
        assert_eq!(ClientSettings::read(home.path()).unwrap(), CODEX);
    }

    #[test]
    fn the_shell_is_named_shell() {
        assert_eq!(
            InterfaceClient::from_str("shell", false),
            Ok(InterfaceClient::Shell)
        );
        assert_eq!(InterfaceClient::Shell.name(), "shell");
    }

    #[test]
    fn unknown_fields_are_refused() {
        let home = tempfile::tempdir().unwrap();
        fs::write(
            client_settings_path(home.path()),
            br#"{"client":"codex","extra":1}"#,
        )
        .unwrap();
        assert!(ClientSettings::read(home.path()).is_err());
    }

    #[test]
    fn a_grok_file_names_the_command_that_replaces_it() {
        let home = tempfile::tempdir().unwrap();
        let path = client_settings_path(home.path());
        fs::write(&path, GROK_FILE).unwrap();

        let error = format!("{:#}", ClientSettings::read(home.path()).unwrap_err());

        assert!(error.contains(&path.display().to_string()), "{error}");
        assert!(error.contains("`gents config client set shell"), "{error}");
    }

    #[test]
    fn set_repairs_a_corrupt_file() {
        let home = tempfile::tempdir().unwrap();
        let path = client_settings_path(home.path());

        fs::write(&path, GROK_FILE).unwrap();
        assert!(CODEX.persist(home.path()).unwrap());
        assert_eq!(ClientSettings::read(home.path()).unwrap(), CODEX);

        fs::write(&path, GROK_FILE).unwrap();
        assert!(SHELL.persist(home.path()).unwrap());
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
}
