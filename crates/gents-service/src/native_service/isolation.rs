//! A test build never reaches the machine's service manager. Test builds are
//! this crate's unit tests and any build enabling `test-home-guard`, which only
//! dev-dependencies do (`service_isolation_stays_test_only` in
//! `write_owner_structure.rs`), so every `gents` a test spawns is one. The
//! label is per user, not per `HOME`: on 2026-11-13 test binaries that ran
//! `gents` with a temporary `HOME` stopped the maintainer's node through it.
//! Here the manager holds nothing, and a request to change it panics with the
//! process that asked. `native-e2e` debug builds keep their dedicated label
//! and drive the real manager.

use std::ffi::{OsStr, OsString};

use super::CommandOutput;

pub(super) fn answer(program: &OsStr, args: &[OsString]) -> Option<CommandOutput> {
    let name = std::path::Path::new(program).file_name()?.to_str()?;
    let args: Vec<String> = args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let verb = args
        .iter()
        .find(|arg| !arg.starts_with('-'))
        .map(String::as_str);
    match (name, verb) {
        ("launchctl", Some("print")) => Some(output(
            false,
            "",
            "Could not find service in domain (a test build of Gents has no service manager)",
        )),
        ("launchctl", Some("print-disabled")) => Some(output(true, "disabled services = {\n}", "")),
        ("systemctl", Some("is-active")) => Some(output(false, "inactive", "")),
        ("systemctl", Some("is-enabled")) => Some(output(false, "disabled", "")),
        ("systemctl", Some("show")) => Some(output(true, &show(&args), "")),
        ("systemctl", Some("daemon-reload")) => Some(output(true, "", "")),
        ("launchctl" | "systemctl", _) => refuse(name, &args),
        _ => None,
    }
}

fn show(args: &[String]) -> String {
    let value_only = args.iter().any(|arg| arg == "--value");
    let mut properties = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "-p" || arg == "--property" {
            properties.extend(args.next().into_iter().flat_map(|list| list.split(',')));
        } else if let Some(list) = arg.strip_prefix("--property=") {
            properties.extend(list.split(','));
        }
    }
    properties
        .into_iter()
        .map(|property| {
            let value = match property {
                "LoadState" => "not-found",
                "ActiveState" => "inactive",
                "SubState" => "dead",
                "Result" => "success",
                "ExecMainStatus" | "NRestarts" | "MainPID" => "0",
                _ => "",
            };
            if value_only {
                value.to_owned()
            } else {
                format!("{property}={value}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn refuse(program: &str, args: &[String]) -> ! {
    panic!(
        "a test build of Gents asked the machine's service manager to run `{program} {}` ({}); test builds never change the machine's services: drive this path through a fake `CommandRunner`, or expect the refusal",
        args.join(" "),
        asking_test()
    )
}

fn asking_test() -> String {
    let thread = std::thread::current();
    let process: Vec<String> = std::env::args().collect();
    format!(
        "thread {}, process `{}`, started by {}",
        thread.name().unwrap_or("unnamed"),
        process.join(" "),
        parent()
    )
}

#[cfg(unix)]
fn parent() -> String {
    // SAFETY: getppid has no preconditions and cannot fail.
    let pid = unsafe { libc::getppid() };
    match std::fs::read(format!("/proc/{pid}/cmdline")) {
        Ok(command) => format!(
            "`{}` (pid {pid})",
            String::from_utf8_lossy(&command).replace('\0', " ").trim()
        ),
        Err(_) => format!("pid {pid}"),
    }
}

#[cfg(not(unix))]
fn parent() -> String {
    "an unknown process".to_owned()
}

fn output(success: bool, stdout: &str, stderr: &str) -> CommandOutput {
    CommandOutput {
        success,
        stdout: stdout.to_owned(),
        stderr: stderr.to_owned(),
    }
}
