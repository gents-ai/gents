use crate::sample::View;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    OpenPicker,
    OpenKeys,
    ClearTab,
    ShowSidebar(View),
    NotWired,
    SwitchKeys,
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Command {
    pub slash: Option<&'static str>,
    pub label: &'static str,
    pub aliases: &'static [&'static str],
    pub about: &'static str,
    pub effect: Effect,
}

const fn command(
    slash: Option<&'static str>,
    label: &'static str,
    aliases: &'static [&'static str],
    about: &'static str,
    effect: Effect,
) -> Command {
    Command {
        slash,
        label,
        aliases,
        about,
        effect,
    }
}

const SIDEBAR_VIEW: &str = "right sidebar view";

/// The one list both `/` and the quick-jump bar read.
#[rustfmt::skip]
pub const COMMANDS: &[Command] = &[
    command(Some("agent"), "/agent", &[], "open an agent in a new tab", Effect::OpenPicker),
    command(Some("new"), "New tab", &["tabnew", "new"], "open a new tab", Effect::OpenPicker),
    command(Some("model"), "/model", &[], "pick a model for this agent", Effect::NotWired),
    command(Some("goal"), "/goal", &[], "set or check this session's goal", Effect::NotWired),
    command(Some("clear"), "/clear", &[], "clear this tab's view", Effect::ClearTab),
    command(Some("jobs"), "/jobs", &[], "show background jobs", Effect::ShowSidebar(View::Agents)),
    command(Some("help"), "/help", &[], "show the keys", Effect::OpenKeys),
    command(Some("quit"), "Quit", &["q", "q!", "qa", "wq"], "quit", Effect::Quit),
    command(None, "Switch key set", &["vim", "emacs", "keys"], "vim to emacs or back", Effect::SwitchKeys),
    command(None, "Sidebar: Changes", &["changes"], SIDEBAR_VIEW, Effect::ShowSidebar(View::Changes)),
    command(None, "Sidebar: Workflows", &["workflows"], SIDEBAR_VIEW, Effect::ShowSidebar(View::Workflows)),
    command(None, "Sidebar: Background agents", &["background"], SIDEBAR_VIEW, Effect::ShowSidebar(View::Agents)),
];
