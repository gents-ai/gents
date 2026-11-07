use super::{wrap_lines, App, Mode, Pane};
use crate::keys::{KeyCtx, KeySet};
use crate::sample::Tone;
use crate::source::{Answer, Connection, Intent, Prompt};

const MUST_ANSWER: &str = "this question needs an answer; it cannot be dismissed";

/// How `prompt` is answered with `set`'s keys.
fn key_row(prompt: &Prompt, set: KeySet) -> String {
    let vim = set == KeySet::Vim;
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
        row.push_str(if vim { "   n dismiss" } else { "   /dismiss" });
    }
    row
}

/// The prompt, its keys, and how many more wait behind it.
fn prompt_rows(prompt: &Prompt, waiting: usize, set: KeySet) -> Vec<(String, Tone)> {
    let mut rows = vec![(format!("! {}", prompt.title), Tone::Warn)];
    let summary = prompt.summary.iter().flat_map(|summary| summary.lines());
    rows.extend(summary.map(|line| (format!("  {line}"), Tone::Label)));
    if let Answer::Choose { options, .. } = &prompt.answer {
        let numbered = options.iter().zip(1..);
        rows.extend(numbered.map(|(option, n)| (format!("  {n} {option}"), Tone::Plain)));
    }
    rows.push((key_row(prompt, set), Tone::Label));
    if waiting > 0 {
        rows.push((format!("  {waiting} more waiting"), Tone::Label));
    }
    rows
}

impl App {
    /// The active tab's conversation, with its first prompt under it, or `working…` while a
    /// turn it sent runs.
    pub fn conversation_rows(&self, width: u16) -> Vec<(String, Tone)> {
        let tab = &self.tabs[self.active];
        let mut rows = tab.wrapped_rows(width);
        if let Some(prompt) = tab.prompts.first() {
            let shown = prompt_rows(prompt, tab.prompts.len() - 1, self.keymap.set());
            rows.extend(shown.into_iter().flat_map(|(text, tone)| {
                let wrapped = wrap_lines(&text, usize::from(width));
                wrapped.into_iter().map(move |row| (row, tone))
            }));
        } else if tab.following && !matches!(self.connection, Connection::Lost(_)) {
            rows.push(("working…".to_string(), Tone::Label));
        }
        rows
    }

    /// What the shell should copy to the terminal's clipboard, once.
    pub fn take_clipboard(&mut self) -> Option<String> {
        self.clipboard.take()
    }

    /// The prompt the agent pane shows, while that pane has the keys.
    pub(super) fn shown_prompt(&self) -> Option<&Prompt> {
        let first = self.tabs[self.active].prompts.first();
        first.filter(|_| self.focus == Pane::Agent)
    }

    /// `enter` accepts in vim's normal mode, and in emacs from an empty box.
    pub(super) fn enter_accepts(&self) -> bool {
        self.shown_prompt().is_some()
            && match self.keymap.set() {
                KeySet::Vim => self.ctx() == KeyCtx::Normal,
                KeySet::Emacs => {
                    self.ctx() == KeyCtx::Input && self.tabs[self.active].input.is_empty()
                }
            }
    }

    /// The prompt's main answer. There is no "always".
    pub(super) fn accept(&mut self) {
        let Some(prompt) = self.tabs[self.active].prompts.first() else {
            return;
        };
        match &prompt.answer {
            Answer::Ack => {
                let item = prompt.item.clone();
                self.notice = Some("acknowledged".to_string());
                self.send_dismiss(item);
            }
            Answer::Document { key, .. } => {
                self.notice = Some(format!("copied the item key {key}"));
                self.clipboard = Some(key.clone());
            }
            Answer::Reply | Answer::Choose { .. } => match self.keymap.set() {
                KeySet::Vim => self.mode = Mode::Input,
                KeySet::Emacs => self.notice = Some("type your reply".to_string()),
            },
        }
    }

    pub(super) fn dismiss(&mut self) {
        let Some(prompt) = self.tabs[self.active].prompts.first() else {
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
        let Some(session) = self.tabs[self.active].session.clone() else {
            return;
        };
        if !self.says_lost() {
            self.emit(Intent::Dismiss { session, item });
        }
    }

    /// The box's text answers a first prompt that takes typed text, unless it is a command.
    pub(super) fn prompt_reply(&self) -> Option<Intent> {
        let tab = &self.tabs[self.active];
        let prompt = tab.prompts.first()?;
        let typed = matches!(prompt.answer, Answer::Reply | Answer::Choose { .. });
        let text = tab.input.text();
        if !typed || text.starts_with('/') {
            return None;
        }
        Some(Intent::Reply {
            session: tab.session.clone()?,
            agent: tab.agent_id.clone(),
            item: prompt.item.clone(),
            text: tab.input.sent(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{channel, Receiver};

    use crate::app::{App, Mode, Tab};
    use crate::keys::KeySet::{self, Emacs, Vim};
    use crate::sample::Tone;
    use crate::source::{Answer, Intent, Prompt, PromptKind, Update};
    use crate::testing::{find, numbered, press, screen, type_text};

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
        let (intents, intended) = channel();
        let mut app = App::connected(set, 132, 36, intents);
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

    fn sent(intended: &Receiver<Intent>) -> Vec<Intent> {
        intended.try_iter().collect()
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
        let (intents, intended) = channel();
        let mut app = App::connected(Vim, 132, 36, intents);
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
        for set in [Vim, Emacs] {
            let question = prompt("q1", choose(&["yes", "no"], true, false), false);
            let (mut app, intended) = showing(set, vec![question]);
            assert!(find(&screen(&app), "answer the prompt above").is_some());
            press(&mut app, "enter");
            match set {
                Vim => assert_eq!(app.mode, Mode::Input),
                Emacs => assert_eq!(app.notice.as_deref(), Some("type your reply")),
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

    /// A subagent's question lands on its own session. Answering it is a mailbox reply, not a
    /// turn, so the read-only subagent tab takes it.
    #[test]
    fn a_subagents_question_is_answered_in_its_tab() {
        let (mut app, intended) = showing(Vim, vec![prompt("r1", Answer::Reply, true)]);
        app.tabs[0].parent = Some(Box::new(Tab::new_session()));
        assert!(find(&screen(&app), "answer the prompt above").is_some());
        press(&mut app, "i");
        type_text(&mut app, "ok");
        press(&mut app, "enter");
        let reply = Intent::Reply {
            session: "s1".to_string(),
            agent: app.tabs[0].agent_id.clone(),
            item: "r1".to_string(),
            text: "ok".to_string(),
        };
        assert_eq!(sent(&intended), [reply]);

        let (mut app, intended) = showing(Vim, Vec::new());
        app.tabs[0].parent = Some(Box::new(Tab::new_session()));
        press(&mut app, "i");
        type_text(&mut app, "ok");
        press(&mut app, "enter");
        assert!(
            sent(&intended).is_empty(),
            "with no prompt it stays read-only"
        );
        assert_eq!(app.tabs[0].input.text(), "ok");
        assert_eq!(
            app.notice.as_deref(),
            Some("a subagent takes input from its parent")
        );
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
