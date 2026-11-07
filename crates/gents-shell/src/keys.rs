use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySet {
    Vim,
    Emacs,
}

/// Sample copy names `esc`, which means back in both key sets; emacs people are shown theirs.
pub fn in_key_set(set: KeySet, text: &str) -> String {
    match set {
        KeySet::Vim => text.to_string(),
        KeySet::Emacs => text.replace("esc", "ctrl-g"),
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
    SearchNext,
    SearchPrev,
    Complete,
    /// `y`: the shown prompt's main answer.
    Accept,
    /// `d` in a menu: the selected row's other act, beside enter's.
    SecondAct,
    Quit,
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
        if let Some(prefix) = self.pending.take() {
            return Self::chord(prefix, key);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return self.control(ctx, key.code);
        }
        if key.modifiers.contains(KeyModifiers::ALT) {
            return match key.code {
                KeyCode::Char('x') if self.set == KeySet::Emacs => Action::OpenBar,
                KeyCode::Char('b') if in_line(ctx) => Action::WordLeft,
                KeyCode::Char('f') if in_line(ctx) => Action::WordRight,
                _ => Action::None,
            };
        }
        match key.code {
            KeyCode::Esc => Action::Back,
            KeyCode::Enter => Action::Submit,
            KeyCode::Down => Action::Next,
            KeyCode::Up => Action::Prev,
            KeyCode::PageUp if matches!(ctx, KeyCtx::Normal | KeyCtx::Input) => Action::PageUp,
            KeyCode::PageDown if matches!(ctx, KeyCtx::Normal | KeyCtx::Input) => Action::PageDown,
            KeyCode::Char(c) => self.character(ctx, c),
            KeyCode::Tab if matches!(ctx, KeyCtx::Input | KeyCtx::Filter) => Action::Complete,
            code if in_line(ctx) => edit_key(code),
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
        let KeyCode::Char(c) = code else {
            return Action::None;
        };
        let emacs = self.set == KeySet::Emacs;
        let line = in_line(ctx);
        match c {
            'c' => Action::Interrupt,
            'g' => Action::Back,
            'n' => Action::Next,
            'p' if !emacs && matches!(ctx, KeyCtx::Normal | KeyCtx::Input) => Action::Palette,
            'p' => Action::Prev,
            'a' if line => Action::Home,
            'e' if line => Action::End,
            'u' if line => Action::DeleteToStart,
            'w' if !emacs && matches!(ctx, KeyCtx::Input | KeyCtx::SearchLine) => {
                Action::DeleteWord
            }
            'w' if !emacs => self.wait_for(Prefix::CtrlW),
            'x' if emacs => self.wait_for(Prefix::CtrlX),
            's' if emacs && ctx == KeyCtx::Input => Action::OpenSearch,
            's' if emacs && ctx == KeyCtx::SearchLine => Action::SearchNext,
            'r' if emacs && ctx == KeyCtx::SearchLine => Action::SearchPrev,
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
            (KeyCtx::Normal, 'y') => Action::Accept,
            (KeyCtx::Normal, 's') if self.set == KeySet::Vim => Action::Stop,
            (KeyCtx::Menu, 'd') => Action::SecondAct,
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

/// A box that takes typed text. macOS terminals send readline keys for Cmd-Backspace and
/// Cmd or Option with the arrows (Ghostty's defaults), so those edit it in both key sets.
pub fn in_line(ctx: KeyCtx) -> bool {
    matches!(ctx, KeyCtx::Input | KeyCtx::Filter | KeyCtx::SearchLine)
}

fn edit_key(code: KeyCode) -> Action {
    match code {
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
        (Menu, "j", A::Next), (Menu, "k", A::Prev), (Menu, ":", A::OpenBar), (Menu, "d", A::SecondAct), (Menu, "enter", A::Submit), (Menu, "esc", A::Back), (Menu, "ctrl-w l", A::MoveFocus(Dir::Right)),
        (Menu, "ctrl-c", A::Interrupt), (Menu, "ctrl-w ]", A::ToggleSidebar), (Filter, "j", A::Insert('j')), (Filter, "bs", A::Backspace), (Filter, "ctrl-c", A::Interrupt), (Filter, "ctrl-n", A::Next), (Filter, "ctrl-p", A::Prev), (Filter, "up", A::Prev),
        (Filter, "ctrl-w h", A::MoveFocus(Dir::Left)), (Filter, "enter", A::Submit), (Filter, "tab", A::Complete),
        (Normal, "ctrl-w ctrl-h", A::MoveFocus(Dir::Left)), (Normal, "ctrl-w ctrl-j", A::MoveFocus(Dir::Down)), (Normal, "ctrl-w ctrl-k", A::MoveFocus(Dir::Up)), (Normal, "ctrl-w ctrl-l", A::MoveFocus(Dir::Right)),
        (Normal, "ctrl-w ctrl-q", A::None), (Menu, "ctrl-w ctrl-l", A::MoveFocus(Dir::Right)), (Filter, "ctrl-w ctrl-h", A::MoveFocus(Dir::Left)), (Input, "ctrl-a", A::Home), (Input, "ctrl-e", A::End),
        (SearchLine, "x", A::Insert('x')), (SearchLine, "bs", A::Backspace), (SearchLine, "ctrl-c", A::Interrupt), (SearchLine, "enter", A::Submit), (SearchLine, "esc", A::Back), (SearchLine, "ctrl-g", A::Back), (SearchLine, "ctrl-w", A::DeleteWord),
        (Input, "ctrl-u", A::DeleteToStart), (Filter, "ctrl-u", A::DeleteToStart), (SearchLine, "ctrl-u", A::DeleteToStart), (Filter, "ctrl-a", A::Home), (Filter, "ctrl-e", A::End), (SearchLine, "ctrl-a", A::Home), (SearchLine, "ctrl-e", A::End),
        (Input, "alt-b", A::WordLeft), (Input, "alt-f", A::WordRight), (Filter, "alt-b", A::WordLeft), (Filter, "alt-f", A::WordRight), (SearchLine, "alt-b", A::WordLeft), (SearchLine, "alt-f", A::WordRight),
        (Normal, "ctrl-u", A::None), (Normal, "ctrl-a", A::None), (Normal, "ctrl-e", A::None), (Menu, "ctrl-u", A::None), (Normal, "alt-b", A::None), (Menu, "alt-f", A::None),
    ];

    #[rustfmt::skip]
    const EMACS: &[(KeyCtx, &str, Action)] = &[
        (Input, "x", A::Insert('x')), (Input, "y", A::Insert('y')), (Input, "/", A::Insert('/')), (Input, "ctrl-x o", A::NextPane), (Input, "ctrl-x n", A::NextTab), (Input, "ctrl-x p", A::PrevTab),
        (Input, "ctrl-x t", A::NewTab), (Input, "ctrl-x 0", A::ClosePane), (Input, "ctrl-x [", A::ToggleSessions), (Input, "ctrl-x ]", A::ToggleSidebar),
        (Input, "alt-x", A::OpenBar), (Input, "ctrl-x x", A::OpenBar), (Input, "ctrl-s", A::OpenSearch), (Input, "ctrl-x ?", A::OpenKeys), (Input, "ctrl-g", A::Back), (Input, "esc", A::Back),
        (Input, "ctrl-n", A::Next), (Input, "ctrl-p", A::Prev), (Input, "enter", A::Submit), (Input, "ctrl-x ctrl-c", A::Quit), (Input, "ctrl-c", A::Interrupt), (Input, "tab", A::Complete),
        (Menu, "ctrl-n", A::Next), (Menu, "alt-x", A::OpenBar), (Menu, "ctrl-x x", A::OpenBar), (Menu, "ctrl-x o", A::NextPane), (Menu, "d", A::SecondAct), (Menu, "ctrl-g", A::Back),
        (Menu, "esc", A::Back), (Menu, "ctrl-x ]", A::ToggleSidebar), (Menu, "ctrl-x ctrl-c", A::Quit),
        (Filter, "j", A::Insert('j')), (Filter, "ctrl-n", A::Next), (Filter, "tab", A::Complete), (Filter, "ctrl-x o", A::NextPane), (Filter, "ctrl-x ctrl-c", A::Quit), (Filter, "alt-x", A::OpenBar), (Filter, "ctrl-x x", A::OpenBar),
        (SearchLine, "ctrl-s", A::SearchNext), (SearchLine, "ctrl-r", A::SearchPrev), (SearchLine, "enter", A::Submit), (SearchLine, "ctrl-g", A::Back), (SearchLine, "ctrl-x ctrl-c", A::Quit), (SearchLine, "alt-x", A::OpenBar), (SearchLine, "ctrl-x x", A::OpenBar),
        (Input, "ctrl-a", A::Home), (Input, "ctrl-e", A::End), (Filter, "ctrl-a", A::Home), (Filter, "ctrl-e", A::End), (Menu, "ctrl-a", A::None), (Input, "ctrl-w", A::None),
        (Input, "ctrl-u", A::DeleteToStart), (Filter, "ctrl-u", A::DeleteToStart), (SearchLine, "ctrl-u", A::DeleteToStart), (SearchLine, "ctrl-a", A::Home), (SearchLine, "ctrl-e", A::End),
        (Input, "alt-b", A::WordLeft), (Input, "alt-f", A::WordRight), (Filter, "alt-b", A::WordLeft), (Filter, "alt-f", A::WordRight), (SearchLine, "alt-b", A::WordLeft), (SearchLine, "alt-f", A::WordRight),
        (Menu, "ctrl-u", A::None), (Menu, "alt-b", A::None), (Menu, "alt-f", A::None),
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
    }

    #[test]
    fn emacs_table() {
        check(KeySet::Emacs, EMACS);
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
