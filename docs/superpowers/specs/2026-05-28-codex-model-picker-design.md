# Codex Model Picker → InferenceProfile Mapping

## Goal

Make Codex's model picker a real control surface for DEFRA inference profiles. Today the shim advertises a single synthetic model passed in by `--codex-shim-model`. We want Codex's `ModelList` to enumerate `InferenceProfile` documents and Codex's "pick a model" UI to switch which profile the bound `AgentBehavior` uses.

## Non-goals

- Exposing multiple `AgentBehavior`s through the Codex picker. The shim stays bound to one behavior per server run.
- Per-Codex-session profile overrides. Profile selection mutates the behavior document; all sessions on that behavior see the change on their next turn.
- Touching `--codex-shim-timeout-secs` / `--codex-shim-poll-ms`. Tracked as a separate cleanup.
- Adding CRUD for `InferenceProfile` via Codex. The picker is read + select-existing; creating/editing profiles stays in `defra-agent config`.

## Conceptual model

- The Codex shim binds to one `AgentBehavior` at server start (`--codex-shim-behavior-id`, or agent default).
- That behavior's `inference_profile_id` is the source of truth for "current model" as Codex sees it.
- `InferenceProfile` documents already exist (`schemas/inference/inference_profile.graphql`): `profile_id`, `display_name`, `context_window`, `max_output_tokens`, `max_turns`, `temperature`, `stream_batch_ms`, `deadline_duration_secs`.
- One `InferenceProfile` document → one entry in Codex's `ModelList`.

## Protocol surface (handlers/basic.rs)

### `ClientRequest::ModelList`

Replace the current `[model_summary(state)]` single-entry stub with a live query over `InferenceProfile`.

For each profile:

```jsonc
{
  "id": "<profile_id>",
  "model": "<profile_id>",
  "displayName": "<display_name or profile_id>",
  "description": "ctx <context_window> · max <max_output_tokens> · temp <temperature>",
  "isDefault": <profile_id == bound_behavior.inference_profile_id>,
  "hidden": false,
  "supportedReasoningEfforts": [],
  "defaultReasoningEffort": "medium",
  "inputModalities": ["text"],
  "supportsPersonality": false,
  "additionalSpeedTiers": [],
  "serviceTiers": [],
  "defaultServiceTier": null,
  "upgrade": null,
  "upgradeInfo": null,
  "availabilityNux": null
}
```

`description` is built from whichever profile fields are present; absent fields are omitted from the string. If `display_name` is empty, fall back to `profile_id`.

### `ClientRequest::ConfigRead`

`config.model` becomes a fresh read of the bound behavior's `inference_profile_id` (not `state.model`). `model_provider`, `approval_policy`, `sandbox_mode` stay as today.

### `ClientRequest::ConfigValueWrite` and `ConfigBatchWrite`

When the payload sets key `model` to a value:

1. Resolve the target `profile_id`. If no `InferenceProfile` doc has that id, reply with an error result. (Use `codex::ConfigWriteResponse`'s existing error path, or a structured JSON-RPC error — pick whichever the protocol crate already supports.)
2. Update the bound `AgentBehavior` document's `inference_profile_id` via the existing `write_agent_behavior_document` helper in `config_writes`. This ensures the same validation and audit path that `defra-agent config behavior set` uses.
3. Reply with the existing `ConfigWriteResponse` shape on success.

Other config keys keep the current no-op ack.

## CLI surface

- Remove `--codex-shim-model` from `ServerArgs` (`cli/args.rs:350`) and the threading through `bind_codex_shim` (`commands/codex_shim.rs:84,143` and `serve.rs:171-179,250`). The `codex_shim_model` block in `serve.rs:171-179` goes away entirely.
- `state.model` field (`commands/codex_shim.rs:53` area) goes away. Anywhere that reads `state.model` reads the bound behavior's `inference_profile_id` instead — there are three call sites: `protocol.rs:94`, `thread_projection/json.rs:82`, `handlers/basic.rs:76`.
- Keep `--codex-shim-behavior-id`. This is the operator's startup picker for which behavior the shim binds to.
- `--codex-shim-timeout-secs` and `--codex-shim-poll-ms` stay untouched in this work.

## Session pinning fix

`thread_projection/storage.rs:85-113`, the `ensure_agent_session` upsert, currently writes `behavior_id` and `agent_name` in both the `add:` and `update:` clauses. That means restarting the server with a different `--codex-shim-behavior-id` silently rewrites the binding on existing sessions the next time Codex touches them.

Fix: drop `agent_name` and `behavior_id` from the `update:` clause. They become write-once-at-create.

If a session resumes under a server bound to a different behavior than the one stored on the `AgentSession` doc, the shim returns an explicit error on the resume RPC rather than rebinding silently. The exact protocol surface for that error (which Codex method, which error code) is an implementation detail of the resume handler — the spec only fixes the rule.

## Startup precondition

If the bound `AgentBehavior` has no `inference_profile_id` set (or the referenced profile doesn't exist), the shim refuses to start with a clear error: which behavior is bound, that it's missing a profile, and the `defra-agent config behavior set --inference-profile-id …` form to fix it.

This is checked in `bind_codex_shim` after the behavior is resolved, before the WebSocket listener is opened.

## Consequences explicitly accepted

- Two Codex sessions bound to the same behavior: a profile switch in one changes the behavior document, so the other's next turn uses the new profile. This is the price of doc-mutation-as-control-surface and is preferred over introducing per-session override plumbing.
- `--codex-shim-model` removal is a breaking flag change for anyone scripting the shim. The flag is experimental and the shim is gated behind `--codex-shim`, so the blast radius is small.

## Test coverage to add or extend

- `tests/cli_codex_shim.rs`: `ModelList` returns one entry per seeded `InferenceProfile`, with `isDefault` set on the bound behavior's profile.
- `tests/cli_codex_shim.rs`: `ConfigValueWrite { model: "<existing-profile>" }` updates the bound `AgentBehavior` document; a follow-up `ConfigRead` returns the new id.
- `tests/cli_codex_shim.rs`: `ConfigValueWrite { model: "<nonexistent-profile>" }` returns an error and leaves the behavior unchanged.
- `tests/cli_codex_shim.rs`: startup with a bound behavior whose `inference_profile_id` is missing fails fast with a useful error.
- New or extended session-pinning test: simulate a server restart with a different `--codex-shim-behavior-id` and assert that an old `AgentSession`'s `behavior_id` is not rewritten by the next interaction.

## Out of scope (for follow-ups)

- Removing `--codex-shim-timeout-secs` / `--codex-shim-poll-ms` once the polling fallback in `turn/stream.rs:254` is confirmed redundant against the live `updates.recv()` subscription.
- Surfacing `unavailable_behaviors()` state through Codex (e.g. disabled profiles).
- A management UX for creating new `InferenceProfile` documents from inside Codex.
