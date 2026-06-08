# rig-core Owned Loop Audit and Design

**Issue:** #400

**Date:** 2026-06-08

**Scope:** dependency-surface audit, sizing, and go/no-go for replacing
`rig-core` with a Defra-owned completion and tool loop.

## Summary

The current `rig-core` dependency is not just a thin provider client. It is a
shared shape for the daemon's model type, agent builder, multi-turn loop,
streaming events, prompt hook callbacks, tool trait, tool definitions, message
model, provider clients, completion errors, token usage, compaction calls, title
generation, one-shot execution, transcript presentation, and tests.

The strongest reason to own the loop is real: Defra needs native control over
in-loop context management. The current hook can truncate the persisted tool
result and can persist a shorter model observation, but it cannot guarantee that
rig's in-memory working message for the same turn is rewritten before the next
completion request. That means an oversized tool result can still affect the
same multi-turn request, even if future requests reload bounded history from
DefraDB.

The recommended decision is:

- **Go** for an adapter-first owned-loop spike.
- **No-go** for an immediate full `rig-core` removal.
- Keep `rig-core` as a provider/tool/message compatibility adapter while Defra
  first owns the request loop and context/threading semantics.

This gives us the product value - bounded in-loop tool observations, native
cancel/deadline behavior, and a single Defra lifecycle model - without taking on
provider streaming quirks all at once.

## Current rig Dependency Surface

### Dependency declarations

- `Cargo.toml` pins `rig-core = "0.35.0"`.
- `crates/defra-agent`, `crates/defra-agent-cli`, and
  `crates/defra-agent-protocol` inherit the workspace dependency.

### Runtime construction

`completion_factory.rs` builds rig `Agent` values from rig `CompletionClient`
implementations. It configures:

- model name and preamble
- max turns
- tool choice
- temperature and max tokens
- provider-specific additional params
- request-specific OpenAI cache scope params
- admission-wrapped clients

`agent/runtime/context.rs` constructs provider clients for:

- OpenAI-compatible backends via `rig::providers::openai`
- OpenRouter via `rig::providers::openrouter`
- ChatGPT Codex via the custom HTTP client and rig's OpenAI provider shape

`oneshot.rs` repeats the same provider and agent construction path for one-off
execution.

### Daemon and multi-turn loop

`agent/daemon.rs` stores `rig::agent::Agent<M>` and keeps the daemon generic over
`rig::completion::CompletionModel`.

`agent/daemon/inference.rs` is the central live dependency. The daemon calls:

```rust
agent
    .stream_prompt(&request.content)
    .with_history(history.to_vec())
    .with_hook(hook)
```

The daemon then wraps the returned stream with:

- admission accounting
- request deadlines
- shutdown
- interrupt cancellation
- stream liveness timeout
- workspace-aware tool execution context
- in-flight tool lifecycle cleanup
- retry classification
- persisted stream finalization

Today rig owns the inner completion -> tool-call -> tool-result -> next
completion iteration. Defra owns almost all outer lifecycle behavior.

### Streaming event model

`agent/stream_processor.rs` consumes rig `MultiTurnStreamItem` and
`StreamedAssistantContent` / `StreamedUserContent` variants. It turns those into:

- streamed response rows
- persisted assistant messages
- tool call identity mapping
- persisted tool result messages
- final response materialization
- partial-turn persistence on interrupt or error

`admission/stream_guard.rs` wraps rig `StreamingCompletionResponse` and maps
provider stream events back into `RawStreamingChoice`, while holding and
releasing admission permits.

Replacing the loop requires a native stream item model with equivalent events:
assistant text, reasoning, tool call, tool-call delta, tool result, final
response, message id, usage, and provider errors.

### Prompt hook and persistence

`hook/persistence/prompt_hook.rs` implements rig `PromptHook<M>` for
`DefraSessionHook`.

The hook persists:

- user prompts
- assistant responses
- regular tool calls
- subagent tool calls
- background process tool calls
- tool results
- lifecycle transitions
- command denial metadata
- managed timeout/cancel terminal states

This is the main impedance mismatch. `PromptHook::on_tool_result` can return a
rig `HookAction`, and Defra can persist a truncated message, but the hook does
not own rig's internal working transcript. A Defra-owned loop should replace the
hook with explicit loop steps:

1. receive model event
2. persist/record lifecycle state
3. execute or skip tool
4. compute the exact model-facing tool observation
5. append that observation to the working context
6. continue or terminate

### Tool surface

The callable tool surface is rig-shaped today:

- `toolset.rs` returns `Vec<Box<dyn rig::tool::ToolDyn>>`.
- `tool_surface/mod.rs` builds host, MCP/meta, subagent, background, custom,
  context-budget, session-history, optional memory, and `defra_query` tools as
  rig tools.
- `tool_call_lifecycle/runtime.rs` wraps `ToolDyn` to attach request deadlines,
  cancellation, workspace cwd, lifecycle state, and runtime failure classes.
- `skills.rs`, `defra_query`, `meta_tools`, `toolset/file_tools.rs`,
  `toolset/bash_tools.rs`, `toolset/subagent.rs`, `toolset/context_budget.rs`,
  `toolset/session_history.rs`, and feature-gated `toolset/memory.rs` implement
  rig `Tool` or `ToolDyn` and return rig `ToolDefinition`.

This is a large but mostly mechanical surface. A Defra-native tool trait can
wrap existing rig tools during migration, but removing rig completely means
porting every tool definition and call implementation.

### Message model and transcript persistence

The persisted transcript uses rig message structures:

- `session/history.rs` decodes persisted rows into rig `Message`.
- `defra-agent-protocol/src/transcript.rs` presents rig `Message`,
  `AssistantContent`, `UserContent`, `ToolResult`, and reasoning blocks.
- `prompt.rs`, `compaction/history.rs`, `background_tools.rs`, and
  `trace_export.rs` read or render rig message content.
- `compaction.rs` uses rig `Message` history and rig `Prompt` for summaries.

This is the highest hidden coupling. Owning the loop while still persisting rig
messages is possible, but full replacement requires a Defra message schema and
compatibility decode path for existing rows.

### Admission, errors, and retry

`admission/client.rs` wraps rig `CompletionClient` and `CompletionModel` to
enforce backend concurrency limits and record inference calls.

`admission/permit.rs`, `admission/controller.rs`, `admission/persistence.rs`,
and `admission/registry.rs` use rig `CompletionError` and `Usage`.

`error.rs` and `retry.rs` classify rig `StreamingError` / `CompletionError`
values into Defra inference errors.

An owned loop needs native error and usage types at the Defra boundary, plus
provider-specific mapping adapters.

### Provider client quirks

Rig currently absorbs a meaningful amount of provider behavior:

- OpenAI-compatible request construction
- OpenRouter preferences/additional params
- streaming response parsing
- tool call delta assembly
- provider error shapes
- token usage extraction

The custom ChatGPT Codex HTTP client already patches requests and synthesizes
responses to fit rig's OpenAI provider shape. If Defra removes rig provider
clients immediately, this path must be revalidated separately.

## Sizing

| Area | Replacement cost | Notes |
| --- | --- | --- |
| Runtime loop | High | We need deterministic completion -> tool -> result -> next-completion semantics, including max turns, termination, tool skip/terminate actions, interrupts, deadlines, retries, and lifecycle persistence. |
| Provider streaming | High | Provider quirks are the biggest unknown: OpenAI-compatible, OpenRouter, local vLLM-like endpoints, and ChatGPT Codex all need streaming/tool-call parity. |
| Tool trait and definitions | Medium | Many call sites, but mostly mechanical if we introduce a Defra trait and adapters. |
| Prompt hook replacement | Medium-high | Conceptually improves the model, but must preserve every persistence and lifecycle side effect. |
| Message model | High | Persisted rows, compaction, transcript presentation, trace export, and history reload all depend on rig message structures. |
| Admission and usage accounting | Medium | Existing logic is Defra-owned but wraps rig request/response/error/usage types. |
| Error classification | Medium | Requires native provider errors or adapter-mapped errors. |
| Tests | Medium-high | Many unit/conformance tests instantiate rig test models, rig tools, rig hooks, and rig stream items. |

Overall size: **large foundational migration**, but feasible if split into
adapter-first phases.

## Proposed Design

Introduce a Defra-native completion-loop boundary while leaving provider clients
and old tool/message compatibility intact initially.

### Native loop types

Add internal types that describe what the daemon actually needs:

- `DefraMessage`
- `DefraAssistantEvent`
- `DefraToolSpec`
- `DefraToolCall`
- `DefraToolObservation`
- `DefraCompletionRequest`
- `DefraCompletionStream`
- `DefraCompletionError`
- `DefraUsage`

The first implementation may convert to/from rig types at the adapter edge. The
daemon and hook replacement should speak these Defra types.

### Native provider boundary

Define a Defra provider trait such as:

```rust
trait CompletionBackend {
    async fn stream_completion(
        &self,
        request: DefraCompletionRequest,
    ) -> Result<DefraCompletionStream, DefraCompletionError>;
}
```

Initial adapters:

- `RigOpenAiCompatibleBackend`
- `RigOpenRouterBackend`
- `RigChatGptCodexBackend`

This keeps provider behavior stable while moving loop ownership.

### Native tool boundary

Define a Defra tool trait that returns a Defra tool spec and a Defra tool result.
During migration:

- existing rig tools can be wrapped as Defra tools
- Defra tools can be converted to rig `ToolDefinition` only inside provider
  adapters if still needed
- lifecycle wrappers should move to the Defra tool boundary first

### Native loop semantics

The owned loop should:

1. Build the working context from the request, preamble, compacted history, and
   current turn.
2. Submit the next completion request to the provider adapter.
3. Stream assistant text/reasoning/tool-call events to `StreamProcessor`.
4. On tool call, run the Defra persistence/lifecycle step before execution.
5. Execute or skip the tool according to the Defra action.
6. Truncate/spill the tool result before appending the model-facing observation
   to the working context.
7. Continue until final response, max turns, interrupt, deadline, or terminal
   tool action.

The important invariant is that persisted transcript state and in-loop working
context are produced by the same Defra decision.

## Migration Plan

### Phase 0 - Freeze current behavior

Add golden tests for:

- a basic streaming assistant response
- a normal tool call and tool result
- oversized tool result truncation
- background/subagent tool skip behavior
- command denial
- interrupt during in-flight tool execution
- stream liveness timeout
- retryable provider failure before observable output

These tests should assert both persisted rows and model-facing next-turn input.

### Phase 1 - Add Defra-native loop types

Introduce native request, event, tool, message, error, and usage types. Add
conversions from rig types, but do not change runtime behavior yet.

### Phase 2 - Provider adapters over rig

Wrap existing rig provider clients behind the Defra provider trait. This should
be behavior-preserving and should keep OpenAI-compatible, OpenRouter, and
ChatGPT Codex paths working.

### Phase 3 - Own the multi-turn loop

Replace `agent.stream_prompt(...).with_history(...).with_hook(...)` with a
Defra loop that calls the provider adapter directly and invokes the existing
persistence/lifecycle logic as explicit steps.

This is the first phase that solves the issue's concrete problem: in-loop tool
observations become bounded before the next model call.

### Phase 4 - Move tools to Defra trait

Port the tool surface to Defra-native traits. Keep rig adapters only where a
provider adapter still requires rig request shapes.

### Phase 5 - Decide provider ownership

After the loop and tool surface are native, reassess whether removing rig
provider clients is worth it. If provider adapters are stable and rig is only a
client library, keeping it may be cheaper than reimplementing every provider
quirk.

## Go / No-Go Decision

**Go:** Build an adapter-first owned loop. This directly addresses the in-loop
context-control problem and reduces the mismatch between Defra's lifecycle model
and rig's hook model.

**No-go:** Do not attempt a single PR that removes `rig-core`. The message
schema, provider stream parsing, tools, hooks, compaction, admission, and tests
are too intertwined for a safe one-step replacement.

**Decision gate for the spike:** The spike is successful if a single behavior can
run a streamed prompt with one tool call, persist the same rows as the rig path,
and append a truncated model-facing tool observation before the next completion
request.

## Risks

- Provider streaming regressions, especially tool-call deltas and final usage.
- Silent transcript drift if Defra-native messages do not round-trip through
  existing persisted rows.
- Duplicate lifecycle side effects if the old hook path and new explicit loop
  both run during migration.
- Test fixture churn because many tests construct rig test models and rig stream
  events directly.
- ChatGPT Codex compatibility, because that path currently relies on fitting a
  custom HTTP client into rig's OpenAI provider interface.

## Non-goals

- No immediate public API change.
- No desktop UI change.
- No provider rewrite in the first migration phase.
- No new memory, MCP, subagent, or scheduling semantics.

## Acceptance Mapping

- **Exact rig API surface:** documented by runtime construction, daemon loop,
  streaming, hook/persistence, tool surface, message model, admission/error, and
  provider sections above.
- **Cost of owning it:** sized by area with high-risk provider/message/loop
  areas called out.
- **Benefits:** native in-loop context control, bounded tool observations before
  same-request continuation, and a single Defra lifecycle model.
- **Decision:** go for adapter-first owned-loop spike; no-go for immediate full
  `rig-core` removal.
