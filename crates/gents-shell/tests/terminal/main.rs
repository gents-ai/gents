#![cfg(unix)]

mod flows;
mod pty;

use pty::{Exit, Shell};

const WITH_STTY: &str = r#""$0" "$@"; echo rc=$?; stty -a"#;

fn assert_mouse_off(e: &Exit) {
    for mode in [
        r"\u001b[?1000l",
        r"\u001b[?1002l",
        r"\u001b[?1003l",
        r"\u001b[?1006l",
    ] {
        assert!(e.raw.contains(mode), "{mode}: {}", e.raw);
    }
}

/// Pushed on the alternate screen and popped before leaving it, whose stack is its own.
fn assert_keyboard_popped(e: &Exit) {
    let pushed = e
        .raw
        .find(r"\u001b[>5u")
        .expect("the keyboard flags pushed");
    let popped = e
        .raw
        .rfind(r"\u001b[<1u")
        .expect("the keyboard flags popped");
    let left = e
        .raw
        .rfind(r"\u001b[?1049l")
        .expect("the alternate screen left");
    assert!(pushed < popped && popped < left, "{}", e.raw);
}

fn assert_restored(e: &Exit) {
    assert_mouse_off(e);
    assert_keyboard_popped(e);
    assert!(
        e.raw.contains(r"\u001b[?2004l"),
        "bracketed paste off: {}",
        e.raw
    );
    assert!(!e.alternate_screen);
    assert!(e.cursor_visible);
    assert!(
        e.raw.trim_end().ends_with(r#"\u001b[?1049l"]"#),
        "{}",
        e.raw
    );
}

fn assert_cooked(e: &Exit) {
    assert!(e.text.contains("icanon"), "{}", e.text);
    assert!(!e.text.contains("-icanon"), "{}", e.text);
}

#[test]
fn vim_quit_restores_the_terminal() {
    let s = Shell::start(&["--keys", "vim"], 36, 132);
    s.wait_for_all(&["sample data", "INPUT"]);
    s.send("\x1b");
    s.wait_for_all(&["NORMAL"]);
    s.send(":q\r");
    let e = s.finish();
    assert_eq!(e.code, 0);
    assert_restored(&e);
}

/// Regular is what starts with no flag: `?` shows only its keys and `ctrl-q` quits.
#[test]
fn regular_is_the_default_and_quits_with_ctrl_q() {
    let s = Shell::start(&[], 36, 132);
    s.wait_for_all(&["sample data", "INPUT", "ctrl-o next pane"]);
    s.send("?");
    s.wait_for_all(&["HELP", "regular", "ctrl-pgdn / ctrl-pgup"]);
    s.send("\x1b");
    s.wait_gone("ctrl-pgdn / ctrl-pgup");
    s.send("\x11");
    let e = s.finish();
    assert_eq!(e.code, 0);
    assert_restored(&e);
}

/// In raw mode ctrl-c is a key, not SIGINT: the first says how to quit, the second quits.
#[test]
fn ctrl_c_twice_quits_and_restores_the_terminal() {
    let s = Shell::start(&[], 36, 132);
    s.wait_for_all(&["sample data", "INPUT"]);
    s.send("\x03");
    s.wait_for_all(&["ctrl-c again to quit"]);
    s.send("\x03");
    let e = s.finish();
    assert_eq!(e.code, 0);
    assert_restored(&e);
}

#[test]
fn emacs_quit_restores_the_terminal() {
    let s = Shell::start(&["--keys", "emacs"], 36, 132);
    s.wait_for_all(&["sample data", "EMACS"]);
    s.send("\x18\x03");
    let e = s.finish();
    assert_eq!(e.code, 0);
    assert_restored(&e);
}

#[test]
fn raw_mode_is_off_after_quit() {
    let s = Shell::start_command(
        "sh",
        &[
            "-c",
            WITH_STTY,
            env!("CARGO_BIN_EXE_gents-shell"),
            "--keys",
            "emacs",
        ],
        &[],
        36,
        132,
    );
    s.wait_for_all(&["EMACS"]);
    s.send("\x18\x03");
    let e = s.finish();
    assert!(e.text.contains("rc=0"), "{}", e.text);
    assert_cooked(&e);
}

#[test]
fn redirected_stdout_exits_2_untouched() {
    let s = Shell::start_command(
        "sh",
        &[
            "-c",
            r#""$0" >/dev/null; echo rc=$?"#,
            env!("CARGO_BIN_EXE_gents-shell"),
        ],
        &[],
        36,
        132,
    );
    let e = s.finish();
    assert!(e.text.contains("rc=2"), "{}", e.text);
    assert!(e.text.contains("needs a terminal"), "{}", e.text);
    assert!(!e.raw.contains("\\u001b[?1049h"), "{}", e.raw);
}

#[test]
fn panic_child_process() {
    if std::env::var_os("GENTS_SHELL_PANIC_CHILD").is_none() {
        return;
    }
    let mut guard = gents_shell::terminal::enter().unwrap();
    // A frame with no cursor hides it.
    guard.terminal().draw(|_| {}).unwrap();
    // As `panic = "abort"` does: no Drop runs, only the hook.
    std::mem::forget(guard);
    panic!("panic_child_process");
}

#[test]
fn panic_restores_the_terminal() {
    let exe = std::env::current_exe().unwrap();
    let s = Shell::start_command(
        "sh",
        &[
            "-c",
            WITH_STTY,
            exe.to_str().unwrap(),
            "panic_child_process",
            "--exact",
            "--nocapture",
        ],
        &[("GENTS_SHELL_PANIC_CHILD", "1")],
        36,
        132,
    );
    let e = s.finish();
    assert!(e.text.contains("rc=101"), "{}", e.text);
    assert!(e.text.contains("panicked"), "{}", e.text);
    assert_mouse_off(&e);
    assert_keyboard_popped(&e);
    assert_cooked(&e);
    assert!(!e.alternate_screen);
    assert!(e.cursor_visible);
}

#[test]
fn resizes_through_too_small_and_quits() {
    let s = Shell::start(&["--keys", "vim"], 36, 132);
    s.wait_for_all(&["sample data"]);

    s.resize(12, 40);
    s.wait_for_all(&["Window too small"]);
    s.wait_gone("sample data");

    s.resize(36, 132);
    s.wait_for_all(&["sample data"]);

    s.send("\x1b");
    s.wait_for_all(&["NORMAL"]);
    s.send(":");
    s.wait_for_all(&["Jump to"]);
    s.send("q\r");
    let e = s.finish();
    assert_eq!(e.code, 0);
    assert_restored(&e);
}
