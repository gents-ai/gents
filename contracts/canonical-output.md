# Canonical output (#1571)

Layer 1 is the breaking protocol/SDL specification. Baseline draft: `21ec9f8fd`.
Executable contracts follow in Lean, generated conformance, then implementation.
No builds or tests run in this layer. No compatibility readers, migration lenses,
historical schema copies, or data wipes are part of this stack.

## The model

Output is three immutable, create-only facts; nothing in it is ever updated.

| Fact | Collection | Says |
| --- | --- | --- |
| `OutputSegment` | `AgentOutputSegment` | These bytes, at `(request_doc_id, source, stream, ordinal)`. Ordinal zero declares the original writer, role, payload kind and native position. |
| `OutputSeal` | `AgentOutputSeal` | This source is finished: closed with the exact extent of every stream, or retracted. At most one per source. |
| `TranscriptMessage` | `AgentMessage` | This native message, with one governing `outcome_seal_doc_id` and payload `{seal_doc_id, stream}` references. |

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
header and pending tool rows before dispatch. Interrupted/failed partial headers
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

Each header's governing Closed seal supplies its outcome, including a zero-stream
seal for empty or URL-only content. Other seals are dependencies: a complete authored
notification can describe a failed tool without becoming a failed message. Forks
keep the governing seal. `AgentRequest.terminal_output` separately selects the exact
final assistant header (or explicit `NoMessage`) with terminalization. Late background
messages cannot change that selection.

## Deletion and ownership

| Deleted contract / implementation handoff | Surviving owner and obligation | Layer |
| --- | --- | --- |
| `AgentResponse` SDL, row and catalog entries (deleted) | `AgentRequest.lifecycle_state`, `failure_reason`, `terminalized_at` own terminal status/error/time; `InferenceCall` owns usage. `terminal_output` replaces final-message selection for subagent delivery. `interrupt_requested_at` remains intent; a governing Interrupted seal describes partial output, not a second request status. | Spec |
| Response progress counters, cumulative text/reasoning writes | `lifecycle/execution_lease.rs`: fresh request-owned segment progress renews the existing lease atomically; exact replay does not. Tool-owned output uses the existing tool lifecycle and never revives a terminal request. | Lean → conformance → runtime |
| Response `materialized_*`, response/request dual terminalization | `lifecycle/materialize.rs` and terminal owner: final header publication, terminal lifecycle and `TerminalOutput` selection commit atomically. `background_tools.rs::load_child_final_response` and bridge recovery resolve that exact scoped message. Missing selection/header/seal/segments is incomplete, never latest-message fallback. Explicit NoMessage handles pre-output failure. Recovery seals committed bytes with the original producer binding. | Lean → conformance → runtime |
| Stream-processor cumulative previews, in-flight message upserts, retraction resets | `agent/stream_processor.rs`: batched immutable segments with producer binding on ordinal zero; a Retracted seal commits before retry backoff. `agent/loop_stream.rs`: dispatch follows accepted seal/header publication with the boundaries above. `rendered_request/scope.rs` must allocate non-reused scopes across reclaim/restart. | Lean → conformance → runtime |
| Tool `args`, `result`, `partial_output_*`; `AgentToolResult` SDL/row (deleted) | `AgentToolCall` owns execution/delivery only and is created pending with the assistant header. The provider turn owns argument bytes; the tool source owns output; tool terminalization seals empty and nonempty output before delivery. Existing completion notification owner composes authored wrappers by reference. | Spec → runtime |
| `truncation/spill.rs`, spill links and discarded-spill flags | Owned provider-input boundary writes `PresentedPayload`: exact UTF-8 output ranges plus authored marker/separator streams. Preserve head/tail behavior and line normalization; retrieval hints name the tool call. Unreferenced retained output is not proof of delivery. `read_tool_output` reads the original stream. | Lean → conformance → runtime |
| Legacy `decode_persisted_message` / `present_persisted_message` and fallback tests (deleted) | Shared strict reconstruction produces native `Message`; existing `present_message` remains a rendering function. Missing seals, conflicts, malformed JSON/media and illegal role/block combinations fail explicitly. | Spec → runtime |
| `session/fork.rs` payload/tool/spill copies and spill remapping | Fork owner copies headers only, with `MessagePublication::Fork`, child-scoped message keys/sequences and no live request membership. Blocks keep the origin's seal references unchanged. Origin tool IDs remain provenance, not child executable rows. Compaction cursors still target retained child headers. | Lean → conformance → runtime |
| Session-only hydration completeness | Existing owner serves authorized header closure: governing seals even with no payloads, payload/presentation seals, fork origins and segments. Terminal selections resolve exact headers. Immutable IDs bind output content; mutable request/tool observations retain their owner checks. Receipt format stays unchanged; missing dependencies remain incomplete, denied dependencies reject. | Spec → Lean → conformance → runtime |
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
empty streams and zero-stream governing seals; mixed dependency outcomes;
gaps and twins; headers arriving before seals or segments; missing ordinal-zero
bindings and superseded unsealed writers; exact terminal selection before header
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
