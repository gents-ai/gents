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

Provider/tool payloads and whole authored content live in segments. Small runtime
presentation literals live inline in headers; they do not duplicate payload bytes.
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

`OutputOutcome` is completeness only: `Complete` or `Partial`. Why output was cut
short, and whether the request or tool succeeded, stay with the request and tool
lifecycles and are not classified a second time. The publisher sets a header's
outcome directly; it is never derived from the closing records its blocks reference, so a
complete notification can wrap a tool's partial output and a message with no
payloads needs no closure. `AgentRequest.terminal_output` separately selects the exact
final assistant header (or explicit `NoMessage`) with terminalization. Late background
messages cannot change that selection.

## Progress is stored once

A segment is the progress fact. Streaming no longer rewrites `AgentRequest`: the
per-flush `(generation, expiry, progress_seq)` CAS — 42k versions of one field over
85 requests in #1543 — is retired along with `execution_progress_seq`. This is a
specification hypothesis until Lean proves the ordering below; it is not yet a
claim that the amplifier is safely gone.

What the old write bought was an ordering: a progress CAS and recovery's CAS
conflicted, so exactly one won. Removing the write must not remove the ordering.

- **Raw payload is the only unfenced write.** A flush commits inside the owning
  runtime's existing mutation write gate without touching the request. Admission
  rereads the generation, lifecycle and effective expiry there: a writer already
  expired cannot revive itself with a fresh timestamp before recovery swaps it.
  This is a guarded insert, not a request-row progress CAS. If a late raw write
  loses a race with recovery's generation swap it is inert: it names a superseded
  writer, renews nothing, and lies beyond the extent recovery closed.
- **Everything that decides keeps the matching-generation CAS on the request:**
  closing or retracting a source, accepting and publishing a turn (closure, header,
  pending tool rows), dispatch, terminalization, recovery. These happen once per
  lifecycle decision, not once per flush. Fresh nonterminal decisions can renew;
  terminalization and exact replay cannot. A
  superseded writer therefore cannot close a source recovery also closes, publish
  executable tool intent, or dispatch; it learns it lost at its next decision.
- **The liveness decision is authoritative in one place.** Only the request's
  owning runtime decides expiry, reading its own store, inside the same write gate
  that orders flush commits, and swaps the generation under that gate. A flush is
  either visible to the decision or ordered after the swap; recovery never
  terminalizes on a snapshot a concurrent flush invalidated. A lagging replica
  never expires work because fresh segments have not arrived; it only observes.
  This relies on the one-active-runtime-per-principal convention exactly as far
  as the existing gate already does; it adds no host identity or second lease.
- **Name and scope of the gate:** `config_client::txn::MutationWriteGate`, held
  from before the authoritative transaction/read through commit or rollback.
  `streaming.rs::response_write_gate` currently orders response operations only;
  it is not an existing shared recovery fence. Route the new output/recovery
  owners through the canonical transaction gate, not a copied mutex. Expiry
  candidates found outside the gate are hints and must be reread inside it.
  A second process/native handle or remote merge bypassing this gate has no
  serialization guarantee from the mutex; Lean and native conformance must state
  this boundary. Do not claim cross-replica consensus or predicate locking.
  Tool-owned closure uses existing tool terminal/delivery guards; it cannot
  require or renew an already-terminal originating request.
- **`created_at` carries authority, so it is defined.** The gate stamps it from
  the owning runtime's clock when admitting a fresh write under the gate; only
  successful commits count. It uses the same clock as claim deadlines and the
  expiry decision. It is non-decreasing within a source, and a
  replay reuses the stored value rather than minting a new one.
- **Claim** installs `execution_generation`, `execution_lease_secs` and the claim's
  own `execution_lease_expires_at`. The existing owner writes that field again only
  at the per-turn decisions above and for progress that produces no output fact (a
  long silent tool, a wait).

Effective expiry is `max(execution_lease_expires_at, newest eligible committed
fact.created_at + execution_lease_secs)`. Eligibility requires the exact physical
request/current generation and valid owner scope; exclude replay, forks, tool-owned
facts, beyond-extent late flushes and malformed/conflicting records. An authoritative
conflict is an integrity failure, not evidence of inactivity. Monotonic elapsed-time
and wall-clock discontinuity assumptions must be explicit in the later model;
created_at is an admission timestamp, not a database-assigned commit timestamp.

The contract Lean states is safety and liveness, not deadline equality. Today's
renewal is `max(now + duration, previous_deadline + 1ms)`; a maximum over output
timestamps is not numerically identical even without concurrency, and the model
must expose that difference rather than assume it away.

- *Safety:* recovery never supersedes a generation that, in the gate's order,
  committed an output fact or explicit renewal within `execution_lease_secs`
  before the decision. At most one of {producer, recovery} closes a source; a
  superseded generation never publishes, dispatches or terminalizes; exact replay
  renews nothing.
- *Liveness:* a generation with no such fact for `execution_lease_secs` becomes
  recoverable, and recovery closes exactly the committed extent.

Fallback if a property fails: explicit renewal at a slow fixed cadence. That is
not a timer alone — it needs the same two pieces, a bounded expiry policy and the
gate-plus-CAS recovery ordering above — and it never returns to per-flush rewrites.

## Commit budget

Payload is no longer the cost; per-commit bookkeeping is (~1 KiB each in #1543).
The model is sized in documents per unit of work, and these are the targets the
benchmark holds it to:

| Work | Documents |
| --- | --- |
| Streaming flush | 1 segment, whatever number of streams advanced; 0 request rewrites |
| Provider turn that fits one batch interval | 1 final-flush record + 1 header (+ pending tool rows), one transaction |
| Whole authored or user message | 1 final-flush record + 1 header, one transaction |
| Tool result | output flushes (closure on the last) + 1 header at delivery; +1 terminal-only record if already flushed |
| Truncation markers, separators, notification wrappers | 0: inline literals in the header's presentation |
| Retried attempt | +1 terminal-only Retracted record |
| Closure/recovery after the final payload was already flushed | +1 terminal-only record; no old segment update |

These count output documents, not all lifecycle writes. Closure/publication still
includes its existing owner CAS; combine required control writes in that transaction
where possible. Dispatch and tool terminalization retain their guards. Only plain
streaming flushes promise zero request rewrites. The extra closure document disappears
when final bytes and closure commit together, not for every possible source ending.

Still open, for the first measurement on real DefraDB: whether the segment
collection needs its `agent_did` / `requester_did` indexes or only the fields (reads
are `request_doc_id` scans), and the default batch interval and size threshold.

## Deletion and ownership

| Deleted contract / implementation handoff | Surviving owner and obligation | Layer |
| --- | --- | --- |
| `AgentResponse` SDL, row and catalog entries (deleted) | `AgentRequest.lifecycle_state`, `failure_reason`, `terminalized_at` own terminal status/error/time; `InferenceCall` owns usage. `terminal_output` replaces final-message selection for subagent delivery. `interrupt_requested_at` remains intent; a `Partial` outcome describes kept output, not a second request status. | Spec |
| Response progress counters, cumulative text/reasoning writes, per-flush lease CAS, `execution_progress_seq` | `lifecycle/execution_lease.rs` keeps claim, explicit byte-less renewal, and the matching-generation terminal/recovery CAS. Liveness is derived from the current generation's newest output fact (see *Progress is stored once*); exact replay creates nothing and so renews nothing. `watcher`, `lifecycle/recovery.rs`, `runtime_trace.rs` read derived liveness instead of the counter. Tool-owned output uses the existing tool lifecycle and never revives a terminal request. | Lean → conformance → runtime |
| Response `materialized_*`, response/request dual terminalization | `lifecycle/materialize.rs` and terminal owner: final header publication, terminal lifecycle and `TerminalOutput` selection commit atomically. `background_tools.rs::load_child_final_response` and bridge recovery resolve that exact scoped message. Missing selection/header/closure/segments is incomplete, never latest-message fallback. Explicit NoMessage handles pre-output failure. Recovery closes committed bytes with the original producer binding. | Lean → conformance → runtime |
| Stream-processor cumulative previews, in-flight message upserts, retraction resets | `agent/stream_processor.rs`: one immutable segment per flush, naming its writer and slicing its payload by stream; a Retracted closure commits before retry backoff. `agent/loop_stream.rs`: dispatch follows accepted closure/header publication with the boundaries above. `rendered_request/scope.rs` must allocate non-reused scopes across reclaim/restart. | Lean → conformance → runtime |
| Tool `args`, `result`, `partial_output_*`; `AgentToolResult` SDL/row (deleted) | `AgentToolCall` owns execution/delivery only and is created pending with the assistant header. The provider turn owns argument bytes; the tool source owns output; tool terminalization closes empty and nonempty output before delivery. Existing completion notification owner composes authored wrappers by reference. | Spec → runtime |
| `truncation/spill.rs`, spill links and discarded-spill flags | Owned provider-input boundary writes `PresentedPayload`: exact UTF-8 output ranges plus inline literal markers/separators. Preserve head/tail behavior and line normalization; retrieval hints name the tool call. Unreferenced retained output is not proof of delivery. `read_tool_output` reads the original stream. | Lean → conformance → runtime |
| Legacy `decode_persisted_message` / `present_persisted_message` and fallback tests (deleted) | Shared strict reconstruction produces native `Message`; existing `present_message` remains a rendering function. Missing closing records, conflicts, malformed JSON/media and illegal role/block combinations fail explicitly. | Spec → runtime |
| `session/fork.rs` payload/tool/spill copies and spill remapping | Fork owner copies headers only, with `MessagePublication::Fork`, child-scoped message keys/sequences and no live request membership. Blocks keep the origin's closure references unchanged. Origin tool IDs remain provenance, not child executable rows. Compaction cursors still target retained child headers. | Lean → conformance → runtime |
| Session-only hydration completeness | Existing owner serves authorized header closure: every referenced closing record, fork origins and the segments within their extents. Terminal selections resolve exact headers. Immutable IDs bind output content; mutable request/tool observations retain their owner checks. Receipt format stays unchanged; missing dependencies remain incomplete, denied dependencies reject. | Spec → Lean → conformance → runtime |
| Response/spill desktop stores, queries, merge heuristics and CLI projections | Shared output reconstruction supplies native messages and live streams; request lifecycle supplies status. No consumer-local text repair or short-message fallback. Parent session removal cannot cascade into retained origin dependencies; no output GC is introduced here. | Consumers |
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
| Subagent pairing templates | Host return leg includes segment with requester filters | Bridge delivery of a delegated call's arguments requires exact authorized dependency delivery to the host; do not broaden the bridge-only route to all parent requests or history |
| `agent/p2p_reconcile/session_hydration.rs` | New collection inventory | Replace session-only selection with authorized reference closure; preserve membership, route admission and bounded exact push |
| `session_hydration_reconcile.rs` | Inventory only; old queries intentionally remain | Resolve closure references, enumerate twins and include origin dependencies in the signed exact manifest |
| `gents-protocol/src/session_hydration.rs` | Closed collection enum; receipt format unchanged | Server validates closure; desktop verifies signature, exact document delivery and header reconstruction before completion |
| Desktop `client/core/writes.rs`, `query/{session_transcript,document_patches}.rs`, `store/*`, observers | Inventory only; deleted row types break old readers | Follow origin references without subscribing to an entire parent session; project reordered header/segment arrival |
| `session/fork.rs` and session retention owners | Typed publication provenance and retained-reference contract | Preserve parent dependencies after parent close/removal, detach live membership, check ACP without copying payloads |

## Next layer's model surfaces

`Transcript`, `CompletionRetry`, `RequestExecutionLease`, `SessionFork`,
`SessionHydration`, tool/background delivery and provider-input narrowing must
cover: retraction without replacement bytes; stale/replayed writes; at most one
closure per source; late raw flushes beyond its extent are inert; header publication before tool
dispatch; background output after request termination; recovery publication;
empty streams and sources; a complete message over partial dependencies;
gaps and twins; headers arriving before closing records or segments; multi-stream
flushes and run/payload accounting; superseded unclosed writers; derived lease
liveness decided only in the owner's write gate; recovery racing a flush, a
closure, an acceptance and a dispatch; inert late flushes; lagging replicas;
unheaded authored sources; exact terminal selection before header
arrival, explicit NoMessage and late background delivery; native block order and
signed reasoning; line-normalized presentation; fork authorization/retention and
reference closure. Dispatch cases include the boundaries above and replace the old
`mid_stream_failure_after_tool_ran_closes_turn_and_continues` expectation.

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
