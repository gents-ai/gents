//! The one-screen frame `gents eval watch` redraws in place on a terminal:
//! a header with the run's progress, a table per cell, one row per trial in
//! flight and the last trials to finish. Built only from what a watcher can
//! read without the home's node: `progress.json` and the runner's
//! `report.json` (or the report the node derives, once it is free).

use std::collections::BTreeMap;
use std::io::{self, Write};

use gents::eval::report::SlotClass;
use gents::eval::runner::{
    is_fresh, EvidenceRecord, GoalEntry, InFlight, LiveSnapshot, Progress, RunView, SCHEMAS_GOAL,
    STALE_WINDOW,
};
use gents::eval::OutcomeKind;

/// The terminal a frame is drawn on.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Screen {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) color: bool,
}

impl Screen {
    /// Stdout's size, or 120×40 when it cannot be read; color unless
    /// `NO_COLOR` is set.
    pub(crate) fn of_stdout() -> Self {
        let (width, height) = terminal_size().unwrap_or((120, 40));
        Self {
            width: width.max(60),
            height: height.max(12),
            color: std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty()),
        }
    }
}

#[cfg(unix)]
fn terminal_size() -> Option<(usize, usize)> {
    // SAFETY: TIOCGWINSZ writes one `winsize` into the struct it is handed
    // and reads nothing else; a failed call leaves it zeroed.
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    let status = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) };
    (status == 0 && size.ws_col > 0 && size.ws_row > 0)
        .then(|| (usize::from(size.ws_col), usize::from(size.ws_row)))
}

#[cfg(not(unix))]
fn terminal_size() -> Option<(usize, usize)> {
    None
}

/// Hide the cursor while the watch redraws.
pub(crate) const HIDE_CURSOR: &str = "\x1b[?25l";
/// Restore it; written on every way out, Ctrl-C included.
pub(crate) const SHOW_CURSOR: &str = "\x1b[?25h";

/// Draw `lines` over the previous frame: home, each line cleared to its end,
/// then everything below cleared.
pub(crate) fn draw(lines: &[String], out: &mut dyn Write) -> io::Result<()> {
    write!(out, "\x1b[H")?;
    for line in lines {
        write!(out, "{line}\x1b[K\r\n")?;
    }
    write!(out, "\x1b[J")?;
    out.flush()
}

/// What the frame knows about the run when no report can be read: the ids
/// the run directory's frozen definition names.
#[derive(Clone, Debug, Default)]
pub(crate) struct Heading {
    pub(crate) run_id: String,
    pub(crate) definition: Option<(String, i64)>,
    pub(crate) note: Option<String>,
}

/// How many finished trials a frame shows.
const FINISHED: usize = 4;

/// How many of a finished trial's failing checks a frame names.
const FAILING_SHOWN: usize = 2;

pub(crate) fn frame(
    heading: &Heading,
    view: Option<&RunView>,
    progress: Option<&Progress>,
    now: chrono::DateTime<chrono::Utc>,
    screen: Screen,
) -> Vec<String> {
    let paint = Paint(screen.color);
    let width = screen.width;
    let mut lines = Vec::new();
    let slots = in_flight(progress);

    // Header.
    let (definition, created_at) = match view {
        Some(view) => (
            Some((
                view.report.run.definition.definition_id.clone(),
                view.report.run.definition.comparability_version,
            )),
            Some(view.report.run.created_at.as_str()),
        ),
        None => (heading.definition.clone(), None),
    };
    let mut header = heading.run_id.clone();
    if let Some((id, version)) = definition {
        header.push_str(&format!("  {id} v{version}"));
    }
    if let Some(elapsed) = created_at.and_then(|at| seconds_since(at, now)) {
        header.push_str(&format!("  elapsed {}", duration(elapsed)));
    }
    match progress.and_then(|progress| progress.holder.as_ref()) {
        Some(holder) if holder.is_fresh(STALE_WINDOW) => {
            header.push_str(&format!("  runner pid {}", holder.pid));
        }
        Some(holder) => header.push_str(&format!("  runner pid {} (stale)", holder.pid)),
        None => header.push_str("  no runner"),
    }
    lines.push(fit(&header, width));

    let running_by_cell = slots.iter().fold(BTreeMap::new(), |mut by, (_, slot)| {
        *by.entry(slot.cell_id.as_str()).or_insert(0u32) += 1;
        by
    });
    let running: u32 = running_by_cell.values().sum();
    match view {
        Some(view) => {
            let (done, total, queued) = view.report.cells.iter().fold((0, 0, 0), |acc, cell| {
                let counts = &cell.counts;
                (
                    acc.0 + counts.pass + counts.fail + counts.unknown + counts.not_evidence,
                    acc.1 + cell.slots.len() as u32,
                    acc.2 + counts.planned,
                )
            });
            let bar_width = 30.min(width.saturating_sub(50));
            lines.push(fit(
                &format!(
                    "{} {done}/{total} trials done  {running} running  {queued} queued",
                    bar(done, total, bar_width)
                ),
                width,
            ));
        }
        None => lines.push(fit(
            &format!(
                "{running} running  {}",
                heading
                    .note
                    .as_deref()
                    .unwrap_or("no report yet: totals appear once the runner writes one")
            ),
            width,
        )),
    }
    lines.push(String::new());

    // Cells.
    lines.push(fit(
        &format!(
            "{:<16} {:>5} {:>5} {:>5} {:>6} {:>8} {:>15} {:>13}",
            "cell", "pass", "fail", "run", "queue", "mean", "tokens in/out", "tools ok/fail"
        ),
        width,
    ));
    match view {
        Some(view) => {
            for cell in &view.report.cells {
                let (ok, failed) = cell_tools(view, &cell.cell_id, &slots);
                let running = running_by_cell
                    .get(cell.cell_id.as_str())
                    .copied()
                    .unwrap_or(0);
                let row = format!(
                    "{:<16} {:>5} {:>5} {:>5} {:>6} {:>8} {:>15} {:>13}",
                    clip(&cell.cell_id, 16),
                    cell.counts.pass,
                    cell.counts.fail,
                    running,
                    cell.counts.planned,
                    percent(cell.headline_bp),
                    format!(
                        "{}/{}",
                        compact(cell.usage.input_tokens),
                        compact(cell.usage.output_tokens)
                    ),
                    format!("{ok}/{failed}"),
                );
                lines.push(fit(&row, width));
            }
        }
        None => {
            for (cell_id, running) in &running_by_cell {
                lines.push(fit(
                    &format!(
                        "{:<16} {:>5} {:>5} {:>5} {:>6} {:>8} {:>15} {:>13}",
                        clip(cell_id, 16),
                        "-",
                        "-",
                        running,
                        "-",
                        "-",
                        "-",
                        "-"
                    ),
                    width,
                ));
            }
        }
    }
    lines.push(String::new());

    // Finished, drawn last but budgeted first so the in-flight rows fill
    // what is left.
    let finished = finished(view, paint, width);
    let budget = screen
        .height
        .saturating_sub(lines.len() + 2 + finished.len() + 1);

    lines.push(fit(
        &format!(
            "in flight {:<4} {:<14} {:<3} {:<7} {:>6} {:>12} {:>8}  docs vs goal",
            slots.len(),
            "case",
            "#",
            "stage",
            "time",
            "tokens",
            "tools"
        ),
        width,
    ));
    let (mut used, mut drawn) = (0, 0);
    for (trial_id, slot) in &slots {
        let rows = slot_rows(trial_id, slot, now, paint, width);
        if used + rows.len() > budget {
            break;
        }
        used += rows.len();
        drawn += 1;
        lines.extend(rows);
    }
    if drawn < slots.len() {
        lines.push(format!("  … {} more in flight", slots.len() - drawn));
    }
    lines.push(String::new());
    lines.extend(finished);
    // A frame taller than the screen would scroll its header away.
    lines.truncate(screen.height.saturating_sub(1));
    lines
}

/// The slots in flight, in a stable order: cell, case, trial, attempt.
fn in_flight(progress: Option<&Progress>) -> Vec<(&String, &InFlight)> {
    let mut slots: Vec<(&String, &InFlight)> = progress
        .map(|progress| progress.slots.iter().collect())
        .unwrap_or_default();
    slots.sort_by(|left, right| {
        (
            &left.1.cell_id,
            &left.1.case_id,
            left.1.trial_index,
            left.1.attempt,
            left.0,
        )
            .cmp(&(
                &right.1.cell_id,
                &right.1.case_id,
                right.1.trial_index,
                right.1.attempt,
                right.0,
            ))
    });
    slots
}

/// A slot's row, and the line naming its last tool call when it has one.
fn slot_rows(
    trial_id: &str,
    slot: &InFlight,
    now: chrono::DateTime<chrono::Utc>,
    paint: Paint,
    width: usize,
) -> Vec<String> {
    let live = slot.live.as_ref();
    let elapsed = live
        .map(|live| live.elapsed_secs)
        .or_else(|| seconds_since(&slot.started_at, now));
    let stale = if is_fresh(slot, STALE_WINDOW) {
        ""
    } else {
        " (stale)"
    };
    let mut row = format!(
        "  {:<10} {:<14} {:<3} {:<7} {:>6} {:>12} {:>8}",
        clip(&slot.cell_id, 10),
        clip(&slot.case_id, 14),
        slot.trial_index,
        clip(slot.stage_id.as_deref().unwrap_or("-"), 7),
        elapsed.map_or_else(|| "?".to_owned(), duration),
        live.map_or_else(
            || "-".to_owned(),
            |live| format!(
                "{}/{}",
                compact(live.input_tokens),
                compact(live.output_tokens)
            )
        ),
        live.map_or_else(
            || "-".to_owned(),
            |live| format!(
                "{}/{}",
                live.tool_calls.saturating_sub(live.failed_tool_calls),
                live.failed_tool_calls
            )
        ),
    );
    let docs = docs_vs_goal(live, &slot.goal);
    if !docs.is_empty() {
        row.push_str("  ");
        row.push_str(&docs);
    }
    row.push_str(stale);
    if live.is_none() && slot.goal.is_empty() {
        row.push_str(&format!("  ({})", short_id(trial_id)));
    }
    let mut rows = vec![fit(&row, width)];
    if let Some(last) = live.and_then(|live| live.last_tool.as_ref()) {
        let state = if last.failed() {
            "err"
        } else {
            last.state.as_deref().unwrap_or("-")
        };
        let text = fit(
            &format!(
                "    └ {} {state}: {}",
                last.tool_name,
                last.result.as_deref().unwrap_or("")
            ),
            width,
        );
        rows.push(if last.failed() {
            text.replacen(" err: ", &format!(" {}: ", paint.red("err")), 1)
        } else {
            text
        });
    }
    rows
}

/// `beh 7/9  task 12/22  trig 9/18`: each goal entry, then the snapshot's
/// other counts, compactly named.
fn docs_vs_goal(live: Option<&LiveSnapshot>, goal: &[GoalEntry]) -> String {
    let mut items: Vec<String> = goal
        .iter()
        .map(|entry| {
            let observed = live.and_then(|live| entry.observed(live));
            format!(
                "{} {}",
                abbreviation(&entry.collection),
                entry.label(observed)
            )
        })
        .collect();
    if let Some(live) = live.filter(|_| goal.is_empty()) {
        items.extend(
            live.documents
                .iter()
                .filter(|(collection, _)| {
                    !goal.iter().any(|entry| &entry.collection == *collection)
                })
                .filter(|(_, rows)| **rows > 0)
                .map(|(collection, rows)| format!("{} {rows}", abbreviation(collection))),
        );
        if !goal.iter().any(|entry| entry.collection == SCHEMAS_GOAL) && !live.schemas.is_empty() {
            items.push(format!("schema {}", live.schemas.len()));
        }
    }
    items.join(" ")
}

/// The short name a frame gives a configuration collection.
fn abbreviation(collection: &str) -> &str {
    match collection {
        "Agent" => "beh",
        "AgentContext" => "ctx",
        "Tools" => "tools",
        "InferenceProfile" => "prof",
        "InferenceExecution" => "exec",
        "AgentTarget" => "sub",
        "DatastoreToolSurface" => "surf",
        "EventSource" => "src",
        "Trigger" => "trig",
        "Task" => "task",
        "Schedule" => "sched",
        SCHEMAS_GOAL => "schema",
        other => other,
    }
}

/// Tool calls ok and failed across a cell: its finished trials' final
/// snapshots and its in-flight trials' latest.
fn cell_tools(view: &RunView, cell_id: &str, slots: &[(&String, &InFlight)]) -> (u64, u64) {
    let finished = view
        .report
        .cells
        .iter()
        .filter(|cell| cell.cell_id == cell_id)
        .flat_map(|cell| &cell.slots)
        .filter_map(|slot| slot.latest.as_ref())
        .filter_map(|latest| view.trials.get(&latest.trial_id))
        .filter_map(|record| record.live.as_ref());
    let running = slots
        .iter()
        .filter(|(_, slot)| slot.cell_id == cell_id)
        .filter_map(|(_, slot)| slot.live.as_ref());
    finished.chain(running).fold((0, 0), |(ok, failed), live| {
        (
            ok + live.tool_calls.saturating_sub(live.failed_tool_calls),
            failed + live.failed_tool_calls,
        )
    })
}

/// The last trials to finish, latest first, one line each with what their
/// failing checks observed against what they expected.
fn finished(view: Option<&RunView>, paint: Paint, width: usize) -> Vec<String> {
    let Some(view) = view else {
        return Vec::new();
    };
    let mut rows: Vec<_> = view
        .report
        .cells
        .iter()
        .flat_map(|cell| {
            cell.slots
                .iter()
                .map(move |slot| (cell.cell_id.as_str(), slot))
        })
        .filter(|(_, slot)| {
            !matches!(slot.class, SlotClass::Planned | SlotClass::Abandoned)
                && slot.latest.is_some()
        })
        .map(|(cell_id, slot)| {
            let record: Option<&EvidenceRecord> = slot
                .latest
                .as_ref()
                .and_then(|latest| view.trials.get(&latest.trial_id));
            (cell_id, slot, record)
        })
        .collect();
    rows.sort_by(|left, right| {
        let ended = |record: Option<&EvidenceRecord>| record.and_then(|r| r.ended_at.clone());
        ended(right.2).cmp(&ended(left.2))
    });
    let mut lines = vec![if rows.len() > FINISHED {
        format!("finished (latest {FINISHED} of {})", rows.len())
    } else {
        format!("finished {}", rows.len())
    }];
    for (cell_id, slot, record) in rows.into_iter().take(FINISHED) {
        let class = match slot.class {
            SlotClass::Pass => "PASS",
            SlotClass::Fail => "FAIL",
            SlotClass::Unknown => "UNKN",
            _ => "N/E ",
        };
        let live = record.and_then(|record| record.live.as_ref());
        let failing: Vec<String> = slot
            .verdicts
            .iter()
            .filter(|verdict| verdict.kind != OutcomeKind::Passed)
            .map(|verdict| {
                format!(
                    "{} {}",
                    verdict.check,
                    verdict
                        .detail
                        .clone()
                        .or_else(|| verdict.reason_code.clone())
                        .unwrap_or_else(|| verdict.kind.as_str().to_owned())
                )
            })
            .collect();
        let line = format!(
            "  {:<10} {:<14} {:<3} {class} {:>7} {:>6} {:>12} {:>8}  {}",
            clip(cell_id, 10),
            clip(&slot.case_id, 14),
            slot.trial_index,
            percent(slot.score_bp),
            live.map_or_else(|| "-".to_owned(), |live| duration(live.elapsed_secs)),
            live.map_or_else(
                || "-".to_owned(),
                |live| format!(
                    "{}/{}",
                    compact(live.input_tokens),
                    compact(live.output_tokens)
                )
            ),
            live.map_or_else(
                || "-".to_owned(),
                |live| format!(
                    "{}/{}",
                    live.tool_calls.saturating_sub(live.failed_tool_calls),
                    live.failed_tool_calls
                )
            ),
            if failing.is_empty() {
                "all checks passed".to_owned()
            } else {
                format!("{} failing", failing.len())
            }
        );
        let painted = match slot.class {
            SlotClass::Pass => paint.green(class),
            SlotClass::Fail => paint.red(class),
            _ => class.to_owned(),
        };
        lines.push(fit(&line, width).replacen(&format!(" {class} "), &format!(" {painted} "), 1));
        for failing in failing.iter().take(FAILING_SHOWN) {
            lines.push(fit(&format!("    └ {failing}"), width));
        }
    }
    lines
}

#[derive(Clone, Copy)]
struct Paint(bool);

impl Paint {
    fn green(self, text: &str) -> String {
        self.wrap("32", text)
    }

    fn red(self, text: &str) -> String {
        self.wrap("31", text)
    }

    fn wrap(self, code: &str, text: &str) -> String {
        if self.0 {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_owned()
        }
    }
}

/// `[██████░░░░]` over `width` cells.
fn bar(done: u32, total: u32, width: usize) -> String {
    let filled = if total == 0 {
        0
    } else {
        (done as usize * width) / total as usize
    };
    format!(
        "[{}{}]",
        "█".repeat(filled),
        "░".repeat(width - filled.min(width))
    )
}

/// `line` cut to `width` chars, marked when cut.
fn fit(line: &str, width: usize) -> String {
    match line.char_indices().nth(width.saturating_sub(1)) {
        Some((at, _)) if line.chars().count() > width => format!("{}…", &line[..at]),
        _ => line.to_owned(),
    }
}

fn clip(text: &str, width: usize) -> String {
    fit(text, width)
}

fn seconds_since(timestamp: &str, now: chrono::DateTime<chrono::Utc>) -> Option<u64> {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .map(|then| {
            (now - then.with_timezone(&chrono::Utc))
                .num_seconds()
                .max(0) as u64
        })
}

pub(crate) fn duration(seconds: u64) -> String {
    match seconds {
        0..=99 => format!("{seconds}s"),
        100..=3599 => format!("{}m{:02}s", seconds / 60, seconds % 60),
        _ => format!("{}h{:02}m", seconds / 3600, seconds % 3600 / 60),
    }
}

/// `9999`, `812.3k`, `1.24M`, or `-` when unknown.
pub(crate) fn compact(value: Option<u64>) -> String {
    match value {
        None => "-".to_owned(),
        Some(value @ 0..=9_999) => value.to_string(),
        Some(value @ 10_000..=999_999) => format!("{:.1}k", value as f64 / 1_000.0),
        Some(value) => format!("{:.2}M", value as f64 / 1_000_000.0),
    }
}

fn percent(bp: Option<u32>) -> String {
    super::render::percent(bp)
}

/// Enough of a trial id to tell slots apart on one line.
pub(crate) fn short_id(trial_id: &str) -> &str {
    trial_id.get(..12).unwrap_or(trial_id)
}
