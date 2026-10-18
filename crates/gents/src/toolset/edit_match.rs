use std::fmt::Write as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    Exact,
    TrailingWs,
    Trim,
    Unicode,
    Regex,
}

impl Strategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Strategy::Exact => "exact",
            Strategy::TrailingWs => "trailing_whitespace",
            Strategy::Trim => "trim",
            Strategy::Unicode => "unicode",
            Strategy::Regex => "regex",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Replace,
    InsertAfter,
    InsertBefore,
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchMode {
    Ladder,
    Regex,
}

pub struct EditRequest<'a> {
    pub old_text: &'a str,
    pub new_text: &'a str,
    pub replace_all: bool,
    pub operation: Operation,
    pub match_mode: MatchMode,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClosestMatch {
    pub line: usize,
    pub similarity_pct: u8,
    pub first_diff: Option<(String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OccurrencePreview {
    pub line: usize,
    pub text: String,
}

#[derive(Debug, PartialEq)]
pub enum EditOutcome {
    Applied {
        result: String,
        strategy: Strategy,
        replacements: usize,
        first_changed_line: usize,
        diff: String,
    },
    NotFound {
        closest: Option<ClosestMatch>,
    },
    Ambiguous {
        strategy: Strategy,
        count: usize,
        previews: Vec<OccurrencePreview>,
    },
    Noop {
        strategy: Strategy,
    },
    InvalidRegex {
        error: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    CrLf,
}

pub struct NormalizedContent {
    pub text: String,
    pub ending: LineEnding,
    pub had_bom: bool,
}

pub fn normalize_content(raw: &str) -> NormalizedContent {
    let (had_bom, rest) = match raw.strip_prefix('\u{FEFF}') {
        Some(rest) => (true, rest),
        None => (false, raw),
    };
    let crlf = rest.matches("\r\n").count();
    let lf_total = rest.matches('\n').count();
    let ending = if crlf > 0 && crlf * 2 >= lf_total {
        LineEnding::CrLf
    } else {
        LineEnding::Lf
    };
    NormalizedContent {
        text: rest.replace("\r\n", "\n"),
        ending,
        had_bom,
    }
}

pub fn restore_content(text: &str, ending: LineEnding, had_bom: bool) -> String {
    let body = match ending {
        LineEnding::Lf => text.to_string(),
        LineEnding::CrLf => text.replace('\n', "\r\n"),
    };
    if had_bom {
        format!("\u{FEFF}{body}")
    } else {
        body
    }
}

pub fn decide(content: &str, req: &EditRequest<'_>) -> EditOutcome {
    if req.old_text.is_empty() {
        return EditOutcome::NotFound { closest: None };
    }
    match req.match_mode {
        MatchMode::Regex => decide_regex(
            content,
            req.old_text,
            req.new_text,
            req.replace_all,
            req.operation,
        ),
        MatchMode::Ladder => {
            let (pattern, replacement) = desugar(req);
            decide_ladder(content, &pattern, &replacement, req.replace_all)
        }
    }
}

fn desugar(req: &EditRequest<'_>) -> (String, String) {
    let old = req.old_text.to_string();
    match req.operation {
        Operation::Replace => (old, req.new_text.to_string()),
        Operation::InsertAfter => {
            let repl = format!("{}{}", req.old_text, req.new_text);
            (old, repl)
        }
        Operation::InsertBefore => {
            let repl = format!("{}{}", req.new_text, req.old_text);
            (old, repl)
        }
        Operation::Delete => (old, String::new()),
    }
}

fn decide_regex(
    content: &str,
    pattern: &str,
    new_text: &str,
    replace_all: bool,
    operation: Operation,
) -> EditOutcome {
    const REGEX_SIZE_LIMIT: usize = 10 * (1 << 20);
    let regex = match regex::RegexBuilder::new(pattern)
        .size_limit(REGEX_SIZE_LIMIT)
        .build()
    {
        Ok(regex) => regex,
        Err(error) => {
            return EditOutcome::InvalidRegex {
                error: error.to_string(),
            };
        }
    };
    let count = regex.find_iter(content).count();
    if count == 0 {
        return EditOutcome::NotFound { closest: None };
    }
    if count > 1 && !replace_all {
        let previews = regex
            .find_iter(content)
            .take(5)
            .map(|m| {
                let line = 1 + content[..m.start()].matches('\n').count();
                let end = floor_char_boundary(content, m.end().min(m.start() + 120));
                OccurrencePreview {
                    line,
                    text: content[m.start()..end].to_string(),
                }
            })
            .collect();
        return EditOutcome::Ambiguous {
            strategy: Strategy::Regex,
            count,
            previews,
        };
    }
    let render = |caps: &regex::Captures<'_>| -> String {
        let mut expanded = String::new();
        caps.expand(new_text, &mut expanded);
        match operation {
            Operation::Replace => expanded,
            Operation::InsertAfter => format!("{}{expanded}", &caps[0]),
            Operation::InsertBefore => format!("{expanded}{}", &caps[0]),
            Operation::Delete => String::new(),
        }
    };
    let limit = if replace_all { 0 } else { 1 };
    let result = regex.replacen(content, limit, render).into_owned();
    finish(content, result, Strategy::Regex, count.max(1))
}

fn decide_ladder(
    content: &str,
    pattern: &str,
    replacement: &str,
    replace_all: bool,
) -> EditOutcome {
    let exact: Vec<usize> = content.match_indices(pattern).map(|(i, _)| i).collect();
    if !exact.is_empty() {
        if exact.len() > 1 && !replace_all {
            let previews = exact
                .iter()
                .take(5)
                .map(|&i| OccurrencePreview {
                    line: 1 + content[..i].matches('\n').count(),
                    text: line_at_offset(content, i).to_string(),
                })
                .collect();
            return EditOutcome::Ambiguous {
                strategy: Strategy::Exact,
                count: exact.len(),
                previews,
            };
        }
        let result = if replace_all {
            content.replace(pattern, replacement)
        } else {
            content.replacen(pattern, replacement, 1)
        };
        return finish(content, result, Strategy::Exact, exact.len());
    }

    let content_lines: Vec<&str> = content.split('\n').collect();
    let mut pattern_lines: Vec<&str> = pattern.split('\n').collect();
    let mut replacement_lines: Vec<&str> = replacement.split('\n').collect();
    let mut merge_tail = false;
    if pattern_lines.len() > 1 && pattern_lines.last() == Some(&"") {
        pattern_lines.pop();
        if replacement_lines.last() == Some(&"") {
            replacement_lines.pop();
        } else {
            merge_tail = true;
        }
    }
    for strategy in [Strategy::TrailingWs, Strategy::Trim, Strategy::Unicode] {
        let occ = window_occurrences(&content_lines, &pattern_lines, strategy);
        if occ.is_empty() {
            continue;
        }
        if occ.len() > 1 && !replace_all {
            let previews = occ
                .iter()
                .take(5)
                .map(|&i| OccurrencePreview {
                    line: i + 1,
                    text: content_lines[i].to_string(),
                })
                .collect();
            return EditOutcome::Ambiguous {
                strategy,
                count: occ.len(),
                previews,
            };
        }
        let result = splice_windows(
            &content_lines,
            &pattern_lines,
            &replacement_lines,
            strategy,
            &occ,
            merge_tail,
        );
        return finish(content, result, strategy, occ.len());
    }

    EditOutcome::NotFound {
        closest: closest_match(&content_lines, &pattern_lines),
    }
}

fn line_matches(strategy: Strategy, file_line: &str, pattern_line: &str) -> bool {
    match strategy {
        Strategy::Exact | Strategy::Regex => file_line == pattern_line,
        Strategy::TrailingWs => file_line.trim_end() == pattern_line.trim_end(),
        Strategy::Trim => file_line.trim() == pattern_line.trim(),
        Strategy::Unicode => {
            normalize_unicode(file_line.trim()) == normalize_unicode(pattern_line.trim())
        }
    }
}

fn normalize_unicode(line: &str) -> String {
    line.chars()
        .map(|c| match c {
            '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
            '\u{2018}'..='\u{201B}' => '\'',
            '\u{201C}'..='\u{201F}' => '"',
            '\u{00A0}' | '\u{2002}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => ' ',
            other => other,
        })
        .collect()
}

fn window_occurrences(
    content_lines: &[&str],
    pattern_lines: &[&str],
    strategy: Strategy,
) -> Vec<usize> {
    if pattern_lines.is_empty() || pattern_lines.len() > content_lines.len() {
        return Vec::new();
    }
    let mut occurrences = Vec::new();
    let mut next_free = 0;
    for i in 0..=content_lines.len() - pattern_lines.len() {
        if i < next_free {
            continue;
        }
        let matched = pattern_lines
            .iter()
            .enumerate()
            .all(|(k, p)| line_matches(strategy, content_lines[i + k], p));
        if matched {
            occurrences.push(i);
            next_free = i + pattern_lines.len();
        }
    }
    occurrences
}

fn splice_windows(
    content_lines: &[&str],
    pattern_lines: &[&str],
    replacement_lines: &[&str],
    strategy: Strategy,
    occ: &[usize],
    merge_tail: bool,
) -> String {
    let mut lines: Vec<String> = content_lines.iter().map(|l| l.to_string()).collect();
    for &i in occ.iter().rev() {
        let mut repl_lines = reindent_replacement(
            replacement_lines,
            strategy,
            content_lines[i],
            pattern_lines.first().copied().unwrap_or(""),
        );
        let end = i + pattern_lines.len();
        if merge_tail && !repl_lines.is_empty() && end < lines.len() {
            let following = lines[end].clone();
            repl_lines
                .last_mut()
                .expect("non-empty replacement lines")
                .push_str(&following);
            lines.splice(i..end + 1, repl_lines);
        } else {
            lines.splice(i..end, repl_lines);
        }
    }
    lines.join("\n")
}

fn leading_whitespace(line: &str) -> &str {
    &line[..line.len() - line.trim_start().len()]
}

fn reindent_replacement(
    replacement_lines: &[&str],
    strategy: Strategy,
    matched_first: &str,
    pattern_first: &str,
) -> Vec<String> {
    match strategy {
        Strategy::Trim | Strategy::Unicode => {
            let matched_lead = leading_whitespace(matched_first);
            let pattern_lead = leading_whitespace(pattern_first);
            replacement_lines
                .iter()
                .map(|l| {
                    if let Some(rest) = l.strip_prefix(pattern_lead) {
                        format!("{matched_lead}{rest}")
                    } else {
                        l.to_string()
                    }
                })
                .collect()
        }
        _ => replacement_lines.iter().map(|l| l.to_string()).collect(),
    }
}

fn finish(original: &str, result: String, strategy: Strategy, replacements: usize) -> EditOutcome {
    if result == original {
        return EditOutcome::Noop { strategy };
    }
    let first_changed_line = original
        .split('\n')
        .zip(result.split('\n'))
        .position(|(a, b)| a != b)
        .map(|i| i + 1)
        .unwrap_or_else(|| original.split('\n').count().min(result.split('\n').count()));
    let diff = render_diff(original, &result);
    EditOutcome::Applied {
        result,
        strategy,
        replacements,
        first_changed_line,
        diff,
    }
}

pub fn render_diff(original: &str, result: &str) -> String {
    const CONTEXT: usize = 2;
    let old: Vec<&str> = original.split('\n').collect();
    let new: Vec<&str> = result.split('\n').collect();
    let mut prefix = 0;
    while prefix < old.len() && prefix < new.len() && old[prefix] == new[prefix] {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < old.len() - prefix
        && suffix < new.len() - prefix
        && old[old.len() - 1 - suffix] == new[new.len() - 1 - suffix]
    {
        suffix += 1;
    }
    let ctx_start = prefix.saturating_sub(CONTEXT);
    let mut out = String::new();
    let _ = writeln!(out, "@@ line {}", ctx_start + 1);
    for (idx, line) in old.iter().enumerate().take(prefix).skip(ctx_start) {
        let _ = writeln!(out, " {:>5} | {}", idx + 1, line);
    }
    for (idx, line) in old.iter().enumerate().take(old.len() - suffix).skip(prefix) {
        let _ = writeln!(out, "-{:>5} | {}", idx + 1, line);
    }
    for (idx, line) in new.iter().enumerate().take(new.len() - suffix).skip(prefix) {
        let _ = writeln!(out, "+{:>5} | {}", idx + 1, line);
    }
    let ctx_end = (old.len() - suffix + CONTEXT).min(old.len());
    for (idx, line) in old
        .iter()
        .enumerate()
        .take(ctx_end)
        .skip(old.len() - suffix)
    {
        let _ = writeln!(out, " {:>5} | {}", idx + 1, line);
    }
    out.trim_end().to_string()
}

fn floor_char_boundary(s: &str, mut index: usize) -> usize {
    while index > 0 && !s.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn line_at_offset(content: &str, offset: usize) -> &str {
    let start = content[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let end = content[offset..]
        .find('\n')
        .map(|i| offset + i)
        .unwrap_or(content.len());
    &content[start..end]
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(curr[j] + 1);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

fn similarity(a: &str, b: &str) -> f64 {
    let max = a.chars().count().max(b.chars().count());
    if max == 0 {
        return 1.0;
    }
    1.0 - levenshtein(a, b) as f64 / max as f64
}

fn closest_match(content_lines: &[&str], pattern_lines: &[&str]) -> Option<ClosestMatch> {
    const MAX_CLOSEST_WINDOWS: usize = 20_000;
    if pattern_lines.is_empty() || content_lines.len() < pattern_lines.len() {
        return None;
    }
    let windows = (content_lines.len() - pattern_lines.len() + 1).min(MAX_CLOSEST_WINDOWS);
    let mut best: Option<(usize, f64)> = None;
    for i in 0..windows {
        let score: f64 = pattern_lines
            .iter()
            .enumerate()
            .map(|(k, p)| similarity(content_lines[i + k].trim(), p.trim()))
            .sum::<f64>()
            / pattern_lines.len() as f64;
        if best.is_none_or(|(_, b)| score > b) {
            best = Some((i, score));
        }
    }
    let (i, score) = best?;
    let first_diff = pattern_lines.iter().enumerate().find_map(|(k, p)| {
        let file_line = content_lines[i + k];
        if file_line.trim() != p.trim() {
            Some((p.to_string(), file_line.to_string()))
        } else {
            None
        }
    });
    Some(ClosestMatch {
        line: i + 1,
        similarity_pct: (score * 100.0).round().clamp(0.0, 100.0) as u8,
        first_diff,
    })
}
