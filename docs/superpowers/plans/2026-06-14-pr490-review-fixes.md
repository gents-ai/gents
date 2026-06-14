# PR #490 Review-Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve the 17 confirmed findings from the ultracode review of PR #490 by fixing their three systemic roots first, then the remaining leaf defects — Lean-first for anything that changes legal transitions.

**Architecture:** This is a Rust agent runtime on DefraDB built on the foundation flow *Lean spec → conformance tests → implementation*. Findings that change what transitions are legal or what invariants hold (single-use invites, reciprocal admission, replicator-filter identity) start in `crates/defra-agent/proofs/`; CLI/plumbing findings do not. Three systemic patterns drive most fixes: (1) name-vs-id key-space mismatch, (2) collection scope over-specified in the CLI but derived only from `--template` in the reconciler, (3) migration enumeration drift across hosts.

**Tech Stack:** Rust (workspace: `defra-agent`, `defra-agent-cli`, `defra-agent-protocol`, `defra-agent-desktop-core`), Lean 4 + Mathlib (`proofs/`), DefraDB (GraphQL control plane), iroh P2P.

**Decisions locked with the user (2026-06-14):**
- Pattern 2: **remove** `--collection`/`--profile` from `pairings set`/`join` entirely (template is the sole scope source). They are unreleased in this PR, so no deprecation alias is owed.
- Security: **all three** deferred items in-scope — #9 (network_id validate), #16 (single-use invite nonce), #8 (reciprocal admission under the join gate).
- Finding 7 (TOFU "divergence") is a **false positive** — Lean `signedByMember` + Rust `decide_join_admission` + the conformance test all agree stale-only registry → `Rejected` is intentional. **No change; do not "fix" it.**

**Gating discipline (CLAUDE.md):**
- Proofs gate: `cd crates/defra-agent/proofs && lake build` — **zero `sorry`**, no vacuous theorems.
- Rust gate per crate: `cargo test -p <crate>` (the FULL package suite, never `--lib` — integration tests are separate compile units).
- Conformance gate: `cargo test -p defra-agent --test conformance`.
- Sharp edges: always `graphql::escape_graphql_string()` for interpolation; never emit `[]` in a DefraDB mutation (emit `null`); `tracing`, never `println`.

---

## File Structure (what each phase touches)

- **Proofs** (`crates/defra-agent/proofs/Proofs/`): `PeerRegistryDiscovery/{State,Transition,Executable}.lean` (nonce + reciprocal admission), `PairingReconcile/` (replicator-filter boundary note).
- **Protocol** (`crates/defra-agent-protocol/src/pairing_token.rs`): token v4 — drop `profiles`, add `nonce`, keep `network_id`.
- **Runtime** (`crates/defra-agent/src/`): `agent/p2p_reconcile/{engine,diff,discovery}.rs` (name-vs-id, ownership upsert), `migration.rs` (consolidation entry + agent_did backfill/warn), `agent/runtime/startup.rs`.
- **CLI** (`crates/defra-agent-cli/src/`): `cli/args.rs` (flag removal, default format), `commands/p2p/{pairings,invite,join,network}.rs`, `cli/deprecations.rs`, plus the local-path migration call sites `commands/{subagent,serve,init}.rs`, `main.rs`.
- **Conformance** (`crates/defra-agent/tests/`): `conformance/peer_registry_discovery.rs`, `support/pairing_conformance/{runner,scenario}.rs` + `pairing_scenarios/*.json`.

---

## Phase A — Lean-first transition changes (must lead)

### Task A1: Single-use invite nonce — Lean model

**Findings:** #16. **Files:**
- Modify: `crates/defra-agent/proofs/Proofs/PeerRegistryDiscovery/State.lean`
- Modify: `crates/defra-agent/proofs/Proofs/PeerRegistryDiscovery/Transition.lean`
- Modify: `crates/defra-agent/proofs/Proofs/PeerRegistryDiscovery/Executable.lean`

- [ ] **Step 1: Model the nonce ledger in State.** Add a `consumedNonces : Finset Nonce` to `DiscoveryState` (abbrev `Nonce := String`). Add `nonce : Nonce` to the `Token` structure in `Transition.lean`.
- [ ] **Step 2: Strengthen the join admission predicate.** Extend `signedByMember` (or add a wrapping `admitsJoin`) so a join additionally requires `tok.nonce ∉ s.consumedNonces`. Add a `joinConsumesNonce` mutator that inserts `tok.nonce` into `consumedNonces` as part of the join transition.
- [ ] **Step 3: Prove single-use.** State and prove `theorem replay_rejected`: if a token is admitted from state `s` producing `s'`, then the same token is NOT admissible from `s'` (its nonce is now consumed). This must be non-vacuous — the hypothesis is "admitted once," the conclusion is "second admission impossible."
- [ ] **Step 4: Mirror in Executable.** Update the executable decision function so it threads the consumed-nonce check; keep it in agreement with the `Transition` relation (the existing Executable↔Transition agreement lemma must still hold).
- [ ] **Step 5: Gate.** `cd crates/defra-agent/proofs && lake build` → builds, zero `sorry`. Confirm `grep -rn sorry Proofs/` is clean.
- [ ] **Step 6: Commit.** `proof(discovery): single-use invite nonce + replay_rejected theorem (#16)`

### Task A2: Reciprocal admission — Lean model

**Findings:** #8. **Files:** `crates/defra-agent/proofs/Proofs/PeerRegistryDiscovery/Transition.lean`, `Executable.lean`.

- [ ] **Step 1:** Add a `reciprocal : Bool` discriminator to the join transition (or a sibling `reciprocalJoin` constructor in the `Transition` inductive) that models the `--reciprocal` leg.
- [ ] **Step 2:** Specify its admission rule. The reciprocal leg must STILL require `signedByMember` (signature + member/TOFU + nonce) — it does not get a free pass. Encode that the reciprocal flag only affects *what is wired*, not *whether the join is admitted*. This closes the "transition with no Lean counterpart" gap.
- [ ] **Step 3:** Prove `theorem reciprocal_join_still_gated`: a reciprocal join from `s` is admissible only if `signedByMember` holds (non-vacuous: construct a witness where signature is invalid → reciprocal join is rejected).
- [ ] **Step 4:** Mirror in `Executable.lean`; preserve the agreement lemma.
- [ ] **Step 5: Gate.** `lake build` zero sorry.
- [ ] **Step 6: Commit.** `proof(discovery): reciprocal join stays under the admission gate (#8)`

### Task A3: Replicator-filter actual-side boundary note

**Findings:** #13. **Files:** `crates/defra-agent/src/agent/p2p_reconcile/diff.rs:39-44` (the `PairingActual` struct) + a one-line note in the relevant `PairingReconcile` Lean file's boundary section.

- [ ] **Step 1:** `PairingActual` carries no `replicator_filter` (the remote read can't observe the installed filter). Document this as an explicit, intended boundary: the `(address, filter)` identity is enforced via `PairingApplied.replicator_filter` (reconciler-owned), not re-read from actual. Add the inline `//` note on `PairingActual` and a matching boundary note in Lean where `ReplicatorId` is defined.
- [ ] **Step 2: Gate.** `lake build`; `cargo build -p defra-agent`.
- [ ] **Step 3: Commit.** `docs(proof): fence the PairingActual replicator-filter boundary (#13)`

---

## Phase B — Reconcile engine correctness

### Task B1: name-vs-id collection diff + mock hardening

**Findings:** #1 (must-fix). **Files:**
- Modify: `crates/defra-agent/src/agent/p2p_reconcile/engine.rs` (`read_actual` / the store read that fills `PairingActual.collections`, and where `PeerPairingApplied` persists collections)
- Modify: test double `MockAdmin` (in `engine.rs` tests) so `list_p2p_collections` returns a token DIFFERENT from the name passed to `add_p2p_collections`.

- [ ] **Step 1: Write the failing engine test.** Add a test where `MockAdmin::add_p2p_collections("AgentRequest")` records, but `list_p2p_collections` returns the collection *id* (a distinct token, e.g. `"col:AgentRequest:bae123"`), then assert a SECOND `reconcile_peer_tick` over a Replicate-template pairing yields ZERO ops. With today's code this fails (re-emits `InstallCollection` forever).
- [ ] **Step 2: Run it, confirm it fails** with a spurious `InstallCollection`. Run: `cargo test -p defra-agent reconcile_peer -- --nocapture`.
- [ ] **Step 3: Implement.** Normalize both sides to one canonical space at the read boundary: resolve desired collection names → `collection_id` (via the node/store) when building `PairingActual`/`PairingDesired` for the diff, and persist ids (not names) in `PeerPairingApplied`. Keep names for display only. The `diff.rs` set logic is unchanged — the fix is purely what fills the sets.
- [ ] **Step 4: Run the test, confirm it passes**, plus `cargo test -p defra-agent --test conformance pairing_reconcile`.
- [ ] **Step 5: Full gate.** `cargo test -p defra-agent`.
- [ ] **Step 6: Commit.** `fix(p2p): diff collections in id-space; mock returns distinct id (#1)`

### Task B2: ownership upsert filter scoped to registry

**Findings:** #2 / #15. **Files:** `crates/defra-agent/src/agent/p2p_reconcile/discovery.rs` (`upsert_registry_desired_mutation` ~640-679, used by the per-peer upsert ~314-366).

- [ ] **Step 1: Write the failing test.** Construct a state where an operator-owned row exists for `peer_id` (`source="operator"`), run the discovery upsert for the same `peer_id`, and assert the operator row's `source` is UNCHANGED (still `"operator"`). Today it flips to `"registry"`.
- [ ] **Step 2: Run, confirm fail.**
- [ ] **Step 3: Implement.** Scope the upsert filter to `source: { _eq: "registry" }` (mirroring the existing `delete_registry_desired_mutation`), so the update branch can never name an operator row; when no registry row matches, it creates one. Use `graphql::escape_graphql_string()` for interpolated values.
- [ ] **Step 4: Run, confirm pass;** `cargo test -p defra-agent --test conformance peer_registry`.
- [ ] **Step 5: Gate.** `cargo test -p defra-agent`.
- [ ] **Step 6: Commit.** `fix(p2p): registry upsert filters on source so it can't claim operator rows (#2/#15)`

---

## Phase C — Token v4 (folds Pattern 2 removal + #9 + #16)

### Task C1: Token v4 — drop profiles, add nonce, wire network_id

**Findings:** #9, #16 (impl half), Pattern 2 (token half). **Files:**
- Modify: `crates/defra-agent-protocol/src/pairing_token.rs`
- Modify: `crates/defra-agent-cli/src/commands/p2p/invite.rs`
- Modify: `crates/defra-agent-cli/src/commands/p2p/join.rs`

- [ ] **Step 1: Write failing token tests.** In `pairing_token.rs` tests: (a) v4 round-trips; (b) `signing_payload` covers `nonce` and `network_id` (two tokens differing only in `nonce` → different payloads; same for `network_id`); (c) `decode` rejects v3 with a re-issue hint. Update existing tests to v4 and drop `profiles` from the sample.
- [ ] **Step 2: Run, confirm fail/compile-error.**
- [ ] **Step 3: Implement token.** Bump `v` to 4. Remove the `profiles: Vec<String>` field (dead post-Pattern-2). Add `nonce: String` (random, set at mint). Keep `network_id` and `template`. Update `decode` to accept only `v == 4`. Update doc header version history.
- [ ] **Step 4: Implement invite mint.** In `invite.rs`: populate `network_id` from `resolve_network_id()` (not hardcoded `"default"`), generate a fresh random `nonce`, stop collecting/encoding `profiles`.
- [ ] **Step 5: Implement join validation.** In `join.rs`: after signature + freshness, (a) compare the token's `network_id` against the local `resolve_network_id()` and reject on mismatch; (b) enforce single-use via the consumed-nonce ledger (see C2). Remove any `profiles` consumption.
- [ ] **Step 6: Run token tests, confirm pass.** `cargo test -p defra-agent-protocol`.
- [ ] **Step 7: Commit.** `feat(pairing): token v4 — single-use nonce, validated network_id, drop dead profiles (#9/#16/Pattern2)`

### Task C2: Consumed-nonce ledger (runtime side of #16)

**Findings:** #16. **Files:** `crates/defra-agent-cli/src/commands/p2p/join.rs`, and whichever collection backs the ledger (new small collection, e.g. `ConsumedInviteNonce`, registered via `migration.rs` — mirror an existing single-purpose collection migration). Conformance lives in the A1 Lean model; this is the impl that satisfies it.

- [ ] **Step 1: Write the failing test** (CLI integration in `crates/defra-agent-cli/tests/cli_p2p.rs`): minting one invite and joining twice with the same token rejects the second join with a replay error.
- [ ] **Step 2: Run, confirm fail** (today both joins succeed).
- [ ] **Step 3: Implement.** On a successful join, record `nonce` (write the ledger doc, emitting `null` not `[]` for any empty array field, escaping all interpolations). Before admitting, query the ledger; reject if present. Register the collection in `ensure_all_runtime_migrations` (see D1) so all hosts get it.
- [ ] **Step 4: Run, confirm pass.** `cargo test -p defra-agent-cli p2p -- --nocapture`.
- [ ] **Step 5: Commit.** `feat(p2p): enforce single-use invites via consumed-nonce ledger (#16)`

### Task C3: Remove --collection/--profile from pairings set/join

**Findings:** #4 (source), #5, Pattern 2. **Files:**
- Modify: `crates/defra-agent-cli/src/cli/args.rs` (`P2pPairingSetArgs` ~1985-2023, `P2pJoinArgs` ~2069-2100)
- Modify: `crates/defra-agent-cli/src/commands/p2p/pairings.rs` (drop the `bail!("provide at least one --collection or --profile")` gate ~81-84; drop `collections`/`profiles` from the `upsert_PeerPairingDesired` mutation ~263-312)
- Check (do not necessarily delete): `crates/defra-agent-cli/src/commands/p2p/collections.rs`, `profiles.rs` — leave intact if still used by `p2p admin` low-level commands; only remove the pairing-front-door usage.

- [ ] **Step 1: Write/adjust failing test.** Update `cli_p2p.rs` / `cli_p2p_templates.rs` so `pairings set --template conversation` (no `--collection`) succeeds and persists scope solely from the template. Assert `--collection` is no longer an accepted flag (clap rejects it).
- [ ] **Step 2: Run, confirm fail.**
- [ ] **Step 3: Implement.** Remove the two flags from both arg structs; make `--template` the authoritative scope input with default `conversation`. Remove the now-dead `collections`/`profiles` columns from the upsert mutation (or set them `null`, never `[]`). Leave the schema fields in place (no destructive schema change) but stop writing them as authoritative.
- [ ] **Step 4: Run, confirm pass;** grep to confirm no remaining reader of `PeerPairingDesired.collections`/`.profiles` in the reconcile path.
- [ ] **Step 5: Gate.** `cargo test -p defra-agent-cli`.
- [ ] **Step 6: Commit.** `refactor(cli): template is the sole pairing scope; drop dead --collection/--profile (#4/#5/Pattern2)`

---

## Phase D — Migration consolidation & host drift

### Task D1: Route every CLI-local path through ensure_all_runtime_migrations

**Findings:** #3, Pattern 3. **Files:**
- Modify: `crates/defra-agent-cli/src/commands/subagent.rs` (~67-68), `crates/defra-agent-cli/src/commands/serve.rs` (~141-142), `crates/defra-agent-cli/src/commands/init.rs` (~160), `crates/defra-agent-cli/src/main.rs` `resolve_config_access` (~574), `crates/defra-agent/src/oneshot.rs` (~39-42).
- Confirm: `crates/defra-agent/src/migration.rs` `ensure_all_runtime_migrations` (~1129-1151) includes `ensure_conversation_scope_key_migrations` and now the C2 nonce-ledger collection.

- [ ] **Step 1: Write the failing test.** A CLI integration test: open a DB created WITHOUT the scope-key field (simulate pre-upgrade), then run `subagent cancel` (local) — today it fails with `Cannot query field "agent_did"`. Assert it now succeeds.
- [ ] **Step 2: Run, confirm fail.**
- [ ] **Step 3: Implement.** Replace each hand-enumerated subset with a single `migration::ensure_all_runtime_migrations(&node).await?` call at each local entry point. Migrations are idempotent check-then-add, so per-invocation cost is bounded. Make `ensure_all_runtime_migrations` the documented single sanctioned entry point (add a `//` note discouraging new hand-enumerated call sites).
- [ ] **Step 4: Run, confirm pass.** `cargo test -p defra-agent-cli` and `cargo test -p defra-agent`.
- [ ] **Step 5: Commit.** `fix(migration): all CLI-local paths run the full migration set; one sanctioned entry (#3/Pattern3)`

### Task D2: agent_did backfill + warn on legacy rows

**Findings:** #11. **Files:** `crates/defra-agent/src/migration.rs` (`ensure_conversation_scope_key_migrations` ~1054-1121, Group-2 branch ~1098-1118).

- [ ] **Step 1: Write the failing test.** Seed a conversation row predating the scope key (agent_did absent/null), run the migration, assert: either the row's `agent_did` is backfilled from the owning session, OR a `tracing::warn` reports the count of un-scoped rows. (Pick backfill where a single owning-session write is available — it's immutability-safe at field creation; otherwise warn.)
- [ ] **Step 2: Run, confirm fail** (today: silent, no backfill, no warn).
- [ ] **Step 3: Implement.** In the Group-2 branch, count legacy rows and `tracing::warn!` with the count and the replication consequence; where the owning session id is resolvable, backfill `agent_did` in the same field-creation write (single write, immutability-safe).
- [ ] **Step 4: Run, confirm pass.** `cargo test -p defra-agent migration -- --nocapture`.
- [ ] **Step 5: Commit.** `fix(migration): backfill/warn legacy conversation rows missing agent_did (#11)`

---

## Phase E — CLI leaf fixes

### Task E1: SUBSCRIBED health honest for Push templates

**Findings:** #4 (health). **Files:** `crates/defra-agent-cli/src/commands/p2p/pairings.rs` (`annotate_pairing_health` ~371-410).

- [ ] **Step 1: Write the failing test.** A healthy default (`conversation`, Push) pairing currently shows SUBSCRIBED=`no`. Assert it reports healthy (REPLICATING-keyed), not a false `no`.
- [ ] **Step 2: Run, confirm fail.**
- [ ] **Step 3: Implement.** Apply the `desired.collections ⊆ applied.collections` SUBSCRIBED check ONLY for Replicate-delivery templates; for Push templates key health off REPLICATING (applied replicator present). Resolve delivery mode from the template.
- [ ] **Step 4: Run, confirm pass.** `cargo test -p defra-agent-cli p2p`.
- [ ] **Step 5: Commit.** `fix(cli): SUBSCRIBED health honest for Push templates (#4)`

### Task E2: Deprecation entries for removed p2p pair / unpair

**Findings:** #6. **Files:** `crates/defra-agent-cli/src/cli/deprecations.rs` (~1-5 table).

- [ ] **Step 1: Write the failing test.** Assert invoking `p2p unpair` and `p2p pair` yields the friendly deprecation message naming the replacement, not clap's opaque "unrecognized subcommand."
- [ ] **Step 2: Run, confirm fail.**
- [ ] **Step 3: Implement.** Add DEPRECATED entries: `p2p unpair` → `p2p pairings rm`, `p2p pair` → `p2p pairings set`. Match the existing deprecation entry shape.
- [ ] **Step 4: Run, confirm pass.** `cargo test -p defra-agent-cli`.
- [ ] **Step 5: Commit.** `fix(cli): deprecation entries for removed p2p pair/unpair (#6)`

### Task E3: []→null in upsert_pairing_mutation

**Findings:** #10 / #14. **Files:** `crates/defra-agent-cli/src/commands/p2p/pairings.rs` (`upsert_pairing_mutation` ~279-303).

- [ ] **Step 1:** Replace the non-null-safe list helper for `collections`/`replicator_addresses` with `graphql_nullable_string_list_literal` (the helper `profiles.rs` already uses), so an empty list renders as `null`, never `[]`. (Note: if Task C3 already removed `collections` from this mutation, apply only to `replicator_addresses`.)
- [ ] **Step 2: Gate.** `cargo test -p defra-agent-cli`.
- [ ] **Step 3: Commit.** `fix(p2p): emit null not [] for empty pairing arrays (#10/#14)`

### Task E4: pairings list default format → Table

**Findings:** #17. **Files:** `crates/defra-agent-cli/src/cli/args.rs` (`pairings list` default ~2042 vs siblings ~1851/1864).

- [ ] **Step 1:** Change the `pairings list` default output format from JSON to Table to match `p2p network list` / `p2p templates list`.
- [ ] **Step 2: Gate.** `cargo test -p defra-agent-cli`.
- [ ] **Step 3: Commit.** `fix(cli): pairings list defaults to Table like its siblings (#17)`

### Task E5: exact peer-id match for health

**Findings:** #18. **Files:** `crates/defra-agent-cli/src/commands/p2p/pairings.rs` (~393, ~469).

- [ ] **Step 1: Write the failing test** (or assertion): a peer-id that is a substring of another no longer reports `connected` by accident.
- [ ] **Step 2:** Replace the substring `connected` check with exact equality (`BTreeSet::contains` / `==`), matching `network.rs`.
- [ ] **Step 3: Gate.** `cargo test -p defra-agent-cli p2p`.
- [ ] **Step 4: Commit.** `fix(cli): exact peer-id match for connected health (#18)`

---

## Phase F — Conformance gap

### Task F1: filter-change scenario fixture

**Findings:** #12. **Files:** `crates/defra-agent/tests/support/pairing_conformance/{runner,scenario}.rs` (~286-290, ~337-363), new `crates/defra-agent/tests/support/pairing_scenarios/filter_change_reinstall.json`.

- [ ] **Step 1:** Extend `Action::OperatorWrite` (scenario.rs) to carry an optional `filter`.
- [ ] **Step 2:** Teach the runner to apply a filter change and assert teardown+install of the affected replicator (mirrors Lean `filter_change_forces_reinstall`).
- [ ] **Step 3:** Add `filter_change_reinstall.json` exercising: converged pairing → operator changes filter → reinstall ops → reconverged.
- [ ] **Step 4: Gate.** `cargo test -p defra-agent --test conformance pairing_reconcile -- --nocapture`.
- [ ] **Step 5: Commit.** `test(p2p): scenario-harness coverage for filter-change reinstall (#12)`

---

## Phase G — Final verification

### Task G1: Full-suite gate + branch review

- [ ] **Step 1:** `cd crates/defra-agent/proofs && lake build` — zero sorry; `grep -rn sorry Proofs/` clean.
- [ ] **Step 2:** `cargo test -p defra-agent` (full package).
- [ ] **Step 3:** `cargo test -p defra-agent-cli` (full package).
- [ ] **Step 4:** `cargo test -p defra-agent-protocol`.
- [ ] **Step 5:** `cargo fmt --all` then `cargo clippy --workspace`.
- [ ] **Step 6:** One final spec-compliance + branch review pass over the full diff (per project review calibration). Confirm Finding 7 was left unchanged and the change log maps 1:1 to the confirmed findings.
- [ ] **Step 7: Commit** any fmt/clippy drift.

---

## Finding → Task coverage map

| Finding | Severity | Task |
|---|---|---|
| #1 name-vs-id diff (must-fix) | high | B1 |
| #2 / #15 ownership upsert | medium | B2 |
| #3 migration host drift | medium | D1 |
| #4 SUBSCRIBED health + scope source | medium | E1 (health) + C3 (source) |
| #5 inert --collection/--profile | medium | C3 |
| #6 missing deprecation entries | medium | E2 |
| #7 TOFU "divergence" | — | **REJECTED (false positive) — no change** |
| #8 reciprocal admission | low | A2 |
| #9 network_id inert | low | C1 |
| #10 / #14 []-for-empty | low/nit | E3 |
| #11 agent_did backfill/warn | low | D2 |
| #12 filter-change scenario gap | low | F1 |
| #13 replicator_filter boundary | low | A3 |
| #16 invite replay (single-use) | nit | A1 (spec) + C1/C2 (impl) |
| #17 list default format | nit | E4 |
| #18 substring peer-id match | nit | E5 |

Patterns: P1 (name-vs-id) → B1; P2 (scope source-of-truth) → C3; P3 (migration drift) → D1.
