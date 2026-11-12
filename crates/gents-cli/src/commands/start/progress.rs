use gents_protocol::operations::install_repair::{Finding, RepairStep};
use std::io::{IsTerminal as _, Write as _};

use gents_service::repair::Progress;

pub(super) fn bar(done: usize, total: usize, width: usize) -> String {
    let filled = (done * width)
        .checked_div(total)
        .unwrap_or(width)
        .min(width);
    format!("[{}{}]", "█".repeat(filled), "░".repeat(width - filled))
}

/// One line per finding, then a bar redrawn in place while it is repaired, or
/// a line per step when stderr is not a terminal. With nothing repaired, only
/// the wait is named, as before repairs existed.
#[derive(Default)]
pub(super) struct TerminalProgress {
    reported: bool,
    terminal: bool,
}

impl TerminalProgress {
    pub(super) fn new() -> Self {
        Self {
            reported: false,
            terminal: std::io::stderr().is_terminal(),
        }
    }

    /// Ends a drawn bar's line, so what follows starts on its own.
    pub(super) fn finish(&mut self) {
        if std::mem::take(&mut self.reported) && self.terminal {
            eprintln!();
        }
    }

    fn finding_text(&mut self, finding: &Finding) -> String {
        let clear = if self.reported && self.terminal {
            "\r\x1b[2K"
        } else {
            ""
        };
        self.reported = true;
        format!("{clear}Repairing: {finding}.\n")
    }

    fn step_text(&self, index: usize, total: usize, step: RepairStep) -> Option<String> {
        if !self.reported {
            return (step == RepairStep::WaitForNode).then(|| {
                format!(
                    "Waiting for this home's runtime to answer (up to {} s).\n",
                    gents_service::repair::ENDPOINT_WAIT.as_secs()
                )
            });
        }
        Some(if self.terminal {
            format!("\r\x1b[2K{} {index}/{total} {step}", bar(index, total, 20))
        } else {
            format!("{index}/{total} {step}\n")
        })
    }
}

impl Progress for TerminalProgress {
    fn finding(&mut self, finding: &Finding) {
        eprint!("{}", self.finding_text(finding));
    }

    fn step(&mut self, index: usize, total: usize, step: RepairStep) {
        if let Some(text) = self.step_text(index, total, step) {
            eprint!("{text}");
            let _ = std::io::stderr().flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_a_terminal_each_step_is_a_plain_line() {
        let mut progress = TerminalProgress::default();
        let text = [
            progress.finding_text(&Finding::StaleNode),
            progress.finding_text(&Finding::ServiceProgramMissing),
            progress.step_text(1, 2, RepairStep::StopOldNode).unwrap(),
        ]
        .concat();
        assert!(!text.contains(['\r', '\x1b']), "{text:?}");
        assert!(text.ends_with("1/2 Stopping the old node\n"), "{text:?}");
        let mut drawn = TerminalProgress {
            terminal: true,
            ..TerminalProgress::default()
        };
        drawn.finding_text(&Finding::StaleNode);
        assert!(drawn
            .step_text(1, 2, RepairStep::StopOldNode)
            .unwrap()
            .starts_with("\r\x1b[2K"));
    }

    #[test]
    fn the_bar_fills_by_steps() {
        assert_eq!(bar(0, 3, 4), "[░░░░]");
        assert_eq!(bar(1, 2, 4), "[██░░]");
        assert_eq!(bar(3, 3, 4), "[████]");
    }
}
