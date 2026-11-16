use std::process::{Command, Output, Stdio};

fn run(args: &[&str]) -> Output {
    Command::new(
        std::env::var_os("CARGO_BIN_EXE_gents-shell")
            .expect("cargo test and nextest set CARGO_BIN_EXE_gents-shell"),
    )
    .args(args)
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .output()
    .expect("gents-shell runs")
}

fn assert_usage_error(out: &Output) {
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--keys regular|vim|emacs"));
}

#[test]
fn piped_run_exits_2_and_leaves_the_terminal_alone() {
    for args in [&[][..], &["--keys", "vim"]] {
        let out = run(args);
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
        assert!(String::from_utf8_lossy(&out.stderr).contains("needs a terminal"));
    }
}

#[test]
fn unknown_key_set_exits_2() {
    assert_usage_error(&run(&["--keys", "nano"]));
    assert_usage_error(&run(&["vim"]));
}

#[test]
fn keys_without_value_exits_2() {
    assert_usage_error(&run(&["--keys"]));
}
