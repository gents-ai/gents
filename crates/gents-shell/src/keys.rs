use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySet {
    /// Always typing, with one ctrl key per command, as most apps have them.
    Regular,
    Vim,
    Emacs,
}

impl KeySet {
    pub fn name(self) -> &'static str {
        match self {
            KeySet::Regular => "regular",
            KeySet::Vim => "vim",
            KeySet::Emacs => "emacs",
        }
    }
}

/// Sample copy names `esc`, which means back in every key set; emacs people are shown theirs.
pub fn in_key_set(set: KeySet, text: &str) -> String {
    match set {
        KeySet::Emacs => text.replace("esc", "ctrl-g"),
        _ => text.to_string(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyCtx {
    Normal,
    Input,
    Filter,
    Menu,
    SearchLine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Down,
    Up,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    Insert(char),
    Backspace,
    Delete,
    DeleteWord,
    DeleteToStart,
    CursorLeft,
    CursorRight,
    WordLeft,
    WordRight,
    Home,
    End,
    /// A line break in the message box.
    NewLine,
    Submit,
    Back,
    Interrupt,
    /// Stops the tab's running turn, leaving the box as it is.
    Stop,
    EnterInput,
    MoveFocus(Dir),
    NextPane,
    NextTab,
    PrevTab,
    NewTab,
    ClosePane,
    ToggleSessions,
    ToggleSidebar,
    OpenBar,
    /// `ctrl-p` in vim outside lists: the jump bar, or up while the slash list is open.
    Palette,
    OpenKeys,
    OpenSearch,
    Next,
    Prev,
    PageUp,
    PageDown,
    /// `gg` in vim, `alt-<` in emacs: the focused pane's first row.
    Top,
    /// `G` in vim, `alt->` in emacs: the focused pane's last row.
    Bottom,
    SearchNext,
    SearchPrev,
    Complete,
    /// Shift-Tab in a form: the field before.
    CompleteBack,
    /// Ctrl-s in a form.
    Save,
    /// `y`: the shown prompt's main answer.
    Accept,
    /// `d` in a menu: the selected row's other act, beside enter's.
    SecondAct,
    /// `.` in a menu: the selected row's further acts.
    More,
    Quit,
}

impl Action {
    /// Changes the text of a line being typed.
    pub fn changes_line(self) -> bool {
        matches!(
            self,
            Action::Insert(_) | Action::Backspace | Action::Delete | Action::DeleteToStart
        )
    }

    /// Changes a line being typed or moves in it, as `LineInput::edit` takes.
    pub fn edits_line(self) -> bool {
        self.changes_line()
            || matches!(
                self,
                Action::CursorLeft
                    | Action::CursorRight
                    | Action::WordLeft
                    | Action::WordRight
                    | Action::Home
                    | Action::End
            )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Prefix {
    CtrlW,
    CtrlX,
    G,
}

pub struct Keymap {
    set: KeySet,
    pending: Option<Prefix>,
}

impl Keymap {
    pub fn new(set: KeySet) -> Keymap {
        Keymap { set, pending: None }
    }

    pub fn set(&self) -> KeySet {
        self.set
    }

    pub fn pending(&self) -> Option<&'static str> {
        self.pending.map(|prefix| match prefix {
            Prefix::CtrlW => "ctrl-w ...",
            Prefix::CtrlX => "ctrl-x ...",
            Prefix::G => "g ...",
        })
    }

    pub fn cancel_pending(&mut self) {
        self.pending = None;
    }

    pub fn map(&mut self, ctx: KeyCtx, key: KeyEvent) -> Action {
        let key = legacy(key);
        if let Some(prefix) = self.pending.take() {
            return Self::chord(prefix, key);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return self.control(ctx, key.code);
        }
        if key.modifiers.contains(KeyModifiers::ALT) {
            return match key.code {
                KeyCode::Char('x') if self.set == KeySet::Emacs => Action::OpenBar,
                KeyCode::Char('<') if self.set == KeySet::Emacs => Action::Top,
                KeyCode::Char('>') if self.set == KeySet::Emacs => Action::Bottom,
                KeyCode::Char('b') | KeyCode::Left if in_line(ctx) => Action::WordLeft,
                KeyCode::Char('f') | KeyCode::Right if in_line(ctx) => Action::WordRight,
                KeyCode::Backspace if in_line(ctx) => Action::DeleteWord,
                KeyCode::Enter if ctx == KeyCtx::Input => Action::NewLine,
                _ => Action::None,
            };
        }
        let regular_search = self.set == KeySet::Regular && ctx == KeyCtx::SearchLine;
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Esc => Action::Back,
            KeyCode::Enter if shift && ctx == KeyCtx::Input => Action::NewLine,
            KeyCode::Enter => Action::Submit,
            KeyCode::Down if regular_search => Action::SearchNext,
            KeyCode::Up if regular_search => Action::SearchPrev,
            KeyCode::Down => Action::Next,
            KeyCode::Up => Action::Prev,
            KeyCode::PageUp if matches!(ctx, KeyCtx::Normal | KeyCtx::Input | KeyCtx::Menu) => {
                Action::PageUp
            }
            KeyCode::PageDown if matches!(ctx, KeyCtx::Normal | KeyCtx::Input | KeyCtx::Menu) => {
                Action::PageDown
            }
            KeyCode::Char(c) => self.character(ctx, c),
            KeyCode::Tab if matches!(ctx, KeyCtx::Input | KeyCtx::Filter) => Action::Complete,
            KeyCode::BackTab if ctx == KeyCtx::Filter => Action::CompleteBack,
            code if in_line(ctx) => edit_key(code, key.modifiers.contains(KeyModifiers::SUPER)),
            _ => Action::None,
        }
    }

    fn chord(prefix: Prefix, key: KeyEvent) -> Action {
        let KeyCode::Char(c) = key.code else {
            return Action::None;
        };
        if key.modifiers.contains(KeyModifiers::ALT) {
            return Action::None;
        }
        let moves = prefix == Prefix::CtrlW && matches!(c, 'h' | 'j' | 'k' | 'l');
        if key.modifiers.contains(KeyModifiers::CONTROL) && !moves {
            return if prefix == Prefix::CtrlX && c == 'c' {
                Action::Quit
            } else {
                Action::None
            };
        }
        match prefix {
            Prefix::G => match c {
                't' => Action::NextTab,
                'T' => Action::PrevTab,
                'g' => Action::Top,
                _ => Action::None,
            },
            Prefix::CtrlW => match c {
                'h' => Action::MoveFocus(Dir::Left),
                'j' => Action::MoveFocus(Dir::Down),
                'k' => Action::MoveFocus(Dir::Up),
                'l' => Action::MoveFocus(Dir::Right),
                'q' => Action::ClosePane,
                '[' => Action::ToggleSessions,
                ']' => Action::ToggleSidebar,
                _ => Action::None,
            },
            Prefix::CtrlX => match c {
                'o' => Action::NextPane,
                'n' => Action::NextTab,
                'p' => Action::PrevTab,
                't' => Action::NewTab,
                '0' => Action::ClosePane,
                '[' => Action::ToggleSessions,
                ']' => Action::ToggleSidebar,
                'x' => Action::OpenBar,
                '?' => Action::OpenKeys,
                _ => Action::None,
            },
        }
    }

    fn control(&mut self, ctx: KeyCtx, code: KeyCode) -> Action {
        let (vim, emacs) = (self.set == KeySet::Vim, self.set == KeySet::Emacs);
        let regular = self.set == KeySet::Regular;
        let line = in_line(ctx);
        let c = match code {
            KeyCode::Char(c) => c,
            KeyCode::PageDown if regular => return Action::NextTab,
            KeyCode::PageUp if regular => return Action::PrevTab,
            KeyCode::Enter if ctx == KeyCtx::Input => return Action::NewLine,
            KeyCode::Left if line => return Action::WordLeft,
            KeyCode::Right if line => return Action::WordRight,
            KeyCode::Backspace if line => return Action::DeleteWord,
            _ => return Action::None,
        };
        match c {
            'c' => Action::Interrupt,
            'g' => Action::Back,
            'n' => Action::Next,
            'p' if vim && matches!(ctx, KeyCtx::Normal | KeyCtx::Input) => Action::Palette,
            'p' if regular && ctx == KeyCtx::Input => Action::Palette,
            'p' if regular && ctx == KeyCtx::Menu => Action::OpenBar,
            'p' => Action::Prev,
            'a' if line => Action::Home,
            'e' if line => Action::End,
            'u' if line => Action::DeleteToStart,
            // Raw mode reads a line feed as ctrl-j, in every terminal.
            'j' if ctx == KeyCtx::Input => Action::NewLine,
            'w' if !emacs && matches!(ctx, KeyCtx::Input | KeyCtx::SearchLine) => {
                Action::DeleteWord
            }
            'w' if vim => self.wait_for(Prefix::CtrlW),
            'x' if emacs => self.wait_for(Prefix::CtrlX),
            's' if ctx == KeyCtx::Filter => Action::Save,
            's' if emacs && ctx == KeyCtx::Input => Action::OpenSearch,
            's' if emacs && ctx == KeyCtx::SearchLine => Action::SearchNext,
            'r' if emacs && ctx == KeyCtx::SearchLine => Action::SearchPrev,
            'f' if regular && ctx == KeyCtx::Input => Action::OpenSearch,
            'o' if regular => Action::NextPane,
            't' if regular => Action::NewTab,
            'b' if regular => Action::ToggleSessions,
            'r' if regular => Action::ToggleSidebar,
            'q' if regular => Action::Quit,
            _ => Action::None,
        }
    }

    fn character(&mut self, ctx: KeyCtx, c: char) -> Action {
        match (ctx, c) {
            (KeyCtx::Input | KeyCtx::Filter | KeyCtx::SearchLine, _) => Action::Insert(c),
            (KeyCtx::Normal, 'i') => Action::EnterInput,
            (KeyCtx::Menu, 'i') if self.set == KeySet::Vim => Action::EnterInput,
            (KeyCtx::Normal, 'g') => self.wait_for(Prefix::G),
            (KeyCtx::Normal, 'h') => Action::MoveFocus(Dir::Left),
            (KeyCtx::Normal, 'l') => Action::MoveFocus(Dir::Right),
            (KeyCtx::Normal | KeyCtx::Menu, '[') if self.set == KeySet::Vim => {
                Action::ToggleSessions
            }
            (KeyCtx::Normal | KeyCtx::Menu, ']') if self.set == KeySet::Vim => {
                Action::ToggleSidebar
            }
            (KeyCtx::Normal, '/') => Action::OpenSearch,
            (KeyCtx::Normal, '?') => Action::OpenKeys,
            (KeyCtx::Normal, 'n') => Action::SearchNext,
            (KeyCtx::Normal, 'N') => Action::SearchPrev,
            (KeyCtx::Normal, 'G') => Action::Bottom,
            (KeyCtx::Normal, 'y') => Action::Accept,
            (KeyCtx::Normal | KeyCtx::Menu, 's') if self.set == KeySet::Vim => Action::Stop,
            (KeyCtx::Menu, 'd') => Action::SecondAct,
            (KeyCtx::Menu, '.') => Action::More,
            (KeyCtx::Normal, ':') => Action::OpenBar,
            (KeyCtx::Menu, ':') if self.set == KeySet::Vim => Action::OpenBar,
            (KeyCtx::Normal | KeyCtx::Menu, 'j') => Action::Next,
            (KeyCtx::Normal | KeyCtx::Menu, 'k') => Action::Prev,
            _ => Action::None,
        }
    }

    fn wait_for(&mut self, prefix: Prefix) -> Action {
        self.pending = Some(prefix);
        Action::None
    }
}

/// Without the keyboard protocol, ctrl-m, ctrl-i and ctrl-[ are enter's, tab's and esc's own
/// bytes; with it they arrive as themselves, and keep meaning those keys.
fn legacy(key: KeyEvent) -> KeyEvent {
    let code = match key.code {
        _ if key.modifiers != KeyModifiers::CONTROL => return key,
        KeyCode::Char('m') => KeyCode::Enter,
        KeyCode::Char('i') => KeyCode::Tab,
        KeyCode::Char('[') => KeyCode::Esc,
        _ => return key,
    };
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// A box that takes typed text. macOS terminals send readline keys for Cmd-Backspace and
/// Cmd or Option with the arrows (Ghostty's defaults), so those edit it in every key set, as
/// those keys themselves do where the terminal reports them.
pub fn in_line(ctx: KeyCtx) -> bool {
    matches!(ctx, KeyCtx::Input | KeyCtx::Filter | KeyCtx::SearchLine)
}

/// With Cmd, the arrows and Backspace take the line, as in macOS text boxes.
fn edit_key(code: KeyCode, cmd: bool) -> Action {
    match code {
        KeyCode::Backspace if cmd => Action::DeleteToStart,
        KeyCode::Left if cmd => Action::Home,
        KeyCode::Right if cmd => Action::End,
        KeyCode::Backspace => Action::Backspace,
        KeyCode::Delete => Action::Delete,
        KeyCode::Left => Action::CursorLeft,
        KeyCode::Right => Action::CursorRight,
        KeyCode::Home => Action::Home,
        KeyCode::End => Action::End,
        _ => Action::None,
    }
}

#[cfg(test)]
mod tests {
    use super::Action as A;
    use super::KeyCtx::{Filter, Input, Menu, Normal, SearchLine};
    use super::*;
    use crate::testing::keys;

    #[rustfmt::skip]
    const VIM: &[(KeyCtx, &str, Action)] = &[
        (Normal, "i", A::EnterInput), (Normal, "ctrl-w h", A::MoveFocus(Dir::Left)), (Normal, "ctrl-w j", A::MoveFocus(Dir::Down)),
        (Normal, "ctrl-w k", A::MoveFocus(Dir::Up)), (Normal, "ctrl-w l", A::MoveFocus(Dir::Right)), (Normal, "g t", A::NextTab), (Normal, "g T", A::PrevTab),
        (Normal, "ctrl-w q", A::ClosePane), (Normal, "ctrl-w [", A::ToggleSessions), (Normal, "ctrl-w ]", A::ToggleSidebar),
        (Normal, ":", A::OpenBar), (Normal, "/", A::OpenSearch), (Normal, "?", A::OpenKeys),
        (Normal, "j", A::Next), (Normal, "k", A::Prev), (Normal, "ctrl-n", A::Next), (Normal, "ctrl-p", A::Palette), (Normal, "down", A::Next), (Normal, "up", A::Prev),
        (Normal, "h", A::MoveFocus(Dir::Left)), (Normal, "l", A::MoveFocus(Dir::Right)),
        (Normal, "[", A::ToggleSessions), (Normal, "]", A::ToggleSidebar), (Menu, "[", A::ToggleSessions), (Menu, "]", A::ToggleSidebar),
        (Input, "ctrl-p", A::Palette), (Input, "h", A::Insert('h')), (Input, "[", A::Insert('[')), (Filter, "]", A::Insert(']')),
        (Menu, "ctrl-p", A::Prev), (Filter, "ctrl-p", A::Prev), (SearchLine, "ctrl-p", A::Prev),
        (Normal, "enter", A::Submit), (Normal, "esc", A::Back), (Normal, "ctrl-g", A::Back), (Normal, "ctrl-c", A::Interrupt),
        (Normal, "n", A::SearchNext), (Normal, "N", A::SearchPrev), (Normal, "y", A::Accept), (Normal, "a", A::None), (Normal, "s", A::Stop),
        (Input, "x", A::Insert('x')), (Input, "T", A::Insert('T')), (Input, "/", A::Insert('/')), (Input, "bs", A::Backspace), (Input, "enter", A::Submit), (Input, "esc", A::Back),
        (Input, "tab", A::Complete), (Input, "ctrl-c", A::Interrupt), (Input, "ctrl-w", A::DeleteWord), (Input, "ctrl-n", A::Next), (Input, "left", A::CursorLeft),
        (Input, "right", A::CursorRight), (Input, "home", A::Home), (Input, "end", A::End), (Input, "del", A::Delete),
        (Menu, "j", A::Next), (Menu, "k", A::Prev), (Menu, ":", A::OpenBar), (Menu, "d", A::SecondAct), (Menu, ".", A::More), (Menu, "enter", A::Submit), (Menu, "esc", A::Back), (Menu, "ctrl-w l", A::MoveFocus(Dir::Right)),
        (Menu, "ctrl-c", A::Interrupt), (Menu, "ctrl-w ]", A::ToggleSidebar), (Filter, "j", A::Insert('j')), (Filter, "bs", A::Backspace), (Filter, "ctrl-c", A::Interrupt), (Filter, "ctrl-n", A::Next), (Filter, "ctrl-p", A::Prev), (Filter, "up", A::Prev),
        (Filter, "ctrl-w h", A::MoveFocus(Dir::Left)), (Filter, "enter", A::Submit), (Filter, "tab", A::Complete),
        (Filter, "shift-tab", A::CompleteBack), (Filter, "ctrl-s", A::Save), (Input, "shift-tab", A::None), (Input, "ctrl-s", A::None), (Menu, "ctrl-s", A::None),
        (Normal, "ctrl-w ctrl-h", A::MoveFocus(Dir::Left)), (Normal, "ctrl-w ctrl-j", A::MoveFocus(Dir::Down)), (Normal, "ctrl-w ctrl-k", A::MoveFocus(Dir::Up)), (Normal, "ctrl-w ctrl-l", A::MoveFocus(Dir::Right)),
        (Normal, "ctrl-w ctrl-q", A::None), (Menu, "ctrl-w ctrl-l", A::MoveFocus(Dir::Right)), (Filter, "ctrl-w ctrl-h", A::MoveFocus(Dir::Left)), (Input, "ctrl-a", A::Home), (Input, "ctrl-e", A::End),
        (SearchLine, "x", A::Insert('x')), (SearchLine, "bs", A::Backspace), (SearchLine, "ctrl-c", A::Interrupt), (SearchLine, "enter", A::Submit), (SearchLine, "esc", A::Back), (SearchLine, "ctrl-g", A::Back), (SearchLine, "ctrl-w", A::DeleteWord),
        (Input, "ctrl-u", A::DeleteToStart), (Filter, "ctrl-u", A::DeleteToStart), (SearchLine, "ctrl-u", A::DeleteToStart), (Filter, "ctrl-a", A::Home), (Filter, "ctrl-e", A::End), (SearchLine, "ctrl-a", A::Home), (SearchLine, "ctrl-e", A::End),
        (Input, "alt-b", A::WordLeft), (Input, "alt-f", A::WordRight), (Filter, "alt-b", A::WordLeft), (Filter, "alt-f", A::WordRight), (SearchLine, "alt-b", A::WordLeft), (SearchLine, "alt-f", A::WordRight),
        (Normal, "ctrl-u", A::None), (Normal, "ctrl-a", A::None), (Normal, "ctrl-e", A::None), (Menu, "ctrl-u", A::None), (Normal, "alt-b", A::None), (Menu, "alt-f", A::None),
        (Menu, "pgup", A::PageUp), (Menu, "pgdn", A::PageDown), (Filter, "pgup", A::None), (Menu, "s", A::Stop),
        (Normal, "ctrl-m", A::Submit), (Normal, "ctrl-[", A::Back), (Normal, "ctrl-i", A::None), (Normal, "ctrl-w ctrl-m", A::None),
        (Normal, "shift-enter", A::Submit), (Normal, "ctrl-j", A::None), (Normal, "alt-left", A::None), (Normal, "super-left", A::None), (Normal, "ctrl-bs", A::None),
        (Normal, "g g", A::Top), (Normal, "G", A::Bottom), (Input, "G", A::Insert('G')), (Menu, "G", A::None),
    ];

    /// The message box's line breaks and every line's text-editing keys, the same in each set.
    #[rustfmt::skip]
    const EVERY_SET: &[(KeyCtx, &str, Action)] = &[
        (Input, "shift-enter", A::NewLine), (Input, "alt-enter", A::NewLine), (Input, "ctrl-enter", A::NewLine), (Input, "ctrl-j", A::NewLine),
        (Filter, "shift-enter", A::Submit), (SearchLine, "shift-enter", A::Submit), (Menu, "shift-enter", A::Submit),
        (Filter, "ctrl-j", A::None), (SearchLine, "ctrl-j", A::None), (Menu, "ctrl-j", A::None), (Filter, "alt-enter", A::None), (SearchLine, "ctrl-enter", A::None),
        (Input, "alt-left", A::WordLeft), (Input, "alt-right", A::WordRight), (Input, "ctrl-left", A::WordLeft), (Input, "ctrl-right", A::WordRight),
        (Input, "super-left", A::Home), (Input, "super-right", A::End), (Input, "alt-bs", A::DeleteWord), (Input, "ctrl-bs", A::DeleteWord), (Input, "super-bs", A::DeleteToStart),
        (Filter, "alt-left", A::WordLeft), (Filter, "alt-right", A::WordRight), (Filter, "ctrl-left", A::WordLeft), (Filter, "ctrl-right", A::WordRight),
        (Filter, "super-left", A::Home), (Filter, "super-right", A::End), (Filter, "alt-bs", A::DeleteWord), (Filter, "ctrl-bs", A::DeleteWord), (Filter, "super-bs", A::DeleteToStart),
        (SearchLine, "alt-left", A::WordLeft), (SearchLine, "alt-right", A::WordRight), (SearchLine, "ctrl-left", A::WordLeft), (SearchLine, "ctrl-right", A::WordRight),
        (SearchLine, "super-left", A::Home), (SearchLine, "super-right", A::End), (SearchLine, "alt-bs", A::DeleteWord), (SearchLine, "ctrl-bs", A::DeleteWord), (SearchLine, "super-bs", A::DeleteToStart),
        (Input, "home", A::Home), (Input, "end", A::End), (Input, "ctrl-a", A::Home), (Input, "ctrl-e", A::End), (Input, "ctrl-u", A::DeleteToStart), (Input, "alt-b", A::WordLeft), (Input, "alt-f", A::WordRight),
        (Menu, "alt-left", A::None), (Menu, "ctrl-right", A::None), (Menu, "super-left", A::None), (Menu, "ctrl-bs", A::None), (Menu, "super-bs", A::None),
        (Input, "ctrl-[", A::Back), (Filter, "ctrl-[", A::Back), (Menu, "ctrl-[", A::Back), (SearchLine, "ctrl-[", A::Back),
        (Input, "ctrl-m", A::Submit), (Filter, "ctrl-m", A::Submit), (Menu, "ctrl-m", A::Submit), (SearchLine, "ctrl-m", A::Submit),
        (Input, "ctrl-i", A::Complete), (Filter, "ctrl-i", A::Complete), (Menu, "ctrl-i", A::None), (SearchLine, "ctrl-i", A::None),
    ];

    #[rustfmt::skip]
    const EMACS: &[(KeyCtx, &str, Action)] = &[
        (Input, "x", A::Insert('x')), (Input, "y", A::Insert('y')), (Input, "/", A::Insert('/')), (Input, "ctrl-x o", A::NextPane), (Input, "ctrl-x n", A::NextTab), (Input, "ctrl-x p", A::PrevTab),
        (Input, "ctrl-x t", A::NewTab), (Input, "ctrl-x 0", A::ClosePane), (Input, "ctrl-x [", A::ToggleSessions), (Input, "ctrl-x ]", A::ToggleSidebar),
        (Input, "alt-x", A::OpenBar), (Input, "ctrl-x x", A::OpenBar), (Input, "ctrl-s", A::OpenSearch), (Input, "ctrl-x ?", A::OpenKeys), (Input, "ctrl-g", A::Back), (Input, "esc", A::Back),
        (Input, "ctrl-n", A::Next), (Input, "ctrl-p", A::Prev), (Input, "enter", A::Submit), (Input, "ctrl-x ctrl-c", A::Quit), (Input, "ctrl-c", A::Interrupt), (Input, "tab", A::Complete),
        (Menu, "ctrl-n", A::Next), (Menu, "alt-x", A::OpenBar), (Menu, "ctrl-x x", A::OpenBar), (Menu, "ctrl-x o", A::NextPane), (Menu, "d", A::SecondAct), (Menu, ".", A::More), (Menu, "ctrl-g", A::Back),
        (Menu, "esc", A::Back), (Menu, "ctrl-x ]", A::ToggleSidebar), (Menu, "ctrl-x ctrl-c", A::Quit),
        (Filter, "j", A::Insert('j')), (Filter, "ctrl-n", A::Next), (Filter, "tab", A::Complete), (Filter, "ctrl-x o", A::NextPane), (Filter, "ctrl-x ctrl-c", A::Quit), (Filter, "alt-x", A::OpenBar), (Filter, "ctrl-x x", A::OpenBar),
        (Filter, "shift-tab", A::CompleteBack), (Filter, "ctrl-s", A::Save), (Menu, "ctrl-s", A::None),
        (SearchLine, "ctrl-s", A::SearchNext), (SearchLine, "ctrl-r", A::SearchPrev), (SearchLine, "enter", A::Submit), (SearchLine, "ctrl-g", A::Back), (SearchLine, "ctrl-x ctrl-c", A::Quit), (SearchLine, "alt-x", A::OpenBar), (SearchLine, "ctrl-x x", A::OpenBar),
        (Input, "ctrl-a", A::Home), (Input, "ctrl-e", A::End), (Filter, "ctrl-a", A::Home), (Filter, "ctrl-e", A::End), (Menu, "ctrl-a", A::None), (Input, "ctrl-w", A::None),
        (Input, "ctrl-u", A::DeleteToStart), (Filter, "ctrl-u", A::DeleteToStart), (SearchLine, "ctrl-u", A::DeleteToStart), (SearchLine, "ctrl-a", A::Home), (SearchLine, "ctrl-e", A::End),
        (Input, "alt-b", A::WordLeft), (Input, "alt-f", A::WordRight), (Filter, "alt-b", A::WordLeft), (Filter, "alt-f", A::WordRight), (SearchLine, "alt-b", A::WordLeft), (SearchLine, "alt-f", A::WordRight),
        (Menu, "ctrl-u", A::None), (Menu, "alt-b", A::None), (Menu, "alt-f", A::None),
        (Input, "alt-<", A::Top), (Input, "alt->", A::Bottom), (Input, "<", A::Insert('<')),
    ];

    #[rustfmt::skip]
    const REGULAR: &[(KeyCtx, &str, Action)] = &[
        (Input, "x", A::Insert('x')), (Input, "?", A::Insert('?')), (Input, "/", A::Insert('/')), (Input, "enter", A::Submit), (Input, "esc", A::Back), (Input, "ctrl-c", A::Interrupt), (Input, "tab", A::Complete),
        (Input, "ctrl-p", A::Palette), (Menu, "ctrl-p", A::OpenBar), (Filter, "ctrl-p", A::Prev), (Filter, "ctrl-n", A::Next),
        (Input, "ctrl-f", A::OpenSearch), (Menu, "ctrl-f", A::None),
        (SearchLine, "down", A::SearchNext), (SearchLine, "up", A::SearchPrev), (SearchLine, "enter", A::Submit), (SearchLine, "esc", A::Back), (SearchLine, "x", A::Insert('x')),
        (Input, "ctrl-o", A::NextPane), (Menu, "ctrl-o", A::NextPane), (Filter, "ctrl-o", A::NextPane),
        (Input, "ctrl-t", A::NewTab), (Input, "ctrl-pgdn", A::NextTab), (Input, "ctrl-pgup", A::PrevTab), (Menu, "ctrl-pgdn", A::NextTab),
        (Input, "ctrl-b", A::ToggleSessions), (Input, "ctrl-r", A::ToggleSidebar), (Menu, "ctrl-b", A::ToggleSessions), (Menu, "ctrl-r", A::ToggleSidebar),
        (Input, "ctrl-q", A::Quit), (Menu, "ctrl-q", A::Quit), (Filter, "ctrl-q", A::Quit),
        (Input, "up", A::Prev), (Input, "down", A::Next), (Input, "pgup", A::PageUp), (Input, "pgdn", A::PageDown), (Menu, "pgup", A::PageUp),
        (Menu, "up", A::Prev), (Menu, "down", A::Next), (Menu, "enter", A::Submit), (Menu, "esc", A::Back), (Menu, "d", A::SecondAct), (Menu, ".", A::More), (Menu, ":", A::None), (Menu, "[", A::None),
        (Input, "ctrl-a", A::Home), (Input, "ctrl-e", A::End), (Input, "ctrl-u", A::DeleteToStart), (Input, "ctrl-w", A::DeleteWord), (SearchLine, "ctrl-w", A::DeleteWord), (Input, "alt-b", A::WordLeft), (Input, "alt-f", A::WordRight),
        (Filter, "ctrl-s", A::Save), (Filter, "shift-tab", A::CompleteBack), (Input, "ctrl-x", A::None), (Input, "alt-x", A::None), (Input, "ctrl-s", A::None),
    ];

    /// Earlier keys of a sequence return `None`; the action comes from the last.
    fn check(set: KeySet, table: &[(KeyCtx, &str, Action)]) {
        for &(ctx, sequence, want) in table {
            let mut map = Keymap::new(set);
            let events = keys(sequence);
            let (last, first) = events.split_last().expect("a key");
            for key in first {
                assert_eq!(map.map(ctx, *key), A::None, "{ctx:?} {sequence}");
            }
            assert_eq!(map.map(ctx, *last), want, "{ctx:?} {sequence}");
        }
    }

    fn map_all(map: &mut Keymap, ctx: KeyCtx, sequence: &str) -> Vec<Action> {
        keys(sequence)
            .into_iter()
            .map(|key| map.map(ctx, key))
            .collect()
    }

    #[test]
    fn vim_table() {
        check(KeySet::Vim, VIM);
        check(KeySet::Vim, EVERY_SET);
    }

    #[test]
    fn emacs_table() {
        check(KeySet::Emacs, EMACS);
        check(KeySet::Emacs, EVERY_SET);
    }

    #[test]
    fn regular_table() {
        check(KeySet::Regular, REGULAR);
        check(KeySet::Regular, EVERY_SET);
    }

    #[test]
    fn unknown_second_key_cancels_the_chord() {
        let mut map = Keymap::new(KeySet::Vim);
        assert_eq!(
            map_all(&mut map, Normal, "ctrl-w x j"),
            [A::None, A::None, A::Next]
        );
        assert_eq!(
            map_all(&mut map, Normal, "g x j"),
            [A::None, A::None, A::Next]
        );
        assert_eq!(map_all(&mut map, Normal, "ctrl-w esc"), [A::None, A::None]);
        assert_eq!(
            map_all(&mut map, Normal, "ctrl-x o j"),
            [A::None, A::None, A::Next]
        );

        let mut map = Keymap::new(KeySet::Emacs);
        assert_eq!(
            map_all(&mut map, Input, "ctrl-x z x"),
            [A::None, A::None, A::Insert('x')]
        );
    }

    #[test]
    fn chord_prefix_has_no_timeout() {
        let mut map = Keymap::new(KeySet::Vim);
        assert_eq!(map.map(Normal, keys("ctrl-w")[0]), A::None);
        // No clock in `Keymap`: the prefix waits however long the next key takes.
        assert_eq!(map.map(Normal, keys("h")[0]), A::MoveFocus(Dir::Left));
    }

    #[test]
    fn a_pending_prefix_is_named_until_the_next_key() {
        let mut map = Keymap::new(KeySet::Vim);
        assert_eq!(map.pending(), None);
        map.map(Normal, keys("ctrl-w")[0]);
        assert_eq!(map.pending(), Some("ctrl-w ..."));
        map.map(Normal, keys("z")[0]);
        assert_eq!(map.pending(), None);
        map.map(Normal, keys("g")[0]);
        assert_eq!(map.pending(), Some("g ..."));

        let mut map = Keymap::new(KeySet::Vim);
        map.map(Input, keys("ctrl-w")[0]);
        assert_eq!(map.pending(), None);

        let mut map = Keymap::new(KeySet::Emacs);
        map.map(Input, keys("ctrl-x")[0]);
        assert_eq!(map.pending(), Some("ctrl-x ..."));
    }

    #[test]
    fn ctrl_x_is_not_a_prefix_in_vim() {
        let mut map = Keymap::new(KeySet::Vim);
        assert_eq!(
            map_all(&mut map, Normal, "ctrl-x ctrl-c"),
            [A::None, A::Interrupt]
        );
    }

    #[test]
    fn modified_chars_are_not_text() {
        for set in [KeySet::Vim, KeySet::Emacs] {
            let mut map = Keymap::new(set);
            assert_eq!(map.map(Input, keys("ctrl-q")[0]), A::None);
            assert_eq!(map.map(Input, keys("alt-y")[0]), A::None);
        }
    }

    #[test]
    fn g_is_a_prefix_only_in_vim_normal() {
        let mut map = Keymap::new(KeySet::Vim);
        assert_eq!(map.map(Input, keys("g")[0]), A::Insert('g'));
        assert_eq!(map.map(Filter, keys("g")[0]), A::Insert('g'));
        assert_eq!(map.map(Menu, keys("g")[0]), A::None);
        assert_eq!(map.map(Input, keys("t")[0]), A::Insert('t'));
    }

    #[test]
    fn plain_chars_do_nothing_in_a_menu_or_normal_mode() {
        let mut map = Keymap::new(KeySet::Vim);
        assert_eq!(map.map(Menu, keys("x")[0]), A::None);
        assert_eq!(map.map(Normal, keys("x")[0]), A::None);
        let mut map = Keymap::new(KeySet::Emacs);
        assert_eq!(map.map(Menu, keys("x")[0]), A::None);
        assert_eq!(map.map(Menu, keys(":")[0]), A::None);
    }
}
