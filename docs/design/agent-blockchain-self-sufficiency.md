# Blockchain-anchored agent lifecycle and economic self-sufficiency

Status: design (no implementation yet).
Owners: runtime / identity / triggers.
Related: [Shinzō](https://shinzo.network/) (same Source Network + DefraDB substrate), [Venice](https://docs.venice.ai/) (private OpenAI-compatible inference), [Akash](https://docs.akash.network/sdl) (decentralized compute).
Related repo work: sourcenetwork/defra-agent#9 (principal/behavior/deployment split — this design assumes it).

## TL;DR

defra-agent already signs every document it writes with a DID-backed identity
(`AgentIdentity`, `crates/defra-agent/src/identity.rs:57`). Shinzō already turns
signed DefraDB commits into externally provable, on-chain-anchored data on this
exact stack. So "tie the agent lifecycle to the blockchain" is not a new crypto
problem — it is an *emission and anchoring* problem on top of machinery that
already exists.

This doc proposes a four-layer build, sequenced by increasing risk:

1. **Attestation** — emit signed, content-addressed `SignedAttestation`
   documents at lifecycle transitions; anchor them outward Shinzō-style.
   *(Days–2 weeks. No money moves.)*
2. **Venice backend** — add a `BackendProviderKind` for Venice (OpenAI-compatible),
   giving agents private inference. *(~1 day for the wire; payment is separate.)*
3. **Treasury** — give a principal a wallet, cost accounting, and budget-gated
   dispatch. **This is a new state machine and starts in Lean.** *(Weeks.)*
4. **Self-provisioned compute** — `AgentDeployment` can request/renew Akash
   leases for its own host. *(New subsystem, sequenced last.)*

Layer 1 validates the whole "lifecycle → verifiable truth" thesis with near-zero
economic risk. Layers 3–4 are where the genuine difficulty lives.

## 1. Current state — what already exists

This section is the load-bearing part: the feasibility argument rests on
primitives that are already in the tree, not aspirational ones.

### 1.1 Identity and signing (production-grade, reusable as-is)

- **`AgentIdentity` trait** — `crates/defra-agent/src/identity.rs:57`:

  ```rust
  #[async_trait]
  pub trait AgentIdentity: Send + Sync {
      fn did(&self) -> &str;
      async fn sign(&self, payload: &[u8]) -> Result<Vec<u8>>;
      async fn verify(&self, did: &str, payload: &[u8], signature: &[u8]) -> Result<bool>;
      fn service_account(&self) -> Option<&ServiceAccount>;
  }
  ```

  Async, pluggable, already abstracted from concrete signers. `sign`/`verify` are
  exactly what an attestation layer needs — no new signing machinery required.

- **`AgentPrincipal`** — `identity.rs:38` — owns `agent_did` plus
  `Arc<dyn AgentIdentity>`. One principal per deployment, shared across behaviors.
  This is the audit/permission boundary the whole idea hangs on.

- **Backends** — `KeyIdentity` (`identity.rs:80`, filesystem keys) and
  `RegisteredIdentity` (DefraDB-registered, remote signers incl. macOS Secure
  Enclave via `defra_core::signing::{RemoteSigner, SigningConfig, SigningKeyType}`,
  `identity.rs:8`). Ed25519 and Secp256r1 supported today.

- **Every DefraDB mutation is already signed.** Writes flow through the principal's
  identity at the `defra_node` layer; documents are content-addressed Merkle-CRDT
  commits. This is the same property Shinzō anchors. We are not adding provenance —
  we are *surfacing and exporting* provenance that already exists.

**Assessment: very high reuse.** The attestation layer is additive; it does not
touch the signing path.

### 1.2 Lifecycle transition points (exist; emission hook does not)

- **Request lifecycle** — `crates/defra-agent/src/lifecycle.rs` —
  `Pending → Claimed → Processing → {Completed, Failed, Superseded, Dead,
  Interrupted}` (+ `InputRequired`). Transitions are persisted to DefraDB.
- **Tool-call lifecycle** — `crates/defra-agent/src/tool_call_lifecycle.rs` —
  `Pending → Running → {Completed, Failed, TimedOut, Cancelled}`.
- **`DefraSessionHook`** — `crates/defra-agent/src/hook.rs` — implements rig-core's
  hook trait and drives transitions during completion.
- **Process lifecycle** — `ProcessLifecycleObserver`
  (`crates/defra-agent/src/agent.rs:70`), states `Uninitialized → Recovering →
  Ready → ShuttingDown → Shutdown`. **Already a pluggable observer trait** — the
  natural seam for an orchestration/lease manager (Layer 4).

**Gap:** there is no general-purpose "emit an external event at a transition"
seam. The rig-core hook result type is limited to `HookAction::{Continue,
Terminate}`, so we do **not** overload it. We add a sibling `AttestationEmitter`
trait (§3.2) that the runtime calls at transition points, independent of rig-core.

### 1.3 Inference backend abstraction (mature strategy pattern)

- **`BackendProviderKind`** — `crates/defra-agent/src/backend_provider.rs:5` —
  `OpenAiCompatible | OpenRouter | ChatGptCodex`, with serde aliases and a
  `parse_optional` parser (`backend_provider.rs:30`).
- **`InferenceBackend`** document — `crates/defra-agent/src/backend_registry.rs` —
  `endpoint`, `api_key`/`api_key_env_var`, `models`, concurrency caps, probe status.
  Schema lives under `crates/defra-agent-protocol/schemas/inference/`.
- **Runtime selection** — `crates/defra-agent/src/agent/runtime/context.rs:128` —
  matches on `behavior.backend_provider_kind` and builds the rig client;
  `OpenAiCompatible` uses `rig::providers::openai::CompletionsClient` with a custom
  `base_url`.
- **Model discovery** — `discover_models` (`backend_provider.rs:92`) is generic over
  the OpenAI `/models` shape.
- An `InferenceCall.lean` model already exists in the proofs tree — inference is
  already a modeled concern, which matters for Layer 3.

**Assessment: very high.** Venice is OpenAI-compatible, so Layer 2 is largely a new
enum variant + a match arm reusing the existing OpenAI client (§4).

### 1.4 Trigger / task system (pluggable; no economic primitive)

- **`TriggerKind`** `Schedule | Event | Manual`
  (`crates/defra-agent/src/trigger_engine/mod.rs:37`); existing sources include
  `schedule_source`, `event_source`, `manual_source`, `subagent_source`,
  `subscription_source` (`trigger_engine/mod.rs:16`).
- **`FireIntent`** (`trigger_engine/mod.rs:63`) carries a one-shot
  `on_result: Box<dyn FnOnce(FireResult) + Send>` callback — a clean seam for
  post-fire bookkeeping.
- **`FireResult`** (`trigger_engine/mod.rs:93`) = `Fired | Skipped | Errored`.
  Budget enforcement (Layer 3) adds/encodes a `Skipped { reason: "insolvent" }`
  outcome rather than inventing a new control path.

**Gap:** there is no wallet, no cost ledger, no price feed, no payment action. The
trigger system can *express* an economic loop but has none of its nouns yet.

### 1.5 Compute / deployment (single-node today)

- The runtime assumes the process already runs somewhere. `DefraAgent::run`
  (`agent.rs`) spawns the trigger engine + behavior daemons in-process.
- CLAUDE.md and #9 describe `AgentDeployment` (where a principal runs) as a target,
  but there is **no provisioning API, no SDL emission, no lease tracking** today.
- `ProcessLifecycleObserver` is the one real seam for an external orchestrator.

**Assessment: moderate.** Layer 4 is a *new subsystem*, not a refactor, which is
why it is sequenced last.

## 2. Goals and non-goals

### Goals

- Every meaningful lifecycle transition produces a signed, content-addressed,
  externally verifiable attestation, anchorable the way Shinzō anchors data.
- An agent can run inference through Venice for privacy and (eventually)
  token-funded access.
- A principal can hold a budget, account for its own inference/compute cost, and
  modulate its behavior to stay solvent — with the solvency rules *proven*, not
  just coded.
- An agent can (eventually) request and renew its own Akash compute.

### Non-goals (initially)

- Building a new chain or consensus. We anchor to existing infrastructure
  (Shinzō / Source Network); we do not reinvent it.
- Holding spend-authority keys in the same custody as signing keys (§6).
- Real funds in Layers 1–2. Treasury starts on testnet / simulated balances.
- Multi-region distributed DefraDB sync (out of scope; tracked elsewhere).

## 3. Layer 1 — Signed lifecycle attestation (the wedge)

The minimum change that proves the central thesis. No money, low risk, high signal.

### 3.1 `SignedAttestation` schema

New collection under `crates/defra-agent-protocol/schemas/` (likely a new
`provenance/` group alongside `inference/` and `services/`), `include_str!`-compiled
like every other schema:

| Field | Type | Notes |
|---|---|---|
| `attestation_id` | ID | content address of the canonical payload |
| `agent_did` | String | signer / principal |
| `subject_kind` | String | `request` \| `tool_call` \| `process` \| `inference` |
| `subject_id` | String | e.g. `request_id`, `tool_call_id` |
| `from_state` | String | nullable for genesis transitions |
| `to_state` | String | target lifecycle state |
| `payload_hash` | String | hash of the canonical transition payload |
| `occurred_at` | DateTime | runtime clock |
| `signature` | String | `AgentIdentity::sign(canonical_bytes)` |
| `anchor_status` | String | runtime-owned: `local` \| `anchored` \| `failed` |
| `anchor_ref` | String | nullable; external anchor handle (Shinzō ref / tx) |

Field ownership follows the existing apply/runtime split (CLAUDE.md): the runtime
owns *all* of these — attestations are live-state, never desired-state. The apply
path never writes them.

### 3.2 `AttestationEmitter` trait

A sibling to the existing hook system — **not** an extension of `HookAction`:

```rust
#[async_trait]
pub trait AttestationEmitter: Send + Sync {
    async fn attest(&self, ev: TransitionEvent) -> Result<AttestationId>;
}
```

- Default impl signs the canonical payload via the principal's `AgentIdentity`
  (`identity.rs:61`) and writes a `SignedAttestation` row.
- Called from the runtime at transition points the runtime already owns
  (request claim/complete in the session path, tool-call terminal in
  `tool_call_lifecycle`, process state in the `ProcessLifecycleObserver`).
- A no-op impl is the default so attestation is opt-in per deployment and adds
  zero overhead when disabled.

**Canonicalization is the only subtle part.** The signed bytes must be
deterministic (stable field order, fixed encoding) so `verify` is reproducible
across nodes. Reuse DefraDB's existing content-addressing convention rather than
inventing a serialization.

### 3.3 Anchoring

A background `AnchorSink` (its own `TriggerSource` or a periodic task) batches
`anchor_status = local` attestations and publishes them to the external anchor
(Shinzō host / Source Network endpoint), then flips `anchor_status` to `anchored`
and records `anchor_ref`. Anchoring is asynchronous and idempotent: an attestation
is *valid* the moment it is signed; anchoring only makes it *externally provable*.

### 3.4 Verification

`AgentIdentity::verify` (`identity.rs:63`) already closes the loop: any third party
with the DID and the canonical payload can verify a signature. A small
`verify-attestation` CLI subcommand demonstrates end-to-end audit without touching
the runtime.

### 3.5 What Layer 1 proves

End-to-end: *transition → sign → store → anchor → externally verify*, exercising the
real signing infrastructure with no economic surface. Everything else builds on it.

## 4. Layer 2 — Venice inference backend

Venice exposes an OpenAI-compatible API (Bearer key; models via `/models`),
billable in USD or via staked VVV → Venice Compute Units. The wiring:

1. Add `Venice` to `BackendProviderKind` (`backend_provider.rs:6`) with serde
   aliases; extend `parse_optional` (`backend_provider.rs:30`).
2. Add a match arm in `agent/runtime/context.rs:128`. If Venice is fully
   OpenAI-compatible it reuses `rig::providers::openai::CompletionsClient` with
   Venice's `base_url` — minimal code. Only Venice-specific headers/params (if any)
   justify a bespoke arm.
3. `discover_models` (`backend_provider.rs:92`) already handles the OpenAI `/models`
   shape; no change expected.
4. Author an `InferenceBackend` document pointing at Venice with
   `api_key_env_var` — purely config, no code.

The API key path covers Layer 2 on its own. **VVV-staked / VCU access is a payment
concern and belongs to Layer 3**, not here — keep the wire and the wallet separate.

## 5. Layer 3 — Treasury and self-sufficiency (starts in Lean)

This is the ambitious core and the first layer that changes *what transitions are
legal*. Per CLAUDE.md, **it starts in the Lean spec**, then conformance tests, then
Rust.

### 5.1 New nouns

- **`Wallet`** — a spend-authority handle for a principal (Venice credits / VVV,
  Akash AKT). Distinct custody from the signing identity (§6).
- **`CostLedger`** — append-only documents recording cost per inference call /
  tool execution / lease interval, with a running balance. Inference cost is
  attributable today: token counts already flow through the response path, and
  `InferenceCall.lean` already models the call.
- **`Budget`** — desired-state policy (apply-owned): spend ceilings, model
  downgrade thresholds, insolvency behavior.
- **`PaymentAction`** — an effect that moves funds (top up Venice, fund an Akash
  escrow). Gated, auditable, and itself attested via Layer 1.

### 5.2 The loop, expressed in existing primitives

- A **`CostTracker` task** (Schedule trigger) periodically rolls completed
  requests/tool calls into `CostLedger` and updates the balance.
- **Budget-gated dispatch**: the trigger engine consults the balance before
  materializing. Below threshold, it emits `FireResult::Skipped { reason }`
  (`trigger_engine/mod.rs:100`) or routes the behavior to a cheaper model — no new
  control path, just a new gate alongside the existing concurrency gate.
- A **`Treasurer` task** tops up credits when the balance is healthy and funds are
  available, emitting `PaymentAction`s.

### 5.3 Why this must be proven, not just coded

A self-funding agent with a treasury wants invariants of the same character as the
existing proven properties (S1 terminal irreversibility, S6 persistence-before-
completion). Candidate theorems for a new `Treasury.lean`:

- **No overspend** — committed spend never exceeds available balance + authorized
  budget.
- **No insolvent dispatch** — a request is never *Claimed* without sufficient
  budget reserved (a reservation/commit discipline, analogous to the scheduler's
  slot accounting in `Fleet.lean`).
- **Insolvency is terminal-and-safe** — an insolvent principal degrades to a
  bounded safe state (pause dispatch) rather than deadlocking or busy-looping.
- **Every spend is attested** — a `PaymentAction` always has a corresponding
  Layer 1 attestation (completeness, analogous to Triggers T4 lineage).

Modeling order: `Treasury.lean` → `state_machine_conformance.rs` /
`lifecycle_regression.rs` → Rust. This is the CLAUDE.md flow, non-negotiable for a
layer that gates legality of transitions on a balance.

## 6. Risks and hard problems

1. **Spend-authority custody ≠ signing custody.** Signing attestations is low-risk;
   authorizing payments is not. The wallet key must be a *separate* identity with
   its own threat model (ideally a remote signer / HSM with spend limits, never the
   filesystem `KeyIdentity`). Conflating them is the single biggest footgun.
2. **Prompt-injection → economic action.** An agent that can spend is an agent an
   attacker wants to drive. `PaymentAction`s must be policy-bounded (rate, ceiling,
   allowlisted payees) *outside* the LLM's control, and every one attested.
3. **Canonicalization drift.** If signed-payload encoding isn't deterministic,
   cross-node `verify` fails. Pin to DefraDB's content-addressing, add a conformance
   test that signs-then-verifies across a serialize/deserialize boundary.
4. **Akash recursion is flashy but least load-bearing.** "Agent rents its own host"
   is a great demo and the weakest part of the core thesis. Sequence it last; don't
   let it gate Layers 1–3.
5. **Anchor availability.** Anchoring must be async and best-effort so anchor
   downtime never blocks the agent. Attestations are valid when signed; anchoring is
   a separate liveness concern with its own retry/backoff.
6. **External-data trust.** Price feeds and balance oracles are untrusted inputs to
   a spending decision. Treat them as adversarial; bound their influence in policy.

## 7. Phasing

| Phase | Deliverable | Risk | Touches Lean? |
|---|---|---|---|
| 1 | `SignedAttestation` schema + `AttestationEmitter` + `AnchorSink` + verify CLI | Low | Optional (attestation completeness lemma) |
| 2 | `Venice` `BackendProviderKind` (API-key path) | Low | No |
| 3a | `Treasury.lean` spec + conformance tests | Med | **Yes (first)** |
| 3b | `Wallet`/`CostLedger`/`Budget`/`PaymentAction` + budget-gated dispatch | High | Drives from 3a |
| 4 | `AgentDeployment` Akash lease manager via `ProcessLifecycleObserver` | High | New deployment model |

Recommended first commit after this doc: **Phase 1**, because it exercises the real
signing path end-to-end and de-risks everything downstream without moving a cent.

## 8. Open questions

- Which Shinzō / Source Network anchoring interface do we target, and what's the
  proof obligation it expects from an anchored document?
- Does Venice need any non-OpenAI request fields that force a bespoke client arm,
  or is `base_url` swap sufficient?
- Where does spend-authority key custody live in the macOS-signing model already
  documented in `docs/macos-signing.md`? Can the remote signer enforce spend caps?
- Is `AgentDeployment` (#9) landed enough to hang an Akash lease manager off, or
  does Layer 4 wait on #9?
