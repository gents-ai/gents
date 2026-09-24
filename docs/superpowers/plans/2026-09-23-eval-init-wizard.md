# `gents eval init` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `gents eval init <subject> --out <dir>` interviews the operator through a model, drafts an eval definition pack, validates it against the contract, the check registry and the subject, writes it to disk, and with `--pilot` runs it once and lets the author revise from the evidence.

**Architecture:** One new CLI module `commands/eval/init/` split by responsibility (dossier, contract, draft parsing, validation, writing, the turn abstraction, the interview loop, the pilot). The author is an `AgentRequest` on the served operator home with behavior `eval-author` from a new bundled `eval_author` pack. The library gains `Check::describe`, `CheckRegistry::catalog`, eval-case sidecar hydration in the pack loader, and a `purpose` on `RunHeader` so pilots do not count as exposure.

**Tech Stack:** Rust, clap, serde_json, `jsonschema` 0.46 (already a gents-cli dependency), tokio, the existing chat turn plumbing (`create_agent_request`, `stream_turn_progress`), the eval runner and report libraries.

**Spec:** `docs/superpowers/specs/2026-09-23-eval-init-wizard-design.md`

**Base branch:** `feat/eval-cli` (#1658). One PR, `feat/eval-init-wizard`, targeting `feat/eval-cli`.

## Global Constraints

- Branch name `feat/eval-init-wizard`; no milestone names anywhere; no `Co-Authored-By` trailers; commit as `git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com"`.
- Escape every interpolated GraphQL string with `graphql::escape_graphql_string()`; never emit `[]` in a mutation; `tracing`, never `println!` in library code (the CLI prints with `writeln!(out, …)` on the `out` writer the eval commands take, and `eprint!` for prompts as `interactive_backend` does).
- The author holds no tools and no write grants. Nothing reaches `--out` or live configuration before validation passes.
- Nothing in the wizard names a check. The catalog is rendered from `CheckRegistry::builtin()`.
- Only pertinent tests per task (`cargo test -p gents-cli --lib eval::init` or the named test); `cargo test -p gents` and `cargo check --workspace --all-targets` once at the PR gate, plus `cargo fmt --all --check`.
- One cargo invocation at a time, `CARGO_BUILD_JOBS=4`, foreground only.
- Rebase note for the held side branch: Task 2 copies `hydrate_eval_cases` from `test/monitor-findings-eval` commit `617bde8fc`; when that branch rebases onto this PR, its copy is dropped in favour of this one.

## File structure

| Path | Responsibility |
|---|---|
| `crates/gents/src/eval/checks/mod.rs` | `CheckDescription`, `Check::describe`, `CheckRegistry::catalog` |
| `crates/gents/src/eval/checks/captured_rows_count.rs` | its `describe` |
| `crates/gents/src/pack/loader.rs`, `loader/tests.rs` | eval-case sidecar hydration |
| `crates/gents/src/eval/scoring.rs`, `eval/report/store.rs`, `eval/report/compare.rs` | `RunHeader.purpose`, exposure skips pilots, compare refuses pilots |
| `packs/eval_author/*`, `packs/catalog.json` | the author behavior and the authoring contract |
| `crates/gents-cli/src/cli/args.rs` | `EvalInitArgs`, `EvalChecksArgs`, `EvalCommand::{Init, Checks}`, `--include-pilot` on compare |
| `crates/gents-cli/src/commands/eval/mod.rs` | dispatch |
| `crates/gents-cli/src/commands/eval/checks.rs` | `gents eval checks` |
| `crates/gents-cli/src/commands/eval/init/mod.rs` | the command: resolve, install author, interview loop, exit codes |
| `crates/gents-cli/src/commands/eval/init/dossier.rs` | subject dossier rendering |
| `crates/gents-cli/src/commands/eval/init/contract.rs` | the first turn: dossier + catalog + contract text |
| `crates/gents-cli/src/commands/eval/init/draft.rs` | fenced-block parsing, `Draft` |
| `crates/gents-cli/src/commands/eval/init/validate.rs` | steps 2 to 6 of spec section 4 |
| `crates/gents-cli/src/commands/eval/init/write.rs` | temp dir, loader round trip, move to `--out`, README |
| `crates/gents-cli/src/commands/eval/init/turn.rs` | `Turn` trait, live impl over chat plumbing, scripted impl for tests |
| `crates/gents-cli/src/commands/eval/init/pilot.rs` | install the pack, run, digest, revision |

---

### Task 1: `Check::describe`, `CheckRegistry::catalog`, `gents eval checks`

**Files:**
- Modify: `crates/gents/src/eval/checks/mod.rs`
- Modify: `crates/gents/src/eval/checks/captured_rows_count.rs`
- Modify: `crates/gents-cli/src/cli/args.rs` (add `Checks(EvalChecksArgs)` to `EvalCommand` and its `scope()` arm)
- Create: `crates/gents-cli/src/commands/eval/checks.rs`
- Modify: `crates/gents-cli/src/commands/eval/mod.rs` (`mod checks;`, dispatch arm)

**Interfaces:**
- Produces: `pub struct CheckDescription { pub name: String, pub version: String, pub summary: String, pub params_schema: serde_json::Value, pub reads: Vec<String>, pub reason_codes: Vec<(String, String)> }` (Serialize, Deserialize, Clone, Debug, PartialEq); `fn describe(&self) -> CheckDescription` on `Check`; `pub fn catalog(&self) -> Vec<CheckDescription>` on `CheckRegistry`, sorted by name.

- [ ] **Step 1: Failing tests in `checks/mod.rs`**

```rust
#[cfg(test)]
mod catalog_tests {
    use super::*;

    #[test]
    fn every_builtin_check_describes_itself_with_a_schema_and_reason_codes() {
        let catalog = CheckRegistry::builtin().catalog();
        assert_eq!(
            catalog.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            CheckRegistry::builtin().names()
        );
        for check in &catalog {
            assert_eq!(check.params_schema["type"], "object", "{}", check.name);
            assert!(!check.summary.is_empty(), "{}", check.name);
            assert!(!check.reason_codes.is_empty(), "{}", check.name);
        }
    }

    #[test]
    fn captured_rows_count_schema_accepts_its_params_and_rejects_unknown_fields() {
        let schema = CheckRegistry::builtin()
            .get("captured_rows_count")
            .unwrap()
            .describe()
            .params_schema;
        let validator = jsonschema::validator_for(&schema).unwrap();
        assert!(validator.is_valid(&serde_json::json!({"name": "items", "min": 1})));
        assert!(!validator.is_valid(&serde_json::json!({"name": "items"})));
        assert!(!validator.is_valid(&serde_json::json!({"name": "items", "min": 1, "extra": 1})));
    }
}
```

`jsonschema` is a dev-dependency of `gents` already (`crates/gents/Cargo.toml:145`); confirm it is under `[dev-dependencies]` or `[dependencies]`; if dev-only, keep the validator test in gents-cli (Task 6) and assert only the schema shape here.

- [ ] **Step 2: Run** `cargo test -p gents --lib eval::checks::catalog_tests` → FAIL (no `describe`).

- [ ] **Step 3: Implement**

In `checks/mod.rs`:

```rust
/// What a check tells an author about itself. Rendered into the catalog an
/// eval author drafts against; never read by `evaluate`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CheckDescription {
    pub name: String,
    pub version: String,
    pub summary: String,
    /// JSON Schema (draft 2020-12) for the check ref's `params`.
    pub params_schema: Value,
    /// What the check reads: `"capture:documents"`, `"capture:files"`,
    /// `"stage:terminal_state"`, or a field path such as `"rows.payload"`.
    pub reads: Vec<String>,
    /// `(reason_code, one line)` for every code `raw.reason_code` can carry.
    pub reason_codes: Vec<(String, String)>,
}

pub trait Check: Send + Sync {
    fn name(&self) -> &'static str;
    fn version(&self) -> &'static str;
    fn evaluate(&self, params: &Value, stage: &StageEvidence) -> CheckVerdict;
    /// The check's entry in the author-facing catalog.
    fn describe(&self) -> CheckDescription;
}

impl CheckRegistry {
    /// Every registered check's description, sorted by name.
    pub fn catalog(&self) -> Vec<CheckDescription> {
        self.checks.values().map(|check| check.describe()).collect()
    }
}
```

In `captured_rows_count.rs`:

```rust
fn describe(&self) -> CheckDescription {
    CheckDescription {
        name: self.name().into(),
        version: self.version().into(),
        summary: "Passes when a documents capture holds between min and max rows (no upper bound when max is absent).".into(),
        params_schema: json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "the capture name"},
                "min": {"type": "integer", "minimum": 0},
                "max": {"type": "integer", "minimum": 0}
            },
            "required": ["name", "min"],
            "additionalProperties": false
        }),
        reads: vec!["capture:documents".into()],
        reason_codes: vec![
            ("in_range".into(), "the count satisfies min and max".into()),
            ("below_min".into(), "fewer rows than min".into()),
            ("above_max".into(), "more rows than max".into()),
            ("missing_capture".into(), "grader: no documents capture of that name".into()),
            ("bad_params".into(), "grader: params did not parse or max < min".into()),
        ],
    }
}
```

Add `use crate::eval::checks::CheckDescription;` and `serde::{Serialize, Deserialize}` imports where needed. Any test-only checks implementing `Check` (search `impl Check for` under `#[cfg(test)]` in `eval/`) get a minimal `describe` returning an empty-properties object schema.

- [ ] **Step 4: The subcommand.** In `args.rs`, after `Gc`:

```rust
#[command(about = "List the checks a definition may name, with their params schema and reason codes")]
Checks(EvalChecksArgs),
```

```rust
#[derive(clap::Args)]
pub(crate) struct EvalChecksArgs {
    #[arg(long)]
    pub(crate) json: bool,
    #[command(flatten)]
    pub(crate) scope: EvalScopeArgs,
}
```

and `Self::Checks(args) => &args.scope` in `scope()`. `commands/eval/checks.rs`:

```rust
//! `gents eval checks`: the builtin catalog, from the registry alone.
use std::io::Write;
use anyhow::Result;
use gents::eval::checks::CheckRegistry;
use crate::cli::EvalChecksArgs;

pub(super) fn checks(registry: &CheckRegistry, args: &EvalChecksArgs, out: &mut dyn Write) -> Result<()> {
    let catalog = registry.catalog();
    if args.json {
        return super::write_json(out, &catalog);
    }
    for check in &catalog {
        writeln!(out, "{} v{}  {}", check.name, check.version, check.summary)?;
        writeln!(out, "  params  {}", serde_json::to_string(&check.params_schema)?)?;
        writeln!(out, "  reads   {}", check.reads.join(", "))?;
        for (code, line) in &check.reason_codes {
            writeln!(out, "  reason  {code}: {line}")?;
        }
    }
    Ok(())
}
```

Dispatch: `EvalCommand::Checks(args) => checks::checks(deps.registry, &args, out)` inside `execute` (it needs no context; keep it there for uniformity, `EvalContext::resolve` is cheap). Test in `checks.rs`: `--json` output parses back into `Vec<CheckDescription>` equal to `CheckRegistry::builtin().catalog()`.

- [ ] **Step 5: Run** the two test modules → PASS. **Commit:** `feat(eval): checks describe themselves; gents eval checks lists the catalog`.

---

### Task 2: eval-case sidecar hydration in the pack loader

**Files:**
- Modify: `crates/gents/src/pack/loader.rs`
- Modify: `crates/gents/src/pack/loader/tests.rs`

Copy `hydrate_eval_cases` and its call from `git show 617bde8fc -- crates/gents/src/pack/loader.rs crates/gents/src/pack/loader/tests.rs` (the held side branch's first commit) verbatim, including the tests `eval_case_sidecars_are_inlined_at_load` and `an_eval_case_sidecar_outside_the_pack_is_refused` (names as in that commit). The function reads each string entry of `eval_definitions[].cases` as a `./` sidecar path through `read_sidecar(Collection::EvalDefinition, definition_id, path)`, parses it as one `EvalCase`, and replaces the string with the inlined case; a non-`./` string is refused.

- [ ] **Step 1:** apply the diff; **Step 2:** `cargo test -p gents --lib pack::loader` → PASS. **Commit:** `feat(pack): an eval definition's cases may be ./ sidecar files`.

---

### Task 3: pilots do not count as exposure; compare refuses them

**Files:**
- Modify: `crates/gents/src/eval/scoring.rs` (`RunHeader`, `exposure`)
- Modify: `crates/gents/src/eval/report/store.rs` (`run_header`)
- Modify: `crates/gents/src/eval/report/compare.rs`
- Modify: `crates/gents-cli/src/cli/args.rs` (`EvalCompareArgs.include_pilot: bool`)
- Modify: `crates/gents-cli/src/commands/eval/compare.rs`

**Interfaces:**
- Produces: `pub const PILOT_PURPOSE: &str = "pilot";` in `gents::eval::scoring`; `RunHeader { …, pub purpose: String }`.

- [ ] **Step 1: Failing tests.** In `scoring.rs` tests: two headers with the same definition and split, one `purpose: "pilot"`, exposure is 1. In `compare.rs` tests (use `report::fixtures`): a report whose `run.purpose == "pilot"` is refused by `compare` with a message containing `pilot`, unless the new `CompareOptions { include_pilot: true }` is passed.

- [ ] **Step 2:** run them → FAIL.

- [ ] **Step 3: Implement.** `RunHeader` gains `pub purpose: String`; `run_header` fills it from `record.origin.purpose`; `exposure` adds `&& run.purpose != PILOT_PURPOSE`. `compare` gains a trailing `options: &CompareOptions` parameter (`#[derive(Default)] pub struct CompareOptions { pub include_pilot: bool }`); when `!options.include_pilot` and either report's `run.purpose == PILOT_PURPOSE`, return `refused(format!("run {} is a pilot (purpose {:?}); pilots are drafts of a definition, not evidence; pass --include-pilot to compare it anyway", …))`. Update every `compare(` caller (`commands/eval/compare.rs`, the optimization driver's use in `gents::optimization` if it calls `report::compare`, and tests) to pass `&CompareOptions::default()`. `EvalCompareArgs` gains `#[arg(long)] pub(crate) include_pilot: bool`, threaded through.

- [ ] **Step 4:** `cargo test -p gents --lib eval::scoring eval::report` and `cargo test -p gents-cli --lib eval::compare` → PASS. **Commit:** `feat(eval): pilot runs are not exposure and compare refuses them by default`.

---

### Task 4: the `eval_author` pack

**Files:**
- Create: `packs/eval_author/manifest.json`, `packs/eval_author/pack_config.json`, `packs/eval_author/agent_behaviors/eval_author/system_prompt.md`, `packs/eval_author/README.md`
- Modify: `packs/catalog.json` (add `"eval_author"` in sorted position)

`manifest.json`:

```json
{
  "manifest_version": 1,
  "name": "eval_author",
  "version": "1.0.0",
  "description": "The eval author: interviews an operator and drafts an eval definition for a subject behavior. Installed by gents eval init.",
  "authors": ["gents-ai contributors"],
  "tags": ["eval"],
  "kind": "documents",
  "assets": ["README.md", "agent_behaviors/eval_author/system_prompt.md", "pack_config.json"],
  "config": "pack_config.json",
  "inference_slots": [{"name": "author", "description": "Runs the eval author.", "behaviors": ["eval-author"]}]
}
```

`pack_config.json`:

```json
{
  "agent_principal": {},
  "agent_behaviors": [{
    "behavior_id": "eval-author",
    "display_name": "Eval author",
    "description": "Interviews the operator and drafts eval cases for one subject behavior.",
    "context_id": "eval-author-context",
    "inference_profile_id": "gents:inference-slot:author"
  }],
  "contexts": [{
    "context_id": "eval-author-context",
    "display_name": "Eval author",
    "system_prompt": "./agent_behaviors/eval_author/system_prompt.md",
    "tools_id": "eval-author-tools"
  }],
  "tools": [{
    "tools_id": "eval-author-tools",
    "display_name": "Eval author: no tools",
    "host": {"bash": {"mode": "Off"}}
  }]
}
```

(`Tools` with only bash Off is the shape `commands/eval/testing.rs::write_pack` uses; it yields no callable tool.)

`system_prompt.md` is the authoring contract of spec section 3, written out. It must contain, in this order, with these headings: `## What you are drafting` (vocabulary: definition, case, stage, capture, check; reducers `all`, `weighted_mean`, `last_stage`), `## The case shape` (one complete worked example: one case, one stage, one documents capture, two checks, in a fenced json block), `## Rules` (the seven rules from spec section 3, numbered), `## The interview` (the five questions; one or two per turn; draft when answered or told "draft"), `## How to reply with a draft` (exactly one fenced ```json block, top level `{"definition": {"definition_id": …, "title": …}, "cases": [ … ]}`, nothing else in that turn; the CLI sets subject and version), `## When the catalog cannot grade something` (say so in prose; never invent a check name). The dossier and catalog arrive in the first user turn under the headings `# Subject` and `# Check catalog`; the prompt says so.

- [ ] **Step 1:** write the files; **Step 2:** `cargo test -p gents --lib pack::tests::every_configuration_pack_declares_slots_and_authors_no_inference_documents` → PASS (the pack declares a slot and no inference documents). `cargo build -p gents` compiles the bundle (build.rs asserts the catalog). **Commit:** `feat(pack): the eval_author pack`.

---

### Task 5: the subject dossier

**Files:**
- Create: `crates/gents-cli/src/commands/eval/init/mod.rs` (module skeleton: `pub(super) mod dossier; mod contract; mod draft; mod validate; mod write; mod turn; mod pilot;` plus the command in Task 8)
- Create: `crates/gents-cli/src/commands/eval/init/dossier.rs`
- Modify: `crates/gents-cli/src/commands/eval/mod.rs` (`mod init;`)

**Interfaces:**
- Produces:

```rust
pub(crate) struct Dossier {
    pub(crate) pack_name: String,
    pub(crate) pack_version: String,
    pub(crate) pack_digest: String,
    pub(crate) behavior_id: String,
    pub(crate) slot: String,
    /// Collections a documents capture may name, with their fields: from
    /// datastore surfaces (`fields` of each entry) and `schemas/*.graphql`
    /// (field names parsed from `type X { … }`).
    pub(crate) collections: BTreeMap<String, BTreeSet<String>>,
    pub(crate) text: String,
}
pub(crate) const DOSSIER_LIMIT_BYTES: usize = 64 * 1024;
pub(crate) fn render(pack_dir: &Path, behavior: Option<&str>) -> Result<Dossier>
```

- [ ] **Step 1: Failing tests** (in `dossier.rs`), against `crates/gents/tests/fixtures/eval_runner/canary_pack` via `Path::new(env!("CARGO_MANIFEST_DIR")).join("../gents/tests/fixtures/eval_runner/canary_pack")`:

```rust
#[test]
fn the_canary_dossier_names_the_behavior_its_prompt_and_its_tools() {
    let dossier = render(&canary_dir(), None).unwrap();
    assert_eq!(dossier.behavior_id, "canary");
    assert_eq!(dossier.slot, "primary");
    assert!(dossier.text.contains("# Subject"));
    assert!(dossier.text.contains("## System prompt"));
    assert!(dossier.text.contains("files: ReadOnly"), "{}", dossier.text);
    assert!(!dossier.text.contains("# canary fixture"), "the README is excluded");
}

#[test]
fn a_pack_with_two_behaviors_needs_a_choice_and_refuses_an_unknown_one() { /* write a temp pack with two behaviors; render(dir, None) errors mentioning both ids; render(dir, Some("nope")) errors; render(dir, Some("b")) works */ }

#[test]
fn placeholders_stay_markers_and_the_size_limit_is_enforced() { /* a temp pack whose tools root is "${GENTS_EVAL_WORKSPACE_ROOT:-.}": text contains that literal; a temp pack with a 70 KB system prompt: render errors naming the prompt path */ }
```

- [ ] **Step 2:** run → FAIL.

- [ ] **Step 3: Implement.** Read the pack with the manifest and declared assets exactly as `resolve_subject_pack` reads a directory manifest plus `gents::pack::declared_paths`; call `gents::pack::load_pack_config(&manifest, &PackInstallOptions { agent_did: "did:key:dossier".into() }, &read_asset, &|_name| None)` so placeholders render as their literal text (the loader interpolates `${VAR:-default}` with the environment closure; with `None` for every name it keeps the default form; verify by the test and, if the loader substitutes the default, render the tools section from the raw `pack_config.json` bytes instead, which is what the test asserts). Choose the behavior: if `behavior` is `Some`, it must be in `config.agent_behaviors`; else exactly one behavior, else an error listing the ids. Its slot: the `manifest.metadata.inference_slots` entry whose `behaviors` contains the id; the context by `behavior.context_id`; its system prompt is already hydrated by the loader. Tools: the context's `tools_id` document, rendered from `serde_json::to_value(&tools)`: `host.files.mode`, `host.bash.mode` (or "none"), and for each `datastore.datastore_tool_surface_ids` the surface document's `entries`, each as `- tool <tool_name> on <collection>: <description>; fields <fields joined>`. Tasks: each `Task` with `behavior_id == chosen`, `prompt_template` verbatim under `### Task <task_id>`. Schemas: every asset path under `schemas/` ending `.graphql`, verbatim. Fixture files: asset paths that are neither config, prompt, schema nor README. Build `collections` from surface entries (`collection` → `fields` from the entry's `fields` list, using each element's `name` when it is an object, the string otherwise) and from schemas (a small parser: for each `type Name {` block, the identifiers before `:` on each line). Compose `text` with the headings `# Subject`, `## Identity`, `## System prompt`, `## Tools`, `## Tasks`, `## Schemas`, `## Fixture files`. Enforce the limit on `text.len()`.

- [ ] **Step 4:** run → PASS. **Commit:** `feat(eval-cli): render a subject pack into the author's dossier`.

---

### Task 6: draft parsing and validation

**Files:**
- Create: `crates/gents-cli/src/commands/eval/init/draft.rs`
- Create: `crates/gents-cli/src/commands/eval/init/validate.rs`
- Modify: `crates/gents-cli/Cargo.toml` only if `jsonschema` is not already a normal dependency (it is at line 108; confirm the section).

**Interfaces:**
- Produces:

```rust
// draft.rs
pub(crate) struct Draft { pub(crate) definition_id: Option<String>, pub(crate) title: Option<String>, pub(crate) cases: Vec<serde_json::Value> }
/// `Ok(None)` when the reply carries no fenced json block (an interview turn).
pub(crate) fn parse_reply(reply: &str) -> Result<Option<Draft>, String>

// validate.rs
pub(crate) struct Assembled { pub(crate) definition: gents::document_config::EvalDefinition }
pub(crate) struct Floors { pub(crate) validation_min: usize }
pub(crate) fn assemble(draft: &Draft, definition_id_flag: Option<&str>, owner: &str, slot: &str) -> Result<Assembled, Vec<String>>
pub(crate) fn validate(assembled: &Assembled, registry: &CheckRegistry, dossier: &Dossier, floors: &Floors) -> Result<(), Vec<String>>
```

- [ ] **Step 1: Failing tests.** `draft.rs`: a reply with prose only → `Ok(None)`; two json blocks → `Err` mentioning "exactly one"; a block that is not JSON → `Err` with the serde message; a block lacking `cases` → `Err`. `validate.rs`, each with a helper `good()` returning a one-case draft on the canary dossier (`captured_rows_count` over a capture named `items` on collection `CanaryItem`, which the canary pack's fixture schema provides only at trial time, so give the dossier test double a `collections` map with `CanaryItem → {item_id, label}`):
  - unknown check name → one message containing `unknown check "no_such"` and the case and stage ids;
  - params violating the schema (`{"name": "items"}` without `min`) → message containing `params` and `min`;
  - capture on a collection the dossier lacks → message containing the collection;
  - capture field not in the collection → message containing the field;
  - a file capture with an absolute glob → refused;
  - only validation cases → message naming the missing splits;
  - five validation cases with floor six → message with `6`;
  - duplicate case id → message from `EvalDefinition::validate` (step 3 runs before 4 to 6);
  - `good()` → `Ok(())`; `assemble` sets `comparability_version == 1`, `subject.inference_slots == [slot]`, and ignores any `subject` or `comparability_version` the draft carried.

- [ ] **Step 2:** run → FAIL.

- [ ] **Step 3: Implement.** `parse_reply`: scan for lines equal to "```json" and the next "```"; count blocks; parse the single one with `serde_json::from_str::<Value>`; require an object with `cases: array`; `definition` object optional with `definition_id` and `title` strings. `assemble`: build the `EvalDefinition` JSON by hand (`definition_id`, `agent_did: owner`, `comparability_version: 1`, `title`, `subject`, `cases`) and `serde_json::from_value` it, collecting the serde error as the one message. `validate`, in order, stopping at the first step that yields messages: (3) `definition.validate()` → one message; (4) for every stage check: `registry.get(name)` else `unknown check`, then `jsonschema::validator_for(&describe.params_schema)` and `iter_errors` into messages `case {c} stage {s} check {n} params: {error}`; (5) `EvalCapture::Documents` collection in `dossier.collections`, each field in its set; `EvalCapture::File` glob not starting with `/` and containing no `..`; (6) splits present per `EvalSplit`, validation count ≥ floor, ids unique (already by 3, keep the count check). Use `BTreeSet` for determinism.

- [ ] **Step 4:** run → PASS. **Commit:** `feat(eval-cli): parse and validate an author's draft against the contract, catalog and subject`.

---

### Task 7: writing the pack and the loader round trip

**Files:**
- Create: `crates/gents-cli/src/commands/eval/init/write.rs`

**Interfaces:**
- Produces:

```rust
pub(crate) struct Written { pub(crate) out: PathBuf, pub(crate) pack_name: String }
pub(crate) async fn write_pack(assembled: &Assembled, interview_summary: &str, out: &Path, force: bool) -> Result<Written>
pub(crate) fn readme(assembled: &Assembled, interview_summary: &str, pilot: Option<&str>) -> String
```

- [ ] **Step 1: Failing tests.** With a `good()` assembled definition of two cases: `write_pack` into a temp `out` produces exactly `manifest.json`, `pack_config.json`, `README.md`, `cases/<id>.json` for each case; `manifest.json` parses as `PackManifest` with `kind == Documents`, no inference slots, `assets` equal to the sorted file list; `pack_config.json` has `agent_principal: {}` and `eval_definitions[0].cases == ["./cases/a.json", "./cases/b.json"]`; loading the written directory through `gents::pack::load_pack_config` yields a definition whose cases equal the assembled ones (this exercises Task 2); a second `write_pack` to the same `out` without `force` errors mentioning `--force`; with `force` it succeeds. The round-trip install: `EmbeddedHome::create_temp("init-write")`, `ensure_agent_principal`, apply `DesiredStateApplyPlan::from_pack_config(&config)` (as `commands/eval/testing.rs::install` does), then query `EvalDefinition { definition_id }` and find the id.

- [ ] **Step 2:** run → FAIL.

- [ ] **Step 3: Implement.** Pack name: `definition_id` with `-` → `_` (must satisfy `gents::pack::is_valid_pack_name`; refuse otherwise with the rule). Write to `tempfile::tempdir()` first: `cases/<case_id>.json` (pretty JSON of each `EvalCase`), `pack_config.json` `{"agent_principal": {}, "eval_definitions": [{definition_id, comparability_version: 1, title, subject, cases: ["./cases/…"]}]}`, `README.md` from `readme`, then `manifest.json` listing every file except itself, sorted. Round trip: read the manifest and assets from the temp dir, `load_pack_config` with a throwaway DID, `DesiredStateApplyPlan::from_pack_config`, apply into `EmbeddedHome::create_temp("eval-init-roundtrip")` (mirror `testing.rs::install`), shut the node down. Then `std::fs::rename` (fallback: copy tree when rename crosses devices) into `out`; refuse when `out` exists unless `force`, in which case remove it first. `readme`: `# <title or id>`, one paragraph "Drafted by `gents eval init` on <date> for behavior <id> of pack <name> (<digest>).", the interview summary under `## What the author was told`, a table `| case | split | stages | checks |`, and `## Pilot` when `pilot` is `Some(run_id)`.

- [ ] **Step 4:** run → PASS. **Commit:** `feat(eval-cli): write a validated draft as a definition pack and prove it installs`.

---

### Task 8: the turn abstraction, the interview loop, the command

**Files:**
- Create: `crates/gents-cli/src/commands/eval/init/turn.rs`
- Modify: `crates/gents-cli/src/commands/eval/init/mod.rs`
- Create: `crates/gents-cli/src/commands/eval/init/contract.rs`
- Modify: `crates/gents-cli/src/cli/args.rs` (`EvalInitArgs`, `EvalCommand::Init`, `scope()`)
- Modify: `crates/gents-cli/src/commands/eval/mod.rs` (dispatch: `Init` resolves its own graphql before `EvalContext`, see below)
- Modify: `crates/gents-cli/src/commands/chat/mod.rs` (make `response_text_content` `pub(crate)`; make `streaming::{load_existing_tool_call_keys, stream_turn_progress}` `pub(crate)`)

**Interfaces:**
- Produces:

```rust
// turn.rs
#[async_trait::async_trait]
pub(crate) trait Turn {
    /// Send one user turn on the author's session; return the author's text.
    async fn send(&mut self, content: &str) -> Result<String>;
    fn session_id(&self) -> &str;
}
pub(crate) struct LiveTurn { graphql: String, agent_did: String, session_id: String, timeout_secs: u64, poll_secs: u64 }
pub(crate) struct ScriptedTurn { replies: VecDeque<String>, pub(crate) sent: Vec<String> }

// contract.rs
pub(crate) fn first_turn(dossier: &Dossier, catalog: &[CheckDescription], floors: &Floors) -> String
pub(crate) const VALIDATION_PREFIX: &str = "The draft did not validate; revise and reply with a new draft.";
pub(crate) const MAX_VALIDATION_ROUNDS: usize = 3;

// mod.rs
pub(crate) struct InitOutcome { pub(crate) written: Option<Written>, pub(crate) session_id: String, pub(crate) rounds: usize }
pub(crate) async fn interview(turn: &mut dyn Turn, lines: &mut dyn Iterator<Item = String>, ctx: &InitContext, out: &mut dyn Write) -> Result<InitOutcome>
```

`EvalInitArgs`:

```rust
#[derive(clap::Args)]
pub(crate) struct EvalInitArgs {
    /// A pack name, resolved as `gents pack install` resolves one, or a pack directory
    pub(crate) subject: String,
    #[arg(long)]
    pub(crate) behavior: Option<String>,
    #[arg(long)]
    pub(crate) out: PathBuf,
    #[arg(long)]
    pub(crate) definition_id: Option<String>,
    /// The inference profile the author runs on; the home's default when absent
    #[arg(long)]
    pub(crate) profile: Option<String>,
    #[arg(long)]
    pub(crate) pilot: bool,
    #[arg(long)]
    pub(crate) yes: bool,
    #[arg(long)]
    pub(crate) force: bool,
    #[arg(long, default_value_t = 6)]
    pub(crate) validation_min: usize,
    #[arg(long, default_value_t = 86_400)]
    pub(crate) timeout_secs: u64,
    #[arg(long, default_value_t = 1)]
    pub(crate) poll_secs: u64,
    #[arg(long)]
    pub(crate) registry: Option<String>,
    #[command(flatten)]
    pub(crate) scope: EvalScopeArgs,
}
```

- [ ] **Step 1: Failing tests** in `mod.rs` using `ScriptedTurn` and a `Vec<String>` of operator lines:
  - `an_interview_ends_when_the_author_drafts_and_the_pack_is_written`: replies `["What must it get right?", "<good draft block>"]`, lines `["It must count items."]`; outcome has `written`, `rounds == 1`; `turn.sent[0]` starts with `# Subject` and contains `# Check catalog` and the contract's `## How to reply with a draft` is not there (it lives in the system prompt), `turn.sent[1] == "It must count items."`.
  - `a_bad_draft_goes_back_with_the_messages_and_a_good_one_follows`: replies `[bad draft, good draft]`; `turn.sent[1]` starts with `VALIDATION_PREFIX` and contains `unknown check`; `rounds == 2`.
  - `three_bad_drafts_end_with_nothing_written`: `written.is_none()`, `rounds == 3`, and the returned error (or outcome flag) carries the last messages; `out` does not exist.
  - `the_operator_can_end_the_interview`: line `/quit` before any draft → `written.is_none()`, no error.

- [ ] **Step 2:** run → FAIL.

- [ ] **Step 3: Implement.** `LiveTurn::send`: `create_agent_request(&graphql, &agent_did, content, Some(&session_id), Some("eval-author"), RequestSubmitOptions::default())`, then `stream_turn_progress(&graphql, &submitted, load_existing_tool_call_keys(&graphql, &session_id).await?, timeout_secs, poll_secs)`, return `response_text_content(&response).to_owned()`. `ScriptedTurn::send` pops the next reply and records `content`.

`interview`: send `first_turn(...)`; print the author's reply to `out`; loop: read a line (`None` or `/quit`/`/exit` → return with `written: None`); send it; on each reply `parse_reply`: `Ok(None)` → print and continue; `Err(msg)` or a draft that fails `assemble`/`validate` → `rounds += 1`, if `rounds == MAX_VALIDATION_ROUNDS` print the messages and return an error `anyhow!("the author's draft did not validate after {MAX_VALIDATION_ROUNDS} rounds; last messages:\n{…}")`, else send `format!("{VALIDATION_PREFIX}\n\n- {}", messages.join("\n- "))` and continue with its reply (no operator line in between); `Ok(Some(draft))` valid → `write_pack(...)` with the interview summary being every operator line joined by newlines, print the case table and the next commands, return.

The command (`run` in `mod.rs`, called from dispatch): refuse when `!std::io::stdin().is_terminal()` with "gents eval init is an interview; run it in a terminal". Resolve graphql as `chat` does (`--graphql`, else `read_runtime_state(home).graphql`, else refuse "start `gents server` for this home and retry"); `resolve_agent_did`; `ensure_local_request_signer`. Resolve the subject with `resolve_subject_pack(&home_dir, &args.subject, args.registry.as_deref(), true)` so a directory is always available for the dossier; `dossier::render(dir, args.behavior.as_deref())`. Install `eval_author`: `gents::pack::resolve_pack("eval_author")`, `load_pack_config` with `PackInstallOptions { agent_did: owner }`, `bind_pack_install_config(&manifest, &config, &[("author", profile)])` where `profile` is `args.profile` or `default_inference_profile_id_for_behavior`/the home's default profile as `run.rs` derives it, then `DesiredStateApplyPlan::from_pack_config` applied through `ctx.access.transact("eval.init.install_author", …)`. Session id: `uuid::Uuid::new_v4()`. Build `LiveTurn`, `interview(...)` with `std::io::stdin().lock().lines()`. Print `session <id>` at the end. Exit codes: 0 written, 1 refused or rounds exhausted, 2 usage (clap).

Dispatch: add `EvalCommand::Init(args) => init::run(ctx, &args, deps, out).await` in `execute`; `EvalContext::resolve` already handles a served home (its access becomes `Graphql` when the runtime state answers). `deps.registry` supplies the catalog; `deps.executor` is used by the pilot (Task 9).

- [ ] **Step 4:** run → PASS; `cargo check -p gents-cli`. **Commit:** `feat(eval-cli): gents eval init interviews the author and writes the validated pack`.

---

### Task 9: the pilot

**Files:**
- Create: `crates/gents-cli/src/commands/eval/init/pilot.rs`
- Modify: `crates/gents-cli/src/commands/eval/init/mod.rs` (call it when `args.pilot`)

**Interfaces:**
- Produces:

```rust
pub(crate) struct PilotOutcome { pub(crate) run_id: String, pub(crate) revised: bool }
pub(crate) async fn pilot(ctx: &EvalContext, deps: &Deps<'_>, turn: &mut dyn Turn, written: &Written, assembled: &Assembled, subject: &SubjectPack, behavior_id: &str, profile: &str, confirm: &mut dyn FnMut(usize) -> bool, out: &mut dyn Write) -> Result<PilotOutcome>
pub(crate) fn digest_turn(report: &EvalReport, rows: &BTreeMap<(String, String), Vec<Value>>) -> String
```

- [ ] **Step 1: Failing tests** with `testing::Fixture` and a `ScriptedExecutor` whose default is `testing::pass()` and which fails `val-a`:
  - `a_pilot_runs_every_split_once_records_its_purpose_and_digests_the_failures`: after `pilot(...)` with `ScriptedTurn(["keep"])`, `load_runs` shows one run with `origin.purpose == "pilot"`, `trials_per_case == 1`, and case ids from all three splits; `turn.sent[0]` contains `val-a`, `captured_rows_count`, `below_min` and the sentence about a wrong case or wrong subject; `revised == false`.
  - `a_revised_draft_after_the_pilot_overwrites_the_pack`: `ScriptedTurn([good draft with a changed case id])` → `revised == true`, `--out/cases/` holds the new id, README mentions the pilot run id.
  - `the_pilot_asks_before_spending_unless_told_yes`: `confirm` returning `false` → no run created, error mentions "declined".
  - `a_pilot_run_is_not_exposure`: `load_report` of a later `eval` run of the same definition shows `exposure == 1`.

- [ ] **Step 2:** run → FAIL.

- [ ] **Step 3: Implement.** Case count = `assembled.definition.cases.len()`; `confirm(count)` (production: `interactive_backend::confirm(&format!("Pilot {count} cases, one trial each, on {profile}?"), true)` unless `--yes`). Install the written pack into the home: read its manifest and assets from `written.out`, `load_pack_config(…, PackInstallOptions { agent_did: ctx.owner })`, `DesiredStateApplyPlan::from_pack_config`, apply via `ctx.access.transact("eval.init.install_definition", …)`. Build a `RunRequest` as `run.rs::run_request` does, with `split` iterated: the runner takes one split per run, so run **three runs**, `<id>-pilot-<ms>-train|validation|held_out`, each `purpose: PILOT_PURPOSE.into()`, `trials_per_case: 1`, one cell `pilot` with `source: subject.source.clone()`, `behavior_id`, `inference_profile_id: profile`; skip a split with no cases. Follow each with `runner::run(&ctx.access, &request, deps.executor, deps.registry, deps.cancel.clone(), &deps.options)` and print `landed …` lines as `run.rs::follow` does (reuse `follow` by making it `pub(super)`). Load each report with `load_report`; for failing slots (`class != Pass`) collect the latest attempt's captured rows through `deps.executor.recollect(&locator, &[])` bounded to 2 KB per case (truncate with `…`). `digest_turn` renders `## Pilot results` then per case `### <case_id> (<split>)`, per stage a line per verdict `<check> <kind> <score_bp> <reason_code>`, then `captured rows:` and the JSON rows for failing stages, then the fixed instruction paragraph and "Reply with a full revised draft, or say 'keep'." Send it; `keep` (case-insensitive, trimmed) → done; otherwise `parse_reply` and the same validate path as the interview (one round: a failing revision is reported and the pack is kept as written, `revised == false`); a valid revision → `write_pack(…, force = true)` with `readme(…, Some(run_ids joined))`.

- [ ] **Step 4:** run → PASS. **Commit:** `feat(eval-cli): --pilot runs the draft once and lets the author revise from the evidence`.

---

### Task 10: after-help, the live test, and the gate

**Files:**
- Modify: `crates/gents-cli/src/cli/args.rs` (`EVAL_INIT_AFTER_HELP` with the exit codes and the served-home requirement; attach to `Init`)
- Modify: `crates/gents/tests/eval_runner_canary.rs` or a new `crates/gents-cli/tests/eval_init_live.rs`: `#[ignore = "needs a served home with GENTS_EVAL_INIT_HOME and a real backend"]` test that runs `gents eval init eval_canary`-style against `crates/gents/tests/fixtures/eval_runner/canary_pack` with `ScriptedTurn` replaced by `LiveTurn` when the env var is set; it asserts only that a pack was written and validated.
- Modify: `docs/superpowers/orchestration/2026-09-23-handoff.md` (a line under section 2 for PR 7) — done by the orchestrator, not the coordinator.

- [ ] **Step 1:** write the after-help and the ignored test.
- [ ] **Step 2: Gate.** `cargo fmt --all --check`; `cargo test -p gents-cli --lib eval` ; `cargo test -p gents`; `cargo check --workspace --all-targets`. Fix what fails. **Commit:** `docs(eval-cli): gents eval init after-help and the live init smoke`.
- [ ] **Step 3:** final whole-branch review per the SDD skill; then report: branch, commits, test summary, and the two rebase notes (Task 2's loader function; Task 3's `compare` signature change for the optimization driver).

## Self-review

- Spec coverage: §1 → Task 8 (command, loop, served-home refusal, exit codes); §2 → Task 5; §3 → Tasks 1 and 4; §4 → Tasks 6, 7 and 2; §5 → Tasks 3 and 9; §6 → each task's tests plus Task 10's live smoke.
- Placeholders: none; every step names files, types and messages.
- Type consistency: `Dossier.collections` (Task 5) is what `validate` (Task 6) reads; `Assembled` (Task 6) feeds `write_pack` (Task 7) and `pilot` (Task 9); `Turn` (Task 8) is what `pilot` (Task 9) takes; `CheckDescription.params_schema` (Task 1) is what step 4 of validation compiles.
- Known deviation from the spec text: the runner takes one split per run, so a pilot is three runs (one per populated split) rather than one; the README and the digest turn name all of them.
