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
| D1 | Per-request context renders into a `<context>`-tagged **user** message | Provider APIs accept only system/user/assistant; reuses the system-reminder message shape. Persistence is **not** automatic (see Rust step 6) — an explicit capture call records it |
| D2 | Reuse the existing MiniJinja `crate::template` engine | The cache guarantee is a property of *which variables a template reads* (extracted by a new complete-or-reject reads-collector, D7), not of engine expressiveness; gets task interop for free |
| D3 | Variable volatility comes from a **runtime-owned catalog** | Behaviors reference names only and cannot mis-declare volatility, so the guard checks against ground truth |
| D4 | `system_prompt` is rendered as a **run-constant** template, once at runtime start, then frozen | Cache-stability becomes *structural* — the preamble literally cannot change per request — matching today's frozen-preamble model |
| D5 | A new `request_context_template` behavior field renders **per request** | The genuinely-new dynamic surface; opt-in, absent = today's behavior |
| D6 | Per-request providers (`collection_summary`) are evaluated **lazily** | Only queried when a template actually reads them (reads known from the reads-collector) — no cost for behaviors that don't use them |
| D7 | The reads-collector is **complete-or-reject**, not best-effort | A cache-safety guard must err toward rejection. The current `parse_template_for_validation` under-collects (misses loop/macro/filter rebindings), which would *accept* unsafe templates. v1 collects the **complete set of full catalog variable refs**, ignores reads inside `{% raw %}` blocks (literal text), rejects unknown `node.*`/`ctx.*` paths, and **rejects** any system template using constructs it cannot fully analyze (see Validation) |

## The variable catalog (v1)

A runtime-owned table mapping each variable reference to its **volatility** and
its **availability** (which render sites supply it). Two new MiniJinja
namespaces are added to `TemplateScope`:

| Namespace.var | Volatility | Source | Availability |
|---|---|---|---|
| `node.node_did` | run-constant | `AgentPrincipal.agent_did` | system, request-ctx, task |
| `node.behavior_id` | run-constant | resolved behavior id | system, request-ctx, task |
| `ctx.now` | per-request | request / fire (RFC3339) | request-ctx, task |
| `ctx.collection_summary` | per-request | lazy DefraDB query | request-ctx |
| `event.*` / `doc.*` / `args.*` | per-request | trigger fire time | task |

Volatility classes: **static** (literal text, no variable), **run-constant**
(filled once at runtime start, frozen), **per-request** (varies per request).

Two orthogonal axes:
- **Volatility** drives the cache-safety guard: a *system* template may read
  only run-constant (or static) variables.
- **Availability** drives a render-site scope check (analogous to today's
  trigger-kind scope validation): a variable is only legal where the runtime
  actually supplies it. `ctx.collection_summary` is **request-context only** —
  the live summary is not established at trigger fire time, so it is not in task
  scope for v1. `ctx.now` is the task-interop time variable (covers "pull
  system time into task start"); `event.fired_at` remains as well.

Every catalog entry is a **full variable ref** (e.g. `node.node_did`), not a
bare namespace root. The guard and the reads-collector operate on full refs:
an unknown `node.*` or `ctx.*` path (not in the catalog) is **rejected**, never
treated as run-constant by default.

**Deferred from v1:** `node.node_id`, `node.deployment_id` (sourced from the
optional `ServiceAccount { host_id, deployment_id }`, with undefined
missing-value behavior under strict-undefined rendering); and `ctx.node_state`
(liveness/peers/replication, contract unspecified). See Follow-ups.

## Behavior surface

- `system_prompt` (existing field) — now rendered through the engine with the
  **run-constant** binding, once at runtime start, into the frozen preamble.
  A `system_prompt` containing no MiniJinja markers (`{{`, `{%`, `{#`) bypasses
  rendering entirely and is used as a literal. A prompt that *does* contain
  markers is validated (D3/D7 guard) and rendered.

  **Intentional breaking edge (accepted):** an existing literal prompt that
  happens to contain `{{`/`{%`/`{#` (e.g. documenting Jinja/Handlebars syntax)
  now becomes a template and will be rejected at apply if it references a
  non-run-constant or unanalyzable construct. This is an accepted break — we
  keep the single `system_prompt` field rather than a separate opt-in field.
  **Escape hatch:** wrap literal braces in a MiniJinja `{% raw %}…{% endraw %}`
  block to keep them literal. This is the documented migration path; the apply
  error message names it.
- `request_context_template` (new optional field) — rendered **per request**
  into the `<context>`-tagged user message. May read `node.*` and the
  request-context `ctx.*` variables (the trigger scopes `event/doc/args` are
  fire-time only and not in scope during the owned loop's per-request render).
  Absent ⇒ no context message is injected (today's behavior).

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
message — add a `contextPreamble` slot to the `assemble` order, positioned
**after** `conversation` and **before** `prompt` (the per-request context
precedes the new user turn), and extend `assemble_spec` to pin the new order.

**Model↔impl fidelity for the guard.** The cache-safety theorem is stated over
`t.reads` and assumes it is the *complete* set of variables the template reads.
Soundness therefore depends on the Rust reads-collector being complete-or-
reject (D7): if the collector cannot prove it captured every read, it must
reject rather than return a partial set. The conformance suite pins this
direction explicitly (a system template using an unanalyzable construct must be
rejected, never silently accepted).

### 2. Conformance mirror — `tests/conformance/prompt_template.rs`

Mirrors each theorem under the structure fence, with a `CoverageLedger` entry
(`PromptAssembly.template.systemRenderStable`). Cases:

- system template reading only `node.*` → guard accepts; render under two
  different `ctx` bindings is identical (`system_render_stable`).
- system template reading `ctx.now` → guard rejects (volatility guard).
- system template using an **unanalyzable construct** (e.g. a `{% for %}` /
  macro rebinding the reads-collector cannot fully resolve) → guard **rejects**
  (D7 complete-or-reject; pins the safe failure direction).
- system template with a per-request ref inside `{% raw %}` → guard **accepts**
  (raw bodies are not reads; pins the escape hatch).
- system template referencing an **unknown** `ctx.*`/`node.*` path → guard
  **rejects** (no default-to-run-constant).
- request-context template reading `ctx.*` → renders differently per binding
  (expected dynamic behavior; documents the boundary).
- assemble order: `contextPreamble` precedes `prompt` (the new slot position).
- determinism: same binding ⇒ same output (`render_determined`).

Ties model to impl: the guard maps to the apply-time check built on the
new complete-or-reject reads-collector + catalog; render maps to
`render_template`.

### 3. Rust implementation

1. **Catalog module** — `Volatility` enum, the catalog table (VarRef →
   `{ volatility, availability }`), and helpers to classify a `VariableRef`
   and resolve which roots/vars are legal at a given render site.

2. **Reads-collector (complete-or-reject)** — a new collector that returns the
   complete set of **full catalog variable refs** a template reads (e.g.
   `node.node_did`, `ctx.now`), or an error if the template uses a construct it
   cannot fully analyze. Required contract:
   - Returns full refs, not bare roots — the volatility guard is keyed by full
     ref.
   - Ignores text inside `{% raw %}…{% endraw %}` blocks (the documented escape
     hatch — literal braces there are not reads).
   - Rejects unknown `node.*`/`ctx.*` paths (not in the catalog).

   Two acceptable implementations, decided in the plan:
   - **Parser-backed**: use MiniJinja's `unstable_machinery` parser to walk the
     AST and collect every name access (preferred for completeness); or
   - **Restricted-subset textual scan**: extend the current scanner to track all
     catalog refs, skip `{% raw %}` bodies, and **reject** templates containing
     rebinding constructs (`{% for %}`, `{% set %}`, `{% macro %}`, filters that
     take name args) when used in a *system* template.

   The existing `parse_template_for_validation` (event/doc/args, best-effort)
   stays as-is for the trigger-scope check it already serves; the new collector
   is what backs the cache-safety guard. Do **not** repurpose the weak scanner
   for the guard.

3. **Scope assembly** — extend `TemplateScope` with `node` and `ctx`
   namespaces. Populate `node.*` once at runtime start (run-constant). Build
   `ctx.*` per request, evaluating `collection_summary` **lazily** based on the
   collected reads of the template being rendered (escape interpolated values
   with `escape_graphql_string` where they reach a mutation; emit `null`, never
   `[]`).

4. **Validation (apply/reconcile, Lean-fenced)** —
   `validate_system_template(template, catalog)`: collect reads (step 2), reject
   any non-run-constant ref and any unanalyzable construct, with an error that
   names the offending var and the `{% raw %}` escape. Plus an
   **availability** check at each render site (system / request-context / task)
   mirroring today's trigger-kind scope validation. The apply path requires the
   Lean model to cover the new guard.

5. **System render** — in `prompt.rs` preamble construction, render
   `system_prompt` as a run-constant template once (marker guard: no markers ⇒
   literal) into the frozen preamble.

6. **Request-context render + explicit persistence** — in
   `agent/loop_stream.rs`, render `request_context_template` per request into a
   `<context>`-tagged user message and inject it **before** the prompt (the new
   `contextPreamble` slot). It is **not** persisted by the current path:
   `new_messages` starts as `vec![prompt]` and `on_completion_call` persists
   only the prompt. Add an explicit persistence call for the context message
   (e.g. a `persist_context_message` analogous to `persist_message`, invoked
   alongside `on_completion_call`) so training capture records exactly what the
   model saw. **Durable order must match provider order:** the context message
   gets the earlier `AgentMessage.sequence`, the prompt the later one (context
   before prompt). The integration case asserts both *existence* and *sequence
   ordering* (context sequence < prompt sequence), not just that the row exists.

7. **`request_context_template` config surface** (first-class step) — wire the
   new behavior field end to end:
   - `crates/defra-agent-schemas/schemas/agent/agent_behavior.graphql` — add
     the field.
   - `document_config` Behavior struct — add the optional field.
   - `AgentBehavior` runtime config + reconcile — carry it into the resolved
     behavior.
   - `crates/defra-agent-cli/src/desired_state/mod.rs` — accept the new field
     (desired-state currently rejects unknown behavior fields).
   - `crates/defra-agent-cli/src/main.rs` export allowlist — include the field
     so export/import round-trips.
   - write-path (create/update behavior mutation) — emit the field; `null` when
     empty, never `[]`.

8. **Task interop** — merge `node.*` and the task-available `ctx.*` (`ctx.now`)
   into the trigger-engine `TemplateScope` so a task's `prompt_template` can
   read `{{ ctx.now }}` / `{{ node.node_did }}` at fire time. Extend the
   trigger-kind scope validation to know the new namespaces and their
   per-render-site availability.

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

- `node.node_id` / `node.deployment_id` — deferred until precisely mapped to a
  runtime source (`ServiceAccount { host_id, deployment_id }` is only
  optionally available through identity) with defined missing-value behavior.
- `ctx.node_state` — deferred until its exact JSON/string contract (which
  liveness/peer/replication fields, as what shape) is specified. Add as a
  catalog entry (per-request, request-context availability) once defined.
- `ctx.acting_seat` / `ctx.acting_did` — deferred until `AgentRequest` carries
  a concrete acting-identity source; v1 fails closed instead of emitting empty
  placeholders.
- Task availability for `ctx.collection_summary` — request-context-only in v1;
  extend to task fire if a concrete need appears and the fire-time value is
  well-defined.
- Additional catalog variables beyond v1 (extend the table).
- Per-request render of the system template with an equality assertion
  (defense-in-depth); D4's render-once-frozen makes this unnecessary for v1.
