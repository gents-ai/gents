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
    /// Operator documentation, mirrored in demo/security-scan/README.md; not read at runtime.
    #[cfg_attr(not(test), allow(dead_code))]
    pub description: &'static str,
    pub tier: NoiseTier,
    /// File-extension gate; empty slice = all files.
    pub extensions: &'static [&'static str],
    /// Regex sources compiled once at registry build.
    pub patterns: &'static [&'static str],
    /// Snippets this matcher MUST flag; enforced by the discovery test.
    /// Consumed only by the discovery test.
    #[cfg_attr(not(test), allow(dead_code))]
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
        Matcher {
            slug: "defra-empty-array",
            description: "Empty [] literal inside a DefraDB mutation string — types as JsonArray and corrupts nillable array columns; emit null instead.",
            tier: NoiseTier::Precise,
            extensions: &["rs"],
            patterns: &[r#""[^"\n]*:\s*\[\][^"\n]*""#],
            examples: &[r#"let q = "mutation { create_Doc(input: { tags: [] }) { _docID } }";"#],
        },
        Matcher {
            slug: "secret-in-fallback",
            description: "Secret env var read with a hardcoded fallback value.",
            tier: NoiseTier::Precise,
            extensions: &["rs", "ts", "tsx", "js"],
            patterns: &[r#"(?s)env(?:::var)?\s*\(\s*"[A-Z0-9_]*(KEY|SECRET|TOKEN|PASSWORD)[A-Z0-9_]*"\s*\)[^;\n]{0,120}unwrap_or"#,
                        r#"process\.env\.[A-Z0-9_]*(KEY|SECRET|TOKEN|PASSWORD)[A-Z0-9_]*\s*(\|\||\?\?)\s*["'][^"'\n]{4,}["']"#],
            examples: &[r#"let key = std::env::var("API_KEY").unwrap_or("sk_test_default".to_string());"#,
                        r#"const token = process.env.API_TOKEN || "dev-token-1234";"#],
        },
        Matcher {
            slug: "insecure-crypto",
            description: "Weak hash algorithms (MD5/SHA-1) in a security context.",
            tier: NoiseTier::Precise,
            extensions: &[],
            patterns: &[r#"(?i)\b(md5|sha-?1)\s*(::|\()"#],
            examples: &[r#"let digest = md5::compute(data);"#],
        },
        Matcher {
            slug: "secret-in-log",
            description: "Credentials or tokens flowing into log statements.",
            tier: NoiseTier::Normal,
            extensions: &["rs", "ts", "tsx", "js"],
            patterns: &[r#"(?i)(trace|debug|info|warn|error)!\s*\([^;\n]{0,160}(token|secret|password|api_key)"#,
                        r#"(?i)console\.(log|info|warn|error)\s*\([^;\n]{0,160}(token|secret|password|api_key)"#],
            examples: &[r#"tracing::info!(token = %token, "authenticated");"#,
                        r#"console.log("auth", apiToken);"#],
        },
        Matcher {
            slug: "command-injection",
            description: "Shell invocation with interpolated or -c arguments — verify inputs cannot reach the shell.",
            tier: NoiseTier::Normal,
            extensions: &["rs"],
            patterns: &[r#"(?s)Command::new\(\s*"(?:sh|bash|zsh)"\s*\)[^;]{0,120}?"-c""#,
                        r#"\.args?\(\s*&?format!"#],
            examples: &[r#"Command::new("sh").arg("-c").arg(user_input)"#,
                        r#"cmd.arg(format!("git clone {url}"))"#],
        },
        Matcher {
            slug: "webhook-handler",
            description: "Webhook ingress — verify signature/authenticity checks before trusting the payload.",
            tier: NoiseTier::Normal,
            extensions: &["rs", "ts", "tsx"],
            patterns: &[r#"(?i)webhook"#],
            examples: &[r#"async fn webhook_handler(body: Bytes) -> StatusCode {"#],
        },
        Matcher {
            slug: "path-traversal",
            description: "Filesystem join with request/user-derived path segments — verify canonicalization/containment.",
            tier: NoiseTier::Noisy,
            extensions: &["rs"],
            patterns: &[r#"\.join\(\s*&?[A-Za-z_]*(input|param|request|user|name|file|path|arg)[A-Za-z_]*\s*\)"#],
            examples: &[r#"let target = root.join(user_path);"#],
        },
        Matcher {
            slug: "missing-auth",
            description: "HTTP route registration — verify authentication/authorization wraps the handler directly.",
            tier: NoiseTier::Noisy,
            extensions: &["rs"],
            patterns: &[r#"\.route\(\s*""#, r#"#\[(get|post|put|delete|patch)\("#],
            examples: &[r#"let app = Router::new().route("/admin/reset", post(reset_handler));"#],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::demo::secscan::match_content;

    #[test]
    fn every_matcher_example_fires() {
        for matcher in registry() {
            assert!(!matcher.examples.is_empty(), "{}: no examples", matcher.slug);
            // Pick a path admitted by the extension gate.
            let path = match matcher.extensions.first() {
                Some(ext) => format!("example.{ext}"),
                None => "example.rs".to_string(),
            };
            for example in matcher.examples {
                let hits = match_content(example, &path);
                assert!(
                    hits.iter().any(|m| m.slug == matcher.slug),
                    "{}: example did not fire: {example:?} (hits: {hits:?})",
                    matcher.slug
                );
            }
        }
    }

    #[test]
    fn registry_slugs_are_unique_and_complete() {
        let mut slugs: Vec<&str> = registry().iter().map(|m| m.slug).collect();
        let expected = [
            "graphql-injection", "defra-empty-array", "secrets-exposure",
            "secret-in-fallback", "insecure-crypto", "secret-in-log",
            "command-injection", "webhook-handler", "path-traversal", "missing-auth",
        ];
        slugs.sort_unstable();
        let mut want = expected.to_vec();
        want.sort_unstable();
        assert_eq!(slugs, want);
    }
}
