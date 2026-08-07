# Schema Durability Roadmap

This roadmap turns the four provisional schema audits into small vertical
slices. Each slice ends with a guarantee that is true on its own. No slice may
claim database-enforced ACP, verified authorship, full reconstruction, or
enterprise retention merely because another future slice plans to provide it.

The target schemas are intentionally breaking. Pre-release deployments may
reset at a declared schema epoch. Where retained data matters, export/import
preserves old `_docID`/CID evidence as `unverified_legacy`; it never invents
new provenance or treats a new CID as the old one.

## Active milestone: signed request-ingest provenance

The active milestone is intentionally narrower than the full durability
backlog. It proves that one incoming request was authored by its claimed
requester, that the runtime selected one exact immutable version of that
request, and that lifecycle processing and provider capture stayed bound to
that version.

For this milestone, assume that target collections will receive appropriate
DefraDB policies in a later policy workstream and that sensitive fields and
credentials will receive appropriate encryption in a later encryption
workstream. Policy authoring, relationship installation, confidentiality, key
custody, and secret migration are not acceptance criteria here. This is a scope
boundary, not a claim that the current schemas already enforce ACP or encrypt
those values.

The active dependency chain is:

1. **Signed ingress boundary.** Every request-producing node has a registered
   signing identity, and the accepted commit is verified against the document's
   declared immediate author. The author may be the agent node for agent-created
   requests; `requester_did` can remain the initiating participant and is not
   globally equal to the immediate author (notably for subagent/automation
   requests). If a remote user-authored request is stored by an agent node, its
   user signature must be preserved in an immutable envelope. An ACP/query
   actor DID is neither a commit signature nor a durable read receipt.
2. **Exact accepted version.** Resolve one request `_docID` and composite CID at
   the accepted/claim boundary, reject logical-ID conflicts, and time-travel
   reload that exact version before any request input is consumed.
3. **Fenced processing.** Carry the accepted version through the request
   lifecycle. Mutations target its `_docID` plus expected state and authorized
   executor; a later head, duplicate logical ID, replay, or signer mismatch
   cannot redirect processing.
4. **Exact provider evidence.** Persist the accepted request reference in the
   immutable pre-send `RenderedRequest`, fail closed before send on a missing or
   mismatched reference, and report the request-authorship evidence as verified
   only when the ingress signature is verified. The overall rendered manifest
   remains `CapturedOnly` until its other evidence dimensions are complete.

The existing #1065 request-version slice implements most of steps 2 and 4 but
deliberately remains `CapturedOnly` because step 1 is unresolved in #1064.
The next implementation work is therefore the narrow request-ingress portion of
#1064 plus request-specific conflict/fencing cases from #1076. It does not wait
for the broader request intent/execution redesign in #1071.

### Active milestone acceptance

- unsigned, malformed, unauthorized, claimed-requester-mismatched, replayed,
  and ambiguous duplicate requests fail before prompt assembly or tool use;
- the value processed is loaded from the verified `_docID`/composite CID, not a
  later logical-ID/current-head query;
- every request transition in this path addresses the same `_docID`, checks its
  expected state, and is attributable to the authorized executor model;
- the captured provider body points to that exact request version and a
  different request/CID cannot be substituted;
- Lean states the accepted-ingress and processing invariants, conformance cases
  are generated from them, and Rust/DefraDB integration tests cover bad signer,
  replay, duplicate, later mutation, crash recovery, and restart; and
- documentation says exactly which signature was verified and never infers it
  from `requester_did`, `agent_did`, or `QueryRequest::with_identity`.

Attaching identity to every query and mutation is still required for the later
policy workstream, but it does not identify the versions a query consumed.
Transcript/config provenance additionally requires an immutable manifest of
every consumed `(collection, _docID, composite CID)` and signature status. A
node signature proves authorship of each named commit; the manifest proves that
those exact commits contributed to the provider request.

## Deferred platform assumptions and cross-cutting gates

- DefraDB policies are designed and attached later. Until that workstream
  lands, this milestone makes no ACP-enforcement claim and does not use an ACP
  caller identity as authorship evidence.
- Encryption, key custody, credential migration, archive encryption, and
  cryptographic erasure are later work. This milestone assumes their eventual
  presence and makes no confidentiality claim.
- Commit signer versus claimed principal is tracked by Gents #1064. Its narrow
  request-ingress phase is part of the active milestone; broader signer wiring
  remains follow-up work.
- `RenderedRequest` consumer work is tracked by Gents #1066. The audit tightens
  its proposed inference join from an ordinal/logical value to an exact
  `InferenceCall` document-version edge.
- Retention and evidence downgrade use the
  [shared lattice](schema-retention-lattice.md), not per-track durations.

## Broader durability backlog

The slices below preserve the findings from the full schema audit. They are not
prerequisites for the active signed-ingest milestone unless that milestone
explicitly cites them.

### Former Slice 1: secret and placement fence (deferred)

**Guarantee:** participant pairing cannot gossip raw credentials or silently
broaden placement of provider-request bodies.

- remove secret-bearing backend/OAuth documents from every unfiltered
  participant route;
- move raw keys/tokens to a local encrypted secret provider and retain only
  versioned secret references in shareable config;
- establish a permanent participant-profile denylist for `RenderedRequest`,
  raw credentials, private memory, and other high-sensitivity facts; and
- add catalog tests proving a later template edit cannot bypass the fence.

This is application-layer containment and does not claim ACP.

### Former Slice 2: fail-closed durable identity

**Guarantee:** correctness paths never silently select or update one of several
documents that share a logical ID.

- use exact `_docID` whenever the writer already has it;
- otherwise enumerate the complete logical-ID conflict set and fail closed;
- transition `InferenceCall` by its already-returned `_docID` plus expected
  state; and
- cover timeline root, request/response terminalization, recovery, child
  result, session, tool-call, goal, and memory correctness paths.

No schema split is required to begin this slice, so it is the highest-leverage
runtime hardening and the prerequisite for later exact-version graphs.

## Deferred fact graph foundation

### Slice 3: append-only transcript and full tool output

**Guarantee:** a committed transcript fact is never rewritten and promised
full tool output is either durable or explicitly reported missing.

Start in the transcript/tool Lean models. Add conformance cases for immutable
idempotency, pair closure, multi-writer sequence conflict, fork lineage, and
spill failure. Then replace message/compaction fact upserts with create-and-
compare, declare order scope, and make the full output edge explicit.

### Slice 4: exact provider-attempt edge

**Guarantee:** every provider send is associated with one running
`InferenceCall` version and one immutable `RenderedRequest`, and queue-only
attempts have no render.

Compose the `InferenceCall` and `RenderedCapture` models. Persist request,
attempt, render, backend, and profile `DocumentVersionRef`s; make terminal call
state write-once; and extend #1066's central reader/timeline path. A logical
call id, ordinal, timestamp, or fingerprint is not the relationship.

### Slice 5: request intent and execution authority

**Guarantee:** the requester cannot rewrite execution state and an executor
cannot rewrite request intent; every execution binds one exact intent version.

Split `RequestIntent` from `RequestExecution`, model execution ownership and
interrupt commands in Lean, and move every lifecycle update to `_docID` plus
expected state/owner epoch. This slice does not wait for every Track A
collection to be redesigned.

## Deferred multi-host and reconstruction foundation

### Slice 6: resolved agent generation

**Guarantee:** a provider capture names one immutable, previously published
generation built from exact configuration versions; config ambiguity keeps the
last good generation active and blocks a new one.

Add version-consistent document loading, fail-closed maximal-head selection,
immutable discovery/tool-surface observations, and a
`ResolvedAgentGeneration`. Carry its `DocumentVersionRef` into
`RenderedRequest`. Provenance remains `CapturedOnly` until transcript, signer,
and ACP evidence is complete.

### Slice 7: deployment assignment and lease epoch

**Guarantee:** at one authority epoch, at most one deployment can claim
`(principal, behavior)`, advance its automation cursor, or publish a resolved
generation.

Model assignment/lease fencing first, then add deployment-scoped runtime,
endpoint, health, cursor, and applied-state identities. Test partition,
expiration, takeover, stale-writer rejection, and recovery.

### Slice 8: frozen projection/export manifest

**Guarantee:** a timeline or adapter export is generated from a frozen exact
source manifest or is explicitly partial; it never presents a silent
current-head reconstruction as historical truth.

Start root discovery from `_docID`, time-travel every included source by CID,
include explicit denied/redacted/erased/missing omission receipts, version the
projection and redaction algorithms, and make sensitive output excluded by
default. Integrate the central `RenderedRequest` reader from #1066.

### Slice 9: exact approval facts

**Guarantee:** tool execution cannot proceed on an ambiguous or replayed
approval, and each accepted decision binds the exact held tool execution and
policy version.

Model decision selection, expiry, signer authorization, concurrency/quorum,
and replay in Lean. Persist immutable approval facts on the execution route;
do not rely on first client timestamp.

## Deferred policy, encryption, and enterprise lifecycle

### Slice 10: ACP installation and history enforcement

**Guarantee:** policies and relationships exist before protected writes are
enabled, and anonymous/unrelated identities are denied for mutations, normal
reads, CID reads, `_version`, and `_commits`.

This slice begins only when the embedded DefraDB API can install and verify the
policy graph. It includes positive and negative multi-node conformance and
must not be simulated by caller DID fields or replication filters.

### Slice 11: archive, restore, hold, and purge receipts

**Guarantee:** post-epoch session bundles declare a truthful evidence state,
restore into an empty authorized verifier, and either complete coordinated
purge or expose every unacknowledged target.

Implement the shared retention lattice over exact source dependencies. Test
legal-hold closure, downgrade from plaintext reconstruction to commitment-only,
peer/archive/backup/cache purge, key destruction, and unreachable-target
escalation. Pre-epoch rows remain `unverified_legacy`.

## Broader backlog dependency graph

```text
1 placement/secret fence ───────────────────────────────────────────────┐
2 fail-closed identity ──┬─> 3 append-only transcript ───────────────┐ │
                         ├─> 4 attempt <-> render ────────────────────┤ │
                         └─> 5 request intent/execution ─────────────┤ │
                                                                    │ │
2 + 4 ────────────────> 6 resolved generation ──> 8 export manifest├─┤
5 ────────────────────> 7 deployment lease ────────────────> 9 approvals│
                                                                    │ │
DefraDB #1318 + 1 + identity model ───────────────────────> 10 ACP ─┤ │
3 + 4 + 5 + 6 + 8 + retention policy ────────────────────> 11 archive

Verified provenance requires 3 + 4 + 5 + 6 + 8 + 10 + Gents #1064.
```

This graph describes the broader backlog only. It does not supersede the active
signed-ingest chain above. Each later implementation issue must name its exact
Lean changes (or explain why it is plumbing), conformance cases, schema
epoch/successor decision, and full repository gates.
