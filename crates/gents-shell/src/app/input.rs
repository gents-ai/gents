use ratatui::style::Style;
use ratatui::text::Span;

use crate::keys::Action;

/// `cursor` is a byte offset. It moves by grapheme cluster, the unit `Buffer` draws, so one
/// backspace removes what one looks like. Control characters but the tab are refused: they are
/// not drawn, and `clusters` skips them, which would put its lengths out of step with the bytes.
/// A tab is kept, so a loaded value saves unchanged, and is one cluster drawn as `TAB`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineInput {
    text: String,
    cursor: usize,
    /// Each long paste's badge, as `text` shows it, and the text it stands for.
    pastes: Vec<(String, String)>,
}

/// A paste of one line up to this many characters is typed in; a longer one, or one of more
/// lines, shows as a badge, because the box is one row.
const INLINE_PASTE_CHARS: usize = 400;

/// How a tab is drawn.
pub const TAB: &str = "    ";

impl LineInput {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn insert(&mut self, c: char) {
        if c == '\t' || !c.is_control() {
            self.text.insert(self.cursor, c);
            self.cursor += c.len_utf8();
        }
    }

    /// A terminal's bracketed paste: typed in when it is short and one line, else a badge.
    pub fn paste(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let text = text.trim_end_matches('\n');
        let lines = text.lines().count();
        let chars = text.chars().count();
        if lines <= 1 && chars <= INLINE_PASTE_CHARS {
            return self.paste_inline(text);
        }
        let n = self.pastes.len() + 1;
        let badge = match lines {
            1 => format!("[Pasted text #{n} {chars} chars]"),
            _ => format!("[Pasted text #{n} +{lines} lines]"),
        };
        self.text.insert_str(self.cursor, &badge);
        self.cursor += badge.len();
        self.pastes.push((badge, text.to_string()));
    }

    /// A paste into a box that holds one line: line breaks and tabs become spaces.
    pub fn paste_inline(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n");
        for c in text.trim_end_matches(['\n', '\r']).chars() {
            match c {
                '\n' | '\r' | '\t' => self.insert(' '),
                c => self.insert(c),
            }
        }
    }

    /// What sending the box sends: each badge replaced by the text it stands for.
    pub fn sent(&self) -> String {
        self.pastes
            .iter()
            .fold(self.text.clone(), |text, (badge, pasted)| {
                text.replacen(badge.as_str(), pasted, 1)
            })
    }

    /// The badges the box shows, to draw them as badges.
    pub fn badges(&self) -> impl Iterator<Item = &str> {
        self.pastes.iter().map(|(badge, _)| badge.as_str())
    }

    /// Removes the cluster before the cursor, or the whole badge that ends there.
    pub fn backspace(&mut self) {
        let before = &self.text[..self.cursor];
        let badge = self.badges().find(|badge| before.ends_with(badge));
        if let Some(len) = badge.map(str::len) {
            let start = self.cursor - len;
            self.text.drain(start..self.cursor);
            self.cursor = start;
            return;
        }
        let end = self.cursor;
        self.left();
        self.text.drain(self.cursor..end);
    }

    pub fn delete(&mut self) {
        if let Some(&(len, _)) = clusters(&self.text[self.cursor..]).first() {
            self.text.drain(self.cursor..self.cursor + len);
        }
    }

    /// Back over spaces, then back to the next space, as `ctrl-w` does in readline.
    pub fn delete_word(&mut self) {
        let end = self.cursor;
        while self.text[..self.cursor].ends_with(char::is_whitespace) {
            self.left();
        }
        while self.cursor > 0 && !self.text[..self.cursor].ends_with(char::is_whitespace) {
            self.left();
        }
        self.text.drain(self.cursor..end);
    }

    pub fn delete_to_start(&mut self) {
        self.text.drain(..self.cursor);
        self.cursor = 0;
    }

    /// Back over what is not a word, then to the word's start, as readline's `backward-word`,
    /// whose words are letters and digits.
    pub fn word_left(&mut self) {
        while self.before().is_some_and(|cluster| !is_word(cluster)) {
            self.left();
        }
        while self.before().is_some_and(is_word) {
            self.left();
        }
    }

    /// On over what is not a word, then to the word's end, as readline's `forward-word`.
    pub fn word_right(&mut self) {
        while self.after().is_some_and(|cluster| !is_word(cluster)) {
            self.right();
        }
        while self.after().is_some_and(is_word) {
            self.right();
        }
    }

    fn before(&self) -> Option<&str> {
        let &(len, _) = clusters(&self.text[..self.cursor]).last()?;
        Some(&self.text[self.cursor - len..self.cursor])
    }

    fn after(&self) -> Option<&str> {
        let &(len, _) = clusters(&self.text[self.cursor..]).first()?;
        Some(&self.text[self.cursor..self.cursor + len])
    }

    pub fn left(&mut self) {
        if let Some(&(len, _)) = clusters(&self.text[..self.cursor]).last() {
            self.cursor -= len;
        }
    }

    pub fn right(&mut self) {
        if let Some(&(len, _)) = clusters(&self.text[self.cursor..]).first() {
            self.cursor += len;
        }
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.text.len();
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.pastes.clear();
    }

    pub fn set(&mut self, text: &str) {
        self.clear();
        text.chars().for_each(|c| self.insert(c));
    }

    pub fn edit(&mut self, action: Action) {
        match action {
            Action::Insert(c) => self.insert(c),
            Action::Backspace => self.backspace(),
            Action::Delete => self.delete(),
            Action::DeleteWord => self.delete_word(),
            Action::DeleteToStart => self.delete_to_start(),
            Action::CursorLeft => self.left(),
            Action::CursorRight => self.right(),
            Action::WordLeft => self.word_left(),
            Action::WordRight => self.word_right(),
            Action::Home => self.home(),
            Action::End => self.end(),
            _ => {}
        }
    }

    /// The part to show in `width` columns, and the cursor's column within it. The cursor
    /// always has a column of its own, so past the width it stays on the last one.
    pub fn view(&self, width: u16) -> (&str, u16) {
        let width = usize::from(width);
        let (mut start, mut column) = (self.cursor, 0);
        for (len, cell) in clusters(&self.text[..self.cursor]).into_iter().rev() {
            if column + cell >= width {
                break;
            }
            column += cell;
            start -= len;
        }
        let (mut end, mut used) = (self.cursor, column);
        for (len, cell) in clusters(&self.text[self.cursor..]) {
            used += cell;
            if used > width {
                break;
            }
            end += len;
        }
        (&self.text[start..end], column as u16)
    }
}

/// The rows a `Lines` box shows at most.
const LINES_SHOWN: usize = 6;

/// A box of many lines, never none. Enter breaks the line at the cursor, and Backspace at a
/// line's start joins it to the end of the one above.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lines {
    lines: Vec<LineInput>,
    row: usize,
}

impl Lines {
    pub fn new(text: &str) -> Lines {
        let lines = text
            .split('\n')
            .map(|line| {
                let mut input = LineInput::default();
                input.set(line);
                input
            })
            .collect();
        Lines { lines, row: 0 }
    }

    pub fn text(&self) -> String {
        let lines: Vec<&str> = self.lines.iter().map(LineInput::text).collect();
        lines.join("\n")
    }

    pub fn lines(&self) -> &[LineInput] {
        &self.lines
    }

    pub fn row(&self) -> usize {
        self.row
    }

    pub fn current(&self) -> &LineInput {
        &self.lines[self.row]
    }

    /// The rows shown, which keep the cursor's in view.
    pub fn shown(&self) -> std::ops::Range<usize> {
        let first = self.row.saturating_sub(LINES_SHOWN - 1);
        first..(first + LINES_SHOWN).min(self.lines.len())
    }

    pub fn break_line(&mut self) {
        let line = &mut self.lines[self.row];
        let rest = LineInput {
            text: line.text.split_off(line.cursor),
            cursor: 0,
            pastes: Vec::new(),
        };
        self.row += 1;
        self.lines.insert(self.row, rest);
    }

    fn backspace(&mut self) {
        if self.row == 0 || self.lines[self.row].cursor > 0 {
            return self.lines[self.row].backspace();
        }
        let line = self.lines.remove(self.row);
        self.row -= 1;
        let above = &mut self.lines[self.row];
        above.cursor = above.text.len();
        above.text.push_str(&line.text);
    }

    /// To the line below, or above, at the same character: false past the last or first.
    pub fn step(&mut self, down: bool) -> bool {
        let to = if down {
            self.row.checked_add(1)
        } else {
            self.row.checked_sub(1)
        };
        let Some(to) = to.filter(|&to| to < self.lines.len()) else {
            return false;
        };
        let from = &self.lines[self.row];
        let at = clusters(&from.text[..from.cursor]).len();
        let line = &mut self.lines[to];
        line.cursor = clusters(&line.text)
            .into_iter()
            .take(at)
            .map(|(len, _)| len)
            .sum();
        self.row = to;
        true
    }

    /// A paste keeps its line breaks and tabs.
    pub fn paste(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        for c in text.trim_end_matches('\n').chars() {
            match c {
                '\n' => self.break_line(),
                c => self.lines[self.row].insert(c),
            }
        }
    }

    /// At a line's end, joins the line below to it.
    fn delete(&mut self) {
        let line = &self.lines[self.row];
        if line.cursor < line.text.len() || self.row + 1 == self.lines.len() {
            return self.lines[self.row].delete();
        }
        let below = self.lines.remove(self.row + 1);
        self.lines[self.row].text.push_str(&below.text);
    }

    pub fn edit(&mut self, action: Action) {
        match action {
            Action::Backspace => self.backspace(),
            Action::Delete => self.delete(),
            action => self.lines[self.row].edit(action),
        }
    }
}

/// A cluster's first character carries its class: a combining mark after a letter is part of it.
fn is_word(cluster: &str) -> bool {
    cluster.chars().next().is_some_and(char::is_alphanumeric)
}

/// Each cluster's bytes and columns. A tab, which `styled_graphemes` skips, is always a cluster
/// of its own (UAX #29 breaks around controls), so the parts between tabs segment as the whole.
fn clusters(text: &str) -> Vec<(usize, usize)> {
    let mut clusters = Vec::new();
    for (index, part) in text.split('\t').enumerate() {
        if index > 0 {
            clusters.push((1, TAB.len()));
        }
        let part = Span::raw(part);
        let graphemes = part.styled_graphemes(Style::new());
        clusters.extend(
            graphemes.map(|grapheme| (grapheme.symbol.len(), Span::raw(grapheme.symbol).width())),
        );
    }
    clusters
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_paste_of_many_lines_is_one_badge_that_sends_its_text() {
        let mut i = LineInput::default();
        i.insert('s');
        i.insert(' ');
        i.paste("one\r\n\n  two\n");
        i.insert(' ');
        i.insert('!');
        assert_eq!(i.text(), "s [Pasted text #1 +3 lines] !");
        assert_eq!(i.sent(), "s one\n\n  two !");
    }

    #[test]
    fn backspace_removes_a_whole_badge() {
        let mut i = LineInput::default();
        i.paste("a\nb");
        i.backspace();
        assert!(i.is_empty());
        assert_eq!(i.sent(), "");
    }

    #[test]
    fn a_short_paste_is_typed_where_the_cursor_is() {
        let mut i = LineInput::default();
        i.paste("hello\tworld\n");
        assert_eq!(i.text(), "hello world");
        assert_eq!(i.sent(), "hello world");
    }

    #[test]
    fn a_long_line_is_a_badge_and_each_paste_has_its_number() {
        let mut i = LineInput::default();
        i.paste(&"x".repeat(401));
        i.paste("a\nb");
        assert_eq!(
            i.text(),
            "[Pasted text #1 401 chars][Pasted text #2 +2 lines]"
        );
        assert_eq!(i.sent(), format!("{}a\nb", "x".repeat(401)));
        i.clear();
        i.paste("c\nd");
        assert_eq!(i.text(), "[Pasted text #1 +2 lines]");
    }

    #[test]
    fn an_inline_paste_turns_line_breaks_into_spaces() {
        let mut i = LineInput::default();
        i.paste_inline("a\r\nb\tc\n");
        assert_eq!(i.text(), "a b c");
    }

    #[test]
    fn line_input_counts_characters_and_display_columns() {
        let mut i = LineInput::default();
        for c in "héllo日本".chars() {
            i.insert(c);
        }
        i.backspace();
        assert_eq!(i.text(), "héllo日");
        assert_eq!(i.view(40), ("héllo日", 7));
        i.home();
        i.right();
        i.delete();
        assert_eq!(i.text(), "hllo日");
    }

    #[test]
    fn long_input_keeps_the_cursor_in_the_box() {
        let mut i = LineInput::default();
        for _ in 0..200 {
            i.insert('x');
        }
        let (slice, column) = i.view(20);
        assert_eq!(column, 19);
        assert_eq!(slice, "x".repeat(19));

        i.home();
        let (slice, column) = i.view(20);
        assert_eq!(column, 0);
        assert_eq!(slice, "x".repeat(20));
    }

    #[test]
    fn wide_characters_stay_inside_the_width() {
        let mut i = LineInput::default();
        for _ in 0..30 {
            i.insert('日');
        }
        let (slice, column) = i.view(21);
        assert_eq!(column, 20);
        assert_eq!(slice, "日".repeat(10));

        i.home();
        let (slice, column) = i.view(21);
        assert_eq!(column, 0);
        assert_eq!(slice, "日".repeat(10));

        assert_eq!(i.view(0), ("", 0));
        assert_eq!(i.view(1).1, 0);
    }

    #[test]
    fn the_cursor_moves_by_whole_characters() {
        let mut i = LineInput::default();
        i.set("日本");
        assert_eq!(i.view(40).1, 4);
        i.left();
        assert_eq!(i.view(40).1, 2);
        i.backspace();
        assert_eq!(i.text(), "本");
        i.backspace();
        i.left();
        assert_eq!(i.text(), "本");
        i.end();
        i.right();
        i.delete();
        assert_eq!(i.view(40), ("本", 2));
        i.clear();
        assert!(i.is_empty());
    }

    #[test]
    fn inserts_at_the_cursor() {
        let mut i = LineInput::default();
        i.set("ac");
        i.left();
        i.insert('b');
        assert_eq!(i.text(), "abc");
        assert_eq!(i.view(40).1, 2);
    }

    #[test]
    fn a_variation_selector_counts_with_its_character() {
        let mut i = LineInput::default();
        for c in "a✔\u{fe0f}".chars() {
            i.insert(c);
        }
        assert_eq!(i.view(40), ("a✔\u{fe0f}", 3));
        assert_eq!(i.view(3), ("✔\u{fe0f}", 2));
        i.backspace();
        assert_eq!(i.text(), "a");
    }

    #[test]
    fn the_cursor_moves_over_a_joined_emoji_as_one() {
        let mut i = LineInput::default();
        i.set("👩\u{200d}💻x");
        i.left();
        assert_eq!(i.view(40).1, 2);
        i.left();
        assert_eq!(i.view(40).1, 0);
        i.delete();
        assert_eq!(i.text(), "x");
        i.home();
        i.right();
        assert_eq!(i.view(40).1, 1);
    }

    #[test]
    fn delete_word_takes_the_word_before_the_cursor() {
        let mut i = LineInput::default();
        i.set("one two  ");
        i.delete_word();
        assert_eq!(i.text(), "one ");
        i.delete_word();
        assert!(i.is_empty());
        i.delete_word();
        assert!(i.is_empty());

        i.set("one two");
        i.left();
        i.left();
        i.delete_word();
        assert_eq!(i.text(), "one wo");
        assert_eq!(i.view(40).1, 4);
    }

    #[test]
    fn delete_word_keeps_clusters_whole() {
        let mut i = LineInput::default();
        i.set("日本 👩\u{200d}💻x");
        i.delete_word();
        assert_eq!(i.text(), "日本 ");
        assert_eq!(i.view(40).1, 5);
        i.delete_word();
        assert!(i.is_empty());

        i.set("a \u{301}b");
        i.delete_word();
        assert!(i.is_empty());
    }

    #[test]
    fn delete_to_start_keeps_what_follows_the_cursor() {
        let mut i = LineInput::default();
        i.set("one two");
        (0..3).for_each(|_| i.left());
        i.delete_to_start();
        assert_eq!(i.text(), "two");
        assert_eq!(i.view(40).1, 0);
        i.end();
        i.delete_to_start();
        assert!(i.is_empty());
        i.delete_to_start();
        assert!(i.is_empty());
    }

    #[test]
    fn delete_to_start_keeps_clusters_whole() {
        let mut i = LineInput::default();
        i.set("日本 he\u{301}llo 👩\u{200d}💻x");
        i.left();
        i.delete_to_start();
        assert_eq!(i.view(40), ("x", 0));
    }

    /// Columns where each move stops, from the end going back, then from the start going on.
    fn word_stops(text: &str) -> (Vec<u16>, Vec<u16>) {
        let mut i = LineInput::default();
        i.set(text);
        let back = (0..6)
            .map(|_| {
                i.word_left();
                i.view(80).1
            })
            .collect();
        let on = (0..6)
            .map(|_| {
                i.word_right();
                i.view(80).1
            })
            .collect();
        (back, on)
    }

    #[test]
    fn word_moves_cross_punctuation_and_spaces_as_readline_does() {
        assert_eq!(
            word_stops("git  commit --amend, now"),
            (vec![21, 14, 5, 0, 0, 0], vec![3, 11, 19, 24, 24, 24])
        );
        assert_eq!(word_stops(""), (vec![0; 6], vec![0; 6]));
        assert_eq!(word_stops(" ,. "), (vec![0; 6], vec![4; 6]));
    }

    #[test]
    fn word_moves_keep_clusters_whole() {
        assert_eq!(
            word_stops("he\u{301}llo, wo\u{308}rld"),
            (vec![7, 0, 0, 0, 0, 0], vec![5, 12, 12, 12, 12, 12])
        );
        assert_eq!(
            word_stops("日本👩\u{200d}💻x"),
            (vec![6, 0, 0, 0, 0, 0], vec![4, 7, 7, 7, 7, 7])
        );
    }

    #[test]
    fn lines_move_at_the_same_character_and_show_six_around_the_cursor() {
        let mut l = Lines::new("日本語\nab\n\n3\n4\n5\n6\n7");
        l.lines[0].left();
        assert!(l.step(true));
        assert_eq!(l.current().view(40), ("ab", 2));
        assert!(l.step(false));
        assert_eq!(l.current().view(40), ("日本語", 4));
        assert!(!l.step(false));
        assert_eq!(l.shown(), 0..6);
        while l.step(true) {}
        assert_eq!((l.row(), l.shown()), (7, 2..8));
        assert_eq!(l.text(), "日本語\nab\n\n3\n4\n5\n6\n7");
    }

    #[test]
    fn a_tab_is_kept_and_its_cursor_steps_over_its_four_columns() {
        let mut l = Lines::new("\tx\ny");
        assert_eq!(l.text(), "\tx\ny");
        assert_eq!(l.current().view(40), ("\tx", 5));
        l.edit(Action::CursorLeft);
        assert_eq!(l.current().view(40).1, 4);
        l.edit(Action::CursorLeft);
        assert_eq!(l.current().view(40).1, 0);
        l.paste("a\tb");
        assert_eq!(l.text(), "a\tb\tx\ny");
        assert_eq!(l.current().view(40), ("a\tb\tx", 6));
    }

    #[test]
    fn delete_at_a_lines_end_joins_the_line_below() {
        let mut l = Lines::new("ab\ncd");
        l.edit(Action::Delete);
        assert_eq!(l.text(), "abcd");
        assert_eq!(l.current().view(40), ("abcd", 2));
        l.edit(Action::End);
        l.edit(Action::Delete);
        assert_eq!(l.text(), "abcd");
    }

    #[test]
    fn control_characters_are_not_text() {
        let mut i = LineInput::default();
        i.insert('\u{85}');
        i.set("a\u{85}b");
        assert_eq!(i.text(), "ab");
        i.left();
        i.left();
        i.backspace();
        assert_eq!(i.text(), "ab");
    }
}
