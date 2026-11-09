use super::changes::{session_rows, tree_rows};
use super::{first_selectable, step_row, App};
use crate::sample::{note, RowAction, Scope, SidebarRow, Tone, View};
use crate::source::{BackgroundCall, Job, JobEnd};

pub(super) fn label(text: String) -> SidebarRow {
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

pub(super) fn item(text: String, tone: Tone, action: RowAction) -> SidebarRow {
    SidebarRow {
        text: text.into(),
        tone,
        action,
        ..note("", Tone::Plain)
    }
}

/// `session`'s jobs, unfinished first, each opened by its place in `jobs`. The open job's detail
/// follows its status row.
pub fn job_rows(
    jobs: &[Job],
    session: Option<&str>,
    as_of: &str,
    open: Option<&str>,
) -> Vec<SidebarRow> {
    let mut shown: Vec<_> = jobs
        .iter()
        .enumerate()
        .filter(|(_, job)| Some(job.session.as_str()) == session)
        .collect();
    shown.sort_by_key(|(_, job)| job.ended.is_some());
    let finished = shown.iter().filter(|(_, job)| job.ended.is_some()).count();
    let waiting = shown
        .iter()
        .filter(|(_, job)| job.ended.is_none() && job.waiting)
        .count();
    let running = shown.len() - finished - waiting;
    let mut counts = format!(" {running} running   {waiting} waiting");
    if finished > 0 {
        counts.push_str(&format!("   {finished} finished"));
    }
    let mut rows = vec![header(counts, as_of), note("", Tone::Plain)];
    for (index, job) in shown {
        let (mark, tone) = match (job.ended, job.waiting) {
            (Some(JobEnd::Done), _) => ('✓', Tone::Accent),
            (Some(JobEnd::Failed), _) => ('✗', Tone::Del),
            (None, true) => ('!', Tone::Warn),
            (None, false) => ('●', Tone::Plain),
        };
        let text = match job.title.as_str() {
            "" => format!(" {mark} {}", job.name),
            title => format!(" {mark} {}: {title}", job.name),
        };
        rows.push(item(text, tone, RowAction::OpenJob(index)));
        rows.push(label(format!("     {}", job.status)));
        // A running job shows its steps until opened; an ended one only once opened.
        if job.ended.is_none() != (open == Some(job.address.as_str())) {
            rows.extend(
                job.detail
                    .iter()
                    .map(|line| label(format!("       {line}"))),
            );
        }
    }
    rows
}

/// Background calls take no control yet, so their rows are not selectable.
pub fn background_rows(
    calls: &[BackgroundCall],
    session: Option<&str>,
    as_of: &str,
) -> Vec<SidebarRow> {
    let calls: Vec<_> = calls
        .iter()
        .filter(|call| Some(call.session.as_str()) == session)
        .collect();
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
        let name = format!(" {mark} {}", call.name);
        rows.push(item(name, tone, RowAction::None));
        rows.push(label(format!("     {}", call.status)));
    }
    rows
}

/// The nearest selectable row at or before `selected`, else the first.
pub(super) fn in_range(rows: &[SidebarRow], selected: usize) -> usize {
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
        let session = self.tabs[self.active].session.as_deref();
        match view {
            View::Diff => match self.scope {
                Scope::Session => session_rows(&self.tabs[self.active].files()),
                Scope::Tree => tree_rows(self.working_tree.as_ref()),
            },
            View::Workflows => job_rows(
                &self.jobs,
                session,
                &self.jobs_as_of,
                self.job_open.as_deref(),
            ),
            View::Agents => background_rows(&self.background, session, &self.background_as_of),
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

    use super::*;
    use crate::app::{Pane, Tab};
    use crate::keys::KeySet::Vim;
    use crate::sample;
    use crate::sample::Tone;
    use crate::source::{Intent, Update};
    use crate::testing::{
        find, press, screen, sent, session_row, type_text, with_right_pane, with_sample_session,
    };

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
            ended: None,
            detail: vec!["● build".to_string(), "○ test".to_string()],
        }
    }

    fn in_s2(job: Job) -> Job {
        Job {
            session: "s2".to_string(),
            ..job
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
        let mut app = with_right_pane(with_sample_session(App::new(Vim, 132, 36)));
        app.view = View::Workflows;
        press(&mut app, "esc ctrl-w l ctrl-w l");
        assert_eq!(app.focus, Pane::Sidebar);
        app
    }

    /// A connected app with the right pane open, whose tabs show `s1` and `s2`, on `s1`.
    fn on_two_sessions() -> (App, Receiver<Intent>) {
        let (intents, intended) = channel();
        let mut app = with_right_pane(App::connected(Vim, 132, 36, intents));
        app.tabs[0].session = Some("s1".to_string());
        let mut s2 = Tab::new_session();
        s2.session = Some("s2".to_string());
        s2.sidebar = true;
        app.push_tab(s2);
        (app, intended)
    }

    fn jobs(app: &mut App, jobs: Vec<Job>) {
        app.apply(Update::Jobs {
            jobs,
            as_of: String::new(),
        });
    }

    /// The selectable rows' text.
    fn listed(app: &App, view: View) -> Vec<String> {
        let rows = app.view_rows(view);
        let listed = rows.iter().filter(|row| row.selectable());
        listed.map(|row| row.text.to_string()).collect()
    }

    #[test]
    fn sample_views_match_the_old_rows() {
        let app = with_sample_session(App::new(Vim, 132, 36));
        let row = |text: &str, tone, selectable| (text.to_string(), tone, selectable);
        let workflows = [
            row(" 2 running   1 waiting", Tone::Label, false),
            row("", Tone::Plain, false),
            row(" ● change unit: cli ux", Tone::Plain, true),
            row("     build · step 3 of 5", Tone::Label, false),
            row("       ✓ plan", Tone::Label, false),
            row("       ✓ edit", Tone::Label, false),
            row("       ● build", Tone::Label, false),
            row("       ○ test", Tone::Label, false),
            row("       ○ review", Tone::Label, false),
            row(" ● nightly eval", Tone::Plain, true),
            row("     trial 12 of 40", Tone::Label, false),
            row(" ! pack release", Tone::Warn, true),
            row("     waiting on your approval", Tone::Label, false),
        ];
        assert_eq!(texts(&app.view_rows(View::Workflows)), workflows);
        let background = [
            row(" 3 background   1 done", Tone::Label, false),
            row("", Tone::Plain, false),
            row(" ● Reviewer · cli ux", Tone::Plain, false),
            row("     reading crates/gents-cli", Tone::Label, false),
            row(" ● Researcher · flaky test", Tone::Plain, false),
            row("     running for 4m", Tone::Label, false),
            row(" ● Engineer · pack sync", Tone::Plain, false),
            row("     running for 11m", Tone::Label, false),
            row(" ✓ Triage · inbox", Tone::Accent, false),
            row("     finished 2m ago", Tone::Label, false),
        ];
        assert_eq!(texts(&app.view_rows(View::Agents)), background);
    }

    #[test]
    fn a_running_jobs_steps_show_until_enter_folds_them() {
        let mut app = on_workflows();

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
        assert_eq!(app.view_rows(View::Workflows).len(), 13);
        assert_eq!(
            selected_text(&app),
            " ● nightly eval",
            "the selection follows its job"
        );
    }

    #[test]
    fn a_waiting_job_is_marked_and_counted() {
        let jobs = [job("a", false), job("b", true), job("c", true)];
        let rows = job_rows(&jobs, Some("s1"), "", None);

        assert_eq!(rows[0].text, " 1 running   2 waiting");
        assert_eq!(rows[6].text, " ! b job: fix login");
        assert_eq!(rows[6].tone, Tone::Warn);
        assert_eq!(rows[6].action, RowAction::OpenJob(1));
        assert_eq!(rows[2].tone, Tone::Plain);
    }

    #[test]
    fn the_header_says_when_the_rows_were_read() {
        let mut app = App::new(Vim, 132, 36);
        app.tabs[0].session = Some("s1".to_string());
        let calls = vec![BackgroundCall {
            session: "s1".to_string(),
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
        assert_eq!(
            background_rows(&calls, Some("s1"), "")[0].text,
            " 1 background"
        );
    }

    #[test]
    fn a_workflow_row_selects_its_job_by_address() {
        let mut app = App::new(Vim, 132, 36);
        app.open_session(session_row("cli ux"));
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
            let bar = Intent::ReadMenu(sample::MenuId::Workflows);
            let intents: Vec<_> = sent(intended)
                .into_iter()
                .filter(|intent| *intent != bar)
                .collect();
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
        app.set_view(View::Agents);
        assert_eq!(refreshes(&intended), 1);
        app.set_view(View::Diff);
        assert_eq!(refreshes(&intended), 0, "the Diff view reads nothing");
    }

    #[test]
    fn a_shorter_job_list_keeps_the_selection_in_range() {
        let mut app = on_workflows();
        press(&mut app, "j j");
        assert_eq!(selected_text(&app), " ! pack release");
        let session = app.tabs[app.active].session.clone().expect("the sample's");
        let tab_job = |address| Job {
            session: session.clone(),
            ..job(address, false)
        };

        app.apply(Update::Jobs {
            jobs: vec![tab_job("a")],
            as_of: String::new(),
        });
        assert_eq!(selected_text(&app), " ● a job: fix login");

        app.apply(Update::Jobs {
            jobs: vec![tab_job("b"), tab_job("a")],
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

    #[test]
    fn the_workflows_view_lists_only_this_tabs_sessions_runs() {
        let (mut app, intended) = on_two_sessions();
        app.view = View::Workflows;
        jobs(
            &mut app,
            vec![job("a", false), in_s2(job("b", false)), job("c", false)],
        );
        press(&mut app, "esc ctrl-w l ctrl-w l j");
        assert_eq!(app.focus, Pane::Sidebar);
        let before: Vec<_> = intended.try_iter().collect();

        assert_eq!(
            listed(&app, View::Workflows),
            [" ● a job: fix login", " ● c job: fix login"]
        );
        assert_eq!(
            app.view_rows(View::Workflows)[0].text,
            " 2 running   0 waiting"
        );
        assert_eq!(selected_text(&app), " ● c job: fix login");

        press(&mut app, "esc g t");

        assert_eq!(app.active, 1);
        let rows = app.view_rows(View::Workflows);
        assert_eq!(listed(&app, View::Workflows), [" ● b job: fix login"]);
        assert_eq!(rows[0].text, " 1 running   0 waiting");
        assert_eq!(
            rows[2].action,
            RowAction::OpenJob(1),
            "the job's place in the read"
        );
        assert_eq!(selected_text(&app), " ● b job: fix login");
        let after: Vec<_> = intended.try_iter().collect();
        assert!(
            after.is_empty(),
            "switching reads nothing: {after:?} after {before:?}"
        );
    }

    #[test]
    fn a_finished_run_keeps_its_steps_and_says_how_it_ended() {
        let (mut app, _intended) = on_two_sessions();
        app.view = View::Workflows;
        let ended = |address, status: &str, end| Job {
            status: status.to_string(),
            ended: Some(end),
            ..job(address, false)
        };
        let done = ended("a", "finished", JobEnd::Done);
        let failed = ended("b", "failed", JobEnd::Failed);
        jobs(&mut app, vec![done.clone(), failed.clone()]);

        let row = |text: &str, tone, selectable| (text.to_string(), tone, selectable);
        let shown = [
            row(" 0 running   0 waiting   2 finished", Tone::Label, false),
            row("", Tone::Plain, false),
            row(" ✓ a job: fix login", Tone::Accent, true),
            row("     finished", Tone::Label, false),
            row(" ✗ b job: fix login", Tone::Del, true),
            row("     failed", Tone::Label, false),
        ];
        assert_eq!(texts(&app.view_rows(View::Workflows)), shown);

        press(&mut app, "esc ctrl-w l ctrl-w l enter");
        assert_eq!(app.job_open.as_deref(), Some("a"));
        let rows = texts(&app.view_rows(View::Workflows));
        let steps: Vec<_> = rows[4..6].iter().map(|(text, ..)| text.as_str()).collect();
        assert_eq!(steps, ["       ● build", "       ○ test"]);

        jobs(&mut app, vec![done, job("c", false), failed]);
        assert_eq!(
            listed(&app, View::Workflows),
            [
                " ● c job: fix login",
                " ✓ a job: fix login",
                " ✗ b job: fix login"
            ],
            "unfinished runs first, each part in read order"
        );
        assert_eq!(
            app.view_rows(View::Workflows)[0].text,
            " 1 running   0 waiting   2 finished"
        );
    }

    #[test]
    fn a_tab_with_no_session_lists_no_work() {
        let app = App::new(Vim, 132, 36);
        assert_eq!(app.tabs[app.active].session, None);
        assert!(!app.jobs.is_empty() && !app.background.is_empty());

        let blank = (String::new(), Tone::Plain, false);
        assert_eq!(
            texts(&app.view_rows(View::Workflows)),
            [
                (" 0 running   0 waiting".to_string(), Tone::Label, false),
                blank.clone()
            ]
        );
        assert_eq!(
            texts(&app.view_rows(View::Agents)),
            [(" 0 background".to_string(), Tone::Label, false), blank]
        );
    }

    #[test]
    fn the_background_view_lists_only_this_tabs_sessions_calls() {
        let (mut app, intended) = on_two_sessions();
        app.view = View::Agents;
        let call = |session: &str, name: &str, done| BackgroundCall {
            session: session.to_string(),
            name: name.to_string(),
            status: "pending".to_string(),
            done,
        };
        let calls = vec![
            call("s1", "bash", false),
            call("s2", "grep", false),
            call("s1", "lint", true),
            call("", "from an old document", false),
        ];
        app.apply(Update::Background {
            calls,
            as_of: String::new(),
        });
        let shown = |app: &App| -> Vec<String> {
            let rows = app.view_rows(View::Agents);
            rows.iter().map(|row| row.text.to_string()).collect()
        };

        let s1 = [
            " 1 background   1 done",
            "",
            " ● bash",
            "     pending",
            " ✓ lint",
            "     pending",
        ];
        assert_eq!(shown(&app), s1);
        press(&mut app, "esc g t");
        assert_eq!(
            shown(&app),
            [" 1 background", "", " ● grep", "     pending"]
        );
        assert!(intended.try_iter().all(|intent| intent != Intent::Refresh));
    }

    #[test]
    fn selection_follows_the_open_job_across_a_reread_that_adds_another_sessions_job() {
        let (mut app, _intended) = on_two_sessions();
        app.view = View::Workflows;
        jobs(&mut app, vec![job("a", false), job("b", false)]);
        press(&mut app, "esc ctrl-w l ctrl-w l j enter");
        assert_eq!(app.job_open.as_deref(), Some("b"));

        let reread = vec![
            in_s2(job("x", false)),
            job("c", false),
            job("a", false),
            job("b", false),
        ];
        jobs(&mut app, reread);

        assert_eq!(selected_text(&app), " ● b job: fix login");
        assert_eq!(app.job_open.as_deref(), Some("b"));
        assert_eq!(
            listed(&app, View::Workflows),
            [
                " ● c job: fix login",
                " ● a job: fix login",
                " ● b job: fix login"
            ]
        );
    }
}
