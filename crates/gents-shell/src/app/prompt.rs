use ratatui::layout::Rect;

use super::layers::LayerKind;
use super::{wrap_lines, App, Mode, Pane, Tab};
use crate::keys::{KeyCtx, KeySet};
use crate::sample::Tone;
use crate::source::{Answer, Connection, Intent, Prompt};

const MUST_ANSWER: &str = "this question needs an answer; it cannot be dismissed";

/// How `prompt` is answered with `set`'s keys, or in a Sidebar layer, which takes enter and `d`
/// and no typed text.
fn key_row(prompt: &Prompt, set: KeySet, layer: bool) -> String {
    if layer && typed(prompt) {
        return "  too narrow to answer here".to_string();
    }
    let vim = set == KeySet::Vim && !layer;
    let mut row = match &prompt.answer {
        Answer::Ack if vim => "  y ok".to_string(),
        Answer::Ack => "  enter ok".to_string(),
        Answer::Reply => "  type a reply, enter sends".to_string(),
        Answer::Choose {
            options,
            free_text,
            skip,
        } => {
            let mut row = match options.len() {
                0 => "  type your answer".to_string(),
                n if *free_text => format!("  type 1-{n}, or your own words"),
                n => format!("  type 1-{n}"),
            };
            if *skip {
                row.push_str(", or skip");
            }
            row + ", enter sends"
        }
        Answer::Document { collection, .. } => {
            let copy = if vim {
                "y copy its key"
            } else {
                "enter copies its key"
            };
            let document = match collection.as_str() {
                "" => "a document".to_string(),
                collection => format!("a {collection} document"),
            };
            format!("  answer with {document}   {copy}")
        }
    };
    if prompt.dismissable {
        row.push_str(match (layer, vim) {
            (true, _) => "   d dismiss",
            (false, true) => "   n dismiss",
            (false, false) => "   /dismiss",
        });
    }
    row
}

/// The prompt, its `keys`, and how many more wait behind it.
fn prompt_rows(prompt: &Prompt, waiting: usize, keys: String) -> Vec<(String, Tone)> {
    let mut rows = vec![(format!("! {}", prompt.title), Tone::Warn)];
    let summary = prompt.summary.iter().flat_map(|summary| summary.lines());
    rows.extend(summary.map(|line| (format!("  {line}"), Tone::Label)));
    if let Answer::Choose { options, .. } = &prompt.answer {
        let numbered = options.iter().zip(1..);
        rows.extend(numbered.map(|(option, n)| (format!("  {n} {option}"), Tone::Plain)));
    }
    rows.push((keys, Tone::Label));
    if waiting > 0 {
        rows.push((format!("  {waiting} more waiting"), Tone::Label));
    }
    rows
}

/// `tab`'s first prompt's rows, with the keys `keys` gives it, wrapped to `width`.
fn wrapped_prompt<F>(tab: &Tab, width: u16, keys: F) -> Vec<(String, Tone)>
where
    F: FnOnce(&Prompt) -> String,
{
    let Some(prompt) = tab.prompts.first() else {
        return Vec::new();
    };
    let shown = prompt_rows(prompt, tab.prompts.len() - 1, keys(prompt));
    shown
        .into_iter()
        .flat_map(|(text, tone)| {
            let wrapped = wrap_lines(&text, usize::from(width));
            wrapped.into_iter().map(move |row| (row, tone))
        })
        .collect()
}

/// Whether the box's text answers `prompt`.
fn typed(prompt: &Prompt) -> bool {
    matches!(prompt.answer, Answer::Reply | Answer::Choose { .. })
}

impl App {
    /// The active tab's conversation, with its first prompt under it, or `working…` while a
    /// turn it sent runs.
    pub fn conversation_rows(&self, width: u16) -> Vec<(String, Tone)> {
        let tab = &self.tabs[self.active];
        let mut rows = tab.wrapped_rows(width);
        if !tab.prompts.is_empty() {
            let set = self.keymap.set();
            rows.extend(wrapped_prompt(tab, width, |prompt| {
                key_row(prompt, set, false)
            }));
        } else if tab.following && !matches!(self.connection, Connection::Lost(_)) {
            rows.push(("working…".to_string(), Tone::Label));
        }
        rows
    }

    /// The sidebar's subagent's first prompt, pinned to the top of `rows`, and the rows below it
    /// and a blank, which its conversation fills. The tab's own prompt is answered first, so
    /// meanwhile it draws no keys.
    pub fn subagent_rows(&self, rows: Rect) -> (Vec<(String, Tone)>, Rect) {
        let tab = &self.tabs[self.active];
        let layer = self
            .layers
            .iter()
            .any(|layer| layer.kind == LayerKind::Sidebar);
        let keys = |prompt: &Prompt| match tab.prompts.is_empty() {
            true => key_row(prompt, self.keymap.set(), layer),
            false => format!("  answer {}'s prompt first", tab.agent),
        };
        let prompt = self
            .subagent
            .as_ref()
            .map_or_else(Vec::new, |shown| wrapped_prompt(shown, rows.width, keys));
        let top = match prompt.len() {
            0 => 0,
            shown => (shown as u16 + 1).min(rows.height),
        };
        let below = Rect {
            y: rows.y + top,
            height: rows.height - top,
            ..rows
        };
        (prompt, below)
    }

    /// The tab whose first prompt the box answers: the active tab, else the subagent the sidebar
    /// shows on screen.
    pub(super) fn prompted(&self) -> Option<&Tab> {
        let tab = &self.tabs[self.active];
        let shown = self.subagent_shown();
        [Some(tab), shown]
            .into_iter()
            .flatten()
            .find(|asking| !asking.prompts.is_empty())
    }

    /// What the empty box sends: an answer to a prompt that takes typed text, else a message.
    pub fn placeholder(&self) -> String {
        let tab = &self.tabs[self.active];
        let answers = self
            .prompted()
            .filter(|asking| asking.prompts.first().is_some_and(typed));
        match answers {
            Some(_) if !tab.prompts.is_empty() => "answer the prompt above".to_string(),
            Some(shown) => format!("answer {}'s question", shown.agent),
            None => format!("Message {}", tab.agent),
        }
    }

    /// What the shell should copy to the terminal's clipboard, once.
    pub fn take_clipboard(&mut self) -> Option<String> {
        self.clipboard.take()
    }

    /// The prompt the keys answer: the box's, while the agent pane has them, or the subagent's,
    /// while the view that shows it has them and the tab asks nothing first.
    pub(super) fn shown_prompt(&self) -> Option<&Prompt> {
        let keys = match self.subagent_has_keys() {
            true => self.tabs[self.active].prompts.is_empty(),
            false => self.layers.is_empty() && self.focus == Pane::Agent,
        };
        self.prompted()?.prompts.first().filter(|_| keys)
    }

    /// Whether a Sidebar layer's keys answer the shown prompt: one that takes no typed text.
    pub(super) fn layer_answers(&self) -> bool {
        self.shown_prompt().is_some_and(|prompt| !typed(prompt))
    }

    /// `enter` accepts in vim's normal mode, and elsewhere from an empty box.
    pub(super) fn enter_accepts(&self) -> bool {
        self.shown_prompt().is_some()
            && match self.keymap.set() {
                KeySet::Vim => self.ctx() == KeyCtx::Normal,
                KeySet::Regular | KeySet::Emacs => {
                    self.ctx() == KeyCtx::Input && self.tabs[self.active].input.is_empty()
                }
            }
    }

    /// The prompt's main answer. There is no "always".
    pub(super) fn accept(&mut self) {
        let Some(prompt) = self.prompted().and_then(|asking| asking.prompts.first()) else {
            return;
        };
        match prompt.answer.clone() {
            Answer::Ack => {
                let item = prompt.item.clone();
                self.notice = Some("acknowledged".to_string());
                self.send_dismiss(item);
            }
            Answer::Document { key, .. } => {
                self.notice = Some(format!("copied the item key {key}"));
                self.clipboard = Some(key);
            }
            Answer::Reply | Answer::Choose { .. } => match self.keymap.set() {
                KeySet::Vim => {
                    self.focus_pane(Pane::Agent);
                    self.mode = Mode::Input;
                }
                KeySet::Regular | KeySet::Emacs => {
                    self.notice = Some("type your reply".to_string());
                }
            },
        }
    }

    pub(super) fn dismiss(&mut self) {
        let Some(prompt) = self.prompted().and_then(|asking| asking.prompts.first()) else {
            self.notice = Some("nothing to dismiss".to_string());
            return;
        };
        if !prompt.dismissable {
            self.notice = Some(MUST_ANSWER.to_string());
            return;
        }
        let item = prompt.item.clone();
        self.send_dismiss(item);
    }

    fn send_dismiss(&mut self, item: String) {
        let Some(session) = self.prompted().and_then(|asking| asking.session.clone()) else {
            return;
        };
        if !self.says_lost() {
            self.emit(Intent::Dismiss { session, item });
        }
    }

    /// The box's text answers a first prompt that takes typed text, unless it is a command.
    pub(super) fn prompt_reply(&self) -> Option<Intent> {
        let asking = self.prompted()?;
        let prompt = asking.prompts.first()?;
        let input = &self.tabs[self.active].input;
        if !typed(prompt) || input.text().starts_with('/') {
            return None;
        }
        Some(Intent::Reply {
            session: asking.session.clone()?,
            agent: asking.agent_id.clone(),
            item: prompt.item.clone(),
            text: input.sent(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::Receiver;

    use ratatui::crossterm::event::Event;

    use crate::app::layers::LayerKind;
    use crate::app::{App, Mode, Pane};
    use crate::keys::KeySet::{self, Emacs, Regular, Vim};
    use crate::sample::{Opens, RunState, Running, Tone};
    use crate::source::{Answer, Intent, Prompt, PromptKind, Update};
    use crate::testing::{
        click_text, connected, drain, find, kinds, numbered, press, right_pane, screen, sent,
        type_text,
    };

    const MUST_ANSWER: &str = "this question needs an answer; it cannot be dismissed";

    fn prompt(item: &str, answer: Answer, dismissable: bool) -> Prompt {
        Prompt {
            item: item.to_string(),
            kind: PromptKind::Ask,
            title: format!("{item} title"),
            summary: None,
            answer,
            dismissable,
        }
    }

    fn choose(options: &[&str], free_text: bool, skip: bool) -> Answer {
        Answer::Choose {
            options: options.iter().map(|option| option.to_string()).collect(),
            free_text,
            skip,
        }
    }

    fn document() -> Answer {
        Answer::Document {
            collection: "Review".to_string(),
            key: "key:1".to_string(),
        }
    }

    /// A connected tab on session `s1` showing `prompts`, out of input mode.
    fn showing(set: KeySet, prompts: Vec<Prompt>) -> (App, Receiver<Intent>) {
        let (mut app, intended) = connected(set, 132, 36);
        app.tabs[0].session = Some("s1".to_string());
        app.tabs[0].lines = numbered(3);
        app.mode = Mode::Normal;
        app.apply(Update::Prompts {
            session: "s1".to_string(),
            prompts,
        });
        (app, intended)
    }

    fn dismissed(item: &str) -> Intent {
        Intent::Dismiss {
            session: "s1".to_string(),
            item: item.to_string(),
        }
    }

    fn prompt_rows(app: &App) -> Vec<(String, Tone)> {
        let shown = app.tabs[app.active].wrapped_rows(80).len();
        app.conversation_rows(80)[shown..].to_vec()
    }

    fn last_row(app: &App) -> Option<(String, Tone)> {
        app.conversation_rows(80).last().cloned()
    }

    fn working() -> Option<(String, Tone)> {
        Some(("working…".to_string(), Tone::Label))
    }

    fn sent_hello() -> (App, Receiver<Intent>) {
        let (mut app, intended) = connected(Vim, 132, 36);
        app.agents = crate::sample::agents();
        type_text(&mut app, "hello");
        press(&mut app, "enter");
        (app, intended)
    }

    #[test]
    fn a_sent_turn_shows_working_until_it_ends() {
        let (mut app, _intended) = sent_hello();
        assert_eq!(
            last_row(&app),
            working(),
            "from enter, before the runtime answers"
        );

        let tab = app.tabs[0].id;
        app.apply(Update::Started {
            tab,
            session: "s1".to_string(),
        });
        app.apply(Update::Turn {
            session: "s1".to_string(),
            turn: "t1".to_string(),
        });
        assert_eq!(last_row(&app), working());

        app.apply(Update::TurnEnded {
            session: "s1".to_string(),
            turn: "t1".to_string(),
        });
        assert_eq!(last_row(&app), None);
    }

    #[test]
    fn a_refused_send_stops_working() {
        let (mut app, _intended) = sent_hello();
        let tab = app.tabs[0].id;
        app.apply(Update::StartFailed {
            tab,
            message: "turn/start: the session is busy".to_string(),
        });
        assert_eq!(last_row(&app), None);
    }

    #[test]
    fn a_prompt_shows_instead_of_working() {
        let (mut app, _intended) = sent_hello();
        let tab = app.tabs[0].id;
        app.apply(Update::Started {
            tab,
            session: "s1".to_string(),
        });
        app.apply(Update::Prompts {
            session: "s1".to_string(),
            prompts: vec![prompt("i1", Answer::Ack, true)],
        });
        let rows = app.conversation_rows(80);
        assert!(!rows.contains(&working().unwrap()), "{rows:?}");
    }

    #[test]
    fn an_ack_prompt_is_dismissed_by_y_or_enter() {
        for keys in ["y", "enter"] {
            let (mut app, intended) = showing(Vim, vec![prompt("i1", Answer::Ack, true)]);
            press(&mut app, keys);
            assert_eq!(sent(&intended), [dismissed("i1")], "{keys}");
            assert_eq!(app.notice.as_deref(), Some("acknowledged"), "{keys}");
        }

        let (mut app, intended) = showing(Emacs, vec![prompt("i1", Answer::Ack, true)]);
        press(&mut app, "enter");
        assert_eq!(sent(&intended), [dismissed("i1")]);
        assert_eq!(app.notice.as_deref(), Some("acknowledged"));
        type_text(&mut app, "hi");
        press(&mut app, "enter");
        assert!(sent(&intended).is_empty(), "only an empty box accepts");
    }

    #[test]
    fn a_question_that_needs_an_answer_cannot_be_dismissed() {
        let question = prompt("q1", choose(&["yes", "no"], false, false), false);
        let (mut app, intended) = showing(Vim, vec![question]);
        press(&mut app, "n");
        assert_eq!(app.notice.as_deref(), Some(MUST_ANSWER));
        press(&mut app, "i");
        type_text(&mut app, "/dismiss");
        press(&mut app, "enter");
        assert_eq!(app.notice.as_deref(), Some(MUST_ANSWER));
        assert!(sent(&intended).is_empty());
        assert_eq!(app.tabs[0].prompts.len(), 1);

        let (mut app, intended) = showing(Vim, vec![prompt("r1", Answer::Reply, true)]);
        press(&mut app, "n i");
        type_text(&mut app, "/dismiss");
        press(&mut app, "enter");
        assert_eq!(sent(&intended), [dismissed("r1"), dismissed("r1")]);
    }

    #[test]
    fn a_typed_reply_goes_to_the_prompt_not_a_turn() {
        for set in [Regular, Vim, Emacs] {
            let question = prompt("q1", choose(&["yes", "no"], true, false), false);
            let (mut app, intended) = showing(set, vec![question]);
            assert!(find(&screen(&app), "answer the prompt above").is_some());
            press(&mut app, "enter");
            match set {
                Vim => assert_eq!(app.mode, Mode::Input),
                Regular | Emacs => assert_eq!(app.notice.as_deref(), Some("type your reply")),
            }
            type_text(&mut app, "1");
            press(&mut app, "enter");
            let reply = Intent::Reply {
                session: "s1".to_string(),
                agent: app.tabs[0].agent_id.clone(),
                item: "q1".to_string(),
                text: "1".to_string(),
            };
            assert_eq!(sent(&intended), [reply], "{set:?}");
            assert!(app.tabs[0].input.is_empty());
        }
    }

    /// `showing(set, prompts)` with subagent `researcher`, on session `c1`, open in the sidebar.
    fn beside(set: KeySet, prompts: Vec<Prompt>) -> (App, Receiver<Intent>) {
        beside_in(set, (132, 36), prompts)
    }

    /// `beside` in a window of `size`.
    fn beside_in(set: KeySet, size: (u16, u16), prompts: Vec<Prompt>) -> (App, Receiver<Intent>) {
        let (mut app, intended) = showing(set, prompts);
        app.handle(Event::Resize(size.0, size.1));
        let researcher = Running {
            name: "researcher".to_string(),
            job: "look".to_string(),
            status: "working".to_string(),
            state: RunState::Working,
            opens: Opens::Agent("c1".to_string()),
        };
        app.apply(Update::Running {
            session: "s1".to_string(),
            rows: vec![researcher],
        });
        click_text(&mut app, "researcher · look");
        drain(&intended);
        (app, intended)
    }

    fn asked(session: &str, prompts: Vec<Prompt>) -> Update {
        Update::Prompts {
            session: session.to_string(),
            prompts,
        }
    }

    fn reply(app: &App, session: &str, item: &str) -> Intent {
        Intent::Reply {
            session: session.to_string(),
            agent: app.tabs[0].agent_id.clone(),
            item: item.to_string(),
            text: "ok".to_string(),
        }
    }

    fn dismissed_in(session: &str, item: &str) -> Intent {
        Intent::Dismiss {
            session: session.to_string(),
            item: item.to_string(),
        }
    }

    /// The question sits at the top of the sidebar. A mailbox reply is not a turn, so the box
    /// answers it on the subagent's session, as it answers the tab's own.
    #[test]
    fn the_box_answers_the_shown_subagents_question() {
        for (set, typing, ack) in [
            (Regular, "", "enter"),
            (Vim, "i", "esc y"),
            (Emacs, "", "enter"),
        ] {
            let (mut app, intended) = beside(set, Vec::new());
            app.apply(Update::Opened {
                session: "c1".to_string(),
                lines: numbered(40),
            });
            app.apply(asked("c1", vec![prompt("q1", Answer::Reply, true)]));
            let pane = right_pane(&app);
            let row = |text| find(&pane, text).map(|(row, _)| row);
            assert_eq!(row("! q1 title"), Some(1), "{set:?}: {pane:#?}");
            assert!(row("message 39").is_some(), "{set:?}: {pane:#?}");
            let s = screen(&app);
            assert!(
                find(&s, "answer researcher's question").is_some(),
                "{set:?}"
            );

            press(&mut app, typing);
            type_text(&mut app, "ok");
            press(&mut app, "enter");
            assert_eq!(sent(&intended), [reply(&app, "c1", "q1")], "{set:?}");

            app.apply(asked("c1", vec![prompt("a1", Answer::Ack, true)]));
            press(&mut app, ack);
            assert_eq!(sent(&intended), [dismissed_in("c1", "a1")], "{set:?}");
        }
    }

    /// The sidebar that shows the question takes its keys too.
    #[test]
    fn the_sidebar_takes_the_keys_of_its_subagents_question() {
        for (set, keys) in [(Regular, "enter"), (Vim, "y"), (Emacs, "enter")] {
            let (mut app, intended) = beside(set, Vec::new());
            app.apply(asked("c1", vec![prompt("a1", Answer::Ack, true)]));
            assert_eq!(app.focus, Pane::Sidebar, "{set:?}");
            press(&mut app, keys);
            assert_eq!(sent(&intended), [dismissed_in("c1", "a1")], "{set:?}");
        }
    }

    #[test]
    fn the_tabs_own_prompt_comes_before_its_subagents() {
        let (mut app, intended) = beside(Vim, vec![prompt("q0", Answer::Reply, true)]);
        app.apply(asked("c1", vec![prompt("q1", Answer::Reply, true)]));
        assert!(find(&screen(&app), "answer the prompt above").is_some());
        press(&mut app, "i");
        type_text(&mut app, "ok");
        press(&mut app, "enter");
        assert_eq!(sent(&intended), [reply(&app, "s1", "q0")]);

        app.apply(asked("s1", Vec::new()));
        assert!(find(&screen(&app), "answer researcher's question").is_some());
        type_text(&mut app, "ok");
        press(&mut app, "enter");
        assert_eq!(sent(&intended), [reply(&app, "c1", "q1")]);
    }

    /// The turn `text` starts on the tab's own session.
    fn sent_to_the_tab(app: &App, text: &str) -> Intent {
        Intent::Send {
            tab: app.tabs[0].id,
            session: Some("s1".to_string()),
            agent: "Engineer".to_string(),
            text: text.to_string(),
        }
    }

    #[test]
    fn the_box_messages_the_parent_once_the_sidebar_moves_on() {
        let (mut app, intended) = beside(Regular, Vec::new());
        app.agents = crate::sample::agents();
        app.apply(asked("c1", vec![prompt("q1", Answer::Reply, true)]));
        type_text(&mut app, "/diff");
        press(&mut app, "enter");
        assert!(find(&screen(&app), "Message Engineer").is_some());
        type_text(&mut app, "hi");
        press(&mut app, "enter");
        assert_eq!(sent(&intended), [sent_to_the_tab(&app, "hi")]);
    }

    /// Hidden, the subagent's prompt is answered by nothing; shown again, it is back.
    #[test]
    fn a_hidden_subagent_leaves_the_box_to_the_parent() {
        for (set, hide, show, ok) in [
            (Regular, "ctrl-r", "ctrl-r", "enter"),
            (Emacs, "ctrl-x ]", "ctrl-x ]", "enter"),
            (Vim, "]", "esc ]", "y"),
            (Vim, "ctrl-w q", "esc ]", "y"),
        ] {
            let (mut app, intended) = beside(set, Vec::new());
            app.agents = crate::sample::agents();
            app.apply(asked("c1", vec![prompt("g1", Answer::Ack, true)]));
            press(&mut app, hide);
            assert!(right_pane(&app).is_empty(), "{set:?} {hide}");
            assert!(find(&screen(&app), "Message Engineer").is_some(), "{hide}");
            press(&mut app, ok);
            assert!(
                sent(&intended).is_empty(),
                "{set:?} {hide}: not answered blind"
            );
            press(&mut app, if set == Vim { "i" } else { "" });
            type_text(&mut app, "hi");
            press(&mut app, "enter");
            assert_eq!(sent(&intended), [sent_to_the_tab(&app, "hi")], "{hide}");

            press(&mut app, show);
            let pane = right_pane(&app);
            assert!(
                find(&pane, "g1 title").is_some(),
                "{set:?} {hide}: {pane:#?}"
            );
        }
    }

    /// A resize that takes the subagent off screen leaves the box to the parent.
    #[test]
    fn a_resize_off_screen_leaves_the_box_to_the_parent() {
        let (mut app, intended) = beside(Regular, Vec::new());
        app.apply(asked("c1", vec![prompt("g1", Answer::Ack, true)]));
        app.handle(Event::Resize(70, 24));
        assert!(app.layers.is_empty());
        press(&mut app, "enter");
        assert!(sent(&intended).is_empty(), "narrowed");

        let (mut app, _) = beside_in(Regular, (70, 24), Vec::new());
        app.apply(asked("c1", vec![prompt("q1", Answer::Reply, true)]));
        app.handle(Event::Resize(132, 36));
        assert!(app.layers.is_empty() && right_pane(&app).is_empty());
        assert!(find(&screen(&app), "Message Engineer").is_some(), "widened");
    }

    /// The layer takes only the keys it draws: enter and `d` for a prompt that needs no text,
    /// none for one that does. Closed, it leaves the box to the parent.
    #[test]
    fn the_narrow_layer_answers_with_the_keys_it_draws() {
        for set in [Regular, Vim, Emacs] {
            let (mut app, intended) = beside_in(set, (70, 24), Vec::new());
            assert_eq!(kinds(&app), [LayerKind::Sidebar], "{set:?}");
            app.apply(asked("c1", vec![prompt("g1", Answer::Ack, true)]));
            let s = screen(&app);
            assert!(
                find(&s, "enter ok   d dismiss").is_some(),
                "{set:?}: {s:#?}"
            );
            press(&mut app, "enter");
            assert_eq!(sent(&intended), [dismissed_in("c1", "g1")], "{set:?}");
            press(&mut app, "d");
            assert_eq!(sent(&intended), [dismissed_in("c1", "g1")], "{set:?}");

            app.apply(asked("c1", vec![prompt("d1", document(), true)]));
            press(&mut app, "enter");
            assert_eq!(app.take_clipboard().as_deref(), Some("key:1"), "{set:?}");

            app.apply(asked("c1", vec![prompt("q1", Answer::Reply, true)]));
            let s = screen(&app);
            assert!(find(&s, "too narrow to answer here").is_some(), "{set:?}");
            assert_eq!(find(&s, "type a reply"), None, "{set:?}");
            assert_eq!(find(&s, "dismiss"), None, "{set:?}");
            press(&mut app, "enter d y");
            assert!(sent(&intended).is_empty(), "{set:?}");

            press(&mut app, if set == Emacs { "ctrl-g" } else { "esc" });
            assert!(app.layers.is_empty(), "{set:?}");
            assert!(find(&screen(&app), "Message Engineer").is_some(), "{set:?}");
            app.apply(asked("c1", vec![prompt("g2", Answer::Ack, true)]));
            press(&mut app, if set == Vim { "y" } else { "enter" });
            assert!(sent(&intended).is_empty(), "{set:?}: not answered blind");
        }
    }

    /// While the tab's own prompt waits, the subagent's draws no keys and none act on it, so
    /// typed text messages the tab as its box says.
    #[test]
    fn the_tabs_own_prompt_silences_the_subagents_keys() {
        for (set, keys) in [(Regular, "enter"), (Vim, "y enter n"), (Emacs, "enter")] {
            let (mut app, intended) = beside(set, vec![prompt("p0", Answer::Ack, true)]);
            app.agents = crate::sample::agents();
            app.apply(asked("c1", vec![prompt("q1", Answer::Reply, true)]));
            let pane = right_pane(&app);
            let first = "answer Engineer's prompt first";
            assert!(find(&pane, first).is_some(), "{set:?}: {pane:#?}");
            assert_eq!(find(&pane, "type a reply"), None, "{set:?}");
            assert!(find(&screen(&app), "Message Engineer").is_some(), "{set:?}");
            assert_eq!(app.focus, Pane::Sidebar, "{set:?}");
            press(&mut app, keys);
            assert!(sent(&intended).is_empty(), "{set:?}");
            press(&mut app, if set == Vim { "i" } else { "" });
            type_text(&mut app, "use option b");
            press(&mut app, "enter");
            let send = sent_to_the_tab(&app, "use option b");
            assert_eq!(sent(&intended), [send], "{set:?}");

            let (mut app, intended) =
                beside_in(set, (70, 24), vec![prompt("p0", Answer::Ack, true)]);
            app.apply(asked("c1", vec![prompt("g1", Answer::Ack, true)]));
            assert!(find(&screen(&app), first).is_some(), "{set:?}");
            press(&mut app, "enter d");
            assert!(sent(&intended).is_empty(), "{set:?}: the layer");
        }
    }

    #[test]
    fn opening_a_tab_again_resets_its_prompts() {
        let (mut app, _) = showing(Vim, vec![prompt("i1", Answer::Ack, true)]);
        app.apply(Update::Opened {
            session: "s1".to_string(),
            lines: numbered(2),
        });
        assert!(
            app.tabs[0].prompts.is_empty(),
            "the source tells them again"
        );
    }

    #[test]
    fn a_documents_key_is_copied() {
        for (set, keys) in [(Vim, "y"), (Vim, "enter"), (Emacs, "enter")] {
            let (mut app, intended) = showing(set, vec![prompt("d1", document(), true)]);
            press(&mut app, keys);
            assert_eq!(app.take_clipboard().as_deref(), Some("key:1"), "{keys}");
            assert_eq!(app.take_clipboard(), None, "once");
            assert_eq!(app.notice.as_deref(), Some("copied the item key key:1"));
            assert!(sent(&intended).is_empty());
        }
    }

    #[test]
    fn a_choice_lists_its_options_and_keys_per_key_set() {
        let choice = Prompt {
            summary: Some("Which channel ships?\nstable ships today".to_string()),
            ..prompt("q1", choose(&["stable", "beta"], true, true), false)
        };
        let (app, _) = showing(Vim, vec![choice.clone(), prompt("q2", Answer::Ack, true)]);
        let row = |text: &str, tone| (text.to_string(), tone);
        assert_eq!(
            prompt_rows(&app),
            [
                row("! q1 title", Tone::Warn),
                row("  Which channel ships?", Tone::Label),
                row("  stable ships today", Tone::Label),
                row("  1 stable", Tone::Plain),
                row("  2 beta", Tone::Plain),
                row(
                    "  type 1-2, or your own words, or skip, enter sends",
                    Tone::Label
                ),
                row("  1 more waiting", Tone::Label),
            ]
        );
        let (app, _) = showing(Emacs, vec![choice]);
        let keys = "  type 1-2, or your own words, or skip, enter sends";
        assert_eq!(
            prompt_rows(&app).last().map(|row| row.0.as_str()),
            Some(keys)
        );

        let pick = choose(&["a", "b", "c"], false, false);
        let table = [
            (Vim, Answer::Ack, "  y ok   n dismiss"),
            (Emacs, Answer::Ack, "  enter ok   /dismiss"),
            (
                Vim,
                Answer::Reply,
                "  type a reply, enter sends   n dismiss",
            ),
            (
                Emacs,
                Answer::Reply,
                "  type a reply, enter sends   /dismiss",
            ),
            (Vim, pick.clone(), "  type 1-3, enter sends   n dismiss"),
            (Emacs, pick, "  type 1-3, enter sends   /dismiss"),
            (
                Vim,
                choose(&[], true, false),
                "  type your answer, enter sends   n dismiss",
            ),
            (
                Vim,
                document(),
                "  answer with a Review document   y copy its key   n dismiss",
            ),
            (
                Emacs,
                document(),
                "  answer with a Review document   enter copies its key   /dismiss",
            ),
        ];
        for (set, answer, want) in table {
            let (app, _) = showing(set, vec![prompt("p1", answer, true)]);
            let rows = prompt_rows(&app);
            assert_eq!(rows.last().map(|row| row.0.as_str()), Some(want), "{set:?}");
        }
    }

    #[test]
    fn a_document_with_no_collection_names_none() {
        let unnamed = Answer::Document {
            collection: String::new(),
            key: "key:1".to_string(),
        };
        let (app, _) = showing(Vim, vec![prompt("d1", unnamed, true)]);
        let rows = prompt_rows(&app);
        let keys = "  answer with a document   y copy its key   n dismiss";
        assert_eq!(rows.last().map(|row| row.0.as_str()), Some(keys));
    }

    #[test]
    fn n_still_moves_a_committed_search() {
        let (mut app, intended) = showing(Vim, vec![prompt("r1", Answer::Reply, true)]);
        press(&mut app, "/");
        type_text(&mut app, "message");
        press(&mut app, "enter");
        let current = |app: &App| app.search.as_ref().map(|search| search.current);
        let before = current(&app);
        press(&mut app, "n");
        assert_ne!(current(&app), before);
        assert!(sent(&intended).is_empty());
        assert_eq!(app.notice, None);
    }

    #[test]
    fn there_is_no_always() {
        let (mut app, intended) = showing(Vim, vec![prompt("g1", Answer::Ack, true)]);
        press(&mut app, "a");
        assert!(sent(&intended).is_empty());
        assert_eq!(app.notice, None);
        assert_eq!(app.tabs[0].prompts.len(), 1);
    }
}
