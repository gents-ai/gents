# Pillar 4 — Compute

Where the agent's process runs, paid from its own treasury, replaceable at
will — because the host is **stateless by design**. Research current as of
2026-06-09; prices marked † are volatile.

## Requirements

- **R1 — Programmatic end-to-end.** Deploy, fund, monitor, renew, and
  redeploy with no human and no dashboard (the agent — or its resurrection
  bootstrap — does this).
- **R2 — Treasury-payable.** Paid in assets the agent's contract/EOA can
  hold and spend under on-chain policy; no credit card, no KYC.
- **R3 — Disposable hosts.** Any individual host can vanish without data
  loss; recovery = redeploy + rehydrate from [storage](./storage.md).
- **R4 — Verifiable workload.** A buyer should be able to check *what code*
  the agent runs (image pinning at minimum; attestation eventually).
- **R5 — Memory confidentiality (aspirational).** The session key and
  decrypted corpus live in process memory; the host operator is in the trust
  boundary until confidential compute is real.

## Recommended: Akash, with eyes open

Akash is a Cosmos-chain reverse-auction marketplace: declare the workload in
SDL (docker-compose-like YAML), providers bid, a **lease** streams payment
per block from an on-chain **escrow** the tenant funds. Small always-on
containers (1–2 vCPU, 2–4 GB — our shape) run ~$3–10†/month.

What the research confirmed:

- **R1 is genuinely strong.** Full lifecycle via `provider-services` CLI,
  official Go/TS SDKs, and a Console REST API; Cosmos **authz deployment
  grants** let a treasury account fund deployments created by a low-privilege
  deploy key — exactly the spend-policy split we want. Precedent exists: the
  ElizaOS Akash plugin (merged Jan 2025) did autonomous deployment, and Akash
  shipped an "Akash Agents" platform in Q1 2026. No Rust SDK — the runtime
  shells out to the CLI or uses `cosmrs`; the provider-side manifest send
  (mTLS) is the fiddly part.
- **R2: payment is AKT-denominated escrow**; since Mainnet 17 (Mar 2026),
  Burn-Mint Equilibrium gives stable *pricing* (payments burn AKT against an
  oracle-priced internal unit). The exact tenant-side funding denom post-BME
  must be verified against the current CLI at build time. "Renewal" is just
  keeping escrow funded — when it hits zero the provider may close the lease
  without warning, so the **escrow top-up watchdog is a core agent task**,
  not an afterthought (a `Schedule` task: query blocks-remaining, top up from
  treasury, attest the spend).
- **R3 is where our design and Akash agree perfectly.** Persistent volumes do
  *not* survive lease changes, provider migration, or even relaunch on the
  same provider — Akash punishes stateful designs and rewards ours. Provider
  churn is real (63 active, shrinking) and there is **no automatic
  failover**: the agent (or an external watchdog) must health-check itself
  and redeploy on provider death. Since redeploy + rehydrate is the same code
  path as resurrection, provider failure becomes a routine, attested,
  self-healing event — and each occurrence is free advertising for the bearer
  thesis.
- **R4 partially.** Pin the image by sha256 digest in the SDL: the on-chain
  manifest hash then transitively commits to the exact image, giving buyers a
  verifiable "this NFT runs this code" claim. Whether the provider *actually*
  runs that image is unverifiable until attestation ships.
- **R5 is the honest gap.** Confidential compute on Akash is roadmap, not
  product (Kata micro-VMs with TDX/SEV-SNP, estimated Q3 2026†). **Today the
  provider's host root can read container memory** — session key and
  decrypted corpus included. Mitigations now: epoch-scoped session keys cap
  the blast radius; audited/tier-1 provider placement filters; and the spend
  policy lives on-chain where a compromised host can't raise it. When
  TEE-capable providers land, move to them and add attestation to the résumé.

## Alternatives

| Option | R1 programmatic | R2 crypto-paid | R3 disposable | R5 confidential | Verdict |
|---|---|---|---|---|---|
| **Akash** | ✅ CLI/SDKs/REST + authz | ✅ AKT escrow (BME) | ✅ by construction | ❌ until ~Q3 2026† | **Recommended.** On-chain lease = auditable compute spend; agent-self-provisioning precedent exists. |
| Flux | partial (marketplace/API) | ✅ FLUX, from $0.99†/mo | ✅ runs ≥3 redundant instances — built-in failover Akash lacks | ❌ | Strong second; worth supporting as a second provider row for redundancy narratives. |
| Crypto-paid VPS (ExtraVM etc., no KYC) | partial (provider panels/APIs) | ✅ 50+ coins | ✅ (manual) | ❌ (but fewer parties) | The honest baseline: beats Akash on raw reliability/$ today. No on-chain lease/audit trail — which is the point of not choosing it. |
| Phala / TEE-native clouds | ✅ | ✅ | ✅ | ✅ **today** | The R5 answer now, at the cost of a different vendor dependency; NEAR Shade Agents deploy via Phala. Candidate for a "confidential tier" agent SKU before Akash TEE lands. |
| io.net / Spheron / Golem | — | — | — | — | Wrong shape: GPU-cluster aggregators or batch execution, not small always-on CPU containers. |

The Phala row is worth taking seriously: for agents whose corpus is the
asset, "confidential tier" hosting (Phala now, Akash-TEE later) may be the
default and plain Akash the budget tier. The runtime shouldn't care — the
deployment target is one field in a deployment document.

## Resurrection flow (the product moment)

The whole pillar exists to make this boring:

1. Holder runs `bearer-agent resurrect <nft>` (or clicks the button).
2. Bootstrap reads the title contract: checkpoint pointer, image digest,
   deployment spec.
3. Creates an Akash deployment (SDL templated from the spec, image pinned by
   digest), funds escrow from the treasury via the authz grant.
4. On boot the container: requests the epoch session key + corpus key (CKD,
   gated by the holder's authorization — see [key-custody](./key-custody.md)),
   fetches the checkpoint from Arweave, verifies the hash, rehydrates
   DefraDB, starts the runtime, emits a `resurrected` attestation.
5. The agent resumes its schedules — including the escrow watchdog and
   checkpoint task that make the *next* resurrection possible.

Target: minutes, one command, zero credentials typed.

## Repo seam

- `AgentDeployment` (sourcenetwork/defra-agent#9) becomes literal: a document
  recording the active lease (dseq, provider, escrow balance, image digest).
  Runtime owns live-state fields; the apply path owns the desired spec — the
  existing field-ownership split.
- **Lease manager** hangs off `ProcessLifecycleObserver`
  (`crates/defra-agent/src/agent.rs:70`) — the seam the superseded design
  already identified — plus two `Schedule` tasks: escrow watchdog and
  health-check/redeploy.
- Every escrow top-up and lease event is a `PaymentAction` → attested →
  visible in the résumé. "Well-hosted" becomes a verifiable property of the
  asset, like checkpoint cadence.
