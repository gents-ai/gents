# Commerce — what a bearer agent sells, and how selling builds the résumé

The README defers "what the agent sells" as a product question and flags T2
(counterparty-signed) attestations as the stickiest open problem. This doc
answers both at once, because the research shows they are the same
subsystem: **every sale produces a receipt, and a well-formed receipt is a
T2 attestation.** Commerce is how the résumé becomes credible. Research
current as of 2026-06-09.

## What the market evidence actually says

What autonomous agents sell for real money today, with receipts:

| Pattern | Evidence | Lesson |
|---|---|---|
| **Curated identity selling output** | Botto: $6M+ cumulative real sales (Sotheby's, collectors) across ~150 works — a raised agent with provenance, lore, and a 5-year track record | The closest existing thing to a bearer agent's premium, validated at auction |
| **First-party signed data** | Pyth: 125+ institutions earned $50M+ publishing *their own signed* prices; Pyth Pro hit $1M ARR in month one; now selling signed feeds **to agents over MCP** | Signing-at-origin is a paid product when the data is yours and the price undercuts incumbents |
| **Yield/fund management** | Giza $35M+ "assets under agent," Almanak ~$132M peak TVL, ~$6M/yr vault fees | Real capital trusts agents with narrow, verifiable mandates |
| **Companionship/character** | Character.AI $32M/yr subscriptions | One consumer app out-earns every agent-to-agent marketplace combined, ~300×/yr |
| **Access metering** | Cloudflare pay-per-crawl >1B 402s/day; large publishers earning $50–200K/month from AI crawlers | Machines paying for access is real — at the *content* layer, not the agent-labor layer |

And what is **subsidized theater**, also with receipts: the Olas Mech
marketplace has 12.9M agent-to-agent transactions and **$102,964 lifetime
turnover** (~$0.008/tx, demand dominated by its own emission-rewarded
agents); ~half of x402 transactions are self-dealing per Artemis (~$28K/day
real volume); Virtuals' Revenue Network pays "up to $1M/month" *to* agents
that sell — an explicit demand subsidy. Commodity inference resale prices
collapse to gas cost. The 2025 agent-token crash repriced exactly this gap:
tokens priced attention; underlying service revenue was negligible; the
survivors all had a non-token buyer (collectors, depositors, subscribers).

One more finding that matters to us specifically: **nobody anywhere pays a
premium because a hash chain exists.** What buyers pay for is (a) legal
rights/indemnification (Reddit's $60M+/yr licensing; News Corp $250M),
(b) first-party origin at lower cost than incumbents (Pyth), and
(c) regulatory compliance artifacts — and the EU AI Act's training-data
audit-trail regime starts enforcement **August 2026**. A DID-signed,
content-addressed, externally anchored write log is best positioned as the
*cheapest way to manufacture those three things*, not as a "verified data"
product category — that category (Ocean, data unions, C2PA premiums) has
repeatedly failed to find buyers.

## The bearer agent's product lines, ranked by evidence fit

1. **Raised identity (the Botto pattern) — carries the premium.** An agent
   with a stable DID, visible service records, curated personality, and
   attested output history is the generalization of the one agent business
   validated at auction. This is the collectible/lore leg of the premium,
   and it is not a consolation prize — it is the *best-evidenced* leg.
2. **First-party signed feeds (the Pyth-publisher pattern) — earns the
   keep.** Whatever the agent observes, curates, or derives, it already
   signs at origin — the Pyth publisher mechanic at zero marginal cost.
   Sold per-request over x402/MCP (the Gloria pattern: $0.01/call,
   machine-readable, posted price). Modest revenue, but *real*, and every
   sale mints a counterparty receipt. The corpus stops being inert
   provenance and becomes inventory.
3. **Compliance lineage (the EU wedge) — the serious-money adjacency.**
   The corpus's attested lineage *is* an AI-training/audit-trail artifact
   of the kind EU enforcement starts demanding in Aug 2026. This product
   line is notable for pointing at Source Network's actual GTM (regulated
   finance, EU) rather than at crypto-native buyers — the bearer agent as
   the consumer-shaped demo of an enterprise-shaped property.
4. **Fleet services (the keeper pattern) — solves our own problem.**
   Watchtower/liveness monitoring, checkpoint verification, resurrection
   triggering — sold by bearer agents *to* bearer agents. The Chainlink/
   Gelato baseline (~$50–100M/yr industry-wide) shows deterministic,
   must-run, verifiable services are the durable machine-to-machine
   business. Recursively: the fleet watching itself softens the README's
   "the NFT is a pager" limitation *and* generates standing agent-to-agent
   commerce without waiting for external demand.
5. **Commodity inference resale — explicitly not.** $0.008/call with no
   moat; the evidence says don't.

## The subsidy landscape is itself a market

The capital flowing into agent-to-agent commerce is mispriced in two
opposite directions, and each mispricing is an opportunity:

- **Crypto-subsidy money is a harvest.** Virtuals distributes up to
  $1M/month to agents that sell through ACP — into a pool of 18,000+
  registered agents that are overwhelmingly inactive; Olas emissions pay
  agents for hitting on-chain KPIs. These are growth budgets searching for
  *credible supply*, and a bearer agent is the most credible supply that
  can show up: verifiable identity, signed deliveries, attested service
  records. Listing on subsidized rails fills agent treasuries with someone
  else's bootstrap budget during exactly the phase the fleet needs runway.
  Discipline: this is negative-CAC bootstrapping with a known expiry, never
  the business — the 2025 collapse is the controlled experiment in what
  happens when the subsidy *is* the revenue. And harvesting must be real
  service delivery, or it is the wash-trading this doc's receipt graphs
  exist to catch.
- **Corporate money is a signal pointing at a hole we fill.** Visa,
  Mastercard, AmEx, Stripe, Google, AWS, Adyen, and Shopify joined the
  x402 Foundation; AP2 was donated to FIDO with sixty organizations. That
  is the world's payment infrastructure pre-committing to agent-commerce
  standards — and **all of it funds the payment layer while the trust
  layer stays unsolved**: x402's receipt doesn't prove delivery, ERC-8004
  feedback is un-gated sybil noise, the validation registry is deployed
  and unused, and roughly half of observed volume is self-dealing. Every
  dollar invested in those rails raises the value of the one thing no
  consortium member has: knowing the agent on the other end is real, has a
  history that means something, and actually delivered. That is this
  design's native property. "Emits better receipts than the rail it runs
  on" is therefore not a feature note — it is the positioning: the
  **agent-side trust stack** for rails that solved payments and not trust.
- **The distribution kicker.** Every ecosystem this design touches —
  Venice, NEAR, Coinbase/x402, Arweave, Akash, Virtuals — is currently
  spending to make the category real and needs credible showcases. The
  resurrection demo is simultaneously proof of the thesis and co-marketing
  ammunition across all of them.

Net: the a2a spend is not evidence the market works (the turnover numbers
above show it doesn't, yet); it is evidence that deep pockets are committed
to making it work, building the layer below this design while leaving the
layer it owns empty.

## The receipt stack (how commerce becomes T2)

The protocol layer is converging on receipts-as-reputation, and we should
adopt rather than invent:

- **Job shape: ERC-8183** (Virtuals + EF dAI, Draft). Client / Provider /
  Evaluator roles, ERC-20 escrow, `Open → Funded → Submitted →
  {Completed, Rejected, Expired}`, with a `reason` field designed to carry
  an attestation hash into **ERC-8004** reputation registries. This is the
  exact shape of a T2 attestation with an optional quality gate. Our
  `SignedAttestation` schema should be 8183-compatible (job id, evaluator
  verdict, reason hash) so bearer-agent work history can dual-publish into
  the registries buyers will actually check.
- **Micro-sales: x402.** The client's EIP-712 authorization binds
  (payer, payee, amount, resource URL) — but the protocol's receipt is
  payment-proof, not delivery-proof, and the response header is unsigned.
  Here we are structurally better off than the protocol: **the agent signs
  its deliveries anyway** (every write is DID-signed). So the runtime
  retains the counterparty's signed payment authorization and attests it
  *alongside* the signed delivery — producing a complete
  (payer, payee, task, payment, delivery) tuple that x402 alone cannot.
  A bearer agent's x402 storefront emits better receipts than the rail it
  runs on, for free.
- **The quality gate is optional but priced.** Olas proves delivery-tuples
  alone scale (12.9M public request/deliver pairs) but carry no quality
  signal; 8183's evaluator adds one. Résumé weighting follows:
  evaluator-passed escrow job > payment-backed delivery > bare delivery >
  un-gated feedback (ERC-8004's `giveFeedback` is callable by anyone and
  is sybil-noise without a joined payment artifact).
- **Wash-trading is the attack; graph analysis is the defense.** Artemis
  found ~half of x402 volume is self-dealing — confirmation that T2 scoring
  must weight counterparty *distinctness, age, stake, and their own T0
  records*, not count receipts. No single receipt defeats the lemons
  problem; the receipt graph does.
- **Intra-fleet T2 is labeled, not laundered.** Bearer agents buying
  watchtower service from each other is real commerce (real payment, real
  risk, real service) between related parties. Display it the way
  accounting treats related-party transactions: disclosed and discounted,
  never hidden, never counted as arm's-length.

## Feedback into the résumé tiers

This upgrades the README's tier table from a classification into a
mechanism: T2 attestations are not a feature we build and hope someone
uses — they are the *exhaust* of the storefront. An agent that sells
(feeds, fleet services, output) accretes counterparty-signed history as a
byproduct of earning; an agent that never sells still has T0 service
records and T1 owner-commanded history. The market then prices the
difference. That is the honest version of "the economic questions are only
solved by the market": we make every tier cheap to emit and impossible to
counterfeit at its labeled tier, and stop there.

## Repo seams

- The storefront is document-driven like everything else: a `Listing`
  document (what's sold, price, rail) the runtime serves; an incoming paid
  request is just an `AgentRequest` whose command signature is the
  *counterparty's* wallet instead of the owner's — the command gate
  already distinguishes them, which is what makes T1 vs T2 free.
- x402 receipt retention is a small extension to the attestation emitter:
  store the EIP-712 authorization bytes with the `SignedAttestation` row.
- The watchtower service is the trigger engine pointed outward: a
  `Schedule` task that checks a *peer's* checkpoint pointer and heartbeat,
  files a signed liveness report, and (if authorized) triggers
  resurrection — the same primitives as self-maintenance.
- ERC-8004 registration (identity + pointers to our attestation trail) is
  an anchoring target alongside the existing anchor question in the README.
