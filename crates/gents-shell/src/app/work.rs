use super::{first_selectable, step_row, App};
use crate::sample::{self, note, RowAction, SidebarRow, Tone, View};
use crate::source::{BackgroundCall, Job};

fn label(text: String) -> SidebarRow {
    SidebarRow {
        text: text.into(),
        ..note("", Tone::Label)
    }
}

fn header(counts: String, as_of: &str) -> SidebarRow {
    match as_of {
        "" => label(counts),
        as_of => label(format!("{counts}   {as_of}")),
    }
}

fn item(text: String, tone: Tone, action: RowAction) -> SidebarRow {
    SidebarRow {
        text: text.into(),
        tone,
        action,
        ..note("", Tone::Plain)
    }
}

/// The open job's detail follows its status row.
pub fn job_rows(jobs: &[Job], as_of: &str, open: Option<&str>) -> Vec<SidebarRow> {
    let waiting = jobs.iter().filter(|job| job.waiting).count();
    let counts = format!(" {} running   {waiting} waiting", jobs.len() - waiting);
    let mut rows = vec![header(counts, as_of), note("", Tone::Plain)];
    for (index, job) in jobs.iter().enumerate() {
        let (mark, tone) = match job.waiting {
            true => ('!', Tone::Warn),
            false => ('●', Tone::Plain),
        };
        let text = match job.title.as_str() {
            "" => format!(" {mark} {}", job.name),
            title => format!(" {mark} {}: {title}", job.name),
        };
        rows.push(item(text, tone, RowAction::OpenJob(index)));
        rows.push(label(format!("     {}", job.status)));
        if open == Some(job.address.as_str()) {
            rows.extend(
                job.detail
                    .iter()
                    .map(|line| label(format!("       {line}"))),
            );
        }
    }
    rows
}

/// Background calls take no control yet, so their rows are not wired.
pub fn background_rows(calls: &[BackgroundCall], as_of: &str) -> Vec<SidebarRow> {
    let done = calls.iter().filter(|call| call.done).count();
    let mut counts = format!(" {} background", calls.len() - done);
    if done > 0 {
        counts.push_str(&format!("   {done} done"));
    }
    let mut rows = vec![header(counts, as_of), note("", Tone::Plain)];
    for call in calls {
        let (mark, tone) = match call.done {
            true => ('✓', Tone::Accent),
            false => ('●', Tone::Plain),
        };
        rows.push(item(
            format!(" {mark} {}", call.name),
            tone,
            RowAction::NotWired,
        ));
        rows.push(label(format!("     {}", call.status)));
    }
    rows
}

/// The nearest selectable row at or before `selected`, else the first.
fn in_range(rows: &[SidebarRow], selected: usize) -> usize {
    let selected = selected.min(rows.len().saturating_sub(1));
    let back = step_row(rows, selected, false);
    match rows.get(selected) {
        Some(row) if row.selectable() => selected,
        _ if rows.get(back).is_some_and(SidebarRow::selectable) => back,
        _ => first_selectable(rows),
    }
}

impl App {
    pub fn view_rows(&self, view: View) -> Vec<SidebarRow> {
        match view {
            View::Changes => sample::CHANGES.to_vec(),
            View::Workflows => job_rows(&self.jobs, &self.jobs_as_of, self.job_open.as_deref()),
            View::Agents => background_rows(&self.background, &self.background_as_of),
        }
    }

    /// The selected job stays selected while it is listed.
    pub(super) fn set_jobs(&mut self, jobs: Vec<Job>, as_of: String) {
        let rows = self.view_rows(View::Workflows);
        let selected = rows
            .get(self.view_sel[View::Workflows as usize])
            .and_then(|row| match row.action {
                RowAction::OpenJob(job) => Some(self.jobs[job].address.clone()),
                _ => None,
            });
        self.jobs = jobs;
        self.jobs_as_of = as_of;
        if !selected.is_some_and(|address| self.select_job(&address)) {
            let rows = self.view_rows(View::Workflows);
            let kept = in_range(&rows, self.view_sel[View::Workflows as usize]);
            self.view_sel[View::Workflows as usize] = kept;
        }
    }

    pub(super) fn set_background(&mut self, calls: Vec<BackgroundCall>, as_of: String) {
        self.background = calls;
        self.background_as_of = as_of;
        let rows = self.view_rows(View::Agents);
        self.view_sel[View::Agents as usize] =
            in_range(&rows, self.view_sel[View::Agents as usize]);
    }

    /// Opening a job closes the one open before, which can move this job's row.
    pub(super) fn toggle_job(&mut self, job: usize) {
        let address = self.jobs[job].address.clone();
        self.job_open = match self.job_open.take() {
            Some(open) if open == address => None,
            _ => Some(address.clone()),
        };
        self.select_job(&address);
    }

    /// Whether `address` is listed, and so now selected.
    pub(super) fn select_job(&mut self, address: &str) -> bool {
        let rows = self.view_rows(View::Workflows);
        let found = rows.iter().position(|row| match row.action {
            RowAction::OpenJob(job) => self.jobs[job].address == address,
            _ => false,
        });
        if let Some(row) = found {
            self.view_sel[View::Workflows as usize] = row;
        }
        found.is_some()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{channel, Receiver};

    use ratatui::crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

    use super::*;
    use crate::app::Pane;
    use crate::keys::KeySet::Vim;
    use crate::sample::Tone;
    use crate::source::{Intent, Update};
    use crate::testing::{find, press, screen, session_row, type_text, with_right_pane};

    fn texts(rows: &[SidebarRow]) -> Vec<(String, Tone, bool)> {
        rows.iter()
            .map(|row| (row.text.to_string(), row.tone, row.selectable()))
            .collect()
    }

    fn job(address: &str, waiting: bool) -> Job {
        Job {
            address: address.to_string(),
            session: "s1".to_string(),
            name: format!("{address} job"),
            title: "fix login".to_string(),
            status: "build · step 1 of 2".to_string(),
            waiting,
            detail: vec!["● build".to_string(), "○ test".to_string()],
        }
    }

    fn selected_text(app: &App) -> String {
        let rows = app.view_rows(View::Workflows);
        rows[app.view_sel[View::Workflows as usize]]
            .text
            .to_string()
    }

    /// The sample app with the sidebar focused on the Workflows view's first job.
    fn on_workflows() -> App {
        let mut app = with_right_pane(App::new(Vim, 132, 36));
        app.view = View::Workflows;
        press(&mut app, "esc ctrl-w l ctrl-w l");
        assert_eq!(app.focus, Pane::Sidebar);
        app
    }

    #[test]
    fn sample_views_match_the_old_rows() {
        let app = App::new(Vim, 132, 36);
        let row = |text: &str, tone, selectable| (text.to_string(), tone, selectable);
        let workflows = [
            row(" 2 running   1 waiting", Tone::Label, false),
            row("", Tone::Plain, false),
            row(" ● change unit: cli ux", Tone::Plain, true),
            row("     build · step 3 of 5", Tone::Label, false),
            row(" ● nightly eval", Tone::Plain, true),
            row("     trial 12 of 40", Tone::Label, false),
            row(" ! pack release", Tone::Warn, true),
            row("     waiting on your approval", Tone::Label, false),
        ];
        assert_eq!(texts(&app.view_rows(View::Workflows)), workflows);
        let background = [
            row(" 3 background   1 done", Tone::Label, false),
            row("", Tone::Plain, false),
            row(" ● Reviewer · cli ux", Tone::Plain, true),
            row("     reading crates/gents-cli", Tone::Label, false),
            row(" ● Researcher · flaky test", Tone::Plain, true),
            row("     running for 4m", Tone::Label, false),
            row(" ● Engineer · pack sync", Tone::Plain, true),
            row("     running for 11m", Tone::Label, false),
            row(" ✓ Triage · inbox", Tone::Accent, true),
            row("     finished 2m ago", Tone::Label, false),
        ];
        assert_eq!(texts(&app.view_rows(View::Agents)), background);
    }

    #[test]
    fn a_job_row_opens_its_phases_and_closes_again() {
        let mut app = on_workflows();

        press(&mut app, "enter");

        let rows = texts(&app.view_rows(View::Workflows));
        let shown: Vec<_> = rows[2..10].iter().map(|(text, ..)| text.as_str()).collect();
        let open = [
            " ● change unit: cli ux",
            "     build · step 3 of 5",
            "       ✓ plan",
            "       ✓ edit",
            "       ● build",
            "       ○ test",
            "       ○ review",
            " ● nightly eval",
        ];
        assert_eq!(shown, open);
        assert!(find(&screen(&app), "○ review").is_some());
        assert_eq!(app.notice, None);

        press(&mut app, "enter");
        assert_eq!(app.view_rows(View::Workflows).len(), 8);

        press(&mut app, "enter j enter");
        assert_eq!(app.job_open.as_deref(), Some("sample:nightly-eval"));
        assert_eq!(app.view_rows(View::Workflows).len(), 8);
        assert_eq!(
            selected_text(&app),
            " ● nightly eval",
            "the selection follows its job"
        );
    }

    #[test]
    fn a_waiting_job_is_marked_and_counted() {
        let rows = job_rows(&[job("a", false), job("b", true), job("c", true)], "", None);

        assert_eq!(rows[0].text, " 1 running   2 waiting");
        assert_eq!(rows[4].text, " ! b job: fix login");
        assert_eq!(rows[4].tone, Tone::Warn);
        assert_eq!(rows[4].action, RowAction::OpenJob(1));
        assert_eq!(rows[2].tone, Tone::Plain);
    }

    #[test]
    fn the_header_says_when_the_rows_were_read() {
        let mut app = App::new(Vim, 132, 36);
        let calls = vec![BackgroundCall {
            name: "bash".to_string(),
            status: "running for 4m".to_string(),
            done: false,
        }];

        app.apply(Update::Jobs {
            jobs: vec![job("a", false)],
            as_of: "as of 14:05".to_string(),
        });
        app.apply(Update::Background {
            calls: calls.clone(),
            as_of: "live".to_string(),
        });

        let header = |view| app.view_rows(view)[0].text.to_string();
        assert_eq!(
            header(View::Workflows),
            " 1 running   0 waiting   as of 14:05"
        );
        assert_eq!(header(View::Agents), " 1 background   live");
        assert_eq!(background_rows(&calls, "")[0].text, " 1 background");
    }

    #[test]
    fn a_workflow_row_selects_its_job_by_address() {
        let mut app = App::new(Vim, 132, 36);
        app.open_session(session_row("pack registry"));
        let mut jobs = sample::jobs();
        jobs.rotate_left(1);
        app.apply(Update::Jobs {
            jobs,
            as_of: String::new(),
        });

        press(&mut app, "j j j j enter");

        assert_eq!(app.view, View::Workflows);
        assert_eq!(selected_text(&app), " ● change unit: cli ux");
    }

    /// The source is gone, so asking it would only replace the reason with a vaguer one.
    #[test]
    fn a_lost_shell_shows_a_work_view_without_asking() {
        let (intents, intended) = channel();
        let mut app = App::connected(Vim, 132, 36, intents);
        let why = "runtime closed: check `gents status`, then reopen the shell (gone)";
        app.apply(Update::Connection(crate::source::Connection::Lost(
            why.to_string(),
        )));
        app.set_view(View::Workflows);
        assert!(intended.try_recv().is_err());
        assert_eq!(
            app.connection,
            crate::source::Connection::Lost(why.to_string())
        );
    }

    #[test]
    fn showing_a_work_view_asks_for_a_refresh() {
        let connected = || {
            let (intents, intended) = channel();
            let app = with_right_pane(App::connected(Vim, 132, 36, intents));
            (app, intended)
        };
        let refreshes = |intended: &Receiver<Intent>| {
            let intents: Vec<_> = intended.try_iter().collect();
            assert!(
                intents.iter().all(|intent| *intent == Intent::Refresh),
                "{intents:?}"
            );
            intents.len()
        };

        let (mut app, intended) = connected();
        press(&mut app, "esc :");
        type_text(&mut app, "workflows");
        press(&mut app, "enter");
        assert_eq!(app.view, View::Workflows);
        assert_eq!(refreshes(&intended), 1);

        let (mut app, intended) = connected();
        type_text(&mut app, "/jobs");
        press(&mut app, "enter");
        assert_eq!(app.view, View::Agents);
        assert_eq!(refreshes(&intended), 1);

        let (mut app, intended) = connected();
        let (row, column) = find(&screen(&app), "Agents").expect("the chip");
        app.handle(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(app.view, View::Agents);
        assert_eq!(refreshes(&intended), 1);
        let (row, column) = find(&screen(&app), "Changes").expect("the chip");
        app.handle(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(refreshes(&intended), 0, "the Changes view reads nothing");
    }

    #[test]
    fn a_shorter_job_list_keeps_the_selection_in_range() {
        let mut app = on_workflows();
        press(&mut app, "j j");
        assert_eq!(selected_text(&app), " ! pack release");

        app.apply(Update::Jobs {
            jobs: vec![job("a", false)],
            as_of: String::new(),
        });
        assert_eq!(selected_text(&app), " ● a job: fix login");

        app.apply(Update::Jobs {
            jobs: vec![job("b", false), job("a", false)],
            as_of: String::new(),
        });
        assert_eq!(
            selected_text(&app),
            " ● a job: fix login",
            "kept by address"
        );

        app.apply(Update::Jobs {
            jobs: Vec::new(),
            as_of: String::new(),
        });
        let selected = app.view_sel[View::Workflows as usize];
        assert!(selected < app.view_rows(View::Workflows).len());
        press(&mut app, "enter");
        assert_eq!(app.notice, None);
    }
}
