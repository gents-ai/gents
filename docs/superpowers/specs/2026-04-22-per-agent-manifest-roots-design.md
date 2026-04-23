# Per-Agent Manifest Roots With Per-Document Folders and Prompt Sidecars

Changes the `defra-agent` CLI's on-disk manifest format so checked-in desired
state can be authored directly as a filesystem tree: one root per agent, one
subdirectory per document inside repeated collections, one canonical
`object.json` per document, and prompt bodies in adjacent Markdown sidecars.
Replaces the current aggregate-JSON format — no back-compat shim.

Closes sourcenetwork/defra-agent#67.

## Problem

Today a manifest root is a mix of top-level aggregate JSON arrays
(`agent-behaviors.json`, `tool-selections.json`, `inference-backends.json`,
`inference-profiles.json`) and flat `*.json` files in a subdirectory
(`tool-services/*.json`, `tasks/*.json`, `schedules/*.json`). `system_prompt`
and `prompt_template` must be inline JSON strings, which makes multi-paragraph
prompts unpleasant to author and review. `config export` prints a single JSON
bundle to stdout; there is no writer that produces a directly editable manifest
root.

That forces downstream projects (e.g., `infra/agents/native/ops-studio-1`) to
maintain custom renderers just to get from a human-readable source layout into
something `defra-agent` can apply. The goal is to make the CLI's native format
directly authorable so those renderers go away.

## Scope

In scope:

- On-disk layout replaced with per-document subdirectories inside each
  collection directory; single `object.json` per document.
- Prompt sidecar hydration on load for exactly two fields:
  `AgentBehavior.system_prompt` and `Task.prompt_template`.
- `config export` writes a manifest root directory; JSON-to-stdout output is
  removed.
- `Collection::file_name()` / `Collection::dir_name()` narrowed so each variant
  has exactly one of the two (file for `AgentPrincipal`, dir for the other
  seven).
- Loader strictness rules (Q7 option 2): directory-name/id mismatch is an
  error, duplicate ids are an error, unknown sibling files in a per-doc
  directory are ignored so authors can keep READMEs and notes next to their
  configs.
- `--force` flag on `config export` to allow overwriting a non-empty root.

Out of scope:

- A `config fmt` / canonicalizer for hand-authored roots.
- Additional sidecar-eligible fields beyond `system_prompt` and
  `prompt_template`.
- Additional sources or sinks (git, S3, HTTP) — the loader/writer stay
  filesystem-only.
- Lean changes. The reconcile proofs in `crates/defra-agent/proofs/` do not
  reference file/dir names; this is strictly a CLI import/export surface
  change.
- `config import` — this command reads the legacy JSON bundle format and is
  intentionally decoupled from `config export --root`. Use `config apply
  --root <dir>` to apply a manifest root produced by `config export`.

## On-Disk Layout

```text
<root>/
  agent-principal.json                     # required, exactly one
  agent-behaviors/
    <behavior_id>/
      object.json                          # required
      system_prompt.md                     # optional sidecar
  tool-selections/
    <selection_id>/
      object.json
  inference-backends/
    <backend_id>/
      object.json
  inference-profiles/
    <profile_id>/
      object.json
  tool-services/
    <service_id>/
      object.json
  tasks/
    <task_id>/
      object.json
      prompt.md                            # optional sidecar for prompt_template
  schedules/
    <schedule_id>/
      object.json
```

Rules:

- `agent-principal.json` is the only top-level file. Its authoring surface is
  narrow — `agent_did` identifies the principal and is typically set once by
  `init`; `display_name`, `default_behavior_id`, and `enabled` are the usual
  edit targets.
- Collection subdirectories are optional. A missing or empty collection
  directory means zero documents in that collection.
- Every per-document subdirectory must contain an `object.json`. Missing
  `object.json` is a hard error.
- Only `object.json`, `system_prompt.md`, and `prompt.md` are recognized
  filenames. Other files alongside them (`README.md`, screenshots, notes) are
  silently ignored. Dotfiles (`.DS_Store`, `.gitkeep`) are ignored.
- The subdirectory name must equal the value of the collection's unique-id
  field inside `object.json`. Mismatch is a hard error.
- Duplicate unique ids across sibling subdirectories is a hard error.
- The filesystem-unsafe-char constraint on unique ids is enforced by the
  writer, never by the reader. A root produced by `config export` is always
  loadable by `config validate`.

## `Collection` Enum Change

In `crates/defra-agent/src/collection.rs`, `file_name` and `dir_name` become
mutually exclusive:

```rust
impl Collection {
    /// Top-level file name, only for collections that don't use a directory form.
    pub fn file_name(self) -> Option<&'static str> {
        match self {
            Collection::AgentPrincipal => Some("agent-principal.json"),
            _ => None,
        }
    }

    /// Directory name for the per-doc subdirectory form.
    pub fn dir_name(self) -> Option<&'static str> {
        match self {
            Collection::AgentPrincipal => None,
            Collection::AgentBehavior => Some("agent-behaviors"),
            Collection::ToolSelection => Some("tool-selections"),
            Collection::InferenceBackend => Some("inference-backends"),
            Collection::InferenceProfile => Some("inference-profiles"),
            Collection::ToolServiceRegistry => Some("tool-services"),
            Collection::Task => Some("tasks"),
            Collection::Schedule => Some("schedules"),
        }
    }
}
```

A unit test enforces that exactly one of `file_name()` / `dir_name()` returns
`Some` for each variant, mirroring the existing parity tests in
`collection.rs`.

## Loader

Two changes in `crates/defra-agent-cli/src/desired_state/load.rs`.

### Per-doc directory scan

Replace the existing `load_required_json` / `load_optional_json` /
`load_optional_json_collection` / `load_json_collection` helpers with a single
per-doc collection reader:

```rust
fn load_per_doc_collection<T>(
    root: &Path,
    collection: Collection,
    errors: &mut Vec<String>,
) -> Vec<T>
where T: for<'de> Deserialize<'de> + HasUniqueId
```

Behavior:

1. Look at `<root>/<dir_name>/`. If missing or empty, return `[]`. If present
   but not a directory → error `"manifest collection path is not a directory:
   <path>"`.
2. Read entries. For each subdirectory entry (non-directory entries and
   dotfiles are skipped):
   - Require `<entry>/object.json`. Missing → error `"per-doc dir is missing
     object.json: <path>"`.
   - Parse `object.json` via serde. Parse errors surface as `"invalid
     <path>: <serde error>"`.
   - Check `parsed.unique_id() == entry_name`. Mismatch → error `"directory
     name '<handle>' does not match <unique_field> '<value>' in <path>"`.
3. After collecting all docs, scan for duplicate unique ids across sibling
   subdirectories → error `"duplicate <unique_field> '<value>' across
   <handle_a>/ and <handle_b>/"`.

A small internal trait `HasUniqueId` is implemented for each of the seven
`Desired*` collection types that have a dir form, returning the value of the
collection's unique field.

`DesiredAgentPrincipal` is loaded via a separate top-level path helper that
reads `<root>/agent-principal.json` as a required file.

### Prompt sidecar hydration

```rust
fn hydrate_sidecar(value: &mut Option<String>, json_dir: &Path) -> Result<(), String>
```

Rule: if `value.as_deref().map_or(false, |s| s.starts_with("./"))`, treat the
string as a path relative to `json_dir` (the directory containing the
referencing `object.json`). Join, read bytes, parse as UTF-8, replace `*value`
with the file contents. Strict failure modes:

- File does not resolve → error `"sidecar path does not resolve: <prompt_path>
  (referenced from <object_json_path>)"`.
- Not valid UTF-8 → error `"sidecar is not valid UTF-8: <prompt_path>"`.
- I/O error → error `"reading <prompt_path> failed: <os error>"`.

Values not starting with `./` are left untouched (absolute paths, relative
paths starting with `../`, and literal strings all pass through). `../` is
rejected implicitly because it would require the loader to reach outside the
per-document directory; the issue's cases all use adjacent sidecars.

Called on each loaded document:

- `DesiredAgentBehavior.system_prompt`
- `DesiredTask.prompt_template`

Hydration runs before `normalize_manifest` and `validate_manifest`, so all
downstream validation sees final inline strings and nothing else in the
pipeline changes.

### Error handling

Errors are accumulated into the existing `DesiredStateValidationReport.errors:
Vec<String>` rather than bailing on the first problem — matches the current
loader style and lets one run surface every issue in the root.

## Writer

New module `crates/defra-agent-cli/src/desired_state/write.rs`.

### Entry point

```rust
pub(crate) fn write_manifest_root(
    root: &Path,
    manifest: &DesiredStateManifest,
    force: bool,
) -> Result<(), String>
```

### Overwrite semantics

1. If `root` does not exist → create it.
2. If `root` exists, is non-empty, and `force == false` → error `"manifest
   root is non-empty; pass --force to overwrite: <path>"`.
3. If `root` exists and `force == true` → remove the directory tree, then
   recreate it. Guarantees no stale files survive after a rename in the DB.

### Handle selection

Per-document directory name = value of the collection's unique field on that
document. Before writing, validate the id against filesystem-unsafe values:
`/`, null byte, `.`, `..`, or empty string. Allows `:`, which is legal on
POSIX (Unix/Linux/macOS) filesystems and appears in DIDs and init-generated
IDs. Unsafe → error `"unique id '<value>' contains filesystem-unsafe
character(s); choose a filesystem-safe id"`. This guarantees that any root
produced by the writer is loadable by the loader.

Handles must not start with `.` — dot-prefixed entries are reserved for hidden
files (e.g., `.DS_Store`, `.gitkeep`) which the loader silently skips. A
dot-prefixed handle would produce a directory that the loader drops, causing a
silent round-trip loss. The writer rejects such ids with the error `"unique id
'<value>' starts with '.'; dot-prefixed handles are reserved for hidden files
and are silently skipped by the loader"`.

### Write order

1. `agent-principal.json` at the top level.
2. For each collection with a dir form, create `<root>/<dir_name>/` and write
   one `<handle>/object.json` per document.

No apply-order semantics on the filesystem — directory order doesn't matter
for correctness, only file contents.

### Sidecar spilling

Mirror of the loader's hydration:

- If `DesiredAgentBehavior.system_prompt` is `Some(body)` with any non-empty
  string, write the body to `<handle>/system_prompt.md` and replace the
  `system_prompt` field in the serialized `object.json` with the literal
  string `"./system_prompt.md"`.
- Same treatment for `DesiredTask.prompt_template` → `<handle>/prompt.md`.
- If the field is `None`, omit it from `object.json` entirely and do not write
  a sidecar.

The mutation happens on a clone of the serialized `serde_json::Value`
immediately before writing — the in-memory `DesiredStateManifest` is not
modified.

### JSON formatting

`object.json` is pretty-printed with two-space indent and a trailing newline.
Field ordering follows the serde struct definition, so diffs stay stable
across exports.

### CLI wiring

In `crates/defra-agent-cli/src/commands/config/export.rs`:

```rust
pub(super) async fn config_export(args: ConfigExportArgs) -> Result<()> {
    let agent_did = resolve_agent_did(args.home.as_deref(), args.agent_did.as_deref())?;
    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref(), false).await?;
    let bundle = build_config_export_bundle(&access, &agent_did).await?;
    let manifest = desired_state::manifest_from_export_bundle(&bundle)?;
    desired_state::write_manifest_root(&args.root, &manifest, args.force)
        .map_err(|e| anyhow::anyhow!(e))?;
    println!("wrote manifest root to {}", args.root.display());
    Ok(())
}
```

`ConfigExportArgs` gains `--root <path>` (required) and `--force` (bool). The
old `print_json(bundle)` behavior is removed.

## Command Surface Summary

- `config validate --root <dir>`: runs the full load (including sidecar
  hydration and all strictness checks) and prints the
  `DesiredStateValidationReport`. No new flags. Exit code unchanged.
- `config diff --root <dir>`: unchanged on the outside. Internally calls the
  new loader and compares hydrated inline prompts against live DB state.
- `config apply --root <dir>`: unchanged on the outside. Same loader. This is
  the intended way to apply a manifest root produced by `config export`.
- `config export --root <dir> [--force]`: writes a manifest root to `<dir>`.
  Replaces the JSON-to-stdout output. Without `--force`, refuses to overwrite
  a non-empty directory.
- `config import [PATH]`: reads a **legacy JSON bundle** file (not a manifest
  root directory). This command is intentionally decoupled from `config export
  --root`: exporting with `config export` and then applying with `config apply
  --root` is the new round-trip path. `config import` remains for teams that
  have existing JSON bundle workflows.

## Error Kinds

New loader errors:

- `per-doc dir is missing object.json: <path>`
- `directory name '<handle>' does not match <unique_field> '<value>' in <path>`
- `duplicate <unique_field> '<value>' across <handle_a>/ and <handle_b>/`
- `sidecar path does not resolve: <prompt_path> (referenced from <object_json_path>)`
- `sidecar is not valid UTF-8: <prompt_path>`

New writer errors:

- `manifest root is non-empty; pass --force to overwrite: <path>`
- `unique id '<value>' contains filesystem-unsafe character(s); choose a filesystem-safe id`
- `I/O error writing <path>: <os error>`

Existing errors carry over (`manifest root does not exist`, `manifest root is
not a directory`, generic serde-level `invalid <path>`, etc.).

## Testing

### Unit tests in `desired_state/tests.rs`

Loader:

- Happy path: full root with every collection populated; parsed manifest
  matches in-memory fixture.
- Sidecar hydration for `system_prompt` and `prompt_template`.
- Sidecar missing on disk → specific error message.
- Sidecar not valid UTF-8 → specific error message.
- Sidecar value not starting with `./` is left literal (absolute path,
  `../foo`, and plain string all pass through).
- Directory name vs unique-field mismatch → error.
- Duplicate unique id across sibling subdirs → error.
- Missing `object.json` inside a per-doc dir → error.
- Unknown sibling files (`README.md`, `notes.md`) alongside `object.json` do
  not error.
- Empty/missing collection dir returns zero docs.

Writer:

- Happy path: writes a root, asserts exact directory layout and that
  sidecars exist with expected content and that `object.json` contains
  `"./system_prompt.md"` / `"./prompt.md"` references.
- Unsafe unique id (contains `/`, is `..`, empty) → error.
- Refuses to overwrite non-empty dir without `--force`.
- With `--force`, removes stray files from a previous export (the rename
  scenario).
- `None` optional fields are omitted from `object.json` and no sidecar file
  is written.

### Round-trip test

The canonical acceptance criterion:

1. Build a `DesiredStateManifest` fixture with every collection populated and
   both sidecar fields non-empty.
2. `write_manifest_root(tmpdir, fixture, force=false)`.
3. `load_manifest_root(tmpdir)` returns the same manifest.
4. `assert_eq!(loaded, fixture)`.

### `Collection` enum test

In `crates/defra-agent/src/collection.rs`, add a test asserting that for every
variant, exactly one of `file_name()` / `dir_name()` returns `Some`.

### Integration test

Add `config_native_root` in `crates/defra-agent-cli/tests/`:

- Write a manifest root to a tempdir.
- Run `defra-agent config validate --root <tmp>`; assert JSON report
  `ok: true`.
- Tweak one file to introduce each error kind; assert the expected error
  string appears in the report.

## Acceptance Criteria (from the issue)

- `defra-agent config validate --root <dir>` accepts the directory shape in
  the On-Disk Layout section. **Covered by**: loader rewrite + integration
  test.
- `defra-agent config diff --root <dir>` and `apply --root <dir>` hydrate
  prompt file references transparently. **Covered by**: `hydrate_sidecar`
  runs in the load pipeline that both commands use.
- `config export --root <dir>` produces a round-trippable manifest root.
  **Covered by**: writer + round-trip test.
- Exporting and reapplying a root is lossless for document content.
  **Covered by**: round-trip test (load → write → load → eq) plus the
  explicit loader-writer invariant that writer output is always loadable.
