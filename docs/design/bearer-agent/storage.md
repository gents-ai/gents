# Pillar 3 — Storage

Where the corpus lives so that the agent is resurrectable from chain state
alone — by whoever holds the title, years later, with no company, keeper bot,
or surviving host in the loop. Research current as of 2026-06-09; prices
marked † are volatile.

## Requirements

- **R1 — Resurrection-grade durability.** The latest checkpoint must be
  retrievable *years* after the last write, by an arbitrary client, with no
  action by anyone in the interim. This is the property that makes a derelict
  agent salvage rather than loss.
- **R2 — Encrypted, title-gated.** Checkpoints are ciphertext; decryption
  capability must follow the NFT (same authority root as
  [key-custody](./key-custody.md)).
- **R3 — Self-service write path.** The running agent posts its own
  checkpoints as a scheduled task, paying from its treasury, from Rust.
- **R4 — Cheap at our scale.** Checkpoints are single-digit-to-tens of MB
  (encrypted DefraDB document export + behavior/config), posted daily-ish.
- **R5 — Discoverable.** A resurrection bootstrap holding only the NFT (and
  the contract state it points to) must be able to *find* the latest
  checkpoint — addressing must be derivable, not tribal knowledge.

## The shape of the answer: two tiers, one load-bearing

The research forced a clarification the original sketch glossed over: **DA
layers are publication guarantees, not storage.** Celestia's own docs say so
plainly — light nodes sample a 7-day window, pruned full nodes keep ~30 days,
and beyond that you're trusting archival nodes/indexers that the protocol does
not guarantee. EigenDA is a hard ~14-day wall (100,800 Ethereum blocks) with
no archival ecosystem at all. Neither can be the thing R1 rests on.

So the design is:

- **Permanent tier (load-bearing): Arweave.** Pay once (~$0.19† per 10 MB via
  the Turbo bundler), retrievable forever by bare HTTP GET on a tx id through
  a decentralized gateway mesh, merkle-verified, backed by 8 years of
  operation and a storage endowment that has never been tapped. A checkpoint
  posted to Arweave requires *zero* future action to remain retrievable —
  exactly R1. Daily permanent checkpoints cost ~$6†/month; weekly ~$0.80†.
- **Hot tier (optional, high-frequency): Celestia.** ~$0.057† per 10 MB post;
  the agent owns a namespace and posts frequent snapshots (hourly if useful).
  Retrieval by `(height, namespace)` via a laptop-class light node or the
  Celenium indexer's free API. The user's "refresh every window" instinct
  collapses nicely: if you post on a cadence shorter than the window, the
  newest snapshot is always live — *the heartbeat is the refresh*. There is
  direct precedent for exactly this pattern: Hibachi publishes verifiably
  encrypted exchange state as Celestia blobs ("Private Blockspace"), with ZK
  attestations over the ciphertext.

**v1 recommendation: start Arweave-only with daily checkpoints.** One system,
permanent by construction, ~$6†/month, and the resurrection path has no
freshness cliff. Add the Celestia hot tier when checkpoint frequency matters
(active agents whose owners want ≤1h recovery points), and note the hot tier
also gives the agent a public, attestable *heartbeat* — a liveness signal a
marketplace can display. EigenDA is eliminated for this use case (14-day
wall, certificate must be carried out-of-band, no indexer).

## Encryption and gating (R2)

Checkpoints are encrypted client-side by the runtime before posting. The
corpus encryption key is **derived from the same NEAR contract authority as
everything else** (CKD — see [key-custody](./key-custody.md)): the current
NFT holder's session can request the derived decryption key; nobody else can.
This deliberately avoids a second trust network for access control:

- Lit Protocol's condition-gated decryption — the classic "holds NFT X"
  combo with Arweave — regressed in its 2026 architecture change (single
  TEE, conditions now code-it-yourself) and is no longer recommended.
- Walrus + Seal does native NFT-gated decryption, but only if the title
  lives on Sui, which ours doesn't.
- DefraDB-native document encryption with Orbis-managed keys remains the
  eventual stack-native answer; the checkpoint format should keep the
  key-derivation seam clean so Orbis can replace CKD here when it's ready.

Key-rotation note: checkpoints are re-encrypted forward, not retroactively.
A seller could have retained pre-sale ciphertext *and* held the pre-sale key
— this is the "seller saw the data" residue disclosed in the README; epochal
key derivation means post-sale checkpoints are dark to them.

## Discovery (R5)

The title contract stores a small **checkpoint pointer record** updated by
the agent (via its delegated session authority) on each permanent checkpoint:
Arweave tx id, content hash, sequence number, timestamp — and the Celestia
namespace if the hot tier is on. Resurrection reads the contract, fetches the
ciphertext, verifies the hash, requests the decryption key via CKD, and
rehydrates. The pointer update is itself an attested act, so the checkpoint
cadence is part of the public résumé (a buyer can see the agent was
well-kept — like service records on a car).

## Comparison

| Option | R1 years-later | R2 gating path | R3 Rust write | Cost at 10 MB/day† | Verdict |
|---|---|---|---|---|---|
| **Arweave (Turbo bundler)** | ✅ pay-once endowment, no renewal actor | CKD-encrypted blobs | ✅ ANS-104 over HTTP; `arweave-rs` | ~$6/mo, one-time per post | **Permanent tier — recommended, load-bearing** |
| **Celestia (namespace blobs)** | ❌ 7–30d window; archival/indexer best-effort | same | ✅ light node / RPC | ~$1.70/mo | **Hot tier — optional heartbeat; Hibachi precedent** |
| EigenDA | ❌ hard 14d wall, no archive ecosystem | same | ✅ proxy sidecar | ~$7.30/mo (free tier exists) | Eliminated: resurrection fails >14d after last heartbeat |
| Walrus (Sui) | ⚠️ ≤2yr prepay; extensions permissionless but *someone* must act | Seal (Sui-native only) | ✅ Rust CLI/crate | ~$0.01/mo equivalent | Strong runner-up; cheapest; wrong chain for our title, and reintroduces a renewal liveness assumption |
| Filecoin (Lighthouse/FOC) | ⚠️ permanence is a company/endowment-contract promise; ~13% raw retrieval success → gateway dependence | Lighthouse Kavach or Lit | HTTP API | ~pennies–$2.50 one-time | Not chosen: interposes a service between title and data |
| DefraDB P2P replication only | ❌ depends on peers staying alive | native | ✅ already built | ~free | Real and useful *redundancy*, not a guarantee; complements, doesn't replace |
| Sia / Storj | ❌ renter must stay alive / $50-min SaaS | credentials | ok | n/a | Wrong model for bearer recovery |

## Repo seam

- **Checkpoint task** — a `Task` + `Schedule` document: export the agent's
  collections (the control plane is already documents; export = the corpus),
  encrypt with the epoch corpus key, post via Turbo, update the on-chain
  pointer, emit a `subject_kind = "checkpoint"` attestation. All existing
  trigger-engine machinery.
- **Restore path** — a CLI verb (`bearer-agent resurrect <nft>`): read
  pointer → fetch → verify hash → CKD decrypt → import documents → start
  runtime. This is the demo that proves the whole pillar; it should exist
  almost as early as the attestation wedge.
- **Cost ledger** — storage spend (AR via Turbo credits) is a `PaymentAction`
  like any other, attested and budget-gated by the treasury rules.
