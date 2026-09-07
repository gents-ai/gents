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

## Independent review and demo removal

Reviewed with `claude --model fable` using read-only source access and a full
diff against main. The follow-up review found no remaining P1/P2 defects after:

- Deleting the interactive `gents demo` command, shell, setup/backend picker,
  desktop launcher, demo-only tests, and its last-caller-only chat wrapper.
  Live scenario helpers moved into `commands/pack`, without copies left behind.
- Correcting a missed subprocess invocation of removed `graph install` and
  testing its generated `pack install` arguments through the real CLI parser.
- Making `manifest.json` the sole dependency declaration and rejecting the
  old scenario dependency field and unsupported nested dependencies.
- Replacing private-lab defaults with loopback endpoints and deleting the
  unused `pack seed --home` option.
- Sharing build/runtime asset-path admission and the existing graph digest
  routine. Distribution bytes and graph execution inputs remain different
  asset sets, not competing lifecycle or installation owners.
- Removing distribution metadata flattening from the original strict graph
  manifest type and deleting the handwritten graph-field whitelist. The
  existing typed graph validator now validates graph-specific fields.

The initial review's claim that Serde ignored the flattened type's unknown-field
check did not reproduce in a focused test. The final shape avoids that question
entirely: the graph type is strict and flatten-free, with a regression test at
the distribution-to-graph boundary. Review is not a substitute for the full
runtime, CLI and workspace gates.

## Live code-review qualification

The review of commit `c9429f8` completed through recon, four parallel scanners,
verification and triage in run `3c434e7d-ed21-4d3f-bf60-54fdf61ea527`.
GLM-5.3-Flash-NVFP4 used temperature 1, top_p 0.95, high reasoning,
524288 context tokens and 65536 maximum output tokens. Captured provider
requests, not just profile documents, confirmed these settings. The run used
117 model calls, about 12.2M input tokens (including repeated context) and
114.4k output tokens. Five tool failures were recovered; all stages completed.
Eleven candidates yielded ten confirmed rows (seven distinct issues after
deduplication) and one refutation of an intentional breaking change.

The resulting fixes stage cache assets beside their destination and publish
with `persist_noclobber`, protecting both concurrent readers and operator edits.
Tests cover concurrent installation and abandoned partial staging files. Old
partial destination files remain fail-closed: they cannot safely be distinguished
from operator edits and are not silently repaired. The JavaScript documentation
checker no longer substitutes for Rust asset admission or environment
interpolation. Fixtures use the production document-handle function, and stale
display paths, removed flags and private endpoint examples are corrected.

An earlier run failed after a scanner exhausted 65536 output tokens on each of
four identical temperature-zero calls. Its reasoning-only responses were treated
as empty output and retried with the transport policy; provider finish metadata
was unavailable. A subsequent live profile update persisted but did not reach
the provider until the isolated node restarted. These runtime diagnostic and
reconciliation observations are separate from the pack fixes; this change does
not claim to repair them. The successful run is evidence for the graph flow,
not a guarantee that every inference run will complete.
