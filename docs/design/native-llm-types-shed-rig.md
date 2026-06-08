# Native LLM types — shed rig from everything except the provider/parsing layer

Status: design pass, in progress
Branch: `feat/native-types-shed-rig` (stacked on `worktree-rig-issues` / PR #426)
Related: #424 (tool trait), #425 (Message family), #400 (owned loop — landed)

## Goal

After #400, rig-core is reduced to a provider/streaming client plus a shared
type vocabulary. This change removes rig from everything **except the provider /
SSE-parsing layer** ("Layer A"), by introducing Defra-native types that **mirror
rig's shapes 1:1** and converting at the single remaining rig boundary.

Result after this PR: rig-core is referenced only by Layer A (the
`CompletionModel`/provider clients/streaming-decode/error/usage types) and a
small `rig_compat` converter module. A later PR owns Layer A and drops rig
entirely.

## Scope (this PR)

| Layer | Native type | rig type replaced | Converter needed? |
| --- | --- | --- | --- |
| D-glue | `HookAction`, `ToolCallHookAction` | `rig::agent::{HookAction, ToolCallHookAction}` | No — internal hook↔loop only |
| D-glue | `ToolChoice` | `rig::message::ToolChoice` | Yes — tiny, at request build |
| C | `Tool`/`ToolDyn` trait, `ToolDefinition` | `rig::tool::{Tool, ToolDyn, ToolError}`, `rig::completion::ToolDefinition` | `ToolDefinition` → rig at request build |
| B | `Message` family + `OneOrMany` | `rig::completion::message::*`, `rig::one_or_many::OneOrMany` | native↔rig at the provider seam |

**Out of scope (Layer A, later PR):** `CompletionModel`, `CompletionRequest`/
`CompletionResponse`, the streaming-decode types (`StreamedAssistantContent`,
`StreamedUserContent`, `RawStreamingChoice`, `StreamingCompletionResponse`,
`MultiTurnStreamItem`, `FinalResponse`, `StreamingError`), the provider clients
(`rig::providers::{openai, openrouter}`, `chatgpt_codex`), `CompletionError`,
`Usage`/`GetTokenUsage`, `rig::http_client`, `rig::client::CompletionClient`.

## Where the native types live

- `crate::llm` — the native LLM type vocabulary.
  - `crate::llm::message` — `Message`, `AssistantContent`, `UserContent`,
    `ToolCall`, `ToolFunction`, `ToolResult`, `ToolResultContent`, `Text`,
    `Reasoning`, `ReasoningContent`, and `OneOrMany`.
  - `crate::llm::tool` — `Tool` / `ToolDyn` trait + `ToolDefinition`.
  - `crate::llm` (root) — `ToolChoice`, `HookAction`, `ToolCallHookAction`.
  - `crate::llm::rig_compat` — `From`/`Into` converters between native and rig
    types, used **only** at the Layer-A boundary (request build + stream
    consume). This module is deleted when Layer A lands.

Types mirror rig field-for-field so converters are mechanical and the call-site
swaps are pure renames. We can simplify the types in a later pass once rig (and
the converters) are gone.

## Sequencing (sequential commits, each compiles + tests green)

1. **Design doc** (this file).
2. **D-glue: `HookAction` / `ToolCallHookAction`.** Pure internal (hook returns
   them, loop matches them) — no converter. Smallest, establishes the `crate::llm`
   module.
3. **D-glue: `ToolChoice`.** Native enum; convert to rig at request build.
4. **C: tool trait + `ToolDefinition`.** Define native `Tool`/`ToolDyn` +
   `ToolDefinition`; port the ~30 tool impls; convert `ToolDefinition` → rig at
   request build; loop dispatches the native `ToolDyn`. Closes #424.
5. **B: `Message` family.** Define native `Message` + content types + `OneOrMany`;
   migrate every call site (`session/history`, `hook/persistence`, `compaction`,
   `background_tools`, `prompt`, `stream_processor`, `trace_export`, and
   `defra-agent-protocol::transcript`); converters at the provider seam
   (native history → rig `CompletionRequest`; rig streamed content → native on
   consume). Closes #425.

## Risks / notes

- **Compile-as-a-whole:** a Rust type swap only compiles when every call site
  agrees, so each layer lands as one coherent commit (compiler-guided). The tool
  ports (C) are independent per file and could be parallelized; messages (B) are
  interdependent and are done compiler-guided.
- **Converter fidelity (B):** the native↔rig `Message` round-trip must be
  lossless. Mirroring rig's shape keeps it field-for-field; covered by the
  existing persistence/transcript tests + the live `d4f` tests.
- **Protocol crate:** `defra-agent-protocol` is a separate crate that also uses
  rig's `Message`; it migrates to native types too (it has no provider concerns,
  so no converter there).
