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

### Provisional direction

1. Define a native Rust `DocumentVersionRef` with validated DefraDB `_docID`
   and CID values.
2. Extend the request loader so the request value and its composite commit CID
   are acquired from one consistent read.
3. Carry the reference through request ownership and capture scope without a
   later lookup by logical ID.
4. Version the rendered provenance manifest and record the request reference.
5. Reconstruct the request with a `docID` plus `cid` time-travel query.
6. Mutate the live `AgentRequest` after capture and prove reconstruction still
   loads the consumed version.
7. Report missing, inaccessible, unsigned, and mismatched provenance as
   explicit non-verified outcomes.

### Required investigation before implementation

- Establish the pinned DefraDB GraphQL shape that returns document fields and
  the consumed composite `_C` CID from one consistent read.
- Define behavior when a document has concurrent composite heads.
- Decide which `AgentRequest` lifecycle version is the provider-input source:
  claimed, processing, or another formally named boundary.
- Extend the PromptAssembly/projection-fidelity model before reporting the
  reconstructed result as `Verified`.
- Confirm ACP is enforced for normal, CID, `_version`, and `_commits` reads.

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

Status: **Provisional**. The DefraDB read-shape and concurrent-head questions
must be settled before the Lean and conformance changes are authored.
