use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{ensure, Context, Result};
use clap::Parser as _;
use gents::store::local::home::remember_home;
use gents::tools::surface::refuse_too_broad_root;

use crate::cli::args::{InitArgs, StoreKeyCustodyArg};
use crate::cli::{Cli, Command};
use crate::client_settings::InterfaceClient;
use crate::commands::codex::{interface_bin, CODEX_BIN_ENV};
use crate::commands::eval::terminal_size;
use crate::commands::init::init_home;
use crate::home_state::{read_finished_init_config, resolve_home_dir};
use crate::interactive_backend::{confirm, prompt_line_default, resolve_backend_interactively};

use super::interface_install_message;

/// The app icon's bars sit exactly on this grid, measured from
/// `apps/gents-desktop/src-tauri/icons/icon.png`.
const LOGO_GRID: [&str; 5] = ["...###.", "###....", ".....##", "..#....", "...###."];

/// The logo's shape: columns across the grid, whose cells share them as evenly as whole
/// columns allow. 14 is the icon's own proportions, since a terminal cell is about twice as
/// tall as it is wide; the logo reads better somewhat wider than that.
const LOGO_WIDTH: usize = 18;
const LOGO_CELLS: usize = 7;

pub(super) fn logo() -> String {
    let column = |cell: usize| (2 * cell * LOGO_WIDTH + LOGO_CELLS) / (2 * LOGO_CELLS);
    LOGO_GRID
        .map(|row| {
            row.chars()
                .enumerate()
                .map(|(cell, mark)| {
                    let shown = if mark == '#' { "█" } else { " " };
                    shown.repeat(column(cell + 1) - column(cell))
                })
                .collect::<String>()
        })
        .join("\n")
}

/// The logo in the middle of a pane, with the lines above it. A question under it that does not
/// fit scrolls it up; nothing is lost but the padding.
fn centered_logo(columns: usize, rows: usize) -> String {
    let above = "\n".repeat(rows.saturating_sub(LOGO_GRID.len()) / 2);
    let indent = " ".repeat(columns.saturating_sub(LOGO_WIDTH) / 2);
    let logo = logo()
        .lines()
        .map(|row| format!("{indent}{row}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!("{above}{logo}")
}

/// Gives the next question a screen to itself. It clears only what is visible: `3J` would also
/// erase the scrollback the person was reading before they typed `gents`.
fn next_screen() {
    let (columns, rows) = terminal_size().unwrap_or_default();
    eprintln!("\x1b[2J\x1b[H{}\n", centered_logo(columns, rows));
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

/// The Engineer with the authority the desktop's setup defaults to: its read/write ceiling
/// is the `yolo` package (`server_host::ensure_standard_home`).
pub(super) fn init_argv(home: &Path, tool_root: &Path) -> Vec<String> {
    [
        "gents",
        "init",
        "--home",
        &home.to_string_lossy(),
        "--tool-package",
        "yolo",
        "--engineer",
        "--tool-root",
        &tool_root.to_string_lossy(),
    ]
    .map(String::from)
    .to_vec()
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

/// Asks in the desktop's order, folders then provider. Inline sign-in writes to the store
/// `init_home` creates, so it comes after every question.
/// Only setup remembers a home, once one exists, so an abandoned run asks again.
pub(super) async fn setup(
    home_given: Option<PathBuf>,
    store_key_custody: Option<StoreKeyCustodyArg>,
) -> Result<PathBuf> {
    next_screen();
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
            next_screen();
            resolve_folder_answer(&answer, &default, &user_home, &cwd)
        }
    };
    ensure!(
        !home.exists() || home.is_dir(),
        "{} is not a folder; choose a folder for Gents' data",
        home.display()
    );
    if folder_asked && read_finished_init_config(&home)?.is_some() {
        remember_home(&home)?;
        return Ok(home);
    }

    eprintln!("Commands run as you; this folder is not a sandbox.");
    let tool_root = ask_tool_root(&cwd, &user_home).await?;
    next_screen();
    let mut args = parse_init_args(&init_argv(&home, &tool_root))?;
    args.store_key_custody = store_key_custody;
    resolve_backend_interactively(&mut args, next_screen).await?;

    let initialized = init_home(args).await?;
    for line in next_step_lines(&initialized, &home) {
        eprintln!("{line}");
    }
    if folder_asked {
        remember_home(&home)?;
    }
    Ok(home)
}

/// What the folder question accepts. A symlink or `..` is judged by where it leads.
pub(super) fn check_tool_root(
    answer: &Path,
    user_home: Option<&Path>,
) -> std::result::Result<PathBuf, String> {
    let root = std::fs::canonicalize(answer)
        .ok()
        .filter(|root| root.is_dir())
        .ok_or_else(|| {
            format!(
                "{} is not an existing folder; choose a folder for agents to work in.",
                answer.display()
            )
        })?;
    refuse_too_broad_root(&root, user_home).map_err(|refusal| refusal.to_string())?;
    Ok(root)
}

/// A person's home is a legal root, but a new terminal starts there, so answering with the
/// default can hand over the whole of it without anyone choosing that.
async fn ask_tool_root(cwd: &Path, user_home: &Path) -> Result<PathBuf> {
    let canonical_home = std::fs::canonicalize(user_home).ok();
    loop {
        let answer = prompt_line_default(
            "Which folder should agents work in?",
            &cwd.to_string_lossy(),
        )
        .await?;
        let answer = resolve_folder_answer(&answer, cwd, user_home, cwd);
        match check_tool_root(&answer, canonical_home.as_deref()) {
            Err(refusal) => eprintln!("{refusal}"),
            Ok(root) if canonical_home.as_deref() == Some(root.as_path()) => {
                eprintln!(
                    "{} is your whole home folder. Agents can reach everything in it, and Codex opens every folder under it without asking.",
                    root.display()
                );
                if confirm("Use it?", false).await {
                    return Ok(root);
                }
            }
            Ok(root) => return Ok(root),
        }
    }
}

/// `init_home` lists steps for someone working by hand. Setup starts the server and the
/// interface itself right after this, so only what it cannot do is left to say.
fn next_step_lines(initialized: &serde_json::Value, home: &Path) -> Vec<String> {
    initialized["next_steps"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .filter(|step| !matches!(*step, "gents server" | "gents codex"))
        .map(|step| {
            if step.starts_with("gents auth login") {
                format!(
                    "Sign in later with `{step} --home {}`.",
                    crate::shell_word(home)
                )
            } else if step.starts_with("ollama ") || step.starts_with("llama-server ") {
                format!("If your model is not running yet, start it with `{step}`.")
            } else {
                step.to_string()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::args::{ToolCeilingArg, ToolPackageArg};

    #[test]
    fn logo_keeps_the_app_icon_grid_at_its_width() {
        let rows: Vec<Vec<char>> = logo().lines().map(|row| row.chars().collect()).collect();
        assert_eq!(rows.len(), LOGO_GRID.len());
        for (row, grid) in rows.iter().zip(LOGO_GRID) {
            assert_eq!(row.len(), LOGO_WIDTH);
            for (cell, mark) in grid.chars().enumerate() {
                let middle = (2 * cell + 1) * LOGO_WIDTH / (2 * LOGO_CELLS);
                assert_eq!(row[middle] == '█', mark == '#', "{grid} cell {cell}");
            }
        }
    }

    #[test]
    fn the_logo_sits_in_the_middle_of_the_pane() {
        let centered = centered_logo(120, 40);
        let lines: Vec<&str> = centered.split('\n').collect();
        let above = (40 - LOGO_GRID.len()) / 2;
        assert!(lines[..above].iter().all(|line| line.is_empty()));
        let indent = " ".repeat((120 - LOGO_WIDTH) / 2);
        let expected: Vec<String> = logo().lines().map(|row| format!("{indent}{row}")).collect();
        assert_eq!(lines[above..], expected);
    }

    #[test]
    fn a_pane_too_small_or_unknown_gets_the_logo_at_the_top_left() {
        assert_eq!(centered_logo(LOGO_WIDTH, LOGO_GRID.len()), logo());
        assert_eq!(centered_logo(10, 3), logo());
        assert_eq!(centered_logo(0, 0), logo());
    }

    #[test]
    fn only_steps_setup_does_not_take_itself_are_printed() {
        let initialized = serde_json::json!({
            "next_steps": [
                "gents auth login codex",
                "llama-server -hf m",
                "gents server",
                "gents codex",
                "To change the model's address, do this.",
            ],
        });
        let home = Path::new("/srv/home");
        assert_eq!(
            next_step_lines(&initialized, home),
            [
                "Sign in later with `gents auth login codex --home /srv/home`.",
                "If your model is not running yet, start it with `llama-server -hf m`.",
                "To change the model's address, do this.",
            ]
        );
        assert!(next_step_lines(&serde_json::json!({}), home).is_empty());
        assert_eq!(
            next_step_lines(
                &serde_json::json!({"next_steps": ["gents auth login codex"]}),
                Path::new("/srv/my home")
            ),
            ["Sign in later with `gents auth login codex --home '/srv/my home'`."]
        );
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

    #[cfg(unix)]
    #[test]
    fn the_folder_question_accepts_only_a_narrow_existing_folder() {
        let temp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(temp.path()).unwrap();
        let user_home = base.join("users/me");
        let inside = user_home.join("my project");
        std::fs::create_dir_all(&inside).unwrap();
        let file = base.join("a-file");
        std::fs::write(&file, "").unwrap();
        let to_root = base.join("to-root");
        std::os::unix::fs::symlink("/", &to_root).unwrap();
        let link = base.join("link");
        std::os::unix::fs::symlink(&inside, &link).unwrap();
        let check = |answer: &Path| check_tool_root(answer, Some(&user_home));

        assert_eq!(check(&inside).unwrap(), inside);
        assert_eq!(check(&link).unwrap(), inside);
        assert_eq!(check(&user_home.join("my project/..")).unwrap(), user_home);

        assert!(check(&base.join("missing"))
            .unwrap_err()
            .contains("not an existing folder"));
        assert!(check(&file).unwrap_err().contains("not an existing folder"));
        for too_broad in [Path::new("/"), to_root.as_path()] {
            assert!(
                check(too_broad).unwrap_err().contains("too broad"),
                "{too_broad:?}"
            );
        }
        assert!(check(&base.join("users"))
            .unwrap_err()
            .contains("broader than your home"));
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
    async fn init_argv_seeds_the_engineer_with_the_desktop_authority() {
        let root = Path::new("/srv/agents");
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("my home");
        let mut argv = init_argv(&home, root);
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
            panic!("setup's argv should parse as init");
        };
        assert!(args.engineer);
        crate::commands::init::init_home(args).await.unwrap();

        let config = crate::home_state::read_init_config(&home).unwrap().unwrap();
        assert_eq!(config.tool_package, Some(ToolPackageArg::Yolo));
        assert_eq!(config.tool_ceiling, ToolCeilingArg::Readwrite);
        assert_eq!(config.tool_root.as_deref(), root.to_str());
    }
}
