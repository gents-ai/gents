//! Bare `gents` in a real PTY against real launchd, with a temporary `HOME` and
//! no copied credentials. The label and ports are fixed, so tests run one at a
//! time, through `make live-onboarding`.

#[path = "cli_onboarding_live/harness.rs"]
mod harness;

use harness::*;

use crate::support::*;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use serde_json::json;

mod no_model {
    use super::*;

    /// A home whose backend is down publishes no runnable agent, so `--agent` needs
    /// one that answers. It runs no turn here and needs no model server.
    fn answering_backend() -> Result<MockChatEndpoint> {
        MockChatEndpoint::start(NO_MODEL_NAME, "unused")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn the_first_open_after_a_start_finds_the_default_agent() -> Result<()> {
        let backend = answering_backend()?;
        let live = LiveBox::new()?;
        let home = live.init_home("home", backend.endpoint(), NO_MODEL_NAME)?;
        live.bring_up(&home.home).await?;
        live.gents_ok(&[
            "runtime",
            "service",
            "stop",
            "--home",
            &home.home.to_string_lossy(),
        ])?;

        let mut command = live.bare_gents(&home.home);
        command.args(["--agent", &home.default_agent_did]);
        let mut terminal = Terminal::spawn(&command, &live.log("first-open"))?;

        terminal.quit_shell().await?;
        ensure!(
            !terminal.transcript().contains("no agent can run"),
            "{}",
            terminal.transcript()
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn a_stopped_home_starts_its_service_and_opens_the_shell() -> Result<()> {
        let backend = answering_backend()?;
        let live = LiveBox::new()?;
        let home = live.init_home("home", backend.endpoint(), NO_MODEL_NAME)?;

        let mut terminal = Terminal::spawn(&live.bare_gents(&home.home), &live.log("first-open"))?;

        terminal.quit_shell().await?;
        ensure!(live.plist().is_file(), "the service was not installed");
        ensure!(
            live.label_disabled()?,
            "a first open must not enable the service at login"
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn a_home_with_no_runnable_agent_opens_the_shell_to_connect_a_model() -> Result<()> {
        let live = LiveBox::new()?;
        let home = live.init_home("home", NO_MODEL_ENDPOINT, NO_MODEL_NAME)?;

        let mut terminal = Terminal::spawn(&live.bare_gents(&home.home), &live.log("no-agent"))?;

        terminal.quit_shell().await?;
        ensure!(
            !terminal.transcript().contains("gents config backend set"),
            "the shell connects a model, not a file: {}",
            terminal.transcript()
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn a_running_runtime_is_left_alone() -> Result<()> {
        let backend = answering_backend()?;
        let live = LiveBox::new()?;
        let home = live.init_home("home", backend.endpoint(), NO_MODEL_NAME)?;
        live.bring_up(&home.home).await?;
        let pid = live.service_pid().context("the service runs")?;
        let mtime = live.plist_mtime()?;

        let mut terminal = Terminal::spawn(&live.bare_gents(&home.home), &live.log("second-open"))?;

        terminal.quit_shell().await?;
        assert_eq!(live.service_pid(), Some(pid));
        assert_eq!(live.plist_mtime()?, mtime);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn a_stopped_service_is_started_without_reinstalling() -> Result<()> {
        let backend = answering_backend()?;
        let live = LiveBox::new()?;
        let home = live.init_home("home", backend.endpoint(), NO_MODEL_NAME)?;
        live.bring_up(&home.home).await?;
        let pid = live.service_pid().context("the service runs")?;
        let mtime = live.plist_mtime()?;
        live.gents_ok(&[
            "runtime",
            "service",
            "stop",
            "--home",
            &home.home.to_string_lossy(),
        ])?;

        live.bring_up(&home.home).await?;

        let restarted = live.service_pid().context("the service runs again")?;
        assert_ne!(restarted, pid);
        assert_eq!(live.plist_mtime()?, mtime);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn first_run_sets_up_and_leaves_the_runtime_up() -> Result<()> {
        let backend = answering_backend()?;
        let live = LiveBox::new()?;
        let folder = live.folder("my data");

        let mut terminal = Terminal::spawn(&live.first_run(), &live.log("first-run"))?;

        terminal.expect("██", PROMPT).await?;
        terminal
            .answer(FOLDER_QUESTION, &folder.to_string_lossy())
            .await?;
        terminal
            .answer_scanned_inference(backend.endpoint(), NO_MODEL_NAME)
            .await?;
        terminal.quit_shell().await?;

        let init = read_json_file(&folder.join("init.json"))?;
        assert_eq!(init["tool_package"], json!("Yolo"));
        assert_eq!(init["tool_ceiling"], json!("Readwrite"));
        assert_eq!(
            init["tool_root"],
            json!(live.root.join(WORK_FOLDER).to_string_lossy())
        );
        ensure!(
            terminal.transcript().contains("not a sandbox")
                && !terminal.transcript().contains("--yolo"),
            "setup says what agents can do, and names no flag nobody typed: {}",
            terminal.transcript()
        );
        let transcript = terminal.transcript();
        let questions = [
            FOLDER_QUESTION,
            ROOT_QUESTION,
            INFERENCE_QUESTION,
            "Server base URL",
            "Model",
        ];
        for screen in transcript.split(CLEAR_SCREEN) {
            let asked = questions
                .iter()
                .filter(|question| screen.contains(**question));
            ensure!(asked.count() <= 1, "more than one question on: {screen}");
        }
        ensure!(
            transcript.starts_with(CLEAR_SCREEN),
            "setup starts on a clear screen: {transcript}"
        );
        let first_screen = transcript
            .split(CLEAR_SCREEN)
            .nth(1)
            .context("setup prints a first screen")?;
        let lines: Vec<&str> = first_screen.split("\r\n").collect();
        let in_logo = |line: &str| line.contains(['█', '▀', '▄']);
        let above = lines
            .iter()
            .position(|line| in_logo(line))
            .context("the logo is on the first screen")?;
        let logo: Vec<&str> = lines[above..]
            .iter()
            .copied()
            .take_while(|line| in_logo(line))
            .collect();
        let left = logo
            .iter()
            .map(|line| line.chars().take_while(|mark| *mark == ' ').count())
            .min()
            .context("the logo has rows")?;
        let logo_width = logo[0].chars().count() - left;
        assert_eq!(above, (usize::from(PTY_ROWS) - logo.len()) / 2);
        assert_eq!(left, (usize::from(PTY_COLUMNS) - logo_width) / 2);
        let agents = live
            .gents()
            .args(["config", "agent", "list", "--home"])
            .arg(&folder)
            .output()?;
        let agents = String::from_utf8_lossy(&agents.stdout);
        ensure!(
            agents.contains("The Engineer"),
            "setup seeds the Engineer: {agents}"
        );
        assert_eq!(
            fs::read_to_string(live.remembered_home())?.trim_end_matches('\n'),
            folder.to_string_lossy()
        );
        ensure!(
            !terminal.transcript().contains(NO_MODEL_TEXT),
            "a model that answers gets no warning: {}",
            terminal.transcript()
        );
        ensure!(
            !terminal.transcript().contains(r#""status":"initialized""#)
                && !terminal.transcript().contains(r#""status": "initialized""#),
            "setup must not print init's JSON: {}",
            terminal.transcript()
        );
        ensure!(live.plist().is_file(), "the service was not installed");

        let pid = live.service_pid().context("the service runs")?;
        let mut second = Terminal::spawn(&live.gents(), &live.log("second-run"))?;
        second.quit_shell().await?;
        ensure!(
            !second.transcript().contains(FOLDER_QUESTION)
                && !second.transcript().contains(INFERENCE_QUESTION),
            "a set up home asks nothing: {}",
            second.transcript()
        );
        let before_the_shell = second.transcript();
        let before_the_shell = before_the_shell.split(SHELL_UP).next().unwrap_or_default();
        ensure!(
            !before_the_shell.contains(CLEAR_SCREEN),
            "only setup clears the screen: {before_the_shell}"
        );
        assert_eq!(live.service_pid(), Some(pid));
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn a_failed_setup_remembers_nothing() -> Result<()> {
        let live = LiveBox::new()?;
        let file = live.root.join("a-file");
        fs::write(&file, "not a folder")?;

        let mut terminal = Terminal::spawn(&live.first_run(), &live.log("failed"))?;
        terminal
            .answer(FOLDER_QUESTION, &file.join("sub").to_string_lossy())
            .await?;
        terminal.answer_root_and_server(NO_MODEL_ENDPOINT).await?;
        let code = terminal.wait_exit(PROMPT).await?;

        ensure!(code != 0, "{}", terminal.transcript());
        ensure!(
            terminal.transcript().contains("creating data directory"),
            "this must fail inside init_home: {}",
            terminal.transcript()
        );
        ensure!(!live.remembered_home().exists(), "nothing is remembered");
        ensure!(!live.plist().exists(), "nothing is installed");
        assert_eq!(fs::read_to_string(&file)?, "not a folder");

        let mut rerun = Terminal::spawn(&live.first_run(), &live.log("rerun"))?;
        rerun
            .answer(FOLDER_QUESTION, &file.to_string_lossy())
            .await?;
        rerun.expect("is not a folder", PROMPT).await?;
        rerun.expect_exit(1).await?;
        ensure!(!live.remembered_home().exists(), "nothing is remembered");
        Ok(())
    }

    /// Bare `gents --home <folder>`. With no model, it stops at the refusal once setup finishes.
    fn setup_of(live: &LiveBox, folder: &Path, log: &str) -> Result<Terminal> {
        let mut command = live.first_run();
        command.arg("--home").arg(folder);
        Terminal::spawn(&command, &live.log(log))
    }

    /// The `<home>.broken-<time>` folder a repair set `home` aside as.
    fn set_aside_of(home: &Path) -> Option<PathBuf> {
        let prefix = format!("{}.broken-", home.file_name()?.to_string_lossy());
        fs::read_dir(home.parent()?)
            .ok()?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(&prefix))
            })
    }

    /// Answers setup against the answering backend, then quits the shell it opens.
    async fn set_up_and_quit(terminal: &mut Terminal, backend: &MockChatEndpoint) -> Result<()> {
        terminal
            .answer_scanned_inference(backend.endpoint(), NO_MODEL_NAME)
            .await?;
        terminal.quit_shell().await
    }

    /// Bare `gents --home <home>` over a broken `home`: names the finding, sets the folder
    /// aside, sets it up again and opens the shell.
    fn remember_home(live: &LiveBox, home: &Path) -> Result<()> {
        let remembered = live.remembered_home();
        fs::create_dir_all(remembered.parent().context("remembered home directory")?)?;
        fs::write(remembered, format!("{}\n", home.display()))?;
        Ok(())
    }

    async fn is_set_aside_and_set_up_again(
        live: &LiveBox,
        backend: &MockChatEndpoint,
        home: &Path,
        why: &str,
    ) -> Result<()> {
        remember_home(live, home)?;
        let mut terminal = setup_of(live, home, "repair")?;
        terminal.expect("Repairing: ", RUNTIME_UP).await?;
        terminal.expect(why, PROMPT).await?;
        set_up_and_quit(&mut terminal, backend).await?;
        ensure!(
            terminal.transcript().contains("it was set aside as "),
            "{}",
            terminal.transcript()
        );
        let aside = set_aside_of(home).context("the broken folder was set aside")?;
        ensure!(aside.is_dir(), "{} is not a folder", aside.display());
        ensure!(home.join("init.json").is_file(), "the home is set up again");
        ensure!(live.plist().is_file(), "the service is installed");
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn a_setup_that_stopped_midway_is_set_aside_and_set_up_again() -> Result<()> {
        let backend = answering_backend()?;
        let live = LiveBox::new()?;
        let home = live.init_home("data", backend.endpoint(), NO_MODEL_NAME)?;
        // The marker init leaves while it provisions the home.
        fs::write(
            home.home
                .join(gents_store::local::home::INIT_PENDING_FILE_NAME),
            "",
        )?;

        is_set_aside_and_set_up_again(&live, &backend, &home.home, "its setup stopped midway").await
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn a_deleted_home_with_its_node_running_is_set_up_again() -> Result<()> {
        let backend = answering_backend()?;
        let live = LiveBox::new()?;
        let home = live.init_home("home", backend.endpoint(), NO_MODEL_NAME)?;
        live.bring_up(&home.home).await?;
        let old_pid = live.service_pid().context("the service runs")?;
        let old_did = node_did_of(&home.home)?;
        fs::remove_dir_all(&home.home)?;
        remember_home(&live, &home.home)?;

        let mut terminal = setup_of(&live, &home.home, "deleted-home")?;
        terminal
            .expect(
                "Repairing: the background node was still serving a deleted or replaced folder",
                RUNTIME_UP,
            )
            .await?;
        set_up_and_quit(&mut terminal, &backend).await?;

        assert_ne!(node_did_of(&home.home)?, old_did);
        let pid = live.service_pid().context("the service runs again")?;
        assert_ne!(pid, old_pid);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn a_missing_store_key_sets_the_home_aside() -> Result<()> {
        let backend = answering_backend()?;
        let live = LiveBox::new()?;
        let home = live.init_home("home", backend.endpoint(), NO_MODEL_NAME)?;
        fs::remove_file(gents_store::local::store_key::home_key_file(&home.home))?;

        is_set_aside_and_set_up_again(
            &live,
            &backend,
            &home.home,
            "the key for its data is missing",
        )
        .await
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn unreadable_settings_are_set_aside() -> Result<()> {
        let backend = answering_backend()?;
        let live = LiveBox::new()?;
        let home = live.init_home("home", backend.endpoint(), NO_MODEL_NAME)?;
        fs::write(home.home.join("init.json"), "{ not json")?;

        is_set_aside_and_set_up_again(
            &live,
            &backend,
            &home.home,
            "its settings file (init.json) can't be read",
        )
        .await
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn missing_settings_with_data_are_set_aside() -> Result<()> {
        let backend = answering_backend()?;
        let live = LiveBox::new()?;
        let home = live.init_home("home", backend.endpoint(), NO_MODEL_NAME)?;
        fs::remove_file(home.home.join("init.json"))?;
        // With its bootstrap marker kept, a home without settings reads as a stopped setup.
        fs::remove_file(
            home.home
                .join(gents_store::local::home::BOOTSTRAP_CONSUMED_FILE_NAME),
        )?;

        is_set_aside_and_set_up_again(
            &live,
            &backend,
            &home.home,
            "its settings file (init.json) is missing",
        )
        .await
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn choosing_another_folder_moves_the_node() -> Result<()> {
        let backend = answering_backend()?;
        let live = LiveBox::new()?;
        let first = live.init_home("first", backend.endpoint(), NO_MODEL_NAME)?;
        live.bring_up(&first.home).await?;
        let second = live.folder("second");

        let mut terminal = Terminal::spawn(&live.first_run(), &live.log("another-folder"))?;
        terminal
            .answer(FOLDER_QUESTION, &second.to_string_lossy())
            .await?;
        set_up_and_quit(&mut terminal, &backend).await?;

        ensure!(second.join("init.json").is_file(), "the second is set up");
        let plist = fs::read_to_string(live.plist())?;
        ensure!(
            plist.contains(&*second.to_string_lossy())
                && !plist.contains(&*first.home.to_string_lossy()),
            "the service serves the new folder: {plist}"
        );
        ensure!(first.home.join("init.json").is_file(), "the first is kept");
        ensure!(live.service_pid().is_some(), "the node runs");
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn a_program_on_the_port_is_named() -> Result<()> {
        let live = LiveBox::new()?;
        let home = live.init_home("home", NO_MODEL_ENDPOINT, NO_MODEL_NAME)?;
        let _listener = std::net::TcpListener::bind(("127.0.0.1", HTTP_PORT))?;

        let mut terminal = Terminal::spawn(&live.bare_gents(&home.home), &live.log("port"))?;
        let code = terminal.wait_exit(Duration::from_secs(15)).await?;

        ensure!(code != 0, "{}", terminal.transcript());
        ensure!(
            terminal
                .transcript()
                .contains("Your local node couldn't start: port 21919 is in use by"),
            "{}",
            terminal.transcript()
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn a_moved_binary_is_reinstalled() -> Result<()> {
        let backend = answering_backend()?;
        let live = LiveBox::new()?;
        let home = live.init_home("home", backend.endpoint(), NO_MODEL_NAME)?;
        let copy = live.root.join("copy-of-gents");
        fs::copy(cli_bin(), &copy)?;
        let mut command = live.gents_from(&copy);
        command.arg("--home").arg(&home.home);
        let mut installing = Terminal::spawn(&command, &live.log("copy"))?;
        installing.quit_shell().await?;
        ensure!(
            fs::read_to_string(live.plist())?.contains(&*copy.to_string_lossy()),
            "the copy installed the service"
        );
        fs::remove_file(&copy)?;

        let mut terminal = Terminal::spawn(&live.bare_gents(&home.home), &live.log("real"))?;
        terminal.quit_shell().await?;

        let plist = fs::read_to_string(live.plist())?;
        ensure!(
            plist.contains(cli_bin()),
            "the real binary is named: {plist}"
        );
        ensure!(!plist.contains(&*copy.to_string_lossy()), "{plist}");
        ensure!(live.service_pid().is_some(), "the node runs");
        Ok(())
    }

    /// A default folder that is set up and running, and a second folder that cannot load.
    async fn default_running_and_another_broken(
        live: &LiveBox,
        backend: &MockChatEndpoint,
    ) -> Result<(InitializedHome, PathBuf, u32)> {
        let default = live.init_home(".gents", backend.endpoint(), NO_MODEL_NAME)?;
        live.bring_up(&default.home).await?;
        let pid = live.service_pid().context("the service runs")?;
        let broken = live.init_home("broken", backend.endpoint(), NO_MODEL_NAME)?;
        fs::write(broken.home.join("init.json"), "{ not json")?;
        Ok((default, broken.home, pid))
    }

    async fn asked_about(terminal: &mut Terminal, live: &LiveBox, broken: &Path) -> Result<()> {
        terminal
            .expect(
                &format!(
                    "{} can't be loaded: its settings file (init.json) can't be read.",
                    broken.display()
                ),
                RUNTIME_UP,
            )
            .await?;
        terminal
            .expect(
                &format!(
                    "1) Use your default data folder ({})",
                    live.root.join(".gents").display()
                ),
                PROMPT,
            )
            .await?;
        terminal
            .expect(
                &format!(
                    "2) Repair {} (what's there is set aside, then setup runs)",
                    broken.display()
                ),
                PROMPT,
            )
            .await?;
        terminal.expect("> ", PROMPT).await
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn a_broken_explicit_folder_asks_and_1_opens_the_default() -> Result<()> {
        let backend = answering_backend()?;
        let live = LiveBox::new()?;
        let (_, broken, pid) = default_running_and_another_broken(&live, &backend).await?;

        let mut terminal = Terminal::spawn(&live.bare_gents(&broken), &live.log("ask-1"))?;
        asked_about(&mut terminal, &live, &broken).await?;
        terminal.send("1\r")?;
        terminal.quit_shell().await?;

        assert_eq!(live.service_pid(), Some(pid));
        ensure!(set_aside_of(&broken).is_none(), "nothing was set aside");
        ensure!(
            broken.join("init.json").is_file(),
            "the folder is untouched"
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn a_broken_explicit_folder_asks_and_2_repairs_it() -> Result<()> {
        let backend = answering_backend()?;
        let live = LiveBox::new()?;
        let (_, broken, _) = default_running_and_another_broken(&live, &backend).await?;

        let mut terminal = setup_of(&live, &broken, "ask-2")?;
        asked_about(&mut terminal, &live, &broken).await?;
        terminal.send("2\r")?;
        set_up_and_quit(&mut terminal, &backend).await?;

        ensure!(set_aside_of(&broken).is_some(), "the folder was set aside");
        ensure!(broken.join("init.json").is_file(), "the folder is set up");
        ensure!(
            fs::read_to_string(live.plist())?.contains(&*broken.to_string_lossy()),
            "the node serves the repaired folder"
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn setup_asks_again_until_the_work_folder_is_usable() -> Result<()> {
        let live = LiveBox::new()?;
        let file = live.root.join("a-file");
        fs::write(&file, "not a folder")?;

        let folder = live.folder("data");
        let mut terminal = setup_of(&live, &folder, "refused")?;
        terminal.answer(ROOT_QUESTION, "missing").await?;
        terminal.expect("is not an existing folder", PROMPT).await?;
        terminal
            .answer(ROOT_QUESTION, &file.to_string_lossy())
            .await?;
        terminal.expect("is not an existing folder", PROMPT).await?;
        terminal.answer(ROOT_QUESTION, "/").await?;
        terminal.expect("too broad", PROMPT).await?;
        terminal.answer(ROOT_QUESTION, "~").await?;
        terminal.expect("your whole home folder", PROMPT).await?;
        terminal.answer("Use it?", "n").await?;
        ensure!(
            terminal.transcript().split(CLEAR_SCREEN).any(|screen| {
                screen.contains("is not an existing folder")
                    && screen.contains("too broad")
                    && screen.matches(ROOT_QUESTION).count() > 1
            }),
            "a refused answer stays on screen with the question: {}",
            terminal.transcript()
        );
        terminal
            .answer_root_and_inference(NO_MODEL_ENDPOINT, NO_MODEL_NAME)
            .await?;
        // With no model that answers, the shell opens on Setup, which connects one.
        terminal.quit_shell().await?;
        assert_eq!(
            read_json_file(&folder.join("init.json"))?["tool_root"],
            json!(live.root.join(WORK_FOLDER).to_string_lossy())
        );
        // Every home on a machine shares the one service label, and the second setup would be
        // refused control of the first home's service.
        live.gents_ok(&[
            "runtime",
            "service",
            "uninstall",
            "--home",
            &folder.to_string_lossy(),
        ])?;

        let folder = live.folder("data-home");
        let mut terminal = setup_of(&live, &folder, "home")?;
        terminal.answer(ROOT_QUESTION, "").await?;
        terminal.expect("your whole home folder", PROMPT).await?;
        terminal.answer("Use it?", "y").await?;
        terminal
            .answer_inference(NO_MODEL_ENDPOINT, NO_MODEL_NAME)
            .await?;
        // With no model that answers, the shell opens on Setup, which connects one.
        terminal.quit_shell().await?;
        assert_eq!(
            read_json_file(&folder.join("init.json"))?["tool_root"],
            json!(live.root.to_string_lossy())
        );

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn setup_asks_again_after_a_garbage_provider_answer_without_repeating_it() -> Result<()> {
        const PASTED: &str = "sk-pasted-at-the-wrong-prompt";
        let live = LiveBox::new()?;
        let folder = live.folder("data");

        let mut terminal = setup_of(&live, &folder, "garbage")?;
        terminal.answer(ROOT_QUESTION, WORK_FOLDER).await?;
        terminal.answer(INFERENCE_QUESTION, PASTED).await?;
        terminal.answer("Answer 1 to 5", LOCAL_PROVIDER).await?;
        terminal
            .answer("Server base URL", NO_MODEL_ENDPOINT)
            .await?;
        terminal.answer("Model", NO_MODEL_NAME).await?;
        // With no model that answers, the shell opens on Setup, which connects one.
        terminal.quit_shell().await?;

        // The terminal itself echoes typing once; anything more came from `gents`.
        ensure!(
            terminal.transcript().matches(PASTED).count() <= 1,
            "{}",
            terminal.transcript()
        );
        ensure!(folder.join("init.json").is_file(), "setup kept going");
        Ok(())
    }

    /// `command` under a shell that outlives it on the same terminal and prints that
    /// terminal's flags afterward, as the shell a person returns to would find them.
    fn reporting_terminal_flags(command: &Command) -> Result<Command> {
        use std::os::unix::fs::PermissionsExt;
        let directory = command
            .get_current_dir()
            .context("terminal fixture needs a working directory")?
            .join("terminal-tools");
        fs::create_dir_all(&directory)?;
        let forwarder = directory.join("stty");
        fs::write(
            &forwarder,
            br#"#!/bin/sh
/bin/stty "$@"
status=$?
if [ "$status" -eq 0 ] && [ "$#" -eq 1 ] && [ "$1" = "-echo" ]; then
    printf '%s\n' GENTS_TEST_TERMINAL_ECHO_DISABLED >&2
fi
exit "$status"
"#,
        )?;
        fs::set_permissions(&forwarder, fs::Permissions::from_mode(0o755))?;
        let inherited = command
            .get_envs()
            .find(|(key, _)| *key == std::ffi::OsStr::new("PATH"))
            .map(|(_, value)| value.map(std::ffi::OsStr::to_os_string))
            .unwrap_or_else(|| std::env::var_os("PATH"));
        let mut search = vec![directory];
        if let Some(inherited) = inherited {
            search.extend(std::env::split_paths(&inherited));
        }
        let mut shell = Command::new("/bin/sh");
        shell
            .args([
                "-c",
                r#"trap : INT; "$@"; code=$?; stty -a; exit $code"#,
                "sh",
            ])
            .arg(command.get_program())
            .args(command.get_args());
        if let Some(directory) = command.get_current_dir() {
            shell.current_dir(directory);
        }
        for (key, value) in command.get_envs() {
            match value {
                Some(value) => shell.env(key, value),
                None => shell.env_remove(key),
            };
        }
        shell.env("PATH", std::env::join_paths(search)?);
        Ok(shell)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn ctrl_c_at_the_hidden_key_prompt_restores_the_terminal() -> Result<()> {
        let live = LiveBox::new()?;
        let folder = live.folder("data");
        let mut command = live.first_run();
        command.arg("--home").arg(&folder);

        let mut terminal = Terminal::spawn(
            &reporting_terminal_flags(&command)?,
            &live.log("key-prompt"),
        )?;
        terminal.answer(ROOT_QUESTION, WORK_FOLDER).await?;
        terminal.answer(INFERENCE_QUESTION, "1").await?;
        terminal.answer("connection method for OpenAI", "2").await?;
        terminal.expect("OpenAI API key (hidden)", PROMPT).await?;
        terminal
            .expect("GENTS_TEST_TERMINAL_ECHO_DISABLED", PROMPT)
            .await?;
        terminal.send("\x03")?;
        terminal.expect_exit(130).await?;

        let transcript = terminal.transcript();
        let flag = |name| transcript.split_whitespace().any(|word| word == name);
        ensure!(flag("echo") && !flag("-echo"), "{transcript}");
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn ctrl_c_still_stops_setup_after_the_key_prompt() -> Result<()> {
        let live = LiveBox::new()?;
        let folder = live.folder("data");
        let mut command = live.first_run();
        command.arg("--home").arg(&folder);

        let mut terminal =
            Terminal::spawn(&reporting_terminal_flags(&command)?, &live.log("after-key"))?;
        terminal.answer(ROOT_QUESTION, WORK_FOLDER).await?;
        terminal.answer(INFERENCE_QUESTION, "1").await?;
        terminal.answer("connection method for OpenAI", "2").await?;
        terminal
            .answer("OpenAI API key (hidden)", "sk-not-a-real-key")
            .await?;
        terminal.expect("Model [", PROMPT).await?;
        terminal.send("\x03")?;
        terminal.expect_exit(130).await?;

        let transcript = terminal.transcript();
        let flag = |name| transcript.split_whitespace().any(|word| word == name);
        ensure!(flag("echo") && !flag("-echo"), "{transcript}");
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn ctrl_d_at_a_question_stops_setup_and_changes_nothing() -> Result<()> {
        let live = LiveBox::new()?;
        let mut terminal = Terminal::spawn(&live.first_run(), &live.log("ctrl-d"))?;
        terminal.expect(FOLDER_QUESTION, PROMPT).await?;
        terminal.send("\x04")?;
        let code = terminal.wait_exit(EXIT).await?;
        ensure!(code != 0, "{}", terminal.transcript());
        ensure!(
            !terminal.transcript().contains(ROOT_QUESTION),
            "end of input is not an empty answer: {}",
            terminal.transcript()
        );

        let folder = live.folder("data");
        let mut terminal = setup_of(&live, &folder, "ctrl-d-menu")?;
        terminal.answer(ROOT_QUESTION, WORK_FOLDER).await?;
        terminal.expect(INFERENCE_QUESTION, PROMPT).await?;
        terminal.send("\x04")?;
        let code = terminal.wait_exit(EXIT).await?;
        ensure!(code != 0, "{}", terminal.transcript());
        ensure!(!folder.join("init.json").exists(), "nothing is set up");
        ensure!(!live.remembered_home().exists(), "nothing is remembered");
        ensure!(!live.plist().exists(), "nothing is installed");
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn ctrl_d_at_the_hidden_key_prompt_restores_the_terminal() -> Result<()> {
        let live = LiveBox::new()?;
        let folder = live.folder("data");
        let mut command = live.first_run();
        command.arg("--home").arg(&folder);

        let mut terminal = Terminal::spawn(
            &reporting_terminal_flags(&command)?,
            &live.log("ctrl-d-key"),
        )?;
        terminal.answer(ROOT_QUESTION, WORK_FOLDER).await?;
        terminal.answer(INFERENCE_QUESTION, "1").await?;
        terminal.answer("connection method for OpenAI", "2").await?;
        terminal.expect("OpenAI API key (hidden)", PROMPT).await?;
        terminal
            .expect("GENTS_TEST_TERMINAL_ECHO_DISABLED", PROMPT)
            .await?;
        terminal.send("\x04")?;
        let code = terminal.wait_exit(EXIT).await?;
        ensure!(code != 0, "{}", terminal.transcript());

        let transcript = terminal.transcript();
        let flag = |name| transcript.split_whitespace().any(|word| word == name);
        ensure!(flag("echo") && !flag("-echo"), "{transcript}");
        ensure!(!folder.join("init.json").exists(), "nothing is set up");
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn ctrl_d_at_a_yes_by_default_question_declines() -> Result<()> {
        let live = LiveBox::new()?;
        let folder = live.folder("data");

        let mut terminal = setup_of(&live, &folder, "ctrl-d-sign-in")?;
        terminal.answer(ROOT_QUESTION, WORK_FOLDER).await?;
        terminal.answer(INFERENCE_QUESTION, "1").await?;
        terminal.answer("connection method for OpenAI", "").await?;
        terminal.expect("Log in to ChatGPT now", PROMPT).await?;
        terminal.send("\x04")?;
        terminal
            .expect(
                &format!("gents accounts login codex --home {}", folder.display()),
                PROMPT,
            )
            .await?;
        // With no model that answers, the shell opens on Setup, which connects one.
        terminal.quit_shell().await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn setup_lists_the_servers_models_and_asks_each_value_with_its_default() -> Result<()> {
        let backend = answering_backend()?;
        let live = LiveBox::new()?;
        let folder = live.folder("data");

        let mut terminal = setup_of(&live, &folder, "scan")?;
        terminal.answer(ROOT_QUESTION, WORK_FOLDER).await?;
        terminal.answer(INFERENCE_QUESTION, LOCAL_PROVIDER).await?;
        terminal
            .answer("Server base URL", backend.endpoint())
            .await?;
        terminal
            .expect("Choose a model for Local (1 found)", PROMPT)
            .await?;
        terminal.answer("part of a name", "not-listed").await?;
        terminal.expect("No model matches that", PROMPT).await?;
        terminal.answer("part of a name", "").await?;
        terminal
            .expect("Temperature (0 to 2) [0.7]", PROMPT)
            .await?;
        terminal.send("9\r")?;
        terminal.expect("Answer a number, 0 to 2", PROMPT).await?;
        terminal.answer("Temperature (0 to 2) [0.7]", "0.2").await?;
        terminal.answer("Top-p (0 to 1) [1]", "").await?;
        terminal.quit_shell().await?;

        let read = |collection: &str, fields: &[&str]| -> Result<String> {
            let mut command = live.gents();
            command.args(["utils", "data", "query", "find", "--collection", collection]);
            for field in fields {
                command.args(["--field", field]);
            }
            let output = command.arg("--home").arg(&folder).output()?;
            ensure!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        };
        let sampling = read("InferenceSampling", &["temperature", "top_p"])?;
        ensure!(sampling.contains("0.2"), "the changed value: {sampling}");
        let profile = read("InferenceProfile", &["model_name", "sampling_id"])?;
        ensure!(
            profile.contains(NO_MODEL_NAME) && profile.contains("-sampling"),
            "the profile uses the chosen model and its values: {profile}"
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn setup_warns_about_a_model_that_does_not_answer_and_keeps_going() -> Result<()> {
        let live = LiveBox::new()?;
        let folder = live.folder("data");

        let mut terminal = setup_of(&live, &folder, "dead-model")?;
        terminal.answer(ROOT_QUESTION, WORK_FOLDER).await?;
        terminal.answer(INFERENCE_QUESTION, LOCAL_PROVIDER).await?;
        terminal
            .answer("Server base URL", NO_MODEL_ENDPOINT)
            .await?;
        terminal.expect("Looking for models", PROMPT).await?;
        terminal.expect(NO_MODEL_TEXT, PROMPT).await?;
        terminal.expect("gents config backend set", PROMPT).await?;
        terminal.answer("Model name:", NO_MODEL_NAME).await?;
        ensure!(
            terminal
                .transcript()
                .split(CLEAR_SCREEN)
                .any(|screen| screen.contains(NO_MODEL_TEXT) && screen.contains("Model name:")),
            "the warning stays on screen with the next question: {}",
            terminal.transcript()
        );
        terminal
            .expect("gents config backend set --file", PROMPT)
            .await?;
        // With no model that answers, the shell opens on Setup, which connects one.
        terminal.quit_shell().await?;

        ensure!(folder.join("init.json").is_file(), "setup kept going");
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn setup_prints_the_next_step_after_a_declined_sign_in() -> Result<()> {
        let live = LiveBox::new()?;
        let folder = live.folder("data");

        let mut terminal = setup_of(&live, &folder, "declined")?;
        terminal.answer(ROOT_QUESTION, WORK_FOLDER).await?;
        terminal.answer(INFERENCE_QUESTION, "1").await?;
        terminal.answer("connection method for OpenAI", "").await?;
        terminal.answer("Log in to ChatGPT now", "n").await?;

        terminal
            .expect(
                &format!("gents accounts login codex --home {}", folder.display()),
                PROMPT,
            )
            .await?;
        // With no model that answers, the shell opens on Setup, which connects one.
        terminal.quit_shell().await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn an_initialized_folder_skips_the_remaining_questions() -> Result<()> {
        let live = LiveBox::new()?;
        let home = live.init_home("existing", NO_MODEL_ENDPOINT, NO_MODEL_NAME)?;

        let mut terminal = Terminal::spawn(&live.first_run(), &live.log("existing"))?;
        terminal
            .answer(FOLDER_QUESTION, &home.home.to_string_lossy())
            .await?;
        // With no model that answers, the shell opens on Setup, which connects one.
        terminal.quit_shell().await?;

        ensure!(
            !terminal.transcript().contains(INFERENCE_QUESTION)
                && !terminal.transcript().contains(ROOT_QUESTION),
            "an initialized folder asks nothing more: {}",
            terminal.transcript()
        );
        assert_eq!(
            fs::read_to_string(live.remembered_home())?.trim_end_matches('\n'),
            home.home.to_string_lossy()
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn a_given_home_is_not_asked_and_not_remembered() -> Result<()> {
        let live = LiveBox::new()?;
        let home = live.folder("given");
        let mut command = live.first_run();
        command.arg("--home").arg(&home);

        let mut terminal = Terminal::spawn(&command, &live.log("given"))?;
        terminal
            .answer_root_and_inference(NO_MODEL_ENDPOINT, NO_MODEL_NAME)
            .await?;
        // With no model that answers, the shell opens on Setup, which connects one.
        terminal.quit_shell().await?;

        ensure!(
            !terminal.transcript().contains(FOLDER_QUESTION),
            "a given home is not asked for: {}",
            terminal.transcript()
        );
        ensure!(home.join("init.json").is_file(), "the given home is set up");
        ensure!(!live.remembered_home().exists(), "nothing is remembered");
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn a_vanished_remembered_folder_is_the_suggested_answer() -> Result<()> {
        let live = LiveBox::new()?;
        let vanished = live.root.join("vanished folder");
        let remembered = live.remembered_home();
        fs::create_dir_all(remembered.parent().context("the file has a parent")?)?;
        fs::write(&remembered, format!("{}\n", vanished.display()))?;

        let mut terminal = Terminal::spawn(&live.first_run(), &live.log("vanished"))?;
        terminal
            .expect(&format!("{FOLDER_QUESTION}{}]", vanished.display()), PROMPT)
            .await?;
        terminal.send("\x03")?;
        terminal.wait_exit(EXIT).await?;
        Ok(())
    }
}

mod with_model {
    use super::*;

    /// Runs first-run setup into `folder` against the model, up to opening the shell.
    async fn set_up(
        live: &LiveBox,
        folder: &Path,
        endpoint: &str,
        model: &str,
    ) -> Result<Terminal> {
        let mut terminal = Terminal::spawn(&live.first_run(), &live.log("first-run"))?;
        terminal
            .answer(FOLDER_QUESTION, &folder.to_string_lossy())
            .await?;
        terminal.answer_scanned_inference(endpoint, model).await?;
        Ok(terminal)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "needs a local model and real launchd; run through `make live-onboarding`"]
    async fn first_run_then_print_completes_a_turn() -> Result<()> {
        let (endpoint, model) = live_model().await?;
        let live = LiveBox::new()?;
        let folder = live.folder("data");

        let mut terminal = set_up(&live, &folder, &endpoint, &model).await?;
        terminal.quit_shell().await?;
        crate::support::graphql::register_served_home(&live.root, &folder, &[], HTTP_PORT)?;

        let prompt = format!(
            "Give one short greeting in plain text. Do not call tools. Turn {}",
            uuid::Uuid::new_v4()
        );
        let mut print = live.gents();
        print.args(["-p", &prompt, "--home"]).arg(&folder);
        let output = tokio::time::timeout(
            TURN,
            tokio::process::Command::from(print)
                .kill_on_drop(true)
                .output(),
        )
        .await
        .context("gents -p did not finish")??;
        ensure!(
            output.status.success(),
            "gents -p failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let graphql = graphql_url(HTTP_PORT);
        let (request, _, _) = wait_for_request(&graphql, &node_did_of(&folder)?, &prompt).await?;
        wait_for_request_lifecycle_state(&graphql, &request, &["completed"], TURN).await?;
        let access = gents_store::config_client::ConfigAccess::Graphql(
            crate::support::graphql::served_endpoint(&graphql),
        );
        let answer =
            gents::support::live_inference::terminal_assistant_answer(&access, &request).await;
        ensure!(
            !answer.trim().is_empty(),
            "completed print turn has no assistant answer"
        );
        let printed = String::from_utf8_lossy(&output.stdout);
        ensure!(
            printed.contains(answer.trim()),
            "print output omitted the persisted assistant answer: {printed}"
        );
        Ok(())
    }
}
