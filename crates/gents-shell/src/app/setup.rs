use super::fields::{self, Field, Kind};
use super::layers::LayerKind;
use super::{stepped, App};
use crate::filter::subsequence;
use crate::keys::Action;
use crate::sample::{MenuId, Tone};
use crate::source::{
    Connection, Discovered, Intent, SavedAccount, Secret, SettingKey, SetupAuth, SetupConnection,
    SetupMethod, SetupModel, SetupProvider, SetupSetting, Update,
};

const READING: &str = " reading…";
const FINDING: &str = " finding models…";
const SAVING: &str = " saving…";
const CANCELLED: &str = " Sign-in cancelled.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Provider,
    Method,
    Connect,
    SigningIn,
    Models,
    Settings,
    Saving,
}

/// Not `Debug`, as its fields are not.
pub struct Setup {
    pub step: Step,
    pub providers: Vec<SetupProvider>,
    /// The selected provider, which every later step is for.
    pub provider: usize,
    /// The selected Method row, as a position in `choices`.
    pub choice: usize,
    pub fields: Vec<Field>,
    pub focus: usize,
    /// The row under the fields: what the step waits on, or what went wrong.
    pub status: Option<(String, Tone)>,
    pub url: Option<String>,
    /// What `Discover` was last sent with.
    pub connection: Option<SetupConnection>,
    /// The Models step's answer, none until it arrives.
    pub found: Option<Discovered>,
    /// The Models step's search, or the model id typed when none was advertised.
    pub query: Field,
    /// The selected model, as a position in `matches`.
    pub model: usize,
    /// The model the Settings step is for, and a field per setting in its order.
    pub chosen: Option<SetupModel>,
    pub settings: Vec<Field>,
    pub setting: usize,
}

impl Setup {
    fn new() -> Setup {
        Setup {
            step: Step::Provider,
            providers: Vec::new(),
            provider: 0,
            choice: 0,
            fields: Vec::new(),
            focus: 0,
            status: None,
            url: None,
            connection: None,
            found: None,
            query: Field::new("", "", ""),
            model: 0,
            chosen: None,
            settings: Vec::new(),
            setting: 0,
        }
    }

    /// Advertised models search; with none, a `manual` answer takes a typed id.
    pub fn shows_query(&self) -> bool {
        self.found
            .as_ref()
            .is_some_and(|found| !found.models.is_empty() || found.manual)
    }

    pub fn matches(&self) -> Vec<&SetupModel> {
        let query = self.query.input.text();
        self.found
            .iter()
            .flat_map(|found| &found.models)
            .filter(|model| subsequence(query, &model.id) || subsequence(query, &model.name))
            .collect()
    }

    fn found(&mut self, found: Discovered) {
        self.query = if found.models.is_empty() {
            Field::new("Model id", "Exact served model ID", "")
        } else {
            Field::new("Search", "Search advertised models", "")
        };
        self.model = 0;
        self.status = found
            .failure
            .as_ref()
            .map(|why| (format!(" {why}"), Tone::Warn));
        self.found = Some(found);
    }

    fn choose(&mut self, model: SetupModel) {
        let field = |setting: &SetupSetting| {
            let field = Field::new(setting.key.label(), "", &setting.value);
            if setting.key != SettingKey::ReasoningEffort {
                return field;
            }
            let choices = setting.choices.iter().map(|c| (c.clone(), c.clone()));
            Field {
                kind: Kind::Choice(choices.collect()),
                ..field
            }
        };
        self.settings = model.settings.iter().map(field).collect();
        self.setting = 0;
        self.chosen = Some(model);
        self.go(Step::Settings);
    }

    pub fn chosen_provider(&self) -> Option<&SetupProvider> {
        self.providers.get(self.provider)
    }

    /// The Method step's rows: each method, and after a sign-in its saved accounts.
    pub fn choices(&self) -> Vec<(&SetupMethod, Option<&SavedAccount>)> {
        let Some(provider) = self.chosen_provider() else {
            return Vec::new();
        };
        provider
            .methods
            .iter()
            .flat_map(|method| {
                let saved = method
                    .accounts
                    .iter()
                    .map(move |account| (method, Some(account)));
                std::iter::once((method, None)).chain(saved)
            })
            .collect()
    }

    pub fn choice(&self) -> Option<(&SetupMethod, Option<&SavedAccount>)> {
        self.choices().get(self.choice).copied()
    }

    /// The method's new account, which the Method step then offers and whose label is taken.
    /// Its row follows the method's own, so `choice` still selects that.
    fn signed_in(&mut self, account: &SavedAccount) {
        let Some(id) = self.choice().map(|(method, _)| method.id.clone()) else {
            return;
        };
        let provider = self.providers.get_mut(self.provider);
        let method = provider.and_then(|provider| provider.methods.iter_mut().find(|m| m.id == id));
        if let Some(method) = method.filter(|method| !method.accounts.contains(account)) {
            method.accounts.push(account.clone());
        }
    }

    /// One method and no saved account offer one row, so the Method step is passed by.
    fn skips_method(&self) -> bool {
        self.choices().len() == 1
    }

    fn go(&mut self, step: Step) {
        self.step = step;
        self.status = None;
    }

    fn connect(&mut self) {
        let Some((method, _)) = self.choice() else {
            return;
        };
        let key = match method.auth {
            SetupAuth::SignIn => None,
            SetupAuth::Key => Some("API key"),
            SetupAuth::OptionalKey => Some("API key (optional)"),
        };
        self.fields = match key {
            None => vec![Field::new("Label", "Optional, e.g. Work", "")],
            Some(key) => vec![
                Field {
                    masked: true,
                    ..Field::new(key, "Stored only when you save", "")
                },
                Field {
                    kind: if method.endpoint_fixed {
                        Kind::Fixed
                    } else {
                        Kind::Text
                    },
                    ..Field::new("Endpoint", "", &method.endpoint)
                },
            ],
        };
        self.focus = 0;
        self.go(Step::Connect);
    }

    /// A key method connects with its fields; a sign-in with `signed_in` or the saved account.
    fn connection(&self, signed_in: Option<&SavedAccount>) -> Option<SetupConnection> {
        let provider = self.chosen_provider()?;
        let (method, saved) = self.choice()?;
        let (endpoint, key) = match (method.auth, &self.fields[..]) {
            (SetupAuth::Key | SetupAuth::OptionalKey, [key, endpoint]) => {
                let key = key.input.text();
                let key = (!key.is_empty()).then(|| Secret(key.to_string()));
                (endpoint.input.text().to_string(), key)
            }
            _ => (method.endpoint.clone(), None),
        };
        Some(SetupConnection {
            provider: provider.id.clone(),
            method: method.id.clone(),
            endpoint,
            key,
            account: signed_in
                .or(saved)
                .map(|account| account.account_ref.clone()),
        })
    }

    /// The field the keys edit. A changed search selects its first match again.
    pub(super) fn field_mut(&mut self) -> Option<&mut Field> {
        match self.step {
            Step::Connect => self.fields.get_mut(self.focus),
            Step::Models if self.shows_query() => {
                self.model = 0;
                Some(&mut self.query)
            }
            Step::Settings => self.settings.get_mut(self.setting),
            _ => None,
        }
    }

    /// Focus passes a fixed field by, so it never rests on one: the first field is never fixed.
    fn next_field(&mut self) {
        let (fields, focus) = match self.step {
            Step::Connect => (&self.fields, &mut self.focus),
            Step::Settings => (&self.settings, &mut self.setting),
            _ => return,
        };
        *focus = fields::next_focus(*focus, fields.len(), true, |index| {
            fields[index].focusable()
        });
    }

    pub(super) fn step_row(&mut self, forward: bool) {
        let count = match self.step {
            Step::Provider => self.providers.len(),
            Step::Method => self.choices().len(),
            Step::Models => self.matches().len(),
            _ => return,
        };
        let selected = match self.step {
            Step::Provider => &mut self.provider,
            Step::Method => &mut self.choice,
            _ => &mut self.model,
        };
        *selected = stepped(*selected, forward, count.saturating_sub(1));
    }
}

impl App {
    /// A layer closed while its sign-in waited, by any way but Esc, sent no cancel.
    pub(super) fn open_setup(&mut self) {
        let waiting = self.setup.as_ref().map(|setup| setup.step) == Some(Step::SigningIn);
        if waiting && !matches!(self.connection, Connection::Lost(_)) {
            self.emit(Intent::CancelSignIn);
        }
        self.setup = Some(Setup::new());
        self.send_setup(Step::Provider, Intent::ReadSetup);
    }

    /// A lost source reads no more intents, so the step stays and says why.
    pub(super) fn setup_says_lost(&mut self) -> bool {
        let Some(setup) = &mut self.setup else {
            return false;
        };
        fields::status_says_lost(&self.connection, &mut setup.status)
    }

    /// Enters `step` before sending, because a sample answers at once.
    fn send_setup(&mut self, step: Step, intent: Intent) {
        if self.setup_says_lost() {
            return;
        }
        let Some(setup) = &mut self.setup else {
            return;
        };
        setup.go(step);
        let status = match intent {
            Intent::ReadSetup => Some(READING),
            Intent::Discover(_) => {
                setup.found = None;
                Some(FINDING)
            }
            Intent::SaveBackend { .. } => Some(SAVING),
            // The source tells no answer for a sign-in it was told to cancel.
            Intent::CancelSignIn => Some(CANCELLED),
            _ => None,
        };
        setup.status = status.map(|text| (text.to_string(), Tone::Label));
        self.emit(intent);
        // `emit` finds a source gone by sending to it.
        self.setup_says_lost();
    }

    fn discover(&mut self, signed_in: Option<&SavedAccount>) {
        let Some(setup) = &mut self.setup else {
            return;
        };
        let Some(connection) = setup.connection(signed_in) else {
            return;
        };
        setup.connection = Some(connection.clone());
        self.send_setup(Step::Models, Intent::Discover(connection));
    }

    /// The keys the Setup layer takes before any layer's: its fields' edits, tab and back.
    pub(super) fn act_on_setup(&mut self, action: Action) -> bool {
        let Some(setup) = &mut self.setup else {
            return false;
        };
        match action {
            _ if action.edits_line() => {
                if let Some(field) = setup.field_mut() {
                    field.edit(action);
                }
            }
            Action::Complete => setup.next_field(),
            Action::Back => self.back_setup(),
            _ => return false,
        }
        true
    }

    pub(super) fn submit_setup(&mut self) {
        let Some(setup) = &mut self.setup else {
            return;
        };
        match setup.step {
            Step::Provider if setup.chosen_provider().is_some() => {
                setup.choice = 0;
                if setup.skips_method() {
                    setup.connect();
                } else {
                    setup.go(Step::Method);
                }
            }
            Step::Method => match setup.choice().map(|(_, saved)| saved.is_some()) {
                Some(true) => self.discover(None),
                Some(false) => setup.connect(),
                None => {}
            },
            Step::Connect => self.connect_setup(),
            Step::Models => self.submit_models(),
            Step::Settings => self.save_setup(),
            _ => {}
        }
    }

    /// A typed id is recommended settings, a failure is found again, and a match is chosen.
    fn submit_models(&mut self) {
        let Some(setup) = &mut self.setup else {
            return;
        };
        let (Some(found), Some(connection)) = (&setup.found, &setup.connection) else {
            return;
        };
        let model = setup.query.input.text().trim().to_string();
        let intent = if found.models.is_empty() && found.manual {
            if model.is_empty() {
                return;
            }
            let connection = connection.clone();
            Intent::Recommend { connection, model }
        } else if found.failure.is_some() {
            Intent::Discover(connection.clone())
        } else {
            let chosen = setup.matches().get(setup.model).map(|&model| model.clone());
            if let Some(chosen) = chosen {
                setup.choose(chosen);
            }
            return;
        };
        self.send_setup(Step::Models, intent);
    }

    fn save_setup(&mut self) {
        let Some(setup) = &mut self.setup else {
            return;
        };
        let (Some(model), Some(connection)) = (&setup.chosen, &setup.connection) else {
            return;
        };
        let settings: Vec<(SettingKey, String)> = model
            .settings
            .iter()
            .zip(&setup.settings)
            .map(|(setting, field)| (setting.key, field.input.text().trim().to_string()))
            .collect();
        if let Some(why) = invalid(&model.settings, &settings) {
            setup.status = Some((format!(" {why}"), Tone::Warn));
            return;
        }
        let intent = Intent::SaveBackend {
            connection: connection.clone(),
            model: model.id.clone(),
            settings,
        };
        self.send_setup(Step::Saving, intent);
    }

    fn connect_setup(&mut self) {
        let Some(setup) = &mut self.setup else {
            return;
        };
        let (Some(provider), Some((method, _)), Some(field)) = (
            setup.chosen_provider(),
            setup.choice(),
            setup.fields.first(),
        ) else {
            return;
        };
        if method.auth != SetupAuth::SignIn {
            return self.discover(None);
        }
        let label = field.input.text().trim().to_string();
        if method.accounts.iter().any(|account| account.label == label) {
            let taken =
                format!(" Another account is already labelled “{label}”. Choose another label.");
            setup.status = Some((taken, Tone::Warn));
            return;
        }
        let intent = Intent::SignIn {
            provider: provider.id.clone(),
            method: method.id.clone(),
            label: (!label.is_empty()).then_some(label),
        };
        setup.url = None;
        self.send_setup(Step::SigningIn, intent);
    }

    fn back_setup(&mut self) {
        let Some(setup) = &mut self.setup else {
            return;
        };
        let before_connect = if setup.skips_method() {
            Step::Provider
        } else {
            Step::Method
        };
        let keyed = setup
            .connection
            .as_ref()
            .is_some_and(|c| c.account.is_none());
        match setup.step {
            Step::Provider => {
                self.layers.pop();
            }
            Step::Method => setup.go(Step::Provider),
            Step::Connect => setup.go(before_connect),
            // A lost source has no sign-in to cancel, but the wait is still left.
            Step::SigningIn => {
                setup.go(Step::Connect);
                self.send_setup(Step::Connect, Intent::CancelSignIn);
            }
            Step::Models if keyed => setup.go(Step::Connect),
            Step::Models => setup.go(before_connect),
            Step::Settings => setup.go(Step::Models),
            // A save cannot be called back, and a second one would add a second backend.
            Step::Saving => {}
        }
    }

    /// An answer for a Setup layer no longer in the stack is dropped, as is a catalog once one
    /// is shown and models for a connection that is no longer the one discovered.
    pub(super) fn apply_setup(&mut self, update: Update) {
        let shown = self
            .layers
            .iter()
            .any(|layer| layer.kind == LayerKind::Setup);
        let Some(setup) = self.setup.as_mut().filter(|_| shown) else {
            return;
        };
        let reading = setup.providers.is_empty();
        let signing_in = setup.step == Step::SigningIn;
        let finding = match &update {
            Update::Models { connection, .. } | Update::Recommended { connection, .. } => {
                setup.step == Step::Models && setup.connection.as_ref() == Some(connection)
            }
            _ => false,
        };
        let saving = setup.step == Step::Saving;
        match update {
            Update::Setup(Ok(providers)) if reading => {
                setup.providers = providers;
                setup.provider = 0;
                setup.status = None;
            }
            Update::Setup(Err(why)) if reading => {
                setup.status = Some((format!(" could not read: {why}"), Tone::Warn));
            }
            Update::SignInUrl { url } if signing_in => setup.url = Some(url),
            Update::SignedIn(Ok(account)) if signing_in => {
                setup.signed_in(&account);
                self.discover(Some(&account));
            }
            Update::SignedIn(Err(why)) if signing_in => {
                setup.go(Step::Connect);
                setup.status = Some((format!(" {why}"), Tone::Warn));
            }
            // An error is a failed discovery, so Enter tries again.
            Update::Models { found, .. } if finding => {
                setup.found(found.unwrap_or_else(|why| Discovered {
                    models: Vec::new(),
                    failure: Some(why),
                    manual: false,
                }))
            }
            Update::Recommended { model, .. } if finding => match model {
                Ok(model) => setup.choose(model),
                Err(why) => setup.status = Some((format!(" {why}"), Tone::Warn)),
            },
            Update::Saved(Ok(notice)) if saving => {
                self.layers.retain(|layer| layer.kind != LayerKind::Setup);
                self.notice = Some(notice);
                self.read_menu(MenuId::Accounts);
            }
            Update::Saved(Err(why)) if saving => {
                setup.go(Step::Settings);
                setup.status = Some((format!(" {why}"), Tone::Warn));
            }
            _ => {}
        }
    }
}

/// The desktop's `validateInferenceSettings`: the first failure, in its order.
fn invalid(settings: &[SetupSetting], values: &[(SettingKey, String)]) -> Option<String> {
    let find = |key| {
        settings
            .iter()
            .zip(values)
            .find(|(setting, _)| setting.key == key)
    };
    if let Some((effort, (_, value))) = find(SettingKey::ReasoningEffort) {
        if !effort.choices.contains(value) {
            return Some("Choose a supported reasoning effort".to_string());
        }
    }
    let numbers = [
        SettingKey::ContextWindow,
        SettingKey::MaxOutputTokens,
        SettingKey::Temperature,
        SettingKey::TopP,
        SettingKey::MaxConcurrent,
    ];
    numbers.into_iter().find_map(|key| {
        let (setting, (_, value)) = find(key)?;
        let label = key.label();
        let Some(number) = value.parse::<f64>().ok().filter(|n| n.is_finite()) else {
            return Some(format!("{label} is required"));
        };
        if key.whole() && number.fract() != 0.0 {
            return Some(format!("{label} must be a whole number"));
        }
        let bound = |text: &Option<String>| text.as_deref().and_then(|t| t.parse::<f64>().ok());
        let below = bound(&setting.min).is_some_and(|min| number < min);
        let above = bound(&setting.max).is_some_and(|max| number > max);
        let min = setting.min.as_deref().unwrap_or("-∞");
        let max = setting.max.as_deref().unwrap_or("∞");
        (below || above).then(|| format!("{label} must be between {min} and {max}"))
    })
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::Receiver;

    use ratatui::crossterm::event::Event;

    use super::*;
    use crate::draw::theme;
    use crate::filter::subsequence;
    use crate::keys::KeySet::{self, Emacs, Vim};
    use crate::sample::MenuId;
    use crate::source::SettingKey::{ContextWindow, MaxConcurrent, ReasoningEffort};
    use crate::source::{Discovered, MenuAct, MenuRow, SetupModel, SetupSetting};
    use crate::testing::{buffer, connected, drain, find, kinds, press, screen, type_text};

    fn catalog() -> Vec<SetupProvider> {
        let method = |id: &str, name: &str, endpoint: &str, auth| SetupMethod {
            id: id.to_string(),
            name: name.to_string(),
            endpoint: endpoint.to_string(),
            endpoint_fixed: false,
            auth,
            accounts: Vec::new(),
        };
        let claude = SavedAccount {
            label: "Claude".to_string(),
            account_ref: "ref-0".to_string(),
        };
        vec![
            SetupProvider {
                id: "anthropic".to_string(),
                name: "Anthropic".to_string(),
                about: "Claude, by subscription or key".to_string(),
                methods: vec![
                    SetupMethod {
                        accounts: vec![claude],
                        ..method(
                            "claude_oauth",
                            "Claude sign-in",
                            "claude-cli://subscription",
                            SetupAuth::SignIn,
                        )
                    },
                    SetupMethod {
                        endpoint_fixed: true,
                        ..method(
                            "api_key",
                            "Anthropic API key",
                            "https://api.anthropic.com",
                            SetupAuth::Key,
                        )
                    },
                ],
            },
            SetupProvider {
                id: "local".to_string(),
                name: "Local".to_string(),
                about: "an OpenAI-compatible server".to_string(),
                methods: vec![method(
                    "optional_api_key",
                    "Endpoint + optional key",
                    "http://127.0.0.1:11434/v1",
                    SetupAuth::OptionalKey,
                )],
            },
        ]
    }

    /// Enter on the Accounts menu's add row, before the catalog is read.
    fn open(set: KeySet) -> (App, Receiver<Intent>) {
        let (mut app, rx) = connected(set, 132, 36);
        app.push_layer(LayerKind::Menu(MenuId::Accounts));
        let add = MenuRow {
            text: " + Add a provider".to_string(),
            tone: Tone::Accent,
            enter: Some(MenuAct::OpenSetup),
            d: None,
            more: Vec::new(),
        };
        app.apply(Update::Menu {
            menu: MenuId::Accounts,
            rows: Ok(vec![add]),
        });
        press(&mut app, "enter");
        (app, rx)
    }

    fn opened_in(set: KeySet) -> (App, Receiver<Intent>) {
        let (mut app, rx) = open(set);
        app.apply(Update::Setup(Ok(catalog())));
        rx.try_iter().for_each(drop);
        (app, rx)
    }

    fn opened() -> (App, Receiver<Intent>) {
        opened_in(Vim)
    }

    fn step(app: &App) -> Step {
        app.setup.as_ref().expect("a setup").step
    }

    fn connection(provider: &str, method: &str, endpoint: &str) -> SetupConnection {
        SetupConnection {
            provider: provider.to_string(),
            method: method.to_string(),
            endpoint: endpoint.to_string(),
            key: None,
            account: None,
        }
    }

    #[test]
    fn setup_reads_the_catalog_as_it_opens() {
        let (mut app, rx) = open(Vim);
        assert_eq!(kinds(&app).last(), Some(&LayerKind::Setup));
        assert_eq!(
            drain(&rx),
            [Intent::ReadMenu(MenuId::Accounts), Intent::ReadSetup]
        );
        let s = screen(&app);
        assert!(find(&s, "Add a provider ").is_some());
        assert!(find(&s, " reading…").is_some());
        assert!(find(&s, " enter choose · esc close").is_some());

        app.apply(Update::Setup(Ok(catalog())));
        let s = screen(&app);
        assert_eq!(find(&s, " reading…"), None);
        assert!(find(&s, " Anthropic   Claude, by subscription or key").is_some());
        assert!(find(&s, " Local   an OpenAI-compatible server").is_some());
    }

    #[test]
    fn a_new_sign_in_sends_its_label() {
        let (mut app, rx) = opened();
        press(&mut app, "enter");
        assert_eq!(step(&app), Step::Method);
        let s = screen(&app);
        assert!(find(&s, "Anthropic: connection").is_some());
        for row in [
            " Claude sign-in · new account",
            " Claude sign-in · use Claude",
            " Anthropic API key",
        ] {
            assert!(find(&s, row).is_some(), "{row}");
        }

        press(&mut app, "enter");
        assert_eq!(step(&app), Step::Connect);
        let s = screen(&app);
        assert!(find(&s, "Anthropic: Claude sign-in").is_some());
        assert!(find(&s, " Label     Optional, e.g. Work").is_some());
        assert!(find(&s, " enter sign in · esc back").is_some());

        type_text(&mut app, "Work");
        press(&mut app, "enter");
        let sign_in = Intent::SignIn {
            provider: "anthropic".to_string(),
            method: "claude_oauth".to_string(),
            label: Some("Work".to_string()),
        };
        assert_eq!(drain(&rx), [sign_in]);
        assert_eq!(step(&app), Step::SigningIn);
        assert!(find(
            &screen(&app),
            " Waiting for you to sign in in your browser."
        )
        .is_some());

        app.apply(Update::SignInUrl {
            url: "https://claude.ai/x".to_string(),
        });
        let s = screen(&app);
        assert!(find(&s, "https://claude.ai/x").is_some());
        assert!(find(&s, " esc cancel").is_some());

        app.apply(Update::SignedIn(Ok(SavedAccount {
            label: "Work".to_string(),
            account_ref: "ref-1".to_string(),
        })));
        let signed_in = SetupConnection {
            account: Some("ref-1".to_string()),
            ..connection("anthropic", "claude_oauth", "claude-cli://subscription")
        };
        assert_eq!(drain(&rx), [Intent::Discover(signed_in)]);
        assert_eq!(step(&app), Step::Models);
        assert!(find(&screen(&app), " finding models…").is_some());
    }

    #[test]
    fn a_taken_label_is_refused() {
        let (mut app, rx) = opened();
        press(&mut app, "enter enter");
        type_text(&mut app, "Claude");
        press(&mut app, "enter");
        let refused = "Another account is already labelled “Claude”. Choose another label.";
        assert!(find(&screen(&app), refused).is_some());
        assert_eq!(drain(&rx), []);
        assert_eq!(step(&app), Step::Connect);
    }

    #[test]
    fn a_saved_account_skips_sign_in() {
        let (mut app, rx) = opened();
        press(&mut app, "enter down enter");
        let saved = SetupConnection {
            account: Some("ref-0".to_string()),
            ..connection("anthropic", "claude_oauth", "claude-cli://subscription")
        };
        assert_eq!(drain(&rx), [Intent::Discover(saved)]);
        assert_eq!(step(&app), Step::Models);
    }

    #[test]
    fn esc_cancels_a_waiting_sign_in() {
        let (mut app, rx) = opened();
        press(&mut app, "enter enter enter");
        let unlabelled = Intent::SignIn {
            provider: "anthropic".to_string(),
            method: "claude_oauth".to_string(),
            label: None,
        };
        assert_eq!(drain(&rx), [unlabelled]);
        assert_eq!(step(&app), Step::SigningIn);

        press(&mut app, "esc");
        assert_eq!(drain(&rx), [Intent::CancelSignIn]);
        assert_eq!(step(&app), Step::Connect);
        assert!(find(&screen(&app), " Sign-in cancelled.").is_some());
        press(&mut app, "esc esc");
        assert_eq!(step(&app), Step::Provider);
        press(&mut app, "esc");
        assert_eq!(kinds(&app), [LayerKind::Menu(MenuId::Accounts)]);
    }

    #[test]
    fn a_key_is_masked_and_never_debug_printed() {
        let (mut app, rx) = opened();
        press(&mut app, "down enter");
        assert_eq!(step(&app), Step::Connect);
        let s = screen(&app);
        assert!(find(&s, "Local: Endpoint + optional key").is_some());
        assert!(find(&s, " API key (optional) Stored only when you save").is_some());
        assert!(find(&s, " enter connect · tab next field · esc back").is_some());

        type_text(&mut app, "sk-secret");
        let s = screen(&app);
        assert!(find(&s, "•••••••••").is_some());
        assert_eq!(find(&s, "sk-secret"), None);

        press(&mut app, "enter");
        let intent = rx.try_recv().expect("a discover");
        let keyed = SetupConnection {
            key: Some(Secret("sk-secret".to_string())),
            ..connection("local", "optional_api_key", "http://127.0.0.1:11434/v1")
        };
        assert_eq!(intent, Intent::Discover(keyed));
        assert!(!format!("{intent:?}").contains("sk-secret"));
        assert_eq!(step(&app), Step::Models);
    }

    #[test]
    fn a_pasted_key_goes_into_the_field() {
        let (mut app, _rx) = opened();
        press(&mut app, "down enter");
        app.handle(Event::Paste("sk-pasted".to_string()));
        let setup = app.setup.as_ref().expect("a setup");
        assert_eq!(setup.fields[0].input.text(), "sk-pasted");
    }

    #[test]
    fn a_fixed_endpoint_cannot_be_edited() {
        let (mut app, _rx) = opened();
        press(&mut app, "enter down down enter");
        assert_eq!(step(&app), Step::Connect);
        press(&mut app, "tab");
        type_text(&mut app, "x");
        let setup = app.setup.as_ref().expect("a setup");
        assert_eq!(setup.focus, 0);
        assert_eq!(setup.fields[0].input.text(), "x");
        assert_eq!(setup.fields[1].input.text(), "https://api.anthropic.com");
        assert_eq!(find(&screen(&app), "https://api.anthropic.comx"), None);
    }

    #[test]
    fn tab_reaches_an_editable_endpoint_in_both_key_sets() {
        for set in [Vim, Emacs] {
            let (mut app, rx) = opened_in(set);
            press(&mut app, "down enter tab bs");
            type_text(&mut app, "2");
            assert_eq!(app.setup.as_ref().expect("a setup").focus, 1, "{set:?}");
            let edited = "http://127.0.0.1:11434/v2";
            assert!(find(&screen(&app), edited).is_some(), "{set:?}");
            press(&mut app, "enter");
            let discover = Intent::Discover(connection("local", "optional_api_key", edited));
            assert_eq!(drain(&rx), [discover], "{set:?}");
        }
    }

    #[test]
    fn closing_a_waiting_sign_in_cancels_it_before_the_next() {
        let (mut app, rx) = opened();
        press(&mut app, "enter enter enter");
        assert_eq!(step(&app), Step::SigningIn);
        press(&mut app, "ctrl-w q");
        assert_eq!(kinds(&app), [LayerKind::Menu(MenuId::Accounts)]);

        press(&mut app, "enter");
        app.apply(Update::Setup(Ok(catalog())));
        press(&mut app, "enter enter enter");
        let sign_in = Intent::SignIn {
            provider: "anthropic".to_string(),
            method: "claude_oauth".to_string(),
            label: None,
        };
        assert_eq!(
            drain(&rx),
            [
                sign_in.clone(),
                Intent::CancelSignIn,
                Intent::ReadSetup,
                sign_in
            ]
        );
    }

    const NO_MODEL: &str = "No model is connected yet, so The Engineer cannot answer.";

    /// A shell whose source found no agent that can run.
    fn without_an_agent() -> (App, Receiver<Intent>) {
        let (mut app, rx) = connected(Vim, 132, 36);
        app.apply(Update::Agents(Vec::new()));
        app.apply(Update::Connection(Connection::NoAgent(
            NO_MODEL.to_string(),
        )));
        (app, rx)
    }

    #[test]
    fn a_shell_with_no_agent_opens_on_setup_and_says_why() {
        let (mut app, rx) = without_an_agent();
        assert_eq!(kinds(&app), [LayerKind::Setup]);
        assert_eq!(drain(&rx), [Intent::ReadSetup]);

        app.apply(Update::Setup(Ok(catalog())));
        let s = screen(&app);
        assert!(find(&s, "Add a provider").is_some(), "{s:#?}");
        assert!(find(&s, "Anthropic").is_some(), "{s:#?}");
        assert_eq!(find(&s, NO_MODEL).map(|(row, _)| row), Some(35), "{s:#?}");
        assert!(find(&s, "no agent ready").is_some(), "{s:#?}");

        press(&mut app, "down enter");
        assert_eq!(step(&app), Step::Connect);

        // The same state told again, as its reason changes, opens nothing over the user.
        press(&mut app, "esc esc");
        assert_eq!(kinds(&app), []);
        app.apply(Update::Connection(Connection::NoAgent(
            "another reason".to_string(),
        )));
        assert_eq!(kinds(&app), []);
        assert!(find(&screen(&app), "another reason").is_some());
    }

    #[test]
    fn the_message_box_sends_nothing_until_an_agent_can_run() {
        let off = "No agent can answer yet. Press enter to connect a model.";
        let (mut app, rx) = without_an_agent();
        app.apply(Update::Setup(Ok(catalog())));
        press(&mut app, "esc");
        assert_eq!(kinds(&app), []);
        let s = screen(&app);
        assert!(find(&s, off).is_some(), "{s:#?}");
        assert!(find(&s, "enter send").is_none(), "{s:#?}");
        rx.try_iter().for_each(drop);

        // Enter on the empty box, and on a typed message, opens Setup and sends nothing.
        press(&mut app, "enter");
        assert_eq!(kinds(&app), [LayerKind::Setup]);
        press(&mut app, "esc");
        type_text(&mut app, "hello");
        press(&mut app, "enter");
        assert_eq!(kinds(&app), [LayerKind::Setup]);
        assert_eq!(app.notice.as_deref(), Some(NO_MODEL));
        assert_eq!(app.tabs[0].input.text(), "hello");
        assert_eq!(drain(&rx), [Intent::ReadSetup, Intent::ReadSetup]);

        // The menus read over the source as they do with an agent.
        app.push_layer(LayerKind::Menu(MenuId::Accounts));
        assert_eq!(drain(&rx), [Intent::ReadMenu(MenuId::Accounts)]);

        // An agent that became ready takes the starter tab, and the typed message goes to it.
        app.apply(Update::Agents(vec![crate::source::Agent {
            id: "did:engineer".to_string(),
            name: "The Engineer".to_string(),
            text: " The Engineer default".to_string(),
            tone: Tone::Plain,
        }]));
        app.apply(Update::Connection(Connection::Ready));
        app.layers.clear();
        assert_eq!(app.picker_matches("").len(), 1);
        assert!(find(&screen(&app), "runtime ready").is_some());
        press(&mut app, "enter");
        assert_eq!(
            drain(&rx),
            [Intent::Send {
                tab: 0,
                session: None,
                agent: "did:engineer".to_string(),
                text: "hello".to_string(),
            }]
        );
        app.tabs[0].following = false;
        assert!(find(&screen(&app), "Message The Engineer").is_some());
    }

    #[test]
    fn a_lost_source_keeps_the_layer_and_says_why() {
        let gone = " could not reach the runtime: gone";
        let (mut app, rx) = opened();
        press(&mut app, "down enter");
        app.connection = Connection::Lost("gone".to_string());
        press(&mut app, "enter");
        assert_eq!(kinds(&app).last(), Some(&LayerKind::Setup));
        assert_eq!(step(&app), Step::Connect);
        assert!(find(&screen(&app), gone).is_some());
        assert_eq!(drain(&rx), []);

        let (mut app, _rx) = opened();
        press(&mut app, "enter enter enter");
        app.apply(Update::Connection(Connection::Lost("gone".to_string())));
        assert_eq!(step(&app), Step::SigningIn);
        assert!(find(&screen(&app), gone).is_some());
    }

    #[test]
    fn the_sample_adds_a_provider_from_the_accounts_menu() {
        let mut app = App::new(Vim, 132, 36);
        app.push_layer(LayerKind::Menu(MenuId::Accounts));
        press(&mut app, "enter");
        assert_eq!(kinds(&app).last(), Some(&LayerKind::Setup));
        assert!(find(&screen(&app), " OpenRouter   ").is_some());
        press(&mut app, "down enter enter enter");
        assert_eq!(step(&app), Step::Models);
        assert!(find(&screen(&app), " Claude Sonnet 4.5   claude-sonnet-4-5").is_some());
        press(&mut app, "enter enter");
        assert_eq!(kinds(&app), [LayerKind::Menu(MenuId::Accounts)]);
        let saved = "saved anthropic-2: claude-sonnet-4-5";
        assert_eq!(app.notice.as_deref(), Some(saved));
    }

    fn local() -> SetupConnection {
        connection("local", "optional_api_key", "http://127.0.0.1:11434/v1")
    }

    /// The Local connection, sent with `Discover` and waiting for its models.
    fn in_models() -> (App, Receiver<Intent>) {
        let (mut app, rx) = opened();
        press(&mut app, "down enter enter");
        assert_eq!(drain(&rx), [Intent::Discover(local())]);
        (app, rx)
    }

    /// The answer to `in_models`' discovery.
    fn found(models: Vec<SetupModel>, failure: Option<&str>, manual: bool) -> Update {
        Update::Models {
            connection: local(),
            found: Ok(Discovered {
                models,
                failure: failure.map(str::to_string),
                manual,
            }),
        }
    }

    fn model(id: &str, name: &str, settings: Vec<SetupSetting>) -> SetupModel {
        SetupModel {
            id: id.to_string(),
            name: name.to_string(),
            settings,
        }
    }

    fn sonnet_settings() -> Vec<SetupSetting> {
        let number = |key, value: &str, min: &str, max: &str| SetupSetting {
            key,
            value: value.to_string(),
            min: Some(min.to_string()),
            max: Some(max.to_string()),
            choices: Vec::new(),
        };
        vec![
            number(ContextWindow, "200000", "1024", "200000"),
            number(MaxConcurrent, "4", "1", "64"),
            SetupSetting {
                key: ReasoningEffort,
                value: "medium".to_string(),
                min: None,
                max: None,
                choices: ["low", "medium", "high"].map(str::to_string).to_vec(),
            },
        ]
    }

    /// Typing "sonnet" and Enter on a list whose first model is another.
    fn in_settings() -> (App, Receiver<Intent>) {
        let (mut app, rx) = in_models();
        let haiku = model("claude-haiku", "claude-haiku", Vec::new());
        let sonnet = model("claude-sonnet", "Claude Sonnet", sonnet_settings());
        app.apply(found(vec![haiku, sonnet], None, false));
        type_text(&mut app, "sonnet");
        press(&mut app, "enter");
        (app, rx)
    }

    fn highlighted(app: &App, text: &str) -> bool {
        let Some((row, column)) = find(&screen(app), text) else {
            return false;
        };
        buffer(app)[(column, row)].bg == theme::SEL_BG
    }

    #[test]
    fn typing_narrows_a_long_model_list() {
        let (mut app, _rx) = in_models();
        let models = (0..300)
            .map(|n| format!("model-{n:03}"))
            .map(|id| model(&id, &id, Vec::new()))
            .collect();
        app.apply(found(models, None, false));
        let s = screen(&app);
        assert!(find(&s, "Choose a model ").is_some());
        assert!(find(&s, " Search  Search advertised models").is_some());
        assert!(find(&s, " type to search · enter choose · esc back").is_some());
        assert!(find(&s, " model-001").is_some());

        type_text(&mut app, "29");
        let s = screen(&app);
        assert!(find(&s, " Search  29").is_some());
        let shown: Vec<&str> = s
            .iter()
            .filter_map(|row| row.find("model-").map(|at| &row[at..at + 9]))
            .collect();
        assert!(shown.len() > 10, "{shown:?}");
        assert!(shown.iter().all(|id| subsequence("29", id)), "{shown:?}");
        assert_eq!(shown.first(), Some(&"model-029"));
        assert_eq!(find(&s, " model-001"), None);

        press(&mut app, "down down down down down down down down down");
        assert!(highlighted(&app, " model-279"));
        press(&mut app, "down down down down down down down down down");
        assert!(highlighted(&app, " model-297"));
    }

    #[test]
    fn choosing_a_model_prefills_its_settings() {
        let (mut app, _rx) = in_models();
        let haiku = model("claude-haiku", "claude-haiku", Vec::new());
        let sonnet = model("claude-sonnet", "Claude Sonnet", sonnet_settings());
        app.apply(found(vec![haiku, sonnet], None, false));
        let s = screen(&app);
        assert!(highlighted(&app, " claude-haiku"));
        assert!(find(&s, " Claude Sonnet   claude-sonnet").is_some());

        press(&mut app, "down enter");
        assert_eq!(step(&app), Step::Settings);
        let s = screen(&app);
        assert!(find(&s, " claude-sonnet ").is_some());
        assert!(find(&s, " Context window    200000").is_some());
        assert!(find(&s, " Concurrency       4").is_some());
        assert!(find(&s, " Reasoning effort  medium").is_some());
        assert!(find(&s, " enter save · tab next · ←→ choose · esc back").is_some());

        press(&mut app, "tab tab right");
        assert!(find(&screen(&app), " Reasoning effort  high").is_some());
    }

    #[test]
    fn settings_are_checked_like_the_desktop() {
        for (to, value, why) in [
            ("", "abc", "Context window is required"),
            ("", "1.5", "Context window must be a whole number"),
            ("tab", "99", "Concurrency must be between 1 and 64"),
        ] {
            let (mut app, rx) = in_settings();
            assert_eq!(step(&app), Step::Settings, "{why}");
            press(&mut app, &format!("{to} ctrl-u"));
            type_text(&mut app, value);
            press(&mut app, "enter");
            assert!(find(&screen(&app), why).is_some(), "{why}");
            assert_eq!(drain(&rx), [], "{why}");
            assert_eq!(step(&app), Step::Settings, "{why}");
        }
    }

    #[test]
    fn save_sends_the_settings_and_closes_on_success() {
        let (mut app, rx) = in_settings();
        press(&mut app, "tab tab right enter");
        let save = Intent::SaveBackend {
            connection: local(),
            model: "claude-sonnet".to_string(),
            settings: vec![
                (ContextWindow, "200000".to_string()),
                (MaxConcurrent, "4".to_string()),
                (ReasoningEffort, "high".to_string()),
            ],
        };
        assert_eq!(drain(&rx), std::slice::from_ref(&save));
        assert_eq!(step(&app), Step::Saving);
        assert!(find(&screen(&app), " saving…").is_some());

        app.apply(Update::Saved(Err("could not write".to_string())));
        assert_eq!(step(&app), Step::Settings);
        assert!(find(&screen(&app), " could not write").is_some());
        press(&mut app, "enter");
        assert_eq!(drain(&rx), [save]);

        let saved = "saved anthropic-2: claude-sonnet";
        app.apply(Update::Saved(Ok(saved.to_string())));
        assert_eq!(kinds(&app), [LayerKind::Menu(MenuId::Accounts)]);
        assert_eq!(app.notice.as_deref(), Some(saved));
        assert_eq!(drain(&rx), [Intent::ReadMenu(MenuId::Accounts)]);
    }

    #[test]
    fn a_failed_discovery_says_why_and_retries() {
        let (mut app, rx) = in_models();
        app.apply(found(Vec::new(), Some("401 Unauthorized"), false));
        assert!(find(&screen(&app), "401 Unauthorized").is_some());
        press(&mut app, "enter");
        assert_eq!(drain(&rx), [Intent::Discover(local())]);
        assert!(find(&screen(&app), " finding models…").is_some());
    }

    #[test]
    fn an_empty_list_asks_for_a_model_id() {
        let (mut app, rx) = in_models();
        app.apply(found(Vec::new(), None, true));
        assert!(find(&screen(&app), " Model id  Exact served model ID").is_some());
        type_text(&mut app, "llama3");
        press(&mut app, "enter");
        let recommend = Intent::Recommend {
            connection: local(),
            model: "llama3".to_string(),
        };
        assert_eq!(drain(&rx), [recommend]);

        app.apply(Update::Recommended {
            connection: local(),
            model: Ok(model("llama3", "llama3", sonnet_settings())),
        });
        assert_eq!(step(&app), Step::Settings);
        assert!(find(&screen(&app), " llama3 ").is_some());
    }

    /// Esc from a waiting discovery, the endpoint fixed, and Enter again.
    #[test]
    fn an_answer_for_an_earlier_connection_is_dropped() {
        let (mut app, rx) = in_models();
        press(&mut app, "esc tab bs");
        type_text(&mut app, "2");
        press(&mut app, "enter");
        let fixed = connection("local", "optional_api_key", "http://127.0.0.1:11434/v2");
        assert_eq!(drain(&rx), [Intent::Discover(fixed.clone())]);

        let old = model("old-model", "old-model", Vec::new());
        app.apply(found(vec![old], None, false));
        app.apply(Update::Recommended {
            connection: local(),
            model: Ok(model("llama3", "llama3", Vec::new())),
        });
        assert_eq!(step(&app), Step::Models);
        let s = screen(&app);
        assert!(find(&s, " finding models…").is_some());
        assert_eq!(find(&s, "old-model"), None);

        let new = Discovered {
            models: vec![model("new-model", "new-model", Vec::new())],
            failure: None,
            manual: false,
        };
        app.apply(Update::Models {
            connection: fixed,
            found: Ok(new),
        });
        app.apply(Update::Models {
            connection: local(),
            found: Err("timed out".to_string()),
        });
        let s = screen(&app);
        assert!(find(&s, " new-model").is_some());
        assert_eq!(find(&s, "timed out"), None);
    }

    #[test]
    fn a_second_catalog_answer_moves_nothing() {
        let (mut app, _rx) = opened();
        press(&mut app, "enter");
        let mut reversed = catalog();
        reversed.reverse();
        app.apply(Update::Setup(Ok(reversed)));
        app.apply(Update::Setup(Err("late".to_string())));
        assert_eq!(step(&app), Step::Method);
        let s = screen(&app);
        assert!(find(&s, "Anthropic: connection").is_some());
        assert_eq!(find(&s, "late"), None);
    }

    #[test]
    fn a_long_failure_wraps_to_show_its_cause() {
        let (mut app, _rx) = in_models();
        let refused = "querying OpenAI-compatible models endpoint http://127.0.0.1:11434/v1/models: error sending request for url (http://127.0.0.1:11434/v1/models): connection refused";
        app.apply(found(Vec::new(), Some(refused), false));
        let s = screen(&app);
        assert!(find(&s, " querying OpenAI-compatible").is_some(), "{s:#?}");
        assert!(find(&s, "connection refused").is_some(), "{s:#?}");
    }

    #[test]
    fn a_fresh_account_is_offered_back_and_its_label_taken() {
        let (mut app, rx) = opened();
        press(&mut app, "enter enter");
        type_text(&mut app, "Work");
        press(&mut app, "enter");
        app.apply(Update::SignedIn(Ok(SavedAccount {
            label: "Work".to_string(),
            account_ref: "ref-1".to_string(),
        })));
        assert_eq!(step(&app), Step::Models);

        press(&mut app, "esc");
        assert_eq!(step(&app), Step::Method);
        assert!(find(&screen(&app), " Claude sign-in · use Work").is_some());
        press(&mut app, "enter");
        type_text(&mut app, "Work");
        rx.try_iter().for_each(drop);
        press(&mut app, "enter");
        let refused = "Another account is already labelled “Work”. Choose another label.";
        assert!(find(&screen(&app), refused).is_some());
        assert_eq!(drain(&rx), []);
    }

    #[test]
    fn esc_on_a_lost_sign_in_goes_back_without_sending() {
        let (mut app, rx) = opened();
        press(&mut app, "enter enter enter");
        rx.try_iter().for_each(drop);
        app.apply(Update::Connection(Connection::Lost("gone".to_string())));
        press(&mut app, "esc");
        assert_eq!(step(&app), Step::Connect);
        assert_eq!(drain(&rx), []);
        assert!(find(&screen(&app), " could not reach the runtime: gone").is_some());
    }

    #[test]
    fn a_send_that_finds_the_source_gone_says_so() {
        let (mut app, rx) = opened();
        press(&mut app, "down enter");
        drop(rx);
        press(&mut app, "enter");
        let s = screen(&app);
        let gone = " could not reach the runtime: the runtime source stopped";
        assert!(find(&s, gone).is_some(), "{s:#?}");
        assert_eq!(find(&s, " finding models…"), None);
    }
}
