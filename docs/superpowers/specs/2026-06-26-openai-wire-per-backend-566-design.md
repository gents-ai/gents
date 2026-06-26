# Per-backend OpenAI wire-API selection + vLLM Responses normalization

**Issue:** #566 (delivers #509 slice 2 subset — effective `openai_wire_api` selection + vLLM Responses normalization)
**Date:** 2026-06-26
**Branch:** `feat/openai-wire-per-profile-566`
**Status:** Design — revised after code-path review (pending user review)

## Summary

Make the outbound OpenAI wire API (Responses vs Chat Completions) a **per-backend
document field** instead of a daemon-wide env var, and **normalize** the outbound
Responses assistant-history shape so strict OpenAI-compatible servers (vLLM) accept it.

This is a **clean break**: the `DEFRA_AGENT_OPENAI_CHAT_COMPLETIONS` env var and its
`force_openai_chat_completions()` code path are **removed entirely** — no deprecated
fallback, no migration shim. Wire selection is the control plane (the database) and, in
tests, the same per-backend field.

Per-backend placement is intentional: backends are the wire/transport boundary, so
exposing both wire APIs against one model means two `InferenceBackend` documents (same
endpoint/model, different `openai_wire_api`).

> **Revised after a code-path review.** The first draft mis-placed normalization on the
> capture/projection path and under-counted the touch surfaces. Corrections are folded in:
> normalization moves to the real outbound `HttpClientExt` seam (§D); the resolved value
> must enter the reconcile fingerprint (§B); a third client-construction site (`oneshot.rs`)
> is enumerated (§B); the clean-fallback error is deferred to #509 (§E); the full schema/
> export/desktop surface is enumerated (Implementation surface).

## Decisions (from brainstorm + review)

1. **Config home:** per-**backend** only (`openai_wire_api` on `InferenceBackend`). No
   profile/behavior override.
2. **Scope:** config plumbing **and** vLLM Responses normalization land together in #566.
3. **Normalization home:** a dedicated normalizer applied at the **real outbound
   `HttpClientExt` transport seam** (not the capture path, not the rig fork); the *same*
   normalizer is reused for the rendered-request capture so projection matches wire.
4. **No backwards compat:** remove the env var and all old global-override code.
5. **Storage:** the field is `Option<OpenAiWireApi>` (None = unset) so "explicitly set"
   is distinguishable from "defaulted" (needed for the ignored-provider warning).
6. **Enum:** `responses | chat_completions`; **unset resolves to `chat_completions`** for
   `OpenAiCompatible` (safer for the local/demo stack that drives #566; hosted OpenAI
   declares `responses` explicitly). `auto` + the `/v1/responses` probe + the
   clean-fallback error are **deferred to #509**.

## Architecture

### A. The field (control plane)

Add `openai_wire_api: Option<OpenAiWireApi>` to the `InferenceBackend` model and document,
where `OpenAiWireApi { Responses, ChatCompletions }` is a dedicated typed enum (serde
`responses` / `chat_completions`), distinct from the broader `RenderedRequestSource`
render-projection enum. `None` (omitted) means unset.

**Per-provider resolution semantics:**

| Provider kind | field | effective wire |
|---|---|---|
| `OpenAiCompatible` | `Some(x)` → `x`; `None` → **`chat_completions`** (default) | honored |
| `OpenRouter` | `Some(_)` → **validation warning**; ignored | always Chat Completions |
| `ChatGptCodex` | `Some(_)` → **validation warning**; ignored | always Responses |

Storing `Option` (not a defaulted value) is what lets the warning fire only when an
ignoring provider has the field **explicitly** set — precedent: `api_key`/`api_key_env_var`
are already `Option<String>` with no serde default (`desired_state/mod.rs:255`).

The field is a scalar string in desired-state — no `[]`/`JsonArray` nillable-array trap.

### B. Resolution + threading (one resolved value, every site)

Resolve the effective `OpenAiWireApi` **once** in `behavior_config_from_documents`
(`agent.rs:301`, where `backend_provider_kind` is set) and carry it onto `AgentBehavior`
(`config.rs:28`).

**Two threading obligations:**

1. **Reconcile fingerprint (load-bearing).** `AgentBehavior` has a *manual* `Debug` impl
   (`config.rs:86`) and runtime fingerprints hash `format!("{behavior:?}")`
   (`runtime_snapshot.rs:521`). A new field is **invisible to reconcile** unless added to
   that manual `Debug` — so switching a backend's `openai_wire_api` would not trigger a
   generation swap. The resolved wire API **must** be added to the manual `Debug` output.
2. **Every selection site reads the resolved value** (not provider-kind-plus-env). There
   are **three** client-construction sites plus the projection — all currently keyed off
   `force_openai_chat_completions()`:
   - runtime daemon client — `agent/runtime/context.rs:135`;
   - **oneshot client** — `run_openai_oneshot_with_tools`, `oneshot.rs:72` *(missed in the
     first draft)*;
   - rendered-request projection — `RenderedRequestSource::for_behavior_provider`,
     `rendered_request.rs:30` (maps `OpenAiWireApi` → `RenderedRequestSource`).
     The projection context must also carry whether Responses normalization is active
     for this behavior (e.g. `normalize_responses_wire: bool`) so capture normalization
     is applied only when the outbound transport wrapper is applied.

### C. Remove the env var entirely (clean break)

- Delete `force_openai_chat_completions()` + `openai_chat_completions_override_enabled()`
  (`inference_http.rs:30`) and every `DEFRA_AGENT_OPENAI_CHAT_COMPLETIONS` reference
  (production, CLI, tests, docs).
- **Tests largely keep working by default.** The `cfg(test) => true` branch defaulted
  in-process unit tests to Chat Completions; with unset now also resolving to
  `chat_completions`, those tests keep getting Chat **for free** — removing the env code is
  low-risk. The labor is: delete the env-setting in integration tests (now redundant) and
  set `openai_wire_api = responses` only on the tests that genuinely exercise Responses.
- **Breaking change (no migration shim).** With unset → `chat_completions`, a **hosted
  OpenAI** backend (production default was Responses) must now declare
  `openai_wire_api: responses`. Local OpenAI-compatible/demo backends work by default.
  This is the explicit no-backwards-compat trade-off; documented as a breaking change in
  the release notes. The `openai` CLI preset writes `openai_wire_api: responses`
  explicitly so newly initialized hosted-OpenAI configs keep using Responses; generic and
  local OpenAI-compatible presets omit it unless the operator passes `--openai-wire-api`.

### D. Responses history normalization at the real outbound seam

The bytes vLLM receives are produced by rig's `CompletionModel::stream`
(`loop_stream.rs:230`), **not** by `rig_compat::provider_request_json` (which only feeds
the optional `on_rendered_request` capture). Normalization therefore lives at the
`HttpClientExt` transport seam, the same place `ChatGptCodexHttpClient::patch_instructions_body`
(`chatgpt_codex.rs:1057`) and `SessionTaggingHttpClient::tag` (`inference_http.rs:103`)
mutate the real request before send.

- Add a `ResponsesNormalizingHttpClient<H>` `HttpClientExt` wrapper, **generic over its
  inner transport** (so it composes innermost in the stack — cf. #509 §C composable
  transport: `SessionTagging<ResponsesNormalizing<Reqwest>>`). Its `send` **and
  `send_streaming`** paths parse the JSON body, run
  `normalize_responses_assistant_items`, and forward to `inner`; `send_multipart` is a
  pass-through because Responses completion requests are JSON bodies.
- The shared normalizer `normalize_responses_assistant_items(&mut Value)` (its own module,
  e.g. `llm/responses_normalize.rs`) ensures each prior **assistant** output item has an
  `id`, `type: "message"`, `role: "assistant"`, `status: "completed"`, and each
  `output_text` content item carries `annotations: []`. Additive and matches what hosted
  OpenAI emits.
- **Composition:** wrap the transport only for `OpenAiCompatible` backends resolved to
  Responses. This keeps the proven ChatGptCodex client stack byte-unchanged. Hosted OpenAI
  also uses `OpenAiCompatible`, so it receives the wrapper when explicitly configured for
  Responses; the guarantee there is semantic/idempotent compatibility, not byte identity
  when rig omitted fields the normalizer adds. Tests assert hosted-OpenAI-shaped bodies
  that already include those fields are unchanged by the normalizer.
- **Projection fidelity:** call the *same* normalizer inside `rendered_completion_request`
  (`rig_compat.rs:35`) **only when the render context says the outbound wrapper is active**,
  so the captured/projected body matches the bytes actually sent. `ChatGptCodex` still
  renders as `OpenAiResponses`, but its context sets normalization inactive because that
  client stack does not get the wrapper.

### E. Clean fallback error — DEFERRED to #509

The actionable "this backend can't serve Responses → set `openai_wire_api: chat_completions`"
error needs behavior-aware classification wrapped around the generic stream-error path
(provider 400/404s are currently classified without knowing "OpenAiCompatible selected
Responses"). That pairs naturally with #509's `auto`/probe work and is **out of #566
scope**. #566's normalization (§D) already removes the most common vLLM Responses failure
(the bad assistant-history shape); a backend with no `/v1/responses` at all still surfaces
the raw provider error until #509 lands the probe + hinted error.

## Implementation surface checklist

Adding the field touches every place that enumerates `InferenceBackend` fields (all
confirmed present):

- runtime struct — `backend_registry.rs:16`;
- protocol row — `defra-agent-protocol/src/row.rs:642` (`InferenceBackendRow`);
- SDL — `defra-agent-protocol/schemas/inference/inference_backend.graphql`;
- desired-state strict deser — `cli/src/desired_state/mod.rs:270` (`#[serde(deny_unknown_fields)]`,
  so the field **must** be added or strict import breaks) + import/export + JSON-schema gen;
- CLI export fields — `cli/src/main.rs:385` (`EXPORT_INFERENCE_BACKEND_FIELDS`);
- desktop query fields — `defra-agent-desktop-core/src/client/query.rs:45`;
- CLI surface — `backend set --openai-wire-api`, `init --openai-wire-api`;
- preset behavior — `openai` writes `openai_wire_api: responses`; local/generic
  OpenAI-compatible presets omit it unless the operator explicitly selects a wire API;
- runtime: `AgentBehavior` field + **manual `Debug`** (`config.rs:86`), resolution in
  `agent.rs:301`, three selection sites (§B), env removal (§C), transport wrapper (§D).

## Test strategy

- **render-source selection** from the field (`for_behavior_provider` maps resolved
  `OpenAiWireApi` → Responses/Chat);
- **runtime + oneshot client selection** from the field (both build the matching client);
- **transport-level normalization**: capture the **real rig-generated body** through the
  `ResponsesNormalizingHttpClient` on both `send` and `send_streaming`, and assert the
  vLLM-accepted shape (`id` + `output_text.annotations: []` + `status`) — this is the
  test that would have caught the blocker; a capture-only test would not;
- **projection == wire**: for OpenAiCompatible Responses, the rendered-request capture
  matches the normalized sent body; for ChatGptCodex Responses, capture remains
  unnormalized to match its unwrapped outbound stack;
- **ChatGptCodex unchanged / hosted OpenAI idempotent**: ChatGptCodex does not get the
  wrapper; hosted-OpenAI-shaped bodies with the required fields already present are
  unchanged by the normalizer;
- **Chat Completions path still supports tools**;
- **reconcile**: changing a backend's `openai_wire_api` produces a new behavior fingerprint
  (regression for the manual-`Debug` obligation);
- **env removal**: no code reads `DEFRA_AGENT_OPENAI_CHAT_COMPLETIONS`; the shared test
  fixture drives wire selection via the field.

Gate with the full package suite (`cargo test -p defra-agent`), not `--lib`.

## Out of scope / deferred to #509

- `auto` enum value + the backend `/v1/responses` **probe** (#509 slice 2 remainder).
- The clean-fallback **error** (§E) — coupled to the probe.
- The **#545** record/replay fixture harness / provider corpus (#509 slice 3). #566 uses
  unit + transport-level wire-shape tests.
- Native Anthropic/Gemini providers, docs matrix (#509 slices 4–5).

## Coordination

#566 **is** #509 slice 2 (minus deferred probe/`auto`/error). Note this on #509 and the
`feat/responses-api-finalize-509` worktree so the `openai_wire_api` field is not
double-implemented; #509 then layers `auto` + probe + hinted error on #566's field.

## Resolved product decision

The #509 divergence (a product/migration call) is **resolved**: #566 removes the env var
and defaults unset → **`chat_completions`** (no `auto`/probe in #566). Rationale: the
local/demo OpenAI-compatible stack that drives #566 works with no extra config; hosted
OpenAI deployments declare `openai_wire_api: responses` explicitly. #509 later adds `auto`
+ the probe so the default can self-resolve; until then this is the committed behavior and
a documented breaking change.

## Foundation note (Lean)

Pure plumbing under the provider-agnostic `PromptAssembly` model: no new legal transition,
no invariant change, and *what the model is fed* is unchanged (the normalizer reshapes the
already-sanitized Responses serialization at the wire boundary only). No new theorems. The
schema/apply field addition rides the existing apply/reconcile Collection-enum fence.

## Sharp edges

- `tracing`, never `println` (the ignored-field validation warning).
- `graphql::escape_graphql_string()` for any interpolated GraphQL in apply/read.
- Scalar field → no `[]`/`JsonArray` trap, but confirm the apply renderer round-trips it.
- `deny_unknown_fields` on the desired-state Wire struct: the field must be added there or
  strict import of any backend with it set fails.
- Gate with the full `-p defra-agent` suite, not `--lib`.
