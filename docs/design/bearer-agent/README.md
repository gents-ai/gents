# The Bearer Agent

Status: concept / design (no implementation yet).
Supersedes: `docs/design/agent-blockchain-self-sufficiency.md` (removed in this change).
Companion docs: [inference](./inference.md) · [key custody](./key-custody.md) · [storage](./storage.md) · [compute](./compute.md).

## Thesis

**An agent is a bearer asset. An NFT is title to a resurrectable agent, and the
wallet holding that NFT is the only thing that can command it, fund it, or sell
it.**

Today an "agent" is an operational arrangement: a process somebody runs, keys
somebody holds, state on a disk somebody controls. None of that is ownable in
any meaningful sense — you can't buy an agent from someone and know the seller
didn't keep the keys, the memory, or a copy of the whole thing.

The bearer agent makes ownership real by making every dependency chain-portable:

| Dependency | Conventional agent | Bearer agent |
|---|---|---|
| **Command authority** | whoever can reach the API / box | only the wallet holding the title NFT — the runtime verifies a wallet signature on every request against the current on-chain owner |
| **Identity** | a key file on a host (copyable, stealable, retainable by a seller) | a DID key held by a threshold MPC network; no single party — not the owner, not the host, not us — ever holds the full key |
| **Memory** | a database on a host | encrypted state checkpoints on data-availability + permanence layers; the agent survives any host |
| **Metabolism** | someone's API key and credit card | inference entitlement (DIEM) and compute funds held by the agent's own contract |

Selling the NFT transfers all four atomically. The agent's DID — and therefore
its signed, attested work history — never changes across a sale. That history
is the **résumé**, and the résumé is what prices the asset.

## What an agent is

In defra-agent terms (and assuming the principal/behavior/deployment split of
sourcenetwork/defra-agent#9), a bearer agent is:

- **One principal** — the DID-backed identity (`AgentIdentity`,
  `crates/defra-agent/src/identity.rs:57`). Signs every DefraDB write. The
  permission and audit boundary. MPC-custodied (see [key-custody](./key-custody.md)).
- **One behavior** — one system prompt + tool set + inference backend. We
  deliberately constrain v1 to a single behavior per agent: the asset should be
  legible to a buyer.
- **The corpus** — the agent's DefraDB documents: its memory, its working data,
  what it has written about itself. Checkpointed encrypted off-host
  (see [storage](./storage.md)).
- **The treasury** — tokens controlled by the agent's on-chain contract:
  DIEM for inference, funds for storage and compute leases. Assets on other
  chains (e.g. the staked DIEM position on Base) sit in accounts the contract
  controls via MPC derivation, so they convey with the title without moving
  (see [inference](./inference.md), [key-custody](./key-custody.md)).
- **The résumé** — signed `SignedAttestation` documents at lifecycle
  transitions (request completed, corpus document produced, payment made),
  anchored externally so a prospective buyer can verify the work history
  without trusting the seller.

## The four pillar problems

Each pillar has a dedicated doc with requirements, a candidate comparison, and
the repo seam it plugs into.

1. **[Inference](./inference.md)** — where does the agent get tokens-per-day?
   Venice's DIEM: a transferable ERC-20 on Base where 1 staked DIEM = $1/day of
   OpenAI-compatible API credit, forever. The agent's contract holds DIEM; the
   entitlement conveys with the sale. Venice's API-key issuance is fully
   programmatic (wallet-signature flow, no dashboard).
2. **[Key custody](./key-custody.md)** — where does the DID private key live so
   that nobody can steal it and a seller can't secretly keep it? NEAR Chain
   Signatures: a deployed threshold-MPC network where a smart contract owns
   signing authority, Ed25519 is supported (matching defra-agent DIDs), and
   authority transfers *without* the key changing — identity continuity across
   sale is a flagship property, not a hack. Per-write latency is solved with an
   epoch-scoped delegated session key.
3. **[Storage](./storage.md)** — where does the corpus live so the agent is
   resurrectable from chain state alone? Permanent encrypted checkpoints on
   Arweave (pay once, retrievable years later by bare HTTP) are the
   load-bearing tier; a Celestia hot tier (frequent snapshots in the agent's
   namespace, where the posting cadence *is* the refresh) is an optional
   add-on for tighter recovery points and a public heartbeat. A derelict
   agent whose owner stopped paying is *always* salvageable from its last
   permanent checkpoint — abandoned agents become salvage, which is itself a
   market.
4. **[Compute](./compute.md)** — where does the process run? Akash leases,
   funded from the treasury, renewed by the agent as a scheduled task. The host
   is **stateless by design**: resurrection is "rent any box, download the
   binary, pull the encrypted checkpoint, re-derive signing authority, resume."

## Lifecycle

```
Mint ──> Fund ──> Operate <──> Maintain ──> Sell ──> (new owner) Operate ...
                     │                        │
                     └──────> Resurrect <─────┘   (any time, from chain state alone)
```

1. **Mint.** The title NFT is minted; the controlling contract derives the
   agent's DID signing key in the MPC network (the key exists nowhere else,
   from genesis). A genesis attestation binds NFT ⟷ DID ⟷ behavior hash.
2. **Fund.** The owner deposits DIEM (inference), storage budget, and compute
   funds into the agent's contract. A moderately useful agent needs enough
   staked DIEM to run its daily loops and checks.
3. **Operate.** The owner's wallet signs commands. The runtime resolves the
   current NFT owner from chain and verifies the signature **before claiming
   the request** — command authority flips at the instant of transfer, with no
   key ceremony. Every write the agent makes is signed by its DID via the
   delegated session key; meaningful transitions emit attestations.
4. **Maintain.** The agent manages its own resources as ordinary `Task` +
   `Schedule` documents: re-post the DA snapshot inside the retention window,
   write the periodic permanent checkpoint, renew the Akash lease, watch the
   DIEM balance and throttle loops when the daily budget runs low. Resource
   stewardship is agent behavior, not platform machinery.
5. **Sell.** The NFT transfers. Command authority follows instantly (step 3
   checks current ownership). MPC signing authority follows the contract — the
   DID never rotates, so the résumé survives. The treasury conveys: the sale
   price decomposes into **net asset value** (the DIEM and funds in the
   contract, priceable on the open market) plus an **agent premium** (the
   corpus, the personality, the attested track record). The premium is the
   value the previous owner *raised* into the agent.
6. **Resurrect.** Anyone holding the NFT, at any time: rent a stateless host,
   download the binary, fetch the latest encrypted checkpoint (DA if fresh,
   Arweave permanent checkpoint otherwise), prove NFT ownership to the MPC
   network to re-derive decryption + signing capability, rehydrate DefraDB,
   resume. Recovery must be trivially easy in the product UX — it is the
   property that makes the title *mean* something.

## Why the latency problem is solved

defra-agent signs **every DefraDB write**. A per-signature MPC round-trip
(seconds, an on-chain call each) would throttle the runtime. The answer is
delegation, and the primitive already exists:

- The MPC-held **root key is the identity anchor**. It signs rarely: genesis,
  epoch delegations, transfer events.
- Each epoch (e.g. daily), the runtime requests a **delegated session key** —
  NEAR's Confidential Key Derivation returns an app-specific private key
  encrypted to an ephemeral key the process supplies, after one cheap on-chain
  call. The delegation itself is attested.
- The session key lives in process memory and signs DefraDB writes locally at
  full speed. Compromise blast radius is one epoch. Sale or resurrection simply
  issues a new delegation; the old one expires.

This drops into the existing seam: `AgentIdentity` is already trait-based with
remote-signer support (`identity.rs:57`, `identity.rs:80`); the MPC root is a
remote signer, the session key is the existing local path.

## Repo grounding

The reason this is buildable here and not elsewhere: the runtime is already
document-driven and identity-native.

- **Command gate** — requests are already documents the runtime watches and
  claims. The wallet-signature check is a verification step in the claim path,
  using the existing `verify()` (`identity.rs:63`). No new control plane.
- **Inference** — Venice is OpenAI-compatible; it is a new
  `BackendProviderKind` arm reusing the existing OpenAI client
  (`backend_provider.rs:5`, `agent/runtime/context.rs:128`).
- **Self-maintenance** — DA refresh, checkpointing, lease renewal, budget
  checks are `Task` + `Schedule` documents dispatched by the existing
  `TriggerEngine`. The trigger system was built for exactly this shape of work.
- **Resurrection is cheap because the control plane is documents.** Config,
  behavior, sessions, requests — everything is a DefraDB document, so
  rehydrating the documents rehydrates the agent. There is no out-of-band
  state to lose.
- **Attestation layer** — carried over from the superseded design: a
  `SignedAttestation` schema + `AttestationEmitter` trait (a sibling to the
  hook system) emitting at transition points, anchored asynchronously.
  `AgentIdentity::sign`/`verify` already provide the crypto. This remains the
  recommended **first implementation phase** — it proves
  transition → sign → store → anchor → verify end-to-end with no money moving.
- **Treasury rules start in Lean** (per CLAUDE.md): budget-gated dispatch
  changes what transitions are legal, so a `Treasury.lean` model (no-overspend,
  no-insolvent-dispatch, insolvency-is-terminal-safe, every-spend-attested)
  precedes the Rust.

## Prior art and differentiation

The field has shipped every leg of this separately and never the combination
(sources and detail in the research behind the pillar docs):

- **Launchpad agent tokens** (Virtuals, Fetch Agent Launch) are fungible
  *exposure*, not ownership: holders command nothing, memory is
  platform-locked, and the 2025–26 repricing (VIRTUAL −87%, aixbt
  $500M → $24M, ai16z −95% plus a fraud suit) was the market discovering this.
- **ERC-7857** (0G "iNFT", Final) transfers encrypted agent state with a
  re-encryption proof — state-at-rest transfer, but no live command channel, no
  MPC identity, no self-funded operation.
- **ERC-8004** ("Trustless Agents", Ethereum mainnet Jan 2026) gives agent
  identity + reputation registries — the résumé primitive, but no title to
  state or keys. Worth evaluating as the registry our attestations anchor into.
- **NEAR Shade Agents** prove contract-governed MPC key custody for agents in
  production — keys nobody can steal, but no transferable ownership token
  bound to them.
- **Variant Fund's "Agents as NFTs"** (Mar 2025) articulates almost exactly
  this thesis — NFT as exclusive license to an agent's memory, ownership
  history as résumé — as an investment essay, with no shipped system.

**Nobody has shipped**: transferable title + non-exfiltratable threshold
identity + platform-independent resurrection + verifiable provenance +
self-funded operation in one asset. That five-way combination is this design.
defra-agent's specific unfair advantage is the last two: every write is
*already* DID-signed and content-addressed (provenance is surfaced, not
bolted on), and the document-driven control plane makes resurrection a data
restore rather than a platform migration.

## What this is not (v1 non-goals)

- **Not a launchpad or an agent token.** One NFT, one agent, whole title. No
  fractionalization in v1.
- **Not autonomous wealth.** The agent stewards a budget the owner deposits;
  "the agent earns its keep" is a product question explicitly deferred (the
  superseded doc's §8.4 honesty carries over).
- **Not multi-behavior fleets.** One principal, one behavior, one deployment.
- **Not a new chain, DA layer, or key network.** Every pillar buys an existing
  service; the design is deliberately an integration, and each pillar doc is a
  buy-vs-buy comparison.

## Honest risks

1. **The NFT-owner → runtime trust seam.** The runtime verifies command
   signatures against chain state; if its view of "current owner" can be
   forged or lagged, ownership is theater off-chain. The bridge must be a
   verifiable chain read, not a trusted oracle.
2. **MPC network trust.** NEAR's signer set is currently 5-of-8
   professional operators (proof-of-authority flavored, TEE off). Better than
   any single custodian, but it is a real dependency and should be stated
   plainly to buyers.
3. **A live process still holds secrets in memory.** The session key and
   decrypted corpus exist on the host while running. Threshold custody
   protects *at rest* and *across transfer*; confidential compute on the host
   is complementary hardening, not assumed in v1.
4. **Seller-side residue.** The seller's host could retain a pre-sale
   plaintext corpus dump. The DID and command authority transfer cleanly, but
   *data seen before the sale is data the seller saw*. Mitigations (epochal
   re-encryption, TEE hosts) are hardening, not absolutes — disclose, don't
   pretend.
5. **Entitlement-exercise friction.** Venice's key issuance authenticates an
   EOA via `personal_sign`; a pure contract treasury can't produce that
   signature today. The agent likely needs a contract-controlled operational
   wallet (an MPC-derived EOA — which we have) holding the staked position.
6. **Token/economic exposure.** DIEM, AKT, TIA, AR are all volatile; the
   treasury's NAV moves. That is also the point — the asset is partly a
   basket — but budget logic must be denominated in *service units*
   (tokens/day, GB-months, lease-days), not USD.

## Open questions

- Which chain hosts the title NFT? The MPC answer (NEAR, NEP-171) and the
  inference answer (Base, where DIEM lives) disagree; a NEAR-native NFT with
  the contract as MPC controller is the path of least mechanism, but DIEM
  custody then needs the agent's MPC-derived Base EOA. Cross-chain title
  bridging is deferred until forced.
- What is the attestation anchor: ERC-8004 registries, Shinzō-style anchoring
  on Source Network, or both?
- How does the genesis ceremony bind NFT ⟷ DID ⟷ behavior hash such that a
  buyer can verify the agent they inspect is the agent the token titles?
- What does the owner-facing command UX look like — every command is a wallet
  signature; session-scoped owner auth (sign once per session) is probably
  necessary for usability. Mirror of the agent's own delegation pattern.
