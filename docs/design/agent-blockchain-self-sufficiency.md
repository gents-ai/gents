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

The endgame is an agent that is an **ownable, self-sustaining asset**: its
ownership is an NFT (the title), an on-chain contract is the front-end that splits
*ownership / contract authority* from *spend authority* from the agent's own
*signing identity*, and the agent uses private compute to bootstrap the data it
stewards and then earns enough to maintain and grow itself. The NFT can be sold —
transferring the whole going concern (corpus, treasury, reputation) to a new owner
without rotating the agent's operational identity.

This doc proposes a five-layer build (Layer 0 is the root of trust), sequenced by
increasing risk:

0. **On-chain ownership & authority** — a Solidity contract is the front-end:
   an ownership NFT denominates the owner; the owner splits spend authority and
   contract authority into separate keys, distinct from the agent's signing
   identity. *(External contract; the root of the trust model.)*
1. **Attestation** — emit signed, content-addressed `SignedAttestation`
   documents at lifecycle transitions; anchor them outward Shinzō-style.
   *(Days–2 weeks. No money moves.)*
2. **Venice backend** — add a `BackendProviderKind` for Venice (OpenAI-compatible),
   giving agents private inference. *(~1 day for the wire; payment is separate.)*
3. **Treasury** — give a principal a wallet, cost accounting, and budget-gated
   dispatch, with authority rooted in the Layer 0 contract. **This is a new state
   machine and starts in Lean.** *(Weeks.)*
4. **Self-provisioned compute** — `AgentDeployment` can request/renew Akash
   leases for its own host. *(New subsystem, sequenced last.)*

Layer 1 validates the whole "lifecycle → verifiable truth" thesis with near-zero
economic risk. Layer 0 makes the custody story easy (§3). Layers 3–4 — closing the
bootstrap-and-grow loop (§8) — are where the genuine difficulty lives.

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
  This is the audit/permission boundary the whole idea hangs on. Crucially, this
  is the *operational signing identity* — it is **not** where funds live and it
  does **not** change when the ownership NFT is sold (§3).

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
trait (§4.2) that the runtime calls at transition points, independent of rig-core.

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
enum variant + a match arm reusing the existing OpenAI client (§5).

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
- Ownership of an agent is an on-chain NFT (title) that can be transferred/sold,
  carrying the going concern — corpus, treasury, attested history — to the buyer
  **without rotating the agent's operational signing identity**.
- Ownership / contract authority, spend authority, and the agent's signing
  identity are three separable keys, split and rotatable via the Layer 0 contract.
- An agent can run inference through Venice for privacy and (eventually)
  token-funded access, and can use that private compute to bootstrap the corpus it
  stewards — with the corpus's provenance itself attested (§8).
- A principal can hold a budget, account for its own inference/compute cost, and
  modulate its behavior to stay solvent — with the solvency rules *proven*, not
  just coded.
- An agent can (eventually) request and renew its own Akash compute.

### Non-goals (initially)

- Building a new chain or consensus. We anchor to existing infrastructure
  (Shinzō / Source Network) and root authority in an EVM contract; we do not
  reinvent either.
- Ever co-mingling spend-authority keys with the signing identity. The Layer 0
  contract makes the split the default; the runtime never holds funds on the
  signing key (§3, §9).
- Real funds in Layers 0–2. Treasury and contract start on testnet / simulated
  balances.
- Multi-region distributed DefraDB sync (out of scope; tracked elsewhere).

## 3. Layer 0 — On-chain ownership and authority

The root of the trust model, and the piece that makes the custody concern *easy*
rather than the biggest footgun. This is an external Solidity contract on an EVM
chain; the runtime treats it as the authority oracle, not as something it
implements.

### 3.1 Three separable authority planes

| Plane | Held by | What it can do | Changes on sale? |
|---|---|---|---|
| **Ownership / contract authority** | the **ownership NFT** holder | set/rotate the spend key, set budget policy, pause the agent, transfer ownership | yes — the NFT *is* this authority |
| **Spend authority** | a contract-scoped **spend key** | move treasury funds within on-chain limits (top up Venice, fund Akash escrow) | optionally — owner may rotate it on transfer |
| **Signing identity** | the agent's **DID** (`AgentIdentity`) | sign DefraDB writes and `SignedAttestation`s | **no** — operational identity is stable across owners |

The contract is the front-end for all three. The owner (NFT holder) calls it to
split spend authority into its own key and to set the budget envelope the spend
key operates within. Because the spend key is *contract-scoped* (the contract
enforces ceilings, rate limits, and allowlisted payees on-chain), a compromised or
prompt-injected runtime cannot drain the treasury beyond what the contract permits.

### 3.2 The NFT as title

Ownership is an ERC-721 (or similar) token. Selling the NFT transfers the entire
going concern atomically: the new owner inherits control of the contract (and thus
budget policy and spend-key rotation), while the agent's **signing DID, its
DefraDB corpus, and its attested history are untouched**. This is the key property
— the asset's *value* (a self-sustaining agent with a curated corpus and a
verifiable track record) survives the ownership change, because operational
identity is decoupled from ownership.

### 3.3 Bridging chain authority into the runtime

The runtime needs to know "who currently holds spend authority / is the agent
paused / what is the budget envelope." Two viable bridges, both reusing existing
primitives:

- **EventTrigger on chain events** — a `TriggerSource` that watches contract events
  (ownership transfer, spend-key rotation, policy change) and writes the resolved
  authority state into DefraDB as runtime-owned config. This fits the existing
  `event_source` model (`trigger_engine/mod.rs:16`) cleanly.
- **MCP bridge / oracle** — an MCP tool service that reads contract state on demand.
  Simpler to start, weaker trust properties.

Either way, the authority state lands as a DefraDB document the treasury layer
(§6) reads before authorizing spend. The bridge must be trust-minimized (§9) —
it is the seam where on-chain truth meets off-chain action.

### 3.4 Why this resolves the custody risk

In the prior draft, "spend keys must be separate custody from signing keys" was
flagged as the single biggest footgun. Layer 0 turns it into a structural default:
funds never touch the signing DID, spend is gated by an on-chain contract with
hard limits, and ownership is just a transferable token. The residual risks (§9)
are about *bounding* the spend key and *trust-minimizing the bridge* — not about
key co-mingling, which the design forbids by construction.

## 4. Layer 1 — Signed lifecycle attestation (the wedge)

The minimum change that proves the central thesis. No money, low risk, high signal.

### 4.1 `SignedAttestation` schema

New collection under `crates/defra-agent-protocol/schemas/` (likely a new
`provenance/` group alongside `inference/` and `services/`), `include_str!`-compiled
like every other schema:

| Field | Type | Notes |
|---|---|---|
| `attestation_id` | ID | content address of the canonical payload |
| `agent_did` | String | signer / principal |
| `subject_kind` | String | `request` \| `tool_call` \| `process` \| `inference` \| `corpus` |
| `subject_id` | String | e.g. `request_id`, `tool_call_id`, corpus doc id |
| `from_state` | String | nullable for genesis transitions |
| `to_state` | String | target lifecycle state |
| `payload_hash` | String | hash of the canonical transition payload |
| `occurred_at` | DateTime | runtime clock |
| `signature` | String | `AgentIdentity::sign(canonical_bytes)` |
| `anchor_status` | String | runtime-owned: `local` \| `anchored` \| `failed` |
| `anchor_ref` | String | nullable; external anchor handle (Shinzō ref / tx) |

Field ownership follows the existing apply/runtime split (CLAUDE.md): the runtime
owns *all* of these — attestations are live-state, never desired-state. The apply
path never writes them. The `corpus` subject kind is what lets bootstrapped data
carry verifiable provenance (§8).

### 4.2 `AttestationEmitter` trait

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

### 4.3 Anchoring

A background `AnchorSink` (its own `TriggerSource` or a periodic task) batches
`anchor_status = local` attestations and publishes them to the external anchor
(Shinzō host / Source Network endpoint), then flips `anchor_status` to `anchored`
and records `anchor_ref`. Anchoring is asynchronous and idempotent: an attestation
is *valid* the moment it is signed; anchoring only makes it *externally provable*.

### 4.4 Verification

`AgentIdentity::verify` (`identity.rs:63`) already closes the loop: any third party
with the DID and the canonical payload can verify a signature. A small
`verify-attestation` CLI subcommand demonstrates end-to-end audit without touching
the runtime.

### 4.5 What Layer 1 proves

End-to-end: *transition → sign → store → anchor → externally verify*, exercising the
real signing infrastructure with no economic surface. Everything else builds on it.

## 5. Layer 2 — Venice inference backend

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
Private inference is also the engine for corpus bootstrap (§8): the agent can
generate and curate data without leaking prompts to a third party.

## 6. Layer 3 — Treasury and self-sufficiency (starts in Lean)

This is the ambitious core and the first layer that changes *what transitions are
legal*. Per CLAUDE.md, **it starts in the Lean spec**, then conformance tests, then
Rust.

### 6.1 New nouns

- **`Wallet`** — a handle that *proposes* spend; actual authorization is rooted in
  the Layer 0 contract's spend key, never in the signing identity (§3.1).
- **`CostLedger`** — append-only documents recording cost per inference call /
  tool execution / lease interval, with a running balance. Inference cost is
  attributable today: token counts already flow through the response path, and
  `InferenceCall.lean` already models the call.
- **`Budget`** — desired-state policy (apply-owned), with the envelope ultimately
  bounded by the on-chain contract: spend ceilings, model downgrade thresholds,
  insolvency behavior.
- **`PaymentAction`** — an effect that moves funds (top up Venice, fund an Akash
  escrow). Gated by the contract, auditable, and itself attested via Layer 1.

### 6.2 The loop, expressed in existing primitives

- A **`CostTracker` task** (Schedule trigger) periodically rolls completed
  requests/tool calls into `CostLedger` and updates the balance.
- **Budget-gated dispatch**: the trigger engine consults the balance + on-chain
  authority state before materializing. Below threshold, it emits
  `FireResult::Skipped { reason }` (`trigger_engine/mod.rs:100`) or routes the
  behavior to a cheaper model — no new control path, just a new gate alongside the
  existing concurrency gate.
- A **`Treasurer` task** tops up credits when the balance is healthy and the
  contract authorizes it, emitting `PaymentAction`s.

### 6.3 Why this must be proven, not just coded

A self-funding agent with a treasury wants invariants of the same character as the
existing proven properties (S1 terminal irreversibility, S6 persistence-before-
completion). Candidate theorems for a new `Treasury.lean`:

- **No overspend** — committed spend never exceeds available balance + the
  on-chain-authorized envelope.
- **No insolvent dispatch** — a request is never *Claimed* without sufficient
  budget reserved (a reservation/commit discipline, analogous to the scheduler's
  slot accounting in `Fleet.lean`).
- **Insolvency is terminal-and-safe** — an insolvent principal degrades to a
  bounded safe state (pause dispatch) rather than deadlocking or busy-looping.
- **Every spend is attested** — a `PaymentAction` always has a corresponding
  Layer 1 attestation (completeness, analogous to Triggers T4 lineage).
- **Authority respected** — no `PaymentAction` is authorized without current
  on-chain spend authority (ties the Lean model to the Layer 0 bridge state).

Modeling order: `Treasury.lean` → `state_machine_conformance.rs` /
`lifecycle_regression.rs` → Rust. This is the CLAUDE.md flow, non-negotiable for a
layer that gates legality of transitions on a balance.

## 7. Layer 4 — Self-provisioned compute (Akash)

The flashiest part and the least load-bearing for the core thesis — an agent that
rents its own host. Sequenced last so it never gates Layers 0–3.

- Hangs off the existing `ProcessLifecycleObserver` seam (`agent.rs:70`): an
  `AkashLeaseManager` observes process state and manages the lease lifecycle.
- Akash deployments are declared in SDL (YAML); leasing is a marketplace auction
  (`createLease` / `deployApplication`) funded in AKT from the treasury escrow,
  gated by the same Layer 0 spend authority as inference top-ups.
- `AgentDeployment` (#9) becomes literal here: a principal's deployment record
  references the active lease, and renewal is a `PaymentAction` like any other.
- There is a published reference pattern (ElizaOS agent on Akash funded via Venice)
  for the exact compute + inference triangle, de-risking the integration shape.

## 8. The bootstrap-and-grow loop

This is the *point* — the layers above are scaffolding for an agent that bootstraps
the data it manages, then sustains and grows itself. It is also where the genuine
open problems live (the earning side is a product question, not a runtime
mechanism).

### 8.1 Phase A — bootstrap the corpus with private compute

The agent uses Venice (Layer 2) to generate and curate the initial dataset it will
steward, *privately* — prompts and intermediate data never leave to a third party.
Each bootstrapped document is written to DefraDB (already signed by the agent's
DID) and gets a `subject_kind = corpus` attestation (§4.1), so the corpus carries
verifiable provenance from genesis: *this agent, at this time, produced this data*.
The corpus's value to a future NFT buyer rests on exactly this attested lineage.

### 8.2 Phase B — maintain

Schedule/Event triggers keep the corpus fresh: re-derive stale views, compact,
re-validate, incorporate new source documents. This is squarely what the existing
trigger/task system already does — maintenance is "just more tasks," now metered by
the `CostLedger` so the agent knows what upkeep costs.

### 8.3 Phase C — grow and earn (closed loop)

The agent provides value from its corpus (serving queries, producing derived/indexed
data à la Shinzō, answering as a paid service), earns into the treasury, and spends
from the treasury on the Venice inference and Akash compute that let it produce more
value. Closed loop: **earn → pay for compute/inference → produce more attested value
→ earn**. Budget-gated dispatch (§6.2) keeps the loop solvent; the proven invariants
(§6.3) keep it safe; the Layer 0 contract lets an owner capture the value by selling
a *demonstrably* self-sustaining, attested asset.

### 8.4 The honest gap

The runtime mechanics of earning (metering, ledgering, gating) are tractable with
the primitives above. **What the agent sells, to whom, and how it gets paid is an
open product question, not a runtime feature.** This design makes the loop
*possible and safe*; it does not by itself make any given agent *profitable*. That
should be stated plainly to anyone evaluating the concept.

## 9. Risks and hard problems

1. **Spend authority is contract-bounded, not co-mingled.** Layer 0 makes the
   custody split structural, but the spend key still needs hard on-chain limits
   (rate, ceiling, allowlisted payees) so a prompt-injected runtime can't drain the
   treasury *up to* its authorized envelope. The contract — not the agent — is the
   enforcement point.
2. **The chain→runtime authority bridge is the new trust seam.** §3.3's bridge
   must be trust-minimized; if the runtime can be fed a forged "you have spend
   authority" document, the contract's guarantees are moot off-chain. Prefer the
   event-watching `TriggerSource` with verifiable chain state over a trusted oracle.
3. **Ownership transfer must be clean.** Selling the NFT must transfer contract
   control and (optionally) rotate the spend key **without** touching the signing
   DID or the corpus. Botching this either bricks the agent or leaks the seller's
   spend authority to the buyer (or vice-versa). Worth its own conformance test.
4. **Canonicalization drift.** If signed-payload encoding isn't deterministic,
   cross-node `verify` fails. Pin to DefraDB's content-addressing, add a conformance
   test that signs-then-verifies across a serialize/deserialize boundary.
5. **Prompt-injection → economic action.** Even bounded, `PaymentAction`s must be
   policy-gated *outside* the LLM's control and every one attested.
6. **Anchor availability.** Anchoring must be async and best-effort so anchor
   downtime never blocks the agent. Attestations are valid when signed.
7. **External-data trust.** Price feeds and balance oracles are untrusted inputs to
   a spending decision. Treat them as adversarial; bound their influence in policy.
8. **Profitability is unproven.** See §8.4 — the loop can be made safe and possible
   without being made profitable.

## 10. Phasing

| Phase | Deliverable | Risk | Touches Lean? |
|---|---|---|---|
| 0 | Solidity ownership/authority contract (NFT title + spend/contract key split) + testnet | Med | No (off-chain) |
| 1 | `SignedAttestation` schema + `AttestationEmitter` + `AnchorSink` + verify CLI | Low | Optional (attestation completeness lemma) |
| 2 | `Venice` `BackendProviderKind` (API-key path) | Low | No |
| 3a | `Treasury.lean` spec + conformance tests | Med | **Yes (first)** |
| 3b | `Wallet`/`CostLedger`/`Budget`/`PaymentAction` + budget-gated dispatch + chain-authority bridge | High | Drives from 3a |
| 4 | `AgentDeployment` Akash lease manager via `ProcessLifecycleObserver` | High | New deployment model |
| 8 | Bootstrap-and-grow loop wired across the above (corpus attestation + metering) | High | Reuses 3a |

Recommended first commit after this doc: **Phase 1**, because it exercises the real
signing path end-to-end and de-risks everything downstream without moving a cent.
Phase 0 (the contract) can proceed in parallel on testnet since it is off-chain
from the runtime's perspective.

## 11. Open questions

- Which EVM chain hosts the contract, and can Source Network / a `TriggerSource`
  watch its events directly (§3.3) or do we need an oracle in between?
- What is the minimal contract surface for the spend/contract authority split —
  is a standard ownable + role-gated treasury enough, or do we need custom limit
  logic on-chain?
- Which Shinzō / Source Network anchoring interface do we target, and what proof
  obligation does it expect from an anchored document?
- Does Venice need any non-OpenAI request fields that force a bespoke client arm,
  or is `base_url` swap sufficient?
- Where does the spend key live relative to the macOS-signing model in
  `docs/macos-signing.md`, and can a remote signer enforce the on-chain caps locally?
- Is `AgentDeployment` (#9) landed enough to hang an Akash lease manager off, or
  does Layer 4 wait on #9?
- **The product question (§8.4):** what does a given agent actually sell, and how
  does revenue land in the treasury?
