# Role-aware prompt templating (cache-safe) — design

Issue: #497 · Branch: `prompt-templating-497`

## Problem

A behavior (driver: zanzibar-rl's `policy_agent`) must inject **dynamic
context** — acting seat/DID, a live collection summary, time, node state —
into each request so the model reasons against the current world. Today that
context is concatenated into the system prompt at build time, which causes:

1. **Cache busting.** A system prefix that changes per request (seat, counts,
   time) invalidates the provider's prefix cache every request.
2. **Train/serve skew.** Whatever assembles the prompt at serve time must be
   byte-identical to what assembled it during training-data capture, or the
   model is trained and served on different conditioning.

zanzibar-rl is consolidating all prompt/behavior into the Rust behavior (one
binary serves *and* is trained) and capturing the rendered prompt + tool
schemas into each training row. This layer is the cache-safe mechanism for the
dynamic portion of that prompt.

## Goal

A role-aware templating layer that keeps the cacheable **system** prefix
byte-stable across requests while letting per-request dynamic context render
into a **user**-role message. Behaviors declare templates; the runtime fills
them per request; system templates are validated to reject per-request-varying
values (the cache-safety guard); the rendered result is exposed so
training-data capture records exactly what the model saw.

All messages stay within the `system` / `user` / `assistant` roles that
provider APIs (including the v2 Responses API) accept. There is no new "context"
role — per-request context renders into a `<context>…</context>`-tagged **user**
message, reusing the existing system-reminder mechanism.

## Non-goals

- No new template engine. We build on the existing `crate::template`
  (MiniJinja) module used by tasks.
- No new protocol `Message` variant, no change to the Lean `MessageRole` enum,
  no change to the `rig_compat` seam.
- No general per-behavior variable registry. The variable catalog is
  runtime-owned (the runtime is the source of truth for what varies).

## Key decisions

| # | Decision | Rationale |
|---|----------|-----------|
| D1 | Per-request context renders into a `<context>`-tagged **user** message | Provider APIs accept only system/user/assistant; reuses the system-reminder path, so it persists and is captured for free |
| D2 | Reuse the existing MiniJinja `crate::template` engine | The cache guarantee is a property of *which variables a template reads* (extracted by `parse_template_for_validation`), not of engine expressiveness; gets task interop for free |
| D3 | Variable volatility comes from a **runtime-owned catalog** | Behaviors reference names only and cannot mis-declare volatility, so the guard checks against ground truth |
| D4 | `system_prompt` is rendered as a **run-constant** template, once at runtime start, then frozen | Cache-stability becomes *structural* — the preamble literally cannot change per request — matching today's frozen-preamble model |
| D5 | A new `request_context_template` behavior field renders **per request** | The genuinely-new dynamic surface; opt-in, absent = today's behavior |
| D6 | Per-request providers (`collection_summary`, `node_state`) are evaluated **lazily** | Only queried when a template actually reads them (reads known from `parse_template_for_validation`) — no cost for behaviors that don't use them |

## The variable catalog (v1)

A runtime-owned table mapping each variable reference to its **volatility**.
Two new MiniJinja namespaces are added to `TemplateScope`:

| Namespace.var | Volatility | Source | Allowed in system template? |
|---|---|---|---|
| `node.node_did` | run-constant | runtime start | ✅ |
| `node.node_id` | run-constant | runtime start | ✅ |
| `node.behavior_id` | run-constant | runtime start | ✅ |
| `node.deployment_id` | run-constant | runtime start | ✅ |
| `ctx.acting_seat` | per-request | request | ❌ |
| `ctx.acting_did` | per-request | request | ❌ |
| `ctx.now` | per-request | request (RFC3339) | ❌ |
| `ctx.collection_summary` | per-request | lazy DefraDB query | ❌ |
| `ctx.node_state` | per-request | lazy liveness query | ❌ |
| `event.*` / `doc.*` / `args.*` | per-request | trigger fire time | ❌ |

Volatility classes: **static** (literal text, no variable), **run-constant**
(filled once at runtime start, frozen), **per-request** (varies per request).

`node.*` (run-constant) is usable in **system**, **request-context**, and
**task** templates. `ctx.*` (per-request) is usable in request-context and
task templates only. The existing `event/doc/args` task scopes are unchanged
and remain per-request.

## Behavior surface

- `system_prompt` (existing field) — now rendered through the engine with the
  **run-constant** binding, once at runtime start, into the frozen preamble.
  **Backward-compat guard:** a `system_prompt` containing no MiniJinja markers
  (`{{`, `{%`, `{#`) bypasses rendering entirely and is used as a literal, so
  existing behaviors are unaffected. A prompt that *does* contain markers is
  validated (D3 guard) and rendered.
- `request_context_template` (new optional field) — rendered **per request**
  into the `<context>`-tagged user message. May read `node.*` and `ctx.*`
  (the trigger scopes `event/doc/args` are fire-time only and are not in scope
  during the owned loop's per-request render). Absent ⇒ no context message is
  injected (today's behavior).

## Foundation flow

### 1. Lean — `proofs/Proofs/PromptAssembly/Template.lean`

A new sub-model that **abstracts over the engine**: a template is characterized
by the set of variable references it reads; `render` depends only on the
binding restricted to those reads (modeling MiniJinja's pure, strict-undefined
evaluation).

```lean
inductive Volatility | static | runConstant | perRequest
abbrev VarRef := String                    -- catalog key, e.g. "node.node_did"
abbrev Catalog := VarRef → Option Volatility
abbrev Binding := VarRef → String

structure Template where
  reads : Finset VarRef

/-- Normal form capturing exactly what the template reads. -/
def render (t : Template) (b : Binding) : Finset (VarRef × String) :=
  t.reads.image (fun v => (v, b v))

/-- Agreement on reads ⇒ identical render (engine purity). -/
theorem render_determined (t) (b1 b2) (h : ∀ v ∈ t.reads, b1 v = b2 v) :
    render t b1 = render t b2

def WellFormedSystem (cat : Catalog) (t : Template) : Prop :=
  ∀ v ∈ t.reads, cat v = some .runConstant

def AgreeRunConstant (cat : Catalog) (b1 b2 : Binding) : Prop :=
  ∀ v, cat v = some .runConstant → b1 v = b2 v

/-- THE cache-stability property: a well-formed system template renders
    identically across any two requests that agree on run-constant values. -/
theorem system_render_stable (cat t b1 b2)
    (wf : WellFormedSystem cat t) (agree : AgreeRunConstant cat b1 b2) :
    render t b1 = render t b2

/-- Decidable validation mirror, proven equivalent to WellFormedSystem. -/
def validateSystem (cat : Catalog) (t : Template) : Bool :=
  t.reads.all (fun v => cat v == some .runConstant)
theorem validateSystem_correct : validateSystem cat t = true ↔ WellFormedSystem cat t
```

Standard: **zero `sorry`s**. Composition with the existing `PromptAssembly`
model: the rendered system template *is* the `preamble` slot text;
`system_render_stable` proves that slot is per-request-invariant, hence the
cacheable prefix is byte-stable. The rendered context is an injected user
message in the `conversation` region.

### 2. Conformance mirror — `tests/conformance/prompt_template.rs`

Mirrors each theorem under the structure fence, with a `CoverageLedger` entry
(`PromptAssembly.template.systemRenderStable`). Cases:

- system template reading only `node.*` → `validateSystem` accepts; render under
  two different `ctx` bindings is identical (`system_render_stable`).
- system template reading `ctx.now` → `validateSystem` rejects (guard).
- request-context template reading `ctx.*` → renders differently per binding
  (expected dynamic behavior; documents the boundary).
- determinism: same binding ⇒ same output (`render_determined`).

Ties model to impl: validation maps to the apply-time check built on
`parse_template_for_validation` + catalog; render maps to `render_template`.

### 3. Rust implementation

1. **Catalog module** — `Volatility` enum, the catalog table (VarRef →
   Volatility), and helpers to classify a `VariableRef`.
2. **Scope assembly** — extend `TemplateScope` with `node` and `ctx`
   namespaces. Populate `node.*` once at runtime start (run-constant). Build
   `ctx.*` per request, evaluating `collection_summary` / `node_state`
   **lazily** based on the reads of the template being rendered.
3. **Validation** — `validate_system_template(template, catalog)` built on
   `parse_template_for_validation`; rejects any per-request ref. Wired into the
   apply/reconcile path (Lean-fenced — the apply path requires the Lean model
   to cover the new check). Surfaces a clear error like the existing
   trigger-scope validation.
4. **System render** — in `prompt.rs` preamble construction, render
   `system_prompt` as a run-constant template once (with the marker
   backward-compat guard) into the frozen preamble.
5. **Request-context render** — in `agent/loop_stream.rs`, render
   `request_context_template` per request into a `<context>`-tagged user
   message, injected ahead of the new prompt; persisted via the existing
   message-persistence path so training capture records it.
6. **Task interop** — merge `node.*`/`ctx.*` into the trigger-engine
   `TemplateScope` so a task's `prompt_template` can read e.g. `{{ ctx.now }}`
   / `{{ node.node_did }}` at fire time. `parse_template_for_validation`'s
   per-kind scope checks gain the new namespaces.

### Sharp edges honored

- `graphql::escape_graphql_string` for any rendered value interpolated into a
  DefraDB mutation (e.g. the persisted context message, request content).
- Never emit `[]` in a mutation — emit `null` for empty values.
- `tracing`, never `println`.
- Gate with the full package suites, not `--lib`.

## Gate

```
cd crates/defra-agent/proofs && lake build      # zero sorry
cargo test -p defra-agent
cargo test -p defra-agent-cli
```

## Out of scope / follow-ups

- Additional catalog variables beyond v1 (extend the table).
- Richer `node_state` detail (peers/replication) if v1's liveness is too thin.
- Per-request render of the system template with an equality assertion
  (defense-in-depth); D4's render-once-frozen makes this unnecessary for v1.
```

