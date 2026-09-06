# Security-Scan Demo Pack Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A whole-codebase security-scan experiment pack (`packs/security_scan`) with a free Rust regex pre-scan at kickoff, a planner-driven investigator fan-out over trigger edges, an adversarial revalidation barrier, and bound single-collection query tools instead of `defra_query`.

**Architecture:** The `gents pack run` runner gains an optional `scan` manifest section: before seeding, a ported deepsec-style matcher engine (`secscan` module) scans the tree and embeds a formatted candidate payload into the single `ScanJob` seed document. Four trigger-edge stages (plan → investigate ×N → revalidate barrier → report) coordinate purely through documents: `{{ doc.* }}` / `{{ group.docs }}` template injection plus bound `query_*` surface tools.

**Tech Stack:** Rust (gents-cli `secscan` module: `regex` + `ignore` crates), DefraDB pack config documents (GraphQL SDL + JSON), GLM-5.2 over one vLLM backend.

**Spec:** `docs/superpowers/specs/2026-08-17-security-scan-pack-design.md` — read it before executing any task.

## Global Constraints

- Env vars and defaults, verbatim: `${GENTS_SCAN_ROOT:-.}`, `${GENTS_SCAN_MODEL:-GLM-5.2}`, `${GENTS_SCAN_ENDPOINT:-http://127.0.0.1:8080/v1}`, `${GENTS_SCAN_MIN_BATCHES:-4}`, `${GENTS_SCAN_MAX_BATCHES:-24}`.
- One inference backend, `max_concurrent: 8`, shared by all four behaviors.
- No `defra_query` anywhere in the pack: `enable_defra_query: false` in every selection; reads happen only through bound `"kind": "query"` surface entries.
- All pack schema fields are `String`-typed (the seed mutation emits strings only).
- Placeholder DID used in every pack JSON: `did:key:zSecurityScanAgentPlaceholder00000000000000000000000` (runner rebinds with `--bind-agent-did home --force-rebind-concrete-did`).
- Payload discipline: complete inventory, truncated evidence; every truncation counted in `overflow_count`, never silent.
- `finding_id` format: `<batch_id>:<finding-slug>`, where `batch_id` is already `<run_id>:batch-NN` — the run id appears exactly once. Prompts write it as `{{ doc.batch_id }}:<finding-slug>`; never prepend the run id again.
- Durable verdict vocabulary is exactly `confirmed` | `refuted` (runner bijection contract). deepsec's `true-positive | false-positive | fixed | uncertain | duplicate` word goes in the `verification` text field.
- Repo rules (CLAUDE.md): `escape_graphql_string()` for anything interpolated into GraphQL; never emit `[]` in a DefraDB mutation; `tracing`, never `println!` (in runtime code — the demo runner already uses `println!` for operator output, keep matching `pack.rs` local style); gate with `cargo test -p gents` and `cargo check --workspace --all-targets`.
- Commit after every task; never mix the pre-existing bound-query work into pack commits.

---

### Task 0: Commit the pending bound-query surface work

The working tree on `agent/security-scan-pack` already contains the verified bound-query implementation (surface `kind: "query"` entries, `QueryToolDecl`, bounded query execution, desired-state validation). It must land as its own commit before pack work starts, or every later commit tangles with it.

**Files:**
- Modify: none (commit-only task)

- [ ] **Step 1: Confirm the pending set is exactly the bound-query work**

Run: `git status --short`
Expected: modified/new files under `crates/gents{,-cli,-schemas}` belonging to the bound-query feature (notably new `crates/gents/src/defra_query/bounded.rs` and `crates/gents/src/document_config/surface_tool.rs`) plus `docs/superpowers/specs/2026-08-07-datastore-tool-surface-design.md`. The exact list may drift — judge by content, not count: everything dirty must be part of the bound-query work. If anything unrelated is dirty, stop and ask.

- [ ] **Step 2: Run the verification gates**

Run: `cargo test -p gents && cargo check --workspace --all-targets`
Expected: PASS. (The author verified surface serde, create+query merge, bound query correlation/hidden-fill/allowlist, advertisement/registration, and desired-state validation — this re-confirms on this machine.)

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "feat(surface): bound single-collection query tools on DatastoreToolSurface

Query entries set kind:\"query\" on the existing entries column. The model
sees a named tool bound to one collection with an allowlisted projection;
correlation-filled filter fields are hidden and applied as _eq. Also
updates the security-scan pack spec to consume bound query tools and keep
sentinels thin.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 1: `secscan` engine — walk, match, collect

**Files:**
- Create: `crates/gents-cli/src/commands/pack/secscan/mod.rs`
- Create: `crates/gents-cli/src/commands/pack/secscan/matchers.rs`
- Modify: `crates/gents-cli/src/commands/pack/mod.rs` (add `pub(crate) mod secscan;` next to the existing module declarations)
- Modify: `crates/gents-cli/Cargo.toml` (add `ignore = "0.4"` beneath the existing `regex = "1"` dependency)

**Interfaces:**
- Produces (used by Tasks 2–4):

```rust
// secscan/matchers.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum NoiseTier { Precise, Normal, Noisy }

impl NoiseTier { pub(crate) fn label(self) -> &'static str /* "precise" | "normal" | "noisy" */ }

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

pub(crate) fn registry() -> &'static [Matcher];

// secscan/mod.rs
#[derive(Debug, Clone)]
pub(crate) struct CandidateMatch {
    pub slug: &'static str,
    pub tier: NoiseTier,
    pub line: usize,        // 1-based
    pub excerpt: String,    // matched line, trimmed, max 160 chars
}

#[derive(Debug, Clone)]
pub(crate) struct FileCandidates {
    pub path: String,       // relative to scan root, forward slashes
    pub matches: Vec<CandidateMatch>,
}

pub(crate) fn match_content(content: &str, path: &str) -> Vec<CandidateMatch>;
pub(crate) fn scan_root(root: &Path) -> anyhow::Result<Vec<FileCandidates>>;
```

- `scan_root` walks with `ignore::WalkBuilder::new(root)` (respects `.gitignore`, skips hidden dirs and `.git`), reads each UTF-8 file ≤ 1 MiB (skip larger/binary), applies every registry matcher whose extension gate admits the file, and returns files with ≥1 match sorted by path. Multi-line regexes run against full content; match byte offset maps to a 1-based line.

- [ ] **Step 1: Write the failing engine tests** (in `#[cfg(test)] mod tests` at the bottom of `secscan/mod.rs`, matching the in-file test convention of `pack.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_content_maps_offsets_to_lines() {
        let content = "fn ok() {}\nlet api_key = \"sk_live_ABCDEF1234567890\";\n";
        let matches = match_content(content, "src/config.rs");
        assert!(matches.iter().any(|m| m.slug == "secrets-exposure" && m.line == 2),
            "expected secrets-exposure on line 2, got {matches:?}");
    }

    #[test]
    fn extension_gate_excludes_non_matching_files() {
        // graphql-injection is gated to .rs; the same text in a .md must not fire it.
        let content = "format!(\"mutation {{ create_Job(input: {{ run_id: \\\"{run_id}\\\" }}) }}\")";
        let rs = match_content(content, "src/a.rs");
        let md = match_content(content, "docs/a.md");
        assert!(rs.iter().any(|m| m.slug == "graphql-injection"));
        assert!(!md.iter().any(|m| m.slug == "graphql-injection"));
    }

    #[test]
    fn scan_root_respects_gitignore_and_returns_relative_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::create_dir_all(dir.path().join("target")).unwrap();
        std::fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();
        std::fs::write(
            dir.path().join("src/leak.rs"),
            "let api_key = \"sk_live_ABCDEF1234567890\";\n",
        ).unwrap();
        std::fs::write(
            dir.path().join("target/leak.rs"),
            "let api_key = \"sk_live_ABCDEF1234567890\";\n",
        ).unwrap();
        let files = scan_root(dir.path()).expect("scan");
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["src/leak.rs"]);
    }
}
```

`tempfile` is already a dev-dependency of gents-cli (used throughout its tests); if `cargo test` says otherwise, add `tempfile = "3"` to `[dev-dependencies]`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p gents-cli secscan -- --nocapture`
Expected: compile error (module does not exist yet) — that counts as the failing state for a new module.

- [ ] **Step 3: Implement `matchers.rs` with a minimal two-matcher registry**

Just enough for the engine tests: `secrets-exposure` and `graphql-injection` (full set arrives in Task 2). Compile patterns once via `std::sync::OnceLock<Vec<(usize, regex::Regex)>>` keyed by registry index, or simply compile per `match_content` call first and optimize only if tests are slow — correctness first.

```rust
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
```

- [ ] **Step 4: Implement `mod.rs` (`match_content`, `scan_root`)**

`match_content`: for each admitted matcher and pattern, iterate `regex.find_iter(content)`, compute the line via `content[..m.start()].bytes().filter(|b| *b == b'\n').count() + 1`, excerpt = that full line trimmed to 160 chars. Deduplicate: one `CandidateMatch` per (slug, line). `scan_root`: `WalkBuilder::new(root).hidden(true).build()`, skip non-files, skip files over 1 MiB, `String::from_utf8_lossy` is NOT used — use `std::fs::read` + `std::str::from_utf8(...).ok()` and skip non-UTF-8; paths relative via `path.strip_prefix(root)` with `/` separators.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p gents-cli secscan -- --nocapture`
Expected: 3 tests PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/gents-cli/src/commands/pack/secscan crates/gents-cli/src/commands/pack/mod.rs crates/gents-cli/Cargo.toml Cargo.lock
git commit -m "feat(demo): secscan matcher engine walk and match core

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: The curated matcher set + example discovery test

**Files:**
- Modify: `crates/gents-cli/src/commands/pack/secscan/matchers.rs`

**Interfaces:**
- Consumes: `Matcher`, `NoiseTier`, `registry()` from Task 1.
- Produces: `registry()` returns exactly ten matchers with the slugs listed below; Task 3's payload formatter and Task 6's prompts reference these slugs verbatim.

The ten matchers (spec section "Scan engine port"). deepsec's structure, our regexes:

| Slug | Tier | Extensions |
| --- | --- | --- |
| `graphql-injection` | precise | rs |
| `defra-empty-array` | precise | rs |
| `secrets-exposure` | precise | (all) |
| `secret-in-fallback` | precise | rs, ts, tsx, js |
| `insecure-crypto` | precise | (all) |
| `secret-in-log` | normal | rs, ts, tsx, js |
| `command-injection` | normal | rs |
| `webhook-handler` | normal | rs, ts, tsx |
| `path-traversal` | noisy | rs |
| `missing-auth` | noisy | rs |

- [ ] **Step 1: Write the failing discovery test** (deepsec's pattern, ported: every example must fire its own matcher; add to the `#[cfg(test)]` module in `matchers.rs`)

```rust
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
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p gents-cli secscan::matchers -- --nocapture`
Expected: FAIL — `registry_slugs_are_unique_and_complete` (only 2 matchers exist).

- [ ] **Step 3: Implement the eight remaining matchers**

```rust
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
```

If an example fails the discovery test, fix the regex (not the example) unless the example itself is unrealistic — the examples double as documentation of intent.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p gents-cli secscan -- --nocapture`
Expected: all secscan tests PASS (Task 1's three + these two).

- [ ] **Step 5: Sanity-run against this repository** (not a test — a calibration look)

Add nothing to the code; from repo root run a one-off:
`cargo test -p gents-cli secscan -- --nocapture` already covers correctness; for calibration, temporarily add `#[test] #[ignore] fn calibrate() { let files = scan_root(Path::new("../..")).unwrap(); eprintln!("{} candidate files", files.len()); }`, run with `-- --ignored calibrate --nocapture`, note the count in the commit message, then delete the ignored test. Expected order of magnitude: tens of candidate files, not thousands. If it's thousands, tighten the noisy matchers (e.g. require `Router::new()` context for `missing-auth`) before committing.

- [ ] **Step 6: Commit**

```bash
git add crates/gents-cli/src/commands/pack/secscan/matchers.rs
git commit -m "feat(demo): curated secscan matcher set with example discovery test

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: Payload formatter — complete inventory, truncated evidence

**Files:**
- Create: `crates/gents-cli/src/commands/pack/secscan/payload.rs`
- Modify: `crates/gents-cli/src/commands/pack/secscan/mod.rs` (add `mod payload; pub(crate) use payload::{format_payload, ScanOutput};`)

**Interfaces:**
- Consumes: `FileCandidates`, `NoiseTier` from Task 1.
- Produces (used by Task 4):

```rust
#[derive(Debug, Clone)]
pub(crate) struct ScanOutput {
    pub payload: String,               // the model-facing candidates block
    pub candidate_total: usize,        // total matches across all files
    pub candidate_files: usize,        // files with >=1 match
    pub slug_counts: Vec<(String, usize)>, // sorted by count desc, then slug
    pub overflow_count: usize,         // files demoted to path-only lines
}

pub(crate) fn format_payload(files: &[FileCandidates], max_chars: usize) -> ScanOutput;
```

Format (spec "Runner extension"): header lines `files: <candidate_files>  candidates: <candidate_total>` and `slugs: graphql-injection=3(precise) secret-in-log=2(normal) …`, then per-file blocks sorted by best (lowest) tier then path:

```
crates/gents/src/foo.rs
  [precise] graphql-injection L214: format!("... {name} ...")
  [normal]  secret-in-log L88: tracing::info!(token = %token, ...)
```

Cap behavior: build full blocks in sorted order; once adding the next full block would exceed `max_chars`, emit the remaining files as single inventory lines `<path> (slugs: a,b)` and count each such file in `overflow_count`. If even an inventory line does not fit, still emit it — inventory is never dropped (the cap governs evidence, not inventory; a pathological cap yields a payload slightly over `max_chars` rather than silent loss).

- [ ] **Step 1: Write the failing tests** (in `#[cfg(test)]` in `payload.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{CandidateMatch, FileCandidates};
    use super::super::matchers::NoiseTier;

    fn file(path: &str, slug: &'static str, tier: NoiseTier, lines: usize) -> FileCandidates {
        FileCandidates {
            path: path.to_string(),
            matches: (1..=lines).map(|line| CandidateMatch {
                slug, tier, line,
                excerpt: format!("let x = {line}; // {}", "y".repeat(80)),
            }).collect(),
        }
    }

    #[test]
    fn full_payload_sorts_precise_first_and_counts() {
        let files = vec![
            file("z/noisy.rs", "path-traversal", NoiseTier::Noisy, 1),
            file("a/precise.rs", "graphql-injection", NoiseTier::Precise, 2),
        ];
        let out = format_payload(&files, 100_000);
        assert_eq!(out.candidate_total, 3);
        assert_eq!(out.candidate_files, 2);
        assert_eq!(out.overflow_count, 0);
        let precise_pos = out.payload.find("a/precise.rs").unwrap();
        let noisy_pos = out.payload.find("z/noisy.rs").unwrap();
        assert!(precise_pos < noisy_pos, "precise files must sort first");
        assert!(out.slug_counts.iter().any(|(s, n)| s == "graphql-injection" && *n == 2));
    }

    #[test]
    fn cap_demotes_to_inventory_and_counts_overflow() {
        let files: Vec<FileCandidates> = (0..50)
            .map(|i| file(&format!("src/f{i:02}.rs"), "secret-in-log", NoiseTier::Normal, 3))
            .collect();
        let generous = format_payload(&files, 1_000_000);
        let tight = format_payload(&files, generous.payload.len() / 4);
        assert!(tight.overflow_count > 0, "tight cap must demote some files");
        // Inventory is complete: every file path still appears.
        for i in 0..50 {
            let path = format!("src/f{i:02}.rs");
            assert!(tight.payload.contains(&path), "missing inventory for {path}");
        }
        // Counters are cap-independent.
        assert_eq!(tight.candidate_total, generous.candidate_total);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p gents-cli secscan::payload -- --nocapture`
Expected: compile error (`payload` module missing).

- [ ] **Step 3: Implement `format_payload`**

Sort key: `(best_tier(file), path)` where `best_tier` is the minimum `NoiseTier` among the file's matches. Excerpts arrive pre-trimmed (Task 1 caps at 160 chars). Header first, then blocks/inventory as specified.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p gents-cli secscan -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/gents-cli/src/commands/pack/secscan
git commit -m "feat(demo): secscan payload formatter with inventory-preserving cap

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: Runner `scan` manifest section

**Files:**
- Modify: `crates/gents-cli/src/commands/pack/scenario.rs`

**Interfaces:**
- Consumes: `secscan::{scan_root, format_payload, ScanOutput}`.
- Produces: `PackManifest` gains `#[serde(default)] scan: Option<PackScan>`; a pure helper `scan_seed_fields(output: &ScanOutput) -> BTreeMap<String, String>` merged into `manifest.seed.fields` before `seed_mutation` is built.

```rust
#[derive(Debug, Deserialize)]
struct PackScan {
    root: String,
    #[serde(default = "default_scan_payload_chars")]
    max_payload_chars: String, // string for ${VAR:-default} interpolation parity
}

fn default_scan_payload_chars() -> String { "49152".to_string() }

fn scan_seed_fields(output: &secscan::ScanOutput) -> BTreeMap<String, String> {
    // keys: candidates, candidate_total, candidate_files, slug_counts, overflow_count
    // slug_counts rendered as "slug=count slug=count …"
}
```

- [ ] **Step 1: Write the failing tests** (append to the existing `#[cfg(test)]` module in `pack.rs`, which already tests manifest parsing)

```rust
#[test]
fn manifest_parses_optional_scan_section() {
    let manifest: PackManifest = serde_json::from_value(serde_json::json!({
        "name": "t", "init": {"inference_url": "http://x", "model_name": "m"},
        "seed": {"collection": "ScanJob", "job_id_field": "run_id", "prompt_field": "focus"},
        "expect": {"trigger_ids": []},
        "scan": {"root": ".", "max_payload_chars": "1024"}
    })).expect("manifest with scan");
    let scan = manifest.scan.expect("scan section");
    assert_eq!(scan.root, ".");
    assert_eq!(scan.max_payload_chars, "1024");

    let bare: PackManifest = serde_json::from_value(serde_json::json!({
        "name": "t", "init": {"inference_url": "http://x", "model_name": "m"},
        "seed": {"collection": "J", "job_id_field": "run_id", "prompt_field": "focus"},
        "expect": {"trigger_ids": []}
    })).expect("manifest without scan");
    assert!(bare.scan.is_none());
}

#[test]
fn scan_seed_fields_render_all_counters() {
    let output = secscan::ScanOutput {
        payload: "files: 1  candidates: 2\nsrc/a.rs\n  [precise] graphql-injection L3: x".to_string(),
        candidate_total: 2,
        candidate_files: 1,
        slug_counts: vec![("graphql-injection".to_string(), 2)],
        overflow_count: 0,
    };
    let fields = scan_seed_fields(&output);
    assert_eq!(fields.get("candidate_total").map(String::as_str), Some("2"));
    assert_eq!(fields.get("candidate_files").map(String::as_str), Some("1"));
    assert_eq!(fields.get("overflow_count").map(String::as_str), Some("0"));
    assert_eq!(fields.get("slug_counts").map(String::as_str), Some("graphql-injection=2"));
    assert!(fields.get("candidates").unwrap().contains("src/a.rs"));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p gents-cli --lib manifest_parses_optional_scan -- --nocapture` (pack.rs unit tests live in the lib target)
Expected: compile FAIL (`scan` field and `scan_seed_fields` missing).

- [ ] **Step 3: Implement**

Add the `scan` field + `PackScan` + `scan_seed_fields`. In `run()` (around the seed at `pack.rs:2335`), immediately **before** `seed_mutation` is built:

```rust
if let Some(scan) = &manifest.scan {
    let scan_root_path = std::path::Path::new(&scan.root);
    let max_chars: usize = scan.max_payload_chars.parse().context("scan.max_payload_chars")?;
    println!("scanning {} …", scan.root);
    let files = secscan::scan_root(scan_root_path)?;
    let output = secscan::format_payload(&files, max_chars);
    println!(
        "scanned  {} candidate files, {} candidates ({} overflow)",
        output.candidate_files, output.candidate_total, output.overflow_count
    );
    manifest.seed.fields.extend(scan_seed_fields(&output));
}
```

`manifest` needs to be `mut` at that point; the manifest's `scan.root` goes through the same env interpolation as every other manifest string (it does automatically if the whole manifest is interpolated before parse — confirm by finding where `interpolate_with` is applied to the manifest text, and do nothing extra if so). Values in `seed.fields` are escaped downstream by `seed_mutation` via `escape_graphql_string` — do not pre-escape.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p gents-cli secscan && cargo test -p gents-cli --lib -- manifest_parses_optional_scan scan_seed_fields_render`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/gents-cli/src/commands/pack/scenario.rs
git commit -m "feat(demo): optional pre-seed scan section in pack manifests

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 5: Pack wiring — schemas, surfaces, selections, triggers, backend, experiment.json

**Files (all Create):**
- `packs/security_scan/schemas/scan_job.graphql`
- `packs/security_scan/schemas/investigation_batch.graphql`
- `packs/security_scan/schemas/candidate_finding.graphql`
- `packs/security_scan/schemas/investigation_result.graphql`
- `packs/security_scan/schemas/finding_verdict.graphql`
- `packs/security_scan/schemas/revalidation_summary.graphql`
- `packs/security_scan/schemas/finding.graphql`
- `packs/security_scan/schemas/scan_report.graphql`
- `packs/security_scan/agent_principal.json`
- `packs/security_scan/inference_backends/scan_backend/object.json`
- `packs/security_scan/inference_profiles/scan_profile/object.json`
- `packs/security_scan/datastore_tool_surfaces/scan_plan_writes/object.json`
- `packs/security_scan/datastore_tool_surfaces/scan_investigate_writes/object.json`
- `packs/security_scan/datastore_tool_surfaces/scan_revalidate_io/object.json`
- `packs/security_scan/datastore_tool_surfaces/scan_report_io/object.json`
- `packs/security_scan/tool_selections/scan_plan_tools/object.json`
- `packs/security_scan/tool_selections/scan_investigate_tools/object.json`
- `packs/security_scan/tool_selections/scan_revalidate_tools/object.json`
- `packs/security_scan/tool_selections/scan_report_tools/object.json`
- `packs/security_scan/event_triggers/scan_plan/object.json`
- `packs/security_scan/event_triggers/scan_investigate/object.json`
- `packs/security_scan/event_triggers/scan_revalidate/object.json`
- `packs/security_scan/event_triggers/scan_report/object.json`
- `packs/security_scan/experiment.json`
- `packs/security_scan/runs/.gitignore` (contents: `*\n!.gitignore\n`, matching the other packs)

Behaviors and tasks are Task 6 (they carry the prompts); this task creates the wiring and validates it with stub-free references, so behaviors/tasks JSONs are ALSO created here (they reference prompt files by path — create the four `system_prompt.md` / four `prompt.md` files as one-line placeholders in this task, then Task 6 replaces their content; `config validate` needs the files to exist):

- `packs/security_scan/agent_behaviors/{scan-plan,scan-investigate,scan-revalidate,scan-report}/object.json` + `system_prompt.md` (placeholder line: `Replaced in the prompts task.`)
- `packs/security_scan/tasks/{scan-plan-task,scan-investigate-task,scan-revalidate-task,scan-report-task}/object.json` + `prompt.md` (same placeholder)

**Interfaces:**
- Consumes: bound-query entry vocabulary from Task 0's committed work; runner `scan` section from Task 4.
- Produces: collection names, tool names, trigger ids, and env vars used verbatim by Task 6 prompts and Task 7 README.

**Schemas** (each file exactly as shown):

`scan_job.graphql`:
```graphql
type ScanJob {
  run_id: String @index(unique: true) @immutable
  scan_root: String
  focus: String
  candidates: String
  candidate_total: String
  candidate_files: String
  slug_counts: String
  overflow_count: String
  batch_min: String
  batch_max: String
}
```

`investigation_batch.graphql`:
```graphql
type InvestigationBatch {
  run_id: String @index @immutable
  batch_id: String @index(unique: true) @immutable
  scan_root: String @immutable
  paths: String
  hits: String
  instructions: String
  expected_total: String @immutable
}
```

`candidate_finding.graphql`:
```graphql
type CandidateFinding {
  run_id: String @index @immutable
  finding_id: String @index(unique: true) @immutable
  batch_id: String
  severity: String
  confidence: String
  path: String
  line: String
  title: String
  detail: String
  evidence: String
}
```

`investigation_result.graphql`:
```graphql
type InvestigationResult {
  run_id: String @index @immutable
  batch_id: String @index(unique: true) @immutable
  expected_total: String @immutable
  finding_count: String
  summary: String
}
```

`finding_verdict.graphql`:
```graphql
type FindingVerdict {
  run_id: String @index @immutable
  finding_id: String @index(unique: true) @immutable
  batch_id: String
  severity: String
  confidence: String
  path: String
  line: String
  title: String
  detail: String
  verdict: String
  evidence: String
  verification: String
}
```

`revalidation_summary.graphql`:
```graphql
type RevalidationSummary {
  run_id: String @index(unique: true) @immutable
  candidate_count: String
  confirmed_count: String
  refuted_count: String
  summary: String
}
```

`finding.graphql`:
```graphql
type Finding {
  run_id: String @index @immutable
  finding_id: String @index(unique: true) @immutable
  batch_id: String
  severity: String
  confidence: String
  path: String
  line: String
  title: String
  detail: String
  verdict: String
  evidence: String
  verification: String
}
```

`scan_report.graphql`:
```graphql
type ScanReport {
  run_id: String @index(unique: true) @immutable
  candidate_total: String
  batch_count: String
  confirmed_count: String
  refuted_count: String
  severity_counts: String
  slug_counts: String
  summary: String
}
```

**`agent_principal.json`:**
```json
{
  "agent_did": "did:key:zSecurityScanAgentPlaceholder00000000000000000000000",
  "display_name": "Whole-Codebase Security Scan Graph",
  "default_behavior_id": "scan-plan",
  "enabled": true
}
```

**`inference_backends/scan-backend/object.json`** (ONE backend, concurrency 8):
```json
{
  "backend_id": "scan-backend",
  "name": "Security scan backend",
  "provider_kind": "OpenAiCompatible",
  "openai_wire_api": "chat_completions",
  "endpoint": "${GENTS_SCAN_ENDPOINT:-http://127.0.0.1:8080/v1}",
  "api_key": null,
  "api_key_env_var": null,
  "max_concurrent": 8,
  "max_queue_depth": 100,
  "models": ["${GENTS_SCAN_MODEL:-GLM-5.2}"],
  "enabled": true
}
```

**`inference_profiles/scan-profile/object.json`** (single shared profile; values copied from code-review's reviewer profile with `GENTS_SCAN_*` names):
```json
{
  "profile_id": "scan-profile",
  "display_name": "Security scan profile",
  "context_window": ${GENTS_SCAN_CONTEXT_WINDOW:-262144},
  "max_output_tokens": ${GENTS_SCAN_MAX_OUTPUT_TOKENS:-65536},
  "max_turns": ${GENTS_SCAN_MAX_TURNS:-1000000},
  "temperature": ${GENTS_SCAN_TEMPERATURE:-1.0},
  "top_p": ${GENTS_SCAN_TOP_P:-0.95},
  "top_k": null,
  "min_p": null,
  "frequency_penalty": null,
  "stream_batch_ms": ${GENTS_SCAN_STREAM_BATCH_MS:-5000},
  "deadline_duration_secs": ${GENTS_SCAN_DEADLINE_SECS:-86400},
  "stream_liveness_timeout_secs": ${GENTS_SCAN_STREAM_LIVENESS_SECS:-86400},
  "retry_max_transport": ${GENTS_SCAN_RETRY_MAX_TRANSPORT:-720},
  "retry_backoff_ms": [5000, 30000, 120000],
  "retry_max_resample": ${GENTS_SCAN_RETRY_MAX_RESAMPLE:-32},
  "retry_allow_repair": true
}
```

**Surfaces.** `scan-plan-writes`:
```json
{
  "surface_id": "scan-plan-writes",
  "agent_did": "did:key:zSecurityScanAgentPlaceholder00000000000000000000000",
  "display_name": "Investigation batch writes",
  "enabled": true,
  "entries": [
    {
      "tool_name": "write_investigation_batch",
      "collection": "InvestigationBatch",
      "description": "Create one member of the closed investigation-batch set.",
      "output_obligation": {"scope": "trigger", "minimum_writes": 1, "expected_count_field": "expected_total"},
      "fields": [
        {"name": "run_id", "required": false, "fill": "correlation"},
        {"name": "batch_id", "required": true},
        {"name": "scan_root", "required": true},
        {"name": "paths", "required": true},
        {"name": "hits", "required": true},
        {"name": "instructions", "required": true},
        {"name": "expected_total", "required": true}
      ]
    }
  ]
}
```

`scan-investigate-writes`:
```json
{
  "surface_id": "scan-investigate-writes",
  "agent_did": "did:key:zSecurityScanAgentPlaceholder00000000000000000000000",
  "display_name": "Candidate finding and sentinel writes",
  "enabled": true,
  "entries": [
    {
      "tool_name": "write_candidate_finding",
      "collection": "CandidateFinding",
      "description": "Record one evidenced security or bug finding for adversarial revalidation.",
      "fields": [
        {"name": "run_id", "required": false, "fill": "correlation"},
        {"name": "finding_id", "required": true},
        {"name": "batch_id", "required": true},
        {"name": "severity", "required": true},
        {"name": "confidence", "required": true},
        {"name": "path", "required": true},
        {"name": "line", "required": false},
        {"name": "title", "required": true},
        {"name": "detail", "required": true},
        {"name": "evidence", "required": true}
      ]
    },
    {
      "tool_name": "write_investigation_result",
      "collection": "InvestigationResult",
      "description": "Signal that one investigation batch is complete.",
      "output_obligation": {"scope": "trigger", "minimum_writes": 1},
      "fields": [
        {"name": "run_id", "required": false, "fill": "correlation"},
        {"name": "batch_id", "required": true},
        {"name": "expected_total", "required": false, "fill": {"source_field": "expected_total"}},
        {"name": "finding_count", "required": true},
        {"name": "summary", "required": true}
      ]
    }
  ]
}
```

`scan-revalidate-io` (first bound query entry):
```json
{
  "surface_id": "scan-revalidate-io",
  "agent_did": "did:key:zSecurityScanAgentPlaceholder00000000000000000000000",
  "display_name": "Revalidation reads and verdict writes",
  "enabled": true,
  "entries": [
    {
      "kind": "query",
      "tool_name": "query_candidate_finding",
      "collection": "CandidateFinding",
      "description": "Load every candidate finding for this run.",
      "fields": ["finding_id", "batch_id", "severity", "confidence", "path", "line", "title", "detail", "evidence"],
      "filter_fields": [
        {"name": "run_id", "required": false, "fill": "correlation"}
      ]
    },
    {
      "tool_name": "write_finding_verdict",
      "collection": "FindingVerdict",
      "description": "Persist one confirmed or refuted decision for a candidate finding.",
      "fields": [
        {"name": "run_id", "required": false, "fill": "correlation"},
        {"name": "finding_id", "required": true},
        {"name": "batch_id", "required": true},
        {"name": "severity", "required": true},
        {"name": "confidence", "required": true},
        {"name": "path", "required": true},
        {"name": "line", "required": false},
        {"name": "title", "required": true},
        {"name": "detail", "required": true},
        {"name": "verdict", "required": true},
        {"name": "evidence", "required": true},
        {"name": "verification", "required": true}
      ]
    },
    {
      "tool_name": "write_revalidation_summary",
      "collection": "RevalidationSummary",
      "description": "Close the revalidation ledger and trigger the report stage.",
      "output_obligation": {"scope": "trigger", "minimum_writes": 1},
      "fields": [
        {"name": "run_id", "required": false, "fill": "correlation"},
        {"name": "candidate_count", "required": true},
        {"name": "confirmed_count", "required": true},
        {"name": "refuted_count", "required": true},
        {"name": "summary", "required": true}
      ]
    }
  ]
}
```

`scan-report-io`:
```json
{
  "surface_id": "scan-report-io",
  "agent_did": "did:key:zSecurityScanAgentPlaceholder00000000000000000000000",
  "display_name": "Report reads and confirmed finding writes",
  "enabled": true,
  "entries": [
    {
      "kind": "query",
      "tool_name": "query_finding_verdict",
      "collection": "FindingVerdict",
      "description": "Load every revalidation verdict for this run.",
      "fields": ["finding_id", "batch_id", "severity", "confidence", "path", "line", "title", "detail", "verdict", "evidence", "verification"],
      "filter_fields": [
        {"name": "run_id", "required": false, "fill": "correlation"}
      ]
    },
    {
      "tool_name": "write_finding",
      "collection": "Finding",
      "description": "Publish one confirmed finding.",
      "fields": [
        {"name": "run_id", "required": false, "fill": "correlation"},
        {"name": "finding_id", "required": true},
        {"name": "batch_id", "required": true},
        {"name": "severity", "required": true},
        {"name": "confidence", "required": true},
        {"name": "path", "required": true},
        {"name": "line", "required": false},
        {"name": "title", "required": true},
        {"name": "detail", "required": true},
        {"name": "verdict", "required": true},
        {"name": "evidence", "required": true},
        {"name": "verification", "required": true}
      ]
    },
    {
      "tool_name": "write_scan_report",
      "collection": "ScanReport",
      "description": "Publish the final scan report.",
      "output_obligation": {"scope": "trigger", "minimum_writes": 1},
      "fields": [
        {"name": "run_id", "required": false, "fill": "correlation"},
        {"name": "candidate_total", "required": true},
        {"name": "batch_count", "required": true},
        {"name": "confirmed_count", "required": true},
        {"name": "refuted_count", "required": true},
        {"name": "severity_counts", "required": true},
        {"name": "slug_counts", "required": true},
        {"name": "summary", "required": true}
      ]
    }
  ]
}
```

**Tool selections.** Copy `packs/code_review/tool_selections/review-scan-tools/object.json` as the base for each and change only what's listed (all other keys keep code-review's values, e.g. the `enable_lsp`/`lsp_config` pair copied verbatim where LSP is on):

- `scan-plan-tools`: `selection_id: "scan-plan-tools"`, display_name `"Plan: batch candidates and write assignments"`, `enable_file_tools: true`, `file_tools_mode: "ReadOnly"`, `file_tool_root: "${GENTS_SCAN_ROOT:-.}"`, `enable_bash: false`, `command_network_mode: "disabled"`, `enable_lsp: false` (omit `lsp_config`), `backgroundable_tool_names: []`, `datastore_tool_surface_ids: ["scan-plan-writes"]`, `enable_defra_query: false`.
- `scan-investigate-tools`: like review-scan-tools (file ReadOnly at `${GENTS_SCAN_ROOT:-.}`, bash Unrestricted, network `enabled` for cargo/dependency fetches, `enable_lsp: true` + code-review's `lsp_config`, `backgroundable_tool_names: ["bash_unrestricted"]`), `datastore_tool_surface_ids: ["scan-investigate-writes"]`.
- `scan-revalidate-tools`: same as scan-investigate-tools but `command_network_mode: "disabled"` (spec: revalidator has no network) and `datastore_tool_surface_ids: ["scan-revalidate-io"]`.
- `scan-report-tools`: `enable_file_tools: false`, `enable_bash: false`, `command_network_mode: "disabled"`, `enable_lsp: false`, `backgroundable_tool_names: []`, `datastore_tool_surface_ids: ["scan-report-io"]`.

**Behaviors** (this task; prompts placeholder until Task 6). Pattern for all four — shown for `scan-plan`, the others substitute their ids/names/selections:
```json
{
  "behavior_id": "scan-plan",
  "agent_did": "did:key:zSecurityScanAgentPlaceholder00000000000000000000000",
  "display_name": "Scan batch planner",
  "description": "Batches pre-scan candidates into investigation assignments.",
  "summary": null,
  "system_prompt": "./system_prompt.md",
  "request_context_template": null,
  "backend_id": "scan-backend",
  "model_name": "${GENTS_SCAN_MODEL:-GLM-5.2}",
  "tool_selection_id": "scan-plan-tools",
  "inference_profile_id": "scan-profile",
  "compaction_strategy": "StripThenSummarize",
  "compaction_threshold": ${GENTS_SCAN_COMPACTION_THRESHOLD:-0.85},
  "enabled": true,
  "skill_refs": [],
  "skill_excludes": []
}
```
- `scan-investigate`: display_name "Batch investigator", description "Investigates one candidate batch in depth and records findings.", tool_selection_id `scan-investigate-tools`.
- `scan-revalidate`: display_name "Adversarial revalidator", description "Re-checks every candidate finding and stamps verdicts.", tool_selection_id `scan-revalidate-tools`.
- `scan-report`: display_name "Scan reporter", description "Publishes confirmed findings and the run report.", tool_selection_id `scan-report-tools`.

**Tasks** — pattern for all four (`scan-plan-task` shown; others substitute):
```json
{
  "task_id": "scan-plan-task",
  "name": "Plan investigation batches",
  "description": null,
  "behavior_id": "scan-plan",
  "prompt_template": "./prompt.md",
  "enabled": true,
  "output_schema_ref": null
}
```

**Triggers:**

`event_triggers/scan-plan/object.json`:
```json
{
  "trigger_id": "scan-plan",
  "task_id": "scan-plan-task",
  "source_collection": "ScanJob",
  "event_kind": "created",
  "filter": null,
  "correlation_field": "run_id",
  "fire_mode": "per_document",
  "expected_count": null,
  "expected_count_field": null,
  "group_timeout_secs": null,
  "group_min_count": null,
  "enabled": true,
  "concurrency": "serial"
}
```

`event_triggers/scan-investigate/object.json`: same shape with `trigger_id/task_id: scan-investigate(-task)`, `source_collection: "InvestigationBatch"`, `fire_mode: "per_document"`, `concurrency: "parallel"`.

`event_triggers/scan-revalidate/object.json`: `trigger_id/task_id: scan-revalidate(-task)`, `source_collection: "InvestigationResult"`, `fire_mode: "per_group"`, `correlation_field: "run_id"`, `expected_count: null`, `expected_count_field: "expected_total"`, `concurrency: "serial"`.

`event_triggers/scan-report/object.json`: `trigger_id/task_id: scan-report(-task)`, `source_collection: "RevalidationSummary"`, `fire_mode: "per_document"`, `concurrency: "serial"`.

**`experiment.json`:**
```json
{
  "name": "security_scan",
  "description": "Whole-codebase security scan: free regex pre-scan -> batch planner -> investigator fan-out -> adversarial revalidation -> report",
  "init": {
    "inference_url": "${GENTS_SCAN_ENDPOINT:-http://127.0.0.1:8080/v1}",
    "model_name": "${GENTS_SCAN_MODEL:-GLM-5.2}",
    "tool_package": "write",
    "tool_root": "${GENTS_SCAN_ROOT:-.}",
    "tool_root_env_var": "GENTS_SCAN_ROOT",
    "tool_root_markers": [],
    "backend_preset": "vllm",
    "openai_wire_api": "chat-completions"
  },
  "scan": {
    "root": "${GENTS_SCAN_ROOT:-.}",
    "max_payload_chars": "${GENTS_SCAN_MAX_PAYLOAD_CHARS:-49152}"
  },
  "seed": {
    "collection": "ScanJob",
    "job_id_field": "run_id",
    "prompt_field": "focus",
    "fields": {
      "scan_root": "${GENTS_SCAN_ROOT:-.}",
      "batch_min": "${GENTS_SCAN_MIN_BATCHES:-4}",
      "batch_max": "${GENTS_SCAN_MAX_BATCHES:-24}"
    }
  },
  "default_prompt": "${GENTS_SCAN_PROMPT:-Scan the repository for exploitable vulnerabilities and high-impact bugs; prioritize authorization, injection, secrets, and data-loss paths.}",
  "expect": {
    "trigger_ids": ["scan-plan", "scan-investigate", "scan-revalidate", "scan-report"],
    "trigger_request_count_sources": {
      "scan-investigate": {
        "collection": "InvestigationBatch",
        "correlation_field": "run_id",
        "expected_count_field": "expected_total"
      }
    },
    "signed_provenance": true,
    "required_tool_call_trigger_ids": ["scan-plan", "scan-investigate", "scan-revalidate", "scan-report"],
    "prompt_tool_contracts": [
      {"task_id": "scan-plan-task", "required_tool_names": ["write_investigation_batch"]},
      {"task_id": "scan-investigate-task", "required_tool_names": ["write_candidate_finding", "write_investigation_result"]},
      {"task_id": "scan-revalidate-task", "required_tool_names": ["query_candidate_finding", "write_finding_verdict", "write_revalidation_summary"]},
      {"task_id": "scan-report-task", "required_tool_names": ["query_finding_verdict", "write_finding", "write_scan_report"]}
    ],
    "source_edges": [],
    "projections": ["atif", "openai-codex", "langgraph", "multi-agent"],
    "collection_counts": {"RevalidationSummary": 1, "ScanReport": 1},
    "result_documents": [
      {
        "collection": "ScanReport",
        "correlation_field": "run_id",
        "fields": ["candidate_total", "batch_count", "confirmed_count", "refuted_count", "severity_counts", "slug_counts", "summary"]
      },
      {
        "collection": "Finding",
        "correlation_field": "run_id",
        "fields": ["finding_id", "batch_id", "severity", "confidence", "path", "line", "title", "detail", "verdict", "evidence", "verification"]
      }
    ],
    "fan_in": {
      "member_collection": "InvestigationBatch",
      "result_collection": "InvestigationResult",
      "report_collection": "ScanReport",
      "correlation_field": "run_id",
      "expected_count_field": "expected_total",
      "min_expected_count": ${GENTS_SCAN_MIN_BATCHES:-4},
      "max_expected_count": ${GENTS_SCAN_MAX_BATCHES:-24},
      "consumer_trigger_id": "scan-revalidate",
      "member_required_fields": ["batch_id", "scan_root", "paths", "hits", "instructions"],
      "verification": {
        "candidate_collection": "CandidateFinding",
        "decision_collection": "FindingVerdict",
        "summary_collection": "RevalidationSummary",
        "confirmed_collection": "Finding",
        "final_consumer_trigger_id": "scan-report",
        "finding_id_field": "finding_id",
        "verdict_field": "verdict",
        "evidence_field": "evidence",
        "confirmed_count_field": "confirmed_count",
        "refuted_count_field": "refuted_count"
      }
    }
  },
  "await_timeout_secs": ${GENTS_SCAN_AWAIT_TIMEOUT_SECS:-86400}
}
```

- [ ] **Step 1: Create every file above exactly as specified** (placeholder prompt files included)

- [ ] **Step 2: Validate the pack config graph**

Run: `cargo run -p gents-cli --bin gents -- config validate --root packs/security_scan`
Expected: JSON output with `"status": "validated"`, `"ok": true`. Fix any reference errors (typo'd ids) until clean. This exercises the new desired-state validation of query entries against a real pack.

- [ ] **Step 3: Confirm pack discovery**

Run: `cargo run -p gents-cli --bin gents -- pack list`
Expected: `security_scan` appears alongside `pipeline`, `code_review`, `repo_maintenance`, `background_continuation`, `lsp_rust`.

- [ ] **Step 4: Commit**

```bash
git add packs/security_scan
git commit -m "feat(demo): security-scan pack wiring (schemas, surfaces, selections, triggers)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 6: Prompts — the four system prompts and four task templates

**Files (all Modify, replacing Task 5 placeholders):**
- `packs/security_scan/agent_behaviors/scan_plan/system_prompt.md`
- `packs/security_scan/agent_behaviors/scan_investigate/system_prompt.md`
- `packs/security_scan/agent_behaviors/scan_revalidate/system_prompt.md`
- `packs/security_scan/agent_behaviors/scan_report/system_prompt.md`
- `packs/security_scan/tasks/scan_plan_task/prompt.md`
- `packs/security_scan/tasks/scan_investigate_task/prompt.md`
- `packs/security_scan/tasks/scan_revalidate_task/prompt.md`
- `packs/security_scan/tasks/scan_report_task/prompt.md`

**Interfaces:**
- Consumes: tool names, field names, slugs, and env vars from Tasks 2 and 5 — use them verbatim (`write_investigation_batch`, `query_candidate_finding`, `finding_id` = `<run_id>:<batch_id>:<finding-slug>`, etc.).
- Template variables available: `{{ event.correlation }}`, `{{ doc.<field> }}` on per-document triggers, `{{ group.correlation_value }}`, `{{ group.count }}`, `{{ group.complete }}`, `{{ group.docs }}` on the barrier trigger. `.md` files keep runtime `{{ }}` untouched by env interpolation.

Severity vocabulary (deepsec's, adapted — used consistently in investigate/revalidate/report prompts): `CRITICAL` (RCE, auth bypass with full access, injection on sensitive data, SSRF to internal services), `HIGH` (XSS, SSRF, privilege escalation, hardcoded live secrets, insecure deserialization, missing authorization on sensitive operations), `MEDIUM` (open redirect, weak crypto, information disclosure, IDOR, race conditions in auth/permission logic), `HIGH_BUG` (non-security: data loss/corruption/outage), `BUG` (notable non-security logic errors, races, leaks).

- [ ] **Step 1: Write `scan-plan/system_prompt.md`**

```markdown
You are the batch planner for a whole-codebase security scan. A free
regex pre-scan has already flagged candidate files; your only job is to
turn that inventory into a closed set of self-contained investigation
batches. You do not investigate, read code deeply, or judge findings —
the investigator swarm owns that.

You decide the complete batch list and its immutable total before your
first write, then call `write_investigation_batch` once per batch. You
never change cardinality after the first write and never retry a
successful write.
```

- [ ] **Step 2: Write `scan-plan-task/prompt.md`**

```markdown
Scan run {{ event.correlation }} covers the repository at `{{ doc.scan_root }}`.
Focus: {{ doc.focus }}

Pre-scan inventory ({{ doc.candidate_files }} candidate files,
{{ doc.candidate_total }} candidates, slug counts {{ doc.slug_counts }},
overflow {{ doc.overflow_count }}):

{{ doc.candidates }}

Turn this inventory into between {{ doc.batch_min }} and {{ doc.batch_max }}
investigation batches. Rules:

1. About five files per batch. Group related files: same slug family, same
   module or subsystem, or the same vulnerability class. Precise-tier
   candidates lead: they get the earliest batch ids and the tightest
   grouping. Never mix a precise-tier file into a batch of purely noisy
   candidates when a tighter grouping exists.
2. Every candidate file appears in exactly one batch. If `overflow_count`
   is greater than zero, the inventory's path-only lines still get
   assigned — group them by directory affinity and say in the batch
   instructions that they carry no excerpt evidence.
3. You may use read-only file tools to check a path exists or gauge a
   file's size when deciding a grouping; do not read files to pre-judge
   findings, and never use a shell.
4. Decide the full batch list first, then write every batch. For each
   batch call `write_investigation_batch` with:
   - `batch_id`: `{{ event.correlation }}:batch-<two-digit-index>`
   - `scan_root`: exactly `{{ doc.scan_root }}`
   - `paths`: comma-separated relative paths (at most sixteen)
   - `hits`: the inventory lines for those files, verbatim
   - `instructions`: self-contained guidance (at most 8,000 characters)
     naming the fired slugs, what each slug means, and what to verify in
     these specific files
   - `expected_total`: the total batch count, identical on every write
5. Do not supply `run_id`; it is runtime-filled. Do not finish until every
   batch has been written successfully.
```

- [ ] **Step 3: Write `scan-investigate/system_prompt.md`** (adapted from deepsec's investigation template; attribution lands in the README in Task 7)

```markdown
You are a world-class security researcher investigating one batch of
pre-flagged candidate files. You think like an attacker: subtle logic
flaws, auth bypasses via parameter manipulation, trust boundary
violations — not just textbook patterns. Flagged candidates are starting
points; review each assigned file for ANY security issue, especially
what automated tools miss.

Ground rules:
- Inspect, do not exploit. Targeted read-only inspection, git history,
  and targeted tests are allowed. Never attempt to trigger a
  vulnerability against a live system, never send attack traffic, never
  modify the repository.
- Before classifying, check mitigations: sanitization or escaping before
  use, framework guards wrapping the handler directly, trusted-only data
  sources. Fully mitigated is not a finding. Report only genuine,
  evidenced issues.
- Severity vocabulary: CRITICAL (RCE, auth bypass with full access,
  injection on sensitive data, SSRF to internal services), HIGH (XSS,
  SSRF, privilege escalation, hardcoded live secrets, insecure
  deserialization, missing authorization on sensitive operations),
  MEDIUM (open redirect, weak crypto, information disclosure, IDOR,
  auth-adjacent race conditions), HIGH_BUG (non-security data
  loss/corruption/outage), BUG (notable non-security defects).
- In this Rust codebase two flagged patterns are project law: anything
  interpolated into a GraphQL string must pass
  `escape_graphql_string()`, and a DefraDB mutation must never contain
  an empty `[]` literal (it must be `null`). Treat violations as real
  findings.
```

- [ ] **Step 4: Write `scan-investigate-task/prompt.md`**

```markdown
Scan run {{ event.correlation }}, batch `{{ doc.batch_id }}` at
`{{ doc.scan_root }}`. Assigned files: `{{ doc.paths }}`.

Pre-scan hits for this batch:

{{ doc.hits }}

Instructions from the planner: {{ doc.instructions }}

Read every assigned file in full, then follow the data: callers,
consumers, and the places user- or network-controlled values enter the
flagged code. Use `lsp` for definitions, references, and hover when
semantic navigation beats text search. Use targeted read-only shell
commands (`git log`/`git blame`, `cargo test <specific test>`) when they
can settle a claim; background long commands. Do not run the full
workspace build or test suite; do not modify the tree.

Every successful tool result stays authoritative. Never repeat an
identical tool call or reread the same range; if exploration starts to
repeat, stop and write your findings.

For each genuine finding call `write_candidate_finding` with an exact
`path:line`, a verbatim code excerpt in `evidence`, severity from the
fixed vocabulary, `confidence` as an integer string 0–100 (only report
at 60 or above), and `finding_id` exactly
`{{ doc.batch_id }}:<finding-slug>`. At most six findings per batch —
prefer the highest-impact. Zero findings is a valid outcome.

Then call `write_investigation_result` exactly once as your final write:
`batch_id` exactly `{{ doc.batch_id }}`, `finding_count` as an integer
string matching your writes, and a two-sentence `summary`. Do not supply
`run_id` or `expected_total`; both are runtime-filled. Never retry a
successful write.
```

- [ ] **Step 5: Write `scan-revalidate/system_prompt.md`**

```markdown
You are an adversarial revalidator. Investigators have recorded candidate
security findings; your job is to kill the false positives and confirm
the real ones. You re-derive each claim from the code as it exists now —
you never take the investigator's word for it.

For every candidate you deliver exactly one durable verdict:
`confirmed` or `refuted`. Nuance goes in `verification` using deepsec's
vocabulary — true-positive, false-positive, fixed (git history shows a
remediation), uncertain (refute; below the confidence bar), or duplicate
(refute; name the primary finding_id) — followed by your reasoning.
A candidate whose reassessed confidence is below 80 is refuted.
```

- [ ] **Step 6: Write `scan-revalidate-task/prompt.md`**

```markdown
Scan run {{ group.correlation_value }} has {{ group.count }} completed
investigation batches (complete={{ group.complete }}):

{{ group.docs }}

Call `query_candidate_finding` once to load every candidate finding for
this run (the run filter is applied automatically). For each candidate,
in order:

1. Re-read the cited `path:line` and its enclosing function, impl, or
   module, plus relevant callers. Your file tools are already rooted at
   the scanned tree.
2. Check for mitigations the investigator may have missed: escaping,
   guards wrapping the handler directly, trusted-only data paths.
3. Consult git history — was this already fixed after the pre-scan?
4. Check for duplicates: two candidates describing one defect keep the
   strongest as primary; the other is refuted as a duplicate naming the
   primary `finding_id` in `verification`.
5. Immediately call `write_finding_verdict` for that candidate before
   inspecting the next: preserve `finding_id`, `batch_id`, `path`,
   `line`, `title`, `detail` verbatim; reassess `severity` and
   `confidence`; set `verdict` to exactly `confirmed` or `refuted`;
   replace `evidence` with what you verified; explain in `verification`
   starting with one of: true-positive, false-positive, fixed,
   uncertain, duplicate.

Use `lsp` and targeted read-only shell (`git log -p`, `git blame`,
`cargo test <specific test>`) as needed; no network, no modification.
Never repeat an identical tool call. Every candidate gets exactly one
verdict — no more, no fewer.

Finally call `write_revalidation_summary` exactly once, as your last
write: `candidate_count`, `confirmed_count`, `refuted_count` (the three
must balance exactly), and a short `summary`. Do not supply `run_id`;
it is runtime-filled.
```

The prompt uses only `{{ group.* }}` variables that code-review's verify stage already exercises — no array indexing into `group.docs` (the sentinel rows carry no `scan_root`, and the template parser would not accept a numeric segment anyway).

- [ ] **Step 7: Write `scan-report/system_prompt.md`**

```markdown
You are the report stage of a whole-codebase security scan. The
revalidation ledger is closed; you publish the confirmed findings and
one final report. You read no code and run no commands — your only
inputs are the verdicts, and your only outputs are documents.
```

- [ ] **Step 8: Write `scan-report-task/prompt.md`**

```markdown
Scan run {{ event.correlation }} revalidation is closed:
{{ doc.candidate_count }} candidates, {{ doc.confirmed_count }} confirmed,
{{ doc.refuted_count }} refuted. Summary: {{ doc.summary }}

Call `query_finding_verdict` once to load every verdict for this run.
Then:

1. For each verdict with `verdict` equal to `confirmed`, call
   `write_finding` carrying every field forward verbatim (`finding_id`,
   `batch_id`, `severity`, `confidence`, `path`, `line`, `title`,
   `detail`, `verdict`, `evidence`, `verification`). Publish nothing for
   refuted verdicts.
2. Then call `write_scan_report` exactly once as your final write:
   - `candidate_total`: the number of verdicts you loaded
   - `batch_count`: the number of distinct `batch_id` values
   - `confirmed_count` / `refuted_count`: exact tallies of your loaded
     verdicts
   - `severity_counts`: like `CRITICAL=1 HIGH=2 MEDIUM=0 HIGH_BUG=1 BUG=0`
     over confirmed findings
   - `slug_counts`: confirmed findings tallied by the slug portion of
     `finding_id`, same `name=count` format
   - `summary`: at most ten sentences — lead with the most severe
     confirmed findings, then coverage notes.

Do not supply `run_id` on any write; it is runtime-filled. Never retry a
successful write.
```

- [ ] **Step 9: Re-validate and commit**

Run: `cargo run -p gents-cli --bin gents -- config validate --root packs/security_scan`
Expected: `"ok": true`.

```bash
git add packs/security_scan
git commit -m "feat(demo): security-scan behavior and task prompts

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 7: Pack README, attribution, and demo index row

**Files:**
- Create: `packs/security_scan/README.md`
- Modify: `packs/README.md` (packs table)

- [ ] **Step 1: Write `packs/security_scan/README.md`**

Cover, in this order (follow `packs/code_review/README.md`'s voice): the five-stage shape (free pre-scan → plan → investigate ×N → revalidate barrier → report) with the ASCII graph from the spec; the self-sufficient-carrier-documents principle (template injection + bound `query_*` tools, no `defra_query`); the matcher table from Task 2 (slug, tier, what it flags) including the two gents-native matchers and why they exist; env retargeting (`GENTS_SCAN_ROOT`, `GENTS_SCAN_ENDPOINT`, `GENTS_SCAN_MODEL`, `GENTS_SCAN_MIN_BATCHES`, `GENTS_SCAN_MAX_BATCHES`, `GENTS_SCAN_MAX_PAYLOAD_CHARS`); the run command `gents pack run security_scan`; and this attribution notice verbatim:

```markdown
## Attribution

The investigation and revalidation prompt structure, the severity
vocabulary, and the matcher taxonomy are adapted from
[vercel-labs/deepsec](https://github.com/vercel-labs/deepsec),
Apache License 2.0, © 2026 Vercel, Inc. and contributors (see the
upstream NOTICE file). The scan engine here is an independent Rust
implementation of the same scan → process → revalidate → report shape.
```

- [ ] **Step 2: Add the pack to `packs/README.md`'s packs table**

After the `background-continuation` row:

```markdown
| [`security-scan/`](security-scan/README.md) | **Whole-codebase scan** — free regex pre-scan at kickoff, planner batch fan-out, adversarial revalidation barrier, bound query tools instead of `defra_query` |
```

- [ ] **Step 3: Commit**

```bash
git add packs/security_scan/README.md packs/README.md
git commit -m "docs(demo): security-scan pack README and index row

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 8: Live e2e entry and final gates

**Files:**
- Create: `crates/gents-cli/tests/cli_pack_secscan_live.rs`

**Interfaces:**
- Consumes: the complete pack from Tasks 5–7 and the runner extension from Task 4.

- [ ] **Step 1: Write the live test** (gated twice: `#[ignore]` and an env var, matching the `lsp_live.rs` convention; it drives the real `gents` binary because `[[bin]] name = "gents"` lives in this crate)

```rust
//! Live qualification: `gents pack run security_scan` end to end against
//! this repository, on the pack's default GLM-5.2 backend (or whatever
//! GENTS_SCAN_ENDPOINT / GENTS_SCAN_MODEL point at).
//!
//! ```bash
//! GENTS_LIVE_SECSCAN=1 cargo test -p gents-cli --test cli_pack_secscan_live \
//!   -- --ignored --test-threads=1 --nocapture
//! ```

use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

#[test]
#[ignore]
fn demo_run_security_scan_live() {
    if std::env::var("GENTS_LIVE_SECSCAN").as_deref() != Ok("1") {
        eprintln!("GENTS_LIVE_SECSCAN != 1; skipping");
        return;
    }
    let root = repo_root();
    let status = Command::new(env!("CARGO_BIN_EXE_gents"))
        .current_dir(&root)
        .env("GENTS_SCAN_ROOT", &root)
        .args(["pack", "run", "security_scan"])
        .status()
        .expect("spawn gents pack run");
    assert!(status.success(), "demo run security-scan exited {status}");

    // The runner writes runs/<job_id>/meta.json; the newest run must exist
    // and record a results artifact.
    let runs = root.join("packs/security_scan/runs");
    let newest = std::fs::read_dir(&runs)
        .expect("runs dir")
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .expect("at least one run dir");
    let meta = std::fs::read_to_string(newest.path().join("meta.json")).expect("meta.json");
    assert!(meta.contains("scan-report"), "meta.json missing final stage: {meta}");
}
```

- [ ] **Step 2: Compile-check the test without running it**

Run: `cargo test -p gents-cli --test cli_pack_secscan_live --no-run`
Expected: compiles.

- [ ] **Step 3: Run the full gates**

Run: `cargo test -p gents && cargo test -p gents-cli && cargo check --workspace --all-targets`
Expected: all PASS. (The live test stays ignored; everything else runs.)

- [ ] **Step 4: Commit**

```bash
git add crates/gents-cli/tests/cli_pack_secscan_live.rs
git commit -m "test(demo): live e2e entry for the security-scan pack

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

- [ ] **Step 5: Live run (operator step — requires the GLM-5.2 box or a retarget)**

Run: `GENTS_LIVE_SECSCAN=1 cargo test -p gents-cli --test cli_pack_secscan_live -- --ignored --test-threads=1 --nocapture`
Expected: run completes; inspect `packs/security_scan/runs/<job_id>/` — `meta.json` stage states, `results.json` with `ScanReport` + `Finding` rows, projections. If the run fails on a prompt-contract or fan-in expectation, fix the prompt or config (not the expectation) and re-run. Use `--keep-home` via a direct `gents pack run security_scan --keep-home` invocation when a failure needs the node re-opened for querying.
