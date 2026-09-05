# Unified packs migration

## Contract

A pack distributes Gents documents and supporting assets. A compiled graph is
optional. The canonical source is `packs/<snake_case_name>/`; package identity
and database document identity are separate from filesystem spelling.

## Execution checklist

- Consolidate demos and bundled graphs without duplicate asset sources.
- Add a versioned manifest and documentation to every worked example.
- Embed declared assets only, never local run databases, logs or credentials.
- Provide a shared catalog/resolution boundary and `gents pack list`, `show`,
  and `install`; reuse graph installation and desired-state application.
- Keep graph execution; remove old packaging CLI commands. Preserve document IDs.
- Use snake_case desired-state directories, with no legacy layout fallback.
- Update checked-in paths, tests, build embedding and documentation together.
- Require graph diagrams and preserve the Grok run history as a case study.
- Verify catalog/manifest/asset contracts, all installer paths, rejection of old CLI names,
  the full runtime and CLI suites, and the workspace all-target build.

## Boundaries

No registry service or network package fetching in this change. Bundled names
are the first source; source resolution is separate from installation. A pack
install never seeds inference, grants additional host authority, or prunes
unrelated configuration. Explicit desired-state identity rebinding remains
subject to the existing installer checks. Runtime execution rules are unchanged.

## Naming

Pack-owned directories use snake_case; conventional README.md filenames remain.
Public package names use snake_case, without aliases. Collection and
document IDs remain unchanged. External source filenames and historical quoted
evidence are not silently reinterpreted as database identities.

The loader/exporter share a document-handle mapping (hyphens to underscores).
Colliding IDs are rejected before export can clear an existing destination.
Old collection directory spellings fail validation rather than being ignored.

## Fixture dependency

The two strict-schema fixture corrections from PR #1380 are included unchanged
in this worktree: obsolete backend fields must be rejected, and MCP fixtures
must supply the required path. These were baseline failures, not a reason to
relax the manifest schema. This does not merge or close that PR.

The full CLI gate also exposed two stale baseline fixtures: the Grok apply-root
server test omitted `--inference-url`, and the negative interop envelope omitted
the required `source_version_status`. Both fixtures are corrected; their runtime
contracts remain unchanged.

An intermittent init/restart readiness timeout found by the full CLI gate is
tracked separately in #1402. The fixture now retains server output, identifies
the boot phase, and includes its last authoritative readiness observation.
These are diagnostic improvements, not a claim that the underlying timeout is
fixed; a passing repetition must not close that issue without a causal fix.
