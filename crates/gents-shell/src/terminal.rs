use std::io::{self, stdout, Write};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use ratatui::crossterm::event::{Event, KeyboardEnhancementFlags};
use ratatui::crossterm::{cursor, event, execute, terminal};
use ratatui::DefaultTerminal;

use crate::app::App;
use crate::draw;
use crate::keys::KeySet;
use crate::source::{Input, Intent};

/// The kitty keyboard protocol's least, as Codex asks for it: shift-enter, and cmd with the
/// arrows or backspace, arrive as themselves, and a multiplexer such as herdr encodes them so
/// only for a pane that asked. No event types: each key would also report its release.
const KEYBOARD: KeyboardEnhancementFlags = KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
    .union(KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS);

pub struct TerminalGuard {
    terminal: Option<DefaultTerminal>,
}

pub fn enter() -> io::Result<TerminalGuard> {
    let terminal = ratatui::try_init().inspect_err(|_| ratatui::restore())?;
    execute!(
        stdout(),
        event::EnableMouseCapture,
        event::EnableBracketedPaste
    )
    .inspect_err(|_| ratatui::restore())?;
    // Pushed without asking, as Codex does: a terminal without the protocol ignores it, and a
    // query waits up to 2 s on one that never answers. Pushed on the alternate screen, whose
    // stack is its own, so it is popped before leaving it. Fails only on the legacy Windows
    // console, which keeps today's keys.
    let _ = execute!(stdout(), event::PushKeyboardEnhancementFlags(KEYBOARD));
    // Runs before the hook `try_init` installed, which leaves the alternate screen.
    let restore_and_report = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(
            stdout(),
            event::DisableMouseCapture,
            event::DisableBracketedPaste,
            cursor::Show,
            event::PopKeyboardEnhancementFlags
        );
        restore_and_report(info);
    }));
    Ok(TerminalGuard {
        terminal: Some(terminal),
    })
}

impl TerminalGuard {
    pub fn terminal(&mut self) -> &mut DefaultTerminal {
        self.terminal.as_mut().expect("present until dropped")
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // The panic hook has already restored the terminal and printed the message, and
        // anything written now would land after it. `forget` because `Terminal`'s own Drop
        // writes too.
        if std::thread::panicking() {
            std::mem::forget(self.terminal.take());
            return;
        }
        // `Terminal`'s own Drop may write show-cursor; leaving the alternate screen must be
        // the last thing written.
        drop(self.terminal.take());
        let _ = execute!(
            stdout(),
            event::DisableMouseCapture,
            event::DisableBracketedPaste,
            cursor::Show,
            event::PopKeyboardEnhancementFlags
        );
        ratatui::restore();
    }
}

pub fn run(set: KeySet) -> io::Result<()> {
    let (width, height) = terminal::size()?;
    let (sender, receiver) = mpsc::channel();
    run_app(App::new(set, width, height), sender, receiver)
}

/// `sender` and `receiver` are one channel; the source keeps its own clone of `sender`.
pub fn run_connected(
    set: KeySet,
    intents: Sender<Intent>,
    sender: Sender<Input>,
    receiver: Receiver<Input>,
) -> io::Result<()> {
    let (width, height) = terminal::size()?;
    run_app(
        App::connected(set, width, height, intents),
        sender,
        receiver,
    )
}

fn run_app(app: App, sender: Sender<Input>, receiver: Receiver<Input>) -> io::Result<()> {
    let mut guard = enter()?;
    // A plain thread, not the runtime's blocking pool: it stays parked in `read` after quit,
    // and the process must not wait for it.
    thread::Builder::new()
        .name("gents-shell-input".to_string())
        .spawn(move || forward(event::read, sender))?;
    run_loop(app, &receiver, |app| {
        guard.terminal().draw(|frame| draw::draw(app, frame))?;
        Ok(())
    })
}

/// Sends each event until a read fails, then sends that failure last.
fn forward<R>(mut read: R, sender: Sender<Input>)
where
    R: FnMut() -> io::Result<Event>,
{
    loop {
        let (input, failed) = match read() {
            Ok(event) => (Input::Terminal(event), false),
            Err(error) => (Input::TerminalError(error), true),
        };
        if sender.send(input).is_err() || failed {
            return;
        }
    }
}

/// Takes every queued input, then renders once if any could change the screen: a render costs
/// a pass over the transcript, and the terminal reports each mouse move. Ends on quit, or with a
/// failed terminal read.
fn run_loop<F>(mut app: App, receiver: &Receiver<Input>, mut render: F) -> io::Result<()>
where
    F: FnMut(&App) -> io::Result<()>,
{
    render(&app)?;
    loop {
        let Ok(first) = receiver.recv() else {
            return Err(io::Error::other("the terminal stopped sending input"));
        };
        let mut changed = false;
        for input in std::iter::once(first).chain(receiver.try_iter()) {
            changed |= !inert(&input);
            match input {
                Input::Terminal(event) => {
                    app.handle(event);
                    if let Some(text) = app.take_clipboard() {
                        copy(&text)?;
                    }
                }
                Input::Runtime(update) => app.apply(update),
                Input::TerminalError(error) => return Err(error),
            }
            if app.should_quit() {
                return Ok(());
            }
        }
        if changed {
            render(&app)?;
        }
    }
}

/// Inputs `App::handle` ignores: pointer motion, releases, and key releases.
fn inert(input: &Input) -> bool {
    use event::{KeyEventKind, MouseEventKind};
    match input {
        Input::Terminal(Event::Mouse(mouse)) => matches!(
            mouse.kind,
            MouseEventKind::Moved | MouseEventKind::Drag(_) | MouseEventKind::Up(_)
        ),
        Input::Terminal(Event::Key(key)) => key.kind != KeyEventKind::Press,
        _ => false,
    }
}

/// OSC 52: the terminal puts `text` on the system clipboard.
fn copy(text: &str) -> io::Result<()> {
    let mut out = stdout();
    write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()))?;
    out.flush()
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let bits = chunk.iter().enumerate().fold(0u32, |bits, (at, &byte)| {
            bits | u32::from(byte) << (16 - 8 * at)
        });
        for at in 0..4 {
            match at <= chunk.len() {
                true => out.push(char::from(ALPHABET[(bits >> (18 - 6 * at) & 63) as usize])),
                false => out.push('='),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::KeySet::Vim;

    #[test]
    fn base64_matches_known_vectors() {
        let vectors = [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("key:1", "a2V5OjE="),
        ];
        for (text, encoded) in vectors {
            assert_eq!(base64(text.as_bytes()), encoded, "{text:?}");
        }
    }

    fn failure() -> io::Error {
        io::Error::other("the tty went away")
    }

    #[test]
    fn a_read_failure_is_forwarded_and_ends_the_input_thread() {
        let (sender, receiver) = mpsc::channel();
        let mut reads = [Ok(Event::FocusGained), Err(failure())].into_iter();
        forward(
            move || reads.next().expect("no read after a failure"),
            sender,
        );
        assert!(matches!(
            receiver.recv(),
            Ok(Input::Terminal(Event::FocusGained))
        ));
        let Ok(Input::TerminalError(error)) = receiver.recv() else {
            panic!("the failure is forwarded");
        };
        assert_eq!(error.to_string(), "the tty went away");
        assert!(
            receiver.recv().is_err(),
            "the input thread let go of its sender"
        );
    }

    #[test]
    fn a_terminal_read_failure_ends_the_loop_with_its_error() {
        let (sender, receiver) = mpsc::channel();
        sender.send(Input::Terminal(Event::FocusGained)).unwrap();
        sender.send(Input::TerminalError(failure())).unwrap();
        let mut renders = 0;
        let ended = run_loop(App::new(Vim, 80, 24), &receiver, |_| {
            renders += 1;
            Ok(())
        });
        let error = ended.expect_err("the loop ends on the failure");
        assert_eq!(error.to_string(), "the tty went away");
        assert_eq!(renders, 1, "one render before the queued inputs");
        drop(sender);
    }

    #[test]
    fn a_queued_batch_draws_once_and_mouse_motion_draws_nothing() {
        let (sender, receiver) = mpsc::channel();
        let mut sender = Some(sender);
        let moved = || {
            Input::Terminal(Event::Mouse(event::MouseEvent {
                kind: event::MouseEventKind::Moved,
                column: 3,
                row: 3,
                modifiers: event::KeyModifiers::NONE,
            }))
        };
        let mut renders = 0;
        let ended = run_loop(App::new(Vim, 80, 24), &receiver, |_| {
            renders += 1;
            if renders == 1 {
                let sender = sender.as_ref().expect("held until the motion");
                for _ in 0..20 {
                    sender.send(Input::Terminal(Event::FocusGained)).unwrap();
                }
            } else if let Some(sender) = sender.take() {
                for _ in 0..20 {
                    sender.send(moved()).unwrap();
                }
            }
            Ok(())
        });
        let error = ended.expect_err("the loop ends when its input stops");
        assert_eq!(error.to_string(), "the terminal stopped sending input");
        assert_eq!(
            renders, 2,
            "one draw for the 20 queued, none for the motion"
        );
    }
}
