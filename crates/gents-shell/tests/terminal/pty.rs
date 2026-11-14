use std::path::Path;
use std::time::{Duration, Instant};

use tui_test::{
    AutomaticRecording, AutomaticRecordingMode, CellColor, ExecutionContext, LocatorExpectOptions,
    LocatorQuery, OpenOptions, Operation, OperationResult, RunOptions, Session, TextStyle,
    Timeouts, TraceMode, TraceOptions, TuiTestError,
};

const WAIT: Duration = Duration::from_secs(20);
/// Concurrent `openpty` calls on macOS can fail with the kernel's internal EREDRIVEOPEN, errno -6,
/// which asks the caller to open again. Nothing has run yet, and the next attempt succeeds.
const OPEN_AGAIN: &str = "Unknown error: -6";

/// The shell under test. Read at run time: an archived test binary runs on
/// another machine, where the compile-time path does not exist.
pub fn gents_shell() -> String {
    std::env::var("CARGO_BIN_EXE_gents-shell")
        .expect("cargo test and nextest set CARGO_BIN_EXE_gents-shell")
}

pub fn cursor_at(row: u16, col: u16) -> Operation {
    Operation::ExpectCursor {
        visible: Some(true),
        shape: None,
        x: Some(col),
        y: Some(row),
        timeout_ms: None,
    }
}

pub fn cursor_hidden() -> Operation {
    Operation::ExpectCursor {
        visible: Some(false),
        shape: None,
        x: None,
        y: None,
        timeout_ms: None,
    }
}

pub fn text_on_background(text: &str, background: &str) -> Operation {
    Operation::WaitLocator {
        query: LocatorQuery {
            style: TextStyle {
                background: Some(background.to_string()),
                ..TextStyle::default()
            },
            ..LocatorQuery::text(text)
        },
        not: false,
        timeout_ms: None,
    }
}

pub struct Shell {
    session: Session,
}

pub struct Exit {
    pub code: i32,
    pub alternate_screen: bool,
    pub cursor_visible: bool,
    /// The recording as asciicast lines, so escapes read as `\u001b`.
    pub raw: String,
    pub text: String,
    // Lives as long as the test, so a failed assertion on this `Exit` still leaves a trace.
    _session: Session,
}

impl Shell {
    pub fn start(args: &[&str], rows: u16, cols: u16) -> Shell {
        Shell::start_command(&gents_shell(), args, &[], rows, cols)
    }

    pub fn start_command(
        program: &str,
        args: &[&str],
        env: &[(&str, &str)],
        rows: u16,
        cols: u16,
    ) -> Shell {
        let test = std::thread::current();
        // Beside the test binary's target directory (CARGO_TARGET_TMPDIR is a
        // compile-time path, absent where an archived binary runs).
        let exe = std::env::current_exe().expect("test binary path");
        let target = exe
            .ancestors()
            .nth(3)
            .expect("test binary is under <target>/<profile>/deps");
        let dir = target
            .join("tmp")
            .join("terminal")
            .join(test.name().unwrap_or("unnamed"));
        let context = ExecutionContext {
            trace: Some(TraceOptions {
                mode: TraceMode::OnFailure,
                directory: dir.clone(),
            }),
            ..ExecutionContext::default()
        };
        let wait = Some(WAIT.as_millis() as u64);
        let defaults = OpenOptions::default();
        let options = RunOptions {
            backend: defaults.backend,
            program: program.to_string(),
            args: args.iter().map(ToString::to_string).collect(),
            profile: defaults.profile,
            cols,
            rows,
            cwd: None,
            env: std::iter::once(("NO_COLOR".to_owned(), String::new()))
                .chain(
                    env.iter()
                        .map(|(key, value)| (key.to_string(), value.to_string())),
                )
                .collect(),
            wait_ready: Some(false),
            restart: false,
            timeouts: Timeouts {
                text: wait,
                idle: wait,
                exit: wait,
                ..Timeouts::default()
            },
            // Without a directory the recording lands in the user's cache directory.
            recording: AutomaticRecording {
                mode: AutomaticRecordingMode::OnFailure,
                directory: Some(dir),
            },
            style: defaults.style,
        };
        let deadline = Instant::now() + WAIT;
        loop {
            let session = Session::new("gents-shell").with_execution_context(context.clone());
            match session.run(options.clone()) {
                Ok(_) => return Shell { session },
                Err(error) if error.message.contains(OPEN_AGAIN) && Instant::now() < deadline => {}
                Err(error) => panic!("{program}: {error}"),
            }
        }
    }

    pub fn send(&self, text: &str) {
        let data = text.to_string();
        self.ok("send", self.session.execute(Operation::Write { data }));
    }

    /// Waits on one operation the tool bounds and polls, and fails with the screen.
    ///
    /// - Send nothing before the first frame is on screen. Input before raw mode is cooked, and
    ///   `ctrl-c` would become a signal.
    /// - After sending `esc`, wait for its effect before sending more; otherwise crossterm reads
    ///   the next bytes as an escape sequence, and two `esc` as one.
    /// - Wait on what only the next frame shows, since a frame can arrive in several chunks, and
    ///   read nothing about that frame outside a wait.
    pub fn wait_until(&self, what: &str, until: Operation) {
        self.ok(what, self.session.execute(until));
    }

    /// Returns on a single read of the screen that holds every needle.
    pub fn wait_for_all(&self, needles: &[&str]) {
        let deadline = Instant::now() + WAIT;
        loop {
            let text = self.text();
            let Some(missing) = needles.iter().find(|needle| !text.contains(**needle)) else {
                return;
            };
            assert!(
                Instant::now() < deadline,
                "{needles:?} were never all on screen at once:\n{text}"
            );
            self.ok(missing, self.session.get_by_text(*missing).expect());
        }
    }

    pub fn wait_gone(&self, needle: &str) {
        let options = LocatorExpectOptions {
            not: true,
            timeout_ms: None,
        };
        let gone = self.session.get_by_text(needle).expect_with(options);
        self.ok(needle, gone);
    }

    pub fn resize(&self, rows: u16, cols: u16) {
        let resize = Operation::Resize { cols, rows };
        self.ok("resize", self.session.execute(resize));
    }

    pub fn finish(self) -> Exit {
        self.wait_until("exit", Operation::WaitExit { timeout_ms: None });
        let state = match self.session.execute(Operation::State) {
            Ok(OperationResult::State(state)) => state,
            other => panic!("State returned {other:?}"),
        };
        Exit {
            code: state.exited.expect("the process has exited"),
            alternate_screen: state.modes["alternate_screen"],
            cursor_visible: state.cursor.visible,
            raw: self.session.recording().unwrap(),
            text: state.text,
            _session: self.session,
        }
    }

    pub fn text(&self) -> String {
        match self.session.execute(Operation::Text { full: false }) {
            Ok(OperationResult::Text(text)) => text,
            other => panic!("Text returned {other:?}"),
        }
    }

    pub fn bg(&self, row: u16, col: u16) -> CellColor {
        let cell = Operation::Cells {
            x: col,
            y: row,
            w: 1,
            h: 1,
        };
        match self.session.execute(cell) {
            Ok(OperationResult::Cells(cells)) => cells[0].bg.clone(),
            other => panic!("Cells returned {other:?}"),
        }
    }

    /// Writes the settled screen as SVG, the one format the tool has without an optional feature.
    pub fn screenshot(&self, path: &Path) {
        self.wait_until("idle", Operation::WaitIdle { timeout_ms: None });
        let shot = Operation::Screenshot {
            full: false,
            path: Some(path.display().to_string()),
            zoom: None,
            background: None,
        };
        self.ok("screenshot", self.session.execute(shot));
    }

    fn ok<T>(&self, what: &str, result: Result<T, TuiTestError>) -> T {
        result.unwrap_or_else(|error| panic!("{what}: {error}\n{}", self.text()))
    }
}
