# Canonical output (#1571)

Layer 1 is the breaking protocol/SDL specification. Baseline draft: `21ec9f8fd`.
Executable contracts follow in Lean, generated conformance, then implementation.
No builds or tests run in this layer. No compatibility readers, migration lenses,
historical schema copies, or data wipes are part of this stack.

## The model

Output is three immutable, create-only facts; nothing in it is ever updated.

| Fact | Collection | Says |
| --- | --- | --- |
| `OutputSegment` | `AgentOutputSegment` | These bytes, at `(request_doc_id, source, stream, ordinal)`. Ordinal zero declares what the stream is and where it sits in the native message. |
| `OutputSeal` | `AgentOutputSeal` | This source is finished: closed with the exact extent of every stream, or retracted. At most one per source. |
| `TranscriptMessage` | `AgentMessage` | This native message, in this session at this sequence, with each payload a `{seal_doc_id, stream}` reference. |

Every byte of model, tool and authored content is stored once, in a segment.
The collections only grow, so replication is set union and a reader only asks
which facts are visible: no seal means still open, a seal with missing segments
means an incomplete replica, twins mean a conflict. Storage grows with new
output plus one small seal per source and one header per message — never with
display-update frequency. Live views, transcripts, provider input, forks,
hydration and exports are projections through one shared reconstruction.

An assistant message is published when its provider turn seals, in one
transaction with the pending `AgentToolCall` rows it names, and before any of
those tools is dispatched. The assistant turn is therefore durable with its
sequence allocated before a tool can run or a background completion can append
(#945), without an in-flight message row.

## Deletion and ownership

| Deleted contract / implementation handoff | Surviving owner and obligation | Layer |
| --- | --- | --- |
| `AgentResponse` SDL, row and catalog entries (deleted) | `AgentRequest.lifecycle_state`, `failure_reason`, `terminalized_at` own terminal status/error/time, including the user-facing error text; `InferenceCall` owns usage. `interrupt_requested_at` remains intent, not proof of interruption; `OutputOutcome::Interrupted` on the seal is the proof. Subagent result delivery and mailbox resolution read the child's final published message, not a response row. | Spec |
| Response progress counters, cumulative text/reasoning writes | `lifecycle/execution_lease.rs`: fresh request-owned segment progress renews the existing lease atomically; exact replay does not. Tool-owned output uses the existing tool lifecycle and never revives a terminal request. | Lean → conformance → runtime |
| Response `materialized_*`, response/request dual terminalization | `lifecycle/materialize.rs` and execution terminal owner: seal, then publish headers by existing message key/sequence; terminalization and required final publication commit atomically. Recovery seals only committed partial bytes under the winning CAS. Empty output still has a sealed outcome. | Lean → conformance → runtime |
| Stream-processor cumulative previews, in-flight message upserts, retraction resets | `agent/stream_processor.rs`: batched immutable segments with the declaration on ordinal zero; a `Retracted` seal is committed before retry backoff, even when the replacement emits nothing. `agent/loop_stream.rs`: tool dispatch moves from mid-stream to after the turn's seal and header publication (dispatch is already awaited inline, so no parallelism is lost). `rendered_request/scope.rs` must allocate non-reused scopes across reclaim/restart. | Lean → conformance → runtime |
| Tool `args`, `result`, `partial_output_*`; `AgentToolResult` SDL/row (deleted) | `AgentToolCall` owns execution/delivery only and is created pending with the assistant header. The provider turn owns argument bytes; the tool source owns output; tool terminalization seals empty and nonempty output before delivery. Existing completion notification owner composes authored wrappers by reference. | Spec → runtime |
| `truncation/spill.rs`, spill links and discarded-spill flags | Owned provider-input boundary writes `PresentedPayload`: exact UTF-8 output ranges plus authored marker/separator streams. Preserve head/tail behavior and line normalization; retrieval hints name the tool call. Unreferenced retained output is not proof of delivery. `read_tool_output` reads the original stream. | Lean → conformance → runtime |
| Legacy `decode_persisted_message` / `present_persisted_message` and fallback tests (deleted) | Shared strict reconstruction produces native `Message`; existing `present_message` remains a rendering function. Missing seals, conflicts, malformed JSON/media and illegal role/block combinations fail explicitly. | Spec → runtime |
| `session/fork.rs` payload/tool/spill copies and spill remapping | Fork owner copies headers only, with `MessagePublication::Fork`, child-scoped message keys/sequences and no live request membership. Blocks keep the origin's seal references unchanged. Origin tool IDs remain provenance, not child executable rows. Compaction cursors still target retained child headers. | Lean → conformance → runtime |
| Session-only hydration completeness | Existing hydration owner serves the authorized reference closure of the served headers: every referenced seal (including presentation pieces and fork origins) and the segments within its extents, under existing ACP. The signed manifest of immutable document identities binds the content; the receipt format is unchanged. Client completion requires each header to reconstruct. Denied dependencies reject; missing dependencies stay incomplete. | Spec → Lean → conformance → runtime |
| Response/spill desktop stores, queries, merge heuristics and CLI projections | Shared output reconstruction supplies native messages and live streams; request lifecycle supplies status. No consumer-local text repair or short-message fallback. Parent session removal cannot cascade into retained origin dependencies; no output GC is introduced here. | Consumers |

Open question for the consumer layer: `CLIENT_TO_RUNTIME_COLLECTIONS` carries
segments and seals because it carried messages and responses before. Identify
the client-authored transcript writer that needs this, and its `OutputWriter`,
or drop those collections from the client-to-runtime direction.

## Schema, pairing and hydration inventory

| Surface | This layer | Implementation handoff |
| --- | --- | --- |
| `gents-schemas/src/lib.rs`, `gents-protocol/src/schemas.rs`, runtime schema exports | Register seal/segment; remove response/spill | Verify fresh schema registration and strict JSON/SDL decoding |
| `gents-migration/src/registry.rs` | Remove obsolete response/spill baselines; update client-authored catalog | Add fresh seal/segment root pins and regenerate changed message/tool pins with DefraDB; catalog coverage/parity remains required. No fabricated pins or relaxed checks |
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
empty streams; gaps and twins; headers arriving before seals or segments;
native block order and signed reasoning; line-normalized presentation; fork
authorization/retention and reference closure.

Verify against the pinned DefraDB before Lean relies on it: a create-only
document's `_docID` derives from its genesis content (#1425), so an identical
replay collides on the same identity and a conflicting twin gets a different
one, and a `seal_doc_id` reference pins the seal's content.

Lean remains free of `sorry`; generated cases then drive the real shared projection
and transaction owners. Runtime/desktop/CLI validation belongs to those descendants.
