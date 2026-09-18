# Canonical output (#1571)

Layer 1 is the breaking protocol/SDL specification. Baseline draft: `21ec9f8fd`.
Executable contracts follow in Lean, generated conformance, then implementation.
No builds or tests run in this layer. No compatibility readers, migration lenses,
historical schema copies, or data wipes are part of this stack.

## The model

Output is three immutable, create-only facts; nothing in it is ever updated.

| Fact | Collection | Says |
| --- | --- | --- |
| `OutputSegment` | `AgentOutputSegment` | One flush of a source, at `(request_doc_id, source, ordinal)`: who wrote it, and the bytes that arrived since the last flush, sliced by stream. The run that opens a stream declares its payload kind and native position. |
| `OutputSeal` | `AgentOutputSeal` | This source is finished: closed (complete or partial) with its exact extent, or retracted. At most one per source. |
| `TranscriptMessage` | `AgentMessage` | This native message, published complete or partial, with payload `{seal_doc_id, stream}` references. |

Every byte of model, tool and authored content is stored once, in a segment.
The collections only grow, so replication is set union and a reader only asks
which facts are visible: no visible seal means closure is unknown, a seal with
missing segments means an incomplete replica, twins mean a conflict. Unsealed
output also needs a matching current request generation or eligible tool owner;
absence of a seal never proves liveness. Storage grows with new
output plus one small seal per source and one header per message — never with
display-update frequency. Live views, transcripts, provider input, forks,
hydration and exports are projections through one shared reconstruction.

An accepted provider turn atomically publishes its Complete seal, assistant
header and pending tool rows before dispatch. Partial headers
may retain tool-call provenance, but only with terminal, nondispatchable rows.
Dispatch still checks existing request/tool cancellation, deadline and policy
owners. Publication is durable intent, not permission to execute after cancellation.

| Boundary | Required behavior |
| --- | --- |
| Provider fails before acceptance/publication | No tool from the attempt has run. Existing retry policy may retract/resample; terminal partial output cannot dispatch. Usage still charges every reported attempt. |
| Publication fails or its commit acknowledgement is lost | No dispatch until the owner confirms the exact seal/header/tool rows committed. Replay reuses those facts. |
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
outcome directly; it is never derived from the seals its blocks reference, so a
complete notification can wrap a tool's partial output and a message with no
payloads needs no seal. `AgentRequest.terminal_output` separately selects the exact
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
  runtime's existing execution write gate without touching the request. If it
  loses a race with recovery's generation swap it is inert: it names a superseded
  writer, renews nothing, and lies beyond the extent recovery closed.
- **Everything that decides keeps the matching-generation CAS on the request:**
  closing or retracting a source, accepting and publishing a turn (seal, header,
  pending tool rows), dispatch, terminalization, recovery. These happen once per
  turn, not once per flush, and each doubles as that turn's explicit renewal. A
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
- **`created_at` carries authority, so it is defined.** The gate stamps it from
  the owning runtime's clock at commit — the clock that stamps claim deadlines and
  that the decision compares against. It is non-decreasing within a source, and a
  replay reuses the stored value rather than minting a new one.
- **Claim** installs `execution_generation`, `execution_lease_secs` and the claim's
  own `execution_lease_expires_at`. The existing owner writes that field again only
  at the per-turn decisions above and for progress that produces no output fact (a
  long silent tool, a wait).

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
| Provider turn that fits one batch interval | 1 segment + 1 seal + 1 header (+ pending tool rows), one transaction |
| Whole authored or user message | 1 segment + 1 seal + 1 header, one transaction |
| Tool result | its output segments + 1 seal at terminalization + 1 header at delivery |
| Truncation markers, separators, notification wrappers | 0: inline literals in the header's presentation |
| Retried attempt | + 1 retracted seal |

Still open, for the first measurement on real DefraDB: whether the segment
collection needs its `agent_did` / `requester_did` indexes or only the fields (reads
are `request_doc_id` scans), and the default batch interval and size threshold.

## Deletion and ownership

| Deleted contract / implementation handoff | Surviving owner and obligation | Layer |
| --- | --- | --- |
| `AgentResponse` SDL, row and catalog entries (deleted) | `AgentRequest.lifecycle_state`, `failure_reason`, `terminalized_at` own terminal status/error/time; `InferenceCall` owns usage. `terminal_output` replaces final-message selection for subagent delivery. `interrupt_requested_at` remains intent; a `Partial` outcome describes kept output, not a second request status. | Spec |
| Response progress counters, cumulative text/reasoning writes, per-flush lease CAS, `execution_progress_seq` | `lifecycle/execution_lease.rs` keeps claim, explicit byte-less renewal, and the matching-generation terminal/recovery CAS. Liveness is derived from the current generation's newest output fact (see *Progress is stored once*); exact replay creates nothing and so renews nothing. `watcher`, `lifecycle/recovery.rs`, `runtime_trace.rs` read derived liveness instead of the counter. Tool-owned output uses the existing tool lifecycle and never revives a terminal request. | Lean → conformance → runtime |
| Response `materialized_*`, response/request dual terminalization | `lifecycle/materialize.rs` and terminal owner: final header publication, terminal lifecycle and `TerminalOutput` selection commit atomically. `background_tools.rs::load_child_final_response` and bridge recovery resolve that exact scoped message. Missing selection/header/seal/segments is incomplete, never latest-message fallback. Explicit NoMessage handles pre-output failure. Recovery seals committed bytes with the original producer binding. | Lean → conformance → runtime |
| Stream-processor cumulative previews, in-flight message upserts, retraction resets | `agent/stream_processor.rs`: one immutable segment per flush, naming its writer and slicing its payload by stream; a Retracted seal commits before retry backoff. `agent/loop_stream.rs`: dispatch follows accepted seal/header publication with the boundaries above. `rendered_request/scope.rs` must allocate non-reused scopes across reclaim/restart. | Lean → conformance → runtime |
| Tool `args`, `result`, `partial_output_*`; `AgentToolResult` SDL/row (deleted) | `AgentToolCall` owns execution/delivery only and is created pending with the assistant header. The provider turn owns argument bytes; the tool source owns output; tool terminalization seals empty and nonempty output before delivery. Existing completion notification owner composes authored wrappers by reference. | Spec → runtime |
| `truncation/spill.rs`, spill links and discarded-spill flags | Owned provider-input boundary writes `PresentedPayload`: exact UTF-8 output ranges plus inline literal markers/separators. Preserve head/tail behavior and line normalization; retrieval hints name the tool call. Unreferenced retained output is not proof of delivery. `read_tool_output` reads the original stream. | Lean → conformance → runtime |
| Legacy `decode_persisted_message` / `present_persisted_message` and fallback tests (deleted) | Shared strict reconstruction produces native `Message`; existing `present_message` remains a rendering function. Missing seals, conflicts, malformed JSON/media and illegal role/block combinations fail explicitly. | Spec → runtime |
| `session/fork.rs` payload/tool/spill copies and spill remapping | Fork owner copies headers only, with `MessagePublication::Fork`, child-scoped message keys/sequences and no live request membership. Blocks keep the origin's seal references unchanged. Origin tool IDs remain provenance, not child executable rows. Compaction cursors still target retained child headers. | Lean → conformance → runtime |
| Session-only hydration completeness | Existing owner serves authorized header closure: every referenced seal, fork origins and the segments within their extents. Terminal selections resolve exact headers. Immutable IDs bind output content; mutable request/tool observations retain their owner checks. Receipt format stays unchanged; missing dependencies remain incomplete, denied dependencies reject. | Spec → Lean → conformance → runtime |
| Response/spill desktop stores, queries, merge heuristics and CLI projections | Shared output reconstruction supplies native messages and live streams; request lifecycle supplies status. No consumer-local text repair or short-message fallback. Parent session removal cannot cascade into retained origin dependencies; no output GC is introduced here. | Consumers |
| Mailbox (retained, not an AgentResponse consumer) | `mailbox/reply.rs` consumes authenticated start-request replies at claim and records the request document. `mailbox.rs` resolves write-document items through correlated domain documents; ack remains explicit. None is redirected to final assistant messages. | No semantic change |

Open question for the consumer layer: `CLIENT_TO_RUNTIME_COLLECTIONS` carries
segments and seals because it carried messages and responses before. Identify
the client-authored transcript writer that needs this, and its `OutputWriter`,
or drop those collections from the client-to-runtime direction.

## Schema, pairing and hydration inventory

| Surface | This layer | Implementation handoff |
| --- | --- | --- |
| `gents-schemas/src/lib.rs`, `gents-protocol/src/schemas.rs`, runtime schema exports | Register seal/segment; remove response/spill | Verify fresh schema registration and strict JSON/SDL decoding |
| `gents-migration/src/registry.rs` | Remove obsolete response/spill baselines; update client-authored catalog | Add fresh seal/segment root pins and regenerate changed request/message/tool pins with DefraDB; catalog coverage/parity remains required. No fabricated pins or relaxed checks |
| `agent/p2p_reconcile/{templates,profiles,policy}.rs` | Replace response/spill routes with seal/segment, preserving requester/agent filters and route directions | Conformance for conversation, client (both directions), machine and operator routes; collection presence is not ACP authorization |
| Subagent pairing templates | Host return leg includes seal and segment with requester filters | Bridge delivery of a delegated call's arguments requires exact authorized dependency delivery to the host; do not broaden the bridge-only route to all parent requests or history |
| `agent/p2p_reconcile/session_hydration.rs` | New collection inventory | Replace session-only selection with authorized reference closure; preserve membership, route admission and bounded exact push |
| `session_hydration_reconcile.rs` | Inventory only; old queries intentionally remain | Resolve seal references, enumerate twins and include origin dependencies in the signed exact manifest |
| `gents-protocol/src/session_hydration.rs` | Closed collection enum; receipt format unchanged | Server validates closure; desktop verifies signature, exact document delivery and header reconstruction before completion |
| Desktop `client/core/writes.rs`, `query/{session_transcript,document_patches}.rs`, `store/*`, observers | Inventory only; deleted row types break old readers | Follow origin references without subscribing to an entire parent session; project reordered header/seal/segment arrival |
| `session/fork.rs` and session retention owners | Typed publication provenance and retained-reference contract | Preserve parent dependencies after parent close/removal, detach live membership, check ACP without copying payloads |

## Next layer's model surfaces

`Transcript`, `CompletionRetry`, `RequestExecutionLease`, `SessionFork`,
`SessionHydration`, tool/background delivery and provider-input narrowing must
cover: retraction without replacement bytes; stale/replayed writes; at most one
seal per source and no segment after it; header publication before tool
dispatch; background output after request termination; recovery publication;
empty streams and sources; a complete message over partial dependencies;
gaps and twins; headers arriving before seals or segments; multi-stream
flushes and run/payload accounting; superseded unsealed writers; derived lease
liveness decided only in the owner's write gate; recovery racing a flush, a
closure, an acceptance and a dispatch; inert late flushes; lagging replicas;
unheaded authored sources; exact terminal selection before header
arrival, explicit NoMessage and late background delivery; native block order and
signed reasoning; line-normalized presentation; fork authorization/retention and
reference closure. Dispatch cases include the boundaries above and replace the old
`mid_stream_failure_after_tool_ran_closes_turn_and_continues` expectation.

Source inspection of pinned DefraDB `b77643076953a63bc7f8d39c4cd42e62a172e66d`
confirms `crates/db/src/block/builder/mod.rs::derive_doc_id` derives `_docID` from
the genesis composite CID; `builder/write.rs` hashes the encoded composite including
its field/signature/encryption links. Immutable IDs pin seal content, not referenced
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
