# Bundled graph productization

Status: accepted design for the first `code_review` vertical slice.

## Outcome

A release binary can discover, install, run, and observe the bundled code-review
graph without a Gents source checkout:

```text
gents init
gents server
gents pack show code_review
gents pack install code_review
gents graph run code_review --watch
gents graph result <run-id>
```

`graph run code_review` defaults to the current directory, `origin/main`, and
`HEAD`. The server is required because it owns reconciliation, request recovery,
and durable graph completion.

This is one product path over existing Gents abstractions. It is not a package
registry, dependency solver, or second workflow engine.

## Evidence and ownership

The graph compiler and runtime already provide typed `GraphIntent`, immutable
plans, ordinary `Task`/`EventTrigger` lowering, revision digests, and bounded
delivery. Desired-state apply already owns ordered configuration writes. The
runtime document view already reconciles behaviors and ordinary tool surfaces.
Agent requests, sessions, inference calls, tool calls, and run timelines are
durable documents.

The missing product seams are:

- immutable package assets compiled into the release binary;
- idempotent materialization of those assets into one initialized home;
- durable revision activation and run pinning;
- terminal result, failure, cancellation, and progress projections;
- a CLI that binds the local repository and the initialized default behavior.

The implementation therefore extends those owners:

| Concern | Owner |
| --- | --- |
| graph semantics and bounds | `GraphIntent` / graph compiler |
| config ordering and writes | desired-state apply |
| behavior tools | `ToolSelection` and `DatastoreToolSurface` reconciliation |
| identity and model placement | principal, behavior, and deployment documents |
| execution | `GraphRevision`, `GraphRun`, triggers, and `AgentRequest` |
| repository access | isolated workspace placement/binding |
| observation | graph-run projection plus ordinary timeline rows |

## Minimal package boundary

`GraphPackageManifest` v1 contains only data used by the first product path:

```json
{
  "manifest_version": 1,
  "name": "code_review",
  "version": "1.0.1",
  "description": "Bounded four-stage code review.",
  "compiler_version": "graph-intent-v3",
  "roles": [
    {"name": "coordinator", "description": "Recon, verification, triage."},
    {"name": "reviewer", "description": "Parallel review lenses."}
  ],
  "schemas": ["schemas/review_job.graphql"],
  "intent": "graph.intent.json",
  "capabilities": "capabilities.json"
}
```

The package digest covers the manifest and every referenced asset. The compiled
plan persists package name, version, digest, binary provenance, logical role
bindings, workspace ceilings, required schema contract digests, and exact
package-owned artifact identities.

The manifest deliberately has no dependency language, upgrade-policy language,
install-variable system, requested-effects list, network policy duplicate, or
second catalog digest. `GraphIntent` remains the owner of entries, results, and
resource bounds. Node invocation bounds remain on graph nodes. New vocabulary
is added only when a second real package proves it necessary.

## Catalog, install, and configuration

### Catalog

The catalog is compiled into the binary and read-only. Listing it performs no
schema or document writes and grants no authority.

### Install

`gents pack install code_review`:

1. resolves the initialized owner principal and its default behavior;
2. binds each package role to that operator-approved principal, deployment,
   backend, inference profile, and model;
3. verifies existing schema contracts or adds missing bundled SDL through the
   server's schema endpoint;
4. lowers package assets through normal desired-state writers;
5. compiles the bound `GraphIntent`;
6. transactionally writes package-owned config, the immutable revision, and
   its ordinary triggers;
7. activates the revision with a compare-and-swap on the graph definition.

The default binding creates no new principal, backend, profile, deployment, or
tool grant. An explicit bindings file is the escape hatch for advanced role
placement.

Repeated install with the same package and bindings reproduces the same
revision digest and is a no-op. Changed package content or role bindings produce
a successor revision. No backward-compatible manifest migration is required
before this feature has users; a clean install is the development contract.

Schemas are additive and outside document transactions. A failed install may
therefore leave compatible schemas present, but never discoverable package
artifacts or an active revision. Retry re-verifies those schemas and resumes
idempotently.

Package installation never invokes global prune. Stable physical IDs include a
configuration digest, so package documents cannot collide with unrelated home
configuration.

### Configuration

Vendor assets and published revisions are immutable. A configuration change
creates and activates a successor revision; it does not edit a prior plan or
encode policy only in a prompt.

Disabling a graph sets its definition to reject new runs. Existing runs remain
pinned to their revision. Garbage collection of retired, unpinned revisions is
deferred until a concrete retention policy is chosen.

## Execution contract

Starting a run requires separate authority from publishing or installing. The
start transaction verifies:

- the graph is enabled and owned by the caller;
- the active revision matches the caller's expected digest;
- revision status is active and all artifacts and schemas are ready;
- the named entry exists;
- the caller and package roles remain valid.

It then atomically creates a running `GraphRun` pinned to that digest and the
entry seed document. The seed carries the run correlation and workspace
lineage. Ordinary event triggers materialize ordinary `AgentRequest` documents;
the existing request lifecycle remains the executor.

The code-review entry is `review`. Its CLI adapter resolves and pins repository
path, base commit, and head commit, provisions a read-only isolated workspace,
and seeds the review job. Package prompts use the normal task template renderer
to project event source fields into each stage; no bespoke handoff channel is
introduced.

The declared terminal outputs are intentionally small:

- `findings`: at most 128 `CodeReviewFinding` documents;
- `report`: exactly one `CodeReviewTriageReport` document.

Collection indexes and graph fan-in enforce stage ledgers. There is no generic
result-predicate language in v1.

## Run lifecycle

```text
running --results satisfied and work quiescent--> succeeded
running --failure proven and work quiescent-----> failed
running --cancel latched and work quiescent------> cancelled
```

Failure and cancellation first interrupt active correlated requests. A terminal
write is legal only after all correlated work is terminal. Completion reloads
the full durable view in one transaction and compare-and-swaps the GraphRun
generation. Exactly one terminal transition wins.

Success persists exact result document IDs and commit CIDs. Failure persists
structured evidence. Cancellation intent is durable and suppresses later
correlated materialization. Restarted reconcilers derive the same decision from
documents and repeat interruption or terminalization safely.

These transition guards are modeled first in
`crates/gents/proofs/Proofs/GraphPipeline.lean`, emitted as conformance cases,
and then implemented in Rust. The proof and conformance suite must contain no
`sorry` and must fence success, failure, cancellation, pinning, activation, and
single-winner terminal behavior.

## Observation

`graph watch` and `graph result` reconstruct durable state; they do not maintain
a CLI-only execution model.

The run view contains status, pinned digest, entry, stages, grouped delivery,
correlated requests, terminal results, failure evidence, and cancellation. The
watch command joins the request/session IDs to the existing prompt-free timeline
projection to show model calls, provider-reported or estimated token usage,
tool calls, and agent sessions.

`graph result <id>` prints the complete report and every confirmed finding,
including severity, path, line, title, detail, evidence, and verification. Its
text output is designed to be piped directly to the clipboard and handed to
another agent. JSON remains available for automation.

Desktop work must consume the same projection through the bridge. A desktop
catalog/run inspector and typed authoring editor are follow-ups, not part of the
first binary acceptance slice.

## Authority invariants

- Bundling and cataloging grant nothing.
- Installing and publishing are distinct from starting a run.
- Logical roles bind only to explicit principal/deployment/model documents.
- Package tools lower through normal behavior tool-surface reconciliation.
- Workspace authority is the meet of the package ceiling and the provisioned
  workspace binding.
- Model-authored content cannot create principals, deployments, backends, tool
  grants, or higher authority.
- Request-intent and execution-authority hardening remain shipping dependencies;
  there is no parallel request path.
- Persisted documents retain package, configuration, revision, principal, and
  run attribution.

## Acceptance test

Run the following against a release-style binary and a clean temporary home:

1. `gents init`; choose ChatGPT/Codex OAuth and complete login.
2. Start `gents server` with schema operations enabled by the embedded server.
3. Confirm `pack show code_review` is read-only.
4. Install `code_review` with no bindings flags; confirm it inherits the
   initialized default behavior and creates one active revision.
5. Repeat install; confirm the digest and document counts are unchanged.
6. From an arbitrary Git worktree, run `gents graph run code_review --watch`.
7. Confirm recon, four scans, verification, and triage become visible with
   model/tool/session activity and non-misleading token usage.
8. Confirm the run terminals only after every correlated request is terminal.
9. Pipe `gents graph result <id>` to a file or clipboard and confirm the full
   actionable review is present.
10. Restart the server during or after a run; watch/result must reconstruct the
    same pinned state and outputs.
11. Change an explicit role/model binding; install must create a successor
    revision without mutating the predecessor.
12. Disable the graph; new starts fail while a pinned run remains observable.
13. Exercise denied publisher, starter, cross-owner binding, remote local-path,
    incompatible schema, and insufficient workspace-authority cases.
14. Force one required request to fail while a sibling is active; siblings must
    be interrupted and the GraphRun must not become failed until they quiesce.

During pre-release development, the full flow may wipe the home between binary
formats. Backward compatibility is not a release requirement yet.

## Ordered implementation stack

Only one implementation issue is active at a time:

1. DefraDB embedded schema HTTP prerequisite.
2. Reusable desired-state apply extraction.
3. Bounded `GraphIntent` entry, result, delivery, and resource contracts.
4. Lean lifecycle model, generated conformance cases, and executable guards.
5. Immutable revision publication, pinned runs, completion, and replay.
6. Bundled package catalog, installation, artifact visibility, and code-review assets.
7. Grok usage normalization.
8. CLI install/run/watch/result vertical slice.
9. Documentation and removal of the obsolete checkout-relative demo.
10. Desktop catalog/run inspector.
11. Typed authoring and publish confirmation.

Proof design for the next lifecycle change may overlap implementation work, but
Rust transition changes wait for the corresponding Lean and generated
conformance cases. Desktop projection work may overlap after the run-view JSON
contract is stable. Larger packages wait until the code-review acceptance suite
passes from a clean binary.

Primary files by layer:

- proof: `crates/gents/proofs/Proofs/GraphPipeline.lean` and conformance files;
- durable schemas: `crates/gents-schemas/schemas/agent/graph_*.graphql`;
- runtime: `crates/gents/src/graph_pipeline/`;
- package lowering: `crates/gents/src/graph_package/` and desired-state apply;
- bundle: `packs/code_review/`;
- CLI: `crates/gents-cli/src/commands/graph.rs` and graph CLI tests;
- observation: `run_timeline_fetch.rs` and the existing adapter/bridge seam.

Every implementation PR runs `cargo test -p gents` and
`cargo check --workspace --all-targets`. Every interpolated GraphQL value uses
`escape_graphql_string`, and JSON-to-GraphQL writers convert empty arrays to
`null` rather than emitting `[]`.

## Deferred operator choices

Only two choices remain genuinely policy-dependent:

- retention period and administrative grant for collecting retired, unpinned
  package revisions;
- whether a future generic graph runner accepts a typed input document, repeated
  `--var` bindings, or package-specific CLI adapters.

Neither blocks the code-review release path.
