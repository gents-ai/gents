use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};

use super::command_args::read_only_launch_args;
use super::command_launch::{
    kernel_network, read_only_reaches_network, seatbelt_profile, select_sandbox,
};
use super::{lsp_server_sandbox, CommandExecutionMode, CommandNetworkMode};

#[test]
fn generated_command_sandbox_cases_match_rust_selection() {
    use gents_lean_vocab::families::contracts_command_sandbox_selection as family;
    use gents_session::workspace::sandbox::SandboxBackend;
    family::replay::<CommandExecutionMode, CommandNetworkMode, SandboxBackend>(|i| {
        let selected = select_sandbox(i.mode, i.sandbox_backend);
        let profile = selected.as_ref().ok().and_then(|sandbox| {
            seatbelt_profile(
                sandbox,
                kernel_network(
                    i.mode,
                    i.network_mode,
                    read_only_reaches_network(&i.lookup_command),
                ),
            )
        });
        let wire = |value: serde_json::Value| value.as_str().map(str::to_owned);
        family::SandboxOutput {
            decision: if selected.is_ok() {
                "selected"
            } else {
                "denied"
            }
            .into(),
            sandbox: selected.as_ref().ok().map(|sandbox| (*sandbox).to_owned()),
            denial_reason: selected
                .as_ref()
                .err()
                .map(|reason| reason.to_contract().to_owned()),
            writes: profile.and_then(|profile| wire(serde_json::json!(profile.writes))),
            network: profile.map(|profile| profile.network),
            process_info: profile.and_then(|profile| wire(serde_json::json!(profile.process_info))),
        }
    });
}

#[test]
fn generated_read_only_launch_cases_match_rust_launch_args() {
    use gents_lean_vocab::families::contracts_command_read_only_launch as family;
    family::replay(|i| family::LaunchOutput {
        args: read_only_launch_args(i.kernel_contained, &i.lookup_command, &i.args),
    });
}

#[test]
fn generated_lsp_server_sandbox_matches_rust() {
    gents_lean_vocab::families::lsp_server_sandbox::replay::<CommandExecutionMode>(|i| {
        lsp_server_sandbox(i.mode)
    });
}

struct Listener {
    socket: TcpListener,
    url: String,
}

impl Listener {
    fn bind() -> Self {
        let socket = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/file.txt", socket.local_addr().unwrap());
        socket.set_nonblocking(true).unwrap();
        Self { socket, url }
    }

    /// Answers every connection already waiting and reports how many there were.
    fn answer_waiting(&self) -> usize {
        let mut answered = 0;
        while let Ok((mut stream, _)) = self.socket.accept() {
            stream.set_nonblocking(false).unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nSet-Cookie: session=1\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
            );
            answered += 1;
        }
        answered
    }
}

fn run_serving(command: &mut Command, listener: &Listener) -> (Output, usize) {
    let mut child = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut answered = 0;
    loop {
        answered += listener.answer_waiting();
        if child.try_wait().unwrap().is_some() {
            answered += listener.answer_waiting();
            return (child.wait_with_output().unwrap(), answered);
        }
        std::thread::yield_now();
    }
}

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

/// A repository whose configuration starts a program that writes `marker`
/// from an fsmonitor hook, an external diff and a textconv filter.
fn repository_with_configured_programs(folder: &Path, marker: &Path) -> std::path::PathBuf {
    let repo = folder.join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    std::fs::write(repo.join("a.txt"), "hello\n").unwrap();
    std::fs::write(repo.join(".gitattributes"), "*.txt diff=marked\n").unwrap();
    git(&repo, &["add", "."]);
    git(
        &repo,
        &[
            "-c",
            "user.name=n",
            "-c",
            "user.email=n@example.com",
            "commit",
            "-qm",
            "init",
        ],
    );
    std::fs::write(repo.join("a.txt"), "hello\nchanged\n").unwrap();
    let program = format!(
        "sh -c 'echo ran > \"{}\"; cat \"$1\" 2>/dev/null' configured-program",
        marker.display()
    );
    let program = program.as_str();
    for (key, value) in [
        ("core.fsmonitor", program),
        ("diff.external", program),
        ("diff.marked.textconv", program),
    ] {
        git(&repo, &["config", key, value]);
    }
    repo
}

fn read_only_git(repo: &Path, args: &[&str]) -> Output {
    let args = args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
    Command::new("git")
        .args(read_only_launch_args(false, "git", &args))
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap()
}

/// External premise: the options `read_only_launch_args` adds keep git from
/// starting a configured fsmonitor hook, external diff or textconv filter, keep
/// curl to HTTP and HTTPS, and keep sed from running a script that writes.
#[test]
fn read_only_launch_options_stop_configured_programs_and_writes() {
    let folder = tempfile::tempdir().unwrap();
    let folder = folder.path().canonicalize().unwrap();
    let marker = folder.join("configured-program-ran");
    let repo = repository_with_configured_programs(&folder, &marker);

    for args in [
        &["status", "--short"][..],
        &["diff"],
        &["log", "-p", "-1"],
        &["show", "HEAD"],
    ] {
        let output = read_only_git(&repo, args);
        assert!(output.status.success(), "git {args:?}: {output:?}");
        assert!(
            !marker.exists(),
            "git {args:?} started a configured program"
        );
    }
    let control = Command::new("git")
        .args(["diff"])
        .current_dir(&repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        marker.exists(),
        "without the launch options git starts the configured program: {control:?}"
    );

    let input = folder.join("input.txt");
    std::fs::write(&input, "line\n").unwrap();
    let copy = folder.join("copy.txt");
    let script = format!("w {}", copy.display());
    let sed = Command::new("sed")
        .args(read_only_launch_args(
            false,
            "sed",
            &["-n".into(), script, input.to_string_lossy().into_owned()],
        ))
        .output()
        .unwrap();
    assert!(!sed.status.success(), "sed ran a writing script: {sed:?}");
    assert!(!copy.exists(), "sed wrote through a w command");

    let listener = Listener::bind();
    let dict = listener.url.replace("http://", "dict://");
    let mut curl = Command::new("curl");
    curl.args(read_only_launch_args(true, "curl", &["-s".into(), dict]));
    let (output, answered) = run_serving(&mut curl, &listener);
    assert_eq!(answered, 0, "curl sent a non-HTTP request: {output:?}");
    assert!(!output.status.success());
}

#[cfg(target_os = "macos")]
mod kernel {
    use super::*;
    use crate::native::shared::command::command_launch::{
        render_seatbelt_profile, READ_ONLY_SEATBELT,
    };
    use gents_session::workspace::sandbox::SANDBOX_EXEC;

    fn contained(network: bool, cwd: &Path, program: &str, args: &[&str]) -> Command {
        let profile =
            render_seatbelt_profile(&seatbelt_profile(READ_ONLY_SEATBELT, network).unwrap());
        let mut command = Command::new(SANDBOX_EXEC);
        command
            .args(["-p", &profile, "--", program])
            .args(args)
            .current_dir(cwd)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        command
    }

    fn written_files(folder: &Path, names: &[&str]) -> Vec<String> {
        names
            .iter()
            .filter(|name| folder.join(name).exists())
            .map(|name| (*name).to_owned())
            .collect()
    }

    /// External premise: Seatbelt enforces the read-only profile on the
    /// command and every process it starts, so the writes and programs the
    /// argument validators once admitted leave nothing behind.
    #[test]
    fn read_only_profile_refuses_writes_the_command_and_its_children_attempt() {
        if !Path::new(SANDBOX_EXEC).exists() {
            return;
        }
        let folder = tempfile::tempdir().unwrap();
        let folder = folder.path().canonicalize().unwrap();
        let marker = folder.join("configured-program-ran");
        let repo = repository_with_configured_programs(&folder, &marker);
        std::fs::write(folder.join("input.txt"), "line\n").unwrap();

        let sed = contained(false, &folder, "sed", &["-n", "w copy.txt", "input.txt"])
            .output()
            .unwrap();
        assert!(!sed.status.success(), "{sed:?}");

        let child = contained(false, &folder, "/bin/sh", &["-c", "echo x > child.txt"])
            .output()
            .unwrap();
        assert!(!child.status.success(), "{child:?}");

        let pager = format!(
            "-O/bin/sh -c 'echo x > {}' ",
            folder.join("pager.txt").display()
        );
        let _ = contained(false, &repo, "git", &["grep", &pager, "hello"])
            .output()
            .unwrap();

        for args in [&["status", "--short"][..], &["diff"]] {
            let _ = contained(false, &repo, "git", args).output().unwrap();
            assert!(
                !marker.exists(),
                "git {args:?}: a configured program wrote under the profile"
            );
        }

        let listener = Listener::bind();
        let mut answered_jar_request = 0;
        for args in [
            &["-s", "-c", "jar.txt"][..],
            &["-s", "-D", "headers.txt"],
            &["-s", "--libcurl", "source.c"],
            &["-s", "--trace", "trace.txt"],
            &["-s", "-o", "out.txt"],
            &["-s", "-O"],
        ] {
            let mut curl = contained(true, &folder, "curl", args);
            curl.arg(&listener.url);
            let (_, answered) = run_serving(&mut curl, &listener);
            if args[1] == "-c" {
                answered_jar_request = answered;
            }
        }
        assert_eq!(
            answered_jar_request, 1,
            "a network reader must reach the server"
        );
        assert_eq!(
            written_files(
                &folder,
                &[
                    "copy.txt",
                    "child.txt",
                    "pager.txt",
                    "jar.txt",
                    "headers.txt",
                    "source.c",
                    "trace.txt",
                    "out.txt",
                    "file.txt",
                ],
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn read_only_profile_keeps_non_readers_offline_and_outside_processes_unsignalled() {
        if !Path::new(SANDBOX_EXEC).exists() {
            return;
        }
        let folder = tempfile::tempdir().unwrap();
        let listener = Listener::bind();
        let mut curl = contained(false, folder.path(), "curl", &["-s", &listener.url]);
        let (output, answered) = run_serving(&mut curl, &listener);
        assert_eq!(
            answered, 0,
            "an offline profile reached the server: {output:?}"
        );
        assert!(!output.status.success());

        let mut outside = Command::new("/bin/sleep").arg("600").spawn().unwrap();
        let pid = outside.id().to_string();
        let kill = contained(false, folder.path(), "/bin/kill", &["-TERM", &pid])
            .output()
            .unwrap();
        let still_running = outside.try_wait().unwrap().is_none();
        let _ = outside.kill();
        let _ = outside.wait();
        assert!(!kill.status.success(), "{kill:?}");
        assert!(
            still_running,
            "the read-only profile let a command signal a process it did not start"
        );
    }
}
