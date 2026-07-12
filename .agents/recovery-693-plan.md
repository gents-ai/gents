# Issue #693: duplicate-tolerant conversation recovery + honest counting

Branch: `iverc/conversation-recovery-duplicates-693` off origin/main (v0.6.11, 4f54a137).
Research: /private/tmp/.../scratchpad/recovery693/*.md

## Root cause (empirically reproduced by readers)

`upsert_X(filter:)` in defradb (`query-plan/src/plan/mutation/upsert.rs:427-431`) errors
`cannot upsert multiple matching documents` when >1 doc matches. AgentConversation.session_id is
unique in the CURRENT SDL, but `add_schema` short-circuits on existing collections
(migration.rs:222-228), so stores whose collection was created under a non-unique SDL keep
duplicates forever. Replication can also mint them (db-merge persists then indexes).

Blast radius is wider than the issue states: the same session_id-filtered upsert backs the LIVE
request path (session/conversation.rs:37-102, called from lifecycle/queue.rs:215 and
materialize.rs:172/408/464), so a duplicate store fails ordinary conversation writes too.
`load_conversation_document` (session/query.rs:69) uses `limit: 1` with NO order → arbitrary
duplicate wins → `resolve_behavior_id` can even bail with "session behavior mismatch".

## Defect 2 is faithful to the Lean model

`Recovery/Contract.lean`: `recover : Row → Row` (total, cannot fail) and
`recoveredRows_length : (recoveredRows rows).length = rows.length`. The model has NO notion of a
failed recovery, so `let count = rows.len()` (recovery.rs:582) implements the spec exactly.
=> Formal-first: the model must gain an outcome/report layer or the Rust fix has no fence.

## Design

### Lean (spec first)
1. `Recovery/Outcome.lean` (new): `SweepOutcome := recovered | failed`, `SweepReport {recovered, failed}`,
   `sweepRun`. Theorems:
   - `report_accounts_for_every_row`: recovered + failed = rows.length
   - `recovered_counts_only_successes`: recovered = (rows.filter succeeds).length
   - `all_failed_reports_zero`: (∀ r, ¬succeeds r) → recovered = 0   [THE anti-theorem for defect 2]
2. `Recovery/Sweeps/Conversation.lean` (new): Row = ConversationGroup (all docs sharing one
   session_id; nonempty). Canonical pick = argmax over a TOTAL order (updatedAt, richness, docId).
   - `canonicalOf_perm_invariant` (List.Perm) — "deterministic" becomes a theorem, needed because
     DefraDB returns duplicates in docID order, not recency order.
   - `canonicalOf_mem`.
   - recover: canonical → terminal status per parent request; non-canonical → same terminal status
     (converge, do NOT delete: the collection is replicated; a delete can be resurrected / fork CRDT).
   - stale/measure/h_recover_zero ⇒ idempotence (second pass recovers 0).
3. `Recovery/Contract.lean`: add `| agentConversation` to `PersistedRecoveryCollection` (+toContract,
   +all). This BREAKS `registered_sweeps_cover_persisted_collections` until the sweep is registered —
   intentional fence; both land in one commit.
4. `Recovery/Sweeps/Registry.lean`: register `conversationRecoverySweep` (cadence := .startup —
   recover_all is startup-only).
5. `Recovery/ContractCases.lean`: new cases (duplicate group recovers canonical; second pass = 0;
   all-failed => recovered 0) + `recoveryEquivalenceTheorem` branch (missing branch silently yields
   "unregistered_recovery_equivalence" and only trips native_decide at :242).
6. Case type gains `targetSelector` (must be "_docID") so the session_id-filtered upsert is illegal
   BY CONTRACT; plus recoveredCount/failedCount.

### Rust product
- `session/query.rs`: canonical conversation selection — load ALL docs for a session, rank in Rust by
  the same total order (do not trust `order:` with null timestamps). New
  `load_canonical_conversation_document` returning (docID, doc, duplicate_doc_ids).
- `session/conversation.rs`: replace EVERY `upsert_AgentConversation(filter:{session_id})` with
  read-canonical → `update_AgentConversation(filter:{_docID:{_eq}})` on hit, `create_` on miss.
  Fixes the live path too. (`upsert_X(docID:)` does NOT parse — docID is Update/Delete-only.)
- `lifecycle/recovery.rs::recover_stuck_conversations`: group rows by session_id, recover canonical +
  converge duplicates by _docID, `recovered += 1` per SESSION on success only, `failed += 1` on error,
  warn per duplicate set with session_id + doc count.
- `lifecycle.rs::RecoveryReport`: + `conversations_failed`, + `duplicate_conversation_sessions`.
- `agent/runtime/startup.rs:596-603`: info! successes; distinct warn! for failures + duplicates.

### Tests
- `tests/support/mod.rs`: `AGENT_CONVERSATION_NON_UNIQUE_SESSION_ID` SDL const +
  `test_db_with_duplicate_tolerant_conversations()` (add_schema legacy SDL BEFORE
  ensure_runtime_schemas, which swallows "already exists") + `create_conversation_row()` raw create.
  NOTE: a plain double-create on the shipped schema is REJECTED by the unique index — this recipe is
  the only way to seed the wild store's shape. Verified.
- `tests/e2e_lifecycle/lifecycle_recovery.rs`: duplicate regression test — pre-fix it fails BOTH on
  count (gets 2) and on status (both docs stay "processing").
- `tests/conformance/recovery_sweeps.rs`: new AgentConversation drive arm; bump the hardcoded 25s
  (recovery_sweeps.rs:11, :51; coverage.rs:174-175); expected_sweep_ids; equivalence theorem map.
- Keep green: lifecycle_recovery.rs:113/182/241 assert conversations_recovered == 1 (still true).

## Sharp edges
- escape_graphql_string everywhere; never emit [].
- Duplicate rows must differ in ≥1 field (docID is content-derived) or they collapse to one doc.
- cadence=.startup or assert_periodic_recovery_registry_matches_lean fails.
- Gate: cargo test -p defra-agent (full package) + cargo check --workspace --all-targets + lake build.
