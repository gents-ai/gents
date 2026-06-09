# Pillar 2 — Key custody

Where the agent's DID private key lives, such that (a) nobody can steal it,
(b) a seller cannot secretly keep it, and (c) the DID — and the résumé bound
to it — never changes across a sale. Research current as of 2026-06-09.

## Requirements

- **R1 — No full key anywhere, ever.** Not on the host, not with the owner,
  not with us. Threshold/MPC generation and signing only.
- **R2 — Authority follows the title.** The contract that the NFT controls is
  the *only* path to signing; when the NFT moves, signing authority moves,
  with no ceremony and no cooperation from the seller.
- **R3 — Transfer without rotation.** The DID public key must be stable across
  ownership changes — the attested work history is bound to it (the asset's
  premium *is* this résumé).
- **R4 — Curve compatibility.** defra-agent DIDs sign with Ed25519 (or
  Secp256r1). The custody network must produce signatures verifiable under
  the existing `AgentIdentity::verify` path.
- **R5 — Doesn't throttle the runtime.** defra-agent signs every DefraDB
  write; the hot path cannot block on a network signing round.
- **R6 — Bonus: decryption custody too.** The corpus encryption key should be
  governable by the same authority, so "holds the NFT" also gates
  resurrection decryption (see [storage](./storage.md)).

## Recommended: NEAR Chain Signatures

NEAR's MPC network ([docs.near.org/chain-abstraction/chain-signatures](https://docs.near.org/chain-abstraction/chain-signatures))
is a deployed, production threshold-signing service controlled by smart
contracts — in production since May 2024, actively maintained (v3.11.2,
June 2026).

How the requirements land:

- **R1.** Keys are derived from an MPC master key; signing is a 5-of-8
  threshold round across independent professional operators (Everstake,
  Stakin, Aurora, et al.). The derived private key never exists anywhere.
- **R2.** Derivation is `(requesting account, path)` — and the requesting
  account can be a **smart contract**. Our title contract is the predecessor:
  its code ("only the current NFT holder's session may trigger `sign` for
  this agent's path") *is* the authorization policy. NEAR's NEP-171 NFT
  standard makes the gate a few lines: `predecessor == nft_token(id).owner_id`.
- **R3.** Because derivation depends only on `(account, path)`, transferring
  the NFT — or even upgrading the contract — never changes the derived key.
  NEAR documents "sell the controller account, the assets stay at the same
  derived address" as a flagship use case. This is exactly
  transfer-without-rotation.
- **R4.** **Ed25519 (FROST) signing is live** (domain 1, since Apr 2025), with
  raw payloads up to 1232 bytes — DID-style payloads can be threshold-signed
  directly. Secp256r1 is **not** supported; the bearer agent's DID is Ed25519.
- **R5.** Solved by delegation — see below.
- **R6.** **Confidential Key Derivation (CKD)** — domain 2, BLS12-381 — is a
  live endpoint (`request_app_private_key`) that returns a deterministic
  app-specific private key, ElGamal-encrypted to an ephemeral public key the
  caller supplies. The same contract gate that controls signing controls who
  may request derived keys. This gives us **both** the session signing key
  *and* the corpus encryption key from one authority root.

Costs and latency: ~1 yoctoNEAR deposit + <$0.01 gas per `sign`; typical
end-to-end a few seconds; hard timeout 4 minutes. Per-minute cadence is
trivially fine; per-second sustained is unproven — which is precisely why the
hot path never touches it (R5).

### The delegation pattern (R5)

```
MPC root (Ed25519, threshold, contract-gated)        — signs ~once per epoch
  └─ epoch delegation: CKD-derived session key,
     delivered encrypted to the running process       — one on-chain call
       └─ session key signs every DefraDB write       — local, full speed
```

- The root signs a **delegation statement** (DID verification-method style:
  "session key S speaks for DID D until T"), itself attested.
- The session key signs all writes for the epoch. Verifiers accept
  root-or-delegated signatures within the delegation window.
- Blast radius of a host compromise: one epoch. Sale/resurrection: issue a new
  delegation; the old expires at T. CKD revocation is "stop honoring the
  delegation," which the epoch bound gives us for free.
- Repo seam: the MPC root is an `AgentIdentity`/remote-signer implementation
  (the trait at `identity.rs:57` is async and pluggable; `RemoteSigner`
  support at `identity.rs:8` already models exactly this shape); the session
  key uses the existing local key path (`KeyIdentity`, `identity.rs:80`).

### Trust caveats (state plainly, including to buyers)

- The signer set is currently **proof-of-authority flavored**: 8 named
  operators, threshold 5; TEE on the nodes is implemented but **off** on
  mainnet; no routine proactive key refresh (last reshare Mar 2026); a
  misbehaving node can stall (not forge) a signing attempt. The promised
  EigenLayer-restaked permissionless set has not materialized.
- After a key reshare, signing latency can degrade for hours-to-days while
  presignature buffers refill — another reason the hot path must not depend
  on live MPC rounds.
- Prior art that this pattern works in production: **Shade Agents** — on-chain
  contracts gating `request_signature` to attested workers, identity tied to
  the contract, swappable operators, unchanged derived keys.

### A useful side effect

The same contract can derive a **secp256k1 Base EOA** (domain 0) for the
agent. That EOA holds the staked DIEM/VVV position and produces the
`personal_sign` that Venice's key issuance requires
(see [inference](./inference.md)). One authority root — the NFT contract —
thus controls signing identity, corpus decryption, *and* the inference
treasury wallet, across chains, with nothing rotating on transfer.

## Alternatives considered

| Option | R1 no full key | R2 follows title | R3 no rotation | R4 Ed25519 | Status / verdict |
|---|---|---|---|---|---|
| **NEAR Chain Signatures** | ✅ threshold 5-of-8 | ✅ contract-gated derivation | ✅ flagship property | ✅ FROST live | **Recommended.** Production 2yrs, <$0.01/sig, CKD bonus. PoA-ish operator set is the honest caveat. |
| Lit Protocol PKPs | ❌ *(no longer)* | ✅ (legacy) | ✅ (legacy) | ❌ roll-your-own | **Eliminated.** The threshold PKP-NFT architecture was retired Apr 2026; current "Chipotle" is a single Phala TEE, proprietary, Lit-operated — a verifiable cloud HSM, not threshold custody. Three incompatible architectures in 18 months is the platform risk. Legacy PKPs also had a real seller-residue flaw (permissions table survived NFT transfer). |
| Orbis (Source-native) | ✅ DKG+PRE+PSS design | ✅ via SourceHub ACP | ✅ re-share not rotate | presumed | **Not ready yet** (user assessment, matches the superseded doc's open question). The *best eventual fit* — native to DefraDB document encryption — and the design should keep the seam clean so Orbis can replace NEAR CKD for corpus keys later. Track, don't block. |
| Single TEE (enclave-held key) | ❌ one box, one vendor | partial | ✅ | ✅ | In-use protection, not custody: vendor + operator trust, no transfer story. Complementary hardening for the *host* (see compute), not the root. |
| Plain multisig of human keys | ❌ humans hold shares | partial | ✅ | ✅ | Operationally a committee, not an asset property; seller can be a signer. |

## What the title contract must do (minimal surface)

1. Hold/track the NEP-171 token (or be the NFT contract itself).
2. Gate `sign` (domain 1, agent DID path) — epoch delegations and rare root
   acts only — to the current holder's authenticated session.
3. Gate `request_app_private_key` (CKD) for the session-key and corpus-key
   paths, same condition.
4. Gate `sign` (domain 0, Base EOA path) for treasury operations, same
   condition — optionally with allowlisted-calldata constraints (the spend
   policy lives here, on-chain, outside the LLM's reach).
5. Emit events for every grant — the command/audit trail the runtime mirrors
   into DefraDB via the existing event-trigger machinery.

Open question (carried from README): whether the NFT itself lives on NEAR
(NEP-171, least mechanism — recommended start) or is mirrored from an EVM
chain later for marketplace reach. The contract gate doesn't change either
way; only the ownership oracle does.
