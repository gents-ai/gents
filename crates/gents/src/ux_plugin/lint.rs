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

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

/// One rule hit: which rule, on which line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub rule: &'static str,
    pub line: usize,
    /// The offending text, trimmed to one line.
    pub excerpt: String,
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
                    code.extend_from_slice(b"${");
                    text.extend_from_slice(b"${");
                    frames.push(Frame::Expr { depth: 0 });
                    prev = Some(b'{');
                    word.clear();
                    i += 2;
                }
                b'`' => {
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
                while i < n && bytes[i] != b && bytes[i] != b'\n' {
                    let take = if bytes[i] == b'\\' && i + 1 < n { 2 } else { 1 };
                    for k in 0..take {
                        let c = bytes[i + k];
                        code.push(if c == b'\n' { b'\n' } else { b' ' });
                        text.push(c);
                    }
                    i += take;
                }
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

/// The lint findings for one module's source; empty means admitted.
pub fn lint(source: &str) -> Vec<Finding> {
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
            });
        }
    }
    findings.sort_by_key(|f| (f.line, f.rule));
    findings.dedup();
    findings
}

/// One sentence per finding, for a report or an error.
pub fn describe(findings: &[Finding]) -> String {
    findings
        .iter()
        .map(|f| format!("line {}: {} ({})", f.line, f.rule, f.excerpt))
        .collect::<Vec<_>>()
        .join("\n")
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

    #[test]
    fn findings_carry_the_line() {
        let f = lint("ok()\n\nObject.setPrototypeOf(a, b)\n");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].line, 3);
        assert!(f[0].excerpt.contains("setPrototypeOf"));
    }
}
