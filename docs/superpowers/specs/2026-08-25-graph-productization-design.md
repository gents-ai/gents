# Graph productization: bundled packages, execution contracts, and authoring UX

**Status:** accepted implementation contract for the first productization work after
`feat/graph-model-tools` (`2d39ade3`). This work does not modify, rebase, or absorb
the four open graph-pipeline commits.

**First acceptance package:** `code-review`, stored only in the canonical bundled
asset tree. The pre-release checkout-relative demo pack is removed rather than kept
behind a compatibility reader or alias.

## Decision

Use a bundled `GraphPackage` manifest as a read-only distribution descriptor, not as
a second control plane. Installing a package lowers its assets into the existing
desired-state resources and an immutable `GraphRevision`. The existing
`GraphDefinition.active_revision_digest` remains the only activation pointer, and the
existing graph revision is also the durable package, provenance, configuration, and
owned-artifact receipt.

No package installation, release, configuration-revision, artifact-ledger, placement,
or install-attempt collections are introduced for the first vertical slice. In
particular, configuration is not a mutable vendor object: changing a typed binding
compiles and materializes a successor `GraphRevision`.

`GraphRun` continues to pin a revision. A small runtime-owned correlation join derives
progress and named results from the existing request, trigger, output, and timeline
documents and commits a terminal result to the run. CLI and desktop use that same
projection. Nothing in the package catalog, CLI, or UI schedules graph stages.

The smallest initial command surface is:

```text
gents graph catalog [<package>]
gents graph install code-review [--version <version>] [--bindings <file>]
gents graph publish code-review --revision <digest> --confirm-revision <digest>
gents graph run code-review --repo /path/to/repo --base origin/main --head HEAD
gents graph watch <run-id>
gents graph result <run-id>
gents graph cancel <run-id>
gents graph disable|enable code-review
```

`install` never activates. It returns an immutable revision receipt and the exact
`publish` command. Publication performs a distinct authorization check and requires
the revision digest to be repeated through `--confirm-revision`. Starting/replaying,
cancelling, and observing use their own checks. Bundling or installing never grants
any of them. The default quickstart binding reuses the initialized home's existing
principal, deployment, behavior backend/profile/model and prints the full mapping;
`--bindings` accepts the same typed structure for explicit successor configuration.
The local-repository v1 slice binds every logical role to that one owner principal
and host deployment while allowing independent backend/profile/model choices per
role. Cross-principal or cross-deployment role placement is a later distributed
execution milestone and fails closed here.

Installing the same package with different bindings or a newer bundled version is the
single successor-revision operation; v1 does not expose separate `configure` and
`upgrade` commands. Exact GC rules exist from the start, but a destructive public GC
command follows the proven vertical slice.

`run` does not implicitly install. `disable` blocks new runs and replays; it does not
hide active artifacts or stop observation, so already-pinned runs can finish.
“Preinstalled” means this bundled, immediately discoverable catalog entry—not eager
document writes in every home.

## Evidence from the current stack

The design uses the tip of the open stack as evidence rather than assuming future
abstractions.

| Evidence | Current behavior | Smallest required extension |
| --- | --- | --- |
| `ef064a29` and `Proofs/GraphPipeline.lean` | Model one-shot publication over an operator-approved task-backed plan. | Extend the same model with immutable revisions, one active digest, readiness, run pinning, terminal results, and cancellation. Atomic create-and-seed is the implementation refinement of start; do not add an externally visible pending phase. |
| `b58b58c7` and `graph_pipeline/{types,compiler}.rs` | Pure `GraphIntent` to canonical `GraphPlan` compilation with stable diagnostics. Each in-memory `StageCapability` delegates execution to one existing `task_id`. | Keep Task as the sole execution/configuration owner. Make bundled/installed task-backed capabilities inputs to the same compiler and add only the code-review plan semantics missing from the foundation. |
| `107fa06b` and `graph_pipeline/runtime.rs` | Publishes deterministic EventTriggers over existing enabled Tasks; it does not synthesize Tasks, behaviors, prompts, or tool surfaces. | Preserve that boundary while adding revision readiness/activation and run pinning. Package install materializes ordinary desired-state Tasks before graph materialization creates only revision-owned triggers. |
| `c9ccf9ba` and `graph_pipeline/tools.rs` | `GraphPipelineModelTools` is a factory around caller-visible in-memory capabilities. Repository callers are tests; it is not a production-injected tool path today. | When model authoring ships, expose these operations through ordinary behavior `ToolSelection` reconciliation. There is no production custom-tool dependency to migrate first. |
| `demo/code-review` | Demonstrates recon, scanner fan-out, verifier dynamic fan-in, triage, durable ledgers, and result export, but resolves checkout-relative files and writes filesystem-only metadata/results. | Bundle those assets, accept any eligible local Git repository, lower the pack to one revision, and replace demo-only observation with `GraphRun` projection. |
| Event trigger runtime | `expected_count_field` is already supported for `per_group`. | Thread a `SourceField` count through `DeliveryMode`, compiler, and graph materializer instead of inventing join behavior. The current pack's model-produced `expected_total` must be immutable, canonical, consistent, and bounded after first write; it cannot be assumed controller-authored. |
| Desired-state apply/diff/prune | Applies schemas first and configuration collections in dependency order. Prune intentionally removes live-only resources for the whole manifest/principal. | Reuse its normalization, validation, ordering, transaction, and diff code with prune always disabled for packages. Package Tasks remain ordinary desired-state resources; exact GC considers only package-owned IDs from an inactive revision plan. |
| Behavior tool surface | Behavior to selection to datastore/MCP/host tools is resolved at reconcile time and fails closed for missing, disabled, cross-owner, malformed, or over-ceiling resources. | Materialize ordinary behaviors, selections, and surfaces, then let the existing reconciler remain authoritative. Pin their immutable IDs and canonical document digests in the revision plan. |
| `run_timeline_fetch` and adapter projections | Reconstruct an `AgentRequest` timeline and export ATIF, OpenAI/Codex, LangGraph, and multi-agent views from durable rows. | Join correlated request IDs once for a graph run and compose the existing per-request timeline/projections. Do not invent a graph-specific transcript. |
| Principal/deployment/workspace documents | Principal is the identity/ACP boundary; behaviors belong to principals; deployments place them; isolated workspaces and repository placement already exist. | Bind manifest roles only to explicit eligible principals/deployments and pass the existing workspace binding into the seed. Do not store host paths in shared package/revision documents. |
| Desktop bridge | Versioned commands, permissions, generated TypeScript, fingerprints, and request timeline views already exist. | Add thin graph DTOs/commands over runtime APIs after the CLI path; no UI-owned execution state. |

Additional findings that constrain the product:

- The existing demo's Gents-specific repository markers and checkout-relative pack
  resolver must be removed from the product path. Its mandatory Cargo/Clippy baseline
  must become repository-aware: Rust checks are useful when present, but an arbitrary
  eligible local repository cannot be rejected merely for lacking Cargo metadata.
- The demo requests broad bash/network access while prompts ask agents not to edit.
  Prompt policy is not a boundary. The acceptance profile uses the existing isolated
  workspace and an operator ceiling; unsupported enforcement fails closed.
- Generic schema names such as `Finding` collide in a shared home. Package schema
  types and physical collections need stable package namespaces.
- The repository has no established `graph_publish`, `graph_start`, or package-grant
  document vocabulary. This design therefore does not manufacture grant types or
  action names. The shipping authority hardening must fence the real start, trigger,
  and direct request materialization paths using the repository's ACP mechanism.

## Reuse map and ownership

```text
immutable bytes in the binary
  GraphPackage manifest + assets
              │ pure validate and lower
              ▼
existing desired-state resources ──► existing GraphRevision.plan_json
              │                            │
              └── same revision fence ─────┘
                                           │ one existing active pointer
                                           ▼
                                     GraphDefinition
                                           │ existing task/trigger/request runtime
                                           ▼
                                        GraphRun
                                           │ correlation join
                                           ▼
                                  GraphRunView + RunTimeline
                                     CLI / bridge / adapters
```

There are six product boundaries but not six persistence models:

1. **Catalog:** immutable binary assets, inspectable without a home or writes.
2. **Installation:** schema-first desired-state apply plus existing graph
   materialization/activation. An incomplete `GraphRevision` is its durable retry
   receipt.
3. **Configuration:** typed bindings compiled into the immutable plan of a successor
   revision. A package asset and active revision are never edited in place.
4. **Execution:** existing graph artifacts, triggers, requests, owned completion loop,
   and workspaces execute the pinned revision. `GraphRun` records completion.
5. **Observation:** one `load_graph_run_view` correlation join composes existing
   timelines and is shared by CLI and bridge.
6. **Authoring:** human and model tools use the same `GraphIntent`, `StageCapability`,
   compiler, stable diagnostics, and exact-digest publish boundary.

## `GraphPackage` v1

The package is a canonical manifest plus local assets compiled into the release
binary. It is a distribution boundary that lowers to `GraphPlan`; it is not a runtime
execution language.

JSON is canonical for v1. A source YAML convenience may be normalized at build time.
The package digest is SHA-256 over a canonical archive of the manifest with its digest
omitted plus each referenced path and byte sequence in lexical order. Paths cannot
escape the package root, be symlinks, or fetch network content.

Illustrative shape:

```json
{
  "api_version": "gents.dev/graph-package/v1",
  "kind": "GraphPackage",
  "metadata": {
    "name": "code-review",
    "version": "1.0.0",
    "digest": "sha256:...",
    "display_name": "Code review",
    "description": "Bounded review of a local Git diff"
  },
  "compatibility": {
    "gents": ">=0.9.0 <1.0.0",
    "graph_compiler": ["graph-intent-v1"],
    "required_host_features": ["git", "isolated_workspace"],
    "optional_host_features": ["workspace_write_sandbox", "lsp"]
  },
  "dependencies": [],
  "upgrade": {
    "policy": "manual",
    "accepted_predecessors": [">=1.0.0 <2.0.0"],
    "configuration_migration": "compatible_bindings_only"
  },
  "schemas": {
    "namespace": "PkgCodeReviewV1",
    "sdl": [{"path": "schemas/v1.graphql", "digest": "sha256:..."}],
    "patches": []
  },
  "variables": [
    {"name": "coordinator_backend", "phase": "configuration", "type": "backend_ref", "required": true},
    {"name": "reviewer_backend", "phase": "configuration", "type": "backend_ref", "required": true},
    {"name": "lens_min", "phase": "configuration", "type": "integer", "default": 4, "minimum": 1, "maximum": 32},
    {"name": "lens_max", "phase": "configuration", "type": "integer", "default": 12, "minimum": 1, "maximum": 32}
  ],
  "roles": [
    {"role_id": "coordinator", "principal": {"required": true}, "deployment": {"required": true}},
    {"role_id": "reviewer", "principal": {"required": true}, "deployment": {"required": true}}
  ],
  "resources": {
    "behaviors": [{"asset": "behaviors/recon.json", "role": "coordinator"}],
    "tool_selections": [{"asset": "tool-selections/recon.json"}],
    "tool_surfaces": [{"asset": "surfaces/recon.json"}],
    "task_templates": [{"asset": "tasks/recon.json", "prompt": "tasks/recon.md"}],
    "stage_capabilities": [{"asset": "capabilities/recon.json"}]
  },
  "graph": {"intent": "graph.intent.json"},
  "entries": [{
    "name": "review",
    "input_schema": "contracts/review-input.schema.json",
    "cli": {
      "repo": {"type": "local_git_repository", "required": true},
      "base": {"type": "git_ref", "required": true},
      "head": {"type": "git_ref", "required": true},
      "focus": {"type": "string", "required": false}
    }
  }],
  "results": [
    {"name": "report", "port": "triage.report", "cardinality": {"exactly": 1}, "terminal": true},
    {"name": "findings", "port": "verify.findings", "cardinality": {"at_most": 1024}, "terminal": false}
  ],
  "authority": {
    "requested_effects": ["schema_change", "isolated_workspace_write"]
  },
  "ceilings": {
    "graph": {"nodes": 8, "depth": 8, "fan_out": 32, "total_invocations": 128, "max_runtime_secs": 7200},
    "stages": {
      "recon": {"workspace": "read_write_isolated", "network": "disabled", "max_invocations": 1},
      "scan": {"workspace": "read_write_isolated", "network": "disabled", "max_invocations": 32},
      "verify": {"workspace": "read_write_isolated", "network": "disabled", "max_invocations": 1},
      "triage": {"workspace": "none", "network": "disabled", "max_invocations": 1}
    }
  }
}
```

`requested_effects` is typed catalog/confirmation data used to reject effects above an
operator ceiling. It does not name grants and package code never maps it to an ACP
action. Install, publish, start, cancel, and observe authorization belongs to the
corresponding hardened runtime endpoint regardless of manifest contents.

Manifest rules:

- V1 dependencies may refer only to another bundled package and compatible version.
  Missing dependencies and cycles fail before writes. Remote packages, registries,
  signatures, and general dependency solving are later work.
- Variables are typed, bounded, phase-labelled, and classified for sensitivity.
  Secrets are references to existing credential/backend documents, never values in a
  branchable plan.
- Roles are logical. Installation binds them to existing operator-approved principal
  and deployment documents. The package cannot create identity or widen grants.
- Schema types and physical collection names are package-namespaced. Additive patches
  use existing schema provisioning; incompatible shape changes require a new package
  namespace/major version.
- Stage authority is the intersection of package request, principal grant, ordinary
  behavior `ToolSelection`, operator ceiling, host capability, and request workspace
  authority. A package value can only narrow that intersection.
- Package provenance is `bundled` with binary version, build commit, package digest,
  and catalog digest. The typed package projection described below—not an unbounded
  manifest blob—is included in `GraphPlan`.

The graph/package formats are pre-release. Change `GraphIntent`, `GraphPlan`, compiler
fixtures, and bundled assets directly to reach the smallest coherent contract; do not
add aliases, dual readers, migration shims, or compatibility-only defaults for the
experimental formats. Select the final compiler version before release. This does not
remove package-to-package upgrade policy: it removes compatibility obligations to the
checkout-era and open-stack serialization shapes.

### Typed plan boundary

`GraphRevision.plan_json` is exactly one serialized, schema-checked `GraphPlan`, never
a free-form receipt bag. Keep nodes, edges, entries, capability manifest, and limits in
their existing owners; extend entry records with their input contract, add top-level
typed result contracts, and add one typed package block:

```text
PackagePlan {
  name, version, package_digest, catalog_digest, bundled_provenance,
  bindings: typed non-secret values,
  roles: role -> { principal_did, deployment_id, backend/profile refs },
  effective_authority_ceiling,
  predecessor_revision_digest,
  artifacts: [{ id, kind, content_digest }],
  required_schema_digests
}
```

Every field participates in canonical graph-plan hashing. The full vendor manifest,
install checkpoints, mutable local readiness, host paths, and ACP grants do not. If a
value is not a typed `GraphPlan` field, it is not hidden as extra durable JSON.

## Bundled catalog

Keep the catalog in the existing `gents` runtime crate so CLI and desktop bridge use
the same parser and DTOs without a new workspace crate. Store source-controlled assets
under `crates/gents/assets/graph_packages/code-review/` and generate a deterministic
index at build/test time. `include_bytes!` (or an equivalent generated static table)
provides immutable lookup without opening a home.

The catalog gate parses all manifests, verifies referenced content digests and paths,
validates compatibility and dependencies, and compiles each intent against fixture
bindings and the declared task-backed `StageCapability` values. Each capability asset
names exactly one Task asset; no capability repeats behavior, prompt, tool-selection,
or authority configuration already owned by that Task's normal desired-state chain.
The gate asserts repeatable archive and plan digests.

`gents graph catalog` reads only this static table. With an optional home it may join
the matching `GraphDefinition` and active `GraphRevision` to report installed,
disabled, current, or upgrade-available; that join does not alter catalog truth.

Conversion moves the current code-review files into the canonical asset tree and
removes the obsolete demo pack, Make targets, environment variables, and root-stamp
special cases. Product execution has no fallback to `./demo/<name>`.

## Installation, successor revisions, disable, and GC

### Durable ownership: extend, do not duplicate

Only three existing document owners are needed:

| Existing document | Minimal additions/ownership |
| --- | --- |
| `GraphDefinition` | Add only mutable `enabled` (default true). `active_revision_digest` remains the sole pointer and `generation` its CAS generation. Package graph IDs are deterministic from owner DID plus package name, so no `source_package` column is needed. |
| `GraphRevision` | `plan_json` is exactly the typed `GraphPlan` and its `PackagePlan`; add only bounded `materialization_error`/timestamp if the existing schema lacks them. `artifacts_complete` remains the readiness bit. |
| `GraphRun` | Continues to join by `revision_digest`; additions are limited to cancellation intent and named terminal result commit references. Package/config attribution is reconstructed through the pinned revision instead of duplicated columns. Progress remains a derived view. |

This deliberately rejects dual package/graph pointers and separate artifact ledgers.
The revision digest covers configuration, so two different configurations cannot
share a revision. The active revision is the installed active configuration.

### One compositional install operation

Installation does not introduce a new legal lifecycle. It composes the already-proven
desired-state apply/reconcile behavior with the graph revision lifecycle:

```text
absent or incomplete revision
  └─ idempotent schema/apply/materialize ─► validated + artifacts_complete
                                             └─ existing CAS activation ─► active

any pre-activation error ─► same inactive revision + bounded materialization_error
retry same digest ─────────► re-observe exact effects and continue
```

The operation is:

1. Read, digest, validate, resolve compatibility, and type-check every explicit
   variable/role/deployment/backend/ceiling binding before writes.
2. Compile the fully bound package to one canonical `GraphPlan`. Allocate desired
   resource IDs in the reserved `pkg-<package>-<configuration>-<component>` namespace,
   where the configuration component covers bundled bytes and typed bindings. The plan
   lists each exact owned ID and canonical expected content digest. Configuration IDs
   cannot embed the final revision digest because those IDs are themselves inputs to
   that digest; the exact plan allowlist avoids a second mutable ownership ledger.
3. Create the deterministic `GraphDefinition` with `enabled=true` and a null active
   pointer if absent, or verify the existing definition without changing its enabled
   state. Then create or verify its inactive `GraphRevision` receipt with
   `artifacts_complete=false`. An existing digest is accepted only if `graph_id` and
   `plan_json` are byte-identical. `--no-activate` therefore leaves a complete revision
   attached to a definition, never an orphan keyed only by digest.
4. Provision package SDL/patches using the current schema-first code and verify the
   required collection versions. No configuration resource is considered ready before
   this succeeds.
5. Generate one existing desired-state bundle per bound principal as necessary and
   apply schemas, behaviors, tool selections/surfaces, and Tasks in current dependency
   order with prune disabled. Then use existing graph materialization for deterministic
   revision-owned EventTriggers only. Every write is idempotently compared with the
   plan's expected content digest.
6. Re-read schema versions and every owned resource. Resolve each behavior through the
   ordinary tool-surface reconciler and verify bindings/ceilings. On success set
   `artifacts_complete=true` and clear the retry error; on failure leave the revision
   inactive and record a bounded error on that same receipt.
7. Return the immutable revision receipt and exact publish command. A later explicit
   `publish` performs authorization and exact-digest confirmation, then uses the
   existing expected-generation CAS to advance `GraphDefinition.active_revision_digest`.
   Never change `enabled` implicitly.

Repeated installation with the same manifest and bindings produces the same revision
digest, observes all effects, and is a no-op. On retry, a missing expected artifact is
written, a matching content digest is skipped, and a mismatched digest fails closed as
drift while leaving `artifacts_complete=false` with a bounded error. Retry creates no
counter, second receipt, or new revision status just because a process restarted.

### Visibility and replicated failure closure

Keep the revision-digest parser and predicate for materialized EventTriggers.
Package-owned Tasks, behaviors, tool selections, and tool surfaces use the reserved
`pkg-` namespace and an exact allowlist derived from the same active-revision plus
nonterminal-run digest set. Both paths consult immutable `GraphRevision.plan_json`;
there is no package install-state collection or second activation pointer. Ordinary
operator resources keep their existing semantics, while an unknown/malformed reserved
package ID is invisible. Any hot update or deletion of a `pkg-` document forces an
ordinary full runtime-view reload so the plan digest and live semantic digest are
rechecked before reconciliation exposes it.

A revision-owned resource is discoverable by the runtime only when:

1. its digest-qualified ID belongs either to the active revision or to a revision
   pinned by a nonterminal `GraphRun`;
2. that revision is `artifacts_complete` and its `plan_json` expects that exact ID and
   canonical content digest;
3. required schema versions exist locally; and
4. normal owner, resource-enabled, tool-reconcile, deployment, workspace, and ceiling
   checks pass.

This is one pointer and one fence. The live-run digest set is retention derived from
existing run pins, not another activation pointer. During upgrade, the old active
revision remains visible until the final CAS and afterward remains visible only while
a nonterminal run needs it. A peer that receives config or an active pointer before
schema/resources cannot reconcile or start it; local readiness is derived by the same
checks and reported as an error, not stored as a new placement document. Convergence
is idempotent after the missing schema/resources arrive.

`enabled=false` is checked by new-start/replay admission, not artifact visibility.
Otherwise disabling during a run would strand its existing triggers and requests.
Successor install may stage or activate a revision while disabled, but does not
re-enable it. Observation and terminal recovery remain available.

### Successor installation and collection

Successor `install` starts from bundled defaults plus explicitly selected compatible
bindings, applies an operator patch, revalidates role/deployment/tool/ceiling
constraints, and creates a successor plan/revision. Selecting a newer bundled version
also checks its predecessor policy. It never edits package bytes, existing
desired-state documents, or the predecessor revision. Backend and credential values
remain references. Failed schema/resource/reconcile work leaves the prior active
digest untouched; new runs pin the successor only after CAS activation.

`gc` is an exact revision-plan deletion, not desired-state prune. For each inactive
revision it:

1. refuses the active digest and any digest pinned by a nonterminal run;
2. reads the owned IDs from that immutable revision's plan;
3. verifies every target still has the expected graph-artifact ID and content digest;
4. deletes only that allowlist in reverse dependency order; and
5. never deletes schemas or package result documents in v1.

Terminal observation does not pin executable graph artifacts: it uses `GraphRun`,
result references, `AgentRequest`, and timeline rows. The revision document remains as
provenance even after its owned executable/config artifacts are collected. V1 ships
the exact GC library and tests, but not an all-purpose uninstall or destructive public
GC command in the first slice. Nothing calls `--apply-prune`, and unrelated home
configuration is never a candidate. GC has a distinct destructive authorization; it
is never implied by install or publish.

### Installation invariants and proof boundary

No new install state machine means no new `GraphPackageInstall.lean` model. The
load-bearing properties should be expressed at existing boundaries:

- `ApplyReconcile.lean` and its conformance suite continue to own idempotent apply,
  dependency ordering, and no accidental delete; package tests lock prune off.
- `GraphPipeline.lean` continues to own `artifacts_complete`, activation readiness,
  pointer alignment, and run pinning. Add or retain the theorem that incomplete or
  mismatched revision artifacts cannot activate/start.
- Rust conformance covers the expanded visibility predicate for every revision-owned
  config kind and all replication orders.

If implementation requires a new revision status or legal retry transition after all,
that change must start in Lean. The proposed design does not require one: failure is
diagnostic data on an inactive revision, not a new lifecycle state.

## Compiler gaps and capabilities

### Code-review graph semantics

Extend existing types rather than add package-only execution concepts:

```text
GroupCount = Static(u32) | SourceField(field_name)
DeliveryMode::PerGroup { expected: GroupCount, timeout: Option<Duration> }
DeliveryConcurrency = Parallel | Serial
GraphEdge { delivery, concurrency: DeliveryConcurrency, ... }
```

The compiler validates source-field existence/type, group bounds, serial concurrency,
timeout bounds, and capability compatibility, then lowers `SourceField` to the
existing EventTrigger `expected_count_field`, `concurrency`, and
`group_timeout_secs`. The materializer must no longer always write null/default values
for those fields. The code-review verifier uses serial concurrency and a nullable
group timeout exactly as the existing pack requires; the run contract supplies an
independent bounded overall deadline.

These are compiler/materializer semantics, not graph lifecycle transitions.
Compiler conformance and snapshots are the primary fence; do not put compiler details
into the Lean pipeline model, which intentionally abstracts compilation.

Result predicates are generic plan data: named port, collection, correlation field,
cardinality, terminal/optional flag, and optional equality/bijection assertions over
declared fields. The code-review ledger can express expected-total consistency,
distinct lens IDs, sentinel/result balance, and exactly one triage report with these
generic predicates. Do not add `validators::code_review` unless a later package proves
a reusable predicate is missing; add that predicate generically first.

### Capability catalog and ordinary model tools

Persistent `StageCapability` documents are not required to run the bundled acceptance
package. For the vertical slice, capabilities are immutable manifest assets fed into
the existing compiler and copied into the revision plan. Installed capability views
are derived from revision plans still present in the home; bundled views come directly
from the catalog.
This is enough for CLI validation, diagnostics, and exact replay without a new catalog
collection.

Authoring needs one `CapabilityCatalog` runtime view, not necessarily a persistence
type. It merges caller-visible bundled capabilities and installed revision
capabilities, returning the existing `StageCapability` DTO. If future operator-authored
capabilities must persist independently, add them through the existing schema plus
desired-state `Collection`/ApplyReconcile path only after that use case exists.

When model graph authoring becomes a production feature, register propose/validate,
publish, and run as ordinary host/datastore tools selected by a behavior's existing
`ToolSelection`. Reuse `tool_surface::{selection,build,explain}` for owner checks,
ceilings, disablement, and diagnostics. Publishing and running still call separate
authorized runtime endpoints. Models may propose typed intent but cannot author raw
tool grants, principals, deployments, or prompt-only policy.

Pin the approved Task ID in each capability-manifest entry and pin canonical digests
for the Task and its package-owned desired-state dependencies in `PackagePlan.artifacts`.
Runtime execution continues through normal Task → Behavior → ToolSelection resolution;
the graph plan does not duplicate those fields. Avoid one global tool-surface hash that includes irrelevant host features
and would falsely reject valid replicas; readiness recomputes only the selected
resources and required host features.

## Execution contract

### Entry and pinning

For `code-review review`, CLI validates the path is a local Git repository, resolves
base/head to immutable commit SHAs, records the logical repository/workspace placement,
and provisions the existing request-scoped isolated workspace. Shared documents never
contain checkout paths.

The atomic start operation:

1. authenticates start authority independently of publish/install authority;
2. requires `GraphDefinition.enabled=true` and one active, complete, locally ready
   revision;
3. validates typed entry input and graph/operator limits;
4. resolves exact role principals/deployments, behavior/selection/surface document
   digests, workspace binding, package/config/result contract, and compiler version
   into `semantic_manifest_json`; and
5. atomically writes `GraphRun(status=running)` and the seed with controller-owned
   correlation.

The caller cannot supply/override the correlation or graph lineage fields. A graph
trigger may materialize a graph-owned request only when its correlation resolves to a
nonterminal `GraphRun` pinned to the trigger's revision. Disable blocks new root runs, while
that correlation rule permits pre-disable runs to finish.

### Minimal `GraphRun` additions

Keep existing immutable identity, revision, input, semantic manifest, limits, status,
error, and timestamps. Add only fields the run itself must own:

- nullable `cancel_requested_at`, `cancel_requested_by`, and bounded reason;
- nullable canonical `result_refs_json`, containing named document IDs and exact
  commit CIDs; and
- a monotone update generation only if DefraDB CAS requires one for terminal races.

The existing `error` string carries a bounded, versioned typed-error JSON value rather
than a package-specific message convention. Result references become immutable in the
same transaction/CAS that wins `succeeded`.

Package version, digest, catalog, configuration, principals, and contracts remain
inspectable through the pinned revision and semantic manifest; they need not become a
second set of mutable columns.

### One projection and one terminal CAS

Add `load_graph_run_view` in runtime library code. It loads the run and pinned plan,
finds requests by the existing correlation/lineage fields, joins current request and
trigger-group states, and composes `run_timeline_fetch` for selected request detail.
It deterministically returns:

- immutable attribution and entry input;
- ordered stage request counts/states and group progress;
- limit usage and cancellation intent;
- named result contract satisfaction and exact result references;
- current/terminal status and typed failure evidence; and
- links to existing adapter projections per request.

Any deployment authorized to reconcile the visible live correlation may evaluate the
view and attempt the terminal CAS. There is no exclusive completion-owner lock: one
CAS wins and every loser reloads, so loss of the starting deployment is not a liveness
hole. This is a projector/reconciler, not a scheduler: it never creates a stage,
trigger, task, or request.

Progress is always the derived `GraphRunView`—completed/active/expected counts by
stable node ID over durable source documents. It is not persisted as a second mutable
transcript or cache on `GraphRun`.

A run succeeds when every required stage/request relevant to terminal results is
terminal-successful, all required result predicates hold, no required group failed or
timed out, and the pinned invocation limits hold. It fails with typed evidence on a
required request/group failure, timeout, contract drift, or limit violation.

### Run completion and cancellation state machine

Rust already starts durably at `running`; preserve that behavior:

```text
Running ── all success predicates + result CAS ──► Succeeded
Running ── required failure/timeout/drift/limit ─► Failed
Running ── cancel intent + work suppressed/interrupted ─► Cancelled
```

Terminal states are immutable. Cancellation is durable intent orthogonal to status;
there is no new `Cancelling` state. Once intent exists, trigger materialization for
that correlation is suppressed and existing request interrupt/cascade APIs are used.
Recovery repeats those idempotent actions until no graph work is active, then attempts
the terminal CAS. Success, failure, and cancellation races have one winner; losers
reload the terminal run. Partial outputs remain inspectable but are not successful
named results.

`run --replay-of` creates a new run/correlation with the source run's exact revision
and canonical input after rechecking start authority, local schema/resources, and
current operator ceilings. Because shared documents contain no checkout path, replay
also requires `--repo` or a currently resolvable eligible repository/workspace
placement; absence fails closed. It never switches to the active revision. `watch`
and `result` are observational replay from durable rows and work after process restart
or revision retirement.

### Formal and conformance work

Extend `Proofs/GraphPipeline.lean` before the Rust terminal writer:

1. atomic start establishes running run plus seed and preserves the active revision
   pin;
2. succeeded requires one abstract `result_contract_satisfied` predicate and no
   required failure;
3. failed/cancelled/succeeded are terminal and preserve the run's immutable
   `revision_digest`; only nonterminal runs retain executable resources in the runtime
   admission set;
4. cancellation intent forbids new correlated materialization and permits the direct
   terminal cancelled transition once active requests terminalize;
5. success/failure/cancel races have at most one terminal winner; and
6. disable, activation, successor installation, and revision retirement cannot alter
   an in-flight run contract.

The abstract success predicate belongs in Lean because it gates a legal lifecycle
transition. Concrete result cardinality/bijection evaluation, immutable commit
references, compiler semantics, and cancellation recovery fairness stay in compiler
snapshots and Rust conformance rather than expanding the graph lifecycle model. Drive
fixtures for legal/illegal transitions, terminal truth tables, cancellation recovery,
terminal races, drift, limit breach, and replay pins. The runtime's direct `running`
creation must conform to the model's atomic start transition rather than exposing an
implementation-only pending record. Zero `sorry`s is mandatory.

## Authority boundary

Request intent/execution authority hardening is a shipping dependency, not a reason
to add a package request path. The hardening issue must inspect and fence the real
paths:

- `graph_pipeline::start_graph_run` for root start/replay;
- `trigger_engine/production_materializer.rs` and
  `lifecycle/materialize.rs` for graph-correlated child requests; and
- other direct `create_AgentRequest` mutations so graph-owned behavior/task IDs or
  claimed graph correlation cannot bypass start/lineage checks.

Ordinary non-graph requests keep their existing path. A graph-attributed child request
must have controller-authored lineage to a nonterminal pinned run. Publishing/
activation and starting are separate ACP decisions even when one CLI invocation asks
for both.
Cancel and observe are independently checked. Each decision is auditable through the
affected revision/run and existing identity/ACP records; bundling writes no grant.

The exact action identifiers and policy document are owned by the authority-hardening
track because the repository does not yet establish graph grant vocabulary. Package
code consumes that API and cannot define a weaker substitute.

## Observation and authoring surfaces

CLI semantics:

- `run` prints a run ID after the atomic seed and supports `--watch`.
- `run --replay-of` preserves the source revision/input but requires a current eligible
  repository/workspace placement and fresh start/readiness/ceiling checks.
- `watch` repeatedly loads `GraphRunView`, reconnects with backoff, and exits 0/1/130
  for succeeded/failed/cancelled. `--jsonl` emits versioned view snapshots.
- `result` reads exact named result refs; a stage option loads the existing timeline
  and adapter projection rather than synthesizing a graph transcript.
- `catalog <name>` shows immutable manifest, compatibility, variables, roles,
  contracts, authority requirements, ceilings, and optional derived install/readiness
  state.

Desktop follows the proven bridge pattern: additive permissions, Rust DTOs, generated
TypeScript, contract fingerprints, and Tauri commands that call catalog/install and
`load_graph_run_view`. The event pump emits a coarse refresh reason; the client never
decides completion. Selecting a stage reuses the current request timeline panels.

The first authoring UX is a typed form/structured JSON editor, not a canvas:

- human editor and model tool both serialize the same `GraphIntent`;
- capability choices come from the derived `CapabilityCatalog` view;
- validate/repair call the same pure compiler and return stable `{code, path,
  message}` diagnostics;
- the review screen shows prospective plan/revision digest, predecessor, role and
  deployment mappings, selected tool surfaces, schemas, ceilings, and authority
  deltas; and
- publish requires confirmation of that exact digest through the separate publish
  endpoint. Validation never implies publish, and publish never implies start.

## Code-review quickstart acceptance

The lifecycle gate runs a release-built `gents` binary from a temporary directory
with no source checkout. It creates a temporary Git repository with base/head commits
and uses a deterministic fake OpenAI-compatible backend that emits a closed four-lens
ledger. An optional live-model lane measures quality but is not a lifecycle gate.

| Case | Required assertion |
| --- | --- |
| Clean catalog | With no home, `graph catalog --json` finds and validates bundled `code-review` and creates no files. |
| Clean install | Explicit bindings lower to ordinary desired-state resources and one complete revision; schema provisioning occurs first; exact digest confirmation and publish authorization activate it; a separate start authorization is required. Every document is attributable through package plan, revision, principal, and deployment. |
| Repeated install | The identical command is a no-op: revision digest, active generation, document counts, and content digests do not change. |
| Interrupted install | Inject failure after receipt, schemas, and resource subsets. The incomplete revision records bounded failure, no partial graph artifact is visible/startable, and retry re-observes by digest and activates exactly once. |
| Shared-home isolation | Seed unrelated behaviors, selections, surfaces, tasks, and triggers. Install, failed install, successor install, disable, and GC never mutate/delete them; package code never invokes apply-prune. |
| Configuration change | Install with a changed reviewer backend or lens bound. A successor revision activates; bundled bytes and predecessor remain identical. An already-running predecessor run completes against its pinned manifest. |
| Run start | Run against the temporary repository. Input persists resolved SHAs and logical placement, not checkout-relative package paths. The isolated workspace stays within the operator ceiling and the source repository remains unchanged. |
| Progress/completion | Watch observes recon, four scanner requests, one verifier, and one triage. Source-field group count is immutable/consistent/bounded. Success occurs only after generic balanced-ledger predicates and the terminal report pass. |
| Output retrieval | Result returns exactly one report plus confirmed findings with document IDs and commit CIDs. Repeated retrieval is byte-stable; stage timelines and all existing adapter projections remain available. |
| Restart/reconnect | Restart after seed and during fan-out. Trigger, request, workspace, and completion recovery converge. Restart watch; its final view/result digest equals uninterrupted observation. |
| Replay | `run --replay-of` creates a fresh run/correlation pinned to the source revision/input, not the current active revision, after fresh authority/readiness/ceiling checks and explicit or resolved current repository placement. |
| Upgrade | A fixture catalog offers v1.1. Failed successor install leaves v1 active. Successful install CAS-activates v1.1; new runs use it. While an old run is nonterminal, GC refuses its executable artifacts; afterward those artifacts may be collected while old run/results remain inspectable. |
| Disable/enable | Disable during an active run: it finishes and remains observable, while new run/replay is denied. Successor install does not re-enable. Enable rechecks readiness before admission. |
| Cancellation | Cancel during fan-out and restart. No new correlated requests appear, existing requests converge terminal, the run becomes cancelled, and partial outputs are inspectable but not successful results. |
| Authority denial | Independently deny schema/install, publish, start/replay, cancel, observe, and destructive GC. Wrong role principal/deployment, cross-owner tool resource, requested network, workspace escape, and over-ceiling graph all fail closed. Bundling alone grants nothing. |
| Replication/drift | Deliver active pointer/config before schema or mutate an expected revision-owned resource in fault fixtures. Reconcile/start fails closed with typed readiness/drift evidence and never falls back to current unrelated config. |
| GC | The library's exact inactive-revision allowlist deletion preserves active/nonterminal-run-pinned resources, all schemas/results, revision provenance, and every unrelated home document. A public command is follow-on. |

GraphQL tests additionally require `graphql::escape_graphql_string()` for every
interpolation and `null`, never `[]`, for nillable empty arrays.

The P0 vertical slice gates clean/repeated/interrupted install, shared-home isolation,
run/progress/result/restart, disable, cancellation, authority denial, and replication/
drift. Configuration successor, bundled-version upgrade, replay, and a public GC
command close the successor-lifecycle follow-on; their acceptance cases are specified
now so the core data model cannot make them impossible.

## Milestone definition of done

The **P0 built-in code-review CLI vertical slice** is done when:

1. A release binary outside the checkout catalogs and explicitly installs code review
   into a non-empty shared home using existing desired-state and graph machinery.
2. The revision receipt proves exact package/config/provenance, schema-first readiness,
   idempotent resume, one active pointer, safe disable, and plan-scoped GC rules.
3. Typed initial role/principal/deployment/backend/ceiling bindings lower to ordinary
   tool-reconciled resources without a second control plane.
4. The graph faithfully represents dynamic fan-in, serial concurrency, and timeout
   semantics already required by the pack.
5. CLI start/watch/result/cancel works for an arbitrary eligible local Git repository
   with derived progress, durable terminal output, and restart recovery.
6. The P0 acceptance subset passes, including authority denial, replication order,
   drift, disable, cancellation, and shared-home isolation.
7. Lean/conformance changes land before lifecycle Rust with zero `sorry`; gates include
   `cargo test -p gents`, CLI integration tests, and
   `cargo check --workspace --all-targets`.

The **successor-lifecycle closure** then requires changed configuration and newer
bundled versions to install as immutable successor revisions, exact replay through
`run --replay-of`, and the GC acceptance case. Desktop catalog/run inspector, typed
human/model authoring, and any independently persisted operator capability catalog are
later milestones. All use the same runtime APIs and abstractions.

## Ordered issues and file-level implementation plan

Keep one Rust implementation issue active at a time. The next issue's Lean/design or
fixture work may overlap only where it does not edit the active issue's Rust surface.
Every issue branches after its predecessor and does not modify the four graph-stack
commits.

### 0. Shipping dependency: authority hardening (owning stack, may overlap)

- Fence `graph_pipeline/runtime.rs::start_graph_run`,
  `trigger_engine/production_materializer.rs`, `lifecycle/materialize.rs`, and audited
  direct `create_AgentRequest` sites using the owning ACP/request-intent design.
- Require controller-authored nonterminal GraphRun correlation for graph-owned child
  requests.
- This may progress alongside issues 1-5, but it is a merge/shipping gate before the
  executable CLI slice. Package work consumes its API and does not name a parallel
  grant document.

### 1. GraphRun terminal contract: Lean and conformance only

- Extend `crates/gents/proofs/Proofs/GraphPipeline.lean`,
  `crates/gents/proofs/Proofs/Conformance/GraphPipeline.lean`, proof README/coverage,
  and conformance fixtures with atomic start, abstract result satisfaction, immutable
  terminal pins, cancellation intent, and terminal-race safety.
- Align atomic Rust create-and-seed with the modeled start transition; do not add
  `Pending`/`Cancelling` implementation states.
- Keep concrete compiler/bijection semantics and recovery fairness out of this model.
  Safe overlap: compiler fixtures for issue 2. No lifecycle Rust changes.

### 2. Typed plan and faithful code-review compiler/materializer

- Extend `crates/gents/src/graph_pipeline/{types,compiler}.rs` with
  `GroupCount::SourceField`, serial concurrency, bounded nullable timeout, entry/result
  predicates, typed `PackagePlan`, and plan-owned resource content digests.
- Pass those fields through `crates/gents/src/graph_pipeline/runtime.rs` to the existing
  EventTrigger fields. Graph materialization validates referenced Tasks and writes only
  triggers. Add compiler/materializer conformance and canonical digest fixtures.
- Keep task-backed `StageCapability` as the existing DTO and in-memory compiler input for this
  issue. Change the pre-release formats directly; do not add compatibility readers.

### 3. Extract desired-state apply into the runtime

- Extract/reuse desired-state schema/apply/diff helpers from
  `crates/gents-cli/src/desired_state/{apply_bundle,load,normalize,validate,write}.rs`
  and `crates/gents-cli/src/config_import.rs` behind runtime library APIs.
- Preserve schema-first dependency ordering and idempotent canonical-content compare;
  make prune unreachable from the package API. Never duplicate GraphQL writers.
- Add parity tests for ordinary CLI apply before any package install command uses the
  extracted API.

### 4. Revision visibility and live-run retention

- Reserve `pkg-` IDs for revision-plan-owned config resources, including ordinary
  desired-state Tasks, compute visible digests as active revisions union nonterminal-run
  pins, and exact-allowlist those IDs in `crates/gents/src/agent/document_view/{load,apply}.rs`
  and tool-surface loaders. Keep the revision-digest parser for graph-owned EventTrigger IDs.
- Require complete typed plans with matching artifact content digests; incomplete and
  malformed reserved artifacts are never visible.
- Add activation-switch, live-old-run, terminal-old-run, drift, and replication-order
  conformance tests. No install command in this issue.

### 5. GraphRun view and terminal CAS writer

- Extend `crates/gents-schemas/schemas/agent/graph_run.graphql`, schema
  exports/migrations, and protocol DTOs with cancellation intent and terminal result
  refs only.
- Add `crates/gents/src/graph_pipeline/{run_view,run_completion}.rs`; reuse
  `run_timeline_fetch`, trigger group queries, request interrupt/cascade APIs, and
  immutable commit loading. Do not persist progress or an exclusive completion lock.
- Run the same projector/reconciler from the normal daemon lifecycle so completion is
  materialized without a connected CLI or UI. Permit authorized recovery deployments
  to race idempotent terminal CAS; add restart,
  result-reference, cancellation, and terminal-race conformance tests.

### 6. Read-only catalog and first revision-backed install

- Add focused `crates/gents/src/graph_package.rs` code and assets at
  `crates/gents/assets/graph_packages/code-review/`; reuse graph intent, desired-state
  DTOs, canonical JSON, and compiler types.
- Add deterministic catalog/package digest, dependency/path/schema-namespace,
  typed-variable, and fixture-compile gates. Add read-only `graph catalog [name]` that
  works with no home.
- Extend `crates/gents-schemas/schemas/agent/graph_definition.graphql` with only
  `enabled` and `graph_revision.graphql` with bounded materialization diagnostics;
  update migrations, exports, DTOs, and readiness conformance.
- Implement definition creation, incomplete revision receipt, schema/apply/materialize,
  exact digest re-observation, publish-authorized CAS activation, and disable/enable.
  Add interrupted/repeated install and non-empty shared-home tests.
- Move code-review assets only when the catalog/install gate passes; remove the
  pre-release checkout-relative pack without a compatibility lookup.

### 7. P0 code-review CLI vertical slice and clean-binary acceptance

- Finish the asset move, namespace schemas, remove Gents checkout markers/defaults,
  and lower the current desired state/experiment checks to the generic manifest/plan.
- Add run/watch/result/cancel command handlers using existing repository
  placement and isolated-workspace APIs plus `load_graph_run_view`.
- Add release-binary tests under `crates/gents-cli/tests/` with temporary Git repo,
  fake backend, restarts, authority denials, output commits, disable, cancellation,
  drift, and replication ordering.
- Update `demo/README.md`, operator docs, and release asset freshness gates.

### 8. Successor installation, replay, and GC closure

- Reuse `install` for changed bindings and newer bundled versions; test failed and
  successful successor CAS while old nonterminal runs retain their artifacts.
- Add `run --replay-of` with explicit/resolved current repository placement and exact
  source revision/input pins.
- Implement and test plan-allowlisted GC with distinct destructive authorization. Ship
  a public command only after library fault tests prove active/nonterminal pins,
  schema/result preservation, and unrelated-home isolation.

### 9. Desktop catalog and run inspector

- Add thin DTOs/commands in `gents-desktop-bridge/src/{types,commands,tauri_commands}.rs`,
  permissions/inventory in `contract.rs` and capability TOML, and generated TypeScript
  fingerprints.
- Call the same catalog/install and `load_graph_run_view` functions. Reuse existing
  request timeline/adapter panels for stage detail. No UI terminal reducer.

### 10. Typed authoring and ordinary reconciled graph model tools

- Add the derived `CapabilityCatalog` view, canonical GraphIntent editor, stable
  diagnostics, prospective digest diff, and explicit publish confirmation.
- Register graph model operations through existing
  `document_config/tool_selection.rs` and `tool_surface/{selection,build,explain}.rs`.
  Do not introduce a custom production injection path.
- Persist standalone operator-authored capabilities only if a concrete use case
  remains after the derived catalog; if needed, use ordinary desired-state
  ApplyReconcile and begin any lifecycle change in Lean.

### 11. Later packages

Only after the vertical slice: defending-code, security/SDLC packages, remote signed
catalogs, and the SDLC RFC. They reuse the same manifest lowering, revision, authority,
run, and observation contracts and cannot add an executor.

## Explicit non-goals

- No second workflow engine, package controller, package-specific scheduler, UI run
  state, or direct model-authored request path.
- No new package installation/release/config/artifact/attempt/placement collections in
  the first product path.
- No silent first-use install, automatic identity/tool grant, ambient shell hook, or
  prompt-only write/network policy.
- No vendor asset, active revision, or pinned run mutation.
- No home-wide desired-state prune for install, upgrade, disable, or GC.
- No code-review-specific terminal validator if generic result predicates suffice.
- No remote registry/download/signature system and no graph canvas before the durable
  CLI path.
- No compatibility readers, aliases, or data migrations for the pre-release demo and
  experimental graph/package serialization formats.

## Unresolved operator choices

Repository evidence resolves the architecture. These deployment policy values require
operator/product choice:

1. Which existing principal DID and host deployment bind each code-review role, and
   which backend/profile each role may use. An interactive single-candidate
   quickstart may propose the full mapping but still requires confirmation.
2. Whether a deployment permits the isolated write/build profile and optional network
   access within its ceiling. Defaults are network-off and fail-closed.
3. On hosts that cannot enforce workspace writes, whether v1 ships a strictly
   read-only/no-build profile or limits the full quickstart to platforms with the
   required sandbox.
