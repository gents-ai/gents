# Pillar 1 — Inference

How a bearer agent pays for tokens-per-day, such that the entitlement is an
asset the agent holds and the sale conveys. Research current as of 2026-06-09;
prices and rates marked † are volatile and must be re-checked at build time.

## Requirements

- **R1 — Bearer entitlement.** The right to inference must be a transferable
  on-chain asset the agent's contract can hold, so it conveys with the title.
- **R2 — Programmatic exercise.** Converting the entitlement into a working
  API key must require no human dashboard step (resurrection is unattended).
- **R3 — OpenAI-compatible wire.** Must slot into the existing
  `BackendProviderKind` / OpenAI client path with at most a new enum arm.
- **R4 — Privacy.** Prompts and corpus content must not be retained by the
  provider; the corpus is the asset being protected.
- **R5 — Predictable daily budget.** The agent's self-maintenance loops need a
  known floor of daily capacity to reason about ("can I afford my checks?").

## The Venice / DIEM model

Venice ([docs.venice.ai](https://docs.venice.ai)) is an OpenAI-compatible
inference provider with a token-gated access model on **Base**:

- **VVV** is the access token (~$16†). Staking it yields a pro-rata share of
  Venice's daily inference capacity, denominated in **Diem** (the accounting
  unit): 1 Diem = $1.00 of API credit per day, reset daily at midnight UTC.
- **DIEM** is also a **transferable ERC-20** (launched Aug 2025), minted by
  locking staked VVV. Each staked DIEM = $1/day of API credit, *in perpetuity*
  ("never expires"). Market price ~$1,430–1,645† per DIEM — i.e. the market
  prices the perpetuity at roughly a 4-year payback. Minimum stake 0.1 DIEM.
- **Key issuance is fully programmatic** (R2 satisfied): the wallet acquires
  and stakes the position, requests a challenge from
  `GET /api_keys/generate_web3_key`, signs it (`personal_sign`), and POSTs
  back to receive an API key — with `expiresAt` and `consumptionLimit`
  parameters, which map nicely onto our epoch model. Spend priority is
  DIEM → bundled credits → USD; an x402/USDC top-up path exists as fallback.
- **OpenAI compatibility** (R3): base URL + Bearer key, stock SDKs work.
  One real gotcha: `venice_parameters.include_venice_system_prompt` defaults
  **true** — Venice injects its own system prompt unless disabled. The
  backend arm must always set it false; the behavior's system prompt is the
  behavior.
- **Privacy** (R4): Venice-hosted open-weights models (GLM, Qwen, Llama,
  DeepSeek, Kimi, ~90 models) are class `private` — zero retention,
  contract-enforced, with TEE and E2EE variants. Proxied frontier models
  (Claude, GPT, Gemini) are only `anonymized` — the upstream provider sees
  prompts. **A bearer agent's default model must be from the `private`
  class**; `anonymized` models are an owner-opt-in.

### What modest agent operation costs

At live per-token rates†, a loop-running agent doing 50–200 calls/day on a
mid-size private model (Qwen3-235B, GLM 4.7, Llama 3.3 70B) consumes roughly
**$0.03–0.72/day ≈ 0.1–1 Diem/day**. So:

- **0.1 DIEM staked (~$150†)** — minimum stake; covers a light agent.
- **1 DIEM staked (~$1,500†)** — comfortable perpetual budget for a working
  agent with steady loops.

This is the "funds in the contract" the README's sale decomposition refers
to: a buyer can price the staked DIEM exactly (liquid market on Aerodrome)
and pays a premium above it for the agent itself.

### Known friction (carried to README risks)

- The web3 key flow authenticates an **EOA** via `personal_sign`; ERC-1271
  contract-wallet support is unconfirmed. The staked position therefore lives
  on the agent's **MPC-derived Base EOA** (secp256k1, derived from the same
  NEAR contract that gates everything else — see [key-custody](./key-custody.md)),
  not on the title contract directly. Transfer of the NFT transfers control
  of that EOA automatically; nothing moves on Base during a sale.
- The web3 key-gen flow documented today authenticates against **staked VVV**;
  whether a pure staked-DIEM position satisfies it is unconfirmed. Worst case
  the agent holds a small sVVV stake alongside DIEM. Verify at build time.
- Per-key `expiresAt` means resurrection must mint a fresh key — which it can,
  unattended (R2), as part of the standard boot sequence.

## Alternatives considered

| Option | R1 bearer | R2 programmatic | R3 wire | R4 privacy | Notes |
|---|---|---|---|---|---|
| **Venice + staked DIEM** | ✅ liquid ERC-20 | ✅ wallet-sig key gen | ✅ (one param gotcha) | ✅ private class | **Recommended.** The only found provider where inference is a *perpetual bearer entitlement*. |
| Venice + USD/x402 credits | ❌ account balance | ✅ x402 USDC | ✅ | ✅ | Good *fallback rail* when DIEM budget is exhausted; not title-conveyable. |
| Direct API key (OpenAI/Anthropic/...) | ❌ key is a liability of a human's account | ❌ KYC/dashboard | ✅ | ❌ provider retention policies vary | What we're escaping; resurrection can't re-mint a key. |
| OpenRouter + crypto top-up | ❌ account balance | partial | ✅ already a `BackendProviderKind` | mixed per-provider | Useful today, but the balance is custodial to the account, not the asset. |
| Bittensor / decentralized inference nets | partial (TAO is bearer, service isn't an entitlement) | partial | ❌ bespoke | varies | Quality/availability not competitive for a dependable daily loop today; revisit. |
| Self-hosted on rented GPU (Akash) | ✅ (it's just compute) | ✅ | ✅ (serve an OAI endpoint) | ✅ strongest | 10–100× the cost floor for an always-on agent; only justified if privacy must exclude even Venice. See [compute](./compute.md). |

## Repo seam

- New `BackendProviderKind::Venice` arm (`backend_provider.rs:5`), reusing the
  OpenAI completions client with Venice's base URL
  (`agent/runtime/context.rs:128`); force
  `include_venice_system_prompt: false`; `discover_models`
  (`backend_provider.rs:92`) works against Venice's `/models` unchanged.
- `InferenceBackend` document points at Venice; the API key is **not** an
  operator-supplied env var but is minted at boot/epoch by the runtime via the
  web3 key flow, signed by the agent's MPC-derived EOA, and held in memory —
  a new, small `KeyMinter` concern owned by the identity/treasury layer, not
  by config.
- Daily Diem budget = the concrete number the treasury layer's budget-gated
  dispatch reasons about (`FireResult::Skipped { reason: "budget" }` — same
  gate shape as the superseded design's §6.2, now with a real denominator:
  Diem/day).
