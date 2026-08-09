# Track B schema audit: responses and inference attempts

This audit covers the durable path from an `AgentRequest` through provider
attempts, streaming response state, transcript materialization, recovery, and
run-timeline projection. It applies the rules in
[`defradb-schema-guide.md`](defradb-schema-guide.md) to `AgentResponse`,
`InferenceCall`, and the response-facing parts of `AgentMessage` and
`RenderedRequest`.

Tracking issues: #1063 and #1070.

The document deliberately separates **current evidence** from **target
decisions**. A target decision is the proposed breaking contract for Track B;
it is not a claim that the current SDL or runtime already enforces it.

The audit baseline was commit `f6d03cb6`; current-state evidence is refreshed
through the working branch for PR #1065. The exact provider-attempt edge,
immutable transcript finalization, and immutable response-outcome checkpoints
have landed. Track B remains open because exact traversal, fleet ownership,
ACP, gossip/late-peer verification, and governed retention are not complete.

Retention classes and evidence downgrades use the
[shared retention and erasure lattice](schema-retention-lattice.md); this track
does not establish independent default durations.

## Executive finding

The current runtime now has a signed exact-version spine for the owned provider
and response path, but the database graph does not yet preserve the full target
contract across every traversal, duplicate, host, policy, or archive:

- `AgentResponse` mixes a replaceable live stream overlay with terminal outcome
  state, but terminal truth now lives in immutable `AgentResponseOutcome` and a
  successful outcome pins the exact signed `AgentMessage` version.
- finalized `AgentMessage` rows are immutable facts; mutable assembly is kept in
  `AgentMessageDraft`.
- normal `InferenceCall` transitions use `_docID`, expected-state fences, exact
  reload, and the node actor identity rather than mutating by logical `call_id`.
- `InferenceCall` and `RenderedRequest` now form a bidirectional exact
  `_docID`/composite-CID/signer edge, and the rendered manifest captures bounded
  exact config versions.
- Recovery and client/timeline reads frequently use logical-ID filters with
  `limit: 1`. A unique index is not a cross-peer conflict policy, so these reads
  can select an arbitrary conflicting document after replication.
- The run timeline does not load response outcomes or rendered requests and
  therefore cannot expose the exact terminal/provider graph or its conflicts.
- None of these collections has ACP. Response, outcome, and transcript facts
  are present in participant replication profiles, while `InferenceCall` is
  local and absent from every profile. Retention and archival behavior remain
  target contracts rather than implemented workflows.

The target is therefore a split model: immutable transcript messages and
terminal response outcomes are canonical facts; live stream state is an
explicitly replaceable observed-state projection; and every provider attempt
is a lifecycle-fenced ledger document linked by `_docID` and composite CID to
the exact request, rendered provider body, and configuration it used.

## Scope and evidence boundary

The primary evidence was read from the current worktree. Important sources are:

- schemas: `crates/gents-schemas/schemas/agent/agent_response.graphql:1`,
  `crates/gents-schemas/schemas/agent/agent_message.graphql:1`,
  `crates/gents-protocol/schemas/inference/inference_call.graphql:1`, and
  `crates/gents-schemas/schemas/agent/rendered_request.graphql:31`;
- response writer and materializer: `crates/gents/src/streaming.rs:155`,
  `crates/gents/src/streaming.rs:406`,
  `crates/gents/src/agent/stream_processor.rs:53`, and
  `crates/gents/src/hook/persistence/message_spawn.rs:13`;
- inference admission and persistence:
  `crates/gents/src/admission/client.rs:155`,
  `crates/gents/src/admission/controller.rs:175`,
  `crates/gents/src/admission/permit.rs:12`, and
  `crates/gents/src/admission/persistence.rs:43`;
- recovery: `crates/gents/src/lifecycle/recovery.rs:196`,
  `crates/gents/src/lifecycle/recovery.rs:349`,
  `crates/gents/src/admission/recovery.rs:36`, and
  `crates/gents/src/startup_recovery.rs:33`;
- reads and projection: `crates/gents-protocol/src/graphql.rs:808`,
  `crates/gents/src/background_tools.rs:2354`,
  `crates/gents/src/run_timeline_fetch.rs:27`, and
  `crates/gents/src/run_timeline.rs:429`;
- formal contracts: `crates/gents/proofs/Proofs/StreamingResponse/State.lean:79`,
  `crates/gents/proofs/Proofs/StreamingResponse/Transition.lean:5`, and
  `crates/gents/proofs/Proofs/InferenceCall/Transition.lean:5`;
- placement: `crates/gents-schemas/src/lib.rs:171`,
  `crates/gents/src/agent/p2p_reconcile/profiles.rs:83`, and
  `crates/gents/src/agent/p2p_reconcile/templates.rs:112`.

This audit does not assert that a claimed `agent_did` is the commit signer.
The landed provenance-critical paths verify commit signer evidence, while the
remaining system-wide signer/delegation boundary is tracked by issue #1064.

## Implemented checkpoints and remaining boundary

Implemented on the current branch:

- immutable finalized `AgentMessage` facts with separate mutable drafts and
  exact idempotency/conflict checks;
- immutable `AgentResponseOutcome` facts that bind an exact signed request
  source/claim and, for completion, an exact signed assistant message;
- recovery and child-result consumption that publish/load the immutable outcome
  and verify the exact final message instead of inferring by sequence;
- lifecycle-fenced `InferenceCall` persistence addressed by `_docID`; and
- bidirectional exact signed `InferenceCall <-> RenderedRequest` linkage, with
  Lean and generated conformance coverage for send/capture ordering.

Still required before Track B completion:

- make every correctness-sensitive entry and traversal exact, especially
  timeline, client turn-state, session roots, and remaining recovery paths;
- expose outcomes, renders, config versions, signer state, and all conflict sets
  in timeline/export output;
- finish the response-live schema/lease cleanup and remove redundant derived
  identities;
- install and test principal/deployment/participant ACP; and
- prove replicated and late-peer convergence and implement governed archive,
  sunset, legal hold, and purge behavior.

## Current fact graph

The graph below is what the code can currently correlate. A dashed edge is a
logical or inferred join, not a durable DefraDB reference.

```text
signed AgentRequest source/claim versions
    +====> AgentResponse live projection
    |          +====> immutable AgentResponseOutcome
    |                       +====> exact signed AgentMessage version
    +====> immutable finalized AgentMessage
    +====> lifecycle-fenced InferenceCall V1
                         +====> immutable RenderedRequest pins V1
                         <====+ InferenceCall V2/V3 pins render version

logical request_id/session_id
    : - -> run timeline, client turn-state, and some session/recovery roots
```

Solid `====>` edges are persisted `_docID`/composite-CID/signer references on
the owned execution path. The dashed projection/root edge remains logical and
is the main reason this graph is not yet provenance-complete. DefraDB provides a
composite CID for every mutation, including non-branchable `InferenceCall`, but
the run timeline still does not traverse or export the complete exact graph.

## Canonical fact model

### Current evidence

| Collection/field set | Current role | Canonical today? | Evidence and consequence |
| --- | --- | --- | --- |
| `AgentResponse.content`, `reasoning` | Mutable in-memory-buffer snapshot and live client overlay | No | Flush replaces the whole tail (`streaming.rs:166-218`); tool-result and retry paths reset it (`streaming.rs:242-295`); normal finalize writes both fields to `""` (`streaming.rs:830-873`). |
| `AgentResponse.status`, error/timestamps | Recoverable live projection and request-terminal bridge | No for terminal truth | Immutable `AgentResponseOutcome` is now the canonical terminal fact. Recovery republishes it and repairs the live/request terminal bridge across persistence cuts. |
| `AgentMessage.content` | Serialized native message used to reconstruct provider history | Yes, after finalization | Mutable assistant assembly uses `AgentMessageDraft`; finalization creates an immutable fact and rejects non-identical logical conflicts. |
| `AgentMessage.role`, `reasoning` | Query/read convenience copies of data encoded in `content` | Derived duplicate | Dedicated reasoning is extracted from the serialized message (`hook/persistence/message_spawn.rs:79-89`). No stored source-version assertion proves that the copies agree. |
| `AgentResponseOutcome` | Immutable terminal response fact | Yes on the owned completion/recovery path | It pins exact signed request source/claim versions and, for completion, the exact signed assistant message; conflicting sibling outcomes fail closed. |
| `InferenceCall` row | Admission/provider-attempt lifecycle ledger | Yes for the owned send path | `_docID`-addressed expected-state transitions preserve an exact render binding through terminal state. The bounded config manifest is captured, but broader candidate/placement completeness is still `CapturedOnly`. |
| `RenderedRequest.request_json` | Exact canonical JSON body captured before HTTP send | Yes | The transport refuses send when capture fails; the immutable fact pins the exact running inference-call version and the call pins the exact render version back. |

### Target decisions

1. **`AgentMessage` is the immutable transcript fact.** A committed message is
   never updated. In-progress assistant assembly moves to a separate
   `AgentResponseLive` (or equivalently named) observed-state collection. If a
   durable pre-tool-effect checkpoint is required, append a typed message
   revision/event; do not overwrite a fact row.
2. **`AgentResponseOutcome` is the immutable terminal response fact.** It records
   one terminal outcome for one exact request document and optionally references
   the exact final/partial assistant message version. It contains no live token
   tail. `complete`, `error`, and `interrupted` are distinct outcomes.
3. **`AgentResponseLive` is a replaceable projection.** It owns streaming text,
   reasoning preview, progress counters, liveness timestamps, and local writer
   lease. It may be compacted or expired after a terminal outcome is durable.
4. **`InferenceCall` remains a canonical provider-attempt lifecycle ledger.** It
   may mutate only along the proven state machine. Its identity and lineage
   fields are immutable; terminal fields are write-once. Every actual HTTP send
   is linked to exactly one immutable `RenderedRequest` fact.
5. **`RenderedRequest` remains the provider-body fact.** Retain the implemented
   exact edge to the `InferenceCall` document/version that authorized the send.
   Do not infer that
   edge from `(request_id, attempt)` or timestamps.

## Cardinality and lifecycle contracts

### Target cardinality

| Edge | Cardinality | Contract |
| --- | --- | --- |
| `AgentRequest -> AgentResponseLive` | `0..1` active projection | At most one canonical live document per request document. A conflict is surfaced and writes stop until ownership is resolved. |
| `AgentRequest -> AgentResponseOutcome` | `0..1` terminal fact | Zero before response terminalization; exactly one after the response-producing execution terminalizes. Rebinding to a different outcome is illegal. |
| `AgentRequest -> AgentMessage` | `0..many` | Every request-produced transcript message carries the exact request `_docID`; legacy/session-only messages are not silently assigned by position. |
| `AgentResponseOutcome -> AgentMessage` | `0..1` final-message version | Required for `complete`; optional for `error`/`interrupted` when no assistant content was durably produced. The reference is `_docID` plus composite CID. |
| `AgentRequest -> InferenceCall` | `0..many` | Compaction, title, inference, scheduled, and one-off attempts are distinct ledger documents. |
| `InferenceCall -> RenderedRequest` | `0..1` | Zero for queue rejection/cancellation before send; exactly one for a provider request that crossed the HTTP send boundary. |
| `RenderedRequest -> InferenceCall` | exactly `1` for admitted calls | One-shot transports without admission require an explicit `call_kind = oneoff` attempt document or a separately declared non-admitted provenance class; empty-string joins are forbidden. |
| `InferenceCall -> backend/config` | exactly `1` version per used source | Persist `_docID` plus composite CID for backend, behavior, and inference profile versions that determined the call. |

### Current `AgentResponse` lifecycle

| State/operation | Current behavior | Durability gap |
| --- | --- | --- |
| begin | Pre-reads by `response_key == request_id`, then creates one `streaming` row with exact signed request source/claim references and empty tails. | Check-then-create is only node-serialized; replicated concurrent live rows can survive. No deployment lease establishes a fleet-wide writer. |
| stream | Full buffered snapshots replace `content`/`reasoning`; progress updates are separate mutations (`streaming.rs:166-218`, `lifecycle/transition.rs:61-113`). | Multiple commits are useful live history but not a declared canonical transcript. `token_count` is whitespace-counted preview telemetry. |
| materialize | Finalization creates an immutable `AgentMessage`, verifies its exact document version/signer, and null-CAS binds that tuple to the live response. | Sequence allocation is still local, and the legacy live row retains both the exact tuple and the older sequence marker. |
| finalize | An immutable outcome is published from the exact request and final-message bindings, then the live response/request bridge is terminalized. | The writes are separate recoverable cuts, not one atomic transaction; correctness depends on re-drive and conflict checks. |
| restart recovery | Recovery verifies exact request/live/message provenance, publishes or reloads the immutable outcome, and repairs the remaining live/request cut. | Fleet-wide single-executor safety and provenance-complete timeline visibility remain absent. |

### Current `InferenceCall` lifecycle

| Transition | Current writer | Current guard | Gap |
| --- | --- | --- | --- |
| absent -> `queued` | admission controller | signed create returns `_docID` and exact version | Logical `call_id` remains redundant correlation, and config completeness remains bounded. |
| absent -> `running` | immediate/one-off permit | signed create returns exact running version | Valid shortcut; provider send is blocked until the render is durably bound. |
| `queued` -> `running` | queued permit acquisition | `_docID` plus expected state, affected-row verification, exact reload | No fleet deployment lease; a local lifecycle fence is not cross-host ownership. |
| running -> render-bound | capture sink/admission client | null-CAS exact render tuple, then exact reload | Captures send authorization, not proof that bytes reached or were processed by the provider. |
| live -> terminal | stream guard / permit drop | `_docID` plus expected state; terminal binding preserves render tuple | Transport receipt/provider-response identity is not yet a separate immutable fact. |
| stale live -> terminal | startup sweep | exact `_docID`, expected state, and conflict reload | Timeline and some parent/session entry reads still begin from logical IDs. |

The target state machines are:

```text
AgentResponseLive: active -> superseded | expired
AgentResponseOutcome: absent -> complete | error | interrupted  (immutable)

InferenceCall: absent -> queued -> running -> completed | failed | cancelled
               absent -> running -> completed | failed | cancelled
               absent -> cancelled/failed  (pre-admission rejection only)
```

No terminal document may transition again. An idempotent observation of the
same terminal fact is a no-op, and a different terminal outcome is an integrity
conflict.

## Identity and version edges

### Current evidence

| Value | Current scope/use | Problem |
| --- | --- | --- |
| `response_key` | Equal to `request_id`; unique-indexed | Redundant application identity. It does not identify the request document and distributed conflicts remain possible. |
| `request_id` | Logical correlation across all four collections | Its schema uniqueness is not consistently declared; many correctness reads use it with `limit: 1`. |
| `materialized_message_sequence` | Legacy display/progress marker retained beside the exact final-message tuple | Sequence remains useful order, but correctness readers can and should use `final_message_doc_id` plus CID/signer instead. |
| `message_key` | Non-unique idempotency/correlation key; commonly `session_id:sequence` | Final facts compare complete content and expose conflicts, but delimiter encoding remains redundant identity and no composite session/sequence index exists. |
| `call_id` | UUID correlation retained on `InferenceCall` | Normal lifecycle writes now use `_docID` and expected state; `call_id` remains redundant schema/application identity. |
| `call_seq` | Monotonic in one shared in-memory admission context | It restarts with a new context/process and is not a global or database invariant. |
| `attempt` | Task-local call policy value | It is not `RenderedRequest.attempt`; completion-loop retries can create later calls while the surrounding scope still reports attempt `1`. |
| `backend_config_fingerprint` | Hash stored by the inference writer | Self-attested query convenience, not a version/proof edge to `InferenceBackend`. |

### Target decisions

- Every response/live/outcome/call/message stores `request_doc_id` as an
  immutable direct edge. Where behavior depends on the consumed request
  snapshot, it also stores `request_commit_cid`.
- `AgentResponseOutcome.final_message` is a `DocumentVersionRef` represented as
  `final_message_doc_id` and `final_message_commit_cid`. Keep message sequence
  only for ordered display.
- `RenderedRequest.inference_call_doc_id` and
  `inference_call_commit_cid` name the running attempt snapshot that authorized
  the send. Queue-only attempts have no render.
- Normal call transitions target `InferenceCall._docID`, carry the expected
  source state, and verify the affected document. Remove `call_id` unless an
  external provider supplies a meaningful call identifier; if retained, rename
  it `external_call_id` and do not use it for database identity.
- Store configuration document/version references. A fingerprint may remain as
  an index, but never substitutes for those references.
- Store component fields for idempotency and ordering. Do not construct
  relationship keys by concatenating or rehashing identifiers. If DefraDB
  cannot enforce the needed composite index, keep an explicitly versioned
  idempotency encoding while retaining every component and a conflict policy.
- Export `_docID`, terminal composite CID, relevant field CIDs, signer evidence,
  and logical correlation fields. The run timeline must retain these references
  rather than `skip_serializing` them.

## Source-of-truth and duplication inventory

| Concept | Current copies | Target authority |
| --- | --- | --- |
| Final assistant body | `AgentMessage.content`; transiently `AgentResponse.content`; projected timeline/client fields | Exact immutable `AgentMessage` version. Response outcome references it; projections never copy it as a competing fact. |
| Reasoning | Encoded in `AgentMessage.content`, copied to `AgentMessage.reasoning`, previewed in `AgentResponse.reasoning` | Message envelope is canonical; any searchable/redacted reasoning projection carries its source message CID and policy decision. Live preview is explicitly lossy. |
| Request identity | `request_id` repeated in response, call, message, and render | `request_doc_id` for relationships; `request_id` only for user/protocol correlation. |
| Response identity | `response_key == request_id`, plus DefraDB `_docID` | `_docID`; uniqueness/conflict policy is over immutable `request_doc_id`. |
| Message identity | `message_key`, `(session_id, sequence)`, `_docID` | `_docID`; component fields express session order and idempotency scope. |
| Provider attempt identity | `InferenceCall._docID` plus retained `call_id`/ordinal correlation | `_docID`, now explicitly and bidirectionally referenced by exact `RenderedRequest` version. |
| Backend/config used | request/call logical fields plus the rendered request's exact bounded config manifest | Exact manifest references are authoritative for captured inputs; completeness remains `CapturedOnly` until resolved candidate/tool/placement inputs are proven complete. |
| Token usage | response whitespace `token_count`; inference provider prompt/completion/cache counts | Provider usage belongs to terminal `InferenceCall`; live token estimates are named approximate projection fields. |
| Terminal status | request lifecycle, response status, call status, message presence | Each fact owns its state; cross-document outcome transaction/recovery enforces the bridge. No reader guesses terminality from content. |

## Writer and query matrix

| Operation | Current addressing | File evidence | Target addressing/behavior |
| --- | --- | --- | --- |
| Begin response | `response_key/request_id` pre-read, then create | `streaming.rs:610-688`; `streaming/queries.rs:98-140` | Create live projection with immutable request version ref and writer lease; on duplicate, load all conflicts and fail closed. |
| Flush/reset live tail | response `_docID` + `status=streaming` | `streaming.rs:166-295` | Keep `_docID` plus lease/monotonic revision compare-and-set. Never mutate transcript facts. |
| Advance response progress | response `_docID` | `lifecycle/transition.rs:61-113` | Same document, expected revision/state, monotonic counters. |
| Materialize assistant message | finalize draft into immutable fact; compare complete conflicts | `hook/persistence/message_spawn.rs`; `session/history.rs` | **Implemented checkpoint.** Exact `_docID`/CID/signer is returned; distributed order allocation remains. |
| Bind materialized message | response `_docID` with null-CAS exact message tuple | `streaming.rs:321-397` | **Implemented checkpoint.** Keep the legacy sequence marker projection-only. |
| Finalize response + request | publish immutable outcome, then exact live/request terminal cuts | `streaming.rs`; `response_outcome.rs` | **Implemented recoverable checkpoint.** Continue re-drive; it is intentionally not represented as an atomic transaction. |
| Recover response | exact request/live/message provenance and outcome conflict enumeration | `lifecycle/recovery.rs`; `response_outcome.rs` | **Implemented for the owned path.** Fleet lease and exact external entry traversal remain. |
| Create/update inference call | signed create; `_docID`/expected-state transition with exact reload | `admission/persistence.rs` | **Implemented checkpoint.** Retire redundant `call_id` as a correctness key and add fleet ownership separately. |
| Recover inference call | stale call `_docID` plus expected state and exact reload | `admission/recovery.rs` | **Implemented call transition fencing.** Timeline/parent entry selection still needs complete exact traversal. |
| Load child final answer | accepted outcome by exact child request `_docID`, then exact message CID/signature verification | `background_tools.rs`; `response_outcome.rs` | **Implemented checkpoint.** ACP policy still must authorize the read. |
| Client turn state | request and response by `request_id limit:1` | `gents-protocol/src/graphql.rs:808-918` | Resolve request conflict explicitly, then traverse its exact outcome/live references. |
| Timeline fetch | request latest by logical ID; session-wide scans; inference calls by request ID | `run_timeline_fetch.rs:82-370` | Start from exact request `_docID` (or return all logical-ID conflicts), traverse direct edges, and include CIDs/signer status. |
| Timeline association | infer message request from response sequence and session position | `run_timeline.rs:478-558`; `run_timeline.rs:678-703` | Use immutable request/message/outcome refs only. Unknown legacy lineage remains unknown. |
| Goal activity/usage failure | existence/latest by `request_id`, `attempt DESC` | `trigger_engine/goal_source.rs:601-667` | Query exact request edge; order calls by declared per-request ordinal plus `_docID`, not ambiguous `attempt`. |

## Branchability, gossip, and late-peer backfill

### Current evidence

- `AgentResponse`, `AgentResponseOutcome`, and `AgentMessage` are `@branchable`
  and are included in broad runtime/chat and requester-filtered
  conversation/machine/subagent-host routes. The immutable outcome is therefore
  available for authorized late-peer backfill; the mutable live response still
  shares that broad route.
- Their live replication filter is immutable `requester_did`. Finalized message
  and outcome scope fields are immutable; the live response's payload/lifecycle
  fields intentionally remain mutable.
- `InferenceCall` is not branchable and appears in no P2P collection profile or
  pairing template. It is local operational telemetry and startup-recovery
  state.
- `RenderedRequest` is branchable but deliberately excluded from desktop bulk
  sync. It also appears in no participant pairing template.

### Target decisions

| Collection role | Live gossip | Late-peer backfill | Branchable |
| --- | --- | --- | --- |
| immutable `AgentMessage` | Owner to requester/session participants, filtered by immutable participant/tenant scope | Required for an authorized participant joining or reconnecting | **Yes** |
| immutable `AgentResponseOutcome` | Same participant scope as its request/message; terminal facts receive bounded convergence re-drive | Required | **Yes** |
| `AgentResponseLive` | Best-effort owner-to-active-requester only; never fleet-wide | Not required; a late peer reads outcome/messages | **No**, unless collection-scoped ACP is the only deployable enforcement mechanism |
| `InferenceCall` | None by default; optional filtered operator-observability channel, never participant gossip | Not required for execution recovery; governed archive supplies enterprise history | **No** in this schema. A future multi-host audit/handoff requirement gets a new branchable successor rather than assuming replication. |
| `RenderedRequest` | No participant gossip; optionally an encrypted/governed audit channel | Required only for authorized audit nodes, not chat clients | **Keep Yes** because the shipped choice is irreversible and governed audit backfill is a stated use; add an explicit audit-sync profile before relying on it |

Terminal response recovery now republishes/reloads immutable outcome and message
facts and repairs the request/live projection across bounded persistence cuts.
The remaining target is replicated/late-peer evidence that the same conflict
and re-drive rules converge across hosts. Live overlays must never be used as
the only recovery source.

## ACP, identity, and confidentiality

### Current evidence

None of the schemas in this audit has `@policy`. Provenance-critical request,
message, outcome, inference-call, and rendered-request operations now attach the
node actor identity and verify the resulting commit signer. That proves who
signed the accepted versions on those paths; it does not prove delegation or
authorize the operation without a policy. Replication filters decide placement,
not reads after blocks arrive. Reasoning and provider payloads therefore still
have no data-layer least-privilege boundary.

### Target decisions

- **Principal and deployment:** the agent principal DID is the owner/audit
  subject. Only a registered deployment authorized for that principal may
  create live state, calls, messages, renders, or outcomes. Commit signer
  verification must prove that delegation; a stored `agent_did` is insufficient.
- **Participant reads:** requester/session-participant identities may read their
  request outcome and redacted transcript. They do not automatically receive
  `InferenceCall`, provider endpoint, full rendered prompts, or reasoning.
- **Operator/auditor reads:** tenant operators and explicitly delegated auditors
  may read call telemetry and governed render captures. Access is relationship
  based and tenant-scoped, not a broad collection fallback.
- **Write boundaries:** requester may create the request command only. The
  authorized agent deployment creates/updates live response and calls and
  appends messages/outcomes. No participant may rewrite a terminal fact.
- **History target:** normal reads, CID reads, `_version`, and `_commits` should
  enforce the same or stricter relationships. The pinned paths are not uniform:
  mutation-result `_version` enrichment can bypass an equivalent read check, so
  this remains blocked on separate tests/upstream repair rather than assumed.
- **Registration:** policy resources and document relationships are installed
  before writes are enabled; an unregistered document fails closed. Tests must
  cover installation failure and the pinned DefraDB fallback behavior.
- **Confidentiality:** reasoning and rendered payloads use a stricter redaction
  class than ordinary assistant text. ACP is not encryption; local datastore,
  replicated-delta, archive, key-custody, and key-rotation controls remain
  separate deliverables.

## Retention, archive, and sunset

These are target policy classes; exact durations are deployment policy, not
hard-coded schema defaults.

| Data | Hot retention | Archive/export | Sunset/purge |
| --- | --- | --- | --- |
| `AgentResponseLive` | Active request plus short diagnostic grace | Normally none; sampled operational metrics only | Expire after outcome durability is verified. Tombstone is not evidence of physical erasure. |
| `AgentResponseOutcome` | Session lifetime plus tenant audit window | Required audit record with request/message version refs and signer/ACP evidence | Logical deletion is replicated; legal hold blocks purge; physical purge coordinates peers/backups. |
| `AgentMessage` | Conversation retention class | Versioned transcript export with redaction decision and all provenance refs | Participant-visible sunset plus governed purge; cryptographic erasure by key destruction where configured. |
| `InferenceCall` | Short operational/incident window | Aggregated metrics by default; full attempt records only for authorized audit/debug retention | Host-local purge after export and hold checks. Failure text is classified because it can contain provider or prompt data. |
| `RenderedRequest` | Shortest practical hot window | Encrypted governed archive only when audit/replay policy requires it | Separate high-sensitivity purge and key-destruction schedule; never inherit transcript retention implicitly. |

Every enterprise export includes schema version, `_docID`, composite and
relevant field CIDs, signer/signature status, request/message/render references,
logical IDs, ACP/redaction decision, and export contract version. A projection
that lacks signer or authorized CID-read evidence labels the record
`CapturedOnly`/unverified rather than upgrading it by inference.

## Illegal states and conflict behavior

### Response/message states that must be rejected

- more than one canonical live response or terminal outcome for one request
  document;
- `complete` without a final assistant `DocumentVersionRef`;
- `complete` with an error reason, or `error`/`interrupted` without a typed reason;
- terminal outcome without `terminalized_at`, or a live projection claiming a
  terminal fact;
- a final-message reference whose document is not an assistant message, belongs
  to another session/request/principal, or does not resolve at the named CID;
- a message whose typed query fields disagree with its canonical envelope;
- a committed message that is later updated;
- a progress or writer-lease revision that decreases;
- a participant-scope field that changes after creation;
- a logical-ID duplicate silently selected by `limit: 1`;
- recovered partial live text represented as if it were a normal completed
  assistant message.

### Inference states that must be rejected

- queued with `started_at`/`ended_at`; running without `started_at`; terminal
  without `ended_at`;
- completed with a failure reason; cancelled/failed without a typed reason;
- any terminal-to-live or terminal-to-different-terminal transition;
- immutable request/backend/behavior/config/call-kind/ordinal fields changing;
- a sent/completed provider attempt without a `RenderedRequest` reference, or a
  render referencing a different attempt/request;
- provider usage claimed on a row whose terminal provider response cannot be
  identified; partial usage must be explicitly typed rather than overloaded;
- two attempts with the same declared request/capture-scope/turn/attempt
  identity silently coexisting;
- recovery binding an attempt to whichever parent request happens to win a
  logical-ID query.

On concurrent create, collect all conflicting documents, verify signatures and
lineage, and expose the conflict. A deterministic `_docID` ordering may choose a
temporary projection winner, but it must not erase or authenticate the loser.

## Run timeline target

The current timeline is a useful UI projection, not an audit reconstruction. It
starts from latest `request_id`, scans session-wide rows, infers message lineage
from materialized sequence, sorts call rows using logical ordinals/timestamps,
and omits `_docID` from serialized output (`run_timeline.rs:45-177`,
`run_timeline.rs:592-703`). It does not load `RenderedRequest` at all.

The target timeline traverses a versioned fact graph:

```text
request DocumentVersionRef
  -> zero/one response outcome DocumentVersionRef
       -> zero/one final message DocumentVersionRef
  -> every request-scoped message DocumentVersionRef
  -> every inference-call DocumentVersionRef
       -> zero/one rendered-request DocumentVersionRef
       -> backend/behavior/profile DocumentVersionRefs
```

The CLI accepts `_docID` (preferred) or a logical `request_id`. A logical lookup
that finds more than one document returns a conflict set and requires selection;
it never chooses latest silently. Timeline events include version references,
signer verification state, redaction status, and recovery provenance. Legacy
rows with only inferred joins remain visibly `unverified_legacy`.

## Lean and conformance implications

This is lifecycle and provider-input work, so each implementation checkpoint
continues to begin in the formal layer. The current branch has already landed
the `ResponseOutcome`, `InferenceRenderedCapture`, and transcript-finalization
models plus generated conformance consumers. They cover immutable terminal
outcomes, exact final-message binding, bidirectional render/call capture, and
the implemented persistence cuts with zero known `sorry`s.

1. Keep the landed immutable outcome/message model aligned with the remaining
   `StreamingResponse` live-projection redesign. Continue to prove:
   - one request document cannot acquire two accepted outcomes;
   - completion requires an exact, matching assistant message version;
   - terminal outcome is immutable;
   - recovery never upgrades partial live text to completed output; and
   - outcome + exact request terminalization converge after every persistence
     cut point.
2. Replace `materializedMessageSequence : Option Sequence` as the proof-level
   completion edge with a message document/version reference. Retain sequence
   as the canonical transcript ordering property, allocated by the
   lease-fenced single writer defined in Track A.
3. Retain the landed exact render linkage and `_docID`-fenced `InferenceCall`
   transitions; extend the model with the remaining immutable config identity,
   timestamps, and typed terminal reasons. Prove terminal
   irreversibility against the executable persistence transitions, including
   queued-to-running and drop/recovery races.
4. Preserve the implemented `InferenceCall`/`RenderedCapture` composition: send
   implies both a durable running attempt and a matching durable render; queue
   rejection implies no render/send; capture failure blocks send and
   terminalizes the attempt. Extend it only when transport receipt/provider
   response facts are introduced.
5. Add concurrency models for duplicate response/outcome/attempt creates and
   deterministic conflict projection without treating the winner as authentic.
6. Drive conformance cases for every crash cut point: message persisted before
   outcome, outcome before request terminal, dropped inference permit before
   terminal write, capture persisted before network failure, and startup sweep
   ordering.
7. Add DefraDB integration tests for replicated conflicting creates, late-peer
   backfill, filtered gossip, unauthorized normal/CID/history reads, exact CID
   timeline reconstruction, and archive/redaction export.

When policies land, model response finalization as the same sequence of
recoverable persistence cuts. The pinned policy-backed mutation path can split
an otherwise implicit multi-mutation transaction, so ACP installation must not
silently strengthen the formal model's atomicity assumption.

The landed response-outcome model exposes message, outcome, live-response, and
request terminalization as recoverable persistence cuts rather than assuming
atomicity. The landed inference/render model validates the exact transition and
capture contract. Remaining proof work is the broader live-writer lease,
resolved-config completeness, replicated conflict convergence, exact timeline
reconstruction, ACP behavior, and governed retention—not a return to the old
logical-ID/upsert model.

## Breaking-schema implications

Track B is an intentional breaking schema generation, not an in-place
compatibility migration. The current branch has started that generation with
immutable messages/outcomes and exact call/render fields; the remaining work
still must:

- finish the explicit `AgentResponseLive` role while retaining the implemented
  immutable `AgentResponseOutcome` role;
- finish making call identity/lineage immutable while retaining immutable
  finalized-message fields;
- replace response/message/call logical joins with `_docID`/CID fields;
- replace string timestamps with `DateTime` and empty-string absence with null;
- remove or rename ambiguous fields (`response_key`, call `attempt`, response
  `token_count`) instead of preserving misleading semantics;
- add exact render and config version references; and
- install ACP resources/relationships as part of bootstrap before enabling the
  new collections.

Existing stores require an explicit export/re-import or successor-collection
tool. The importer may preserve old rows as `unverified_legacy`, but must not
invent CIDs, signer evidence, request edges, or final-message identity. Because
`@branchable` cannot be enabled later, successor collection branchability must
match the decisions above at creation time.

## Prioritized child-issue candidates

### Implemented checkpoint: #1075 exact provider-attempt edge

The first Track B implementation checkpoint uses a bidirectional exact-version
contract instead of the logical/ordinal joins described in the original
inventory:

```text
signed running InferenceCall V1
    -> immutable RenderedRequest pins V1
    -> conditional InferenceCall V2 pins that RenderedRequest
    -> one HTTP send attempt
    -> terminal InferenceCall V3 preserves the binding
```

Each edge carries `_docID`, composite CID, and the verified commit signer DID.
All reads and writes on this path attach the node actor identity; signature
verification, rather than the query actor alone, supplies authorship evidence.
The binding write null-CASes all render fields and a zero-row result is accepted
only when exact reload observes the identical V2, so a concurrent different
render cannot be overwritten or mistaken for idempotency. One-off calls create
the same running call fact explicitly rather than bypassing provenance.

This checkpoint proves durable render and send authorization. It does not prove
that bytes reached the network, that the provider received or processed them,
or that a response belongs to the attempt; those require later transport and
response facts.

### Implemented checkpoint: immutable response outcome

`AgentResponseOutcome` now records immutable terminal truth for one exact signed
request execution. A complete outcome binds the exact signed assistant message;
error/interrupted outcomes carry typed reasons and optional partial-message
evidence. Creation enumerates signed sibling conflicts, and recovery re-drives
the message/outcome/live/request persistence cuts. `AgentResponse` remains the
recoverable live projection during this schema generation. A separately named
live collection, writer lease, provenance-complete subscriptions/timeline, and
replicated conflict convergence remain broader Track B work.

1. **P0 — Finish the response fact split and exact materialization edge.** The
   immutable outcome/message model, crash-cut recovery, and exact materialized
   message binding are implemented. Finish the explicit live role, writer
   lease, and all external correctness readers.
2. **P0 — Complete provider-attempt config/transport provenance.** Exact
   bidirectional `InferenceCall`/`RenderedCapture` linkage is implemented.
   Finish resolved-config completeness and add transport receipt/provider
   response facts if those claims are required.
3. **P0 — Finish replacing logical-ID mutation and recovery addressing.**
   Inference transitions, response materialization/recovery, and child final
   response loading now use exact references. Timeline, client turn-state,
   session roots, and remaining recovery/goal consumers still need conversion.
4. **P1 — Finish the breaking Track B schemas.** Immutable
   `AgentResponseOutcome` and finalized messages exist; finish the replaceable
   live collection/lease, typed timestamps/reasons, redundant-key removal, and
   remaining exact version-reference fields. No compatibility migration is
   required; provide a legacy importer only if an existing deployment needs it.
5. **P1 — Enforce principal/deployment ACP at the data layer.** Install policies
   and relationships before writes, attach caller identity to every mutation,
   and test unrelated/anonymous normal, CID, version, and commit-history reads.
6. **P1 — Make timeline/export provenance-complete.** Traverse exact document
   edges, include render/call/config versions and signer state, surface logical
   duplicates, and version redaction/export contracts.
7. **P1 — Prove participant gossip and outcome convergence.** Test requester
   filters, immutable placement fields, live-overlay exclusion from backfill,
   terminal fact replay, and a late authorized peer.
8. **P2 — Implement retention and governed archival.** Separate live,
   transcript, inference telemetry, and rendered-payload retention; add legal
   hold, coordinated purge, and cryptographic-erasure evidence.
9. **P2 — Remove derived-key and source duplication.** Replace delimiter message
   keys and `response_key`, demote fingerprints/hashes to query indexes, and
   validate any retained typed projections against their source CID.

## Track B completion gate

Track B is complete only when the P0/P1 work above is implemented and the
following evidence exists:

- Lean proofs and generated conformance tests cover every target lifecycle and
  persistence cut point with zero `sorry`s;
- every correctness mutation and traversal uses exact document identity and,
  where version-sensitive, a composite CID;
- a sent provider request has one matching inference attempt and render fact;
- a completed response has one matching immutable assistant message version;
- duplicate and replicated conflicts fail closed and are visible in timeline
  output;
- participant/operator ACP tests cover normal, CID, `_version`, and `_commits`
  reads;
- gossip, late-peer backfill, archive, sunset, and legal-hold tests match the
  collection decisions; and
- `cargo test -p gents` plus `cargo check --workspace --all-targets` pass on the
  breaking schema generation.
