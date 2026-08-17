//! Static registry of regex-based security matchers.
//!
//! Patterns are grouped by `NoiseTier`, gated by file extension, and each
//! carries example snippets the discovery test uses to enforce that the
//! matcher actually fires on the case it claims to catch.

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum NoiseTier {
    Precise,
    Normal,
    Noisy,
}

impl NoiseTier {
    pub(crate) fn label(self) -> &'static str {
        match self {
            NoiseTier::Precise => "precise",
            NoiseTier::Normal => "normal",
            NoiseTier::Noisy => "noisy",
        }
    }
}

pub(crate) struct Matcher {
    pub slug: &'static str,
    pub description: &'static str,
    pub tier: NoiseTier,
    /// File-extension gate; empty slice = all files.
    pub extensions: &'static [&'static str],
    /// Regex sources compiled once at registry build.
    pub patterns: &'static [&'static str],
    /// Snippets this matcher MUST flag; enforced by the discovery test.
    pub examples: &'static [&'static str],
}

pub(crate) fn registry() -> &'static [Matcher] {
    &[
        Matcher {
            slug: "secrets-exposure",
            description: "Hardcoded API keys, tokens, or passwords in source.",
            tier: NoiseTier::Precise,
            extensions: &[],
            patterns: &[r#"(?i)(api[_-]?key|secret|token|password)\s*[:=]\s*"[A-Za-z0-9+/_\-]{16,}""#],
            examples: &[r#"let api_key = "sk_live_ABCDEF1234567890";"#],
        },
        Matcher {
            slug: "graphql-injection",
            description: "GraphQL built with format! interpolation — verify escape_graphql_string is applied to every interpolated value.",
            tier: NoiseTier::Precise,
            extensions: &["rs"],
            patterns: &[r#"(?s)format!\s*\([^;]{0,200}?(?:mutation|query)\s*\{"#],
            examples: &[r#"format!("mutation {{ create_Job(input: {{ run_id: \"{run_id}\" }}) }}")"#],
        },
    ]
}
