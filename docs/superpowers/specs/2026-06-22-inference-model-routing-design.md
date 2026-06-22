# Inference selection, fallback, and model routing — design

Branch: `claude/inference-profile-routing-uyxnxj`

> Status: **design only**. This document proposes the architecture and the
> spec-first cut. No Rust or Lean is written yet. It exists to be reacted to
> on paper before any code.

## Problem

Today the model and inference parameters a conversation runs on are a
**late-bound property of the behavior document**, resolved live at request
time and never snapshotted. Concretely:

- `model_name` + `backend_id` live inline on the **behavior**
  (`agent_behavior.graphql`).
- `InferenceProfile` (`inference_profile.graphql`) holds *sampling + loop
  tuning only* — `context_window`, `max_output_tokens`, `max_turns`,
  `temperature`, stream/liveness timings, deadline. It does **not** carry the
  model or provider.
- Both are materialized into the runtime `AgentBehavior` at reconcile/snapshot
  time (`agent.rs` `behavior_config_from_documents`) from whatever the
  documents *currently* say.

Three consequences fall out of this, and they are the motivation for this
design:

1. **Live-update footgun.** Editing a behavior's profile (or model)
   retroactively changes the meaning of *every* in-flight and future turn under
   that behavior. You cannot have two conversations on one behavior using two
   different profiles. The two obvious workarounds are both bad: *one behavior
   per profile* overloads the identity boundary (a behavior is a
   permission/audit/interface surface; `(did, behavior)` maps to exactly one
   deployment — multiplying behaviors for serving-config reasons pollutes the
   identity space and duplicates prompt+tools, which then drift); *model config
   on the conversation* sounds like conversations owning resolution logic.

2. **No fallback.** There is no way to express "if primary inference is down,
   route to a fallback." This requires *observed health state*, which a pure
   function of the request/behavior documents cannot represent.

3. **No cache-aware routing.** Long conversations want to *pin* to the backend
   instance whose prefix (KV) cache is already warm. This requires *per-
   conversation affinity state* accumulated across turns — again, not a
   function of documents.

The unifying observation: (1) is a **binding-time** problem, and (2)/(3) are
**stateful-routing** problems. The override chain that fixes (1) is necessary
but not sufficient for (2)/(3); behind the same seam we need a stateful engine,
not a lookup.

## Goal

A single, coherent **inference selection** that is:

- **Overridable through a defaulting chain** (request → conversation →
  behavior default) — the exact pattern already used for sampling
  (`sampling_for_request`, `completion_factory.rs`).
- **Resolved by a stateful engine** that consults observed backend health and
  per-conversation cache affinity against a declarative routing policy.
- **Frozen onto the immutable request** as a resolved outcome *plus its
  rationale*, so the rest of the system (and the proven core) only ever sees a
  deterministic, auditable selection.

This gives us, in one mechanism: per-conversation profile/model selection
without forking identity; health-driven fallback; cache-affinity pinning; and a
substrate that cost/difficulty routing later plugs into.

## Non-goals

- **No god state machine.** The engine is *factored* into small fenced
  concerns (below), not one monolithic FSM.
- **No new transcript/provider machinery.** `PromptAssembly` and the message
  family are untouched; selection is strictly upstream of provider-input
  narrowing.
- **No hedged requests in v1.** Tail-latency hedging doubles token cost and
  breaks cache affinity; it becomes an opt-in policy flag later, not a default.
- **No cost/difficulty routing in v1.** It is policy-on-top of this seam and
  ships last.
- **No change to the identity model.** Behavior stays the
  permission/audit/interface boundary; it gains a *default* selection and an
  optional policy reference, nothing more.

## Key decisions

| # | Decision | Rationale |
|---|----------|-----------|
| D1 | Inference selection is a **resolved-at-request-time override chain** (request → conversation → behavior default), then **frozen onto the immutable request** | Mirrors the proven sampling-override pattern; fixes the live-update footgun by making each turn freeze what it ran on while behavior edits still propagate *forward*; gives reproducibility/audit for free |
| D2 | The frozen request carries the **resolved outcome *and* its rationale** (chosen backend+model+profile, policy id, health snapshot ref, attempt #, whether affinity was honored or broken) | The selection engine is nondeterministic and time-varying; freezing the *why* is the only way to keep a verifiable, reproducible record |
| D3 | **Unify model + provider + params into one selectable thing** (an `InferenceSelection`): fold `model_name`/`backend_id` into the profile, or introduce a thin `(backend, model, profile)` binding doc | Today's split (model on behavior, params in profile) is exactly what forces a behavior fork to swap a model; routing needs *one* coherent unit to select/override/route |
| D4 | Routing is a **declarative `RoutingPolicy` document** (candidate set, ordered fallback chains, affinity rules, health thresholds, weights); behavior references a policy the way it references a profile | Keeps "the route map" in the control plane as pure data; behavior holds only a *default*; policy is gossiped like every other document |
| D5 | The resolution engine is **factored into three fenced machines**, not one: (A) backend-health FSM, (B) conversation→backend affinity lease, (C) per-request selection walk | Each is independently provable and composes; a monolith would not survive the foundation flow |
| D6 | Strict precedence: **health (hard) > affinity (soft) > routing policy (soft)** | Fallback (mobility) and cache-pinning (stickiness) pull opposite ways; affinity must be a *preference within the healthy set*, never a hard pin. This is the central safety invariant |
| D7 | Health state is **richer than up/down**: at least `Healthy / Degraded(slow) / RateLimited / Open(down) / HalfOpen` | "Down", "rate-limited", and "slow" demand *different* routing responses (eject / shed overflow / deprioritize); this is where most of the policy expressiveness lives |
| D8 | The selection engine **composes with existing proven machinery** — fleet/scheduler slot accounting, recovery convergence — rather than reinventing it; backend health is **replicated over P2P** | Load-aware routing reuses slot accounting; failover convergence composes with recovery convergence; gossiped health is a DefraDB-native capability a normal stack lacks |
| D9 | **Affinity routing is primarily a self-hosted-fleet feature** | Per-instance KV cache only matters where we control placement (vLLM/SGLang fleets). For hosted providers, prompt caching is provider-side; the relevant concern there is cache-control *breakpoint stability*, which is a `PromptAssembly` concern, not a routing one |

## The membrane: resolve, then freeze

The single idea that makes this safe to add: **the engine is allowed to be
stateful, live, and messy — but its output is frozen onto the immutable
request.** That stamp is the membrane between the live routing world and the
verified transcript world.

```
                      live / stateful / nondeterministic
   ┌─────────────────────────────────────────────────────────────┐
   │  RoutingPolicy doc ── (A) health FSM ── (B) affinity lease    │
   │                         \           /                         │
   │                          (C) selection walk                  │
   └───────────────────────────────┬──────────────────────────────┘
                                    │  resolved InferenceSelection
                                    │  + rationale  (D2)
                ════════════════════▼════════════════════  ← FREEZE onto request
                                    │
   ┌────────────────────────────────┴─────────────────────────────┐
   │  proven core: request lifecycle · PromptAssembly · transcript │
   │  (only ever sees a frozen, deterministic selection)           │
   └──────────────────────────────────────────────────────────────┘
```

The proven core never sees the routing machinery; it sees a frozen selection.
This is why none of the new stateful state leaks into what is already proven,
and why a nondeterministic feature still yields a reproducible audit trail.

## The three fenced machines

### (A) Backend-health FSM

Per `(backend, model)`. Driven by observed success/error/latency/rate-limit
signals (passive outlier detection — no active probing required, though
`HalfOpen` issues a trial).

```
        success                         error rate ≥ threshold
   ┌──────────────┐                    ┌──────────────────────┐
   ▼              │                    ▼                      │
 Healthy ──slow──> Degraded ──errors──> Open ──cooldown──> HalfOpen
   ▲    <─recover──   │  <────────────────┘  trial ok ─────────┘
   │                  │
   └──── RateLimited ─┘   (429: shed overflow, do NOT eject)
```

This is the **failover substrate**. Health is a **document**, so an observation
on node 1 informs routing on node 2 via P2P gossip — we get cross-node health
propagation from replication rather than a bespoke side channel.

### (B) Conversation→backend affinity lease

Per conversation. A **lease** (not a pin) recording "this thread is warm on
instance X", with a TTL. **Consistent hashing** is the assignment primitive:
stable conversation→instance mapping that reshuffles *minimally* when an
instance leaves — i.e. minimal cache loss on failover.

Per D6, the lease is **health-gated**: if X goes `Open`, the lease breaks, the
walk re-pins, and we deliberately eat the cache miss. Affinity is a preference
within the healthy set, never an override of health.

### (C) Per-request selection walk

The fallback walk is itself a tiny FSM and rhymes with the existing request
lifecycle + `reissue_failed`:

```
 Select(primary) ──attempt──> Failed ──> ReSelect(exclude failed, respect health)
        │                                         │
        │ ok                                      ▼
        ▼                                  Select(fallback) ──> … ──> Exhausted
     Bound                                                            (terminal)
```

Fallback is "retry that re-resolves with an exclusion set." On retry it
*must make progress* (the failed backend is excluded) and *must* terminate.

## Distributed-systems patterns: adopt vs defer

| Pattern | v1? | Notes |
|---|---|---|
| Circuit breaker + passive outlier detection | ✅ | The core of failover (machine A) |
| Consistent-hash session affinity | ✅ | Cache pinning (machine B) |
| Per-backend token-bucket rate limiting | ✅ | Feeds `RateLimited`; sheds overflow to fallback |
| Least-outstanding / load-aware routing | ✅ (reuse) | Reuse fleet **slot accounting**; never route to a backend with no slots |
| Admission control / backpressure | ✅ (reuse) | Reuse the **scheduler** |
| Bulkhead | ✅ | Falls out of per-backend health |
| Hedged requests | ⛔ defer | Doubles cost, breaks affinity; opt-in policy flag later |
| Cost/difficulty routing | ⛔ defer | Policy-on-top; ships last |

## Foundation flow (Lean → conformance tests → Rust)

Per `CLAUDE.md`, this starts in the spec. The spec today *deliberately* does
not model inference config (model is an external lookup keyed by behavior, and
`PromptAssembly` is model-agnostic). We lift only the *resolved selection* into
the model — the routing machinery's *output*, at the membrane — not the
provider internals.

**Spec cut (proposed proof obligations):**

1. **Selection lifts into `RequestContext`.** The per-request context (already
   present in `SessionRecovery.lean` / `RuntimeReconcile/State.lean`) gains a
   resolved `InferenceSelection`. The resolver is a **pure function** of
   (request override, conversation default, behavior default, policy, observed
   state); prove determinism given a fixed observed-state snapshot (this is
   what D2's frozen rationale records).
2. **Machine (A) health FSM:** transitions well-formed; no stuck-`Open` without
   a `HalfOpen` trial; **convergence** back to `Healthy` on recovery (liveness,
   same family as the existing recovery-convergence proof).
3. **Machine (B) affinity:** the **health-gates-affinity safety invariant** —
   the walk never selects an `Open`/ejected backend, lease or no lease (D6).
4. **Machine (C) selection walk:** **termination** (`Exhausted` reachable, no
   infinite re-select) and **progress on retry** (failed backend excluded);
   never selects an unhealthy or no-slot backend.
5. **Composition:** routing preserves **slot-accounting** invariants; failover
   convergence **composes with** recovery convergence.
6. **Reissue preserves/re-resolves:** `reissue_failed` either preserves the
   frozen selection or deterministically re-resolves with the failed backend
   excluded (the fallback obligation).

**Conformance tests** fence each of (2)–(6) against the Rust. **Rust** then
satisfies them: the `InferenceSelection` doc unification (D3), the
`RoutingPolicy` doc (D4), the override-chain resolver mirroring
`sampling_for_request`, the three machines, and the freeze-onto-request stamp
(D1/D2).

## Sequencing & delivery

This is genuinely stageable; each stage is independently useful and
independently provable.

1. **Override chain + freeze (fixes the live-update problem).** `InferenceSelection`
   unification (D3), optional override on request + conversation default,
   resolve through the existing chain, stamp resolved selection onto the
   request. Small surface; mirrors the sampling-override code almost line for
   line. Delivers per-conversation profiles without forking identity.
2. **Config-only fallback.** `RoutingPolicy` doc with a static ordered chain +
   the machine-(A) circuit breaker. Delivers "primary down → fallback."
3. **Affinity + health/load-aware routing.** Machine (B) consistent-hash leases,
   slot integration, gossiped health, the richer health states (D7).
4. **Cost/difficulty routing + opt-in hedging.** Policy-on-top.

## Out of scope / follow-ups

- Hedged requests and cost/difficulty routing (deferred above).
- Provider-side prompt-cache breakpoint stability (a `PromptAssembly` concern,
  noted in D9 only to scope it *out* of routing).
- Active health probing (v1 uses passive outlier detection + `HalfOpen`
  trials).
- Any change to the message family, `rig_compat` seam, or transcript shape.
