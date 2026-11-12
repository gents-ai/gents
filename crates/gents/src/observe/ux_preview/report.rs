//! What one preview found, as data (`report.json`) and as the text a model
//! reads: the answer, then how to proceed, then metadata.

use std::fmt::Write as _;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::Viewport;

const TEXT_PROBLEM_LINES: usize = 40;
const TEXT_VISIBLE_LINES: usize = 60;

/// One console error or uncaught exception, from the shell or the frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsoleEntry {
    pub level: String,
    pub text: String,
}

/// One data call the page made through the door, and how it answered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DoorCall {
    pub surface: String,
    pub tool: String,
    pub args: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The state of the shell page once its data calls went quiet.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageStatus {
    #[serde(default)]
    pub plugin_id: Option<String>,
    #[serde(default)]
    pub loaded: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub pages: Vec<String>,
    #[serde(default)]
    pub page: Option<String>,
    #[serde(default)]
    pub settled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextLine {
    pub text: String,
    pub offscreen: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clipped {
    /// `past_viewport`, `clipped` or `text_truncated`.
    pub kind: String,
    pub selector: String,
    pub sample: String,
    pub edge: String,
    pub overflow_px: i64,
    pub clipped_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UndefinedToken {
    pub name: String,
    pub used_in: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LowContrast {
    pub ratio: f64,
    pub required: f64,
    pub color: String,
    pub background: String,
    pub selector: String,
    pub sample: String,
    pub count: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Extent {
    pub width: u32,
    pub height: u32,
}

/// What `audit.js` measured inside the frame.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Audit {
    #[serde(default)]
    pub settled: bool,
    #[serde(default)]
    pub viewport: Extent,
    #[serde(default)]
    pub root_scroll: Extent,
    #[serde(default)]
    pub text: Vec<TextLine>,
    #[serde(default)]
    pub text_truncated: bool,
    #[serde(default)]
    pub clipped: Vec<Clipped>,
    #[serde(default)]
    pub clipped_omitted: u64,
    #[serde(default)]
    pub undefined_tokens: Vec<UndefinedToken>,
    #[serde(default)]
    pub undefined_tokens_omitted: u64,
    #[serde(default)]
    pub low_contrast: Vec<LowContrast>,
    #[serde(default)]
    pub low_contrast_omitted: u64,
    #[serde(default)]
    pub contrast_skipped_over_images: u64,
    #[serde(default)]
    pub elements_truncated: bool,
}

/// One theme's rendering.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThemeReport {
    pub theme: String,
    pub png: PathBuf,
    pub status: PageStatus,
    /// The frame's report, absent when no frame was drawn.
    pub audit: Option<Audit>,
    /// Why the frame could not be measured, when it could not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit_error: Option<String>,
    pub console: Vec<ConsoleEntry>,
    pub console_omitted: u64,
    pub data_calls: Vec<DoorCall>,
    pub data_calls_omitted: u64,
}

/// The whole preview.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UxPreviewReport {
    pub target: String,
    /// The installed listing whose grants this rendering ran under.
    pub listing: Option<String>,
    pub file: String,
    pub node_did: String,
    pub viewport: Viewport,
    pub browser: String,
    pub out: PathBuf,
    pub report: PathBuf,
    pub themes: Vec<ThemeReport>,
}

struct Line {
    text: String,
    themes: Vec<String>,
}

fn add(lines: &mut Vec<Line>, text: String, theme: &str) {
    match lines.iter_mut().find(|line| line.text == text) {
        Some(line) => line.themes.push(theme.to_owned()),
        None => lines.push(Line {
            text,
            themes: vec![theme.to_owned()],
        }),
    }
}

impl UxPreviewReport {
    fn problems(&self) -> Vec<Line> {
        let mut lines = Vec::new();
        for report in &self.themes {
            let theme = report.theme.as_str();
            let status = &report.status;
            if !status.loaded {
                let reason = status.error.as_deref().unwrap_or("no reason reported");
                add(
                    &mut lines,
                    format!("the plugin did not load: {reason}"),
                    theme,
                );
            } else if status.page.is_none() {
                add(
                    &mut lines,
                    "the plugin registered no page in plugin.pages; preview renders plugin pages only".to_owned(),
                    theme,
                );
            }
            if let Some(error) = &report.audit_error {
                add(
                    &mut lines,
                    format!("the frame could not be measured: {error}"),
                    theme,
                );
            }
            for entry in &report.console {
                let text = entry.text.split_whitespace().collect::<Vec<_>>().join(" ");
                add(
                    &mut lines,
                    format!("console {}: {text}", entry.level),
                    theme,
                );
            }
            for call in report.data_calls.iter().filter(|call| call.error.is_some()) {
                add(
                    &mut lines,
                    format!(
                        "data call {}/{} failed: {}",
                        call.surface,
                        call.tool,
                        call.error.as_deref().unwrap_or_default()
                    ),
                    theme,
                );
            }
            if report.console_omitted > 0 {
                add(
                    &mut lines,
                    format!(
                        "{} more console errors were not recorded",
                        report.console_omitted
                    ),
                    theme,
                );
            }
            let Some(audit) = &report.audit else {
                continue;
            };
            let omitted =
                audit.clipped_omitted + audit.undefined_tokens_omitted + audit.low_contrast_omitted;
            if omitted > 0 || audit.elements_truncated {
                add(
                    &mut lines,
                    format!(
                        "the audit is partial: {omitted} findings past its limits, elements cut off: {}",
                        audit.elements_truncated
                    ),
                    theme,
                );
            }
            if !status.settled || !audit.settled {
                add(
                    &mut lines,
                    "the page was still changing at capture; a timer or a data call never went quiet".to_owned(),
                    theme,
                );
            }
            for token in &audit.undefined_tokens {
                add(
                    &mut lines,
                    format!(
                        "undefined CSS custom property {} ({})",
                        token.name, token.used_in
                    ),
                    theme,
                );
            }
            for clipped in &audit.clipped {
                let sample = if clipped.sample.is_empty() {
                    String::new()
                } else {
                    format!(" \"{}\"", clipped.sample)
                };
                let what = match clipped.kind.as_str() {
                    "past_viewport" => format!(
                        "runs {}px past the {} edge of the viewport and cannot be scrolled to; the frame's body does not scroll, so the page needs its own overflow: auto container",
                        clipped.overflow_px, clipped.edge
                    ),
                    "text_truncated" => format!("text is cut off by {}px", clipped.overflow_px),
                    _ => format!(
                        "is clipped {}px at the {} edge by {}",
                        clipped.overflow_px,
                        clipped.edge,
                        clipped.clipped_by.as_deref().unwrap_or("an ancestor")
                    ),
                };
                add(
                    &mut lines,
                    format!("{}{sample} {what}", clipped.selector),
                    theme,
                );
            }
            for contrast in &audit.low_contrast {
                add(
                    &mut lines,
                    format!(
                        "low contrast {}:1 (needs {}:1): \"{}\" {} is {} on {} ({} element{})",
                        contrast.ratio,
                        contrast.required,
                        contrast.sample,
                        contrast.selector,
                        contrast.color,
                        contrast.background,
                        contrast.count,
                        if contrast.count == 1 { "" } else { "s" }
                    ),
                    theme,
                );
            }
        }
        lines
    }

    /// `rerun` is what the caller would run to preview again, in the caller's
    /// own form (a shell command, a `config` call).
    pub fn to_text(&self, rerun: &str) -> String {
        let problems = self.problems();
        let rendered = self.themes.iter().filter(|t| t.audit.is_some()).count();
        let mut out = String::new();
        let themes = self
            .themes
            .iter()
            .map(|t| t.theme.as_str())
            .collect::<Vec<_>>()
            .join(" and ");
        let _ = writeln!(
            out,
            "{} at {}x{}: rendered {} of {} themes ({themes}), {} problem{}.",
            self.target,
            self.viewport.width,
            self.viewport.height,
            rendered,
            self.themes.len(),
            problems.len(),
            if problems.len() == 1 { "" } else { "s" }
        );
        if !problems.is_empty() {
            out.push_str("\nProblems:\n");
            for line in problems.iter().take(TEXT_PROBLEM_LINES) {
                let _ = writeln!(out, "  [{}] {}", line.themes.join(", "), line.text);
            }
            if problems.len() > TEXT_PROBLEM_LINES {
                let _ = writeln!(
                    out,
                    "  (+{} more in report.json)",
                    problems.len() - TEXT_PROBLEM_LINES
                );
            }
        }
        out.push_str("\nData calls:\n");
        self.write_data_calls(&mut out);
        self.write_text_section(&mut out);
        let next = if problems.is_empty() {
            format!(
                "\nNext: nothing is flagged. Look at the PNGs below if you can view images; after a change, preview again with {rerun}.\n"
            )
        } else {
            format!(
                "\nNext: fix the problems above in {}, then preview again with {rerun}.\n",
                self.file
            )
        };
        out.push_str(&next);
        out.push_str("\nMetadata:\n");
        let _ = writeln!(out, "  report: {}", self.report.display());
        for theme in &self.themes {
            let _ = writeln!(out, "  {}: {}", theme.theme, theme.png.display());
        }
        let _ = writeln!(out, "  file: {}", self.file);
        let _ = writeln!(
            out,
            "  listing: {}",
            self.listing
                .as_deref()
                .unwrap_or("none (no installed pack plugin has this id; ctx.data has no grants)")
        );
        let _ = writeln!(out, "  node: {}", self.node_did);
        let _ = writeln!(out, "  browser: {}", self.browser);
        out
    }

    fn write_data_calls(&self, out: &mut String) {
        let mut lines = Vec::new();
        for report in &self.themes {
            for call in &report.data_calls {
                let outcome = match (&call.rows, &call.error) {
                    (_, Some(error)) => format!("error: {error}"),
                    (Some(rows), None) => {
                        format!("{rows} row{}", if *rows == 1 { "" } else { "s" })
                    }
                    (None, None) => "answered".to_owned(),
                };
                add(
                    &mut lines,
                    format!("{}/{}: {outcome}", call.surface, call.tool),
                    &report.theme,
                );
            }
        }
        if lines.is_empty() {
            out.push_str("  none ran\n");
        }
        for line in lines {
            let _ = writeln!(
                out,
                "  {} ({} call{})",
                line.text,
                line.themes.len(),
                if line.themes.len() == 1 { "" } else { "s" }
            );
        }
    }

    fn write_text_section(&self, out: &mut String) {
        let Some(report) = self.themes.iter().find(|t| t.audit.is_some()) else {
            return;
        };
        let audit = report.audit.as_ref().expect("filtered on audit");
        let _ = writeln!(
            out,
            "\nVisible text ({}, {} line{}{}):",
            report.theme,
            audit.text.len(),
            if audit.text.len() == 1 { "" } else { "s" },
            if audit.text_truncated {
                ", truncated"
            } else {
                ""
            }
        );
        if audit.text.is_empty() {
            out.push_str("  (the page drew no text)\n");
        }
        for line in audit.text.iter().take(TEXT_VISIBLE_LINES) {
            let marker = if line.offscreen { "[offscreen] " } else { "" };
            let _ = writeln!(out, "  {marker}{}", line.text);
        }
        if audit.text.len() > TEXT_VISIBLE_LINES {
            let _ = writeln!(
                out,
                "  (+{} more in report.json)",
                audit.text.len() - TEXT_VISIBLE_LINES
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme(name: &str, audit: Audit, data_calls: Vec<DoorCall>) -> ThemeReport {
        ThemeReport {
            theme: name.to_owned(),
            png: PathBuf::from(format!("/out/{name}.png")),
            status: PageStatus {
                loaded: true,
                settled: true,
                page: Some("board:page".to_owned()),
                ..PageStatus::default()
            },
            audit: Some(audit),
            audit_error: None,
            console: Vec::new(),
            console_omitted: 0,
            data_calls,
            data_calls_omitted: 0,
        }
    }

    fn report(themes: Vec<ThemeReport>) -> UxPreviewReport {
        UxPreviewReport {
            target: "examples/board_pack/board".to_owned(),
            listing: Some("examples/board_pack/board".to_owned()),
            file: "/pack/plugin.js".to_owned(),
            node_did: "did:key:z".to_owned(),
            viewport: Viewport {
                width: 1280,
                height: 800,
            },
            browser: "/chrome".to_owned(),
            out: PathBuf::from("/out"),
            report: PathBuf::from("/out/report.json"),
            themes,
        }
    }

    fn audit_with_token() -> Audit {
        Audit {
            settled: true,
            undefined_tokens: vec![UndefinedToken {
                name: "--background".to_owned(),
                used_in: "plugin stylesheet: .page".to_owned(),
            }],
            text: vec![TextLine {
                text: "Triage".to_owned(),
                offscreen: false,
            }],
            ..Audit::default()
        }
    }

    #[test]
    fn the_text_report_reads_answer_then_next_then_metadata() {
        let text = report(vec![
            theme("light", audit_with_token(), Vec::new()),
            theme("dark", audit_with_token(), Vec::new()),
        ])
        .to_text("gents config pack ux preview board");
        let answer = text
            .find("rendered 2 of 2 themes (light and dark), 1 problem.")
            .unwrap();
        let problems = text
            .find("[light, dark] undefined CSS custom property --background")
            .unwrap();
        let visible = text.find("Visible text (light").unwrap();
        let next = text
            .find("Next: fix the problems above in /pack/plugin.js")
            .unwrap();
        let metadata = text.find("Metadata:").unwrap();
        assert!(answer < problems && problems < visible && visible < next && next < metadata);
        assert!(text.contains("report: /out/report.json"));
    }

    #[test]
    fn problems_merge_identical_findings_across_themes() {
        let dark_only = Audit {
            low_contrast: vec![LowContrast {
                ratio: 1.2,
                required: 4.5,
                color: "rgb(1, 1, 1)".to_owned(),
                background: "rgb(2, 2, 2)".to_owned(),
                selector: "p".to_owned(),
                sample: "hello".to_owned(),
                count: 3,
            }],
            ..audit_with_token()
        };
        let text = report(vec![
            theme("light", audit_with_token(), Vec::new()),
            theme("dark", dark_only, Vec::new()),
        ])
        .to_text("again");
        assert!(text.contains("2 problems."), "{text}");
        assert!(text.contains("[dark] low contrast 1.2:1"), "{text}");
    }

    #[test]
    fn a_failed_data_call_is_a_problem_and_rows_are_reported() {
        let calls = vec![
            DoorCall {
                surface: "board".to_owned(),
                tool: "list_tasks".to_owned(),
                args: Value::Null,
                rows: Some(3),
                error: None,
            },
            DoorCall {
                surface: "board".to_owned(),
                tool: "list_moves".to_owned(),
                args: Value::Null,
                rows: None,
                error: Some("surface \"board\" is disabled".to_owned()),
            },
        ];
        let text = report(vec![theme(
            "light",
            Audit {
                settled: true,
                ..Audit::default()
            },
            calls,
        )])
        .to_text("again");
        assert!(text.contains("board/list_tasks: 3 rows"), "{text}");
        assert!(
            text.contains(
                "[light] data call board/list_moves failed: surface \"board\" is disabled"
            ),
            "{text}"
        );
    }

    #[test]
    fn a_clean_page_says_nothing_is_flagged() {
        let text = report(vec![theme(
            "light",
            Audit {
                settled: true,
                ..Audit::default()
            },
            Vec::new(),
        )])
        .to_text("again");
        assert!(text.contains("0 problems."), "{text}");
        assert!(text.contains("Next: nothing is flagged."), "{text}");
    }

    #[test]
    fn an_unloaded_plugin_names_the_reason() {
        let mut failed = theme("light", Audit::default(), Vec::new());
        failed.status = PageStatus {
            error: Some("unsupported import: lodash".to_owned()),
            ..PageStatus::default()
        };
        failed.audit = None;
        let text = report(vec![failed]).to_text("again");
        assert!(text.contains("rendered 0 of 1 themes"), "{text}");
        assert!(
            text.contains("the plugin did not load: unsupported import: lodash"),
            "{text}"
        );
    }
}
