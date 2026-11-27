use std::ops::Range;

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag};
use ratatui::style::Modifier;

/// How a span of a reply draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Look {
    pub modifier: Modifier,
    pub code: bool,
}

impl Look {
    const CODE: Look = Look {
        modifier: Modifier::empty(),
        code: true,
    };

    fn styled(modifier: Modifier) -> Look {
        Look {
            modifier,
            code: false,
        }
    }

    fn with(self, other: Look) -> Look {
        Look {
            modifier: self.modifier | other.modifier,
            code: self.code || other.code,
        }
    }
}

/// Ranges of a text and how each draws.
pub(crate) type Marks = Vec<(Range<usize>, Look)>;

/// A reply as the shell shows it: `source` without the markdown delimiters it recognizes, and
/// the byte ranges of that text they styled. Every other byte stays as typed.
pub(crate) fn render(source: &str) -> (String, Marks) {
    let mut cuts = Vec::new();
    let mut marks = Vec::new();
    let mut open: Vec<Look> = Vec::new();
    let events = Parser::new_ext(source, Options::ENABLE_STRIKETHROUGH).into_offset_iter();
    for (event, range) in events {
        let look = open
            .iter()
            .fold(Look::default(), |all, look| all.with(*look));
        match event {
            Event::Start(tag) => open.push(opened(source, &tag, &range, &mut cuts)),
            Event::End(_) => {
                open.pop();
            }
            Event::Text(_) if look != Look::default() => marks.push((range, look)),
            Event::Code(_) => {
                let ticks = run(source, range.start, b'`');
                ends(&range, ticks, &mut cuts);
                marks.push((
                    range.start + ticks..range.end - ticks,
                    look.with(Look::CODE),
                ));
            }
            _ => {}
        }
    }
    cuts.sort_by_key(|cut| cut.start);
    cuts.dedup_by(|next, kept| {
        if next.start > kept.end {
            return false;
        }
        kept.end = kept.end.max(next.end);
        true
    });
    let shift = |at: usize| {
        let before = cuts
            .iter()
            .map(|cut| cut.end.min(at).saturating_sub(cut.start));
        at - before.sum::<usize>()
    };
    let marks = marks
        .into_iter()
        .map(|(range, look)| (shift(range.start)..shift(range.end), look))
        .collect();
    let mut text = String::with_capacity(source.len());
    let mut at = 0;
    for cut in &cuts {
        text.push_str(&source[at..cut.start]);
        at = cut.end;
    }
    text.push_str(&source[at..]);
    (text, marks)
}

/// The look `tag` gives what it wraps, after cutting its delimiters.
fn opened(source: &str, tag: &Tag, range: &Range<usize>, cuts: &mut Vec<Range<usize>>) -> Look {
    match tag {
        Tag::Strong => {
            ends(range, 2, cuts);
            Look::styled(Modifier::BOLD)
        }
        Tag::Emphasis => {
            ends(range, 1, cuts);
            Look::styled(Modifier::ITALIC)
        }
        Tag::Strikethrough => {
            ends(range, run(source, range.start, b'~'), cuts);
            Look::styled(Modifier::CROSSED_OUT)
        }
        Tag::Heading { .. } => {
            let hashes = run(source, range.start, b'#');
            if hashes > 0 {
                let after = &source[range.start + hashes..range.end];
                let spaces = after.len() - after.trim_start_matches([' ', '\t']).len();
                cuts.push(range.start..range.start + hashes + spaces);
            }
            Look::styled(Modifier::BOLD)
        }
        Tag::CodeBlock(CodeBlockKind::Fenced(_)) => {
            fences(source, range, cuts);
            Look::CODE
        }
        Tag::CodeBlock(CodeBlockKind::Indented) => Look::CODE,
        _ => Look::default(),
    }
}

fn run(source: &str, from: usize, byte: u8) -> usize {
    source.as_bytes()[from..]
        .iter()
        .take_while(|&&at| at == byte)
        .count()
}

fn ends(range: &Range<usize>, width: usize, cuts: &mut Vec<Range<usize>>) {
    cuts.push(range.start..range.start + width);
    cuts.push(range.end - width..range.end);
}

/// Cuts a fenced block's first line, and its last when that line only repeats the fence.
fn fences(source: &str, range: &Range<usize>, cuts: &mut Vec<Range<usize>>) {
    let block = &source[range.clone()];
    let mark = block.as_bytes()[0];
    let fence = run(source, range.start, mark);
    let first = block.find('\n').map_or(range.end, |at| range.start + at);
    cuts.push(fence_line(source, range.start, first));
    let Some(last) = block.rfind('\n').map(|at| range.start + at + 1) else {
        return;
    };
    let closing = &source[last..range.end];
    let trimmed = closing.trim();
    if trimmed.len() >= fence && trimmed.bytes().all(|at| at == mark) {
        let at = last + closing.len() - closing.trim_start().len();
        cuts.push(fence_line(source, at, range.end));
    }
}

/// The fence at `start..end` with the whitespace before it on its line, and its newline, or
/// the one before it when it ends the text.
fn fence_line(source: &str, start: usize, end: usize) -> Range<usize> {
    let line = source[..start].rfind('\n').map_or(0, |at| at + 1);
    let start = if source[line..start].trim().is_empty() {
        line
    } else {
        start
    };
    if source[end..].starts_with('\n') {
        start..end + 1
    } else if source[..start].ends_with('\n') {
        start - 1..end
    } else {
        start..end
    }
}

#[cfg(test)]
mod tests {
    use ratatui::style::Modifier;

    use super::*;

    fn shown(source: &str) -> (String, Vec<(String, Look)>) {
        let (text, marks) = render(source);
        let marked = marks
            .into_iter()
            .map(|(range, look)| (text[range].to_string(), look))
            .collect();
        (text, marked)
    }

    fn look(modifier: Modifier) -> Look {
        Look {
            modifier,
            code: false,
        }
    }

    fn bold() -> Look {
        look(Modifier::BOLD)
    }

    fn code() -> Look {
        Look {
            modifier: Modifier::empty(),
            code: true,
        }
    }

    fn marked(pairs: &[(&str, Look)]) -> Vec<(String, Look)> {
        pairs
            .iter()
            .map(|(text, look)| (text.to_string(), *look))
            .collect()
    }

    #[test]
    fn delimiters_go_and_what_they_wrapped_is_styled() {
        assert_eq!(
            shown("**Fixed** the race in `wrap()`."),
            (
                "Fixed the race in wrap().".to_string(),
                marked(&[("Fixed", bold()), ("wrap()", code())])
            )
        );
    }

    #[test]
    fn nested_and_struck_spans_combine_their_looks() {
        assert_eq!(
            shown("***both*** _it_ ~~gone~~"),
            (
                "both it gone".to_string(),
                marked(&[
                    ("both", look(Modifier::BOLD | Modifier::ITALIC)),
                    ("it", look(Modifier::ITALIC)),
                    ("gone", look(Modifier::CROSSED_OUT)),
                ])
            )
        );
    }

    #[test]
    fn a_heading_loses_its_hashes_and_shows_bold() {
        assert_eq!(
            shown("## Fix\nnext"),
            ("Fix\nnext".to_string(), marked(&[("Fix", bold())]))
        );
    }

    #[test]
    fn a_fenced_block_loses_its_fence_lines() {
        assert_eq!(
            shown("Here:\n```rust\nlet x = 1;\n```\nDone."),
            (
                "Here:\nlet x = 1;\nDone.".to_string(),
                marked(&[("let x = 1;\n", code())])
            )
        );
        assert_eq!(render("x\n```\ncode\n```").0, "x\ncode");
        assert_eq!(render("- item\n  ```\n  code\n  ```").0, "- item\n  code");
        assert_eq!(render("```\n```").0, "");
    }

    #[test]
    fn streaming_markdown_shows_as_typed_until_it_closes() {
        assert_eq!(shown("a **b"), ("a **b".to_string(), Vec::new()));
        assert_eq!(
            shown("open:\n```\nstill"),
            ("open:\nstill".to_string(), marked(&[("still", code())]))
        );
    }

    #[test]
    fn everything_else_stays_as_typed() {
        let source = "- one\n| a | b |\n> quote\n[x](y) snake_case_name a * b * c \\*";
        assert_eq!(shown(source), (source.to_string(), Vec::new()));
    }
}
