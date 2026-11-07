use super::fields::{self, Field, Kind};
use super::input::Lines;
use super::layers::LayerKind;
use super::App;
use crate::keys::Action;
use crate::sample::{MenuId, Tone};
use crate::source::{
    Doc, FieldKind, Form, FormAct, FormAction, FormError, FormField, FormRef, Intent, MenuAct,
    Update,
};

const LOADING: &str = "Loading…";
const SAVING: &str = "Saving…";
const STARTING: &str = "Starting…";
const TESTING: &str = "Testing…";

/// A field as a form shows it: the key it saves under, its help, and a save's error on it.
pub struct Entry {
    pub key: String,
    pub help: Option<String>,
    /// Shown only while the field `.0` holds one of `.1`.
    pub when: Option<(String, Vec<String>)>,
    pub error: Option<String>,
    pub field: Field,
}

/// Not `Debug`, as its fields are not.
pub struct FormState {
    pub form: FormRef,
    pub title: String,
    pub note: Vec<String>,
    pub fields: Vec<Entry>,
    pub actions: Vec<FormAction>,
    /// A field's position, or past the fields an action's.
    pub focus: usize,
    pub status: Option<(String, Tone)>,
    /// The source's answer arrived, so there is something to save.
    loaded: bool,
}

impl FormState {
    fn loading(form: FormRef) -> FormState {
        FormState {
            form,
            title: String::new(),
            note: Vec::new(),
            fields: Vec::new(),
            actions: Vec::new(),
            focus: 0,
            status: None,
            loaded: false,
        }
    }

    fn load(&mut self, form: Form) {
        self.title = form.title;
        self.note = form.note;
        self.fields = form.fields.into_iter().map(entry).collect();
        self.actions = form.actions;
        self.status = None;
        self.loaded = true;
        let count = self.fields.len() + self.actions.len();
        let last = count.saturating_sub(1);
        self.focus = fields::next_focus(last, count, true, |index| self.focusable(index));
    }

    pub fn shows(&self, entry: &Entry) -> bool {
        entry.when.as_ref().is_none_or(|(key, values)| {
            let on = self.fields.iter().find(|other| other.key == *key);
            on.is_some_and(|on| values.contains(&on.field.value()))
        })
    }

    fn focusable(&self, index: usize) -> bool {
        match self.fields.get(index) {
            Some(entry) => entry.field.focusable() && self.shows(entry),
            None => index < self.fields.len() + self.actions.len(),
        }
    }

    fn step_focus(&mut self, forward: bool) {
        let count = self.fields.len() + self.actions.len();
        self.focus = fields::next_focus(self.focus, count, forward, |index| self.focusable(index));
    }

    /// None while an action has the focus.
    pub fn focused(&self) -> Option<&Field> {
        self.fields.get(self.focus).map(|entry| &entry.field)
    }

    pub(super) fn field_mut(&mut self) -> Option<&mut Field> {
        self.fields
            .get_mut(self.focus)
            .map(|entry| &mut entry.field)
    }

    fn lines_mut(&mut self) -> Option<&mut Lines> {
        match &mut self.field_mut()?.kind {
            Kind::Lines(lines) => Some(lines),
            _ => None,
        }
    }

    fn action(&self) -> Option<&FormAction> {
        let index = self.focus.checked_sub(self.fields.len())?;
        self.actions.get(index)
    }

    pub fn on_choice(&self) -> bool {
        matches!(
            self.focused().map(|field| &field.kind),
            Some(Kind::Choice(_))
        )
    }

    /// Headings, fixed and hidden fields hold nothing to save.
    fn values(&self) -> Vec<(String, String)> {
        self.fields
            .iter()
            .filter(|entry| entry.field.focusable() && self.shows(entry))
            .map(|entry| (entry.key.clone(), entry.field.value()))
            .collect()
    }

    /// ← and → or Space cycle a choice, which takes no typing.
    fn edit(&mut self, action: Action) {
        let Some(field) = self.field_mut() else {
            return;
        };
        match &mut field.kind {
            Kind::Text => field.input.edit(action),
            Kind::Lines(lines) => lines.edit(action),
            Kind::Choice(choices) => {
                let forward = match action {
                    Action::CursorRight | Action::Insert(' ') => true,
                    Action::CursorLeft => false,
                    _ => return,
                };
                let now = choices
                    .iter()
                    .position(|(value, _)| value == field.input.text());
                let next = fields::cycle(now, choices.len(), forward);
                if let Some((value, _)) = choices.get(next) {
                    field.input.set(value);
                }
            }
            Kind::Fixed | Kind::Heading => {}
        }
    }

    /// A named field that is shown takes the error and the focus; any other error is the status.
    fn failed(&mut self, error: FormError) {
        let named = error.field.as_ref().and_then(|key| {
            let shown = |entry: &Entry| entry.key == *key && self.shows(entry);
            self.fields.iter().position(shown)
        });
        match named {
            Some(index) => {
                self.fields[index].error = Some(error.message);
                self.focus = index;
                self.status = None;
            }
            None => self.status = Some((format!(" {}", error.message), Tone::Warn)),
        }
    }
}

fn entry(field: FormField) -> Entry {
    let kind = match field.kind {
        FieldKind::Text => Kind::Text,
        FieldKind::Lines => Kind::Lines(Lines::new(&field.value)),
        FieldKind::Choice(choices) => Kind::Choice(choices),
        FieldKind::Fixed => Kind::Fixed,
        FieldKind::Heading => Kind::Heading,
    };
    Entry {
        key: field.key,
        help: field.help,
        when: field.when,
        error: None,
        field: Field {
            kind,
            ..Field::new(&field.label, &field.placeholder, &field.value)
        },
    }
}

impl App {
    /// Shows "Loading…" until the source answers with `Update::Form`.
    pub fn open_form(&mut self, form: FormRef) {
        self.form = Some(FormState::loading(form.clone()));
        self.push_layer(LayerKind::Form);
        self.send_form(Intent::OpenForm(form), LOADING);
    }

    /// The desktop's `ConfirmDelete` (`ListDetail.tsx:518-612`), in place of any open form.
    pub(super) fn confirm_delete(&mut self, doc: Doc, id: String, label: String, name: String) {
        let noun = doc.noun();
        let form = FormRef::Delete(doc, id);
        let mut confirm = FormState::loading(form.clone());
        confirm.load(Form {
            form,
            title: format!("Delete {label}?"),
            note: vec![format!(
                "This removes the {noun} for good. To confirm, type its name: {name}"
            )],
            fields: vec![FormField {
                key: "name".to_string(),
                label: String::new(),
                kind: FieldKind::Text,
                value: String::new(),
                placeholder: format!("Type {label} to confirm"),
                help: None,
                when: None,
            }],
            actions: vec![FormAction {
                label: format!("Delete {noun}"),
                act: FormAct::Delete { label, name },
            }],
        });
        self.form = Some(confirm);
        self.push_layer(LayerKind::Form);
    }

    fn close_form(&mut self) {
        self.layers.retain(|layer| layer.kind != LayerKind::Form);
        self.form = None;
    }

    /// A lost source reads no more intents, so the form stays and says why.
    pub(super) fn form_says_lost(&mut self) -> bool {
        let Some(form) = &mut self.form else {
            return false;
        };
        fields::status_says_lost(&self.connection, &mut form.status)
    }

    /// Shows `waiting` before sending, because a sample answers at once.
    fn send_form(&mut self, intent: Intent, waiting: &str) {
        if self.form_says_lost() {
            return;
        }
        if let Some(form) = &mut self.form {
            form.status = Some((format!(" {waiting}"), Tone::Label));
        }
        self.emit(intent);
        // `emit` finds a source gone by sending to it.
        self.form_says_lost();
    }

    /// The keys the Form layer takes before any layer's: Esc is left to close it.
    pub(super) fn act_on_form(&mut self, action: Action) -> bool {
        let Some(form) = &mut self.form else {
            return false;
        };
        match action {
            Action::Complete => form.step_focus(true),
            Action::CompleteBack => form.step_focus(false),
            Action::Next | Action::Prev => {
                let down = action == Action::Next;
                if !form.lines_mut().is_some_and(|lines| lines.step(down)) {
                    form.step_focus(down);
                }
            }
            Action::Save => self.submit_form(),
            Action::Submit => match (form.action().map(|a| a.act.clone()), form.lines_mut()) {
                (Some(act), _) => self.run_form_act(act),
                (None, Some(lines)) => lines.break_line(),
                (None, None) => self.submit_form(),
            },
            _ if action.edits_line() => form.edit(action),
            _ => return false,
        }
        true
    }

    /// Ctrl-s, or Enter on a text or choice field: the confirmation checks the name instead.
    fn submit_form(&mut self) {
        let Some(form) = &mut self.form else {
            return;
        };
        if matches!(form.form, FormRef::Delete(..)) {
            return self.delete_confirmed();
        }
        if !form.loaded {
            return;
        }
        form.fields.iter_mut().for_each(|entry| entry.error = None);
        let intent = Intent::SaveForm {
            form: form.form.clone(),
            values: form.values(),
        };
        self.send_form(intent, SAVING);
    }

    fn run_form_act(&mut self, act: FormAct) {
        let Some(form) = &self.form else {
            return;
        };
        let values = form.values();
        match (form.form.clone(), act) {
            (FormRef::Delete(..), _) => self.delete_confirmed(),
            (FormRef::Edit(doc, id), FormAct::Delete { label, name }) => {
                self.confirm_delete(doc, id, label, name);
            }
            (_, FormAct::Delete { .. }) => {}
            (form, act) => {
                let waiting = match act {
                    FormAct::RunSchedule => STARTING,
                    _ => TESTING,
                };
                self.send_form(Intent::FormAct { form, act, values }, waiting);
            }
        }
    }

    /// The name typed deletes, compared trimmed as the desktop does (`ListDetail.tsx:540`).
    fn delete_confirmed(&mut self) {
        let Some(form) = &self.form else {
            return;
        };
        let FormRef::Delete(doc, id) = form.form.clone() else {
            return;
        };
        let typed = form.fields.first().map(|entry| entry.field.value());
        let asked = form.actions.iter().find_map(|action| match &action.act {
            FormAct::Delete { name, .. } => Some(name.trim()),
            _ => None,
        });
        if typed.as_deref().map(str::trim) != asked || self.form_says_lost() {
            return;
        }
        let intent = Intent::Act {
            menu: doc.menu(),
            act: MenuAct::Delete(doc, id),
        };
        self.close_form();
        self.emit(intent);
    }

    /// An answer for a form no longer shown, or for another form, is dropped.
    pub(super) fn apply_form(&mut self, update: Update) {
        let shown = self
            .layers
            .iter()
            .any(|layer| layer.kind == LayerKind::Form);
        let Some(state) = self.form.as_mut().filter(|_| shown) else {
            return;
        };
        let (Update::Form { form, .. }
        | Update::FormSaved { form, .. }
        | Update::FormNote { form, .. }) = &update
        else {
            return;
        };
        if *form != state.form {
            return;
        }
        match update {
            Update::Form {
                result: Ok(form), ..
            } => state.load(form),
            Update::Form {
                result: Err(why), ..
            } => state.status = Some((format!(" {why}"), Tone::Warn)),
            Update::FormSaved {
                form,
                result: Ok(notice),
            } => {
                self.close_form();
                self.notice = Some(notice);
                match form {
                    FormRef::New(doc) | FormRef::Edit(doc, _) | FormRef::Delete(doc, _) => {
                        self.read_menu(doc.menu());
                    }
                    FormRef::AddFolder => self.read_menu(MenuId::AllowedFolders),
                    FormRef::Node => {}
                }
            }
            Update::FormSaved {
                result: Err(error), ..
            } => state.failed(error),
            Update::FormNote { text, tone, .. } => state.status = Some((format!(" {text}"), tone)),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{channel, Receiver};

    use ratatui::crossterm::event::Event;

    use super::*;
    use crate::draw::theme;
    use crate::keys::KeySet::{self, Emacs, Vim};
    use crate::source::Connection;
    use crate::testing::{buffer, find, kinds, press, screen, type_text};

    fn skill() -> FormRef {
        FormRef::Edit(Doc::Skill, "s1".to_string())
    }

    fn field(key: &str, label: &str, kind: FieldKind, value: &str) -> FormField {
        FormField {
            key: key.to_string(),
            label: label.to_string(),
            kind,
            value: value.to_string(),
            placeholder: String::new(),
            help: None,
            when: None,
        }
    }

    fn text(key: &str, label: &str, value: &str) -> FormField {
        field(key, label, FieldKind::Text, value)
    }

    fn shown_when(field: FormField, key: &str, values: &[&str]) -> FormField {
        let values = values.iter().map(|value| value.to_string()).collect();
        FormField {
            when: Some((key.to_string(), values)),
            ..field
        }
    }

    fn action(label: &str, act: FormAct) -> FormAction {
        FormAction {
            label: label.to_string(),
            act,
        }
    }

    fn form(fields: Vec<FormField>, actions: Vec<FormAction>) -> Form {
        Form {
            form: skill(),
            title: "Code review".to_string(),
            note: Vec::new(),
            fields,
            actions,
        }
    }

    fn skill_form() -> Form {
        form(
            vec![
                text("name", "Name", "Review"),
                text("description", "Description", ""),
            ],
            Vec::new(),
        )
    }

    /// `form` opened and answered at `height`, with what opening it sent drained.
    fn opened_at(set: KeySet, height: u16, form: Form) -> (App, Receiver<Intent>) {
        let (tx, rx) = channel();
        let mut app = App::connected(set, 132, height, tx);
        app.open_form(form.form.clone());
        app.apply(Update::Form {
            form: form.form.clone(),
            result: Ok(form),
        });
        rx.try_iter().for_each(drop);
        (app, rx)
    }

    fn opened(form: Form) -> (App, Receiver<Intent>) {
        opened_at(Vim, 36, form)
    }

    fn sent(rx: &Receiver<Intent>) -> Vec<Intent> {
        rx.try_iter().collect()
    }

    fn values(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    fn save(pairs: &[(&str, &str)]) -> Intent {
        Intent::SaveForm {
            form: skill(),
            values: values(pairs),
        }
    }

    /// A field's row as drawn: its label in the 22-column label column, then its value.
    fn row(label: &str, value: &str) -> String {
        format!(" {label:<22}{value}")
    }

    fn shows(app: &App, text: &str) -> bool {
        find(&screen(app), text).is_some()
    }

    #[test]
    fn a_form_opens_and_esc_closes_it_without_a_write() {
        let (tx, rx) = channel();
        let mut app = App::connected(Vim, 132, 36, tx);
        app.open_form(skill());
        assert_eq!(sent(&rx), [Intent::OpenForm(skill())]);
        assert_eq!(kinds(&app), [LayerKind::Form]);
        assert!(shows(&app, "Loading…"));

        app.apply(Update::Form {
            form: skill(),
            result: Ok(skill_form()),
        });
        assert!(shows(&app, " Code review "));
        assert!(shows(&app, &row("Name", "Review")));
        assert!(shows(&app, &row("Description", "")));
        assert!(shows(&app, " tab next · ctrl-s save · esc cancel"));
        assert!(!shows(&app, "Loading…"));

        press(&mut app, "esc");
        assert_eq!(kinds(&app), []);
        assert_eq!(sent(&rx), []);

        app.open_form(skill());
        app.apply(Update::Form {
            form: skill(),
            result: Err("could not read s1".to_string()),
        });
        let s = screen(&app);
        let (y, x) = find(&s, "could not read s1").expect("the error");
        assert_eq!(buffer(&app)[(x, y)].fg, theme::WARN);
    }

    #[test]
    fn tab_skips_fixed_and_headings_and_wraps() {
        let fields = vec![
            field("general", "General", FieldKind::Heading, ""),
            text("name", "Name", ""),
            field("id", "Skill ID", FieldKind::Fixed, "s1"),
            text("description", "Description", ""),
        ];
        let delete = FormAct::Delete {
            label: "Review".to_string(),
            name: "Review".to_string(),
        };
        let (mut app, rx) = opened(form(fields, vec![action("Delete this skill", delete)]));
        type_text(&mut app, "a");
        press(&mut app, "tab");
        type_text(&mut app, "b");
        press(&mut app, "tab tab");
        type_text(&mut app, "c");
        press(&mut app, "shift-tab shift-tab");
        type_text(&mut app, "d");
        assert!(shows(&app, &row("Name", "ac")));
        assert!(shows(&app, &row("Skill ID", "s1")));
        assert!(shows(&app, &row("Description", "bd")));
        assert_eq!(sent(&rx), []);
    }

    #[test]
    fn typing_edits_the_focused_text_field() {
        let (mut app, rx) = opened(skill_form());
        type_text(&mut app, "djk");
        assert!(shows(&app, &row("Name", "Reviewdjk")));
        press(&mut app, "left bs");
        assert!(shows(&app, &row("Name", "Reviewdk")));
        assert_eq!(kinds(&app), [LayerKind::Form]);
        assert_eq!(sent(&rx), []);
    }

    #[test]
    fn a_lines_field_breaks_and_joins_lines() {
        let fields = vec![
            text("name", "Name", ""),
            field("body", "Body", FieldKind::Lines, ""),
        ];
        let (mut app, rx) = opened(form(fields, Vec::new()));
        press(&mut app, "tab");
        type_text(&mut app, "a");
        press(&mut app, "enter");
        type_text(&mut app, "b");
        assert!(shows(&app, &row("Body", "a")));
        assert!(shows(&app, &row("", "b")));
        assert_eq!(sent(&rx), []);

        press(&mut app, "home bs");
        assert!(shows(&app, &row("Body", "ab")));
        assert!(!shows(&app, &row("", "b")));

        press(&mut app, "enter ctrl-s");
        assert_eq!(sent(&rx), [save(&[("name", ""), ("body", "a\nb")])]);
    }

    fn schedule_fields() -> Vec<FormField> {
        let cadence = FieldKind::Choice(vec![
            ("interval".to_string(), "Interval".to_string()),
            ("cron".to_string(), "Cron".to_string()),
        ]);
        vec![
            field("cadence", "Cadence", cadence, "cron"),
            shown_when(
                text("interval", "Interval seconds", ""),
                "cadence",
                &["interval"],
            ),
            shown_when(text("expression", "Cron", ""), "cadence", &["cron"]),
            shown_when(text("timezone", "Timezone", "UTC"), "cadence", &["cron"]),
        ]
    }

    #[test]
    fn a_choice_cycles_and_hides_what_it_does_not_select() {
        let (mut app, rx) = opened(form(schedule_fields(), Vec::new()));
        assert!(shows(&app, &row("Cadence", "Cron")));
        assert!(shows(&app, &row("Cron", "")));
        assert!(shows(&app, &row("Timezone", "UTC")));
        assert!(!shows(&app, "Interval seconds"));
        assert!(shows(
            &app,
            " tab next · ctrl-s save · esc cancel · ←→ choose"
        ));

        press(&mut app, "right");
        type_text(&mut app, "x");
        assert!(shows(&app, &row("Cadence", "Interval")));
        assert!(shows(&app, &row("Interval seconds", "")));
        assert!(!shows(&app, "Cron"));
        assert!(!shows(&app, "Timezone"));

        press(&mut app, "tab");
        assert!(!shows(&app, "←→ choose"));
        press(&mut app, "shift-tab");
        type_text(&mut app, " ");
        assert!(shows(&app, &row("Cadence", "Cron")));
        press(&mut app, "left ctrl-s");
        assert_eq!(
            sent(&rx),
            [save(&[("cadence", "interval"), ("interval", "")])]
        );
    }

    #[test]
    fn save_sends_only_visible_values_in_order() {
        let mode = FieldKind::Choice(vec![
            ("x".to_string(), "X".to_string()),
            ("y".to_string(), "Y".to_string()),
        ]);
        let fields = vec![
            field("general", "General", FieldKind::Heading, ""),
            text("a", "A", "1"),
            field("id", "ID", FieldKind::Fixed, "s1"),
            field("mode", "Mode", mode, "x"),
            shown_when(text("b", "B", "2"), "mode", &["y"]),
            field("c", "C", FieldKind::Lines, "3\n4"),
        ];
        let (mut app, rx) = opened(form(fields, Vec::new()));
        let saved = save(&[("a", "1"), ("mode", "x"), ("c", "3\n4")]);
        press(&mut app, "enter");
        assert_eq!(sent(&rx), std::slice::from_ref(&saved));
        assert!(shows(&app, "Saving…"));
        press(&mut app, "tab tab ctrl-s");
        assert_eq!(sent(&rx), [saved]);
    }

    #[test]
    fn a_save_error_lands_under_its_field_and_focuses_it() {
        let (mut app, rx) = opened(skill_form());
        press(&mut app, "ctrl-s");
        assert_eq!(
            sent(&rx),
            [save(&[("name", "Review"), ("description", "")])]
        );
        app.apply(Update::FormSaved {
            form: skill(),
            result: Err(FormError {
                field: Some("description".to_string()),
                message: "Description is required".to_string(),
            }),
        });
        assert!(shows(&app, &row("", "Description is required")));
        assert!(!shows(&app, "Saving…"));
        type_text(&mut app, "x");
        assert!(shows(&app, &row("Description", "x")));
        assert!(shows(&app, &row("Name", "Review")));

        app.apply(Update::FormSaved {
            form: skill(),
            result: Err(FormError {
                field: None,
                message: "could not write".to_string(),
            }),
        });
        assert!(shows(&app, " could not write"));
        assert_eq!(kinds(&app), [LayerKind::Form]);
    }

    #[test]
    fn a_saved_form_closes_and_rereads_its_list() {
        let (tx, rx) = channel();
        let mut app = App::connected(Vim, 132, 36, tx);
        app.push_layer(LayerKind::Menu(MenuId::Skills));
        app.open_form(skill());
        app.apply(Update::Form {
            form: skill(),
            result: Ok(skill_form()),
        });
        rx.try_iter().for_each(drop);
        app.apply(Update::FormSaved {
            form: skill(),
            result: Ok("Saved".to_string()),
        });
        assert_eq!(kinds(&app), [LayerKind::Menu(MenuId::Skills)]);
        assert_eq!(app.notice.as_deref(), Some("Saved"));
        assert_eq!(sent(&rx), [Intent::ReadMenu(MenuId::Skills)]);
    }

    #[test]
    fn an_answer_for_another_form_is_ignored() {
        let (mut app, rx) = opened(skill_form());
        let other = FormRef::Edit(Doc::Skill, "other".to_string());
        let mut theirs = form(vec![text("name", "Other name", "x")], Vec::new());
        theirs.form = other.clone();
        app.apply(Update::Form {
            form: other.clone(),
            result: Ok(theirs),
        });
        app.apply(Update::FormSaved {
            form: other.clone(),
            result: Ok("Saved".to_string()),
        });
        app.apply(Update::FormNote {
            form: other,
            text: "Schedule started".to_string(),
            tone: Tone::Plain,
        });
        assert_eq!(kinds(&app), [LayerKind::Form]);
        assert_eq!(app.notice, None);
        assert_eq!(sent(&rx), []);
        let s = screen(&app);
        assert!(find(&s, &row("Name", "Review")).is_some());
        assert_eq!(find(&s, "Other name"), None);
        assert_eq!(find(&s, "Schedule started"), None);
    }

    #[test]
    fn delete_asks_for_the_name_and_sends_only_when_it_matches() {
        let delete = FormAct::Delete {
            label: "Code review".to_string(),
            name: "Review".to_string(),
        };
        let (mut app, rx) = opened(form(
            vec![text("name", "Name", "Review")],
            vec![action("Delete this skill", delete)],
        ));
        press(&mut app, "tab enter");
        assert_eq!(kinds(&app), [LayerKind::Form]);
        let s = screen(&app);
        assert!(find(&s, " Delete Code review? ").is_some());
        let note = "This removes the skill for good. To confirm, type its name: Review";
        assert!(find(&s, note).is_some());
        assert!(find(&s, " Delete skill").is_some());
        assert!(find(&s, &row("", "Type Code review to confirm")).is_some());
        assert!(find(&s, " enter delete · esc cancel").is_some());
        assert_eq!(find(&s, "ctrl-s save"), None);
        assert_eq!(find(&s, "Delete this skill"), None);

        type_text(&mut app, "Revie");
        press(&mut app, "enter tab enter ctrl-s");
        assert_eq!(sent(&rx), []);
        assert_eq!(kinds(&app), [LayerKind::Form]);

        press(&mut app, "shift-tab");
        type_text(&mut app, "w");
        press(&mut app, "enter");
        let act = Intent::Act {
            menu: MenuId::Skills,
            act: MenuAct::Delete(Doc::Skill, "s1".to_string()),
        };
        assert_eq!(sent(&rx), std::slice::from_ref(&act));
        assert_eq!(kinds(&app), []);

        let (label, name) = ("Code review".to_string(), "Review".to_string());
        app.confirm_delete(Doc::Skill, "s1".to_string(), label, name);
        type_text(&mut app, " Review ");
        press(&mut app, "enter");
        assert_eq!(sent(&rx), [act]);
        assert_eq!(kinds(&app), []);
    }

    #[test]
    fn a_lost_source_keeps_the_form_and_says_why() {
        let gone = " could not reach the runtime: gone";
        let (mut app, rx) = opened(skill_form());
        press(&mut app, "ctrl-s");
        rx.try_iter().for_each(drop);
        app.apply(Update::Connection(Connection::Lost("gone".to_string())));
        assert!(shows(&app, gone));
        assert!(!shows(&app, "Saving…"));

        press(&mut app, "ctrl-s");
        assert_eq!(sent(&rx), []);
        assert!(shows(&app, gone));
        assert_eq!(kinds(&app), [LayerKind::Form]);

        let (label, name) = ("Code review".to_string(), "Review".to_string());
        app.confirm_delete(Doc::Skill, "s1".to_string(), label, name);
        assert!(!shows(&app, gone));
        type_text(&mut app, "Review");
        press(&mut app, "enter");
        assert_eq!(sent(&rx), []);
        assert_eq!(kinds(&app), [LayerKind::Form]);
        assert!(shows(&app, gone));
        assert_eq!(app.notice, None);
    }

    #[test]
    fn run_and_test_answers_show_in_the_form() {
        let schedule = FormRef::Edit(Doc::Schedule, "nightly".to_string());
        let actions = vec![
            action("Run schedule now", FormAct::RunSchedule),
            action("Test connection", FormAct::TestConnection),
        ];
        let at_schedule = Form {
            form: schedule.clone(),
            ..form(vec![text("name", "Name", "Nightly")], actions)
        };
        let (mut app, rx) = opened(at_schedule);
        press(&mut app, "tab enter");
        let run = Intent::FormAct {
            form: schedule.clone(),
            act: FormAct::RunSchedule,
            values: values(&[("name", "Nightly")]),
        };
        assert_eq!(sent(&rx), [run]);
        assert!(shows(&app, "Starting…"));

        app.apply(Update::FormNote {
            form: schedule.clone(),
            text: "Schedule started · r1".to_string(),
            tone: Tone::Plain,
        });
        assert!(shows(&app, "Schedule started · r1"));
        assert!(!shows(&app, "Starting…"));

        press(&mut app, "down enter");
        let test = Intent::FormAct {
            form: schedule,
            act: FormAct::TestConnection,
            values: values(&[("name", "Nightly")]),
        };
        assert_eq!(sent(&rx), [test]);
        assert!(shows(&app, "Testing…"));
    }

    #[test]
    fn a_pasted_block_keeps_its_lines_only_in_a_lines_field() {
        let fields = vec![
            text("name", "Name", ""),
            field("body", "Body", FieldKind::Lines, ""),
        ];
        let (mut app, rx) = opened(form(fields, Vec::new()));
        app.handle(Event::Paste("a\nb".to_string()));
        press(&mut app, "tab");
        app.handle(Event::Paste("a\nb".to_string()));
        press(&mut app, "ctrl-s");
        assert_eq!(sent(&rx), [save(&[("name", "a b"), ("body", "a\nb")])]);
    }

    #[test]
    fn a_long_form_scrolls_to_the_focused_field() {
        let fields = (0..20)
            .map(|n| text(&format!("f{n}"), &format!("Field {n}"), &format!("v{n}")))
            .collect();
        let (mut app, _rx) = opened_at(Vim, 24, form(fields, Vec::new()));
        assert!(shows(&app, &row("Field 0", "v0")));
        assert!(!shows(&app, &row("Field 19", "v19")));
        for _ in 0..19 {
            press(&mut app, "tab");
        }
        assert!(shows(&app, &row("Field 19", "v19")));
        assert!(!shows(&app, &row("Field 0", "v0")));
        press(&mut app, "tab");
        assert!(shows(&app, &row("Field 0", "v0")));
    }

    #[test]
    fn forms_work_with_emacs_keys() {
        let (mut app, rx) = opened_at(Emacs, 36, skill_form());
        assert!(shows(&app, " tab next · ctrl-s save · ctrl-g cancel"));
        type_text(&mut app, "s");
        press(&mut app, "tab");
        type_text(&mut app, "djk");
        press(&mut app, "shift-tab ctrl-a");
        type_text(&mut app, "A ");
        press(&mut app, "ctrl-s");
        assert_eq!(
            sent(&rx),
            [save(&[("name", "A Reviews"), ("description", "djk")])]
        );
        press(&mut app, "ctrl-g");
        assert_eq!(kinds(&app), []);
        assert_eq!(sent(&rx), []);
    }
}
