//! The static lint a UX plugin's module must pass at `gents pack check`
//! and `gents pack build`: the admission tripwire, not a sandbox. A plugin
//! runs in the webview with the app's authority, so what is refused here
//! is the set of things a reviewer should never have to look for: reaching
//! into the app through prototypes, evaluating a second stage, importing
//! anything but the SDK, injecting script tags. After hermes-agent's
//! `hermes_cli/plugin_validate_desktop.py`, rule for rule.
//!
//! Strings and comments are masked before matching, so a plugin's own
//! help text may mention `eval` without being refused; a regex literal is
//! masked too, so a pattern like `/a\/script/` is not read as markup.

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
}

const RULES: &[Rule] = &[
    Rule {
        name: "prototype patching",
        pattern: r"\b[A-Za-z_$][\w$]*\.prototype\.[\w$]+\s*=[^=]",
    },
    Rule {
        name: "prototype patching",
        pattern: r"\bObject\.definePropert(?:y|ies)\(\s*[\w$.]+\.prototype\b",
    },
    Rule {
        name: "prototype patching",
        pattern: r"\b(?:Reflect|Object)\.setPrototypeOf\(|\.__proto__\s*=",
    },
    Rule {
        name: "dynamic code",
        pattern: r"(?:^|[^\w$.])eval\(|\bnew\s+Function\(",
    },
    Rule {
        // a dynamic import of anything but the SDK (a URL, an app chunk):
        // matched as "any import(" and then filtered by `is_sdk_dynamic_import`
        // because the regex crate has no lookahead
        name: "non-sdk import",
        pattern: r#"\bimport\(\s*(?:[^'"\s)][^)]*|['"][^'"]*['"])"#,
    },
    Rule {
        // a static import of a URL scheme (`https:`, `blob:`, `data:`)
        name: "non-sdk import",
        pattern: r#"\bimport\s+(?:[^;'"]*?\bfrom\s*)?['"][a-zA-Z][\w+.-]*:"#,
    },
    Rule {
        name: "script injection",
        pattern: r#"createElement\(\s*['"]script['"]\s*\)|<script\b"#,
    },
];

static COMPILED: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
    RULES
        .iter()
        .map(|rule| {
            (
                rule.name,
                Regex::new(rule.pattern).expect("a lint rule is a valid regex"),
            )
        })
        .collect()
});

/* `(?<![\w)\]])` has no equivalent in the regex crate (no lookbehind), so
the regex-literal mask is a small hand lexer instead: a `/` that opens a
regex literal is one that follows nothing a value could end with. */
fn mask_regex_literals(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'/' && i + 1 < bytes.len() && bytes[i + 1] != b'/' && bytes[i + 1] != b'*' {
            let prev = source[..i].trim_end().bytes().last();
            let opens = !matches!(prev, Some(b) if b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b == b')' || b == b']');
            if opens {
                if let Some(end) = regex_end(bytes, i) {
                    out.push_str(&" ".repeat(end - i));
                    i = end;
                    continue;
                }
            }
        }
        let ch = source[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn regex_end(bytes: &[u8], slash: usize) -> Option<usize> {
    let mut j = slash + 1;
    let mut in_class = false;
    while j < bytes.len() {
        match bytes[j] {
            b'\\' => j += 2,
            b'\n' => return None,
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

static COMMENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)/\*.*?\*/|(?:^|[^:\w])//[^\n]*").unwrap());
static STRING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#""(?:[^"\\\n]|\\.)*"|'(?:[^'\\\n]|\\.)*'|`(?:[^`\\]|\\.)*`"#).unwrap()
});

/* replace a span with spaces so line numbers survive */
fn blank(text: &str, re: &Regex, keep_first_char: bool) -> String {
    re.replace_all(text, |caps: &regex::Captures| {
        let m = caps.get(0).unwrap().as_str();
        let mut masked = String::with_capacity(m.len());
        for (idx, ch) in m.chars().enumerate() {
            masked.push(if ch == '\n' || (keep_first_char && idx == 0) {
                ch
            } else {
                ' '
            });
        }
        masked
    })
    .into_owned()
}

static SDK_DYNAMIC_IMPORT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^import\(\s*['"](?:@gents/ux-sdk|react|react/jsx-runtime)['"]"#).unwrap()
});

/* whether byte `at` of the strings-blanked text is code: blanked text is a
space where a string was, so a non-space there means code */
fn inside_code(no_strings: &str, at: usize) -> bool {
    no_strings.as_bytes().get(at).is_some_and(|b| *b != b' ')
}

/// The lint findings for one module's source; empty means admitted.
pub fn lint(source: &str) -> Vec<Finding> {
    let masked = mask_regex_literals(source);
    let masked = blank(&masked, &COMMENT, true);
    /* import specifiers are strings, and the import rules need to read
    them; the other rules read code only */
    let no_strings = blank(&masked, &STRING, false);
    let mut findings = Vec::new();
    for (name, re) in COMPILED.iter() {
        /* an import specifier and `createElement('script')` are strings the
        rule has to read; the other rules read code only */
        let reads_strings = *name == "non-sdk import" || *name == "script injection";
        let haystack = if reads_strings { &masked } else { &no_strings };
        for m in re.find_iter(haystack) {
            if *name == "non-sdk import" && SDK_DYNAMIC_IMPORT.is_match(m.as_str()) {
                continue;
            }
            /* `<script` inside a plain string is content (an innerHTML
            template, a help message), not an injection: only the
            createElement form, or markup in code, counts */
            if *name == "script injection"
                && m.as_str().starts_with('<')
                && !inside_code(&no_strings, m.start())
            {
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
                rule: name,
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
        assert_eq!(
            rules("document.createElement('script')"),
            vec!["script injection"]
        );
        assert_eq!(
            rules("el.innerHTML = `<script>`"),
            Vec::<&str>::new(),
            "markup in a string is content"
        );
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
