# Track A schema audit: conversation and session durability

Status: **provisional architecture decision** for issues #1063 and #1068. This document is
an evidence-backed audit, not an assertion that the current schemas already
enforce the recommended contract.

## Scope and method

This audit covers `AgentSession`, `AgentConversation`, `AgentRequest`,
`AgentMessage`, `AgentToolCall`, `AgentToolResult`, `AgentToolApproval`,
`CompactionEntry`, `Goal`, and `AgentMemory`. It applies the vocabulary and
decision template in [DefraDB Schema Design for Gents](defradb-schema-guide.md)
and [Schema Decision Ledger](schema-decision-ledger.md).

Evidence came from the current SDL, canonical Rust writers, lifecycle and
recovery queries, Lean models, replication templates, projection/export code,
and the pinned DefraDB implementation. Recommendations are labelled as such.
No recommendation below is implemented merely by recording it here.

The current-state evidence in this revision is refreshed through commit
`f6d03cb6`. Several durability checkpoints described by the original audit have
since landed; those checkpoints are called out explicitly below. They do not
make the proposed end-state contract, ACP, fleet ownership, or retention work
complete.

Retention terms and evidence downgrades in this track are governed by the
[shared retention and erasure lattice](schema-retention-lattice.md). Any
example duration below is illustrative deployment policy, not a schema default.

The intended end state is a database-enforced conversation plane that can move
between authorized hosts without changing identity, authority, ordering, or
provenance. A host is an executor, not the owner of the truth.

## Executive decision

All ten collections need a breaking schema pass. Retain `@branchable` for each
one, but for concrete reasons: authorized peers must be able to initiate
collection catch-up, and these collections will require collection-scoped ACP.
A push replicator can also backfill non-branchable documents, so branchability
is not being used as a synonym for all backfill, document history, or live
replication.

The current collection names obscure four different roles that must be
separated:

1. Immutable commands and facts: request intent, messages, tool invocation,
   tool output, approval decisions, and compaction records.
2. Single-writer lifecycle records: request execution, tool execution, session
   lifecycle, and goal-controller state.
3. Replaceable projections: conversation list/title/preview and mutable heads
   over append-only memory revisions.
4. Placement and authorization fields: immutable principal, participant, and
   session document references used by replication and ACP.

The highest-risk remaining defects are:

- `AgentRequest` combines requester-authored intent and agent-authored mutable
  execution state, which prevents least-privilege document ACP.
- session, request, tool, and transcript joins commonly use logical strings;
  several correctness reads still use logical-ID `limit: 1`.
- finalized `AgentMessage` rows and `CompactionEntry` rows are now immutable
  create-only facts, but transcript order is still allocated by a local
  read-max protocol and the schema does not enforce unique
  `(session, sequence)` ordering;
- `AgentToolResult` is now an immutable, exact-call-version fact, but remains a
  best-effort overflow spill rather than the canonical complete output ledger;
- approvals are now immutable exact-call-version facts with signer checks and
  conflict enumeration, but ACP-backed approver authority and a declared
  concurrent/quorum policy remain absent;
- copied messages, tool facts, approvals, and compactions now carry source
  `_docID`/CID/signer provenance, but there is no first-class `SessionFork`
  prefix manifest or lease-fenced session owner;
- the standard routes now include approvals, tool results, and compactions,
  while `Goal` and `AgentMemory` still lack the target routes and none of the ten
  base schemas has a DefraDB `@policy`;
- the participant-facing conversation/machine templates also carry several
  configuration collections without filters, including secret-bearing
  `InferenceBackend`; this is a current placement fact even though policy and
  encryption remediation are deferred from the provenance milestone.

## Current entity and provenance graph

Solid arrows are current logical-string joins. The dotted arrows are fields
that exist but do not provide a complete durable edge.

```text
AgentSession(session_id)
  ├── AgentConversation(session_id, latest_request_id) ....> AgentRequest
  ├── AgentRequest(session_id, request_id)
  │     ├── retry_parent_request / retry_root_request .....> AgentRequest
  │     ├── caused_by_parent_request_id ...................> AgentRequest
  │     └── caused_by_parent_tool_call_id .................> AgentToolCall
  ├── AgentMessage(session_id, request_id)
  ├── AgentToolCall(session_id, request_id, tool_call_id)
  │     ├── child_request_id ..............................> AgentRequest
  │     ├── exact result _docID/CID/signer ...............> AgentToolResult
  │     └── exact approval _docID/CID/signer .............> AgentToolApproval
  ├── AgentToolResult(exact AgentToolCall _docID/CID/signer)
  ├── CompactionEntry(exact versioned source manifest)
  └── Goal(session_id, last_*_request_id) .................> AgentRequest

AgentToolApproval(exact AgentToolCall _docID/CID/signer) ..> AgentToolCall
AgentMemory(agent_did, key)  [cross-session; no session edge]
```

The tool-result and approval edges now carry exact `_docID`, composite CID, and
verified signer DID, and compaction records carry a versioned exact-source
manifest. Finalized messages also carry the exact request `_docID`. Session,
conversation, goal, memory, request lineage, and several tool/subagent edges
still use logical IDs. The implemented `AgentRequest -> RenderedRequest` edge is
the broader useful counterexample: it carries `_docID` plus a composite commit
CID.

## Implemented checkpoints and remaining boundary

Implemented on the current branch:

- finalized transcript rows are immutable facts; `AgentMessageDraft` contains
  mutable assembly, and idempotent replay compares the complete finalized fact;
- `AgentToolResult` and `AgentToolApproval` pin the exact tool-call document
  version and signer, enumerate logical conflicts, and fail closed on mismatch;
- `CompactionEntry` is create-only and pins a versioned canonical source
  manifest, with Lean and generated conformance coverage;
- forked messages, tool calls/results/approvals, and compactions preserve exact
  source document-version and signer provenance; and
- the requester-scoped replication profiles include the immutable tool facts,
  approvals, compactions, and response outcomes.

Still required before Track A completion:

- split request intent from execution and tool invocation from execution;
- establish `AgentSession._docID` plus a deployment lease as the ownership and
  transcript-allocation spine;
- replace per-row fork copying as the authority with a frozen `SessionFork`
  prefix manifest;
- split goals, version memory, and source-version the conversation projection;
- install and test base-collection ACP, complete Goal/Memory placement, and
  make timeline/export traversal exact; and
- implement archive/restore, retention, legal hold, sunset, and purge receipts.

### Recommended graph

```text
Session (_docID, immutable principal/participant scope)
  ├── RequestIntent (immutable requester command)
  │      └── RequestExecution (agent-owned lifecycle; intent _docID + CID)
  ├── MessageFact (immutable; request-intent _docID when request-scoped)
  ├── ToolInvocation (immutable; request/message _docID + CID)
  │      ├── ToolExecution (agent-owned lifecycle)
  │      ├── ToolOutputFact (immutable, complete or chunked)
  │      └── ToolApprovalFact (immutable operator decision + expected call CID)
  ├── CompactionFact (immutable source-version manifest and boundary)
  ├── GoalIntent + GoalExecution (operator intent / agent controller state)
  └── ConversationProjection (replaceable head, source-versioned)

AgentMemoryHead(agent _docID, key)
  └── AgentMemoryRevision (immutable previous-head reference)
```

Every child carries the parent `_docID`. Any edge that claims what bytes or
state were consumed also carries the parent composite CID. Logical IDs remain
at API boundaries and in exports for correlation; they do not choose a row.

## Evidence: placement, ACP, and export today

- The `conversation` and `machine` scope templates push `AgentRequest`,
  `AgentMessage`, `AgentToolCall`, `AgentToolResult`, `AgentToolApproval`,
  `AgentSession`, `AgentConversation`, and `CompactionEntry` using immutable
  `requester_did` filters
  (`crates/gents/src/agent/p2p_reconcile/templates.rs:112-293`). Those
  templates also include `AgentResponse`, `AgentResponseOutcome`, pairing
  readiness, and six
  configuration collections without a filter rule; this audit must not describe
  the complete participant route as transcript-only.
- The `subagent-host` template returns requests, responses/outcomes, messages,
  tool calls, and approvals; it deliberately omits session/conversation,
  result spills, and compactions (`templates.rs:350-401`).
- The legacy `runtime` and `chat-requests` profiles include tool results,
  approvals, and compactions (`profiles.rs:83-136`). `Goal` and `AgentMemory`
  occur in none of these runtime profiles or requester-filtered templates. They
  remain in the desktop branchable bulk-sync list, however, so paired desktops
  can backfill them without the target private-memory/goal placement policy.
- All ten SDL files use `@branchable`; none uses `@policy`. Consequently the
  claimed `agent_did`, `requester_did`, and `approver_did` values are routing
  claims, not database-enforced authorship.
- Projection ACP in `gents trace` performs a post-query decision over already
  fetched rows (`crates/gents-cli/src/commands/trace.rs:440-559`). It is not
  base-collection read/write ACP and does not cover tool-output spills,
  approvals, compactions, goals, or memory.
- `load_run_timeline_rows` now resolves an exact signed request root (or rejects
  logical twins), includes compaction entries, and attaches exact
  `AgentToolResult` and `AgentToolApproval` versions to tool calls. Every
  returned row is exact-reloaded and signature-verified. The manifest remains
  `PartialExact` because child-request, request/session-scoped transcript,
  response, inference, rendered-request, and compaction extents are discovered
  by non-atomic logical/session scans; those open domains are recorded as
  explicit coverage gaps (`crates/gents/src/run_timeline_fetch.rs:1918-2006`).
  Goals and memory remain outside the run-timeline projection.
- No operational retention, legal-hold, coordinated purge, or full-fidelity
  enterprise archive implementation was found for these collections.
- In pinned DefraDB, `sync_branchable_collection` rejects a non-branchable
  collection (`../../sourcenetwork/defradb.rs/crates/p2p-adapter/src/libp2p_doc_pusher.rs:283-289`),
  and collection-scoped ACP is selected only for branchable collections
  (`../../sourcenetwork/defradb.rs/crates/acp/src/read_access.rs:91-109`).

### Recommended placement and access model

Use two explicit, independently authorized replication routes instead of one
broad conversation profile:

- **Execution/failover route:** filter every owned execution row on immutable
  `agent_did`; deliver to deployments authorized to execute that principal.
- **Participant route:** filter shareable session facts on immutable
  `requester_did`; deliver only the redaction class permitted to that
  participant. Never use this route for private memory.

`requester_did` must be non-null for participant-routed data. Locally scoped
data needs an explicit placement enum, not null/empty-string overloading.
`AgentMemory` gossips only among the principal's authorized execution hosts.
Approvals flow from authorized operators to the owning execution hosts. Goals
flow to the principal's failover set and only to participants granted goal
visibility.

At the ACP layer, install policies and relationships before enabling the schema
and fail closed if registration is absent. The minimum roles are principal
owner, delegated executor, session participant, approval operator, auditor,
archive exporter, and retention administrator. CID reads, `_version`, and
`_commits` must be included in negative and positive ACP tests. Issue #1064's
commit-signer verification is a prerequisite for calling any author field
verified.

## Uniqueness and ordering invariants

### Evidence

| Invariant | Current mechanism | Durability gap |
| --- | --- | --- |
| One session | globally unique `session_id`; upsert by it | concurrent peers can create twins; reads use `limit: 1` |
| One conversation per session | unique `session_id` | known legacy/replicated duplicates require ranking and sweeping |
| One request | `request_id` is non-unique; `retry_key` is unique only for retry successors | many request reads choose newest logical match |
| One transcript position | unique `message_key`, normally `session_id:sequence` | stable event keys may differ; no `(session, sequence)` unique index |
| Next transcript position | max message/tool reservation + 1, retry five times | a local retry protocol, not a replicated allocator |
| One tool call | unique `tool_call_key = session_id:tool_call_id` | components are mutable and logical; several reads use raw `tool_call_id` |
| One approval verdict | immutable `approval_key` plus exact held call version | logical/replicated twins remain legal; authority still depends on future ACP/quorum policy |
| One compaction step | non-unique immutable `compaction_key` plus exact source manifest | sequence remains locally allocated; conflicts are surfaced rather than consensus-resolved |
| One goal | length-prefixed `(agent_did, session_id)` logical `goal_id`, not unique | twins are expected; only one canonical twin is advanced |
| One memory key | injective length-prefixed `(agent_did, key)` `memory_id` | components mutable; concurrent LWW winner is not a declared policy |

The transcript Lean model requires strictly increasing, unique message
sequences, unique tool-call IDs, unique result keys, and pair closure
(`proofs/Proofs/Transcript/State.lean:113-181`). The database schema does not
enforce the first three at their actual component scope.

### Recommendations

- Add real composite unique indexes where supported: session document plus
  sequence, session document plus tool-call ID, tool-call document plus approval
  round, and principal document plus memory key. Pinned DefraDB's uniqueness
  implementation is field-count generic, but null components bypass uniqueness
  and upstream lacks direct composite-unique coverage. Make every component
  non-null and add a DefraDB integration test for each critical index.
- Treat such indexes as local conflict detection, not distributed consensus.
  On convergence, enumerate every conflicting document and apply a declared
  canonical rule; never scan with `limit: 1`.
- Allocate transcript positions through one lease-fenced session owner and a
  compare-and-set append reservation. Retain the existing Lean-visible strictly
  increasing `sequence` as the canonical order; offline multi-writer append
  during a partition fails closed rather than introducing a competing tuple
  order. The lease epoch and reservation crash cut must be modeled explicitly.
- Remove opaque concatenated keys when a composite index can carry the
  components. Retain stable idempotency keys only for actual retry semantics.
  `retry_key` can become components `(requester, parent_request_doc_id,
  retry_ordinal)`; the SHA-256 value adds no integrity.
- Every conflict resolver must return the complete conflict set plus the chosen
  `_docID`, and archive both the conflict and repair decision.

## Canonical writer and query matrix

Tests and fixtures also write these collections directly. This table names the
production ownership seams and hot or correctness-sensitive reads.

| Collection | Canonical writers | Hot/correctness-sensitive reads | Ambiguity observed |
| --- | --- | --- | --- |
| `AgentSession` | session ensure/close (`session/sessions.rs`); fork (`session/fork.rs`); desktop transactional submit (`gents-desktop-core/.../chat/conversation.rs`) | completion binding (`session/query.rs`); desktop session snapshot; run timeline | core/session CLI reads enumerate the complete logical set and fail with typed `_docID` conflicts; the target lease/ownership model is still open |
| `AgentConversation` | request projection/title/status (`session/conversation.rs:76-359`); recovery sweep (`lifecycle/recovery.rs:555-780`); desktop submit; fork | ranked canonical loader (`session/query.rs:133-207`); recent-title query; run timeline | duplicates are real; deterministic rank exists, but a later update can change the winner |
| `AgentRequest` | lifecycle materialize/claim/transition/recovery/queue; desktop submit/retry; trigger and subagent materializers | watcher pending scan and CID reload; lifecycle `_docID` reads; timeline/session/client queries | `request_id` is not unique; timeline and multiple APIs order/limit by logical ID |
| `AgentMessage` | owned-loop hook through `session/history.rs`; fork copy; desktop projection fixtures | provider history; compaction; timeline/session projections | finalized facts are immutable and conflicts are enumerated, but max+1 allocation is not fleet-fenced |
| `AgentToolCall` | `ToolCallLifecycle` native/bridge/mode transitions (`tool_call_lifecycle/transition/*.rs`); background recovery; fork | lifecycle load/result; held-call polling; transcript/timeline/background projections | some reads correctly use `tool_call_key`, others use `(session_id, tool_call_id)` or raw `tool_call_id` with `limit: 1` |
| `AgentToolResult` | truncation spill (`truncation/spill.rs`); fork copy | exact attachment in timeline/session projections | immutable exact call-version fact with conflict enumeration, but spill persistence remains fail-open and is not the complete output ledger |
| `AgentToolApproval` | CLI/desktop approval client (`config_client/approval.rs`); fork copy | exact held-call watcher and timeline attachment (`hook/persistence/approval.rs`) | immutable exact call-version and signer checks landed; ACP authority and concurrent/quorum policy remain |
| `CompactionEntry` | compaction reducer (`session/compaction_entries.rs`); fork copy | prompt assembly, context-budget tools, session UI | create-only exact source manifest landed; ordinal allocation and full fork-prefix authority remain |
| `Goal` | goal API/CLI and trigger controller (`goal.rs:546-888`, `trigger_engine/goal_source.rs`) | canonical goal load, active-goal trigger scan, usage aggregation | canonical earliest twin is chosen, but twins can diverge and only the selected doc is CAS-updated |
| `AgentMemory` | agent memory tool (`toolset/memory.rs`) | same tool by complete `memory_id` match set | reads/writes now reject twins, validate the immutable owner/key tuple, mutate exact `_docID`, and post-verify; head/revision and fleet-writer contracts remain open |

## Detailed collection decisions

Each entry first records evidence, then the recommended target contract. A
target field marked immutable is intended to be non-null unless explicitly
described as optional.

### `AgentSession`

**Evidence.** The schema is a mutable branchable envelope keyed by globally
unique `session_id`; only `agent_did` and `requester_did` are immutable. The
runtime now enumerates every physical `session_id` match and fails closed with
a typed, deterministically ordered `_docID` conflict. Ensure validates the
existing principal, requester, and behavior binding, updates the exact
`_docID` (or creates only from an empty set), then re-enumerates and verifies
the result. Close likewise targets and verifies the exact document. This is
reader/writer hardening; it does not yet provide the deployment lease or
single-writer transition authority recommended below.

**Recommended contract.** Primary archetype: single-writer lifecycle envelope;
canonical, with a separate conversation projection. Meaning: one durable
collaboration scope. Authorized creator: requester creates session intent or
the owning agent creates it while processing an authorized request. Transition
writer: the elected agent execution owner; requester may request closure but
does not write lifecycle fields. Claimed principal: `agent_did`; required
signers: creator for genesis and elected owner for lifecycle commits.

Logical `session_id` is globally scoped protocol correlation, while `_docID` is
the parent for every Track A row. Concurrent creates are a surfaced conflict;
choose lowest `_docID` only as a temporary deterministic display canonical and
stop execution until ownership/routing fields agree. A genesis CID is not a
portable substitute for document identity. Immutable fields: `session_id`,
agent principal doc ID/DID, participant DID, behavior doc ID, placement class,
and created time. Mutable fields: lifecycle state and ended time only. Illegal:
completed with no `ended_at`, active with `ended_at`, principal/behavior change,
or children referencing a noncanonical/conflicted session.

Live gossip: agent failover route plus participant route. Backfill: branchable
sync by the same scopes. Branchable: **retain**. ACP: participant read/create
intent; principal executor update; auditor/exporter governed read. Encryption:
per-session data-encryption key, with envelope keys for participant and executor
groups.

Retention: hot while active plus a policy default of 90 days after closure;
metadata/audit archive seven years by default. Legal hold freezes purge and key
destruction. Sunset is a signed closed/tombstone fact followed by coordinated
peer/archive purge; cryptographic erasure destroys the session DEK where no
hold applies. Indexes: unique logical session ID for local conflict detection,
principal/status, participant, and ended time. Remove all logical-ID
`limit: 1` correctness reads.

Lean/conformance: add session lifecycle, ownership, conflict, and child-parent
closure; existing conversation recovery is not a session model. Breaking plan:
fresh schema epoch or successor collection, explicit import ledger for old
rows, and rejection/quarantine of duplicate bindings. Status: **provisional**.

### `AgentConversation`

**Evidence.** This is a mutable UI materialization, but title updates and fork
metadata give it non-regenerable input. Known duplicate `session_id` rows are
ranked by `(updated_at, richness, _docID)` and recovery converges their status
instead of deleting them (`session/query.rs:133-226`,
`lifecycle/recovery.rs:555-780`). Writes correctly target the chosen `_docID`,
but the chosen row can change when any twin gets a newer update.

**Recommended contract.** Primary archetype: projection/materialization;
derived. Meaning: list/detail head for one session. Authorized creator/updater:
projection worker acting for the agent; human title changes enter a separate
immutable `ConversationAnnotation` command/fact. Claimed principal and required
signer: projection owner agent, while annotation signer is the participant or
operator.

Identity: one projection per `session_doc_id` and projection version. It stores
the session `_docID` plus the exact source manifest/CIDs that produced
`latest_request`, preview, status, and generated title. Immutable: session
reference, agent/participant placement, projection contract version. Mutable:
projection body and source manifest as one atomic replacement. Illegal: latest
request outside the session, generated title without source capture, fork
metadata without an exact source session/version, or mixed-source fields.

Live gossip/backfill: participant and failover scopes; branchable **retain** so
late clients can obtain annotations and the current head. ACP: participants
read; projection worker update; annotation authors cannot rewrite projection.
Sensitive preview/title use the session key. Retention follows the session;
projection may be rebuilt or purged independently, but annotation facts follow
audit/legal hold.

Hot indexes: unique session doc reference, principal/updated time,
participant/updated time, and status. Remove the recovery rank as normal
identity; keep it only in legacy import. Lean/conformance: model projection
determinism, source-version monotonicity, annotation precedence, and rebuild
equivalence. Breaking plan: archive legacy twins, import annotations, rebuild
one projection from canonical facts. Status: **provisional**.

### `AgentRequest`

**Evidence.** One document mixes immutable routing fields, mutable user input,
claim ownership, backend selection, retries, interrupt command, lineage, and
terminal state. `request_id` is indexed but not unique. The Lean request model
defines nine states and guarded transitions
(`proofs/Proofs/Request/State.lean:5-169`,
`Request/Transition.lean:5-75`), and runtime transitions usually compare
`_docID` plus expected state. The first provenance slice pins the exact claim
commit before provider use. Nevertheless, timeline and status helpers still
select a logical request ID with order plus `limit: 1`
(`run_timeline_fetch.rs:75-111`, `lifecycle/lookup.rs:90-132`).

**Recommended contract.** Split into immutable `RequestIntent` and mutable
`RequestExecution`. Intent is a command created and signed by the requester (or
an agent/trigger acting under a recorded delegation); execution is a
single-writer lifecycle owned by the target principal's elected deployment.
The execution genesis pins intent `_docID` plus composite CID. `request_id`
remains globally scoped correlation; retry/root/parent/trigger/subagent edges
become document references, with CIDs wherever state was consumed.

Intent immutable fields: session doc, requester, target principal, behavior
reference, content, sampling options, metadata envelope version, origin,
valid-until, lineage, and created time. Execution immutable fields: intent
version, elected deployment/claim epoch, selected behavior/backend versions,
and claim time. Mutable execution fields: lifecycle, deadline, retry count,
failure/terminal evidence, and interrupt acknowledgement. An interrupt itself
is a separate immutable command so requester and executor do not share update
authority.

Illegal states include `status != lifecycle_state` projections, terminal with
no terminal evidence/time, processing without an active claim/backend,
completed without committed response, retry edges outside one root, asymmetric
subagent parent links, expired intent claimed after TTL, and lifecycle changes
by a non-owner. Live gossip/backfill: requester participant route and target
principal failover route; branchable **retain**. ACP: requester creates/reads
intent, target reads intent and creates/updates execution, participant reads
per grant, nobody mutates intent. Encrypt content/metadata with the session key.

Retention: request intent/execution hot with session, audit archive seven years;
payload erasure uses the session key while retaining non-sensitive signed
metadata when policy permits. Indexes: intent `request_id`, target/status/time,
session/created; execution unique intent doc and owner/state/deadline. Replace
hashed `retry_key` with component fields and a composite local unique index.

Lean/conformance: refactor the existing state machine over the execution row;
prove intent immutability, owner fencing, exact intent binding, retry graph
closure, interrupt command consumption, conflict fail-closed, and terminal
replica convergence. Breaking plan: split every legacy row at a named composite
CID; legacy rows lacking a trustworthy boundary are imported as unverified and
cannot be replayed. Execution-owner fencing depends on the deployment assignment
and lease epoch defined by Track D; a string `agent_did` alone is not a fence.
Status: **provisional; P0**.

### `AgentMessage`

**Evidence.** The provider loads messages by `session_id`, ordered only by
`sequence`. Mutable assembly now lives in `AgentMessageDraft`; finalization
creates an immutable `AgentMessage`, reloads its exact document version, and
accepts idempotency only when the complete canonical fact agrees. Conflicting
logical facts are enumerated and rejected. Sequence is still
`max(message, background-reservation)+1`, followed by bounded retries, so the
ordering allocator remains node-local. Forked messages carry the source
`_docID`, composite CID, and verified signer DID.

**Recommended contract.** Primary archetype: immutable durable transcript fact;
canonical. Creator: the actual message author—requester for user content,
principal executor for assistant/tool observations—under a role-specific ACP
relationship. Claimed principal: immutable `author_did`, distinct from session
owner and participant. Required signer: author/delegated executor consistent
with role.

Identity: `_docID`; logical `message_id` is globally unique correlation.
Immutable fields: session doc, author, role/kind, request intent/execution ref,
sequence tuple, content/reasoning envelope, timestamp, tool pairing refs,
source doc/CID for forked material. There are no mutable fields. Duplicate
idempotency keys must compare the complete canonical fact; mismatch is an
integrity error.

Illegal: duplicate session order key, unknown role/kind, tool result without a
completed linked execution, assistant call without invocation rows, message
after a closed session unless explicitly late/recovered, or copied content
without source lineage. Live gossip/backfill: session participant plus owner
failover; branchable **retain**. ACP: participant reads; author creates; no
update/delete outside retention workflow. Encrypt payload/reasoning per session.

Retention: configurable 30-day hot payload default, seven-year encrypted audit
archive for regulated deployments; metadata may outlive erased payload only if
the export contract marks it redacted. Legal hold retains DEK and blocks purge.
Indexes: composite unique session/order; request doc; author/time; tool
execution doc. Lean/conformance: strengthen Transcript refinement so database
rows enforce unique total order, author/role coherence, immutable idempotency,
and replicated conflict handling. Breaking plan: freeze old rows, import each
as a fact with source `_docID`/CID; never upsert. Status: **immutable
finalization checkpoint implemented; distributed ordering and the broader P0
contract remain provisional**.

### `AgentToolCall`

**Evidence.** `ToolCallLifecycle` models seven states and writes conditional
`_docID` transitions (`tool_call_lifecycle.rs:20-73`,
`tool_call_lifecycle/transition/native.rs:31-811`). The schema still combines
invocation arguments, lifecycle, output, policy decision detail, background
coordination, and child linkage. Invocation identity, route, arguments, and fork
source fields are now immutable, and mutable result/approval bindings can pin
exact fact versions. The invocation and execution roles nevertheless remain in
one document. `tool_call_key` concatenates session and provider call ID. Some
reads use that key; lifecycle load filters session plus call ID with `limit: 1`
(`tool_call_lifecycle/query.rs:89-139`).

**Recommended contract.** Split immutable `ToolInvocation` from mutable
`ToolExecution`; represent full outputs as `ToolOutputFact`. Invocation creator
is the request execution owner and pins request, assistant message, selected
tool/service/config, arguments, policy-input versions, and call ID. Execution
writer is the elected local/remote tool owner; bridge ownership is explicit.
Required signers correspond to those owners.

Immutable invocation fields include session/request/message refs, agent and
requester route, tool identity, args, await/cancel policy, child target, and
workflow membership. Execution genesis pins invocation CID and immutable claim
epoch/deployment. Mutable fields are lifecycle, deadlines, partial-output head,
terminal classification, latency, remote-ack state, and output reference.
Denial evidence is an immutable policy-decision fact rather than fields that can
be rewritten with execution state.

Illegal: terminal without completion/failure evidence, completed without output
fact, approval-required running without accepted approval bound to the held
version, native execution with child link, bridge with missing symmetric parent
links, cancel policy changed after dispatch, or terminal-to-live transition.
Live gossip: owner/failover, participant-redacted, subagent coordinator by
target, and host return by requester. Backfill: all relevant routes;
branchable **retain**. ACP separates invocation creator, execution owner,
approver, participant reader, and auditor. Args/results use session or
delegation-scoped encryption.

Retention follows request/session, with long-lived policy and authorization
evidence. Indexes: composite unique session doc/call ID; request doc/state;
spawn target/state; workflow group/role; deadline/state. Lean/conformance:
retain ToolExecution/Background models, then prove split-row coherence,
owner fencing, exact approval/output binding, and recovery across replication.
Breaking plan: split at invocation genesis and terminal composite CIDs; mark
ambiguous legacy mutations unverified. Status: **provisional; P0**.

### `AgentToolResult`

**Evidence.** Despite its name, the only canonical writer is the truncation
spill path, which creates a row only when model-visible output is truncated;
failure is still logged and execution continues without a spill. The row is now
an immutable fact with a stable result key, route fields, full tool input/output,
the exact accepted `AgentToolCall` `_docID`/composite CID/signer, and fork-source
provenance. Creation enumerates conflicting logical facts and requires exact
equality for idempotency. The run timeline verifies and attaches exact result
versions to their tool call. This makes the spill auditable, but not mandatory
or canonical for every full tool output.

**Recommended contract.** Replace with immutable `ToolOutputFact`; canonical
for every tool output, not just truncation overflow. Creator and signer: the
writer that actually observed the output—normally the tool execution owner,
but the parent/bridge owner for a returned child result. Identity: `_docID`,
linked to invocation/execution `_docID` and the exact execution CID that
accepted it. For chunking, unique composite
`(execution_doc_id, output_stream, chunk_sequence)` plus terminal manifest.

Immutable fields: session/request/invocation/execution refs, agent/requester
scope, MIME/encoding, complete output or encrypted blob reference, truncation
projection metadata, chunk position, created time, discard/interruption status,
and source refs for imports/forks. No mutable fields. Illegal: orphan output,
multiple complete terminal manifests, truncated model observation without a
durable full-output fact when retention policy requires it, or an interrupted
output reported as model-consumed.

Live gossip/backfill: owner/failover always; participant only under result
redaction grant; branchable **retain**. ACP: execution owner create, session
grantee read at allowed redaction, no update. Use session/tool-output DEKs and
external object-store envelope encryption for large values.

Retention defaults shorter for raw output (30 days hot) with policy-controlled
archive; authorization/metadata manifests remain with request audit. Legal hold
pins blobs and keys. Indexes: execution doc/chunk, request doc, session/time,
participant route. Lean/conformance: extend transcript pairing to require exact
output fact and test fail-closed durability when full output is promised.
Breaking plan: import spills as `legacy_partial_archive`; do not claim they are
a complete output ledger. Status: **exact immutable spill checkpoint
implemented; complete-output durability and the broader P0 split remain
provisional**.

### `AgentToolApproval`

**Evidence.** Approval creation now resolves the exact held `AgentToolCall`
version, verifies its signer, and writes an immutable approval fact containing
that call `_docID`/composite CID/signer plus immutable routing and decision
fields. The held-call watcher enumerates approval conflicts, verifies the
approval commit signer against `approver_did`, and fails closed instead of
choosing the first timestamp. Approval facts are present in the standard
runtime/chat and requester-filtered replication routes. The remaining authority
gap is that no DefraDB policy proves the signer is an authorized approver, and
no data-layer round/quorum contract resolves concurrent valid operators.

**Recommended contract.** Primary archetype: immutable authorization fact;
canonical. Creator/signer: an ACP-authorized human/device/service approver.
Meaning: one decision for one approval round and one exact held tool-execution
version. Identity: `_docID`; composite unique `(tool_execution_doc_id,
approval_round, approver_did)` with a separately declared quorum/first-wins
policy. Store held execution `_docID` plus CID and policy/version evaluated.

Every field is immutable: agent, requester/session scope, approver, decision,
reason, tool execution/version, approval round, policy reference, signed time,
and expiry. Illegal: decision for a non-held version, unauthorized signer,
decision after expiry/cancel, conflicting decisions without a deterministic
policy outcome, or approval reused after args/tool/policy change.

Live gossip: approver/operator to owning agent execution hosts; optional
participant receipt. Backfill: required for audit and failover; branchable
**retain**. ACP: approver create, owner read/consume, participant/auditor
governed read, nobody update/delete. Encrypt arguments/reason but retain a
verifiable non-sensitive decision envelope.

Retention: seven-year authorization audit default, legal hold capable; purge
only with the linked execution and policy audit set. Indexes: tool execution +
round + approver, agent/unconsumed/time, expiry. Lean/conformance: model
approval document selection, version binding, signer authorization, replay
rejection, and concurrent decisions. Breaking plan: old decisions import as
unverified because signer and held-version evidence are absent. Status:
**exact immutable approval checkpoint implemented; ACP/quorum and the broader
P0 authorization contract remain provisional**.

### `CompactionEntry`

**Evidence.** A finalized compaction is now an immutable create-only fact. Its
versioned canonical source manifest pins the exact transcript, resolved-config,
and prior-compaction document versions consumed by the summary. Runtime
admission enumerates same-key conflicts and accepts only exact idempotent replay;
Lean and generated conformance cases cover manifest equality, source stability,
and conflicting replay. Fork copies preserve exact source document-version and
signer provenance. The compaction ordinal is still derived locally from prior
entries, and there is no fleet lease or first-class fork-prefix manifest tying
all compaction boundaries to one distributed session owner.

**Recommended contract.** Primary archetype: immutable transcript-reduction
fact; canonical. Creator/signer: owning agent's compaction worker. Identity:
`_docID` with composite unique session doc/compaction ordinal. The fact pins
the prior compaction (if any), exact message/tool-output document versions,
compacted-through order key, summarizer rendered-request capture, and resulting
summary/output field CID.

All fields immutable: session and actor scope, ordinal, source manifest,
boundary, summary, file activity as typed/versioned data, token accounting,
created time, and algorithm/version. Illegal: overlapping or regressing
boundaries, missing source versions, summary captured before safe-to-reduce,
token counts inconsistent with manifest, mutable same-key rewrite, or fork copy
without original provenance.

Live gossip/backfill follows session participant and failover routes;
branchable **retain**. ACP: agent compactor create, participant read under
transcript grant, no update. Encrypt summary and paths with session key.
Retention follows source transcript and cannot outlive erasure of source
payload as “verified”; archive keeps manifest/CIDs and redaction state. Indexes:
unique session/ordinal, session/boundary, created time.

Lean/conformance: connect the Compaction model to persisted `_docID`/CID
manifests, database order allocation, idempotent create equality, and fork
prefix composition. Breaking plan: legacy entries import as captured summaries
without reconstructible provenance. Status: **exact source-manifest checkpoint
implemented; distributed allocation/fork composition and the broader P0
contract remain provisional**.

### `Goal`

**Evidence.** Goal combines operator objective/budget and runtime controller
state. `goal_id` is deliberately non-unique; code sorts twins by earliest
`(created_at, goal_id, _docID)` and advances one with `_docID` compare-and-set
(`goal.rs:463-530`, `546-784`). A comment says twins arise through replication,
but standard replication templates omit the collection. Most timestamps are
nullable strings. Lean proves lifecycle, blocked-audit threshold, and
continuation decisions (`proofs/Proofs/Goals.lean`).

**Recommended contract.** Split immutable/versioned `GoalIntent` (objective,
budget, operator actions) from agent-owned `GoalExecution` lifecycle. Creator:
session participant/operator with goal authority, or the agent under explicit
self-goal authority. Execution transition writer/signer: elected agent owner.
Identity: one active goal execution per session document; intent versions are
append-only and execution records which version they adopted. Request
continuations link by request `_docID`, not ID strings.

Immutable execution fields: session/principal/intent version and controller
epoch. Mutable fields: formally modelled status, usage checkpoint, active time,
blocked-audit state, continuation CAS counter, wrapup and failure evidence.
Illegal states are those rejected by `Goals.step?`, plus negative counters,
non-positive budgets, `complete` without completion evidence,
`budget_limited` without wrapup requested, completed wrapup without request
evidence, or two active controller epochs.

Live gossip/backfill: principal failover set; participant read/write-intent only
when granted; branchable **retain**. ACP separates intent author from execution
updater. Encrypt objective/failure/evidence per session. Retention follows
session plus controller audit; legal hold retains intent/action/execution chain.
Clearing a goal appends a cancel/sunset intent rather than deleting twins.
Indexes: unique session/current execution, principal/status, intent version,
continuation parent request.

Lean/conformance: preserve Goals model and add split authority, epoch fencing,
exact request evidence, twin conflict convergence, and replicated failover.
Breaking plan: select legacy canonical only for import, record every twin and
selection; do not silently delete. Status: **provisional; P1**.

### `AgentMemory`

**Evidence.** The agent tool builds an injective length-prefixed logical
`memory_id` from `(agent_did, key)`. It now enumerates every match, rejects
logical twins, validates `memory_id`, `agent_did`, and `key`, creates only from
an empty set or updates the exact `_docID`, and re-reads the complete set to
verify the write. `agent_did` and `key` remain mutable in the schema, however,
and the collection is branchable without the target revision/head or writer
lease model. There is still no consumption/version reference when memory
affects a decision beyond any resulting transcript/provider capture.

**Recommended contract.** Model a mutable `MemoryHead` projection over
immutable `MemoryRevision` facts. Meaning: principal-owned cross-session
knowledge, never participant transcript by default. Creator/updater/signer:
the principal's authorized execution owner or explicit operator. Identity:
composite `(agent_principal_doc_id, normalized_key)`; no concatenated
`memory_id` required. Each revision pins previous head/version and author.

Immutable head scope: principal document, key, placement/encryption class.
Mutable head: current revision reference and updated time. Revision fields are
all immutable: value envelope, previous ref, author, source request/message
refs, created time, TTL/classification. Illegal: principal/key move, revision
cycle, head regression, conflicting concurrent revisions hidden by `limit: 1`,
or sharing to requester without an explicit release fact.

Live gossip/backfill: only the principal's authorized failover/execution hosts;
branchable **retain**. ACP: principal/executor read and append; operator by
delegation; ordinary session participants denied. Use a per-principal or
per-memory-class DEK with rotation and revision-aware erasure.

Retention: classification-specific TTL; default 30 days hot for unclassified
memory, with no enterprise archive unless explicitly promoted. Legal hold is
explicit and exceptional. Sunset appends a deletion revision, removes head
visibility, then coordinates peer/archive purge and key destruction. Indexes:
unique principal/key, head update time, revision previous/source/TTL.

Lean/conformance: add revision DAG/head selection, owner fencing, concurrent
write policy, placement noninterference, and deletion/legal-hold behavior.
Breaking plan: import current value as a genesis revision with legacy source
CID; enumerate duplicate logical keys before choosing a head. Status:
**provisional; P1**.

## Fork durability decision

Fork is not a bulk-copy implementation detail; it is a provenance operation.
The current implementation computes a cut and creates child messages, calls,
results, approvals, and compactions with new keys. Each copied fact now carries
the original `_docID`, composite CID, and verified signer DID, and copied route
fields retain participant placement. Lean and generated conformance tests prove
that a copied fact cannot silently change its source version. This is per-row
derivation evidence, not yet a single immutable assertion of the exact ordered
prefix: cut selection still mixes transcript sequence with timestamp-bound
facts, and the child session has no `SessionFork` manifest that freezes the
complete source set.

The target fork is an immutable `SessionFork` fact containing source session
`_docID`, source composite CID, exact ordered prefix manifest, child session
`_docID`, and fork policy/version. Prefer a child history view that composes the
immutable source prefix with child-local facts. If physical copying is needed
for placement or retention, every copy must carry original `_docID` and CID and
must retain the child's immutable routing fields. Lean must prove prefix
closure, pair closure, source-version stability, and that copying/reordering
cannot manufacture a different history.

## Retention and enterprise archive contract

The target archive unit is a session bundle plus separately governed principal
memory. A bundle is incomplete unless it carries:

- schema epoch and collection schema version;
- every `_docID`, composite CID, relevant field CIDs, signature, and verified
  signer status;
- logical IDs and exact document-version lineage edges;
- request/tool/goal lifecycle transition evidence;
- fork and compaction source manifests;
- the ACP policy/resource/relationship version and redaction decision applied;
- payload encryption class, key identifier (not key material), hold state, and
  purge eligibility;
- conflict sets and canonicalization/repair decisions; and
- export contract version, exporter identity, time, and destination receipt.

Archive is not the current run timeline. The timeline omits several facts and
performs logical-ID joins. Implement archive projection from exact document
references, fail closed on ambiguous/missing sources, and test restore into an
empty authorized host. A tombstone or projection deletion alone is not physical
erasure; completion requires acknowledgements from operational peers, archive,
backup, and key custody.

## Breaking schema and proof sequence

These recommendations intentionally do not require an in-place compatibility
migration. The implementation sequence is:

1. Declare a new schema epoch and exact successor collection contracts.
2. Update Lean models first for changed lifecycle, authority, transcript order,
   provenance, fork, compaction, approval, and goal semantics.
3. Generate conformance cases for legal/illegal transitions, conflict sets,
   exact-version reconstruction, ACP roles, and replication recovery.
4. Implement new writers that create immutable facts and update only
   `_docID`-addressed lifecycle/projection heads.
5. Switch all hot reads to document references and complete conflict reads.
6. Add filtered live replication, branchable late-peer sync, signer checks,
   ACP installation/relationships, encryption, archive, and purge workflows.
7. For disposable pre-release stores, reset at the epoch boundary. For retained
   stores, export the legacy DAG first and import successor facts with explicit
   `legacy_source_doc_id`/CID and verification status. New CIDs must never be
   represented as preservation of old CIDs.

Required integration tests span at least two writers and two nodes: concurrent
session/request/message/tool/goal/memory creation, filtered gossip, reconnect
and late-peer backfill, unauthorized normal/history/CID reads, owner failover,
fork reconstruction, compaction reconstruction, archive/restore, hold, and
coordinated purge.

## Prioritized child-issue candidates

1. **P0 — Split `AgentRequest` intent from execution and replace logical joins
   with exact document/version references.** Includes lifecycle owner fencing,
   interrupt facts, retry components, and removal of request-ID `limit: 1`.
2. **P0 — Finish transcript ordering and pairing.** Append-only finalized
   messages, separate drafts, exact request links, and conflict checks are
   implemented. Add composite uniqueness and lease-fenced multi-host allocation.
3. **P0 — Finish splitting tool invocation/execution/output and make full output
   durable.** Exact immutable result facts and timeline attachment are
   implemented for spills. Replace best-effort spill semantics and bind every
   terminal execution to its complete output version.
4. **P0 — Finish approval authorization semantics.** Signed,
   exact-version-bound, replicated approval facts and conflict enumeration are
   implemented. Define concurrent/quorum behavior and add base DefraDB ACP,
   delegation, replay, and expiry tests.
5. **P0 — Finish compaction ownership and fork composition.** Immutable exact
   source manifests and conformance coverage are implemented. Add lease-fenced
   ordinal allocation and compose them with the frozen fork-prefix manifest.
6. **P1 — Establish `AgentSession` `_docID` as the conversation-plane spine.**
   Add lifecycle/ownership model, route fields, conflict quarantine, and remove
   session-ID `limit: 1` reads.
7. **P1 — Replace fork copying with an exact-version fork manifest.** Preserve
   routing, prefix/pair closure, and auditable derivation.
8. **P1 — Split goal intent/controller state and implement replicated owner
   failover.** Eliminate divergent canonical twins and deletion-as-clear.
9. **P1 — Introduce versioned principal memory with private placement.** Make
   principal/key immutable, define concurrency, ACP, TTL, and erasure.
10. **P1 — Rebuild `AgentConversation` as a source-versioned projection.** Move
    human annotations to facts and retire duplicate ranking from normal reads.
11. **P1 — Implement the two-route replication and base-collection ACP matrix.**
    Cover all Track A collections, relationship bootstrap, history/CID reads,
    and late-peer sync. Coordinate signer verification with #1064.
12. **P2 — Ship governed session archive/restore, retention, legal hold, and
    purge receipts.** Require the full provenance envelope above.

The first five issues form one durability foundation and should be designed in
parallel but merged in dependency order: authority split, transcript order,
tool facts, approvals, then compaction. Session identity and fork provenance
must land before declaring Track A complete.
