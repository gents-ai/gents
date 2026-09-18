# Canonical output (#1571)

Layer 1 is the breaking protocol/SDL specification. Baseline draft: `21ec9f8fd`.
Executable contracts follow in Lean, generated conformance, then implementation.
No builds or tests run in this layer. No compatibility readers, migration lenses,
historical schema copies, or data wipes are part of this stack.

## Deletion and ownership

| Deleted contract / implementation handoff | Surviving owner and obligation | Layer |
| --- | --- | --- |
| `AgentResponse` SDL, row and catalog entries (deleted) | `AgentRequest.lifecycle_state`, `failure_reason`, `terminalized_at` own terminal status/error/time; `InferenceCall` owns usage. `interrupt_requested_at` remains intent, not proof of interruption. | Spec |
| Response progress counters, cumulative text/reasoning writes | `lifecycle/execution_lease.rs`: fresh request-owned segment progress renews the existing lease atomically; exact replay does not. Tool-owned output uses the existing tool lifecycle and never revives a terminal request. | Lean → conformance → runtime |
| Response `materialized_*`, response/request dual terminalization | `lifecycle/materialize.rs` and execution terminal owner: publish sealed headers by existing message key/sequence; terminalization and required final publication commit atomically. Recovery seals only committed partial bytes under the winning CAS. Empty output still has a terminal outcome. | Lean → conformance → runtime |
| Stream-processor cumulative previews, in-flight message upserts, retraction resets | `agent/stream_processor.rs`: batched immutable segments, append-only stream declarations, explicit source retraction before backoff. Closed sources and immutable headers replace rollover/repair. `rendered_request/scope.rs` must allocate non-reused scopes across reclaim/restart. | Lean → conformance → runtime |
| Tool `args`, `result`, `partial_output_*`; `AgentToolResult` SDL/row (deleted) | `AgentToolCall` owns execution/delivery and a sealed `arguments` reference for dispatch/recovery before message publication. Provider streams own argument bytes; tool streams own output; terminal tool closure seals empty and nonempty output before delivery. Existing completion notification owner composes authored wrappers by reference. | Spec → runtime |
| `truncation/spill.rs`, spill links and discarded-spill flags | Owned provider-input boundary writes `PresentedPayload`: exact UTF-8 output ranges plus authored marker/separator segments. Preserve head/tail behavior and line normalization; retrieval hints name the tool call. Unreferenced retained output is not proof of delivery. `read_tool_output` reads the original stream. | Lean → conformance → runtime |
| Legacy `decode_persisted_message` / `present_persisted_message` and fallback tests (deleted) | Shared strict reconstruction produces native `Message`; existing `present_message` remains a rendering function. Missing seals, conflicts, malformed JSON/media and illegal role/block combinations fail explicitly. | Spec → runtime |
| `session/fork.rs` payload/tool/spill copies and spill remapping | Fork owner copies headers with `MessagePublication::Fork`, child-scoped message keys/sequences, no live request membership. Origin tool IDs remain provenance, not child executable rows. Compaction cursors still target retained child headers. | Lean → conformance → runtime |
| Session-only hydration completeness | Existing hydration owner follows every header payload (including authored presentation pieces), source and request/tool provenance under existing ACP. Fork publication requires an authorized complete reference closure. Receipt v2 signs payload extents; client completion reconstructs each seal. Denied dependencies reject; missing dependencies stay incomplete. | Spec → Lean → conformance → runtime |
| Response/spill desktop stores, queries, merge heuristics and CLI projections | Shared output reconstruction supplies native messages/live declarations; request lifecycle supplies status. No consumer-local text repair or short-message fallback. Parent session removal cannot cascade into retained origin dependencies; no output GC is introduced here. | Consumers |

`AgentOutputSource` holds only producer identity, stream declarations and terminal
disposition/extents. It is controlled by existing request/tool owners, not another
request lifecycle. Retraction has a durable representation even with zero replacement
bytes. Source twins and segment twins are conflicts, not last-writer selection.
Sealed history remains valid after its producer's generation is no longer active.

## Schema, pairing and hydration inventory

| Surface | This commit | Implementation handoff |
| --- | --- | --- |
| `gents-schemas/src/lib.rs`, `gents-protocol/src/schemas.rs`, runtime schema exports | Register source/segment; remove response/spill | Verify fresh schema registration and strict JSON/SDL decoding |
| `gents-migration/src/registry.rs` | Remove obsolete response/spill baselines; update client-authored catalog | Add fresh source/segment root pins and regenerate changed message/tool/hydration pins with DefraDB; catalog coverage/parity remains required. No fabricated pins or relaxed checks |
| `agent/p2p_reconcile/{templates,profiles,policy}.rs` | Replace response/spill routes with source/segment, preserving requester/agent filters and route directions | Conformance for conversation, client (both directions), machine and operator routes; collection presence is not ACP authorization |
| Subagent pairing templates | Host return leg includes source and segment with requester filters | Coordinator bridge `arguments` references require exact authorized dependency delivery to the host; do not broaden the bridge-only route to all parent requests or history |
| `agent/p2p_reconcile/session_hydration.rs` | New collection inventory | Replace session-only selection with authorized reference closure; preserve membership, route admission and bounded exact push |
| `session_hydration_reconcile.rs` | Inventory only; old queries intentionally remain | Query source/segment coordinates, enumerate twins and include origin dependencies in the signed exact manifest |
| `gents-protocol/src/session_hydration.rs`, `session_hydration_request.graphql` | Closed collection enum, v2 receipt with signed payload seals and SDL storage fields | Server validates closure; desktop verifies signature, exact document delivery and reconstructed extents before completion |
| Desktop `client/core/writes.rs`, `query/{session_transcript,document_patches}.rs`, `store/*`, observers | Inventory only; deleted row types break old readers | Follow origin references without subscribing to an entire parent session; project reordered header/source/segment arrival |
| `session/fork.rs` and session retention owners | Typed publication provenance and retained-reference contract | Preserve parent dependencies after parent close/removal, detach live membership, check ACP without copying payloads |

## Next layer's model surfaces

`Transcript`, `CompletionRetry`, `RequestExecutionLease`, `SessionFork`,
`SessionHydration`, tool/background delivery and provider-input narrowing must
cover: retraction without replacement bytes; stale/replayed writes; background
output after request termination; recovery publication; empty streams; gaps and
twins; headers arriving before payloads; native block order and signed reasoning;
line-normalized presentation; fork authorization/retention and reference closure.
Lean remains free of `sorry`; generated cases then drive the real shared projection
and transaction owners. Runtime/desktop/CLI validation belongs to those descendants.
