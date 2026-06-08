# Issue #400 — Own the completion+tool loop, keep rig as the provider/streaming client

Status: design pass, pre-implementation
GitHub: https://github.com/sourcenetwork/defra-agent/issues/400
Related: #401 (the in-loop truncation gap, mitigated by PR #416), #377 / PR #382 (subagent enablement; this is sequenced after it)

## TL;DR

The issue title says "replace rig-core." After auditing the surface, the right
cut is narrower and lower-risk: **own the multi-turn completion→tool loop, but
keep rig as a provider/streaming client library.**

- **Keep** the layer rig is genuinely good at and that is expensive to re-own:
  `CompletionModel::completion` / `CompletionModel::stream`, the provider clients
  (OpenAI-compatible, OpenRouter, ChatGPT-Codex responses), the streaming decode
  (`RawStreamingChoice` → `StreamedAssistantContent` / `StreamedUserContent`), the
  `CompletionRequest` / `CompletionResponse` types, and the `Message` family. This
  is the cross-provider quirk + byte-level streaming normalization layer.
- **Own** the layer that constrains us: the multi-turn loop itself
  (`Agent` / `AgentBuilder` / `stream_prompt` / `prompt` / `prompt_request`) and the
  `PromptHook` trait. These become our own loop function and direct method calls.

The key enabling fact: **we already own most of the loop's control.** rig's
`stream_prompt` is just the inner engine; our `inference.rs` already wraps it
with shutdown, interrupt, deadline, liveness-timeout, tool-cancellation, retry
accounting, and partial-turn persistence. Owning the loop means replacing the
inner engine — build request → `model.stream()` → consume → dispatch tools →
thread results → repeat — not rebuilding everything.

## Current state

### How the integration is layered today

rig has two layers, and we sit across the seam:

1. **Provider / model layer** — `CompletionModel` trait
   (`rig-core-0.35.0/src/completion/request.rs:470-505`). It exposes
   `completion(request) -> CompletionResponse<Response>` and
   `stream(request) -> StreamingCompletionResponse<StreamingResponse>`. Each
   provider (`rig::providers::{openai, openrouter, anthropic, ...}`) implements
   this and absorbs all the provider-specific request shaping, SSE parsing, and
   tool-call format differences. `streaming.rs` turns each provider's raw chunks
   into a uniform `RawStreamingChoice<R>` → `StreamedAssistantContent<R>` stream.

2. **Agent / loop layer** — `Agent`, `AgentBuilder`, and `prompt_request/`
   (`mod.rs` 826 LOC, `streaming.rs` 1393 LOC, `hooks.rs` 147 LOC ≈ **2,366 LOC**).
   This builds the `CompletionRequest` (preamble + history + tools), runs the
   multi-turn iteration (when a turn ends in tool calls: dispatch them, append
   results as user messages, re-request), dispatches tools through the `Tool` /
   `ToolDyn` traits, threads messages, and fires `PromptHook` callbacks
   (`on_completion_call`, `on_completion_response`, `on_tool_call`,
   `on_tool_result`).

We consume layer 2 and re-implement a lot around it.

### What we already own (outside rig)

`crates/defra-agent/src/agent/daemon/inference.rs` calls
`agent.stream_prompt(&request.content).with_history(...).with_hook(hook)` to get a
stream, then runs **our own** outer loop over `stream.next()` that handles:

- shutdown (`shutdown.changed()`)
- interrupt (`interrupt_rx.changed()` → cancel tokens, persist partial turn)
- per-request deadline (`await_with_request_deadline`)
- stream liveness timeout (`DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS`)
- in-flight tool-call cancellation / timeout sweeps
  (`persistence_hook.{cancel,timeout,fail}_in_flight_tool_calls`)
- retry / attempt accounting (`classify_completion_error`, `attempt_index`,
  `max_attempts`)
- per-item dispatch into `StreamProcessor`
  (`crates/defra-agent/src/agent/stream_processor.rs`) which converts
  `MultiTurnStreamItem` variants into our types and drives persistence.

So the lifecycle envelope is already ours. What still lives *inside* rig's
`stream_prompt` is: request construction, the turn iteration, tool dispatch, the
in-loop working-message vec, and `PromptHook` firing.

### What is woven into rig (the audit / sizing)

Approximately **65 files** touch `rig::`, ~254 call sites. `rig-core = "0.35.0"`
is a workspace dep used by `defra-agent`, `defra-agent-cli`, `defra-agent-protocol`,
and `desktop-tauri`. By replacement difficulty:

| Area | Where | Keep or own? | Note |
|---|---|---|---|
| `CompletionModel::completion`/`stream` | provider call sites in `oneshot.rs`, `agent/runtime/context.rs` | **Keep** | the layer worth keeping |
| Provider clients (openai-compat, openrouter, chatgpt-codex) | `oneshot.rs`, `agent/runtime/context.rs`, `chatgpt_codex.rs` | **Keep** | quirk absorption |
| Streaming decode types (`RawStreamingChoice`, `StreamedAssistantContent`, `StreamedUserContent`, `StreamingCompletionResponse`) | `agent/stream_processor.rs`, `admission/stream_guard.rs` | **Keep** | byte→item normalization |
| `Message` / `AssistantContent` / `UserContent` / `ToolResult` / `OneOrMany` | `hook/persistence/mod.rs`, `compaction/history.rs`, `background_tools.rs`, `prompt.rs`, `session.rs`, protocol `transcript.rs` | **Keep** (for now) | pervasive; keeping avoids a type migration |
| `Agent` / `AgentBuilder` / `completion_factory.rs` | `completion_factory.rs`, `oneshot.rs`, `compaction.rs`, `daemon.rs` | **Own** | replace with our loop driver |
| Multi-turn call (`stream_prompt`, `prompt`) | `inference.rs`, `oneshot.rs`, `compaction.rs`, `daemon/title.rs` | **Own** | 4 call sites, 1 streaming + 3 one-shot |
| `PromptHook<M>` (`DefraSessionHook`) | `hook/persistence/prompt_hook.rs`, `hook/persistence/mod.rs`, `hook.rs` | **Own** | trait → direct calls; logic stays |
| `Tool` / `ToolDyn` impls (~30 tools) | `toolset/`, `meta_tools/`, `defra_query/` | **Own** the trait, keep the logic | re-skin to our tool trait or keep rig's `Tool` |
| `CompletionClient` wrapper (admission) | `admission/client.rs` | **Own** | wrap our model abstraction instead |
| `Usage` / `GetTokenUsage` | `admission/`, `compaction/tests.rs` | **Keep** | trivial |

## The proposal

### The boundary

Drop rig's layer 2, keep rig's layer 1. Concretely, replace
`agent.stream_prompt(prompt).with_history(h).with_hook(hook)` with our own
async loop driver that holds a `CompletionModel` (`M`), the preamble, the tool
set, and a reference to our persistence/lifecycle object (the old hook, now a
plain struct), and does:

```text
loop {
    request   = build_completion_request(preamble, history, tools, tool_choice)
    // -- our callback, was PromptHook::on_completion_call
    persistence.before_completion(&request).await
    stream    = model.stream(request).await?          // rig still owns this
    turn      = consume_stream(stream)                 // our StreamProcessor, mostly as-is
    // -- our callback, was on_completion_response
    persistence.after_completion(&turn).await
    if turn.tool_calls.is_empty() { break }            // final assistant text
    for call in turn.tool_calls {
        // -- our callbacks, were on_tool_call / on_tool_result
        result        = dispatch_tool(call).await       // our tool_call_lifecycle::runtime
        bounded        = bound_in_loop_result(result)    // <-- native; closes #401
        history.push(tool_result_to_user_message(bounded))
        persistence.record_tool_result(call, result_full, bounded).await
    }
    if turns >= max_turns { break_with_max_turns_error() }
}
```

This is a small loop. The hard parts inside it — `model.stream()`, the stream
decode, message construction helpers (`tool_result_to_user_message`,
`build_full_history`) — already exist in rig and we keep using them (or copy
the ~3 helper fns, which are trivial). Our `StreamProcessor` already consumes
the per-item stream; it changes from consuming `MultiTurnStreamItem` (rig's
multi-turn wrapper) to consuming a single-turn `StreamedAssistantContent`
stream, with the turn boundary handled by our outer loop instead of rig's.

### PromptHook collapses into direct calls

Today `DefraSessionHook` implements `PromptHook<M>` and rig calls it at four
points. Those four points become four method calls in our loop. The persistence
logic — DefraDB writes, tool-call lifecycle transitions, spill/truncation — is
entirely ours already; only the trait wiring and the `HookAction` /
`ToolCallHookAction` return-value indirection go away. This is a net
simplification: the hook's return values currently encode "tell rig to
continue/cancel," which we'd express directly as control flow.

### The #401 payoff (the concrete motivation)

#401: `on_tool_result` can bound the **persisted** copy but not rig's **in-loop
working message**, so an oversized tool result hits the model raw within one
request. PR #416 worked around this with a `ToolResultRecorder`
(`tool_call_lifecycle/runtime.rs`, ~429 LOC) that bounds the foreground result
*before rig appends it* to the same-request message vec — a shim that exists
only because we don't own the message vec.

When we own the loop, `history.push(tool_result_to_user_message(bounded))` uses
the bounded result by construction. The recorder shim can be deleted; the
in-loop and persisted bounds are the same code path. #401's root cause
disappears rather than being mitigated.

## Cost / sizing

**New code we write:**

- The loop driver: ~150–300 LOC (the skeleton above, plus max-turns/tool-choice
  handling). The lifecycle envelope (deadline/interrupt/liveness/retry) stays in
  `inference.rs` largely unchanged — it wraps our loop the same way it wraps
  rig's stream today.
- `DefraSessionHook` → plain struct with the same methods, minus the trait:
  mechanical, ~net negative LOC.
- Provider client construction stays; we keep calling `model.stream(request)`.
- `StreamProcessor` retargets from `MultiTurnStreamItem` to single-turn items:
  moderate, the per-item match arms barely change.

**Code we delete:** `completion_factory.rs` agent-builder plumbing, the
`PromptHook` impl wiring, the `ToolCallHookAction`/`HookAction` indirection, and
the PR #416 recorder shim.

**One-shot paths** (`oneshot.rs`, `compaction.rs`, `daemon/title.rs`) call
`agent.prompt().await`. These get a thin non-streaming variant of the loop (or
just `model.completion(request)` + the same tool loop). Small.

Rough order of magnitude: this is a **focused refactor of the loop seam**, not a
4,000–5,000 LOC rewrite. The big LOC numbers in a naive "replace rig" estimate
come from the 30 tool impls and the pervasive `Message` usage — both of which we
**keep** under this boundary.

## Risks and open questions

1. **Tool trait: keep rig's `Tool`/`ToolDyn` or define our own?** Keeping rig's
   means our loop dispatches via `ToolDyn::call`, no tool churn — but we stay
   coupled to rig's tool types. Defining our own removes the last big coupling
   but touches ~30 impls. *Leaning: keep rig's `Tool` initially; revisit.*

2. **`CompletionModel::stream` stability.** We'd depend directly on rig's
   model-level streaming contract (`StreamingCompletionResponse`,
   `StreamedAssistantContent`) rather than the higher-level `stream_prompt`. This
   is a more stable, lower-level surface, but we pin a rev and own the upgrade.

3. **`max_turns` / `PromptError::MaxTurnsError` semantics.** rig's loop emits a
   specific error at the turn cap; we reproduce that behavior and its retry
   classification in our loop.

4. **ChatGPT-Codex responses client.** `chatgpt_codex.rs` uses a custom HTTP
   adapter and rig's responses client. Confirm it still slots in at the
   `model.stream()` boundary (it implements `CompletionModel`, so it should).

5. **Does any formal spec move?** This changes *how* the loop is driven, not
   *what transitions are legal*. The request/tool-call lifecycle state machines
   (and their Lean proofs) are about persisted state, which is unchanged. Per
   CLAUDE.md, confirm no transition legality changes before touching code — I
   believe this is plumbing, not a spec change, but that's a checkpoint.

6. **Message type migration deferred.** We keep rig's `Message` family to avoid a
   large type migration now. That leaves a residual rig coupling. Acceptable as a
   stopping point, or a follow-up if we want zero rig.

## Decisions (resolved)

The issue asks for a go/no-go after the design doc. Decided:

- **D1 — GO.** Own the multi-turn loop; keep rig as the provider/streaming client.
- **D2 — Keep rig's `Tool`/`ToolDyn` for now.** It's just structs, we already
  import rig and will continue to, so there's no real benefit to pulling it fully
  out as part of this work. Track full extraction as a later follow-up issue.
- **D3 — Keep rig's `Message` family for now.** Same reasoning as D2; track
  migrating off rig's message types as a later follow-up issue.
- **D4 — Convert all four call sites in one PR, on separate commits** (streaming
  `inference.rs`, then `oneshot`, `compaction`, `title`), so the whole rig
  `Agent`/`AgentBuilder`/`completion_factory` layer is deleted in one go while
  staying reviewable per commit. **Compaction is expected to be the hardest** of
  the four and should get its own commit + careful review.
- **D5 — Delete the PR #416 `ToolResultRecorder` shim** as part of this work,
  once the owned loop makes the in-loop and persisted tool-result bounds the same
  code path (the shim's reason to exist goes away).

### Sequencing for implementation

1. Confirm no formal-spec change (per CLAUDE.md): this is loop-driver plumbing,
   not transition legality — verify before code.
2. Build the owned loop driver + retarget `StreamProcessor` to single-turn items;
   convert `inference.rs` streaming path. Delete the #416 recorder shim here.
3. Convert `oneshot`, then `compaction` (hardest), then `daemon/title` — one
   commit each.
4. Delete `completion_factory.rs` agent-builder plumbing and the `PromptHook`
   trait wiring once no path uses rig's `Agent`.

### Follow-up issues (deferred, not in scope here)

- #424 — Pull the tool trait fully out of rig (replace `Tool`/`ToolDyn`). (D2)
- #425 — Migrate off rig's `Message` family to our own message types. (D3)
