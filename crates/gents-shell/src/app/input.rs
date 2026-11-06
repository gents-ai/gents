use ratatui::style::Style;
use ratatui::text::Span;

use crate::keys::Action;

/// `cursor` is a byte offset. It moves by grapheme cluster, the unit `Buffer` draws, so one
/// backspace removes what one looks like. Control characters are refused: they are not drawn,
/// and `clusters` skips them, which would put its lengths out of step with the bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineInput {
    text: String,
    cursor: usize,
}

impl LineInput {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn insert(&mut self, c: char) {
        if !c.is_control() {
            self.text.insert(self.cursor, c);
            self.cursor += c.len_utf8();
        }
    }

    pub fn backspace(&mut self) {
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

/// A cluster's first character carries its class: a combining mark after a letter is part of it.
fn is_word(cluster: &str) -> bool {
    cluster.chars().next().is_some_and(char::is_alphanumeric)
}

fn clusters(text: &str) -> Vec<(usize, usize)> {
    Span::raw(text)
        .styled_graphemes(Style::new())
        .map(|grapheme| (grapheme.symbol.len(), Span::raw(grapheme.symbol).width()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
