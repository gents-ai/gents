# Per-backend OpenAI wire-API selection + vLLM Responses normalization

**Issue:** #566 (delivers #509 slice 2 — "Effective `openai_wire_api` selection + clean Responses-fallback error")
**Date:** 2026-06-26
**Branch:** `feat/openai-wire-per-profile-566`
**Status:** Design — pending user review

## Summary

Make the outbound OpenAI wire API (Responses vs Chat Completions) a **per-backend
document field** instead of a daemon-wide env var, and **normalize** the outbound
Responses assistant-history shape so strict OpenAI-compatible servers (vLLM) accept it.

This is a **clean break**: the `DEFRA_AGENT_OPENAI_CHAT_COMPLETIONS` env var and its
`force_openai_chat_completions()` code path are **removed entirely** — no deprecated
fallback, no migration shim. Wire selection is the control plane (the database) and,
in tests, the same per-backend field.

This delivers #509's implementation **slice 2** (see
`2026-06-19-responses-finalize-multiprovider-design.md` §B). Per-backend placement is
intentional: backends are the wire/transport boundary, so exposing both wire APIs
against one model means two `InferenceBackend` documents (same endpoint/model,
different `openai_wire_api`) — that is the model, not a workaround.

## Decisions (from brainstorm)

1. **Config home:** per-**backend** only (`openai_wire_api` on `InferenceBackend`). No
   profile/behavior override layer.
2. **Scope:** config plumbing **and** vLLM Responses normalization land together in #566.
3. **Normalization home:** a dedicated, independently-testable unit in defra-agent,
   applied at the `rig_compat` Responses seam — not in the vendored rig fork.
4. **No backwards compat:** remove the env var and all old global-override code.
5. **Enum:** `responses | chat_completions`, **default `responses`**. `auto` + the
   `/v1/responses` probe are **deferred to #509** (out of scope here).

## Architecture

### A. The field (control plane)

Add `openai_wire_api: "responses" | "chat_completions"` (default `responses`) to:

- the `InferenceBackend` runtime struct — `crates/defra-agent/src/backend_registry.rs:16`;
- the `InferenceBackend` config document + DefraDB schema (desired-state);
- the CLI surface (`backend set --openai-wire-api`, `init --openai-wire-api`), with
  presets defaulting sensibly (`openai` → `responses`, local vLLM/ollama left to the
  operator; no implicit chat default now that the env var is gone).

Represent the choice with a dedicated typed config enum
`OpenAiWireApi { Responses, ChatCompletions }` (serde `responses` / `chat_completions`),
distinct from the broader `RenderedRequestSource` render-projection enum (which also
covers future Anthropic/Gemini render shapes). Resolution maps `OpenAiWireApi` →
`RenderedRequestSource` at the selection sites. No raw strings past the document boundary.

**Per-provider semantics** (unchanged from #509 §B): only `OpenAiCompatible` honors the
field. `OpenRouter` (always Chat Completions) and `ChatGptCodex` (always Responses)
**ignore** it; setting it on those is a **config-validation warning**, not a silent no-op.

The field is a scalar string in desired-state — no `[]`/`JsonArray` nillable-array trap.
Schema/apply additions follow the existing apply/reconcile path (Collection enum fence);
adding a scalar field to an existing collection is plumbing, but the apply diff/renderer
must round-trip it.

### B. Resolution + threading (one resolved value, both call sites)

Resolve the effective wire API **once** in `behavior_config_from_documents`
(`crates/defra-agent/src/agent.rs:301`, where `backend_provider_kind` is set) and carry
it onto `AgentBehavior` (`crates/defra-agent/src/config.rs:28`) next to
`backend_provider_kind`.

Switch **both** selection sites to read the resolved value (not provider-kind-plus-env):

- the rendered-request projection — `RenderedRequestSource::for_behavior_provider`
  (`crates/defra-agent/src/rendered_request.rs:30`): take the resolved
  `OpenAiWireApi` for the `OpenAiCompatible` arm instead of consulting the env;
- runtime client construction — `crates/defra-agent/src/agent/runtime/context.rs:135`:
  branch on `behavior`'s resolved wire API instead of `force_openai_chat_completions()`.

So the projection (#503) and the actual client agree on one resolved value.

### C. Remove the env var entirely (clean break)

- Delete `force_openai_chat_completions()` and `openai_chat_completions_override_enabled()`
  — `crates/defra-agent/src/inference_http.rs:30`.
- Remove every `DEFRA_AGENT_OPENAI_CHAT_COMPLETIONS` reference (production, CLI, tests, docs).
- **Tests move to the field.** The `cfg(test) => true` branch was the only thing giving
  in-process unit tests Chat Completions; with it gone, the central test backend/behavior
  fixture sets `openai_wire_api = chat_completions` explicitly, and integration tests that
  exported the env var set the field instead. This is the bulk of the removal labor — find
  the shared test backend builder and default it to `chat_completions` so existing
  chat-mock-based tests keep passing; audit any test that genuinely exercises Responses.

### D. Responses history normalization unit

A dedicated `normalize_responses_assistant_items(&mut Value)` (its own module, e.g.
`crates/defra-agent/src/llm/responses_normalize.rs`), applied in the Responses arm of
`provider_request_json` (`crates/defra-agent/src/llm/rig_compat.rs:67`) after rig builds
the body and before serialization/send.

For each prior **assistant** output item it ensures:
- an `id` (e.g. stable/synthesized `msg_*` when absent);
- `type: "message"`, `role: "assistant"`, `status: "completed"`;
- each `content[]` `output_text` item carries `annotations: []`.

These are exactly the fields hosted OpenAI already emits, so the pass is **additive and
safe** on the Responses path generally (it is applied to all Responses renders, not gated
to `OpenAiCompatible`). A fixture/wire-shape **test locks in** that the ChatGptCodex /
hosted-OpenAI Responses bodies are unchanged by the pass, and a second test feeds a
prior-assistant history through and asserts the vLLM-accepted shape.

(Chosen over patching the vendored rig fork: keeps wire-fidelity logic in our code,
aligns with the staged rig removal #438/#439, and avoids the fork-divergence risk #509
flags.)

### E. Clean fallback error

When an `OpenAiCompatible` backend selected for Responses can't serve it, surface an
**actionable** error naming the field to set (`openai_wire_api: chat_completions`) — never
a raw provider 400. (Endpoint-presence detection / `auto` resolution stays deferred to
#509's probe work; this error is the #566-scope guardrail.)

## Test strategy

Per #566 acceptance criteria:

- **render-source selection** from the field (`for_behavior_provider` returns
  Responses/Chat per resolved `openai_wire_api`);
- **runtime client selection** from the field (`context.rs` builds the matching client);
- **vLLM-shape acceptance**: a prior-assistant Responses history, after the normalization
  unit, matches the minimal vLLM-accepted shape (`id` + `output_text.annotations: []` +
  `status`);
- **Chat Completions path still supports tools**;
- **env removal regression**: no code reads `DEFRA_AGENT_OPENAI_CHAT_COMPLETIONS`; the
  shared test fixture drives wire selection via the field.

Gate with the full package suite (`cargo test -p defra-agent`), not `--lib` — integration
tests are separate compile units.

## Out of scope / deferred to #509

- `auto` enum value and the backend `/v1/responses` **probe** (#509 slice 2 remainder).
- The **#545** record/replay fixture harness / provider corpus (#509 slice 3). #566 uses
  plain unit/wire-shape tests for the normalization.
- Native Anthropic/Gemini providers, docs matrix (#509 slices 4–5).

## Coordination

#566 **is** #509 slice 2 (minus the deferred probe/`auto`). Note this on #509 and the
`feat/responses-api-finalize-509` worktree so the `openai_wire_api` field is not
double-implemented; #509 then layers `auto` + probe on top of #566's field.

## Foundation note (Lean)

Pure plumbing under the provider-agnostic `PromptAssembly` model: no new legal transition,
no invariant change, and *what the model is fed* is unchanged (the same sanitized native
message family; normalization only reshapes the already-sanitized Responses serialization
at the wire boundary). No new theorems. The schema/apply field addition rides the existing
apply/reconcile Collection-enum fence.

## Sharp edges

- `tracing`, never `println` (the ignored-field validation warning, the fallback error).
- `graphql::escape_graphql_string()` for any interpolated GraphQL in the apply/read path.
- Scalar field → no `[]`/`JsonArray` trap, but confirm the apply renderer round-trips it.
- Gate with the full `-p defra-agent` suite, not `--lib`.
