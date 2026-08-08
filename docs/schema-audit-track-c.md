# Schema Audit Track C: Provenance and Projections

This audit covers the durable provider-request fact and the read/export path
that turns runtime documents into run timelines and adapter projections. It
applies the [DefraDB schema guide](defradb-schema-guide.md) and the detailed
entry template in the [decision ledger](schema-decision-ledger.md).

Tracking issues: #1063 and #1069.

The audit separates facts about the current implementation from target
decisions. A target below is not implemented merely because it is documented.
Retention and post-erasure evidence states use the
[shared retention and erasure lattice](schema-retention-lattice.md).

## Scope and dependency graph

```text
AgentRequest exact version ──┐
configuration versions ──────┤
transcript/tool versions ─────┼──> RenderedRequest provenance manifest
ACP + signer evidence ────────┘                 │
                                                │
AgentRequest/Session/Conversation ──────────────┤
AgentMessage/ToolCall/Response/InferenceCall ───┼──> run_timeline vN
RenderedRequest ────────────────────────────────┘         │
                                                          ├──> ATIF vN
ProjectionAcpBinding + actor DID ──> ACP filtering ───────┼──> OpenAI/Codex vN
                                                          ├──> LangGraph vN
                                                          └──> multi-agent vN
                                                                    │
                                               archive/export manifest + decision
```

The canonical records are the DefraDB documents and their commit DAGs.
`RunTimeline` and every adapter shape are derived views. They are safe to cache
or export only with a manifest that identifies every exact source version and
the authorization/redaction decision used to produce the view.

## Current implementation facts

- `RenderedRequest` is an immutable, branchable fact written before provider
  send by `rendered_request/transport.rs` and `rendered_request/sink.rs`.
- Its signed source and claim edges pin the exact accepted `AgentRequest`
  version and the exact target-agent claim version by `_docID`, composite CID,
  and cryptographically verified signer DID. `capture_key` is an
  idempotency/query key, not an integrity proof. `request_json`'s DefraDB field
  CID is the payload integrity anchor.
- The v7 capture manifest also pins the exact signed running `InferenceCall`,
  the canonically ordered exact signed transcript snapshot loaded for the call,
  and a bounded exact signed core-config bundle for reconciled document-runtime
  behaviors: principal, behavior, backend, profile, optional tool selection,
  and effective skills. CID-only changes to those selected config facts rotate
  the runtime generation fingerprint and the same bundle is captured before
  send.
- The capture nevertheless remains `CapturedOnly`: that bounded bundle is not
  a separately published `ResolvedAgentGeneration` and does not prove the
  complete skill candidate set, MCP registry/discovery and health observations,
  host tool ceiling, compaction inputs for the provider call, placement/lease
  authority, ACP authorization, encryption, or a durable read decision.
- `RunTimelineRows` currently loads `AgentRequest`, `AgentSession`,
  `AgentConversation`, `AgentMessage`, `AgentToolCall`, `AgentResponse`, and
  `InferenceCall`. Tool result and approval facts are now loaded through their
  exact `_docID`/composite-CID/signer edges and verified against the physical
  historical `AgentToolCall`. It still does not load `RenderedRequest`,
  compaction entries, or a complete source-version manifest for the other rows.
- Finalized `CompactionEntry` facts now carry a canonical exact-source manifest
  over their signed transcript snapshot and prior compaction facts, with
  create-and-compare and pre-finalization re-verification. This is durable
  compaction evidence, but the central timeline/projection path does not yet
  consume it or explain compacted omissions.
- `run_timeline_fetch.rs` begins with `AgentRequest(request_id, order:
  created_at DESC, limit: 1)` and discovers related rows through logical IDs.
  This is a current-head operational view, not an exact historical read.
- `build_run_timeline` creates deterministic output only after its selected
  rows are known. Timestamp/order keys cannot make an ambiguous source query
  historically exact.
- Adapter envelopes declare projection id/version, source logical IDs,
  redaction mode, and a small provenance record. They do not contain the source
  document-version manifest, source schema versions, ACP policy/version and
  decision evidence, or signer evidence required for durable export.
- CLI `trace project` can discover a `ProjectionAcpBinding`, requires an actor
  DID and remote GraphQL access when enforcing ACP, asks DefraDB for per-document
  read decisions, and filters denied child rows. It fails closed when the root
  request is denied or a required `_docID` is unavailable.
- Binding selection rejects incomparable matches, but replicated concurrent
  bindings with the same logical scope still have no declared canonicalization
  or repair policy.
- Full, training-safe, and public redaction are implemented while constructing
  projections. The current envelope records the requested mode, not a durable
  proof of which ACP decision authorized each included source document.

## Target projection contract

1. Name an immutable `RunTimelineManifest` (or equivalent versioned envelope)
   for one projection build. It must pin every included source as
   `(collection, schema_version, _docID, composite_commit_cid)` and pin relevant
   field CIDs where a field is the exported evidence.
2. A root lookup by logical `request_id` is a discovery step only. It must fail
   on duplicates unless the caller supplies `_docID`, then freeze the selected
   root and every reachable row by exact CID before projection.
3. Include `RenderedRequest` in the timeline source model and expose the exact
   provider body only in projection variants/redaction modes whose ACP decision
   permits it. A request/version mismatch is an error, never a partial join.
4. Record projection implementation id/version, schema versions, build time,
   actor DID, active policy id/version, resource mapping version, per-document
   allow evidence or an auditable decision receipt, and redaction algorithm
   version. An output hash may aid deduplication but is not provenance.
5. Make projection generation pure over the frozen manifest: replaying the same
   authorized source versions with the same projection/redaction versions must
   yield byte-equivalent canonical output.
6. Define partial-read behavior per projection. The root is mandatory. Optional
   denied children must be represented as explicit omissions with collection,
   identity, reason class, and decision reference; silently filtering them can
   produce an apparently complete but false trace.
7. Export both the projection and its manifest. Archives must preserve commit
   signatures/signers when available and state `unverified` when evidence
   cannot be read or validated.
8. Projection caches, if added, are replaceable materializations keyed by the
   source manifest CID plus projection/redaction contract versions. They are
   never canonical facts.

## Collection decision: `RenderedRequest`

- **Primary archetype / meaning:** immutable durable fact for the exact body
  sent during one provider attempt. Canonical, not derived from a later replay.
- **Authority:** the runtime deployment executing the request is the sole
  creator. No transition writer exists; updates are illegal. The capture sink
  requires the node signing identity to match `agent_did`, cryptographically
  verifies the stored `RenderedRequest` commit signer, and re-verifies the
  signed request source/claim and running `InferenceCall` chain before send.
  This is authorship evidence; future ACP still decides whether that author was
  authorized.
- **Identity:** `_docID` is the durable identity. `capture_key` has uniqueness
  scope `(agent_did, session_id, request_doc_id + capture_scope, turn_index,
  attempt)` and exists for idempotent retry/query. Concurrent duplicate creates
  must be compared byte-for-byte and mismatches fail closed; a unique-index
  winner alone does not canonicalize replicated conflicts. `request_doc_id`
  plus the source and claim composite CIDs and signer DIDs form the signed
  execution-provenance edges. The `request_json` field CID is the payload
  evidence and must be exposed to archive manifests.
- **Mutability / illegal states:** every field is immutable. A document-backed
  capture without the complete signed source/claim reference set, a one-shot
  capture with a forged nonempty request reference, unsupported
  `capture_version`, malformed provenance, or a capture-key/canonical-body
  mismatch is illegal.
- **Gossip:** no participant route carries this raw provider body. Live
  delivery is limited to execution failover and explicitly governed audit
  sinks, filtered by immutable principal/session placement fields; participant
  projections are separately redacted facts. It must not default to all paired
  peers.
- **Backfill / branchability:** retain `@branchable`. It preserves
  peer-initiated collection catch-up and future collection-scoped ACP. A push
  replicator can backfill existing non-branchable documents, so generic
  backfill alone is not the rationale. Branchability does not supply document
  CIDs or gossip.
- **ACP / encryption:** target document and collection resources must permit
  create only to the executing deployment principal and read only to declared
  participants/auditors. Normal, CID, `_version`, and `_commits` reads need
  negative tests. Until DefraDB policy installation/relationships are available
  and proven, this fact is not participant-safe. Provider payload secrecy also
  requires local/archive encryption and key custody beyond delta encryption.
- **Retention:** hot for the run's audit window; cold export preserves body (or
  a governed redacted form), field/composite CIDs, signer evidence, lineage,
  policy/redaction receipt, and schema/contract versions. Legal hold supersedes
  sunset. Physical purge must cover peers, archives, backups, and keys; a
  tombstone is not erasure.
- **Writers / queries:** `rendered_request/transport.rs` captures and
  `rendered_request/sink.rs` inserts and checks by `capture_key`; lifecycle and
  daemon construction supply the request version. Target consumers are
  `run_timeline_fetch.rs`, `run_timeline.rs`, adapter projection builders, and
  CLI trace/export. Existing indexes on capture, request, session, principal,
  prompt/tools fingerprints, and creation time must each remain tied to an
  observed query; otherwise remove them after measurement.
- **Formal / migration:** `proofs/RenderedCapture.lean` and conformance tests
  fence persist-before-send and idempotency. Extend the model for manifest
  completeness and projection consumption. Schema work is intentionally
  breaking; no compatibility migration is required, but an explicit backfill
  policy must say that old captures are `CapturedOnly`, never upgraded by
  inference.
- **Decision:** target direction decided; signer verification is implemented
  for the bounded request/claim/inference/render and selected transcript/config
  evidence described above. Complete-generation evidence, ACP authorization,
  complete manifest semantics, and projection consumption remain unimplemented
  gates.

## Collection decision: `ProjectionAcpBinding`

- **Primary archetype / meaning:** desired authorization configuration that
  selects a published policy/resource-map version for one projection scope.
  It is not authorization evidence and must not also serve as its own rotation
  event log.
- **Authority:** an enterprise/security administrator authors desired bindings;
  a policy publisher records immutable publication/rotation facts. Runtime
  projection readers never mutate bindings.
- **Identity:** replace opaque global `binding_id` assumptions with a declared
  scope tuple `(owner agent _docID or DID, optional behavior _docID, optional
  projection_id, environment/network)` and a durable `_docID`. Discovery must
  reject duplicate equivalent scopes and replicated conflicts. Do not select
  one with `limit: 1` or timestamps.
- **Target split:** keep mutable desired selection (`enabled`, selected
  published policy-version reference) separate from immutable
  `ProjectionPolicyPublication` facts (`policy_id`, version/CID, resource map,
  publisher signer, published_at, predecessor). Remove staged/previous/status
  combinations from the binding envelope once the publication fact exists.
- **Types:** use `DateTime` for timestamps. Use null for absent behavior or
  projection scope. Replace query-significant JSON resource mappings with a
  versioned referenced document or typed relationships; never reinterpret an
  old mapping in place.
- **Gossip / backfill / branchability:** bindings are principal/network-scoped
  configuration and must reach every authorized projection host, including late
  joiners. Because the current collection is nonbranchable and branchability is
  irreversible, introduce a branchable successor rather than pretending an
  annotation flip is possible. Local-only overrides, if retained, belong in a
  separate non-gossiped collection.
- **ACP:** administrators/publishers can create or rotate; deployment principals
  can read only bindings for agents they host; projection actors cannot read
  policy configuration merely because they can read an exported projection.
  Policy installation and relationship creation must precede enabling a
  binding, and missing registration fails closed.
- **Retention:** desired bindings retain current state and a bounded operational
  history; immutable publication and decision receipts follow audit/legal-hold
  retention. Sunset disables selection but does not destroy receipts.
- **Writers / queries:** current reader is
  `gents-cli/src/commands/trace.rs`, filtering enabled rows by `agent_did` then
  resolving projection/behavior specificity and rejecting incomparable rows.
  Configuration writers must be inventoried before implementation; no writer
  is currently named by the schema. Index the exact successor scope key and
  active selection, not every descriptive field.
- **Formal / migration:** model selection determinism, fail-closed missing or
  ambiguous policy state, immutable policy publication, and no-authorization-
  widening during rotation. This is a breaking schema redesign: create a
  successor collection and deliberately decline automatic migration of unsafe
  ambiguous rows, or rebuild clean deployments from authoritative policy
  configuration.
- **Decision:** successor/split direction provisional pending DefraDB ACP policy
  API availability and the administrator/publisher principal model.

## Run timeline and adapter projection decisions

These are Rust projections rather than current DefraDB collections, but their
source selection is part of the durability contract.

| Surface | Current source selection | Target identity/order contract |
| --- | --- | --- |
| Root request | logical id, newest timestamp, `limit: 1` | caller-selected `_docID`; exact composite CID; duplicate logical ids fail |
| Related requests | session and parent logical ids | durable edges frozen to exact CIDs |
| Messages | session id, sequence ascending | exact CIDs; uniqueness/order scope `(session document, sequence)` with deterministic conflict handling |
| Tool calls/results/approvals | exact signed result/approval facts are verified against the physical historical tool call, but the complete lifecycle/attempt graph is not frozen in a source manifest | include every lifecycle fact by `_docID`/CID and preserve attempt/order edges |
| Responses/inference | logical request/session ids | exact attempt/materialization versions, stable attempt and progress ordering |
| Compaction | not a timeline source | include checkpoint/source-range facts so omitted transcript is explainable |
| Rendered request | not a timeline source | join exact request version; expose body/field CID subject to ACP/redaction |
| Adapter outputs | projection `v1` over current rows | immutable manifest + projection and redaction algorithm versions |

Retry, fork, and compaction edges must remain relationships in the manifest,
not be reconstructed solely from timestamps. Ordering keys require a declared
scope and tie breaker. Timestamp parsing failure or equal timestamps must never
silently reorder evidence; use durable sequence/attempt keys and `_docID` only
as a final deterministic presentation tie-breaker, not as causal proof.

## Writer and query matrix

| Component | Writes | Reads / projects | Durability concern |
| --- | --- | --- | --- |
| Rendered capture transport/sink | immutable `RenderedRequest` before send | idempotency lookup by `capture_key` | replicated conflict policy, complete-generation evidence, and ACP authorization |
| Request watcher/lifecycle/daemon | claim transition and exact request version | supplies capture context | preserve the exact claim boundary |
| `run_timeline_fetch.rs` | none | logical-id/current-head GraphQL or local reads | ambiguous root and no source CIDs |
| `run_timeline.rs` | none | sorts/materializes runtime rows | deterministic display is not exact provenance |
| `adapter_projection.rs` + ATIF | none | emits four versioned formats with redaction | incomplete source/ACP manifest |
| CLI `trace timeline/project` | none | discovers binding, obtains ACP decisions, filters rows | omission receipts and policy version absent |
| `external_adapter_capture.rs` | imports external shapes into timeline rows | projection-specific mappings | imported claims need origin/schema/signer trust class |

## Lean and conformance obligations

- Extend the rendered-capture model so `Verified` requires a complete manifest,
  exact historical reads, authorized signer evidence, and matching field CIDs.
- Model projection input as an immutable set of source refs; prove replay
  stability and that adding a denied document cannot add output.
- Prove binding selection is deterministic or fails closed under duplicate,
  incomparable, stale, and rotating state.
- Add conformance tests for duplicate logical IDs, concurrent heads, deleted or
  unavailable history, request/capture mismatch, unauthorized normal/CID/
  history reads, partial ACP denial receipts, equal timestamps, retry/fork/
  compaction ordering, and byte-stable replay.
- Test late-peer backfill separately from live gossip, and archive verification
  after the live node is unavailable.

## Implementation issues to split from this audit

1. Define and prove the versioned run-timeline source manifest and exact-CID
   fetcher.
2. Add `RenderedRequest` and compaction facts to timeline reconstruction, retain
   the implemented exact tool-result/approval edges in the frozen manifest, and
   represent ACP-aware omissions explicitly.
3. Version adapter provenance and export receipts with source, signer, policy,
   resource-map, and redaction evidence.
4. Replace `ProjectionAcpBinding` with branchable desired-binding and immutable
   policy-publication successor collections.
5. Add replicated-conflict, late-backfill, unauthorized-history, and archive
   replay conformance suites.
6. Define trust classes and provenance for external adapter imports.

Track C is **provisional** overall. `RenderedRequest` now has signed exact
request/claim, inference-attempt, transcript, and bounded core-config evidence;
tool results/approvals and finalized compactions also have narrower exact-source
foundations. The projection pipeline still does not freeze or consume those
facts as one complete source manifest and is not yet a durable, independently
verifiable export path.
