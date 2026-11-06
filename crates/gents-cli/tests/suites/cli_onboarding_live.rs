//! Bare `gents` in a real PTY against real launchd, with a temporary `HOME` and
//! no copied credentials. The label and ports are fixed, so tests run one at a
//! time, through `make live-onboarding`.

#[path = "cli_onboarding_live/harness.rs"]
mod harness;

use harness::*;

use crate::support::*;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
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

    /// A stand-in interface that writes its arguments to `args` and exits 0.
    fn recording_interface(live: &LiveBox) -> Result<(PathBuf, PathBuf)> {
        use std::os::unix::fs::PermissionsExt as _;
        let script = live.root.join("recording-interface");
        let args = live.root.join("recorded-args");
        fs::write(
            &script,
            format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n", args.display()),
        )?;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755))?;
        Ok((script, args))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn codex_opens_in_the_terminal_directory_only_inside_the_tool_root() -> Result<()> {
        let backend = answering_backend()?;
        let live = LiveBox::new()?;
        let home = live.init_home("home", backend.endpoint(), NO_MODEL_NAME)?;
        let (interface, recorded) = recording_interface(&live)?;
        let project = live.root.join("project");
        fs::create_dir(&project)?;
        let open = |directory: &Path, extra: &[&str], log: &str| {
            let mut command = live.bare_gents(&interface.to_string_lossy(), &home.home);
            command.current_dir(directory).args(extra);
            Terminal::spawn(&command, &live.log(log))
        };

        let mut terminal = open(&project, &[], "inside")?;
        let code = terminal.wait_exit(RUNTIME_UP).await?;
        ensure!(code == 0, "{}", terminal.transcript());
        let args = fs::read_to_string(&recorded)?;
        ensure!(
            args.contains(&format!("--cd\n{}\n", project.display())),
            "{args}"
        );
        ensure!(
            args.contains(&format!("127.0.0.1:{SHIM_PORT}")),
            "with no --agent there is no relay: {args}"
        );
        ensure!(
            !terminal.transcript().contains(OUTSIDE_THE_ROOT_TEXT),
            "{}",
            terminal.transcript()
        );

        let mut terminal = open(Path::new("/"), &[], "outside")?;
        let code = terminal.wait_exit(RUNTIME_UP).await?;
        ensure!(code == 0, "{}", terminal.transcript());
        let args = fs::read_to_string(&recorded)?;
        ensure!(!args.contains("--cd"), "{args}");
        ensure!(
            terminal
                .transcript()
                .matches(&format!("{OUTSIDE_THE_ROOT_TEXT} {}", live.root.display()))
                .count()
                == 2,
            "the notice comes before the interface and again after it: {}",
            terminal.transcript()
        );

        let mut never_opens = live.bare_gents(MISSING_BIN, &home.home);
        never_opens.current_dir("/");
        let mut terminal = Terminal::spawn(&never_opens, &live.log("outside-never-opens"))?;
        terminal.expect_exit(1).await?;
        ensure!(
            terminal.transcript().matches(OUTSIDE_THE_ROOT_TEXT).count() == 1,
            "with no interface nothing took the screen, so the notice is not repeated: {}",
            terminal.transcript()
        );

        let mut terminal = open(&project, &["--agent", &home.default_agent_id], "agent")?;
        let code = terminal.wait_exit(RUNTIME_UP).await?;
        ensure!(code == 0, "{}", terminal.transcript());
        let args = fs::read_to_string(&recorded)?;
        ensure!(
            args.contains(&format!("--cd\n{}\n", project.display())),
            "{args}"
        );
        ensure!(
            !args.contains(&format!("127.0.0.1:{SHIM_PORT}")),
            "--agent must connect through the relay: {args}"
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn a_listener_on_the_grok_socket_that_is_not_this_runtime_is_refused() -> Result<()> {
        let live = LiveBox::new()?;
        let home = live.init_home("home", NO_MODEL_ENDPOINT, NO_MODEL_NAME)?;
        let home_arg = home.home.to_string_lossy();
        live.gents_ok(&["config", "client", "set", "grok", "--home", &home_arg])?;
        let (interface, recorded) = recording_interface(&live)?;
        // Under /tmp because a Unix socket path has a short length limit.
        let runtime_dir = tempfile::Builder::new()
            .prefix("gents-")
            .tempdir_in("/tmp")?;
        let socket = runtime_dir.path().join("gents/grok.sock");
        fs::create_dir_all(socket.parent().context("the socket has a parent")?)?;
        let _stock_leader = std::os::unix::net::UnixListener::bind(&socket)?;
        let mut command = live.bare_gents_grok(&interface.to_string_lossy(), &home.home);
        command.env("XDG_RUNTIME_DIR", runtime_dir.path());

        let mut terminal = Terminal::spawn(&command, &live.log("stock-leader"))?;

        terminal
            .expect(&socket.to_string_lossy(), RUNTIME_UP)
            .await?;
        terminal.expect_exit(1).await?;
        ensure!(!recorded.exists(), "nothing may open");
        ensure!(!live.plist().exists(), "nothing may be installed");
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn grok_opens_on_the_socket_the_service_holds_whatever_the_terminal_expects() -> Result<()>
    {
        let backend = answering_backend()?;
        let live = LiveBox::new()?;
        let home = live.init_home("home", backend.endpoint(), NO_MODEL_NAME)?;
        live.gents_ok(&[
            "config",
            "client",
            "set",
            "grok",
            "--home",
            &home.home.to_string_lossy(),
        ])?;
        let (interface, recorded) = recording_interface(&live)?;
        let terminal_dir = tempfile::Builder::new()
            .prefix("gents-")
            .tempdir_in("/tmp")?;
        let mut command = live.bare_gents_grok(&interface.to_string_lossy(), &home.home);
        command.env("XDG_RUNTIME_DIR", terminal_dir.path());

        let mut terminal = Terminal::spawn(&command, &live.log("other-runtime-dir"))?;

        let code = terminal.wait_exit(RUNTIME_UP).await?;
        ensure!(code == 0, "{}", terminal.transcript());
        let runtime: serde_json::Value =
            reqwest::get(format!("http://127.0.0.1:{SHIM_PORT}/runtime"))
                .await?
                .json()
                .await?;
        let held = runtime["grok_socket"]
            .as_str()
            .context("the service holds a socket")?;
        ensure!(
            !Path::new(held).starts_with(terminal_dir.path()),
            "the service must not share the terminal's directory: {held}"
        );
        let args = fs::read_to_string(&recorded)?;
        ensure!(
            args.contains(&format!("--leader-socket\n{held}\n")),
            "{args}"
        );
        Ok(())
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

        let mut command = live.bare_gents("/usr/bin/true", &home.home);
        command.args(["--agent", &home.default_agent_id]);
        let mut terminal = Terminal::spawn(&command, &live.log("first-open"))?;

        let code = terminal.wait_exit(RUNTIME_UP).await?;
        ensure!(code == 0, "{}", terminal.transcript());
        ensure!(
            !terminal.transcript().contains("unknown agent"),
            "{}",
            terminal.transcript()
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn an_unknown_agent_names_gents_config_agent_list() -> Result<()> {
        let backend = answering_backend()?;
        let live = LiveBox::new()?;
        let home = live.init_home("home", backend.endpoint(), NO_MODEL_NAME)?;
        let mut command = live.bare_gents("/usr/bin/true", &home.home);
        command.args(["--agent", "nosuch"]);

        let mut terminal = Terminal::spawn(&command, &live.log("unknown"))?;

        terminal.expect("unknown agent", RUNTIME_UP).await?;
        terminal.expect("gents config agent list", EXIT).await?;
        terminal.expect_exit(1).await
    }

    #[test]
    #[ignore = "ignored only so `make live-onboarding` runs it; touches nothing"]
    fn only_an_isolated_build_on_macos_passes_the_label_check() {
        for (os, label) in [
            ("macos", "ai.gents.runtime"),
            ("macos", ""),
            ("macos", "ai.gents.runtime.native-e2e.other"),
            ("linux", "ai.gents.runtime.native-e2e"),
        ] {
            let metadata = json!({"os": os, "service_label": label});
            assert!(
                isolated_label(&metadata).is_err(),
                "{os} {label} must be refused"
            );
        }
        assert!(isolated_label(&json!({})).is_err());
        assert_eq!(
            isolated_label(&json!({"os": "macos", "service_label": "ai.gents.runtime.native-e2e"}))
                .unwrap(),
            "ai.gents.runtime.native-e2e"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn a_stopped_home_starts_its_service_and_reports_the_missing_interface() -> Result<()> {
        let backend = answering_backend()?;
        let live = LiveBox::new()?;
        let home = live.init_home("home", backend.endpoint(), NO_MODEL_NAME)?;

        let mut terminal = Terminal::spawn(
            &live.bare_gents(MISSING_BIN, &home.home),
            &live.log("first-open"),
        )?;

        terminal.expect(INSTALL_MESSAGE, RUNTIME_UP).await?;
        terminal.expect("gents -p MESSAGE", EXIT).await?;
        terminal.expect_exit(1).await?;
        ensure!(live.plist().is_file(), "the service was not installed");
        ensure!(
            live.label_disabled()?,
            "a first open must not enable the service at login"
        );
        shim_call("/", "thread/loaded/list", json!({})).await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn a_home_with_no_runnable_agent_says_so_and_opens_nothing() -> Result<()> {
        let live = LiveBox::new()?;
        let home = live.init_home("home", NO_MODEL_ENDPOINT, NO_MODEL_NAME)?;
        let home_arg = home.home.to_string_lossy();
        let (interface, recorded) = recording_interface(&live)?;
        let interface = interface.to_string_lossy();

        for (client, command) in [
            ("codex", live.bare_gents(&interface, &home.home)),
            ("grok", live.bare_gents_grok(&interface, &home.home)),
        ] {
            live.gents_ok(&["config", "client", "set", client, "--home", &home_arg])?;
            if client == "grok" {
                live.gents_ok(&["runtime", "service", "restart", "--home", &home_arg])?;
            }

            let mut terminal = Terminal::spawn(&command, &live.log(client))?;

            terminal.expect("no agent can run", RUNTIME_UP).await?;
            terminal.expect("gents status", EXIT).await?;
            terminal.expect("gents config backend set", EXIT).await?;
            terminal.expect_exit(1).await?;
            ensure!(!recorded.exists(), "{client}: nothing may open");
        }
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

        let mut terminal = Terminal::spawn(
            &live.bare_gents("/usr/bin/false", &home.home),
            &live.log("second-open"),
        )?;

        terminal.expect("Codex exited", RUNTIME_UP).await?;
        terminal.expect_exit(1).await?;
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
    async fn another_homes_runtime_is_refused_and_named() -> Result<()> {
        let backend = answering_backend()?;
        let live = LiveBox::new()?;
        let a = live.init_home("home-a", backend.endpoint(), NO_MODEL_NAME)?;
        let b = live.init_home("home-b", NO_MODEL_ENDPOINT, NO_MODEL_NAME)?;
        live.bring_up(&a.home).await?;
        let pid = live.service_pid().context("home A's service runs")?;

        let mut terminal =
            Terminal::spawn(&live.bare_gents(MISSING_BIN, &b.home), &live.log("refused"))?;

        terminal
            .expect(&a.home.to_string_lossy(), RUNTIME_UP)
            .await?;
        terminal.expect_exit(1).await?;
        ensure!(
            terminal.transcript().contains("gents --home"),
            "the refusal must say how to open the other home: {}",
            terminal.transcript()
        );
        assert_eq!(live.service_pid(), Some(pid));

        live.gents_ok(&[
            "runtime",
            "service",
            "stop",
            "--home",
            &a.home.to_string_lossy(),
        ])?;
        let mut terminal = Terminal::spawn(
            &live.bare_gents(MISSING_BIN, &b.home),
            &live.log("owned-by-a"),
        )?;

        terminal.expect("belongs to Gents home", RUNTIME_UP).await?;
        terminal.expect_exit(1).await
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn an_endpoint_that_cannot_identify_itself_gets_the_same_refusal() -> Result<()> {
        let live = LiveBox::new()?;
        let home = live.init_home("home", NO_MODEL_ENDPOINT, NO_MODEL_NAME)?;
        let endpoint = format!("127.0.0.1:{SHIM_PORT}");

        for (name, reply) in [
            ("closes", &b""[..]),
            (
                "not-found",
                &b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"[..],
            ),
        ] {
            let held = HeldShimPort::serving(reply).await?;
            let mut terminal =
                Terminal::spawn(&live.bare_gents(MISSING_BIN, &home.home), &live.log(name))?;

            terminal.expect(&endpoint, RUNTIME_UP).await?;
            terminal.expect_exit(1).await?;
            ensure!(!live.plist().exists(), "{name}: nothing may be installed");
            held.release().await;
        }
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn a_runtime_up_with_its_endpoint_off_says_restart_and_restarts_nothing() -> Result<()> {
        let live = LiveBox::new()?;
        let home = live.init_home("home", NO_MODEL_ENDPOINT, NO_MODEL_NAME)?;
        let child = live
            .gents()
            .args(["server", "--no-codex-shim", "--home"])
            .arg(&home.home)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("starting the foreground server")?;
        let mut server = ServeProcess::new(child);
        let graphql = graphql_url(HTTP_PORT);
        crate::support::graphql::register_served_home(&home.home, HTTP_PORT)?;
        wait_for_runtime_state_graphql(&home.home, &graphql, RUNTIME_UP).await?;

        let mut terminal = Terminal::spawn(
            &live.bare_gents(MISSING_BIN, &home.home),
            &live.log("endpoint-off"),
        )?;

        terminal
            .expect("Waiting for this home's runtime", PROMPT)
            .await?;
        terminal.expect("gents status", RUNTIME_UP).await?;
        terminal
            .expect("gents runtime service restart", EXIT)
            .await?;
        terminal.expect("whatever is running it", EXIT).await?;
        terminal.expect_exit(1).await?;
        ensure!(
            server.child.try_wait()?.is_none(),
            "the foreground server must still be running"
        );
        ensure!(!live.plist().exists(), "nothing may be installed");
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn first_run_sets_up_and_leaves_the_runtime_up() -> Result<()> {
        let backend = answering_backend()?;
        let live = LiveBox::new()?;
        let folder = live.folder("my data");

        let mut terminal = Terminal::spawn(&live.first_run(MISSING_BIN), &live.log("first-run"))?;

        terminal.expect("██", PROMPT).await?;
        terminal.expect(INSTALL_MESSAGE, PROMPT).await?;
        terminal
            .answer(FOLDER_QUESTION, &folder.to_string_lossy())
            .await?;
        terminal
            .answer_scanned_inference(backend.endpoint(), NO_MODEL_NAME)
            .await?;
        terminal.expect(INSTALL_MESSAGE, RUNTIME_UP).await?;
        terminal.expect("gents -p MESSAGE", EXIT).await?;
        terminal.expect_exit(1).await?;

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
        shim_call("/", "thread/loaded/list", json!({})).await?;

        let pid = live.service_pid().context("the service runs")?;
        let mut no_flags = live.gents();
        no_flags.env("GENTS_CODEX_BIN", MISSING_BIN);
        let mut second = Terminal::spawn(&no_flags, &live.log("second-run"))?;
        second.expect(INSTALL_MESSAGE, RUNTIME_UP).await?;
        second.expect_exit(1).await?;
        ensure!(
            !second.transcript().contains(FOLDER_QUESTION)
                && !second.transcript().contains(INFERENCE_QUESTION),
            "a set up home asks nothing: {}",
            second.transcript()
        );
        ensure!(
            !second.transcript().contains(CLEAR_SCREEN),
            "only setup clears the screen: {}",
            second.transcript()
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

        let mut terminal = Terminal::spawn(&live.first_run(MISSING_BIN), &live.log("failed"))?;
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

        let mut rerun = Terminal::spawn(&live.first_run(MISSING_BIN), &live.log("rerun"))?;
        rerun
            .answer(FOLDER_QUESTION, &file.to_string_lossy())
            .await?;
        rerun.expect("is not a folder", PROMPT).await?;
        rerun.expect_exit(1).await?;
        ensure!(!live.remembered_home().exists(), "nothing is remembered");
        Ok(())
    }

    /// Bare `gents --home <folder>`, which stops at the refusal once setup finishes.
    fn setup_of(live: &LiveBox, folder: &Path, log: &str) -> Result<Terminal> {
        let mut command = live.first_run(MISSING_BIN);
        command.arg("--home").arg(folder);
        Terminal::spawn(&command, &live.log(log))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn a_setup_that_stopped_midway_asks_again() -> Result<()> {
        let live = LiveBox::new()?;
        let held = HeldShimPort::serving(b"").await?;
        let folder = live.folder("data");
        // A file where the store needs a folder stops init after it records key custody.
        let blocker = folder.join("data/wal");
        fs::create_dir_all(blocker.parent().context("the blocker has a parent")?)?;
        fs::write(&blocker, "")?;

        let mut terminal = setup_of(&live, &folder, "midway")?;
        terminal.answer_root_and_server(NO_MODEL_ENDPOINT).await?;
        let code = terminal.wait_exit(RUNTIME_UP).await?;
        ensure!(code != 0, "{}", terminal.transcript());
        ensure!(
            terminal
                .transcript()
                .contains("embedded DefraDB node for init"),
            "this must fail after init records its key custody: {}",
            terminal.transcript()
        );
        ensure!(folder.join("init.json").is_file(), "custody is recorded");
        ensure!(!live.plist().exists(), "nothing is installed");
        fs::remove_file(&blocker)?;

        let mut rerun = setup_of(&live, &folder, "midway-rerun")?;
        rerun
            .answer_root_and_inference(NO_MODEL_ENDPOINT, NO_MODEL_NAME)
            .await?;
        rerun.expect(ENDPOINT_TEXT, RUNTIME_UP).await?;
        rerun.expect_exit(1).await?;

        held.release().await;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn setup_asks_again_until_the_work_folder_is_usable() -> Result<()> {
        let live = LiveBox::new()?;
        let held = HeldShimPort::serving(b"").await?;
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
        terminal.expect(ENDPOINT_TEXT, RUNTIME_UP).await?;
        terminal.expect_exit(1).await?;
        assert_eq!(
            read_json_file(&folder.join("init.json"))?["tool_root"],
            json!(live.root.join(WORK_FOLDER).to_string_lossy())
        );

        let folder = live.folder("data-home");
        let mut terminal = setup_of(&live, &folder, "home")?;
        terminal.answer(ROOT_QUESTION, "").await?;
        terminal.expect("your whole home folder", PROMPT).await?;
        terminal.answer("Use it?", "y").await?;
        terminal
            .answer_inference(NO_MODEL_ENDPOINT, NO_MODEL_NAME)
            .await?;
        terminal.expect(ENDPOINT_TEXT, RUNTIME_UP).await?;
        terminal.expect_exit(1).await?;
        assert_eq!(
            read_json_file(&folder.join("init.json"))?["tool_root"],
            json!(live.root.to_string_lossy())
        );

        held.release().await;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn setup_asks_again_after_a_garbage_provider_answer_without_repeating_it() -> Result<()> {
        const PASTED: &str = "sk-pasted-at-the-wrong-prompt";
        let live = LiveBox::new()?;
        let held = HeldShimPort::serving(b"").await?;
        let folder = live.folder("data");

        let mut terminal = setup_of(&live, &folder, "garbage")?;
        terminal.answer(ROOT_QUESTION, WORK_FOLDER).await?;
        terminal.answer(INFERENCE_QUESTION, PASTED).await?;
        terminal.answer("Answer 1 to 5", LOCAL_PROVIDER).await?;
        terminal
            .answer("Server base URL", NO_MODEL_ENDPOINT)
            .await?;
        terminal.answer("Model", NO_MODEL_NAME).await?;
        terminal.expect(ENDPOINT_TEXT, RUNTIME_UP).await?;
        terminal.expect_exit(1).await?;

        // The terminal itself echoes typing once; anything more came from `gents`.
        ensure!(
            terminal.transcript().matches(PASTED).count() <= 1,
            "{}",
            terminal.transcript()
        );
        ensure!(folder.join("init.json").is_file(), "setup kept going");
        held.release().await;
        Ok(())
    }

    /// `command` under a shell that outlives it on the same terminal and prints that
    /// terminal's flags afterward, as the shell a person returns to would find them.
    fn reporting_terminal_flags(command: &Command) -> Command {
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
        shell
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn ctrl_c_at_the_hidden_key_prompt_restores_the_terminal() -> Result<()> {
        let live = LiveBox::new()?;
        let folder = live.folder("data");
        let mut command = live.first_run(MISSING_BIN);
        command.arg("--home").arg(&folder);

        let mut terminal =
            Terminal::spawn(&reporting_terminal_flags(&command), &live.log("key-prompt"))?;
        terminal.answer(ROOT_QUESTION, WORK_FOLDER).await?;
        terminal.answer(INFERENCE_QUESTION, "1").await?;
        terminal.answer("connection method for OpenAI", "2").await?;
        terminal.expect("OpenAI API key (hidden)", PROMPT).await?;
        // Echo goes off just after the prompt prints, and the interrupt has to find it off.
        tokio::time::sleep(Duration::from_millis(500)).await;
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
        let mut command = live.first_run(MISSING_BIN);
        command.arg("--home").arg(&folder);

        let mut terminal =
            Terminal::spawn(&reporting_terminal_flags(&command), &live.log("after-key"))?;
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
        let mut terminal = Terminal::spawn(&live.first_run(MISSING_BIN), &live.log("ctrl-d"))?;
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
        let mut command = live.first_run(MISSING_BIN);
        command.arg("--home").arg(&folder);

        let mut terminal =
            Terminal::spawn(&reporting_terminal_flags(&command), &live.log("ctrl-d-key"))?;
        terminal.answer(ROOT_QUESTION, WORK_FOLDER).await?;
        terminal.answer(INFERENCE_QUESTION, "1").await?;
        terminal.answer("connection method for OpenAI", "2").await?;
        terminal.expect("OpenAI API key (hidden)", PROMPT).await?;
        // Echo goes off just after the prompt prints, and the end of input has to find it off.
        tokio::time::sleep(Duration::from_millis(500)).await;
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
        let held = HeldShimPort::serving(b"").await?;
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
        terminal.expect(ENDPOINT_TEXT, RUNTIME_UP).await?;
        terminal.expect_exit(1).await?;
        held.release().await;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn setup_lists_the_servers_models_and_asks_each_value_with_its_default() -> Result<()> {
        let backend = answering_backend()?;
        let live = LiveBox::new()?;
        let held = HeldShimPort::serving(b"").await?;
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
        terminal.expect(ENDPOINT_TEXT, RUNTIME_UP).await?;
        terminal.expect_exit(1).await?;

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
        held.release().await;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn setup_warns_about_a_model_that_does_not_answer_and_keeps_going() -> Result<()> {
        let live = LiveBox::new()?;
        let held = HeldShimPort::serving(b"").await?;
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
        terminal.expect(ENDPOINT_TEXT, RUNTIME_UP).await?;
        terminal.expect_exit(1).await?;

        ensure!(folder.join("init.json").is_file(), "setup kept going");
        held.release().await;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn setup_prints_the_next_step_after_a_declined_sign_in() -> Result<()> {
        let live = LiveBox::new()?;
        let held = HeldShimPort::serving(b"").await?;
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
        terminal.expect(ENDPOINT_TEXT, RUNTIME_UP).await?;
        terminal.expect_exit(1).await?;
        held.release().await;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn an_initialized_folder_skips_the_remaining_questions() -> Result<()> {
        let live = LiveBox::new()?;
        let home = live.init_home("existing", NO_MODEL_ENDPOINT, NO_MODEL_NAME)?;
        let held = HeldShimPort::serving(b"").await?;

        let mut terminal = Terminal::spawn(&live.first_run(MISSING_BIN), &live.log("existing"))?;
        terminal
            .answer(FOLDER_QUESTION, &home.home.to_string_lossy())
            .await?;
        terminal.expect(ENDPOINT_TEXT, RUNTIME_UP).await?;
        terminal.expect_exit(1).await?;

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
        held.release().await;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn a_given_home_is_not_asked_and_not_remembered() -> Result<()> {
        let live = LiveBox::new()?;
        let home = live.folder("given");
        let held = HeldShimPort::serving(b"").await?;
        let mut command = live.first_run(MISSING_BIN);
        command.arg("--home").arg(&home);

        let mut terminal = Terminal::spawn(&command, &live.log("given"))?;
        terminal
            .answer_root_and_inference(NO_MODEL_ENDPOINT, NO_MODEL_NAME)
            .await?;
        terminal.expect(ENDPOINT_TEXT, RUNTIME_UP).await?;
        terminal.expect_exit(1).await?;

        ensure!(
            !terminal.transcript().contains(FOLDER_QUESTION),
            "a given home is not asked for: {}",
            terminal.transcript()
        );
        ensure!(home.join("init.json").is_file(), "the given home is set up");
        ensure!(!live.remembered_home().exists(), "nothing is remembered");
        held.release().await;
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

        let mut terminal = Terminal::spawn(&live.first_run(MISSING_BIN), &live.log("vanished"))?;
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

    fn unique_prompt() -> String {
        format!("Reply with exactly this token: {}", uuid::Uuid::new_v4())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "needs a local model, real launchd and real Codex; run through `make live-onboarding`"]
    async fn continue_reopens_a_session_in_real_codex_with_its_agent() -> Result<()> {
        let (endpoint, model) = live_model().await?;
        let Some(codex) = real_interface("codex") else {
            return Ok(());
        };
        let codex = codex.to_string_lossy();
        let live = LiveBox::new()?;
        let home = live.init_home("home", &endpoint, &model)?;
        live.bring_up(&home.home).await?;
        crate::support::graphql::register_served_home(&home.home, HTTP_PORT)?;
        let b = live.clone_agent(&home, "other").await?;
        let graphql = graphql_url(HTTP_PORT);
        // The session starts in Codex, so Codex keeps its own record of it, as it does for
        // a person; it refuses permission overrides when resuming such a session.
        let first = unique_prompt();
        let mut opening = live.bare_gents(&codex, &home.home);
        opening.args(["--agent", &b, &first]);
        let mut first_window = Terminal::spawn(&opening, &live.log("window-first"))?;
        let session: Result<String> = async {
            let (request, session, _) = wait_for_request(&graphql, &home.node_did, &first).await?;
            wait_for_request_lifecycle_state(&graphql, &request, &["completed"], TURN).await?;
            first_window.settle().await;
            first_window.send("\x03")?;
            first_window.wait_exit(PROMPT).await?;
            Ok(session)
        }
        .await;
        let session =
            session.with_context(|| format!("the first window:\n{}", first_window.transcript()))?;

        let second = unique_prompt();
        let mut command = live.bare_gents(&codex, &home.home);
        command.args(["--continue", &session, &second]);
        let mut window = Terminal::spawn(&command, &live.log("window-continue"))?;
        let turn: Result<()> = async {
            let (request, again, agent) =
                wait_for_request(&graphql, &home.node_did, &second).await?;
            ensure!(
                again == session,
                "the prompt went to {again}, not {session}"
            );
            ensure!(agent == b, "the prompt went to agent {agent}, not {b}");
            wait_for_request_lifecycle_state(&graphql, &request, &["completed"], TURN).await?;
            // Until the shim's next poll tells Codex the turn ended, Ctrl-C interrupts it.
            window.settle().await;
            window.send("\x03")?;
            window
                .expect(
                    &format!("Pick this up again with: gents --continue {session}"),
                    PROMPT,
                )
                .await
        }
        .await;
        turn.with_context(|| format!("the window:\n{}", window.transcript()))
    }

    /// `doctor` is a Codex subcommand, so it reaches the agent only past the separator.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "needs a local model, real launchd and real Codex; run through `make live-onboarding`"]
    async fn a_prompt_opens_real_codex_with_it_even_when_it_names_a_codex_command() -> Result<()> {
        let (endpoint, model) = live_model().await?;
        let Some(codex) = real_interface("codex") else {
            return Ok(());
        };
        let codex = codex.to_string_lossy();
        let live = LiveBox::new()?;
        let home = live.init_home("home", &endpoint, &model)?;
        live.bring_up(&home.home).await?;
        crate::support::graphql::register_served_home(&home.home, HTTP_PORT)?;

        let mut command = live.bare_gents(&codex, &home.home);
        command.arg("doctor");
        let window = Terminal::spawn(&command, &live.log("window-prompt"))?;
        let graphql = graphql_url(HTTP_PORT);
        let turn: Result<()> = async {
            let (request, _, _) = wait_for_request(&graphql, &home.node_did, "doctor").await?;
            wait_for_request_lifecycle_state(&graphql, &request, &["completed"], TURN).await?;
            Ok(())
        }
        .await;
        turn.with_context(|| format!("the window:\n{}", window.transcript()))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "needs a local model, real launchd and real Codex; run through `make live-onboarding`"]
    async fn yolo_makes_its_agent_and_opens_it_in_real_codex() -> Result<()> {
        let (endpoint, model) = live_model().await?;
        let Some(codex) = real_interface("codex") else {
            return Ok(());
        };
        let codex = codex.to_string_lossy();
        let live = LiveBox::new()?;
        let home = live.init_home("home", &endpoint, &model)?;
        live.bring_up(&home.home).await?;
        crate::support::graphql::register_served_home(&home.home, HTTP_PORT)?;
        let route = agent_route(&format!("{}:yolo", home.node_did));

        let mut command = live.bare_gents(&codex, &home.home);
        command.arg("--yolo");
        let window = Terminal::spawn(&command, &live.log("window-yolo"))?;
        wait_for_loaded_thread(&window, &route, &[])
            .await
            .with_context(|| format!("the yolo window:\n{}", window.transcript()))?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "needs a local model, real launchd and real Codex; run through `make live-onboarding`"]
    async fn two_real_codex_windows_keep_agents_apart() -> Result<()> {
        let (endpoint, model) = live_model().await?;
        let Some(codex) = real_interface("codex") else {
            return Ok(());
        };
        let codex = codex.to_string_lossy();
        let live = LiveBox::new()?;
        let home = live.init_home("home", &endpoint, &model)?;
        live.bring_up(&home.home).await?;
        crate::support::graphql::register_served_home(&home.home, HTTP_PORT)?;
        let pid = live.service_pid().context("the service runs")?;
        let a = home.default_agent_id.clone();
        let b = live.clone_agent(&home, "other").await?;
        let (route_a, route_b) = (agent_route(&a), agent_route(&b));
        let (prompt_a, prompt_b) = (unique_prompt(), unique_prompt());

        let window = |agent: &str, prompt: &str, log: &str| {
            let mut command = live.gents();
            command.env("GENTS_CODEX_BIN", &*codex).args([
                "codex",
                "--agent",
                agent,
                "--no-alt-screen",
                prompt,
            ]);
            Terminal::spawn(&command, &live.log(log))
        };
        let window_a = window(&a, &prompt_a, "window-a")?;
        let window_b = window(&b, &prompt_b, "window-b")?;

        wait_for_loaded_thread(&window_a, &route_a, &[]).await?;
        wait_for_loaded_thread(&window_b, &route_b, &[]).await?;
        let graphql = graphql_url(HTTP_PORT);
        let turns: Result<_> = async {
            let (request_a, session_a, agent_a) =
                wait_for_request(&graphql, &home.node_did, &prompt_a).await?;
            let (request_b, session_b, agent_b) =
                wait_for_request(&graphql, &home.node_did, &prompt_b).await?;
            assert_eq!(agent_a, a);
            assert_eq!(agent_b, b);
            assert_ne!(session_a, session_b);
            wait_for_request_lifecycle_state(&graphql, &request_a, &["completed"], TURN).await?;
            wait_for_request_lifecycle_state(&graphql, &request_b, &["completed"], TURN).await?;
            Ok((session_a, session_b))
        }
        .await;
        let (session_a, session_b) = turns.with_context(|| {
            format!(
                "window A:\n{}\nwindow B:\n{}",
                window_a.transcript(),
                window_b.transcript()
            )
        })?;

        for (route, own, other) in [
            (&route_a, &session_a, &session_b),
            (&route_b, &session_b, &session_a),
        ] {
            let listed = shim_call(route, "thread/list", json!({})).await?;
            let listed = listed["data"].as_array().context("thread/list has data")?;
            let has = |id: &String| listed.iter().any(|thread| &thread["id"] == id);
            ensure!(has(own) && !has(other), "{route} lists {listed:?}");
            let loaded = loaded_threads(route).await?;
            ensure!(
                loaded.contains(own) && !loaded.contains(other),
                "{route} loads {loaded:?}"
            );
        }

        let project = live.root.join("project");
        fs::create_dir(&project)?;
        let known = loaded_threads(&route_b).await?;
        let mut from_project = live.bare_gents(&codex, &home.home);
        from_project.current_dir(&project).args(["--agent", &b]);
        let window_project = Terminal::spawn(&from_project, &live.log("window-project"))?;
        let thread = wait_for_loaded_thread(&window_project, &route_b, &known).await?;
        let listed = shim_call(&route_b, "thread/list", json!({})).await?;
        let started = listed["data"]
            .as_array()
            .and_then(|threads| {
                threads
                    .iter()
                    .find(|listed| listed["id"] == thread.as_str())
            })
            .with_context(|| format!("{thread} is not listed: {listed}"))?;
        assert_eq!(started["cwd"], json!(project.to_string_lossy()));

        assert_eq!(live.service_pid(), Some(pid));
        Ok(())
    }

    /// Opens Grok through `command`, submits a prompt, and returns the request's agent
    /// once it completes.
    async fn turn_in_grok(
        live: &LiveBox,
        home: &InitializedHome,
        command: &Command,
        log: &str,
    ) -> Result<String> {
        let mut terminal = Terminal::spawn(command, &live.log(log))?;
        terminal.settle().await;
        let prompt = unique_prompt();
        terminal.send(&prompt)?;
        tokio::time::sleep(Duration::from_millis(1500)).await;
        terminal.send("\r")?;

        let graphql = graphql_url(HTTP_PORT);
        let result: Result<String> = async {
            let (request, _, agent) = wait_for_request(&graphql, &home.node_did, &prompt).await?;
            wait_for_request_lifecycle_state(&graphql, &request, &["completed"], TURN).await?;
            Ok(agent)
        }
        .await;
        result.with_context(|| format!("Grok transcript:\n{}", terminal.transcript()))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "needs a local model, real launchd and real Grok; run through `make live-onboarding`"]
    async fn real_grok_opens_by_setting_and_by_agent() -> Result<()> {
        let (endpoint, model) = live_model().await?;
        let Some(grok) = real_interface("grok") else {
            return Ok(());
        };
        let grok = grok.to_string_lossy();
        let socket = Path::new("/tmp/gents-grok.sock");
        ensure!(
            !socket.exists(),
            "{} exists, so another runtime holds Grok's socket",
            socket.display()
        );
        let live = LiveBox::new()?;
        let home = live.init_home("home", &endpoint, &model)?;
        let home_arg = home.home.to_string_lossy();
        live.bring_up(&home.home).await?;
        crate::support::graphql::register_served_home(&home.home, HTTP_PORT)?;
        let b = live.clone_agent(&home, "other").await?;
        let pid = live.service_pid().context("the service runs")?;

        live.gents_ok(&["config", "client", "set", "grok", "--home", &home_arg])?;
        let mut refused = Terminal::spawn(
            &live.bare_gents_grok(&grok, &home.home),
            &live.log("grok-socket-off"),
        )?;
        refused
            .expect("gents runtime service restart", RUNTIME_UP)
            .await?;
        refused.expect_exit(1).await?;
        assert_eq!(live.service_pid(), Some(pid));

        live.gents_ok(&["runtime", "service", "restart", "--home", &home_arg])?;
        let agent = turn_in_grok(
            &live,
            &home,
            &live.bare_gents_grok(&grok, &home.home),
            "grok-default",
        )
        .await?;
        assert_eq!(agent, home.default_agent_id);

        let mut by_agent = live.bare_gents_grok(&grok, &home.home);
        by_agent.args(["--agent", &b]);
        let agent = turn_in_grok(&live, &home, &by_agent, "grok-agent").await?;
        assert_eq!(agent, b);

        // `doctor` is a Grok subcommand, so it reaches the agent only past the separator.
        let mut by_prompt = live.bare_gents_grok(&grok, &home.home);
        by_prompt.arg("doctor");
        let window = Terminal::spawn(&by_prompt, &live.log("grok-prompt"))?;
        let graphql = graphql_url(HTTP_PORT);
        let turn: Result<()> = async {
            let (request, _, _) = wait_for_request(&graphql, &home.node_did, "doctor").await?;
            wait_for_request_lifecycle_state(&graphql, &request, &["completed"], TURN).await?;
            Ok(())
        }
        .await;
        turn.with_context(|| format!("Grok transcript:\n{}", window.transcript()))?;
        drop(window);

        drop(live);
        ensure!(
            !socket.exists(),
            "{} outlived the runtime; `lsof` names what holds it",
            socket.display()
        );
        Ok(())
    }

    /// Runs first-run setup into `folder` against the model, up to opening the interface.
    async fn set_up(
        live: &LiveBox,
        interface: &str,
        folder: &Path,
        endpoint: &str,
        model: &str,
    ) -> Result<Terminal> {
        let mut terminal = Terminal::spawn(&live.first_run(interface), &live.log("first-run"))?;
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

        let mut terminal = set_up(&live, MISSING_BIN, &folder, &endpoint, &model).await?;
        terminal.expect(INSTALL_MESSAGE, RUNTIME_UP).await?;
        terminal.expect_exit(1).await?;
        crate::support::graphql::register_served_home(&folder, HTTP_PORT)?;

        let prompt = format!("Reply with exactly this token: {}", uuid::Uuid::new_v4());
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
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "needs a local model, real launchd and real Codex; run through `make live-onboarding`"]
    async fn first_run_opens_real_codex_and_a_turn_completes() -> Result<()> {
        let (endpoint, model) = live_model().await?;
        let Some(codex) = real_interface("codex") else {
            return Ok(());
        };
        let live = LiveBox::new()?;
        let folder = live.folder("data");
        let codex = codex.to_string_lossy();

        let mut terminal = set_up(&live, &codex, &folder, &endpoint, &model).await?;
        let result = turn_in_codex(&live, &mut terminal, &folder).await;
        result.with_context(|| format!("Codex transcript:\n{}", terminal.transcript()))
    }

    async fn turn_in_codex(live: &LiveBox, terminal: &mut Terminal, folder: &Path) -> Result<()> {
        wait_for_loaded_thread(terminal, "/", &[]).await?;
        let pid = live.service_pid().context("the service runs")?;
        crate::support::graphql::register_served_home(folder, HTTP_PORT)?;

        let prompt = format!("Reply with exactly this token: {}", uuid::Uuid::new_v4());
        terminal.send(&prompt)?;
        tokio::time::sleep(Duration::from_millis(1500)).await;
        terminal.send("\r")?;

        let graphql = graphql_url(HTTP_PORT);
        let (request, _, _) = wait_for_request(&graphql, &node_did_of(folder)?, &prompt).await?;
        wait_for_request_lifecycle_state(&graphql, &request, &["completed"], TURN).await?;

        // The request completes before the shim's next stream poll tells Codex, and until
        // then Ctrl-C interrupts the turn Codex still counts as running instead of exiting.
        terminal.settle().await;
        terminal.send("\x03")?;
        terminal.wait_exit(PROMPT).await.with_context(|| {
            let events = folder.join("codex-ui/log/codex-shim-events.jsonl");
            format!(
                "what the shim last received:\n{}",
                fs::read_to_string(events).unwrap_or_default()
            )
        })?;
        assert_eq!(live.service_pid(), Some(pid));
        shim_call("/", "thread/loaded/list", json!({})).await?;
        Ok(())
    }
}
