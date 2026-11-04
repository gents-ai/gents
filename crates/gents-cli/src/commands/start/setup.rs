use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{ensure, Context, Result};
use clap::Parser as _;
use gents::store::local::home::remember_home;

use crate::cli::args::{InitArgs, StoreKeyCustodyArg};
use crate::cli::{Cli, Command};
use crate::client_settings::InterfaceClient;
use crate::commands::codex::{interface_bin, CODEX_BIN_ENV};
use crate::commands::init::init_home;
use crate::home_state::{read_init_config, resolve_home_dir};
use crate::interactive_backend::{prompt_line, prompt_line_default, resolve_backend_interactively};

use super::interface_install_message;

/// The app icon's bars sit exactly on this grid, measured from
/// `apps/gents-desktop/src-tauri/icons/icon.png`.
const LOGO_GRID: [&str; 5] = ["...###.", "###....", ".....##", "..#....", "...###."];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ToolAccess {
    ReadOnly,
    Write,
    Unrestricted,
}

/// One grid cell is two columns by one row, about square in a terminal.
pub(super) fn logo() -> String {
    LOGO_GRID
        .map(|row| row.replace('#', "██").replace('.', "  "))
        .join("\n")
}

/// `None` carries nothing to repeat back: a key pasted at this prompt must not reach
/// scrollback a second time.
pub(super) fn parse_tool_access(answer: &str) -> Option<ToolAccess> {
    match answer.trim() {
        "" | "1" => Some(ToolAccess::ReadOnly),
        "2" => Some(ToolAccess::Write),
        "3" => Some(ToolAccess::Unrestricted),
        _ => None,
    }
}

/// `~user` is not expanded, so that spelling is an ordinary relative folder.
pub(super) fn resolve_folder_answer(
    answer: &str,
    default: &Path,
    user_home: &Path,
    cwd: &Path,
) -> PathBuf {
    match answer.trim() {
        "" => default.to_path_buf(),
        "~" => user_home.to_path_buf(),
        answer => match answer.strip_prefix("~/") {
            Some(rest) => user_home.join(rest),
            None => cwd.join(answer),
        },
    }
}

pub(super) fn init_argv(home: &Path, access: ToolAccess, tool_root: Option<&Path>) -> Vec<String> {
    let mut argv = vec![
        "gents".to_string(),
        "init".to_string(),
        "--home".to_string(),
        home.to_string_lossy().into_owned(),
    ];
    match access {
        ToolAccess::ReadOnly => {}
        ToolAccess::Write => {
            argv.push("--write".to_string());
            if let Some(root) = tool_root {
                argv.push("--tool-root".to_string());
                argv.push(root.to_string_lossy().into_owned());
            }
        }
        ToolAccess::Unrestricted => argv.push("--yolo".to_string()),
    }
    argv
}

pub(super) fn binary_on_path(bin: &str, path: Option<&OsStr>) -> bool {
    if bin.contains(std::path::MAIN_SEPARATOR) {
        return Path::new(bin).is_file();
    }
    path.is_some_and(|path| std::env::split_paths(path).any(|dir| dir.join(bin).is_file()))
}

fn parse_init_args(argv: &[String]) -> Result<InitArgs> {
    let Some(Command::Init(args)) = Cli::try_parse_from(argv)?.command else {
        unreachable!("setup's argv must parse as init")
    };
    Ok(args)
}

/// Asks in the order the store allows: inline sign-in writes to the store
/// `init_home` creates, so the provider is chosen first and signed in to last.
/// Only setup remembers a home, once one exists, so an abandoned run asks again.
pub(super) async fn setup(
    home_given: Option<PathBuf>,
    store_key_custody: Option<StoreKeyCustodyArg>,
) -> Result<PathBuf> {
    eprintln!("{}\n", logo());
    eprintln!("Welcome to Gents. A few questions and this machine is ready.\n");
    let interface = interface_bin(CODEX_BIN_ENV, "codex");
    if !binary_on_path(&interface, std::env::var_os("PATH").as_deref()) {
        eprintln!(
            "{}\n",
            interface_install_message(InterfaceClient::default())
        );
    }

    let cwd = std::env::current_dir().context("resolving current directory")?;
    let user_home = std::env::home_dir().context("resolving the user home directory")?;
    let home_fixed = home_given.or_else(|| {
        std::env::var_os("GENTS_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    });
    let folder_asked = home_fixed.is_none();
    let home = match home_fixed {
        Some(home) => crate::absolutize(home)?,
        None => {
            let default = crate::absolutize(resolve_home_dir(None))?;
            let answer = prompt_line_default("Data folder", &default.to_string_lossy()).await?;
            resolve_folder_answer(&answer, &default, &user_home, &cwd)
        }
    };
    ensure!(
        !home.exists() || home.is_dir(),
        "{} is not a folder; choose a folder for Gents' data",
        home.display()
    );
    if folder_asked && read_init_config(&home)?.is_some() {
        remember_home(&home)?;
        return Ok(home);
    }

    let mut args = parse_init_args(&init_argv(&home, ToolAccess::ReadOnly, None))?;
    args.store_key_custody = store_key_custody;
    resolve_backend_interactively(&mut args).await?;

    eprintln!("What may agents do?");
    eprintln!("  1) read files and run read-only tools (default)");
    eprintln!("  2) write inside one folder");
    eprintln!("  3) anything your user can do, with no sandbox");
    let access = loop {
        match parse_tool_access(&prompt_line("> ").await?) {
            Some(access) => break access,
            None => eprintln!("Answer 1, 2 or 3."),
        }
    };
    let tool_root = if access == ToolAccess::Write {
        let answer =
            prompt_line_default("Folder agents may write in", &cwd.to_string_lossy()).await?;
        let root = resolve_folder_answer(&answer, &cwd, &user_home, &cwd);
        ensure!(
            root.is_dir(),
            "{} is not an existing folder; choose a folder agents may write in",
            root.display()
        );
        Some(root)
    } else {
        None
    };
    let chosen = parse_init_args(&init_argv(&home, access, tool_root.as_deref()))?;
    args.write_tools = chosen.write_tools;
    args.yolo = chosen.yolo;
    args.tool_root = chosen.tool_root;

    let initialized = init_home(args).await?;
    for step in sign_in_steps(&initialized) {
        eprintln!("Sign in later with `{step} --home {}`.", home.display());
    }
    if folder_asked {
        remember_home(&home)?;
    }
    Ok(home)
}

/// A sign-in declined during setup leaves its step only in what `init_home` returns.
fn sign_in_steps(initialized: &serde_json::Value) -> Vec<&str> {
    initialized["next_steps"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .filter(|step| step.starts_with("gents auth login"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::args::ToolCeilingArg;

    #[test]
    fn logo_matches_the_app_icon_grid() {
        let grid = ["...###.", "###....", ".....##", "..#....", "...###."];
        let expected: Vec<String> = grid
            .iter()
            .map(|row| row.replace('#', "██").replace('.', "  "))
            .collect();
        assert_eq!(logo().lines().collect::<Vec<_>>(), expected);
    }

    #[test]
    fn empty_answer_is_read_only() {
        assert_eq!(parse_tool_access(""), Some(ToolAccess::ReadOnly));
        assert_eq!(parse_tool_access("  \n"), Some(ToolAccess::ReadOnly));
    }

    #[test]
    fn only_exact_choices_grant_more() {
        assert_eq!(parse_tool_access("1"), Some(ToolAccess::ReadOnly));
        assert_eq!(parse_tool_access("2\n"), Some(ToolAccess::Write));
        assert_eq!(parse_tool_access("3"), Some(ToolAccess::Unrestricted));
        for garbage in ["4", "yolo", "2 3", "22", "write"] {
            assert_eq!(parse_tool_access(garbage), None, "{garbage:?}");
        }
    }

    #[test]
    fn only_sign_in_steps_are_printed() {
        let initialized = serde_json::json!({
            "next_steps": ["gents auth login codex", "gents server", "gents codex"],
        });
        assert_eq!(sign_in_steps(&initialized), ["gents auth login codex"]);
        assert!(sign_in_steps(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn tilde_and_relative_answers_become_absolute() {
        let default = Path::new("/default/home");
        let user = Path::new("/users/me");
        let cwd = Path::new("/work");
        let resolve = |answer| resolve_folder_answer(answer, default, user, cwd);

        assert_eq!(resolve(""), default);
        assert_eq!(resolve("  \n"), default);
        assert_eq!(resolve("~"), user);
        assert_eq!(resolve("~/x"), user.join("x"));
        assert_eq!(resolve("x y"), cwd.join("x y"));
        assert_eq!(resolve("/abs/x y"), Path::new("/abs/x y"));
        assert_eq!(resolve("~user/x"), cwd.join("~user/x"));
    }

    #[test]
    fn a_binary_with_a_separator_is_checked_directly() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("tool");
        std::fs::write(&bin, "").unwrap();
        let search = std::env::join_paths([dir.path()]).unwrap();

        assert!(binary_on_path("tool", Some(&search)));
        assert!(!binary_on_path("other", Some(&search)));
        assert!(!binary_on_path("tool", None));
        assert!(binary_on_path(bin.to_str().unwrap(), None));
        assert!(!binary_on_path("/nonexistent/tool", Some(&search)));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn init_argv_parses_and_initializes() {
        let cwd = std::env::current_dir().unwrap();
        let root = Path::new("/srv/agents");
        for (access, tool_root, ceiling, recorded_root) in [
            (
                ToolAccess::ReadOnly,
                None,
                ToolCeilingArg::Readonly,
                cwd.as_path(),
            ),
            (
                ToolAccess::Write,
                Some(root),
                ToolCeilingArg::Readwrite,
                root,
            ),
            (
                ToolAccess::Unrestricted,
                None,
                ToolCeilingArg::Readwrite,
                cwd.as_path(),
            ),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let home = temp.path().join("my home");
            let mut argv = init_argv(&home, access, tool_root);
            argv.extend(
                [
                    "--store-key-custody",
                    "file",
                    "--inference-url",
                    "http://127.0.0.1:9/v1",
                    "--model-name",
                    "m",
                ]
                .map(String::from),
            );

            let Some(Command::Init(args)) = Cli::try_parse_from(argv).unwrap().command else {
                panic!("{access:?} should parse as init");
            };
            crate::commands::init::init_home(args).await.unwrap();

            let config = crate::home_state::read_init_config(&home).unwrap().unwrap();
            assert_eq!(config.tool_ceiling, ceiling, "{access:?}");
            assert_eq!(
                config.tool_root.as_deref(),
                recorded_root.to_str(),
                "{access:?}"
            );
        }
    }
}
