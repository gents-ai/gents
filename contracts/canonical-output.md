# Canonical output (#1571)

Layer 1 is the breaking protocol/SDL specification. Baseline draft: `21ec9f8fd`.
Executable contracts follow in Lean, generated conformance, then implementation.
No builds or tests run in this layer. No compatibility readers, migration lenses,
historical schema copies, or data wipes are part of this stack.

## The model

Output uses two immutable, create-only document shapes; nothing in either is updated.

| Fact | Collection | Says |
| --- | --- | --- |
| `OutputSegment` | `AgentOutputSegment` | A flush, closure, or both. Data has `(request_doc_id, source, ordinal)`; closure has `(request_doc_id, source)`. A terminal-only record has no ordinal. |
| `TranscriptMessage` | `AgentMessage` | This native message, published complete or partial, with payload `{close_doc_id, stream}` references. |

Normal closure rides on the final flush. If all bytes were already flushed,
recovery closes an existing prefix, or an attempt is retracted, append a
terminal-only record with no ordinal/runs/payload. Never edit an earlier flush.
A final-flush record has both a data coordinate and a closure coordinate;
terminal-only closure has just the latter. `Closed.segments` counts data flushes,
including the closing record's data if present, never terminal-only records.
Resolve all closure candidates first; conflicting closures fail. Then validate
exactly the selected extent, ignoring raw flushes beyond it before data-twin
checks. This prevents a late ordinal-N flush from colliding with recovery's
terminal-only closure of ordinals 0..N. A reference to a plain flush is invalid.

Fresh Complete acceptance is stricter than replica reconstruction: under the
owner gate, its extent must include every committed data flush for that source,
including any final flush. Validate the dense prefix, writer binding and
nondecreasing timestamps, with no data timestamp after closure. This fresh-write
check must not be reapplied to reject exact replay or later out-of-extent facts.

Provider/tool payloads and whole authored content live in segments. Small runtime
presentation literals live inline in headers; they do not duplicate payload bytes.
There is no payload-copy exception: a session started by `create_session` or
`send_message` receives its own materialized request, not copied arguments.
The collections only grow, so replication is set union and a reader only asks
which facts are visible: no visible closure means closure is unknown, a closure with
missing segments means an incomplete replica, twins mean a conflict. Unsealed
output also needs a matching current request generation or eligible tool owner;
absence of a closure never proves liveness. Storage grows with new
output records and one header per message — never with
display-update frequency. Live views, transcripts, provider input, forks,
hydration and exports are projections through one shared reconstruction.

An accepted provider turn atomically publishes its Complete closure, assistant
header and pending tool rows before dispatch. Partial headers
may retain tool-call provenance, but only with terminal, nondispatchable rows.
Dispatch still checks existing request/tool cancellation, deadline and policy
owners. Publication is durable intent, not permission to execute after cancellation.

| Boundary | Required behavior |
| --- | --- |
| Provider fails before acceptance/publication | No tool from the attempt has run. Existing retry policy may retract/resample; terminal partial output cannot dispatch. Usage still charges every reported attempt. |
| Publication fails or its commit acknowledgement is lost | No dispatch until the owner confirms the exact closure/header/tool rows committed. Replay reuses those facts. |
| Cancellation/deadline wins after publication, before dispatch | Existing owner terminalizes undispatched calls without effects; the published turn remains history. |
| Dispatch/result fails after publication | Complete the existing call's outcome; never retract or resample the accepted provider turn. |
| Recovery after publication or an uncertain tool effect | Recover existing lifecycle rows under their established policy. Pending intent is not proof an external effect never happened; no exactly-once host-effect claim. |

Tool startup moves later and can lose overlap with the remaining provider response.
Awaiting dispatch previously stopped local polling, not remote generation/delivery.
The benefit is a durable accepted turn before effects and a simpler pre-publication
retry boundary. Preserve call order and the cumulative invalid-tool budget: stopping
dispatch must account for every published undispatched call through the existing
terminal owner, not silently drop it.

### Request/tool ownership composition

Accepted publication, dispatch, tool terminal output, result delivery and request
termination share one authoritative session transcript and tool-document state.
`ToolCallContext` is the existing lifecycle owner; its logical labels do not
substitute for the physical `AgentToolCall`/request binding. A tool is bound to
the exact accepted header's session, sequence and physical call document.
Transcript tool rows are a checked projection of those lifecycle rows, not an
independent execution machine. Publication installs pending lifecycle rows and
their transcript projection in the same transaction; dispatch changes both.
The shared transcript is session-wide: a previous request's background call may
still deliver while the next request owns its lease. Generation numbers alone
are not an ownership key. Cancellation/recovery must match the physical request
and accepted header as well as the generation; delivery uses the originating
call's request binding and never the currently active request's identity.
`spawn_process` is an explicit exception to direct provider-intent membership:
its separate native background execution row carries immutable
`spawned_by_tool_call_doc_id`, naming the physical accepted meta-call that
created it. Validate that parent and its configured operation through the
existing spawn owner; matching a session/sequence/tool name alone is not
provenance. The spawned row is not another assistant tool call and creates no
invented provider reservation. Its lifecycle, output and completion notification
retain the actual spawned document identity, while cancellation/recovery trace
its request/generation ownership through the accepted parent call.
That parent link is execution-admission provenance, not a new hydration grant.
Output hydration follows the referenced spawned tool document and its request;
it does not recursively expose the meta-call or parent arguments. Fork retention
follows the canonical payload/header dependency graph, not arbitrary tool-row
foreign keys.

Normal request completion requires every started direct invocation to have its
durable native reply. Foreground obligations additionally require terminal tool
outcomes. A running foreground tool, or any started direct invocation whose reply
has not been published, blocks normal completion. A separately spawned execution
does not owe a second provider invocation reply. Never make this pass by
automatically backgrounding or detaching the
tool inside terminalization. An explicit existing tool-control operation may
transfer a running call to background ownership first. Background mode and
cancellation policy are separate: ordinary completion may leave owned background
work running, while explicit interruption applies its cascade/detach policy.

Recovery and exceptional termination cannot wait for an external process to
acknowledge cancellation. In the same transaction as the generation swap or
terminal decision, cancel exact owned pending calls before dispatch and hand
running calls to the existing tool cancellation/recovery owner. Preserve their
actual running lifecycle until terminal evidence arrives. Existing
`cancel_cascade_intent_at`, `cancel_pending_remote_ack` and `stuck_since`
observations represent the durable handoff; clearing parent in-flight ownership
does not prove a host process stopped. Never resubmit a possibly-started effect
as a new pending invocation. Unrelated requests' tools are untouched.

Native restart recovery additionally requires the existing registry's exact
physical-tool orphan observation; parent expiry is not proof of process absence.
With no registered executor, recovery closes only already-committed output using
a terminal-only Partial record. It may not invent a final flush. The ordinary
notification still goes through the existing Goal/queue and native template owner.

Tool delivery uses the session's single sequence allocator. Its canonical header,
transcript row (native result or ordinary notification), delivery identity and
tool-row projection commit together;
replay reuses that exact publication and allocates no sequence. Execution outcome
and delivery remain distinct: delivering a failed/cancelled/timed-out result
does not change its lifecycle to completed. Pending direct-call cancellation may
require a later empty output closure and native cancellation result; it must remain deliverable without
redispatch. A late background result can close and publish after parent expiry
or termination without renewing or reopening that request. A
`create_session`/`send_message` row closes with the terminal output of the
request it caused, not a fabricated native completion.

An invocation reply is not always execution completion. A started session
returns a native `tool_result` receipt while its row remains running, then a
separate ordinary-text completion notification after the caused request
terminates.
Preserve both publications and their independent replay identities. The receipt
uses its own whole authored source under the tool writer; it does not close the
still-running tool output source or claim a terminal lifecycle. The terminal
notification follows the existing background-completion/session-queue owner,
without appending a second copy of its transcript message. Native shapes
distinguish the invocation reply from the notification; no new mutable response
status is introduced. Compaction projects native provider call IDs, not physical
tool-document IDs, and must retain the distinction between the initial native
result and the later ordinary notification.

The notification and continuation queue have one publication transaction. Goal
presence never suppresses the wake: the notification's `request_doc_id`
names its coalesced wake request;
Goal-owned input-only delivery remains parent-bound. This is publication
membership, not the payload's provenance: references still resolve the exact
originating tool/request source. Replay after a wake is claimed or finished uses
the actual persisted notification-to-request binding and authenticated request
source/key/session, not a bare active/terminal logical request ID or the tool's
notification-delivered timestamp. Both request dependencies retain their existing
ACP checks during hydration. Do not fabricate a second receipt column or let a
tool publisher select an arbitrary request as the notification owner.

Claiming the next queue entry must activate that exact authenticated physical
request under the same local gate. Preserve the session transcript, sequence
allocator, compaction cursor and earlier requests' tool ownership; do not reset
them with the request lease. A live predecessor cannot be replaced. The wake's
immutable input snapshot is derived from canonical notification bindings at the
claim cutoff, not supplied as an arbitrary subset. Finishing the exact claim
uses the authoritative terminal request decision: completed work acknowledges
its snapshot; failed or cancelled work releases the queue without acknowledging
it. A snapshot or separately persisted response cannot manufacture a terminal
decision. Goal continuation publication retains its existing Goal owner and
must join the actual queue admission, not fabricate a background wake.

User and steering admission does not publish transcript output. While queued,
clients display the signed `AgentRequest.content` as admission input, not as an
`AgentMessage`. When owned execution starts, its request-generation authority
publishes the actual authored prompt/context through the canonical writer before
provider dispatch. Enqueueing alone neither allocates a transcript sequence nor
grants publication authority. Clients reconcile the queued input with its
published input using the exact physical request binding, never text equality;
replica arrival order must not create duplicate user messages. This does not
change tool-owned background notification publication, which has independent
`ToolDelivery` authority and retains its atomic notification/queue transaction.

Provider acceptance and retry retraction use the existing retry transitions
against the latest gate-held execution state. Policy-only actions cannot confirm
durable publication, and raw publication cannot bypass retry eligibility. Exact
replay validates the original canonical facts without spending another attempt.
Backoff wake-up uses the actual monotone observation time, at or after the
scheduled lower bound and within the request deadline; timer overshoot alone is
not failure and must not rewind the owner's clock.

The composed model must prove these cross-owner effects and sequence ordering,
not merely accept isolated owner predicates. Native host stop/acknowledgement,
remote replication latency and transaction isolation remain external evidence.

`OutputOutcome` is completeness only: `Complete` or `Partial`. Why output was cut
short, and whether the request or tool succeeded, stay with the request and tool
lifecycles and are not classified a second time. The publisher sets a header's
outcome directly; it is never derived from the closing records its blocks reference, so a
complete notification can wrap a tool's partial output and a message with no
payloads needs no closure. `AgentRequest.terminal_output` separately selects the exact
final assistant header (or explicit `NoMessage`) with terminalization. Late background
messages cannot change that selection.

## Recovery publication

Closing retained bytes does not make them a valid native message. Recovery of an
unheaded provider source closes exactly the committed extent as Partial (reusing
an existing Partial closure rather than appending another), but
publishes only ordinary Text streams at native part zero, using Full presentation references in their
original block order. The recovery header is Partial with no native message ID.
It omits tool arguments, reasoning (including summaries and opaque forms), and
media, even if some bytes appear decodable: their final structure or metadata may
never have committed. Never repair JSON, invent signatures/IDs, or relabel these
streams as ordinary text. This conservative rule needs no new metadata journal.
Declarations keep their original positions; omitted blocks leave gaps, so recovered
headers validate survivor order rather than equality with compacted block indexes.

Text at another part is retained diagnostically, not relabeled or allowed to
prevent closure and generation recovery. If no eligible Text stream exists,
publish no assistant header for that source. An explicitly
declared empty Text stream remains representable as an empty Partial message.
Terminal selection uses an eligible published assistant header under its existing
scope rules, or NoMessage if there is none; retained bytes alone are not an answer.
Already-published headers are resolved unchanged, never replaced with recovered
text-only versions. Complete dependency validation and provider-input narrowing
still apply to every published message.
Validate extent and run accounting across the entire source; native payload decoding
applies to referenced blocks only. Incomplete JSON in an omitted stream does not
invalidate a text-only header, and remains unchanged in diagnostic storage.

Omitted streams remain diagnostic data in the original segments, not tool intent
or provider input. Once terminal selection and any selected header are resolved,
unreferenced streams of closed Partial provider sources are RetainedPartial,
not indefinitely PendingPublication. Opaque reasoning remains non-renderable.
Tool-owned recovery continues through the tool lifecycle; this provider-source
rule does not discard tool results or rewrite accepted turns.

Recovery replay confirms the exact committed closure, optional header and
publication row under their original writer and recovered generation. It does
not rerun new-recovery source selection: a published Partial source is no longer
eligible for new recovery. Exact replay neither republishes facts nor renews the
lease; changed identities or conflicting artifacts fail confirmation.

## Output and lease renewal have separate owners

Segments record output, not request liveness. Streaming never rewrites
`AgentRequest`; the per-flush request CAS and `execution_progress_seq` remain
retired. Only claim/reclaim and bounded explicit renewal establish the lease
deadline. Output, publication, dispatch, socket activity and replay do not extend it.

The existing request execution owner renews while it owns active work, including
silent inference and foreground tool waits.
Renewal must be independently polled from provider/tool reads; a blocking read
is not permission to miss the deadline.
Its lifetime is tied to the owned completion loop and stops when that ownership
ends. A daemon-wide timer must not renew arbitrary active rows after their loop
has exited; cancellation or a lost ownership check stops renewal.
Tool activity itself is not proof the request owner remains alive. Detached
background tools retain their own lifecycle after the parent stops renewing.
Owner renewal is not a progress watchdog: it must not disable provider idle
timeouts, request deadlines, cancellation, tool timeouts or existing failure
policy. Those owners still stop work that has stalled or exhausted its budget;
a scheduled renewal alone cannot certify useful progress.

- **One explicit deadline.** Claim installs `execution_generation`,
  `execution_lease_secs` and `execution_lease_expires_at`. Effective expiry is
  exactly that stored deadline. It requires no output scan or reconstruction.
- **Bounded renewal.** Renew near the midpoint of the lease, not per flush or UI
  update. The Lean policy uses a half-duration window in discrete time; successful
  renewal must strictly advance the deadline to `now + duration`. Early attempts,
  stale generation/deadline observations and expired owners cannot renew.
  The native duration/cadence must leave scheduling and commit-latency margin;
  the discrete model does not choose a production seconds value. A renewable
  duration needs at least two model ticks: a one-tick lease cannot both advance
  its deadline and renew strictly before expiry, and is not a usable native
  renewal configuration.
- **Compare and swap.** Renewal rereads the active lifecycle, generation and
  observed deadline under the existing mutation gate and conditionally updates
  that exact lease. Retrying the same expected deadline cannot extend it again.
  After a lost acknowledgement, reread the authoritative lease and schedule from
  its current deadline; do not manufacture progress by replaying output.
  An early timer poll is a skipped write, not a failed request. A deadline-CAS
  mismatch requires a reread; only the resulting ownership/lifecycle/expiry
  decision determines whether the owner must stop.
- **Expiry is not inactivity of output.** At or after the deadline the old owner
  cannot append, publish, dispatch, normally finalize or renew, even if its
  generation still matches. Recovery/policy revocation retain their separate
  authority. A healthy scheduled owner can keep a silent request live; a sleeping
  or stopped owner cannot be assumed to renew. Wake after expiry follows recovery,
  not retroactive renewal or blind redispatch of uncertain effects.
- **One local ordering boundary.** Use
  `config_client::txn::MutationWriteGate` for authoritative reads through commit
  or rollback. Raw payload appends check the live lease but do not update it.
  Producer decisions retain their matching-generation request CAS without
  implicitly extending expiry. Recovery rereads the lease and swaps generation
  under this gate. A due renewal that wins before expiry prevents recovery;
  output alone does not. Lagging replicas observe, never decide owner expiry.
  Native conformance must establish the decision fence even when no deadline is
  changed: an ordinary snapshot read is not a substitute for the required
  conditional transaction ordering against a generation swap. The identity
  lease transition in Lean proves authorization, not DefraDB no-op-CAS behavior.
- **No new process authority.** The existing one-active-runtime-per-principal
  convention remains a boundary, not an enforcement theorem. Another process or
  remote merge may bypass the local gate. Conflicts remain visible; never pick a
  winning twin or silently discard evidence.
- **Integrity is separate from liveness.** Source append/publication/reconstruction
  still validate their own output. Invalid output does not corrupt the lease
  deadline or prevent its independent renewal. Composed integrity revocation uses
  the existing policy-revoke generation CAS to terminate as dead/superseded
  without reconstructing corrupt payloads. Exact immutable header/tool membership
  still fences which pending calls are cancelled and which running calls receive
  a reconciliation handoff. Preserve conflicting records as evidence; do not
  manufacture a successful closure or claim external effects stopped. Tool
  terminal acknowledgements remain independently deliverable after revocation.
- **Timestamps describe output, not renewal authority.** The owner stamps
  `created_at` at admission, keeps source timestamps nondecreasing, and reuses
  them on replay. They never extend the request lease. The model's clock remains
  nondecreasing; wall-clock discontinuities and suspend/resume are native
  refinement obligations, not assumptions that a timer ran during sleep.

A flush must contain a run ending on a UTF-8 boundary: it either opens a stream
(including a genuinely empty payload) or contributes nonempty bytes. Empty
continuations are not heartbeats. Output batching controls document counts;
renewal cadence controls request-row writes, independently.

The proof obligations are explicit-deadline admission, no renewal by output or
replay, bounded due-only deadline advancement, stale/expired renewal rejection,
and ordering renewal against recovery. Continued ownership during silent work is
conditional on the owner actually committing renewals before expiry; no
unconditional scheduler/fairness or host-process survival claim is made.

This deliberately replaces output-derived liveness. It retains the main storage
win without making every flush revalidate the request's entire output history.
Native implementation must still measure total reads, writes and gate hold time.

## Commit budget

Payload is no longer the cost; per-commit bookkeeping is (~1 KiB each in #1543).
The model is sized in documents per unit of work, and these are the targets the
benchmark holds it to:

| Work | Documents |
| --- | --- |
| Streaming flush | 1 segment, whatever number of streams advanced; 0 request rewrites |
| Due owner renewal | 0 output documents; 1 request lease CAS, independent of flush count |
| Provider turn that fits one batch interval | 1 final-flush record + 1 header (+ pending tool rows), one transaction |
| Whole authored or user message | 1 final-flush record + 1 header, one transaction |
| Tool result | output flushes (closure on the last) + 1 header at delivery; +1 terminal-only record if already flushed |
| Running background invocation receipt | 1 whole authored source with closure + 1 native-result header; separate from eventual output closure and completion notification |
| Truncation markers, separators, notification wrappers | 0: inline literals in the header's presentation |
| Retried attempt | +1 terminal-only Retracted record |
| Closure/recovery after the final payload was already flushed | +1 terminal-only record; no old segment update |

These count output documents, not all lifecycle writes. Closure/publication still
includes its existing owner CAS; combine required control writes in that transaction
where possible. Dispatch and tool terminalization retain their guards. Only plain
streaming flushes promise zero request rewrites. The extra closure document disappears
when final bytes and closure commit together, not for every possible source ending.

Measure read work as well as writes: records examined and bytes read per flush,
reconstruction and recovery; buffered bytes; and mutation-gate hold time as output
grows. A full-prefix rescan on every flush or preview must not turn append-only
writes into quadratic read work. Rebuildable local indexes/caches may accelerate
reads, but cannot replace authoritative recovery checks under the gate or introduce
a second durable progress record. Benchmark these costs in the implementation layer.

Still open, for the first measurement on real DefraDB: whether the segment
collection needs its `agent_did` / `requester_did` indexes or only the fields (reads
are `request_doc_id` scans), and the default batch interval and size threshold.

## Client execution and output visibility

The shared client projection takes request facts only; there is no response
lifecycle. Preserve the existing supersession override and unique retry-tip
selection, rather than choosing whichever attempt has visible output. The target
`ClientHeadProjection` mapping, after the supersession override, is:

| Request lifecycle | Client turn state |
| --- | --- |
| WorkspaceBindingPending, Pending | WaitingForClaim |
| Claimed, Processing | Running |
| Completed | Completed |
| Failed, Dead | Failed |
| Superseded | Superseded |
| Interrupted | Interrupted |

`request_state` retains the precise lifecycle for workspace/input-wait indicators.
`Running` replaces the misleading execution label `Streaming`; silent work is still
running. Shared reconstruction supplies live previews, missing-dependency loading,
integrity errors and published message completeness separately. Missing output
cannot demote completed execution; a Complete message cannot terminalize an active
request; a Partial message is not another failure status. A terminal NoMessage
selection does not wait for a nonexistent answer. Keep one shared projection, not
consumer-local status reconciliation. This is a target contract: update
`Proofs/Client.lean`, then conformance, then implement the replacement projection.

Known authorization denial of any required dependency is AccessDenied, not loading.
An absent query result alone establishes neither denial nor replication lag; keep it
unresolved unless the authorization owner reports denial. Hydration must propagate
known denial as rejection rather than retrying it as missing data. No parallel ACP
decision logic is introduced in reconstruction or the UI.

Complete reconstruction still requires every dependency of the requested message
to be present and valid, including the full referenced closed stream, even for a
head/tail presentation. Existing live previews may display while reconstruction is
incomplete; they do not certify hydration. Reconstructing one message does not
require unrelated session history. Full-session hydration retains its exact manifest
and authorized reference-closure requirements. No range-fetch protocol, second
hydration-completeness definition or selective-hydration optimization is added here;
measure large-output transfer costs before proposing one.

Live reconstruction selects the longest valid contiguous ordinal prefix, not all
currently visible flushes as one all-or-nothing extent. A later ordinal arriving
before an intermediate one must not erase the earlier preview. For the same source
and writer, benign immutable arrivals preserve each displayed stream's declaration
and byte prefix. A known closure bounds the preview and changes it to non-live
pending publication; bytes outside that extent remain inert. Conflicts, denial,
retraction, recovery narrowing and final presentation windows are explicit changes,
not promises that every eventual rendered string extends the preview. This is one
shared reconstruction rule, not another durable counter or consumer-local cache.

Hydration selection must consume the canonical manifest builder for the exact
admitted peer/requester/agent/session request, not a caller-supplied document set.
Every selected document, including headers and output segments, requires native
ACP authorization for that scope. Selected roots belong to the requested session;
authorized fork dependencies may cross session boundaries. Missing, denied or
conflicting observations cannot become a successfully served empty manifest.

## Deletion and ownership

| Deleted contract / implementation handoff | Surviving owner and obligation | Layer |
| --- | --- | --- |
| `AgentResponse` SDL, row and catalog entries (deleted) | `AgentRequest.lifecycle_state`, `failure_reason`, `terminalized_at` own terminal status/error/time; `InferenceCall` owns usage. `terminal_output` replaces final-message selection for session-message delivery. `interrupt_requested_at` remains intent; a `Partial` outcome describes kept output, not a second request status. | Spec |
| Response progress counters, cumulative text/reasoning writes, per-flush lease CAS, `execution_progress_seq` | `lifecycle/execution_lease.rs` keeps claim, bounded explicit owner renewal, and the matching-generation terminal/recovery CAS. `execution_lease_expires_at` alone determines liveness; output, publication and replay never renew. `watcher`, `lifecycle/recovery.rs`, `runtime_trace.rs` read that deadline, not payload history. Tool-owned output uses the existing tool lifecycle and never revives a terminal request. | Lean → conformance → runtime |
| Response `materialized_*`, response/request dual terminalization | `lifecycle/materialize.rs` and terminal owner: final header publication, terminal lifecycle and `TerminalOutput` selection commit atomically. The completion observer and session-message recovery resolve that exact scoped message. Missing selection/header/closure/segments is incomplete, never latest-message fallback. Explicit NoMessage handles pre-output failure. Recovery closes committed bytes with the original producer binding. | Lean → conformance → runtime |
| Stream-processor cumulative previews, in-flight message upserts, retraction resets | `agent/stream_processor.rs`: one immutable segment per flush, naming its writer and slicing its payload by stream; a Retracted closure commits before retry backoff. `agent/loop_stream.rs`: dispatch follows accepted closure/header publication with the boundaries above. `rendered_request/scope.rs` must allocate non-reused scopes across reclaim/restart. | Lean → conformance → runtime |
| Tool `args`, `result`, `partial_output_*`; `AgentToolResult` SDL/row (deleted) | `AgentToolCall` owns execution/delivery, created pending with the assistant header. The provider turn owns canonical argument bytes; the tool source owns output; tool terminalization closes empty and nonempty output before delivery. Existing completion notification owner composes authored wrappers by reference. | Spec → runtime |
| `truncation/spill.rs`, spill links and discarded-spill flags | Owned provider-input boundary writes `PresentedPayload`: exact UTF-8 output ranges plus inline literal markers/separators. Preserve head/tail behavior and line normalization; retrieval hints name the tool call. Unreferenced retained output is not proof of delivery. `read_tool_output` reads the original stream. | Lean → conformance → runtime |
| Tool-output ring storage and `read_tool_output` persisted/live/empty source dispatch | Read the exact physical tool's canonical open prefix or committed closed extent through shared reconstruction, then page those bytes. Registry loss cannot erase committed output; missing/conflicting facts are not an empty successful read. Retain generic paging guarantees and the separate host process-control registry, not a second authoritative payload store. | Lean → conformance → runtime |
| Legacy `decode_persisted_message` / `present_persisted_message` and fallback tests (deleted) | Shared strict reconstruction produces native `Message`; existing `present_message` remains a rendering function. Missing closing records, conflicts, malformed JSON/media and illegal role/block combinations fail explicitly. | Spec → runtime |
| `session/fork.rs` payload/tool/spill copies and spill remapping | Fork owner copies headers only, with `MessagePublication::Fork`, child-scoped message keys, unchanged numeric sequences in the child session and no live request membership. Readers enforce the same sequence preservation: independently valid copies cannot reorder history. Blocks keep the origin's closure references unchanged. Origin tool IDs remain provenance, not child executable rows. Compaction cursors still target retained child headers. | Lean → conformance → runtime |
| Session-only hydration completeness | Existing owner serves authorized header closure: every referenced closing record, fork origins and the segments within their extents. Terminal selections resolve exact headers. Immutable IDs bind output content; mutable request/tool observations retain their owner checks. Receipt format stays unchanged; missing dependencies remain incomplete, denied dependencies reject. | Spec → Lean → conformance → runtime |
| Response/spill desktop stores, queries, merge heuristics and CLI projections | Shared output reconstruction supplies native messages and live streams; request lifecycle supplies status. No consumer-local text repair or short-message fallback. Parent session removal cannot cascade into retained origin dependencies; no output GC is introduced here. | Consumers |
| `client_protocol::{ResponseStatus, InvalidResponseStatus, ResponseSnapshot}`, `AttemptView.response`, response-aware projection functions (deleted); client execution variant `Streaming` (renamed `Running`) | `client_protocol` retains request-only input/output types. `Proofs/Client.lean` and client conformance must adopt the mapping above before the shared projection is reimplemented. Desktop `store/turns.rs` and response indexes; CLI `codex_shim/{turn,subagent_projection,history_projection,thread_projection,progress}` consume it with no response fallback. Existing protocol tests are handoff evidence, not the new contract. Preserve retry-tip ambiguity rejection and supersession selection. | Spec → Lean → conformance → consumers |
| `streaming.rs::StreamBufferSnapshot` current/persisted cumulative copies; `agent/stream_processor.rs` intermediate accumulated-message persistence snapshots | Segment writer retains the uncommitted batch and stream/header bookkeeping, not cumulative copies for comparison and rewrite. Shared reconstruction owns persisted-output assembly. Native messages assembled for provider input remain legitimate; do not delete required provider context or structural metadata. Remove the snapshot/upsert machinery in the implementation layer. | Runtime |
| Compaction's separate `ResponseStatus`, `ResponseStatusIndex`, `AllTerminal`/`NoneKnown`, `session_has_live_response` gate (handoff, not yet deleted) | `compaction.rs`, `agent/daemon/request.rs` and the PromptView/PromptAssembly proofs must establish that the selected published-message prefix remains stable under later publication and provider-input sanitization. Immutable headers remove in-place mutation, not tool-pairing or reused-call-ID effects. Replace the response-status gate only after modeling its surviving guarantee; do not substitute request terminality or assume all headers are safe to compact. | Lean → conformance → runtime |
| Mailbox (retained, not an AgentResponse consumer) | `mailbox/reply.rs` consumes authenticated start-request replies at claim and records the request document. `mailbox.rs` resolves write-document items through correlated domain documents; ack remains explicit. None is redirected to final assistant messages. | No semantic change |

Open question for the consumer layer: `CLIENT_TO_RUNTIME_COLLECTIONS` carries
segment records because it carried messages and responses before. Identify
the client-authored transcript writer that needs this, and its `OutputWriter`,
or drop those collections from the client-to-runtime direction.

## Schema, pairing and hydration inventory

| Surface | This layer | Implementation handoff |
| --- | --- | --- |
| `gents-schemas/src/lib.rs`, `gents-protocol/src/schemas.rs`, runtime schema exports | Register segment only; remove separate seal, response and spill collections | Verify fresh schema registration and strict JSON/SDL decoding |
| `gents-migration/src/registry.rs` | Remove obsolete response/spill baselines; update client-authored catalog | Add the fresh segment root pin and regenerate changed request/message/tool pins with DefraDB; catalog coverage/parity remains required. No fabricated pins or relaxed checks |
| `agent/p2p_reconcile/{templates,profiles,policy}.rs` | Replace response/spill routes with segment, preserving requester/agent filters and route directions | Conformance for conversation, client (both directions), machine and operator routes; collection presence is not ACP authorization |
| `agent/p2p_reconcile/session_hydration.rs` | New collection inventory | Replace session-only selection with authorized reference closure; preserve membership, route admission and bounded exact push |
| `session_hydration_reconcile.rs` | Inventory only; old queries intentionally remain | Resolve closure references, enumerate twins and include origin dependencies in the signed exact manifest |
| `gents-protocol/src/session_hydration.rs` | Closed collection enum; receipt format unchanged | Server validates closure; desktop verifies signature, exact document delivery and header reconstruction before completion |
| Desktop `client/core/writes.rs`, `query/{session_transcript,document_patches}.rs`, `store/*`, observers | Inventory only; deleted row types break old readers | Follow origin references without subscribing to an entire parent session; project reordered header/segment arrival |
| `session/fork.rs` and session retention owners | Typed publication provenance and retained-reference contract | Preserve parent dependencies after parent close/removal, detach live membership, check ACP without copying payloads |

## Cross-layer validation surfaces

`Transcript`, `CompletionRetry`, `RequestExecutionLease`, `SessionFork`,
`SessionHydration`, `Client`, tool/background delivery and provider-input narrowing
define the coverage required across Lean, generated conformance and implementation.
The Lean proof map records the modeled contracts and remaining native premises.
Preserve coverage for: retraction without replacement bytes; stale/replayed writes; at most one
closure per source; late raw flushes beyond its extent are inert; header publication before tool
dispatch; background output after request termination; recovery publication;
empty streams and sources; a complete message over partial dependencies;
gaps and twins; headers arriving before closing records or segments; multi-stream
flushes and run/payload accounting; superseded unclosed writers; explicit lease
liveness decided only in the owner's write gate; recovery racing a due renewal, a flush, a
closure, an acceptance and a dispatch; inert late flushes; lagging replicas;
unheaded authored sources; exact terminal selection before header
arrival, explicit NoMessage and late background delivery; native block order and
signed reasoning; line-normalized presentation; fork authorization/retention and
reference closure. Dispatch cases include the boundaries above and replace the old
`mid_stream_failure_after_tool_ran_closes_turn_and_continues` expectation.

Client cases cover every lifecycle mapping above, silent running work, input wait,
terminal execution before output arrival, Complete/Partial messages during active
execution, explicit NoMessage, supersession and ambiguous retry tips. Output arrival
or absence alone never changes execution status. Preserve integrity errors as errors,
not a perpetual loading indicator. Regenerate the old response-aware client cases
after the model changes; do not claim the previous projection proof covers this one.

Recovery cases include text followed by incomplete argument JSON, missing reasoning
signatures/IDs, opaque reasoning, incomplete media, omitted-block position gaps,
text-free and explicitly empty-text sources, existing accepted headers and replay.
Distinguish retained diagnostics from native messages without changing saved bytes.
Producer decision/renewal cases include expiry before recovery has replaced the
generation and exact-deadline admission; recovery retains authority to finalize.
Renewal cases include silent foreground waits, too-early ticks, stale expected
deadlines, lost acknowledgement replay, and wake after missed expiry. Output,
including conflicted or future-timestamp output, cannot alter the lease deadline.
Authorization cases distinguish explicit denial for each dependency kind from
unavailable data, including a missing query result with no denial evidence.
Compaction cases cover later tool-result publication, background delivery and
reused provider call IDs changing sanitization of an otherwise immutable prefix.

Closure representation cases must include a combined final flush, closure after
an earlier flush, zero-byte streams, a zero-stream source, retraction, recovery
with a late ordinal-N flush, duplicate terminal records and a reference to a plain
flush. Decode absent/null terminal-only runs and payload without manufacturing
data. Expired-but-not-yet-recovered writers cannot renew themselves with new bytes.

Source inspection of pinned DefraDB `b77643076953a63bc7f8d39c4cd42e62a172e66d`
confirms `crates/db/src/block/builder/mod.rs::derive_doc_id` derives `_docID` from
the genesis composite CID; `builder/write.rs` hashes the encoded composite including
its field/signature/encryption links. Immutable IDs pin closure content, not referenced
segments without reconstruction. Identical genesis is stronger than identical text:
creation metadata, schema and cryptographic encoding matter.
`crates/db/src/write/autocommit/helpers.rs::register_created_doc` returns an
already-exists error for duplicate genesis, not successful replay. Writers resolve,
verify and reuse the persisted fact (including `created_at`), handle duplicate-create
races through the existing transaction owner, and surface coordinate conflicts.
Replay cannot invent fresh timestamps, change segment boundaries or renew semantic
progress. This is source-verified behavior; native conformance belongs to the later layer.

Lean remains free of `sorry`; generated cases then drive the real shared projection
and transaction owners. Runtime/desktop/CLI validation belongs to those descendants.
