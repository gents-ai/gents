use std::cmp::Reverse;

use super::work::{in_range, item, label};
use super::{first_selectable, wrap_lines, App, Tab};
use crate::keys::in_key_set;
use crate::sample::{note, RowAction, Scope, SidebarRow, Tone, View, DIFF_BACK_ROW};
use crate::source::{Change, ChangeKind};

/// The changes to one shown path: one turn's in `Tab::files`, every turn's once merged, or one
/// working-tree change. The mark is its first change's.
pub struct File<'a> {
    /// Its first change's turn: a merged file's first turn, and empty in the working tree.
    pub turn: &'a str,
    pub path: String,
    pub mark: char,
    pub added: usize,
    pub removed: usize,
    pub changes: Vec<&'a Change>,
    /// Where its latest change sits: in the tab's lines for a session's file, and in git's
    /// order for the working tree's.
    pub last: usize,
}

impl<'a> File<'a> {
    /// `change` alone, shown as its kind's mark and its path, a move's as both paths.
    fn of(change: &'a Change, at: usize) -> File<'a> {
        let (mark, path) = match &change.kind {
            ChangeKind::Add => ('A', change.path.clone()),
            ChangeKind::Delete => ('D', change.path.clone()),
            ChangeKind::Update => ('M', change.path.clone()),
            ChangeKind::Move { to } => ('R', format!("{} → {to}", change.path)),
        };
        let (added, removed) = change.counts();
        File {
            turn: &change.turn,
            path,
            mark,
            added,
            removed,
            changes: vec![change],
            last: at,
        }
    }
}

/// The files a card lists; the Diff view lists the rest.
const CARD_FILES: usize = 10;

const BLANK: (String, Tone) = (String::new(), Tone::Plain);

/// `text`, then `gap` and `+a -d`, leaving out a side that is zero and both when both are.
fn counted(text: &str, gap: &str, added: usize, removed: usize) -> String {
    let sides = [('+', added), ('-', removed)].into_iter();
    let counts: Vec<String> = sides
        .filter(|&(_, count)| count > 0)
        .map(|(sign, count)| format!("{sign}{count}"))
        .collect();
    match counts.is_empty() {
        true => text.trim_end().to_string(),
        false => format!("{text}{gap}{}", counts.join(" ")),
    }
}

/// The file's mark and path, the path padded to `width`, then its counts.
fn file_row(indent: &str, file: &File, width: usize) -> String {
    let text = format!("{indent}{} {:<width$}", file.mark, file.path);
    counted(&text, "  ", file.added, file.removed)
}

/// The turn's header, its first files, how many more the Diff view lists, and a blank,
/// wrapped to `width`.
fn card(files: &[File], turn: &str, width: usize) -> Vec<(String, Tone)> {
    let listed: Vec<(usize, &File)> = files
        .iter()
        .enumerate()
        .filter(|(_, file)| file.turn == turn)
        .collect();
    let added = listed.iter().map(|(_, file)| file.added).sum();
    let removed = listed.iter().map(|(_, file)| file.removed).sum();
    let header = format!("  {} {} changed", listed.len(), files_noun(listed.len()));
    let shown = &listed[..listed.len().min(CARD_FILES)];
    let path_width = shown.iter().map(|(_, file)| file.path.chars().count());
    let path_width = path_width.max().unwrap_or(0);
    let mut rows = vec![(counted(&header, "  ", added, removed), Tone::Label)];
    rows.extend(
        shown
            .iter()
            .map(|&(index, file)| (file_row("  ", file, path_width), Tone::File(index))),
    );
    if listed.len() > CARD_FILES {
        let more = listed.len() - CARD_FILES;
        rows.push((format!("  … {more} more in Diff"), Tone::Label));
    }
    rows.push((String::new(), Tone::Plain));
    rows.into_iter()
        .flat_map(|(text, tone)| {
            let wrapped = wrap_lines(&text, width);
            wrapped.into_iter().map(move |row| (row, tone))
        })
        .collect()
}

/// ` {n} file(s)   +a -d`, counting distinct paths.
fn header(files: &[File]) -> String {
    let (paths, added, removed) = totals(files);
    let header = format!(" {paths} {}", files_noun(paths));
    counted(&header, "   ", added, removed)
}

/// Distinct paths, lines added and lines removed.
fn totals(files: &[File]) -> (usize, usize, usize) {
    let mut paths: Vec<&str> = Vec::new();
    for file in files {
        // ponytail: linear grouping, fine for hundreds of files
        if !paths.contains(&file.path.as_str()) {
            paths.push(&file.path);
        }
    }
    let added = files.iter().map(|file| file.added).sum();
    let removed = files.iter().map(|file| file.removed).sum();
    (paths.len(), added, removed)
}

fn files_noun(count: usize) -> &'static str {
    if count == 1 {
        "file"
    } else {
        "files"
    }
}

/// One path's files from every turn as one: the first's mark, and every turn's counts.
fn merged<'a>(turns: &[&File<'a>]) -> File<'a> {
    let first = turns[0];
    File {
        turn: first.turn,
        path: first.path.clone(),
        mark: first.mark,
        added: turns.iter().map(|file| file.added).sum(),
        removed: turns.iter().map(|file| file.removed).sum(),
        changes: turns.iter().flat_map(|file| file.changes.clone()).collect(),
        last: turns
            .iter()
            .map(|file| file.last)
            .max()
            .unwrap_or(first.last),
    }
}

/// The Diff view's session scope: the session's totals, then each path once, the latest
/// changed first, opening its first file. A running turn's files are listed too.
pub fn session_rows(files: &[File]) -> Vec<SidebarRow> {
    if files.is_empty() {
        return vec![label(" no changes in this session".to_string())];
    }
    let mut rows = vec![label(header(files)), note("", Tone::Plain)];
    let width = files.iter().map(|file| file.path.chars().count()).max();
    let width = width.unwrap_or(0);
    let mut listed: Vec<(usize, Vec<&File>)> = Vec::new();
    for (index, file) in files.iter().enumerate() {
        // ponytail: linear grouping, fine for hundreds of files
        match listed
            .iter_mut()
            .find(|(first, _)| files[*first].path == file.path)
        {
            Some((_, turns)) => turns.push(file),
            None => listed.push((index, vec![file])),
        }
    }
    let mut listed: Vec<(usize, File, usize)> = listed
        .into_iter()
        .map(|(index, turns)| (index, merged(&turns), turns.len()))
        .collect();
    listed.sort_by_key(|(_, file, _)| Reverse(file.last));
    rows.extend(listed.into_iter().map(|(index, file, turns)| {
        let mut text = file_row(" ", &file, width);
        if turns > 1 {
            text.push_str(&format!("   {turns} turns"));
        }
        item(text, Tone::Plain, RowAction::OpenDiff(index))
    }));
    rows
}

/// The Diff view's working-tree scope: the folder's totals, then each changed file in git's
/// order, opening its position there.
pub fn tree_rows(tree: Option<&Result<Vec<Change>, String>>) -> Vec<SidebarRow> {
    let changes = match tree {
        None => return vec![label(" reading the working tree…".to_string())],
        Some(Err(why)) => return vec![label(format!(" no working tree: {why}"))],
        Some(Ok(changes)) => changes,
    };
    if changes.is_empty() {
        return vec![label(" no changes in this folder".to_string())];
    }
    let files: Vec<File> = changes
        .iter()
        .enumerate()
        .map(|(at, change)| File::of(change, at))
        .collect();
    let width = files.iter().map(|file| file.path.chars().count()).max();
    let width = width.unwrap_or(0);
    let header = format!("{}   whole folder", header(&files));
    let mut rows = vec![label(header), note("", Tone::Plain)];
    rows.extend(files.iter().enumerate().map(|(index, file)| {
        let text = file_row(" ", file, width);
        item(text, Tone::Plain, RowAction::OpenDiff(index))
    }));
    rows
}

/// The working tree's `file`: its header, then its lines.
fn tree_diff(changes: &[Change], file: usize) -> Option<Vec<(String, Tone)>> {
    let file = File::of(changes.get(file)?, file);
    let mut rows = vec![(file_row(" ", &file, 0), Tone::Label), BLANK];
    rows.extend(change_lines(&file.changes));
    Some(rows)
}

/// The narrow right pane: the same totals as the Diff view's session header.
pub fn rail_rows(files: &[File]) -> Vec<SidebarRow> {
    let (paths, added, removed) = totals(files);
    let toned = |text: String, tone| SidebarRow {
        text: text.into(),
        ..note("", tone)
    };
    vec![
        label(format!(" {paths}  ")),
        note("", Tone::Plain),
        toned(format!(" +{added}"), Tone::Add),
        toned(format!(" -{removed}"), Tone::Del),
    ]
}

/// A diff line's colour: hunk headers, additions, removals and context.
fn diff_tone(line: &str) -> Tone {
    if line.starts_with("@@") {
        Tone::Label
    } else if line.starts_with('+') {
        Tone::Add
    } else if line.starts_with('-') {
        Tone::Del
    } else {
        Tone::Plain
    }
}

impl Tab {
    /// One per turn and shown path, in the order each was first changed, so a new change only
    /// appends a file or adds to one.
    pub fn files(&self) -> Vec<File<'_>> {
        let mut files: Vec<File<'_>> = Vec::new();
        let changes = self.lines.iter().enumerate();
        let changes = changes.filter_map(|(at, line)| Some((at, line.change.as_ref()?)));
        for (at, change) in changes {
            let file = File::of(change, at);
            // ponytail: linear grouping, fine for hundreds of files
            let listed = files
                .iter_mut()
                .find(|listed| listed.turn == file.turn && listed.path == file.path);
            match listed {
                Some(listed) => {
                    listed.added += file.added;
                    listed.removed += file.removed;
                    listed.changes.push(change);
                    listed.last = at;
                }
                None => files.push(file),
            }
        }
        files
    }

    /// `file`'s path in every turn that changed it: the header, then each turn's prompt and
    /// changes.
    fn session_diff(&self, file: usize) -> Option<Vec<(String, Tone)>> {
        let files = self.files();
        let path = &files.get(file)?.path;
        let turns: Vec<&File> = files.iter().filter(|listed| listed.path == *path).collect();
        let mut rows = vec![(file_row(" ", &merged(&turns), 0), Tone::Label)];
        for file in turns {
            rows.push(BLANK);
            rows.push((format!(" {}", self.prompt(file.turn)), Tone::Accent));
            rows.extend(change_lines(&file.changes));
        }
        Some(rows)
    }

    /// The first line of your last message before `turn`'s first change, else the turn's id.
    fn prompt(&self, turn: &str) -> String {
        let first = self.lines.iter().position(|line| {
            let change = line.change.as_ref();
            change.is_some_and(|change| change.turn == turn)
        });
        let before = &self.lines[..first.unwrap_or(0)];
        let you = before.iter().rev().find(|line| line.tone == Tone::You);
        match you.and_then(|line| line.text.lines().next()) {
            Some(text) => text.to_string(),
            None => format!("turn {turn}"),
        }
    }

    /// Each ended turn's card, wrapped to `width`, with the line its rows go before: the first
    /// message from you after the turn's last change, or the end. Sorted by that line.
    pub(super) fn cards(&self, width: usize) -> Vec<(usize, Vec<(String, Tone)>)> {
        let files = self.files();
        let mut last: Vec<(&str, usize)> = Vec::new();
        for (at, line) in self.lines.iter().enumerate() {
            let Some(change) = &line.change else {
                continue;
            };
            // ponytail: linear grouping, fine for hundreds of files
            match last.iter_mut().find(|(turn, _)| *turn == change.turn) {
                Some(seen) => seen.1 = at,
                None => last.push((&change.turn, at)),
            }
        }
        let end = |at: usize| {
            let you = self.lines[at..]
                .iter()
                .position(|line| line.tone == Tone::You);
            you.map_or(self.lines.len(), |after| at + after)
        };
        let mut cards: Vec<_> = last
            .into_iter()
            .filter(|&(turn, _)| self.turn.as_deref() != Some(turn))
            .map(|(turn, at)| (end(at), card(&files, turn, width)))
            .collect();
        cards.sort_by_key(|(before, _)| *before);
        cards
    }
}

impl App {
    pub(super) fn selected_file(&self) -> Option<usize> {
        let rows = self.view_rows(View::Diff);
        match rows.get(self.view_sel[View::Diff as usize])?.action {
            RowAction::OpenDiff(file) => Some(file),
            _ => None,
        }
    }

    /// Selects `file`'s row while the Diff view lists it, else the nearest row that can be
    /// selected.
    pub(super) fn keep_file_selected(&mut self, file: Option<usize>) {
        let rows = self.view_rows(View::Diff);
        let listed = file.and_then(|file| {
            rows.iter()
                .position(|row| row.action == RowAction::OpenDiff(file))
        });
        let selected = &mut self.view_sel[View::Diff as usize];
        *selected = listed.unwrap_or_else(|| in_range(&rows, *selected));
    }

    /// The working tree's changes, empty until read or when the read failed.
    fn tree(&self) -> &[Change] {
        match &self.working_tree {
            Some(Ok(changes)) => changes,
            _ => &[],
        }
    }

    /// A reread can add or drop files ahead of the open or the selected one, so in the
    /// working-tree scope each follows its path, and an open file that is gone closes.
    pub(super) fn set_working_tree(&mut self, tree: Result<Vec<Change>, String>) {
        if self.scope != Scope::Tree {
            self.working_tree = Some(tree);
            return;
        }
        let path = |app: &App, file: usize| app.tree().get(file).map(|change| change.path.clone());
        let open = self.diff.and_then(|file| path(self, file));
        let selected = self.selected_file().and_then(|file| path(self, file));
        self.working_tree = Some(tree);
        let index =
            |app: &App, path: String| app.tree().iter().position(|change| change.path == path);
        self.diff = open.and_then(|path| index(self, path));
        let selected = selected.and_then(|path| index(self, path));
        self.keep_file_selected(selected);
    }

    /// The Diff view lists another scope: an open diff indexed the old list, so it closes.
    pub fn set_scope(&mut self, scope: Scope) {
        self.scope = scope;
        self.set_view(View::Diff);
        self.view_sel[View::Diff as usize] = first_selectable(&self.view_rows(View::Diff));
    }

    /// The scope's diff of `file`, then a blank and the back row, which is the last row. A file
    /// past the list, as after a reread, shows only the back row.
    pub fn diff_rows(&self, file: usize) -> Vec<(String, Tone)> {
        let back = (in_key_set(self.keymap.set(), DIFF_BACK_ROW), Tone::Label);
        let shown = match self.scope {
            Scope::Session => self.tabs[self.active].session_diff(file),
            Scope::Tree => tree_diff(self.tree(), file),
        };
        let Some(mut rows) = shown else {
            return vec![back];
        };
        rows.extend([BLANK, back]);
        rows
    }
}

fn change_lines(changes: &[&Change]) -> Vec<(String, Tone)> {
    let mut rows = Vec::new();
    for change in changes {
        if change.diff.is_empty() {
            rows.push((" no diff text recorded".to_string(), Tone::Label));
            continue;
        }
        let sign = match change.kind {
            ChangeKind::Add => "+",
            ChangeKind::Delete => "-",
            ChangeKind::Update | ChangeKind::Move { .. } => "",
        };
        // Ratatui drops control characters, a tab among them.
        rows.extend(change.diff.lines().map(|line| {
            let line = format!("{sign}{}", line.replace('\t', "    "));
            (format!(" {line}"), diff_tone(&line))
        }));
    }
    rows
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{channel, Receiver};

    use crate::app::{App, ChatLine, Pane, Tab};
    use crate::commands::Effect;
    use crate::keys::KeySet::Vim;
    use crate::sample::{RowAction, Scope, Tone, View};
    use crate::source::{Change, ChangeKind, Connection, Intent, Update};
    use crate::testing::{click_text, find, press, screen, session_row, with_right_pane};

    const EDIT: &str = "@@ -1 +1 @@\n-a\n+b\n";

    fn line(text: &str, tone: Tone) -> ChatLine {
        ChatLine {
            text: text.to_string(),
            tone,
            item: None,
            change: None,
        }
    }

    fn edit(turn: &str, path: &str, kind: ChangeKind, diff: &str) -> ChatLine {
        let change = Change::new(turn.to_string(), path.to_string(), kind, diff.to_string());
        ChatLine {
            change: Some(change),
            ..line(&format!("  ✓ edit  {path}"), Tone::Label)
        }
    }

    /// A connected tab showing `s1`, opened with `lines`.
    fn opened(lines: Vec<ChatLine>) -> (App, Receiver<Intent>) {
        let (intents, receiver) = channel();
        let mut app = App::connected(Vim, 132, 36, intents);
        app.tabs[0].session = Some("s1".to_string());
        app.apply(Update::Opened {
            session: "s1".to_string(),
            lines,
        });
        (app, receiver)
    }

    fn sample_session() -> App {
        let mut app = App::new(Vim, 132, 36);
        app.open_session(session_row("pack registry"));
        app
    }

    fn texts(rows: &[(String, Tone)]) -> Vec<&str> {
        rows.iter().map(|(text, _)| text.as_str()).collect()
    }

    fn row(text: &str, tone: Tone) -> (String, Tone) {
        (text.to_string(), tone)
    }

    #[test]
    fn a_change_counts_its_lines_once() {
        use ChangeKind::{Add, Delete, Update};
        let counts =
            |kind, diff: &str| Change::new("t1".into(), "a.rs".into(), kind, diff.into()).counts();
        let headed = "--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,2 @@\n-old\n+new\n ctx\n";
        assert_eq!(counts(Update, headed), (1, 1));
        assert_eq!(
            counts(Update, "@@ -1 +1 @@\n---x\n+++y\n"),
            (1, 1),
            "inside a hunk these are content lines"
        );
        assert_eq!(counts(Add, "x\ny\n"), (2, 0));
        assert_eq!(counts(Delete, "x\n"), (0, 1));
        assert_eq!(counts(Update, ""), (0, 0));
    }

    #[test]
    fn an_ended_turn_shows_its_card_under_its_last_line() {
        let s = screen(&sample_session());
        let header = find(&s, "3 files changed  +8 -4").expect("the card's header");
        for file in [
            "M cli/args.rs   +3 -3",
            "M cli/mod.rs    +2 -1",
            "A shell/mod.rs  +3",
        ] {
            assert!(find(&s, file).is_some(), "{file}");
        }
        let ran = find(&s, "… run   cargo check -p gents-cli").expect("the turn's last row");
        assert!(header.0 > ran.0, "{s:#?}");
    }

    #[test]
    fn a_turn_in_progress_shows_no_card_until_it_ends() {
        let (mut app, _intents) = opened(vec![
            line("do it", Tone::You),
            edit("t2", "a.rs", ChangeKind::Update, EDIT),
            line("done", Tone::Reply),
        ]);
        let s1 = || "s1".to_string();
        let t2 = || "t2".to_string();
        app.apply(Update::Turn {
            session: s1(),
            turn: t2(),
        });
        assert_eq!(find(&screen(&app), "1 file changed"), None);

        app.apply(Update::TurnEnded {
            session: s1(),
            turn: t2(),
        });
        assert!(find(&screen(&app), "1 file changed  +1 -1").is_some());

        app.apply(Update::Item {
            session: s1(),
            line: line("next", Tone::You),
        });
        let rows = app.conversation_rows(80);
        let header = rows
            .iter()
            .position(|(text, _)| text == "  1 file changed  +1 -1");
        let next = rows
            .iter()
            .position(|(text, tone)| *tone == Tone::You && text.trim() == "next");
        assert!(
            header.expect("the card") < next.expect("the message"),
            "{rows:#?}"
        );
    }

    #[test]
    fn the_card_groups_a_turns_files_and_each_row_opens_its_diff() {
        use ChangeKind::{Add, Update};
        let (mut app, _intents) = opened(vec![
            line("go", Tone::You),
            edit("t1", "a.rs", Update, EDIT),
            edit("t1", "a.rs", Update, EDIT),
            edit("t1", "b.rs", Add, "x\n"),
        ]);
        let s = screen(&app);
        for card in ["2 files changed  +3 -2", "M a.rs  +2 -2", "A b.rs  +1"] {
            assert!(find(&s, card).is_some(), "{card}");
        }

        click_text(&mut app, "M a.rs");
        assert_eq!(app.diff, Some(0));
        assert!(app.tabs[0].sidebar);
        assert_eq!(app.focus, Pane::Agent);
        let hunks = app.diff_rows(0);
        let hunks = hunks.iter().filter(|(text, _)| text == " @@ -1 +1 @@");
        assert_eq!(hunks.count(), 2);

        click_text(&mut app, "A b.rs");
        assert_eq!(app.diff, Some(1));
        assert!(app.diff_rows(1).contains(&row(" +x", Tone::Add)));
        assert_eq!(app.diff_rows(9), [row(" esc back to files", Tone::Label)]);
    }

    #[test]
    fn a_large_new_file_counts_and_scrolls_to_its_last_line() {
        let content: String = (0..5000).map(|n| format!("line {n}\n")).collect();
        let (mut app, _intents) = opened(vec![edit("t1", "big.txt", ChangeKind::Add, &content)]);
        assert!(find(&screen(&app), "A big.txt  +5000").is_some());
        assert_eq!(app.diff_rows(0).len(), 5005);

        app.show_file(0);
        app.diff_scroll = usize::MAX;
        let s = screen(&app);
        assert!(find(&s, "+line 4999").is_some());
        assert!(find(&s, "esc back to files").is_some());
    }

    #[test]
    fn an_empty_diff_shows_its_path_and_says_no_text() {
        let (mut app, _intents) = opened(vec![edit("t1", "logo.png", ChangeKind::Update, "")]);
        let rows = app.conversation_rows(80);
        assert!(
            rows.contains(&row("  M logo.png", Tone::File(0))),
            "{rows:#?}"
        );
        let said = row(" no diff text recorded", Tone::Label);
        assert!(app.diff_rows(0).contains(&said));
        app.show_file(0);
        assert!(find(&screen(&app), "no diff text recorded").is_some());
    }

    #[test]
    fn a_moved_file_shows_both_paths() {
        let moved = ChangeKind::Move {
            to: "new.rs".to_string(),
        };
        let (app, _intents) = opened(vec![edit("t1", "old.rs", moved, EDIT)]);
        assert!(find(&screen(&app), "R old.rs → new.rs  +1 -1").is_some());
    }

    #[test]
    fn forty_files_cap_the_card_at_ten() {
        let added = (0..40).map(|n| edit("t1", &format!("f{n:02}.rs"), ChangeKind::Add, "x\n"));
        let (mut app, _intents) = opened(added.collect());
        let rows = app.conversation_rows(80);
        let rows = texts(&rows);
        let header = rows
            .iter()
            .position(|row| *row == "  40 files changed  +40");
        let header = header.expect("the header counts every file");
        let shown: Vec<String> = (0..10).map(|n| format!("  A f{n:02}.rs  +1")).collect();
        assert_eq!(rows[header + 1..header + 11], shown);
        assert_eq!(rows[header + 11], "  … 30 more in Diff");
        assert!(
            !rows.iter().any(|row| row.contains("A f10.rs")),
            "{rows:#?}"
        );

        let before = app.tabs.clone();
        click_text(&mut app, "… 30 more in Diff");
        assert_eq!(app.diff, None);
        assert_eq!(app.tabs, before);
        click_text(&mut app, "A f09.rs");
        assert_eq!(app.diff, Some(9));
    }

    #[test]
    fn tabs_in_a_diff_become_spaces() {
        let tabbed = "@@ -1 +1 @@\n-\told\n+\tnew\n";
        let (app, _intents) = opened(vec![edit("t1", "a.rs", ChangeKind::Update, tabbed)]);
        let rows = app.diff_rows(0);
        assert!(rows.contains(&row(" -    old", Tone::Del)), "{rows:#?}");
        assert!(rows.contains(&row(" +    new", Tone::Add)), "{rows:#?}");
    }

    #[test]
    fn stepping_into_or_out_of_a_subagent_closes_a_diff() {
        let mut app = sample_session();
        click_text(&mut app, "M cli/mod.rs");
        assert_eq!(app.diff, Some(1));
        press(&mut app, "j enter");
        assert_eq!(app.tabs[0].agent, "Reviewer");
        assert_eq!(app.diff, None, "stepping in");

        app.show_file(0);
        press(&mut app, "esc");
        assert_eq!(app.tabs[0].agent, "Engineer");
        assert_eq!(app.diff, None, "stepping out");
    }

    #[test]
    fn picking_the_parent_from_the_sessions_list_closes_a_subagents_diff() {
        let mut app = sample_session();
        press(&mut app, "j enter");
        assert_eq!(app.tabs[0].agent, "Reviewer");
        app.show_file(0);
        assert_eq!(app.diff, Some(0));
        click_text(&mut app, "pack registry");
        assert_eq!(app.tabs[0].agent, "Engineer", "stepped out to the parent");
        assert_eq!(app.diff, None);
    }

    fn view(app: &App) -> Vec<String> {
        let rows = app.view_rows(View::Diff);
        rows.iter().map(|row| row.text.to_string()).collect()
    }

    fn selected_action(app: &App) -> RowAction {
        app.view_rows(View::Diff)[app.view_sel[View::Diff as usize]].action
    }

    /// Turn `t1` edits `a.rs`; turn `t2` adds `b.rs` and edits `a.rs` again.
    fn two_turns() -> Vec<ChatLine> {
        use ChangeKind::{Add, Update};
        vec![
            line("one", Tone::You),
            edit("t1", "a.rs", Update, EDIT),
            line("two", Tone::You),
            edit("t2", "b.rs", Add, "x\ny\n"),
            edit("t2", "a.rs", Update, EDIT),
        ]
    }

    #[test]
    fn the_diff_view_lists_each_file_once_newest_change_first() {
        let (app, _intents) = opened(two_turns());
        assert_eq!(
            view(&app),
            [
                " 2 files   +4 -2",
                "",
                " M a.rs  +2 -2   2 turns",
                " A b.rs  +2"
            ]
        );
        let actions: Vec<RowAction> = app
            .view_rows(View::Diff)
            .iter()
            .map(|row| row.action)
            .collect();
        use RowAction::{None, OpenDiff};
        assert_eq!(actions, [None, None, OpenDiff(0), OpenDiff(1)]);
    }

    #[test]
    fn a_files_diff_shows_each_turns_edits_under_its_prompt() {
        let (app, _intents) = opened(two_turns());
        let rows = app.diff_rows(0);
        assert_eq!(
            texts(&rows),
            [
                " M a.rs  +2 -2",
                "",
                " one",
                " @@ -1 +1 @@",
                " -a",
                " +b",
                "",
                " two",
                " @@ -1 +1 @@",
                " -a",
                " +b",
                "",
                " esc back to files",
            ]
        );
        assert_eq!(rows[2].1, Tone::Accent);
        assert_eq!(rows[7].1, Tone::Accent);
    }

    #[test]
    fn a_cards_row_opens_the_whole_files_diff() {
        let (app, _intents) = opened(two_turns());
        assert_eq!(
            app.tabs[0].files()[2].turn,
            "t2",
            "the card under t2 lists a.rs"
        );
        assert_eq!(app.diff_rows(2), app.diff_rows(0));
        assert_eq!(
            app.diff_rows(2)[0].0,
            " M a.rs  +2 -2",
            "every turn's edits"
        );
    }

    #[test]
    fn a_turn_with_no_prompt_before_it_is_named_by_its_id() {
        let (app, _intents) = opened(vec![edit("t9", "a.rs", ChangeKind::Update, EDIT)]);
        assert_eq!(app.diff_rows(0)[2], row(" turn t9", Tone::Accent));
    }

    #[test]
    fn a_session_without_changes_says_so() {
        let app = with_right_pane(App::new(Vim, 104, 30));
        assert_eq!(view(&app), [" no changes in this session"]);
        let s = screen(&app);
        let rail: Vec<String> = (1..5)
            .map(|y| {
                let row: String = s[y].chars().skip(98).take(5).collect();
                row.trim_end().to_string()
            })
            .collect();
        assert_eq!(rail, [" 0", "", " +0", " -0"]);
    }

    #[test]
    fn a_running_turns_files_list_before_it_ends() {
        let (mut app, _intents) = opened(vec![line("go", Tone::You)]);
        app.apply(Update::Turn {
            session: "s1".to_string(),
            turn: "t2".to_string(),
        });
        app.apply(Update::Item {
            session: "s1".to_string(),
            line: edit("t2", "a.rs", ChangeKind::Update, EDIT),
        });
        assert!(view(&app).contains(&" M a.rs  +1 -1".to_string()));
        let rows = app.conversation_rows(80);
        assert!(
            !rows.iter().any(|(_, tone)| matches!(tone, Tone::File(_))),
            "{rows:#?}"
        );
    }

    #[test]
    fn a_new_turn_keeps_the_selected_file_selected() {
        let (mut app, _intents) = opened(two_turns());
        let rows = app.view_rows(View::Diff);
        let first = rows
            .iter()
            .position(|row| row.action == RowAction::OpenDiff(0));
        app.view_sel[View::Diff as usize] = first.expect("t1's a.rs");
        app.apply(Update::Item {
            session: "s1".to_string(),
            line: edit("t3", "c.rs", ChangeKind::Add, "z\n"),
        });
        assert_eq!(
            view(&app)[2],
            " A c.rs  +1",
            "the newest change lists first"
        );
        assert_eq!(selected_action(&app), RowAction::OpenDiff(0));
    }

    #[test]
    fn the_fortieth_file_opens_from_the_diff_view() {
        let added = (0..40).map(|n| edit("t1", &format!("f{n:02}.rs"), ChangeKind::Add, "x\n"));
        let (app, _intents) = opened(added.collect());
        let mut app = with_right_pane(app);
        let rows = app.view_rows(View::Diff);
        let files = rows.iter().filter(|row| row.selectable());
        assert_eq!(files.count(), 40);
        let last = rows.iter().position(|row| row.text == " A f39.rs  +1");
        let last = last.expect("the fortieth file");
        assert_eq!(rows[last].action, RowAction::OpenDiff(39));

        press(&mut app, "ctrl-w l");
        app.view_sel[View::Diff as usize] = last;
        press(&mut app, "enter");
        assert_eq!(app.diff, Some(39));
        assert_eq!(app.diff_rows(39)[0].0, " A f39.rs  +1");
        assert!(find(&screen(&app), "A f39.rs  +1").is_some());
    }

    #[test]
    fn switching_tabs_shows_that_tabs_changes() {
        use ChangeKind::Add;
        let three = ["a.rs", "b.rs", "c.rs"].map(|path| edit("t1", path, Add, "x\n"));
        let (mut app, _intents) = opened(three.into());
        let second = app.push_tab(Tab::new_session());
        app.tabs[second].session = Some("s2".to_string());
        app.apply(Update::Opened {
            session: "s2".to_string(),
            lines: ["d.rs", "e.rs"]
                .map(|path| edit("t9", path, Add, "y\n"))
                .into(),
        });
        let first = view(&app);
        app.view_sel[View::Diff as usize] = 2;
        assert_eq!(selected_action(&app), RowAction::OpenDiff(2));

        press(&mut app, "g t");
        assert_eq!(app.active, second);
        assert_ne!(view(&app), first);
        assert!(matches!(selected_action(&app), RowAction::OpenDiff(_)));

        app.show_file(0);
        assert_eq!(selected_action(&app), RowAction::OpenDiff(0));
        assert_eq!(view(&app)[app.view_sel[View::Diff as usize]], " A d.rs  +1");
    }

    fn asked(intents: &Receiver<Intent>) -> Vec<Intent> {
        intents.try_iter().collect()
    }

    fn read_tree() -> Intent {
        Intent::WorkingTree {
            agent: "a".to_string(),
        }
    }

    fn in_tree(path: &str, kind: ChangeKind, diff: &str) -> Change {
        Change::new(String::new(), path.to_string(), kind, diff.to_string())
    }

    /// `two_turns` on agent `a`, the right pane showing the Diff view on the working tree.
    fn on_the_tree() -> (App, Receiver<Intent>) {
        let (app, intents) = opened(two_turns());
        let mut app = with_right_pane(app);
        app.tabs[0].agent_id = "a".to_string();
        app.set_scope(Scope::Tree);
        (app, intents)
    }

    fn two_in_the_tree() -> Update {
        Update::WorkingTree(Ok(vec![
            in_tree(
                "src/a.rs",
                ChangeKind::Update,
                "@@ -1 +1,2 @@\n-a\n+b\n+c\n",
            ),
            in_tree("b.rs", ChangeKind::Add, "x\n"),
        ]))
    }

    #[test]
    fn switching_to_the_working_tree_asks_for_it_and_waits() {
        let (mut app, intents) = opened(two_turns());
        app.tabs[0].agent_id = "a".to_string();
        app.set_scope(Scope::Tree);
        assert_eq!(asked(&intents), [read_tree()]);
        assert_eq!(view(&app), [" reading the working tree…"]);

        app.set_view(View::Workflows);
        assert_eq!(asked(&intents), [Intent::Refresh]);
        app.set_view(View::Diff);
        assert_eq!(asked(&intents), [read_tree()], "shown again, read again");
        app.run_effect(Effect::ShowDiff(Scope::Tree));
        assert_eq!(asked(&intents), [read_tree()], "once from the bar");
    }

    #[test]
    fn the_working_tree_lists_the_folders_files() {
        let (mut app, _intents) = on_the_tree();
        app.apply(two_in_the_tree());
        assert_eq!(
            view(&app),
            [
                " 2 files   +3 -1   whole folder",
                "",
                " M src/a.rs  +2 -1",
                " A b.rs      +1",
            ]
        );
        let actions: Vec<RowAction> = app
            .view_rows(View::Diff)
            .iter()
            .map(|row| row.action)
            .collect();
        use RowAction::{None, OpenDiff};
        assert_eq!(actions, [None, None, OpenDiff(0), OpenDiff(1)]);
        assert_eq!(selected_action(&app), OpenDiff(0), "the first file row");
        assert_eq!(
            texts(&app.diff_rows(1)),
            [" A b.rs  +1", "", " +x", "", " esc back to files"]
        );
        assert_eq!(app.diff_rows(2), [row(" esc back to files", Tone::Label)]);
    }

    #[test]
    fn a_tree_file_without_a_body_still_gets_a_row() {
        let (mut app, _intents) = on_the_tree();
        let moved = ChangeKind::Move {
            to: "new.rs".to_string(),
        };
        app.apply(Update::WorkingTree(Ok(vec![
            in_tree("logo.png", ChangeKind::Update, ""),
            in_tree("old.rs", moved, ""),
        ])));
        assert_eq!(
            view(&app),
            [
                " 2 files   whole folder",
                "",
                " M logo.png",
                " R old.rs → new.rs"
            ]
        );
        let said = row(" no diff text recorded", Tone::Label);
        assert_eq!(app.diff_rows(0)[2], said);
        assert_eq!(app.diff_rows(1)[2], said);
    }

    #[test]
    fn a_failed_working_tree_read_says_why() {
        let (mut app, _intents) = on_the_tree();
        app.apply(Update::WorkingTree(Err("not a git repository".to_string())));
        assert_eq!(view(&app), [" no working tree: not a git repository"]);
        app.apply(Update::WorkingTree(Ok(Vec::new())));
        assert_eq!(view(&app), [" no changes in this folder"]);
    }

    #[test]
    fn a_turn_ending_rereads_a_shown_working_tree() {
        let (mut app, intents) = on_the_tree();
        asked(&intents);
        let ended = || Update::TurnEnded {
            session: "another tab's".to_string(),
            turn: "t7".to_string(),
        };
        app.apply(ended());
        assert_eq!(asked(&intents), [read_tree()]);

        app.set_scope(Scope::Session);
        app.apply(ended());
        assert!(
            asked(&intents).is_empty(),
            "the session scope reads nothing"
        );
    }

    fn turn_ended() -> Update {
        Update::TurnEnded {
            session: "s1".to_string(),
            turn: "t2".to_string(),
        }
    }

    #[test]
    fn a_turn_ending_behind_a_closed_pane_reads_no_working_tree() {
        let (mut app, intents) = on_the_tree();
        press(&mut app, "esc ctrl-w ]");
        assert!(!app.tabs[0].sidebar);
        asked(&intents);
        app.apply(turn_ended());
        assert!(asked(&intents).is_empty());

        // The sample answers a read at once, so a read shows as a tree.
        let mut narrow = with_right_pane(App::new(Vim, 104, 30));
        narrow.set_scope(Scope::Tree);
        narrow.working_tree = None;
        narrow.apply(turn_ended());
        assert_eq!(narrow.working_tree, None, "the rail shows no tree");
        press(&mut narrow, "esc ctrl-w ]");
        narrow.apply(turn_ended());
        assert_eq!(narrow.working_tree, Some(Ok(Vec::new())), "the layer does");
    }

    #[test]
    fn showing_the_pane_again_rereads_the_working_tree() {
        let (mut app, intents) = on_the_tree();
        press(&mut app, "esc ctrl-w ]");
        asked(&intents);
        press(&mut app, "ctrl-w ]");
        assert_eq!(asked(&intents), [read_tree()]);

        press(&mut app, "ctrl-w ]");
        app.set_view(View::Workflows);
        asked(&intents);
        press(&mut app, "ctrl-w ]");
        assert!(asked(&intents).is_empty(), "another view reads no tree");
    }

    #[test]
    fn opening_the_layer_rereads_the_working_tree() {
        let (sender, intents) = channel();
        let mut app = with_right_pane(App::connected(Vim, 104, 30, sender));
        app.tabs[0].agent_id = "a".to_string();
        app.set_scope(Scope::Tree);
        asked(&intents);
        click_text(&mut app, "+0");
        assert_eq!(app.layers.len(), 1, "the rail opens the layer");
        assert_eq!(asked(&intents), [read_tree()]);

        press(&mut app, "esc");
        assert!(app.layers.is_empty());
        press(&mut app, "esc ctrl-w l");
        assert_eq!(app.layers.len(), 1, "ctrl-w l opens it too");
        assert_eq!(asked(&intents), [read_tree()]);
    }

    #[test]
    fn an_open_tree_diff_and_the_selection_follow_their_file() {
        let (mut app, _intents) = on_the_tree();
        let a = || in_tree("a.rs", ChangeKind::Update, EDIT);
        let b = || in_tree("b.rs", ChangeKind::Add, "x\n");
        app.apply(Update::WorkingTree(Ok(vec![a(), b()])));
        app.view_sel[View::Diff as usize] = 3;
        app.diff = Some(1);

        let new = in_tree("new.rs", ChangeKind::Add, "n\n");
        app.apply(Update::WorkingTree(Ok(vec![new.clone(), a(), b()])));
        assert_eq!(app.diff, Some(2));
        assert_eq!(app.diff_rows(2)[0].0, " A b.rs  +1");
        assert_eq!(selected_action(&app), RowAction::OpenDiff(2));

        app.apply(Update::WorkingTree(Ok(vec![new, a()])));
        assert_eq!(app.diff, None, "its file is gone");
        assert_eq!(selected_action(&app), RowAction::OpenDiff(1));
    }

    #[test]
    fn a_lost_connection_asks_for_no_working_tree() {
        let (mut app, intents) = opened(two_turns());
        app.apply(Update::Connection(Connection::Lost("gone".to_string())));
        app.set_scope(Scope::Tree);
        app.set_view(View::Diff);
        app.apply(Update::TurnEnded {
            session: "s1".to_string(),
            turn: "t2".to_string(),
        });
        assert!(asked(&intents).is_empty());
    }

    #[test]
    fn a_late_working_tree_only_replaces_the_stored_tree() {
        let (mut app, _intents) = on_the_tree();
        app.apply(two_in_the_tree());
        app.diff = Some(1);
        app.set_scope(Scope::Session);
        assert_eq!(app.diff, None);
        assert_eq!(selected_action(&app), RowAction::OpenDiff(0));

        let late = vec![in_tree("c.rs", ChangeKind::Delete, "z\n")];
        app.apply(Update::WorkingTree(Ok(late.clone())));
        assert_eq!(app.working_tree, Some(Ok(late)));
        assert_eq!(app.scope, Scope::Session);
        assert_eq!(app.diff, None);
        assert_eq!(selected_action(&app), RowAction::OpenDiff(0));
    }
}
