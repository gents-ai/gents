use ratatui::style::Style;
use ratatui::text::Span;

use crate::draw::agent::bubble_edges;
use crate::sample::Tone;

/// A drag in the conversation, each end a (row index, display column) of its rows, so
/// scrolling keeps it on its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: (usize, u16),
    pub end: (usize, u16),
}

impl Selection {
    /// Its ends in reading order.
    pub fn ordered(self) -> ((usize, u16), (usize, u16)) {
        (self.anchor.min(self.end), self.anchor.max(self.end))
    }

    /// What it covers of `rows`, through the end's column, each row without trailing spaces and
    /// your messages' rows without the padding that sets their bubble right.
    pub fn text(self, rows: &[(String, Tone)]) -> String {
        let (start, end) = self.ordered();
        let edges = bubble_edges(rows);
        let shown = rows.iter().enumerate().take(end.0 + 1).skip(start.0);
        let cut = shown.map(|(at, (row, _))| {
            let edge = u16::try_from(edges[at]).unwrap_or(u16::MAX);
            let from = if at == start.0 { start.1 } else { 0 }.max(edge);
            let to = if at == end.0 { end.1 + 1 } else { u16::MAX };
            columns(row, from, to).trim_end().to_string()
        });
        cut.collect::<Vec<_>>().join("\n")
    }
}

/// The graphemes of `text` with a cell in display columns `from..to`, so a wide one the
/// highlight covers half of is kept whole.
pub(crate) fn columns(text: &str, from: u16, to: u16) -> String {
    let mut at: u16 = 0;
    let mut kept = String::new();
    for grapheme in Span::raw(text).styled_graphemes(Style::new()) {
        let width = Span::raw(grapheme.symbol).width();
        let width = u16::try_from(width).unwrap_or(u16::MAX);
        if at < to && at.saturating_add(width.max(1)) > from {
            kept.push_str(grapheme.symbol);
        }
        at = at.saturating_add(width);
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_slice_by_display_width() {
        assert_eq!(columns("a日本b", 1, 3), "日");
        assert_eq!(
            columns("日本x", 1, 3),
            "日本",
            "a cut through a wide grapheme keeps it"
        );
        assert_eq!(columns("abc", 1, 99), "bc");
        assert_eq!(columns("abc", 5, 9), "");
    }

    #[test]
    fn a_selection_reads_its_rows_in_order_without_trailing_spaces() {
        let rows = [
            ("one two   ".to_string(), Tone::Reply),
            ("three".to_string(), Tone::Reply),
        ];
        let down = Selection {
            anchor: (0, 4),
            end: (1, 2),
        };
        let up = Selection {
            anchor: (1, 2),
            end: (0, 4),
        };
        assert_eq!(down.text(&rows), "two\nthr");
        assert_eq!(up.text(&rows), down.text(&rows));
        let past = Selection {
            anchor: (0, 0),
            end: (9, 0),
        };
        assert_eq!(past.text(&rows), "one two\nthree");
    }

    #[test]
    fn your_message_copies_without_its_bubble_padding() {
        let rows = [
            ("      Group the".to_string(), Tone::You),
            ("        indented".to_string(), Tone::You),
            ("      under one.".to_string(), Tone::You),
        ];
        let all = Selection {
            anchor: (0, 0),
            end: (2, 20),
        };
        assert_eq!(all.text(&rows), "Group the\n  indented\nunder one.");
    }
}
