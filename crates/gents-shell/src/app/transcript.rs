use std::borrow::Cow;
use std::cell::RefCell;
use std::fmt;
use std::ops::Range;
use std::time::{Duration, Instant};

use rapidhash::{HashSetExt, RapidHashSet};
use ratatui::style::Style;
use ratatui::text::Span;

use super::input::Lines;
use super::markdown::{self, Marks};
use super::table::table_rows;
use crate::sample::{changes, Running, Tone, CHAT};
use crate::source::{Agent, Change, Prompt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatLine {
    pub text: String,
    pub tone: Tone,
    /// The transcript item the line shows.
    pub item: Option<String>,
    /// The file change the line shows.
    pub change: Option<Change>,
    /// The item this line opens under: it shows only while the tab has that item open.
    pub under: Option<String>,
}

pub(super) fn blank() -> ChatLine {
    ChatLine {
        text: String::new(),
        tone: Tone::Plain,
        item: None,
        change: None,
        under: None,
    }
}

/// `scroll` counts lines up from the bottom. No `lines` means no messages yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tab {
    /// Set by `App::push_tab`; the subagent the sidebar shows keeps its parent's.
    pub id: u64,
    /// The id of the session the tab shows.
    pub session: Option<String>,
    pub name: String,
    pub agent: String,
    pub agent_id: String,
    pub lines: Vec<ChatLine>,
    pub scroll: usize,
    pub input: Lines,
    pub agent_picked: bool,
    pub running: Vec<Running>,
    /// The selected running row, while those rows have the keys.
    pub running_sel: Option<usize>,
    /// The session's turn in progress.
    pub turn: Option<String>,
    /// A send without a session is waiting for its `Started`.
    pub starting: bool,
    /// Whether the right pane is wanted while this tab shows. Not saved across runs.
    pub sidebar: bool,
    /// The session's open mailbox questions and approvals, oldest first.
    pub prompts: Vec<Prompt>,
    /// The tab sent a turn it has not seen end. The endpoint streams that turn here, and no
    /// other client's.
    pub following: bool,
    /// When the tab sent the turn it follows.
    pub since: Option<Instant>,
    /// The items whose lines are open, showing the lines under them. Not saved across runs.
    pub open: RapidHashSet<String>,
    pub(super) wrapped: Wrapped,
}

/// Each line's rows at the width they were wrapped for. Every frame reads the rows, and wrapping
/// a long transcript costs far more than drawing it, so only a changed line is wrapped again.
#[derive(Default)]
pub(super) struct Wrapped(RefCell<(u16, Vec<WrappedLine>)>);

type WrappedLine = (ChatLine, Vec<(String, Marks, &'static str)>);

/// A copy starts empty, and two tabs never differ by what they have cached.
impl Clone for Wrapped {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl PartialEq for Wrapped {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for Wrapped {}

impl fmt::Debug for Wrapped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Wrapped")
    }
}

impl Tab {
    pub fn new_session() -> Tab {
        Tab {
            id: 0,
            session: None,
            name: "new session".to_string(),
            agent: "Engineer".to_string(),
            agent_id: "Engineer".to_string(),
            lines: Vec::new(),
            scroll: 0,
            input: Lines::default(),
            agent_picked: false,
            running: Vec::new(),
            running_sel: None,
            turn: None,
            starting: false,
            sidebar: false,
            prompts: Vec::new(),
            following: false,
            since: None,
            open: RapidHashSet::new(),
            wrapped: Wrapped::default(),
        }
    }

    /// The lines' rows with each ended turn's card in place. Cards are built on each call, from
    /// the lines, and never cached.
    pub fn wrapped_rows(&self, width: u16) -> Vec<(String, Tone)> {
        let rows = self.rows_of_lines(width).into_iter();
        rows.map(|(row, tone, ..)| (row, tone)).collect()
    }

    /// How each of `wrapped_rows` styles its text, as display columns of the row.
    pub(crate) fn row_marks(&self, width: u16) -> Vec<Marks> {
        let rows = self.rows_of_lines(width).into_iter();
        rows.map(|(.., marks, _)| marks).collect()
    }

    /// What joins each of `wrapped_rows` to the row before it in a copy: a newline where its line
    /// breaks, else what the wrap took out there.
    pub(crate) fn row_joins(&self, width: u16) -> Vec<&'static str> {
        let rows = self.rows_of_lines(width).into_iter();
        rows.map(|(.., join)| join).collect()
    }

    /// The item whose line shows `row` of `wrapped_rows`, when lines open under it.
    pub(super) fn opener_at(&self, width: u16, row: usize) -> Option<String> {
        let line = &self.lines[self.rows_of_lines(width).get(row)?.2?];
        let item = line.item.as_ref()?;
        let opens = self
            .lines
            .iter()
            .any(|under| under.under.as_ref() == Some(item));
        opens.then(|| item.clone())
    }

    /// The rows of `wrapped_rows` that end in an open or closed mark: each opener's last.
    pub(super) fn mark_rows(&self, width: u16) -> Vec<usize> {
        let rows = self.rows_of_lines(width);
        let openers = self.openers();
        let opens = |line: usize| {
            let item = self.lines[line].item.as_deref();
            item.is_some_and(|item| openers.contains(item))
        };
        let last = |at: usize| rows.get(at + 1).map(|next| next.2) != Some(rows[at].2);
        let marked = |&at: &usize| rows[at].2.is_some_and(opens) && last(at);
        (0..rows.len()).filter(marked).collect()
    }

    /// The items other lines open under.
    fn openers(&self) -> RapidHashSet<&str> {
        let unders = self.lines.iter().filter_map(|line| line.under.as_deref());
        unders.collect()
    }

    /// Opens a closed item and closes an open one.
    pub(super) fn toggle(&mut self, item: String) {
        if !self.open.remove(&item) {
            self.open.insert(item);
        }
    }

    /// `wrapped_rows`, each with the index of the line it shows, its marks and its join; a card's
    /// rows have neither line nor marks. A line
    /// under a closed item shows no rows, nor does the blank after it, and a line that others
    /// open under ends in its open or closed mark.
    fn rows_of_lines(&self, width: u16) -> Vec<(String, Tone, Option<usize>, Marks, &'static str)> {
        let mut cards = self.cards(usize::from(width)).into_iter().peekable();
        let mut wrapped = self.wrapped.0.borrow_mut();
        let (wrapped_at, seen) = &mut *wrapped;
        if *wrapped_at != width {
            *wrapped_at = width;
            seen.clear();
        }
        seen.truncate(self.lines.len());
        let openers = self.openers();
        let mut rows = Vec::new();
        let mut hid = false;
        for (at, line) in self.lines.iter().enumerate() {
            while let Some((_, card)) = cards.next_if(|(before, _)| *before == at) {
                rows.extend(
                    card.into_iter()
                        .map(|(row, tone)| (row, tone, None, Marks::new(), "\n")),
                );
            }
            let closed = line
                .under
                .as_deref()
                .is_some_and(|item| !self.open.contains(item));
            let hidden = closed || (hid && *line == blank());
            hid = closed;
            let shown = self.marked(line, &openers);
            if !matches!(seen.get(at), Some((was, _)) if *was == *shown) {
                let fresh = (
                    shown.clone().into_owned(),
                    line_rows(&shown, usize::from(width)),
                );
                match seen.get_mut(at) {
                    Some(slot) => *slot = fresh,
                    None => seen.push(fresh),
                }
            }
            if !hidden {
                let line_rows = seen[at].1.iter();
                rows.extend(line_rows.map(|(row, marks, join)| {
                    (row.clone(), line.tone, Some(at), marks.clone(), *join)
                }));
            }
        }
        let cards = cards.flat_map(|(_, card)| card);
        rows.extend(cards.map(|(row, tone)| (row, tone, None, Marks::new(), "\n")));
        rows
    }

    fn marked<'a>(&self, line: &'a ChatLine, openers: &RapidHashSet<&str>) -> Cow<'a, ChatLine> {
        let Some(item) = line.item.as_deref().filter(|item| openers.contains(item)) else {
            return Cow::Borrowed(line);
        };
        let mark = if self.open.contains(item) {
            "▾"
        } else {
            "▸"
        };
        Cow::Owned(ChatLine {
            text: format!("{}  {mark}", line.text),
            ..line.clone()
        })
    }

    /// An unseen item adds its line and a blank: under the item it opens under, after the lines
    /// already there, or else at the end. A seen one replaces its line.
    pub(super) fn put(&mut self, line: ChatLine) {
        if let Some(index) = line.item.as_deref().and_then(|item| self.item_line(item)) {
            self.lines[index] = line;
            return;
        }
        let parent = line.under.as_deref();
        let opener = parent.and_then(|parent| self.item_line(parent));
        let at = opener.map_or(self.lines.len(), |opener| {
            let under = |shown: &ChatLine| *shown == blank() || shown.under.as_deref() == parent;
            let past = self.lines[opener + 1..]
                .iter()
                .take_while(|shown| under(shown));
            opener + 1 + past.count()
        });
        self.lines.splice(at..at, [line, blank()]);
    }

    /// The selected row stays selected while it is listed, else its position is kept in range.
    pub(super) fn set_running(&mut self, rows: Vec<Running>) {
        let selected = self.running_sel.and_then(|row| self.running.get(row));
        let kept =
            selected.and_then(|selected| rows.iter().position(|row| row.opens == selected.opens));
        let last = rows.len().checked_sub(1);
        let clamped = self.running_sel.zip(last).map(|(sel, last)| sel.min(last));
        self.running_sel = kept.or(clamped);
        self.running = rows;
    }

    pub(super) fn set_agent(&mut self, agent: &Agent) {
        self.agent = agent.name.clone();
        self.agent_id = agent.id.clone();
    }

    pub(super) fn item_line(&self, item: &str) -> Option<usize> {
        self.lines
            .iter()
            .position(|line| line.item.as_deref() == Some(item))
    }
}

/// A line's rows, each with its marks and what joins it to the row before in a copy.
pub(super) fn line_rows(line: &ChatLine, width: usize) -> Vec<(String, Marks, &'static str)> {
    let rows = match line.tone {
        Tone::Reply => return reply_rows(&line.text, width),
        Tone::You => bubble(&line.text, width),
        _ => joined_rows(&line.text, width),
    };
    rows.into_iter()
        .map(|(row, join)| (row, Marks::new(), join))
        .collect()
}

/// A reply's rows as its markdown shows it, each table drawn in place of its lines.
fn reply_rows(text: &str, width: usize) -> Vec<(String, Marks, &'static str)> {
    let (shown, marks, tables) = markdown::render(text);
    let mut rows = Vec::new();
    let mut line_start = 0;
    for line in shown.split('\n') {
        let line_end = line_start + line.len();
        let starts = tables
            .iter()
            .find(|table| (line_start..=line_end).contains(&table.range.start));
        let inside = tables
            .iter()
            .any(|table| table.range.start < line_start && line_start < table.range.end);
        if let Some(table) = starts {
            let cells: Vec<Vec<(&str, usize)>> = table
                .rows
                .iter()
                .map(|row| row.iter().map(|cell| trimmed(&shown, cell)).collect())
                .collect();
            let drawn = table_rows(&cells, &marks, width).into_iter();
            rows.extend(drawn.map(|(row, marks)| (row, marks, "\n")));
        } else if !inside {
            rows.extend(marked_rows(line, line_start, &marks, width));
        }
        line_start = line_end + 1;
    }
    rows
}

/// The text of `range` in `text` without the spaces around it, and where it starts.
fn trimmed<'a>(text: &'a str, range: &Range<usize>) -> (&'a str, usize) {
    let cell = &text[range.clone()];
    let start = range.start + cell.len() - cell.trim_start().len();
    (cell.trim(), start)
}

/// `line`, at byte `start` of the text `marks` are ranges of, wrapped in `width` columns: each row
/// with its marks as display columns of the row, and what joins it to the row before in a copy, a
/// newline for the first, a space where the wrap took spaces out and nothing where it broke a word.
pub(super) fn marked_rows(
    line: &str,
    start: usize,
    marks: &Marks,
    width: usize,
) -> Vec<(String, Marks, &'static str)> {
    let mut rows = Vec::new();
    let mut end = 0;
    for row in wrap(line, width) {
        let at = end + line[end..].find(row.as_str()).unwrap_or(0);
        let join = match at {
            _ if rows.is_empty() => "\n",
            at if at > end => " ",
            _ => "",
        };
        end = at + row.len();
        let (from, to) = (start + at, start + end);
        let cells = |at: usize| Span::raw(&row[..at - from]).width();
        let marked = marks
            .iter()
            .filter(|(range, _)| range.start < to && range.end > from)
            .map(|(range, look)| {
                (
                    cells(range.start.max(from))..cells(range.end.min(to)),
                    *look,
                )
            })
            .collect();
        rows.push((row, marked, join));
    }
    rows
}

/// `text`'s lines wrapped in `width` columns, each row with what joins it to the row before.
fn joined_rows(text: &str, width: usize) -> Vec<(String, &'static str)> {
    let lines = text.split('\n');
    let rows = lines.flat_map(|line| marked_rows(line, 0, &Marks::new(), width));
    rows.map(|(row, _, join)| (row, join)).collect()
}

pub(super) fn wrap_lines(text: &str, width: usize) -> Vec<String> {
    text.split('\n')
        .flat_map(|line| wrap(line, width))
        .collect()
}

/// Breaks at the last space that fits, dropping the spaces there, and inside a word only when the row holds no
/// space past its indent. Breaks fall between grapheme clusters, the units `Buffer` draws.
pub(super) fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    let (mut row, mut used, mut broke) = (String::new(), 0, false);
    for grapheme in Span::raw(text).styled_graphemes(Style::new()) {
        let cell = Span::raw(grapheme.symbol).width();
        if broke && row.is_empty() && grapheme.symbol == " " {
            continue;
        }
        if used + cell > width && used > 0 {
            broke = true;
            if grapheme.symbol == " " {
                rows.push(std::mem::take(&mut row).trim_end().to_string());
                used = 0;
                continue;
            }
            let space = row.rfind(' ').filter(|&at| !row[..at].trim().is_empty());
            let rest = space.map_or(String::new(), |at| {
                row.split_off(at).trim_start().to_string()
            });
            rows.push(std::mem::replace(&mut row, rest).trim_end().to_string());
            used = Span::raw(row.as_str()).width();
        }
        row.push_str(grapheme.symbol);
        used += cell;
    }
    rows.push(row);
    rows
}

/// Padded on the left so one block of text sits against the right edge, leaving a cell of tint
/// on each side. Padding in the text keeps search columns right.
pub(super) fn bubble(text: &str, width: usize) -> Vec<(String, &'static str)> {
    let room = (width * 3 / 4).min(width.saturating_sub(2)).max(1);
    let rows = joined_rows(text, room);
    let widest = rows
        .iter()
        .map(|(row, _)| Span::raw(row.as_str()).width())
        .max();
    let left = width.saturating_sub(widest.unwrap_or(0) + 1);
    rows.into_iter()
        .map(|(row, join)| (format!("{:left$}{row}", ""), join))
        .collect()
}

/// The sample's changes come after its sixth line, the read.
pub fn sample_chat() -> Vec<ChatLine> {
    let chat = |lines: &[(&str, Tone)]| -> Vec<ChatLine> {
        lines
            .iter()
            .map(|&(text, tone)| ChatLine {
                text: text.to_string(),
                tone,
                item: None,
                change: None,
                under: None,
            })
            .collect()
    };
    let (read, ran) = CHAT.split_at(6);
    [chat(read), changes(), chat(ran)].concat()
}

/// `spent` in whole seconds as the transcript reads it: `8s`, `1m 23s`, `1h 2m`.
pub fn elapsed(spent: Duration) -> String {
    let secs = spent.as_secs();
    match (secs / 3600, secs / 60 % 60, secs % 60) {
        (0, 0, s) => format!("{s}s"),
        (0, m, s) => format!("{m}m {s}s"),
        (h, m, _) => format!("{h}h {m}m"),
    }
}
