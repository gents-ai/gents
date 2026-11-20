use std::path::PathBuf;

use tui_test::CellColor;

use super::assert_restored;
use super::pty::{cursor_at, cursor_hidden, text_on_background, Shell};

const FOCUS: &str = "#7fc29b";
// `MENU` alone would match the jump bar's `MENUS` header.
const MENU: &str = " MENU ";

fn wait_sessions_focused(s: &Shell) {
    s.wait_until("Sessions focused", text_on_background("Sessions", FOCUS));
    assert_eq!(s.bg(0, 3), CellColor::Rgb(0x7f, 0xc2, 0x9b));
}

fn resize_through_too_small(s: &Shell) {
    s.resize(20, 70);
    s.wait_gone("Sessions");
    s.wait_for_all(&["sample data"]);
    s.resize(20, 40);
    s.wait_for_all(&["Window too small: need at least 60 by 16"]);
    s.wait_gone("sample data");
    s.resize(36, 132);
    s.wait_for_all(&["Sessions", "sample data"]);
}

/// Real SGR presses, past crossterm's parser and into the app.
#[test]
fn clicks_reach_the_app() {
    let click = |s: &Shell, row: u16, column: u16| {
        let (row, column) = (row + 1, column + 1);
        s.send(&format!("\x1b[<0;{column};{row}M\x1b[<0;{column};{row}m"));
    };
    let s = Shell::start(&["--keys", "vim"], 36, 132);
    s.wait_for_all(&["sample data", "Sessions"]);
    click(&s, 6, 5);
    s.wait_for_all(&["Reviewer · auth", "Triage · inbox", "NORMAL"]);
    click(&s, 29, 40);
    s.wait_for_all(&[
        "Subagent: Triage",
        "Done: 4 for the CLI",
        "1 packs · Engineer",
    ]);
    s.send("\x1b");
    s.wait_gone("Subagent: Triage");
    s.wait_for_all(&["NORMAL"]);
    s.send(":q\r");
    assert_restored(&s.finish());
}

/// Real bracketed pastes: a short one is typed in with its lines, a long one is a badge, and
/// one message sends both.
#[test]
fn a_bracketed_paste_is_one_message() {
    let s = Shell::start(&["--keys", "vim"], 36, 132);
    s.wait_for_all(&["sample data", "INPUT", "Message Engineer"]);
    s.send("\x1b[200~first line\n\n    second line\x1b[201~");
    s.wait_for_all(&["> first line", "    second line"]);
    s.send("\x1b[200~p1\np2\np3\np4\np5\np6\x1b[201~");
    s.wait_for_all(&["second line[Pasted text #1 +6 lines]"]);
    s.send("\r");
    s.wait_for_all(&["first line", "    second linep1", "p6", "Message Engineer"]);
    s.wait_gone("[Pasted text #1");
    s.send("\x1b");
    s.wait_for_all(&["NORMAL"]);
    s.send(":q\r");
    assert_restored(&s.finish());
}

/// Each word on a row of the box of its own, under the box's left edge: a key that broke no
/// line, or sent, leaves `> alphabravo` or `> bravo` instead.
const BOX_LINES: [&str; 4] = ["> alpha ", "│   bravo ", "│   charlie ", "│   delta "];

/// Shift-enter as the keyboard protocol sends it, then alt-enter and ctrl-j as any terminal
/// does: each breaks the line, and enter sends the lines as one message.
#[test]
fn real_line_break_keys_make_one_message_of_many_lines() {
    let s = Shell::start(&[], 36, 132);
    s.wait_for_all(&["sample data", "INPUT", "Message Engineer"]);
    s.send("alpha\x1b[13;2ubravo\x1b\rcharlie\ndelta");
    s.wait_for_all(&BOX_LINES);
    s.send("\r");
    let bubble = [" alpha ", " bravo ", " charlie ", " delta "];
    s.wait_for_all(&[&["sample reply", "Message Engineer"][..], &bubble].concat());
    s.send("\x11");
    assert_restored(&s.finish());
}

/// Typing in vim, `esc enter` in one read is alt-enter: the line breaks and typing goes on.
#[test]
fn vim_alt_enter_breaks_the_line_while_typing() {
    let s = Shell::start(&["--keys", "vim"], 36, 132);
    s.wait_for_all(&["INPUT", "Message Engineer"]);
    s.send("alpha\x1b\rbravo");
    s.wait_for_all(&[BOX_LINES[0], BOX_LINES[1], "INPUT"]);
    s.send("\x1b");
    s.wait_for_all(&["NORMAL"]);
    s.send(":q\r");
    assert_eq!(s.finish().code, 0);
}

/// Word and line keys in the forms the keyboard protocol and legacy terminals send.
#[test]
fn real_word_and_line_keys_edit_the_box() {
    let s = Shell::start(&[], 36, 132);
    s.wait_for_all(&["INPUT", "Message Engineer"]);
    s.send("alpha bravo charlie");
    s.wait_for_all(&["> alpha bravo charlie"]);
    // alt-backspace, as the protocol and then as a legacy terminal sends it
    s.send("\x1b[127;3u");
    s.wait_gone("charlie");
    s.send("\x1b\x7f");
    s.wait_gone("bravo");
    // cmd-left, alt-right, ctrl-left
    s.send("\x1b[1;9DX");
    s.wait_for_all(&["> Xalpha"]);
    s.send("\x1b[1;3CY");
    s.wait_for_all(&["> XalphaY"]);
    s.send("\x1b[1;5DZ");
    s.wait_for_all(&["> ZXalphaY"]);
    // cmd-right, cmd-backspace
    s.send("\x1b[1;9C\x1b[127;9u");
    s.wait_for_all(&["Message Engineer"]);
    s.send("\x11");
    assert_restored(&s.finish());
}

#[test]
fn vim_flow() {
    let s = Shell::start(&["--keys", "vim"], 36, 132);
    s.wait_for_all(&["sample data", "INPUT", "Message Engineer", "Sessions"]);

    s.send("hello");
    s.wait_for_all(&["hello"]);
    s.wait_until("cursor after hello", cursor_at(32, 39));
    s.send("\r");
    s.wait_for_all(&["sample reply"]);
    s.send("\x1b");
    s.wait_for_all(&["NORMAL"]);
    s.wait_until("cursor hidden in normal mode", cursor_hidden());

    s.send("\x17h");
    wait_sessions_focused(&s);
    s.send("j\r");
    s.wait_for_all(&["2 flaky · Reviewer", "Group the sign-in commands"]);
    s.send("gT");
    s.wait_for_all(&["sample reply"]);
    s.send("gt");
    s.wait_for_all(&["Group the sign-in commands"]);

    s.send(":tabnew\r");
    s.wait_for_all(&["New tab: pick an agent", "Researcher"]);
    s.send("\r");
    s.wait_for_all(&["3 new session · Engineer", "INPUT", "g e n t s"]);
    s.send("\x1b");
    s.wait_for_all(&["NORMAL"]);
    s.send("\x17q");
    s.wait_gone("3 new session");
    s.wait_for_all(&["Group the sign-in commands", "NORMAL"]);

    s.send(":rev");
    s.wait_for_all(&["Jump to", ": rev", "TABS", "2 flaky · Reviewer", "JUMP"]);
    s.wait_until("cursor after rev", cursor_at(6, 37));
    s.send("\x1b");
    s.wait_gone("Jump to");
    s.wait_for_all(&["NORMAL"]);

    s.send(":acc\r");
    s.wait_for_all(&["signed in", "Jump to", MENU]);
    s.send(":");
    s.wait_for_all(&["JUMP"]);
    s.send("\x1b");
    s.wait_for_all(&[MENU]);
    s.send("\x1b");
    s.wait_gone("signed in");
    s.wait_for_all(&["JUMP"]);
    s.send("\x1b");
    s.wait_for_all(&["NORMAL"]);

    s.send("?");
    s.wait_for_all(&["HELP", "Move between panes"]);
    s.send("\x1b");
    s.wait_for_all(&["NORMAL"]);

    s.send("i/");
    s.wait_for_all(&["Commands", "/agent", "/quit"]);
    s.send("cl");
    s.wait_gone("/agent");
    s.wait_for_all(&["/clear"]);
    s.send("\t");
    s.wait_for_all(&["> /clear"]);
    s.send("\r");
    s.wait_gone("Commands");
    s.wait_gone("Group the sign-in commands");

    s.send(&format!("{}\r", "x".repeat(300)));
    s.wait_for_all(&["sample reply", "Message Reviewer"]);
    let text = s.text();
    let typed: usize = text
        .split(|c| c != 'x')
        .filter(|run| run.len() > 1)
        .map(str::len)
        .sum();
    assert_eq!(typed, 300, "{text}");

    resize_through_too_small(&s);
    s.send("\x1b");
    s.wait_for_all(&["NORMAL"]);
    s.send(":q\r");
    let e = s.finish();
    assert_eq!(e.code, 0);
    assert_restored(&e);
}

#[test]
fn emacs_flow() {
    let s = Shell::start(&["--keys", "emacs"], 36, 132);
    s.wait_for_all(&["sample data", "EMACS", "Sessions"]);

    s.send("hello");
    s.wait_for_all(&["> hello"]);
    s.send("\x01");
    s.wait_until("cursor at the line's start", cursor_at(32, 34));
    s.send("\x05");
    s.wait_until("cursor at the line's end", cursor_at(32, 39));
    s.send("\r");
    s.wait_for_all(&["sample reply"]);
    s.send("\x18o");
    wait_sessions_focused(&s);
    s.send("\x0e\r");
    s.wait_for_all(&["2 flaky · Reviewer", "Group the sign-in commands"]);
    s.send("\x18p");
    s.wait_for_all(&["sample reply"]);
    s.send("\x18n");
    s.wait_for_all(&["Group the sign-in commands"]);

    s.send("\x18t");
    s.wait_for_all(&["New tab: pick an agent"]);
    s.send("\r");
    s.wait_for_all(&["3 new session · Engineer", "g e n t s"]);
    s.send("\x180");
    s.wait_gone("3 new session");

    s.send("\x1bx");
    s.wait_for_all(&["Jump to"]);
    s.send("rev");
    s.wait_for_all(&["Jump to", ": rev", "TABS", "JUMP"]);
    s.send("\x07");
    s.wait_gone("Jump to");

    s.send("\x18xacc\r");
    s.wait_for_all(&["signed in", "Jump to", MENU]);
    s.send("\x1bx");
    s.wait_for_all(&["JUMP"]);
    s.send("\x07");
    s.wait_for_all(&[MENU]);
    s.send("\x07");
    s.wait_gone("signed in");
    s.wait_for_all(&["JUMP"]);
    s.send("\x07");
    s.wait_for_all(&["EMACS"]);

    // ctrl-s reaching the app proves raw mode turned flow control off.
    s.send("\x13");
    s.wait_for_all(&["SEARCH"]);
    s.send("sign");
    s.wait_for_all(&["SEARCH", "search: sign"]);
    s.send("\x07");
    s.wait_gone("search: sign");

    s.send("/");
    s.wait_for_all(&["Commands", "/agent"]);
    s.send("cl\t\r");
    s.wait_gone("Group the sign-in commands");

    resize_through_too_small(&s);
    s.send("\x18\x03");
    let e = s.finish();
    assert_eq!(e.code, 0);
    assert_restored(&e);
}

#[test]
fn vim_ctrl_w_deletes_a_word_while_writing_and_is_a_chord_in_normal_mode() {
    let s = Shell::start(&["--keys", "vim"], 36, 132);
    s.wait_for_all(&["INPUT"]);
    s.send("hello there");
    s.wait_for_all(&["> hello there"]);
    s.send("\x17q");
    s.wait_for_all(&["> hello q", "1 new session"]);
    s.send("\x1b");
    s.wait_for_all(&["NORMAL"]);
    s.send("\x17");
    s.wait_for_all(&["ctrl-w ..."]);
    s.send("\x08");
    wait_sessions_focused(&s);
    s.send(":q\r");
    assert_eq!(s.finish().code, 0);
}

#[test]
fn vim_esc_and_a_key_in_one_write_act_as_two_keys() {
    let s = Shell::start(&["--keys", "vim"], 36, 132);
    s.wait_for_all(&["INPUT"]);
    s.send("\x1b:");
    s.wait_for_all(&["Jump to", "JUMP"]);
    s.send("q\r");
    assert_eq!(s.finish().code, 0);
}

#[test]
fn too_small_at_start_still_quits() {
    let s = Shell::start(&["--keys", "emacs"], 10, 40);
    s.wait_for_all(&["Window too small"]);
    s.send("\x18\x03");
    assert_eq!(s.finish().code, 0);
}

#[test]
#[ignore = "writes SVG screenshots to GENTS_SHELL_CAPTURE_DIR for a person to review"]
fn capture_screens_for_review() {
    let dir = PathBuf::from(std::env::var_os("GENTS_SHELL_CAPTURE_DIR").expect("the directory"));
    std::fs::create_dir_all(&dir).unwrap();
    let shot = |s: &Shell, name: &str| {
        let path = dir.join(format!("{name}.svg"));
        s.screenshot(&path);
        let bytes = std::fs::metadata(&path).unwrap().len();
        assert!(bytes > 0, "{} is empty", path.display());
    };

    for (name, rows, cols) in [
        ("01-startup", 36, 132),
        ("05-wide", 48, 168),
        ("06-medium", 30, 104),
        ("07-narrow", 24, 70),
    ] {
        let s = Shell::start(&["--keys", "vim"], rows, cols);
        s.wait_for_all(&["sample data", "INPUT"]);
        shot(&s, name);
    }

    let s = Shell::start(&["--keys", "vim"], 36, 138);
    s.wait_for_all(&["INPUT"]);
    s.send("\x1b");
    s.wait_for_all(&["NORMAL"]);
    s.send("]");
    s.wait_for_all(&["no changes in this session"]);
    shot(&s, "15-sidebar-changes");
    s.send("\x17}");
    s.wait_for_all(&["2 running"]);
    shot(&s, "15-sidebar-workflows");
    s.send("\x17}");
    s.wait_for_all(&["3 background"]);
    shot(&s, "15-sidebar-agents");

    let s = Shell::start(&["--keys", "vim"], 36, 132);
    s.wait_for_all(&["INPUT"]);
    s.send("\x1b");
    s.wait_for_all(&["NORMAL"]);
    s.send("\x17h");
    wait_sessions_focused(&s);
    s.send("\r");
    s.wait_for_all(&["1 cli ux · Engineer"]);
    s.send("\x17h");
    wait_sessions_focused(&s);
    s.send("j\r");
    s.wait_for_all(&["2 flaky · Reviewer"]);
    s.send("\x17h");
    wait_sessions_focused(&s);
    s.send("j\r");
    s.wait_for_all(&["3 packs · Engineer"]);
    s.send("gt");
    s.wait_until(
        "the first tab in front",
        text_on_background("1 cli ux · Engineer", "#283f2d"),
    );
    shot(&s, "02-working");

    s.send("j");
    s.wait_for_all(&["AGENTS"]);
    shot(&s, "16-running-selected");
    s.send("\r");
    s.wait_for_all(&["Subagent: Reviewer", "Checking the new", "NORMAL"]);
    shot(&s, "17-subagent");
    // Leaving the tab drops the subagent: back on it, the pane shows the Diff view.
    s.send("gtgT");
    s.wait_for_all(&["session   working tree"]);
    s.wait_gone("Subagent: Reviewer");
    s.send("]");
    s.wait_gone("session   working tree");

    s.send("\x10\r");
    s.wait_for_all(&["Node settings", "UX Plugins"]);
    shot(&s, "18-node-settings");
    s.send("\x1b");
    s.wait_gone("UX Plugins");
    s.send("\x1b");
    s.wait_for_all(&["NORMAL"]);

    s.send(":acc\r");
    s.wait_for_all(&["signed in", MENU]);
    shot(&s, "03-menus");
    s.send("\x1b");
    s.wait_gone("signed in");
    s.send("\x1b");
    s.wait_for_all(&["NORMAL"]);

    s.send(":tabnew\r");
    s.wait_for_all(&["New tab: pick an agent", "Researcher"]);
    shot(&s, "04-agents");
    s.send("\x1b");
    s.wait_gone("New tab: pick an agent");
    s.send("\x1b");
    s.wait_for_all(&["NORMAL"]);

    s.send("?");
    s.wait_for_all(&["HELP", "Move between panes"]);
    shot(&s, "10-keys");
    s.send("\x1b");
    s.wait_for_all(&["NORMAL"]);

    s.send(":rev");
    s.wait_for_all(&[": rev", "JUMP"]);
    shot(&s, "13-jump");
    s.send("\x1b");
    s.wait_for_all(&["NORMAL"]);

    s.send("i/");
    s.wait_for_all(&["Commands", "/agent"]);
    shot(&s, "14-slash");
    s.send("\x7f");
    s.wait_gone("Commands");
    s.send("\x1b");
    s.wait_for_all(&["NORMAL"]);

    s.send("]");
    s.wait_for_all(&["3 files"]);
    s.send("\x17l\r");
    s.wait_for_all(&["esc back to files"]);
    shot(&s, "09-changes-one-file");
    s.send("\x1b");
    s.wait_gone("esc back to files");
    shot(&s, "09-changes-files");

    s.resize(24, 70);
    s.wait_gone("Sessions");
    s.wait_for_all(&["sample data"]);
    s.send("\x17[");
    s.wait_for_all(&["Sessions", MENU]);
    shot(&s, "08-narrow-sessions-open");
}
