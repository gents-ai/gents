use std::io::{self, stdout};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use ratatui::crossterm::event::Event;
use ratatui::crossterm::{cursor, event, execute, terminal};
use ratatui::DefaultTerminal;

use crate::app::App;
use crate::draw;
use crate::keys::KeySet;
use crate::source::{Input, Intent};

pub struct TerminalGuard {
    terminal: Option<DefaultTerminal>,
}

pub fn enter() -> io::Result<TerminalGuard> {
    let terminal = ratatui::try_init().inspect_err(|_| ratatui::restore())?;
    execute!(stdout(), event::EnableMouseCapture).inspect_err(|_| ratatui::restore())?;
    // Runs before the hook `try_init` installed, which leaves the alternate screen.
    let restore_and_report = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(stdout(), event::DisableMouseCapture, cursor::Show);
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
        let _ = execute!(stdout(), event::DisableMouseCapture, cursor::Show);
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

/// Renders before each input. Ends on quit, or with a failed terminal read.
fn run_loop<F>(mut app: App, receiver: &Receiver<Input>, mut render: F) -> io::Result<()>
where
    F: FnMut(&App) -> io::Result<()>,
{
    loop {
        render(&app)?;
        match receiver.recv() {
            Ok(Input::Terminal(event)) => app.handle(event),
            Ok(Input::Runtime(update)) => app.apply(update),
            Ok(Input::TerminalError(error)) => return Err(error),
            Err(_) => return Err(io::Error::other("the terminal stopped sending input")),
        }
        if app.should_quit() {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::KeySet::Vim;

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
        assert_eq!(renders, 2, "a render before each input");
        drop(sender);
    }
}
