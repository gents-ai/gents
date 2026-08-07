# Schema Decision Ledger

This ledger is the working inventory for
[issue #1063](https://github.com/source-inc/gents/issues/1063). It applies the
[DefraDB schema guide](defradb-schema-guide.md) to every collection shipped by
Gents.

The inventory records current state and review hypotheses. It does not endorse
the current schema. A row becomes **decided** only after its detailed entry
answers every question in the template below and links the resulting tests and
migration work.

## Status vocabulary

- **Unreviewed:** only current schema facts and a preliminary archetype are
  recorded.
- **In review:** writers, readers, invariants, and DefraDB capabilities are
  being traced.
- **Provisional:** a direction is recorded, but a dependency, proof, or
  migration decision remains.
- **Decided:** the target contract, tests, and migration path are accepted.
- **Implemented:** the decided contract is enforced in the shipped schema and
  runtime.

## Detailed entry template

Every collection review must record:

```text
Collection:
Primary archetype:
Document meaning:
Canonical or derived:

Authorized creator:
Authorized transition writers:
Claimed principal field:
Required commit signer:

Logical ID and uniqueness scope:
Durable _docID relationships:
Pinned composite/field CIDs:
Concurrent-create/conflict behavior:

Immutable fields:
Mutable lifecycle/configuration fields:
Illegal state combinations:

Live gossip scope/filter:
Late-peer backfill:
Branchable decision:
ACP policy/resource/relationships:
Encryption and key-custody class:

Hot retention:
Archive/export contract:
Sunset/purge behavior:
Legal-hold behavior:

Canonical writers and queries:
Indexes justified by those queries:
Ambiguous limit: 1 or logical-ID reads:

Lean/conformance impact:
Migration/successor/backfill plan:
Open dependencies:
Decision status:
```

## Current collection inventory

`Branchable` reflects the current SDL on the #1059 branch. Archetypes are
starting hypotheses to test, not conclusions.

### Conversation, execution, and projections — Tracks A-C

| Collection | Archetype hypothesis | Branchable | Track | Status |
| --- | --- | ---: | --- | --- |
| `AgentSession` | Lifecycle envelope | Yes | A | Unreviewed |
| `AgentConversation` | Materialized UX projection | Yes | A | Unreviewed |
| `AgentRequest` | Command plus lifecycle envelope | Yes | A/B | In review |
| `AgentResponse` | Streaming materialization plus terminal result | Yes | B | Unreviewed |
| `InferenceCall` | Durable provider-attempt fact/ledger | No | B | Unreviewed |
| `AgentMessage` | Durable transcript fact | Yes | A | Unreviewed |
| `AgentToolCall` | Tool lifecycle envelope | Yes | A | Unreviewed |
| `AgentToolResult` | Durable tool-result fact | Yes | A | Unreviewed |
| `AgentToolApproval` | Durable authorization decision | Yes | A | Unreviewed |
| `CompactionEntry` | Durable transcript-reduction fact | Yes | A | Unreviewed |
| `Goal` | Long-lived lifecycle envelope | Yes | A | Unreviewed |
| `AgentMemory` | Mutable principal knowledge | Yes | A | Unreviewed |
| `RenderedRequest` | Immutable provider-call fact | Yes | C | In review |
| `ProjectionAcpBinding` | Desired projection authorization state | No | C/D | Unreviewed |

### Agent and inference configuration — Track D

| Collection | Archetype hypothesis | Branchable | Track | Status |
| --- | --- | ---: | --- | --- |
| `AgentPrincipal` | Desired identity configuration | No | D | Unreviewed |
| `AgentBehavior` | Desired behavior configuration | No | D | Unreviewed |
| `ToolSelection` | Desired capability configuration | No | D | Unreviewed |
| `Skill` | Desired capability/instruction configuration | No | D | Unreviewed |
| `InferenceBackend` | Desired backend plus observed health state | No | D | Unreviewed |
| `InferenceProfile` | Desired inference configuration | No | D | Unreviewed |
| `OAuthCredential` | Local secret/configuration | No | D | Unreviewed |
| `WorkspaceRoot` | Local host configuration | No | D | Unreviewed |
| `AgentRuntime` | Observed deployment state | Yes | D | Unreviewed |
| `ToolServiceRegistry` | Desired service identity plus observed state | No | D | Unreviewed |
| `ToolServiceHealthState` | Observed service state | No | D | Unreviewed |

### Automation

| Collection | Archetype hypothesis | Branchable | Track | Status |
| --- | --- | ---: | --- | --- |
| `Task` | Desired work configuration | Yes | D | Unreviewed |
| `Schedule` | Desired schedule plus observed firing state | Yes | D | Unreviewed |
| `EventTrigger` | Desired trigger plus observed firing state | Yes | D | Unreviewed |
| `PersonaConfigRequest` | Command/intent plus outcome | Yes | D | Unreviewed |

### Network, pairing, and placement

| Collection | Archetype hypothesis | Branchable | Track | Status |
| --- | --- | ---: | --- | --- |
| `AgentDirectoryEntry` | Replicated directory projection | Yes | D | Unreviewed |
| `AgentNetwork` | Durable network configuration | Yes | D | Unreviewed |
| `NetworkMembership` | Durable authorization/membership fact | Yes | D | Unreviewed |
| `NetworkJoinRequest` | Command/intent plus outcome | Yes | D | Unreviewed |
| `PeerEndpoint` | Replicated endpoint configuration | Yes | D | Unreviewed |
| `PeerRegistry` | Local/desired peer registry | No | D | Unreviewed |
| `PeerPairingDesired` | Desired local pairing state | No | D | Unreviewed |
| `DataPlanePairingDesired` | Desired local pairing state | No | D | Unreviewed |
| `PeerPairingApplied` | Observed local reconciliation state | No | D | Unreviewed |
| `ConsumedInviteNonce` | Durable replay-prevention fact | No | D | Unreviewed |
| `ReciprocalConversationIntent` | Desired cross-peer intent | No | D | Unreviewed |
| `PairingBearerClaim` | Replicated command/claim | Yes | D | Unreviewed |
| `BearerPairingReady` | Replicated acknowledgement fact | Yes | D | Unreviewed |

## First vertical slice: request provenance

### Scope

Build on [PR #1059](https://github.com/source-inc/gents/pull/1059) by
establishing a reusable exact-version reference and applying it to the
`AgentRequest -> RenderedRequest` edge.

```text
DocumentVersionRef {
    doc_id
    composite_commit_cid
}
```

### Current facts

- `RenderedRequest.request_doc_id` identifies the exact `AgentRequest`
  document, while `request_id` remains a logical correlation value.
- `AgentRequest` is lifecycle-mutated after creation. Its `_docID` therefore
  does not identify which version supplied the captured request.
- `RenderedRequest.request_json` is immutable and its field commit CID anchors
  the stored provider payload.
- `@branchable` is not required for the `AgentRequest` document-version CID or
  the `RenderedRequest.request_json` field CID. It remains independently
  relevant to backfill and collection-scoped ACP.
- The signer of either document is not yet guaranteed to match its claimed
  principal; that is tracked by issue #1064.

### Implemented direction

1. `DocumentVersionRef` pairs the stable `_docID` with a composite commit CID.
2. The formal capture fact compares that source version as well as the rendered
   body; equal provider bytes cannot make a different source version
   idempotent.
3. The conditional claim write is the named provider-input boundary. After the
   write, the runtime excludes every composite observed before the mutation,
   then uses CID time-travel reads to locate the earliest new
   `processing`/`claimed` snapshot with the claim's timestamp, deadline,
   behavior, backend, and execution origin. Selecting the earliest new match
   prevents a later content edit that inherits the claim markers from moving
   the boundary forward.
4. The runtime replaces the watcher-loaded value with that reconstructed
   snapshot before prompt assembly. This closes the watcher-read/claim-write
   race even while request input fields remain mutable in the current schema.
5. A document-backed capture fails closed without the reference. The
   `RenderedRequest` row stores `request_doc_id` and `request_commit_cid`, and
   provenance manifest v3 carries the structured pair.
6. The status remains `CapturedOnly`: config, transcript, ACP-read, and signer
   evidence are not all pinned yet.

### Findings from the pinned DefraDB implementation

- `_version` on a normal query or mutation returns all reachable composite
  versions, sorted by height. Treating element zero as “the consumed version”
  would be unsound under concurrent heads.
- DefraDB update events carry the exact written CID, but the event bus is
  live-only and is therefore not a sufficient recovery source.
- `_commits` exposes composite `_C` CIDs and `Collection(cid: [CID])` performs
  an exact historical read. Excluding the pre-mutation history and matching the
  earliest new claim-state snapshot selects the runtime's claim commit without
  relying on head order or assuming unchanged marker fields are unique forever.
- The exact claimed snapshot, before the later `claimed -> processing`
  transition, is the request source boundary.
- ACP behavior for CID/history reads, signer evidence, and complete
  reconstructibility remain follow-up gates before `Verified` is legal.

### Initial acceptance criteria

- Every document-backed rendered request carries an `AgentRequest` `_docID`
  and composite commit CID.
- No capture-time re-query by `request_id` or current configuration state is
  used to manufacture provenance.
- A later request lifecycle transition does not change reconstruction output.
- A CID belonging to another document is rejected.
- Missing or unauthorized history produces an explicit non-verified result.
- Signer verification remains explicit and cannot be inferred from
  `agent_did` or `requester_did` fields.

Status: **Implemented first slice**. Exact request-version provenance is
captured; the manifest intentionally remains `CapturedOnly` until the remaining
config, transcript, ACP, and signer evidence is modeled and implemented.
