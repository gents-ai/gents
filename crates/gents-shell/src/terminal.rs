use std::io::{self, stdout};

use ratatui::crossterm::{cursor, event, execute};
use ratatui::DefaultTerminal;

use crate::app::App;
use crate::draw;
use crate::keys::KeySet;

pub struct TerminalGuard {
    terminal: Option<DefaultTerminal>,
}

pub fn enter() -> io::Result<TerminalGuard> {
    let terminal = ratatui::try_init().inspect_err(|_| ratatui::restore())?;
    // Runs before the hook `try_init` installed, which leaves the alternate screen.
    let restore_and_report = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(stdout(), cursor::Show);
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
        let _ = execute!(stdout(), cursor::Show);
        ratatui::restore();
    }
}

pub fn run(set: KeySet) -> io::Result<()> {
    let mut guard = enter()?;
    let size = guard.terminal().size()?;
    let mut app = App::new(set, size.width, size.height);
    loop {
        guard.terminal().draw(|frame| draw::draw(&app, frame))?;
        app.handle(event::read()?);
        if app.should_quit() {
            return Ok(());
        }
    }
}
