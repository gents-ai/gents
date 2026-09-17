# Behavior-scoped naming and configuration: implementation plan

Status: proposal for implementation, not an implemented contract.

Implementation base: `stack/onboarding-acceptance` at `72be84b87`, taken from
`gents-onboarding-coordinator` and verified against the pushed remote branch.
Planning branch: `docs/behavior-scoped-naming`, rebased onto that commit.
The 32-worker source audit ran at `a36055cec`; the subsequent structured-config
and mailbox-eval commit was reviewed separately and incorporated below.

## Product decisions

Generated names expose their scope directly. Do not invent title-case aliases,
Java-style concatenations, or a second presentation vocabulary.

```text
gents:base:configurator
gents:base:configurator:context
gents:base:configurator:tools
gents:base:configurator:inference
gents:base:configurator:sampling
gents:base:configurator:execution
gents:base:configurator:retry-policy
gents:base:configurator:compaction

gents:code-review:reviewer
gents:code-review:reviewer:inference
```

Use lowercase colon-separated scopes and kebab-case segments for generated
logical names. Descriptions explain the work. Namespaces are not identities,
authorization grants, or proof of a publisher's authenticity. DefraDB DID/ACP
continues to authorize documents; lookups remain principal- and collection-scoped.

The user's follow-up adds an important distinction: personal behaviors need
good generated defaults without restricting the names people choose in the UI.
Recommended personal grammar is `local:{slug}` and `local:{slug}:{component}`.
For example, a user can request the display name `Jack's code reviewer` while
the stable logical behavior key is `local:jacks-code-reviewer`. An unnamed
personal reviewer defaults to the visible name `local:reviewer`.

- Reuse the existing `display_name` field for an explicit user name. Do not
  title-case or humanize generated names automatically.
- Rename the display name without changing keys, component references, session
  references, or scope. Duplicate display names are allowed; selectors include
  their qualified key when disambiguation is needed.
- New keys use the naming convention. Existing retained keys are not rewritten
  on startup or inferred from a display name.
- Pack coordinates, installed scopes, descriptions, and display names are
  separate concepts. Do not globally replace underscores in registry coordinates,
  asset paths, GraphQL collections, tool names, or fixture markers.
- `local:{slug}` is a proposed default based on the user's latest suggestion;
  the exact personal-key grammar is to be fixed in the first contract change.

## Isolation boundary

A behavior owns the mutable configuration that determines its context and
inference. Creating or cloning a behavior produces an independent closure:

| Starting document | References included in the owned closure |
| --- | --- |
| AgentBehavior | AgentContext, InferenceProfile |
| AgentContext | Tools, Compaction |
| InferenceProfile | InferenceSampling, InferenceExecution |
| InferenceExecution | InferenceRetryPolicy |
| Compaction | Its optional summary InferenceProfile and that profile's dependencies |

Missing optional documents stay missing and retain runtime defaults. Materialize
a scoped document when the user first configures that setting; do not eagerly
create empty policy documents. Preserve aliases within a closure when the same
document is intentionally referenced twice. Distinct primary and compaction
profiles need distinct deterministic component paths, for example
`:inference` and `:compaction:inference`, including their separate sampling and
execution paths. Traverse collection-qualified references with a visited map.

InferenceBackend and credentials remain shared connections. Skill selections,
MCP service selections, subagent targets, and datastore selections remain explicit
references to existing reusable resources. Copying Tools must preserve their
grants exactly, not broaden authority or clone arbitrary external resources.
Resource edits can still have shared effects: expose affected consumers at those
owners rather than promising isolation beyond this closure. Tasks, schedules,
triggers, graphs, and runs keep their existing owners and lifecycle semantics.

Recommended enforcement: add a canonical, protected behavior association
(`scope_behavior_id`, provisional field name) to mutable component documents.
The ordinary typed reference validator checks that scoped components point only
within the selected behavior's scope, except for the documented shared resources.
Use existing canonical types, schemas, transaction and validation owners; do not
introduce a parallel configuration graph or authorization model. Tags may project
scope for filtering but are not authoritative ownership or selection data.

Before choosing the final representation, model whether the existing reverse
reference closure is sufficient without new stored metadata. The contract PR must
choose one representation and reject inconsistent declarations; implementations
must not maintain competing scope authorities.

An edit through a behavior must not mutate another behavior's owned documents.
Reject cross-scope rebindings and conflicting direct writes. Reading another
profile as a source is allowed within existing authorization, but selecting it for
a new behavior copies its mutable closure instead of retaining a live alias.
No new advanced shared-mutable-config mode is required for this rollout.

## Confirmed baseline gaps

1. `crates/gents/src/agent/persona_ops.rs::derive_behavior_id` derives a DID-prefixed
   slug from the display name. `apply_persona_request` uses request-key context/tools
   IDs for interrupted-apply repair and retains the selected inference profile.
   New naming must preserve durable idempotency, not simply replace the strings.
2. `crates/gents/src/pack/inference.rs::bind_pack_install_config` substitutes
   slot bindings directly into behaviors. `validate_pack_inference_authoring`
   rejects pack-authored inference documents. Multiple behaviors bound to one
   slot therefore share the profile. Keep source-profile selection, but copy
   its values into each installed behavior's independent inference closure.
3. Pack provenance uses `gents:pack:{snake_case_name}` tags and mutable discovery
   labels. Registry lookup uses `{namespace}/{name}`. Neither is already the
   proposed installed-object namespace, and neither should silently become it.
4. `SelfConfigCore` protects the invoking behavior against indirect lockout from
   shared inference edits. This is weaker than preventing sibling mutation.
   Preserve its lockout guarantees while adding the new isolation predicate.
5. CLI init, desktop runtime setup, provider setup, and various CLI shims derive
   default behavior/profile IDs. Updating only the configurator creator misses
   these independent entry points.
6. `CompactionConfig.inference_profile_id` is an optional additional inference
   reference. A shallow profile copy would leave a second sharing path.
7. Setup instructions currently teach reuse of existing profiles. The evals
   prescribe Builder/Explorer/Reviewer, high/medium/low profile bindings, and
   exact document counts. The new contract changes those relationships and
   counts; explicit user-supplied display names must still be preserved.
8. `document_config/behavior.rs::load_agent_behavior_record` filters only by
   `behavior_id` with `limit: 1`. Removing DID prefixes makes common names across
   principals routine. Audit and fix every caller requiring a principal-qualified
   lookup before publishing newly named behaviors; do not rely on global key
   uniqueness or the one-runtime-per-principal operating convention.
9. Desktop `createBehavior.ts` allocates timestamp-based keys and selects the
   first existing context/profile for an inert scaffold. Preserve the disabled
   scaffold guarantee, but allocate and materialize through the common owner.
   `ProfilesPanel.tsx` also derives sampling IDs locally with `-sampling`.

## Ordered implementation stack

Each implementation PR targets its immediate parent, with one integration owner
maintaining the stack and its validation record. Start from the latest accepted
coordinator commit, reconciling the staged/newer work before cutting the stack.
Resolve changes to the #1430 configuration specification before propagating
conflicting semantics. Do not ship a non-buildable types/proof foundation alone.

### 1. Naming and isolation contract; Lean model

Fix the personal-key grammar, reserved component segments, collision behavior,
length limits, explicit display-name semantics, and pack installation identity.
Define the exact closure, optional-default behavior, shared-resource exceptions,
and authoritative scope representation. Use a single naming/parser helper at
authoring boundaries; runtime selection continues to use canonical references.

Update `Proofs/Configuration.lean`, `Proofs/ConfigDocuments.lean`,
`Proofs/PeerRegistryDiscovery/PersonaRequest.lean`, applicable `SelfConfig` and
`ApplyReconcile/Publication` models, plus graph configuration where necessary.
Prove sibling noninterference, clone independence, reference-scope consistency,
failure atomicity, and create/clone replay idempotency. Preserve DID ownership,
no-lockout, protected configurator, default selection, and publication guarantees.

Exit: explicit model and source-level contract with no `sorry`; `lake build`.

### 2. Model-driven conformance and canonical vocabulary

Generate conformance cases before implementing Rust behavior. Update schemas,
canonical Rust fields, collection metadata, desired-state field lists, immutable
patch masks, signed request payloads if affected, and generated client bindings.
Decide scope field mutability explicitly; generic sparse patches cannot move a
component into another behavior. Do not hand-edit generated TypeScript.

Cover same names under different DIDs, delimiter/collision cases, invalid
cross-scope references, null/omitted settings, shared backend exceptions, shared
source profiles, compaction profile branches, and transaction replay.

Exit: generated witnesses and runtime bridges identify the intended contract;
the integrated descendant must become green before anything merges.

### 3. Runtime materialization and all write boundaries

Implement one transactional closure-copy operation in the existing configuration
owner, reused by behavior creation, cloning, profile replacement, and pack install.
Snapshot source documents coherently and copy all values, including unknown-to-UI
but canonical fields. Resolve deterministic destination keys once. Atomically
publish components, behavior references, and optional default selection.

Replace request-key-ID-based repair with an equally durable mapping/receipt in
the existing signed-intent owner. Replaying a completed request must return its
original behavior, not allocate `-2`; distinct colliding requests need explicit,
preview-visible resolution. Display-name edits must not regenerate components.

Enforce scope in common reference/publication validation, then audit direct CLI,
desktop, self-config, config apply/import, and backend/provider setup entry points.
Make behavior/component reads explicitly `(agent_did, collection, logical_id)`
scoped wherever they currently depend on globally unique DID-prefixed names.
Document the boundary for raw DefraDB writes: ACP remains access control; neither
tags nor names magically enforce a new invariant for arbitrary external writers.
Invalid externally authored closures must not become runnable through resolution.

For existing shared or unscoped configuration, retain reads and stored identities.
Do not silently normalize or fan out an edit. Offer the ordinary explicit clone/
scope-materialization operation or reject with a precise consumer list. No data
wipes, alias database, historical conversion chain, or automatic bulk rename.

Exit: prove by persisted snapshots that changing any owned component of A leaves
B unchanged, including after restart, replication, and interrupted publication.

### 4. Bootstrap and pack installation

CLI and desktop bootstrap create `gents:base:configurator` with scoped components.
Preserve protected-configurator recognition through the existing semantic marker;
do not authorize/protect by display name. Runtime readiness, default selection,
provider setup, login, discovery, shims, and reopen paths must agree on the IDs.

Keep pack inference slots as authoring inputs. A binding selects a source profile
to snapshot separately for every installed behavior; it no longer promises a live
shared profile reference. Include copied values and installed scope in the preview
digest, and reject changed source configuration between preview and apply rather
than committing an unreviewed result. Preserve backend/credential references.
Update provenance stamping so newly copied pack-owned profiles/policies receive
the installation association, while source profiles and shared backends do not.
Review catalog auto-selection as copied profiles multiply: the existing
one-slot/one-usable-profile shortcut will otherwise stop being predictable.

Map pack-local document keys and typed references once through the existing pack
loader/installer. Carry namespace from bundle/registry provenance. Distinguish
distribution coordinates from an installation scope; if repeat installation is
supported, two instances must have disjoint config, task and graph references.
If that support is not ready, reject a second instance explicitly rather than
claiming it works from naming alone. Keep immutable graph revision/digest rules.

Update every shipped pack's behavior names, component names, task references,
slot behavior lists, graph capabilities/intents, subagent targets, callback args,
and prompt examples as applicable. Do not rename domain schema fields or external
tool contracts without a separate reason. Preview/export must show the actual
installed closure and provenance.

Exit: bundled document and graph installs, repeat install, changed-source preview,
two-instance isolation (if supported), and fresh graph execution pass.

### 5. Configurator prompt, authoring tools, and user interfaces

Rewrite `crates/gents-protocol/prompts/setup.md` around the new naming and ownership
rules after the materializers work. Keep the current mailbox, skill, authority,
preview, verification, and re-entry guarantees. Remove contradictory advice to
bind new behaviors directly to a shared mutable profile. Selecting existing
inference means copying settings while retaining the backend connection.
Preserve `72be84b87`'s structured model inputs (`argv`, `options`, `set`, `clear`,
and supported `target_id` positions). Name examples should use literal options
and native JSON patches, not revive JSON-within-argv escaping instructions.
Keep runtime-owned mailbox condition identity and typed created/reused/updated
receipts; naming must not become mailbox deduplication or routing authority.

Teach concrete examples for bundled keys, generated personal keys, explicit user
display names, clone collisions, and component edits. Read returned IDs and use
them exactly; models do not guess keys from names. The model suggests a role/name;
the canonical materializer allocates IDs. This also applies to optional recipes,
CLI help, and model-facing config help/errors.

Selectors use the explicit user display name or the qualified generated name.
Keep full keys available where names collide; do not strip scope unconditionally.
Editing a visible name preserves scope. Component editors show which behavior
they affect. Shared backends/resources show their broader impact. Update Setup
navigation copy to configurator consistently where it names the behavior; the
word "setup" can remain for the onboarding activity and internal contracts.

Exit: same identifiers and names across desktop/mobile, CLI, configurator output,
session reopening, and generated client projections.

### 6. Evals, examples, and final coverage closure

Update eval fixtures and assertions with the new prompt and implementation in the
same integrated stack. Separate generated-name cases from explicit-name cases:
an existing request to create `Builder` still expects that visible name; it now
also checks its scoped logical key and independent inference closure.

Add behavioral scenarios for:

- Generated `local:reviewer` and exact user-authored Unicode/punctuation names.
- Two custom names normalizing to one slug, duplicate display names, and retries.
- Two behaviors copied from one profile: edit model, sampling, execution, retry,
  compaction, prompt and tools separately; the sibling remains byte-for-byte stable.
- Separate compaction inference, missing optional documents, and shared backend
  mutation with correctly reported impact.
- Cloning, display renaming, default changes, protected configurator, re-entry,
  restart, and fresh-session selection without duplicate documents or lost edits.
- Pack roles using one source slot, repeat install preserving user edits, stale
  preview rejection, and two installations where supported.
- CLI/desktop/import writes cannot bypass the same isolation rules; failed writes
  leave canonical state unchanged.

Preserve exact prompt sentinels and meaningful outcome assertions. Update counts
to the intended owned closure; do not weaken checks to "at least one" to get green.
Validate names, stored references, scopes, snapshots, tool outcomes and actual
fresh-session execution. Assistant prose is supporting evidence only.

## Coverage process and parallel work

`coverage.csv` is a tracked-file lexical inventory at the implementation base, not a claim
that every match must change or that every indirect dependency was found. It
starts with more than 700 files across runtime, CLI, desktop, generated clients, schemas,
proofs, packs, evals, tests, and documentation. Each row starts `review-pending`.
During implementation assign each row one of `changed`, `retained-with-reason`,
or `generated-from:<source>` and record the responsible PR. Add indirect files
discovered by reference tracing and pack asset enumeration.

Audit both directions: from each renamed producer through every reader/reference,
and from each shipped example back to the owner that accepts it. Search spelling
variants (`Setup`, `setup-steward`, `default-profile`, `review-recon`, slot names,
snake_case pack names) and string-embedded IDs. Classify negative fixtures and
historical prose rather than mechanically replacing them. Generated files must
be regenerated and checked for drift. Include docs/scripts outside the first
lexical sweep and screenshot/accessibility expectations where visible copy changes.

The first audit uses 32 independent GLM requests through a worktree-local Gents
runtime at concurrency 32, with bounded source packets and read-only tools. Raw
prompts, responses, logs and lane definitions live under ignored
`.gents/naming-audit/`. Model reports are leads to verify, not authoritative findings.
The audit binary predates the baseline; it is orchestration tooling only, not
validation of the proposed implementation or the baseline's runtime behavior.

Implementation parallelism starts after contract and conformance are fixed:

| Workstream | Ownership |
| --- | --- |
| Core integration | Canonical types, naming helper, transaction/closure owner, proof/conformance coordination |
| Bootstrap | CLI init, desktop startup/provider setup, shims and default selectors |
| Packs | Loader/binding contract plus separate pack-specific fixture/example batches |
| Consumers | Desktop/client mutation adapters, editors and generated projections |
| Authoring | Configurator prompt, recipes, CLI/model help and documentation |
| Evals/coverage | Fixture assertions, live acceptance, per-file disposition and independent reference audit |

Use many read-only audit workers but fewer code writers with disjoint file
ownership. One owner edits shared materialization, schemas, generated code and
integration contracts. Workers report exact changed files, retained cases, tests,
and unresolved references. Cross-check every lane with a separate reviewer.

## Validation and merge gates

1. `lake build` for proof changes, generated conformance replay, targeted semantic
   tests for atomic closure copying and noninterference.
2. CLI configuration/bootstrap/pack suites and desktop tests for affected consumers;
   client generation and contract drift checks.
3. All bundled packs validate through their real loader/installer. Run representative
   graph packs and verify terminal artifacts, not just accepted previews.
4. Configurator live evals against an explicitly recorded model/endpoint, with
   retained canonical snapshots and reports. Parallelize isolated audit/eval cases;
   run stateful restart/re-entry scenarios serially within their isolated homes.
5. Before each push: `cargo test -p gents` and
   `cargo check --workspace --all-targets`, plus affected CLI/desktop suites.
6. Every inventory row has a reviewed disposition; a new sweep finds no unexplained
   old generated-name producers, shared-profile assumptions, or stale examples.
7. Rebase and validate the integrated stack against the latest coordinator work.
   No foundation merges by itself; no retained-state reset is used to pass tests.

The planning change itself contains documentation and an audit inventory only.
Runtime/proof/UI/eval changes belong to the implementation stack above.
