# Validate & finalize the Responses default; harvest rig's native providers

**Issue:** #509 (folds in #339; provider-surface input to #438/#439)
**Date:** 2026-06-19
**Branch:** `feat/responses-api-finalize-509`
**Status:** Design — pending user review

## Summary

Issue #509 as filed is a *finalize-and-audit* deliverable: verify the Responses-API
default across backends, decide the fate of the Chat-Completions fallback, fix the
cryptic no-`/v1/responses` failure, and write down the committed backend support set as
the provider-surface requirement feeding the rig-removal epic (#438/#439).

Investigating rig reframes it. The pinned sourcenetwork rig fork
(`81c34131…`) already ships **streaming `CompletionModel` impls for OpenAI Responses,
OpenAI Chat Completions, OpenRouter, Anthropic (native Messages w/ thinking blocks), and
Gemini (native generateContent + Interactions, w/ thinking)** — plus a long OpenAI-compat
tail (vLLM/ollama/llama-cpp/azure/groq/deepseek/xai). Decisively, **every provider
normalizes into the same `StreamedAssistantContent` enum the owned loop already consumes**
(text / tool-call / tool-call-delta / reasoning / reasoning-delta), and provider thinking
surfaces all land as the unified `Reasoning` type. "Use each provider's native API, not an
OpenAI-compat shim" is therefore *already true inside rig* — its `anthropic` module speaks
native Anthropic.

So this work **harvests rig's provider breadth now** to deliver broad native coverage
cheaply, while keeping #438 on the books as-is. The reconciling mechanism is the test
harness: a recording `HttpClientExt` captures the raw wire bytes of every provider
exchange. Those fixtures are **double-duty** — a deterministic CI conformance guarantee
today, *and* the golden wire-contract that Layer-A native providers (#438) must reproduce
later. Every provider added via rig leaves behind exactly the corpus #438 needs to
reimplement it. The harness is the bridge that turns "harvest now" from a bet into a
de-risked staging step.

## Goals

- Native, first-class support for a committed provider set, each exercised over its
  native wire API with reasoning + tools + streaming.
- Wire-API selection (Responses vs Chat Completions) promoted from a process-wide env var
  to a per-backend **document** field — control plane is the database.
- No silent/cryptic failure when a backend doesn't serve `/v1/responses`.
- A record/replay test harness producing a durable, CI-enforced support matrix, with the
  fixture corpus reusable as the #438 provider-surface contract.
- A `docs/` matrix that graduates #492's rationale and is linked from #438.

## Non-goals

- Implementing any native (non-rig) provider client. That is #438/#439. This work
  *produces the requirement and the conformance corpus* for it.
- Removing or weakening the rig dependency. We deepen it deliberately; #438 stays as-is.
- New Lean theorems (see Foundation note — the wiring is plumbing under the existing
  provider-agnostic `PromptAssembly` model).

## Committed support set

| Provider | Wire API | Auth | Notes |
|---|---|---|---|
| OpenAI (hosted) | Responses (`/v1/responses`) | API key | production default |
| Local OpenAI-compat (vLLM / ollama / llama-cpp) | Responses or Chat Completions (per-backend) | none/local | demo's own stack; Responses support varies by server |
| OpenRouter | Chat Completions | API key | aggregator; rig has no Responses for it |
| Anthropic | native Messages | `x-api-key` + version headers | thinking blocks → `Reasoning` |
| Gemini | native generateContent | `key=` / `x-goog-api-key` | thinking config → `Reasoning` |
| ChatGPT-OAuth (#339) | Responses over OAuth bearer | OAuth (`~/.codex`) + refresh | finishes the `chatgpt_codex.rs` seam |

`ChatGptCodex` / OpenAI-compat / OpenRouter `BackendProviderKind` variants exist today;
`Anthropic` and `Gemini` are added.

## Architecture

### A. N-way provider dispatch

The 3-way match in `agent/runtime/context.rs::run_behavior()` becomes N-way over types that
already satisfy rig's `CompletionModel` and already stream `StreamedAssistantContent`. Per
new provider:

- a `BackendProviderKind` variant + serde aliases (`backend_provider.rs`);
- a client-builder reusing rig's `AnthropicBuilder` / `GeminiBuilder` (api-key / base-url /
  headers), in a small per-provider module mirroring `inference_http.rs`;
- a match arm calling the existing **generic** `run_behavior_with_client` — no loop changes;
- a `RenderedRequestSource` variant + `rig_compat::provider_request_json` arm
  (`AnthropicMessages`, `GeminiGenerateContent`) so the rendered-request projection (#503)
  stays whole;
- a `completion_factory::provider_additional_params` mapping (OpenAI reasoning-effort ↔
  Anthropic thinking-budget ↔ Gemini thinking-level/`include_thoughts`).

No new completion loop and no new stream normalization — that is the entire payoff of rig's
unified surface.

### B. Wire-API selection becomes document-driven

The Responses-vs-Chat-Completions choice **only exists for OpenAI-style wire** — Anthropic
(native Messages) and Gemini (native generateContent) each have exactly one wire shape with
no toggle. So the field is named and scoped accordingly: a per-backend **`openai_wire_api`**
field on the `InferenceBackend` document, `responses` | `chat_completions` | `auto`
(default `auto`), governing only OpenAI-style backends.

Per-provider semantics (explicit for every committed provider, no undefined cases):

| Provider kind | `openai_wire_api` honored? | Effective wire |
|---|---|---|
| `OpenAiCompatible` | yes | `responses` / `chat_completions` / probe-resolved `auto` |
| `OpenRouter` | ignored (validated: warn if set) | always Chat Completions (rig has no Responses) |
| `ChatGptCodex` (OAuth) | ignored (validated) | always Responses (OAuth path) |
| `Anthropic` | ignored (validated) | native Messages |
| `Gemini` | ignored (validated) | native generateContent |

Setting `openai_wire_api` on a provider that ignores it is a **config-validation warning**,
not a silent no-op.

- **Resolves "fallback fate":** the Chat-Completions path is **kept and tested**, selected
  per-backend in the control plane, not via a global toggle.
- **Effective-value threading.** Today selection hangs off provider kind plus the legacy global override
  in `agent/runtime/context.rs:135` and `rendered_request.rs:31`. The resolved effective
  `openai_wire_api` must be carried onto `AgentBehavior` (alongside `backend_provider_kind`)
  at reconcile time and *both* of those call sites switched to read it — so the rendered-
  request projection and the runtime client choice agree on one resolved value.
- This was superseded by the 2026-06-26 per-backend wire design: the legacy global override is
  removed rather than demoted, and unset `OpenAiCompatible` backends resolve to Chat Completions.
- The backend probe (already hits `/models`, carries `probe_status`/`last_probe`) is extended
  to detect `/v1/responses`. `auto` resolves via the probe result.
- When an OpenAI-style backend can't serve the selected/`auto` Responses path, emit a **clean
  actionable error naming the field to set** (`openai_wire_api: chat_completions`) — never a
  silent failure on the ollama default stack.

CLI/preset surface (`cli/args.rs`): presets set a sensible default (`openai`→`responses`,
`ollama`→`auto`, etc.); `backend set --openai-wire-api` and `init --openai-wire-api` expose it.

### C. Record/replay test harness (keystone)

A recording/replaying `HttpClientExt` wrapper that captures **the bytes actually sent**,
working for *every* rig provider for free.

**Composable transport (load-bearing).** The existing transport wrappers mutate requests
before sending: `SessionTaggingHttpClient` injects headers
(`inference_http.rs:155`) and `ChatGptCodexHttpClient` patches the request body
(`chatgpt_codex.rs:117`). Each currently holds a *concrete* `inner`, so a recorder wrapped
*outside* them would capture **pre-mutation** bytes — not true wire. The harness therefore
requires these wrappers to become **generic over their inner transport** so the stack
composes with the recorder innermost, closest to reqwest:
`SessionTagging<ChatGptCodex<Recorder<Reqwest>>>`. The `Recorder` then sees exactly what
leaves the process. This generic-ization of the two wrappers is explicit harness scope.

**Redaction contract (mandatory, fixtures hold no secrets).** Because the recorder sits at
true wire, every provider's credential is present at capture: Gemini puts the API key in the
URL query (`?key=…` / `x-goog-api-key`), Anthropic uses `x-api-key`, OpenAI/OpenRouter use
`Authorization: Bearer`, and ChatGPT-OAuth adds `ChatGPT-Account-ID` + bearer. The recorder
applies a **redaction pass before anything touches disk**:
- strip/placeholder a denylist of auth headers (`authorization`, `x-api-key`,
  `x-goog-api-key`, `chatgpt-account-id`, …) and any `key=`/token query params;
- redact bearer/OAuth material in bodies if present;
- a **fixture-scanning test** greps every committed fixture for credential patterns and
  fails CI if any match — secrets in `tests/fixtures/**` are a hard gate, not a review note.
- **Response reasoning** is treated as *controlled test data* (it is model output, not a
  secret) and recorded as-is; the redaction denylist targets credentials and request PII
  only. This is stated so the policy is explicit, not assumed.

- **Record mode** (env-gated, live): dumps redacted request → response(SSE) pairs to
  versioned fixtures under `tests/fixtures/providers/<provider>/`.
- **Replay mode** (default in CI): serves recorded responses by a normalized-request key,
  deterministic and offline. Request keying hashes the normalized wire request (model +
  messages + tools + params, with volatile fields — session ids, timestamps, redacted auth —
  masked). Because identical requests recur across retries and across nodes, **each key maps
  to an ordered multiset whose entries carry a stable per-key ordinal assigned at record time**
  — the ordinal is the ordering source, *not* wall-clock arrival order, so multi-node replay is
  deterministic regardless of how concurrent calls interleave at replay. Replay serves entries
  in ordinal order and asserts **every recorded entry is consumed exactly once** (no unmatched
  request, no leftover fixture) — so a dropped or duplicated provider call fails replay.
- Complementary to **#444** (which mocks at the `CompletionModel` trait seam for
  response-logic tests): HTTP-seam = wire fidelity; trait-seam = loop logic. Two layers.

**Corpus generation — run the multi-node workflow e2e against each provider.** Rather than
hand-authoring per-provider reasoning/tools/streaming trios, run the existing ambitious
multi-agent e2e (the workflow/fleet 5-node test — #378/#511 lineage) against each committed
provider in record mode. One live run harvests a *realistic* trace set — multi-turn, tool
calls, reasoning round-trips, subagent bridging — exactly the shapes we support. That frozen
run becomes the provider's replay corpus; replay reproduces it deterministically in CI.

- **Live tier** (env-gated, extends `tests/e2e_live/`): (re)captures fixtures and smoke-tests
  real endpoints; this is also how fixtures are refreshed when a provider's wire shape moves.

### D. Committed support matrix + docs

A `docs/` page (`docs/backends.md`) with the matrix
(provider × reasoning / tools / streaming / wire-api / auth / fixture-status /
live-verified), graduating #492's Responses-default rationale out of the PR body per the
curation principle, and **linked from #438 as its provider-surface requirement**. The
fixture corpus is named there as the #438 conformance contract.

### E. Foundation note (Lean)

`PromptAssembly` is provider-agnostic: it models the universal tool-call active-block
contract and proves `sanitize` sound (T1), fixpoint (T2/T3/T5), and split-stable (T4). The
strict active-block validity it enforces **implies each committed provider's tool-pairing
requirement** (Anthropic / Gemini / OpenAI all reject a tool-result that doesn't immediately
close its announcing turn — `ProviderValid` is the stricter predicate, so anything it accepts
they accept). Adding providers changes no legal transition, no invariant, and not
*what the model is fed* (every provider consumes the same sanitized native message family).

Therefore: **no new theorems.** Add a conformance note/assertion that each committed
provider's pairing requirement is implied by `ProviderValid`, and treat the wiring as
plumbing per CLAUDE.md — stated explicitly, not silently. (If a future provider's contract
were *looser*, that is a fidelity question, not a soundness one, and out of scope here.)

## Implementation slices

1. **Finish #339 ChatGPT-OAuth** on the proven `chatgpt_codex.rs` seam — real token
   refresh / subscription handling — as the first concrete build.
2. **Effective `openai_wire_api` selection + clean Responses-fallback error.** Add the
   field, thread the resolved effective value onto `AgentBehavior`, switch both selection
   sites (`context.rs:135`, `rendered_request.rs:31`) to read it, demote the env var, extend
   the probe, emit the actionable error. *Lands before fixtures* so the corpus is captured
   against final selection behavior, not behavior we already know will change.
3. **Composable recording transport + harness.** Generic-ize the transport wrappers,
   add `Recorder<Reqwest>` (redaction pass + fixture-scan test + multiset replay keying),
   proven on the OAuth/Responses path.
4. **Anthropic + Gemini native** providers (variant + builder + dispatch arm + render
   variant + params). Generate fixtures by running the multi-node workflow e2e against each.
5. **Matrix doc + #438 linkage + #492 rationale graduation**; conformance note for the Lean
   superset argument.

Order rationale: #339 first (proven seam); wire-api settles selection behavior *before* any
fixture is frozen; the harness needs a settled path to record; Anthropic/Gemini depend on the
harness for their corpus; docs close out.

## Risks & open questions

- **Request-key stability across providers.** The normalized-request hash must mask volatile
  fields without masking semantically-meaningful ones. Mis-keying → replay misses. Resolve in
  the harness slice with a per-provider normalization spec, multiset-per-key ordering, and the
  "every recorded entry consumed exactly once" assert.
- **Secret leakage into fixtures.** Mitigated by the mandatory redaction pass + CI fixture-scan
  gate (§C); the scan is the backstop if a new provider introduces a credential the denylist
  doesn't yet cover — add the pattern and the scan stays green.
- **Fixture churn.** Provider wire shapes drift; live re-capture (tier in C) is the refresh
  path. Fixtures are versioned and reviewed as contract changes.
- **OAuth refresh in CI.** #339's live tier needs real credentials; CI runs replay-only. Live
  capture is a local/operator step, not a CI gate.
- **rig fork divergence.** We deepen reliance on the fork's reasoning-id round-trip. The
  fixture corpus protects us: a fork bump that regresses wire handling fails replay.
