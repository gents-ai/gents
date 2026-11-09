use crate::sample::{MenuId, Scope, View};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    OpenPicker,
    OpenKeys,
    OpenMenu(MenuId),
    ClearTab,
    CloseTab,
    ShowSidebar(View),
    ShowDiff(Scope),
    Goal,
    NotWired,
    SwitchKeys,
    DismissPrompt,
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Command {
    pub slash: Option<&'static str>,
    pub label: &'static str,
    pub aliases: &'static [&'static str],
    pub about: &'static str,
    pub effect: Effect,
    /// The quick-jump bar lists it; commands that act on this tab's
    /// conversation stay in `/`.
    pub jump: bool,
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
        jump: true,
    }
}

const fn slash_only(
    slash: &'static str,
    label: &'static str,
    about: &'static str,
    effect: Effect,
) -> Command {
    Command {
        jump: false,
        ..command(Some(slash), label, &[], about, effect)
    }
}

const SIDEBAR_VIEW: &str = "right sidebar view";

/// The one list both `/` and the quick-jump bar read.
#[rustfmt::skip]
pub const COMMANDS: &[Command] = &[
    slash_only("agent", "/agent", "open an agent in a new tab", Effect::OpenPicker),
    command(Some("new"), "New tab", &["tabnew", "new"], "open a new tab", Effect::OpenPicker),
    command(None, "Close tab", &["close", "tabclose", "tabc"], "close this tab", Effect::CloseTab),
    slash_only("goal", "/goal", "set a goal the agent works toward until it's met", Effect::Goal),
    slash_only("clear", "/clear", "start a new session with this agent", Effect::ClearTab),
    slash_only("jobs", "/jobs", "show background jobs", Effect::ShowSidebar(View::Agents)),
    slash_only("usage", "/usage", "show your providers' usage limits", Effect::OpenMenu(MenuId::Usage)),
    command(Some("help"), "/help", &[], "show the keys", Effect::OpenKeys),
    command(Some("quit"), "Quit", &["q", "q!", "qa", "wq"], "quit", Effect::Quit),
    slash_only("dismiss", "/dismiss", "dismiss the prompt above", Effect::DismissPrompt),
    command(None, "Switch key set", &["vim", "emacs", "keys"], "vim to emacs or back", Effect::SwitchKeys),
    command(Some("diff"), "Sidebar: Diff", &["diff", "changes"], SIDEBAR_VIEW, Effect::ShowDiff(Scope::Session)),
    command(Some("workflows"), "Sidebar: This session's workflows", &["workflows"], SIDEBAR_VIEW, Effect::ShowSidebar(View::Workflows)),
    command(None, "Sidebar: Diff, working tree", &["tree", "worktree", "git"], SIDEBAR_VIEW, Effect::ShowDiff(Scope::Tree)),
    command(None, "Sidebar: Background agents", &["background"], SIDEBAR_VIEW, Effect::ShowSidebar(View::Agents)),
];
