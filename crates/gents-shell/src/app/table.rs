use ratatui::style::Modifier;
use ratatui::text::Span;

use super::markdown::{Look, Marks};
use super::transcript::marked_rows;

/// A table's rows in `width` columns: `cells` row by row, the head first, each a cell's text and
/// that text's byte offset in the text `marks` are ranges of. Columns are as wide as their widest
/// cell while the table fits, else the widest give up columns first and their cells wrap; a rule
/// follows the head, which shows bold.
pub(super) fn table_rows(
    cells: &[Vec<(&str, usize)>],
    marks: &Marks,
    width: usize,
) -> Vec<(String, Marks)> {
    let count = cells.iter().map(Vec::len).max().unwrap_or(0);
    let widths = widths(cells, count, width);
    let mut rows = Vec::new();
    for (at, row) in cells.iter().enumerate() {
        let wrapped: Vec<Vec<(String, Marks, &str)>> = (0..count)
            .map(|column| {
                let (text, offset) = row.get(column).copied().unwrap_or(("", 0));
                marked_rows(text, offset, marks, widths[column])
            })
            .collect();
        let tall = wrapped.iter().map(Vec::len).max().unwrap_or(0);
        for line in 0..tall {
            let mut shown = String::new();
            let mut marked = Marks::new();
            for (column, pieces) in wrapped.iter().enumerate() {
                if column > 0 {
                    shown.push_str(" │ ");
                }
                let x = Span::raw(shown.as_str()).width();
                let (piece, piece_marks) =
                    pieces.get(line).map_or(("", &[][..]), |(piece, marks, _)| {
                        (piece.as_str(), &marks[..])
                    });
                let cells = Span::raw(piece).width();
                if at == 0 && cells > 0 {
                    marked.push((x..x + cells, Look::styled(Modifier::BOLD)));
                }
                let shifted = piece_marks
                    .iter()
                    .map(|(range, look)| (x + range.start..x + range.end, *look));
                marked.extend(shifted);
                shown.push_str(piece);
                if column + 1 < count {
                    shown.push_str(&" ".repeat(widths[column].saturating_sub(cells)));
                }
            }
            rows.push((shown, marked));
        }
        if at == 0 {
            let rule: Vec<String> = widths.iter().map(|&wide| "─".repeat(wide)).collect();
            rows.push((rule.join("─┼─"), Marks::new()));
        }
    }
    rows
}

/// Each column's width: its widest cell, at least 1, when the table fits in `width`; else every
/// column capped at the widest cap that fits, the room left going to capped columns from the left.
fn widths(cells: &[Vec<(&str, usize)>], count: usize, width: usize) -> Vec<usize> {
    let natural: Vec<usize> = (0..count)
        .map(|column| {
            let wide = cells.iter().filter_map(|row| row.get(column));
            let wide = wide.map(|(text, _)| Span::raw(*text).width()).max();
            wide.unwrap_or(0).max(1)
        })
        .collect();
    let room = width.saturating_sub(3 * count.saturating_sub(1)).max(count);
    let used = |cap: usize| natural.iter().map(|&wide| wide.min(cap)).sum::<usize>();
    let widest = natural.iter().copied().max().unwrap_or(1);
    if used(widest) <= room {
        return natural;
    }
    let cap = (1..widest)
        .rev()
        .find(|&cap| used(cap) <= room)
        .unwrap_or(1);
    let mut left = room.saturating_sub(used(cap));
    natural
        .iter()
        .map(|&wide| {
            let capped = wide.min(cap);
            if wide > cap && left > 0 {
                left -= 1;
                capped + 1
            } else {
                capped
            }
        })
        .collect()
}
