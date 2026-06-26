# Unified Tool Policy — Design

**Status:** Design (approved shape; SP1 detailed, SP2/SP3 sketched)
**Date:** 2026-06-26
**Author:** Jack Zampolin (with Claude)

## 1. Problem

defra-agent resolves a per-behavior tool surface from a `ToolSelection` document,
clamped by an operator `ToolCeiling`. An audit (13-reader sweep + 6 adversarial
verifications, all confirmed) found the system is structurally inconsistent:

1. **The ceiling is category-narrow.** `ToolCeiling` is a 4-field struct
   (`{file_tools, bash, cli_tools, root}`, `tool_surface/modes.rs:62-67`) governing
   only ~3 of 11 tool categories. Eight categories — MCP/meta, subagents,
   orchestration, background, memory, session-history, defra_query, write_tools —
   plus the always-on `context_budget` have **no operator-level upper bound at all**.
   A restrictive ceiling is trivially bypassed by enabling a built-in read tool or
   any online MCP service. This is the root cause; the rest are downstream.

2. **Permissive-default + empty=allow-all ambiguity collide.** `enable_meta_tools`
   and `enable_defra_query` default `true` (`tool_surface/selection.rs:168,180`);
   empty `allowed_mcp_service_ids` / `defra_query_collections` mean *all*. But an
   empty list means **deny** in `cli_tool_names` / `backgroundable_tool_names` and
   **allow-all** in MCP / defra_query. There is no consistent encoding of
   "unset vs explicitly-none vs all".

3. **No single intersection / no audit manifest.** `ceiling ∩ behavior`
   (`tool_surface/behavior_config.rs:84-155`), `∩ runtime-MCP` (`:213-234`),
   `∩ active-behaviors` (`agent/document_view/snapshot.rs:236-273`), and
   `∩ skills` (`skills.rs`) happen in **four places at three times**. Drops are
   silent `tracing::warn`s; there is no `requested → allowed → dropped` record.

4. **Determinism break.** `enable_meta_tools` is ANDed with a fresh, uncached async
   MCP-presence probe inside resolve (`behavior_config.rs:209`), so the effective
   surface is not statically computable at context-load time.

5. **Parity drift across schema → protocol → desktop.** `write_tools` and
   `subagent_default_await_mode` exist in schema but are absent from
   `ToolSelectionRow` (`defra-agent-protocol/src/row.rs:580-637`). Desktop save
   silently drops `enable_defra_query` / `defra_query_collections` / `write_tools`
   (read-but-not-written → **data loss on save**;
   `defra-agent-desktop-core/src/client/mutations/manage/tools.rs`).

6. **Lean proves the pieces, not the whole.** `CommandPolicy/*.lean` and
   `Skills.lean` (skills-within-ceiling) are proven independently and never compose;
   operator-global ceiling composition and runtime availability have no model
   (`proofs/README.md`, `Skills.lean:40-88`).

## 2. Goal

Unify the tool-permission experience by **reducing concept count, not adding layers**.
Today there are several *different shapes* for one idea: a `ToolSelection` grab-bag, a
differently-shaped `ToolCeiling`, and scattered runtime gates. That inconsistency is
the complexity. We unify the **shape**, keep exactly two appliers of it, and resolve
in one place.

Non-goals for this design (parked): physically relocating "wiring" fields onto
`AgentBehavior` as inline columns; a posture/wiring taxonomy as a user-facing concept;
behavior-local *override* fields. These were considered and rejected in favor of
fewer concepts.

## 3. The model

### 3.1 One per-category vocabulary

Every category is expressed with one of two typed atoms:

- **`Capability`** — a ranked mode for *how much*:
  - file: `Off ≤ ReadOnly ≤ ReadWrite`
  - bash: `Off ≤ ReadOnly ≤ Full` (carrying its command policy)
  - boolean capabilities (`Off ≤ On`): meta-tools, defra_query-enabled, subagent
    spawn / steering / background / orchestration / cross-deployment, memory,
    session-history, context_budget.
  - Meet (`⊓`) = `min` on the rank.

- **`EndpointScope<T>`** — `None ≤ Only(set) ≤ All` for *which things*: cli tools,
  MCP services, defra_query collections, subagent targets, backgroundable tools,
  write_tools.
  - Meet: `Only(A) ⊓ Only(B) = Only(A ∩ B)`; `Only(A) ⊓ All = Only(A)`;
    `x ⊓ None = None`; `All ⊓ All = All`.

`None`, `Only`, and `All` are **distinct values**. This is the single change that
eliminates the empty=allow-all ambiguity (problem #2): there is no longer a bare empty
list with an overloaded meaning anywhere.

### 3.2 Two appliers of the one vocabulary

- **Behavior policy** — what the behavior wants. The `ToolSelection` document is the
  unit of policy and is **behavior-owned 1:1 by convention**. No schema invention:
  the existing `AgentBehavior.tool_selection_id` pointer already permits 1:1 — we make
  owned-not-shared the norm. Sharing is reserved for presets.
- **Operator ceiling** — the host/deployment hard cap, expressed in the **same
  vocabulary**, now **category-complete** (replacing today's native-only 4-field
  `ToolCeiling`).
- **Presets** ("wide-open", future named sets) are simply *shared instances* of the
  vocabulary. There is no profile layer and no profile tools — a preset is the same
  type as any other policy, saved once and pointed at.

### 3.3 One resolution, one law

```
Effective = BehaviorPolicy ⊓ OperatorCeiling ⊓ RuntimeAvailability
```

Computed per category by the meet operations above. By construction
`Effective ⊆ OperatorCeiling` and `Effective ⊆ BehaviorPolicy` for every category
(meet is a lower bound) — the generalization of the already-proven
`activation_subset_ceiling` (`Skills.lean`).

The four scattered clamp sites collapse into a single
`BehaviorToolConfig::from_selection_*` that takes
`(behavior_policy, ceiling, runtime_snapshot)` and returns
`(effective_surface, ToolSurfaceExplanation)`.

- **`RuntimeAvailability`** is a **precomputed snapshot** (MCP-online service ids,
  active behavior ids, compiled feature flags) passed *into* resolution rather than
  probed mid-resolve. This fixes the determinism break (problem #4): the effective
  surface is now a pure function of three inputs.
- **`ToolSurfaceExplanation`** records, per category, `requested → ceiling → runtime →
  effective` with a drop reason for anything removed. It replaces the four silent
  `warn`-and-drop sites (problem #3) and is the artifact consumed by CLI `explain`
  and the desktop UI.

### 3.4 Defaults & migration

- **Secure-minimal is the default value-set**: an unset field resolves to deny / `Off`
  / `None`, not today's `true` / allow-all.
- **`wide-open` is a seeded preset** reproducing today's permissive behavior, one
  pointer away.
- This **flips the unset-defaults** for `enable_meta_tools` and `enable_defra_query`
  from `true` to secure — a deliberate breaking change. Migration: existing
  `ToolSelection` documents keep their *explicit* values (those are preserved verbatim
  through the retype); only genuinely-unset fields adopt the secure default.
  Deployments that want no behavior change point their behaviors at `wide-open`. This
  break is called out in release notes and the SP2 migration.

## 4. The proof obligation (Lean)

Per "decide the shape first, then Lean" — the shape is now fixed and the Lean scope is
decided **inside SP1**. The model:

- A per-category lattice with `Capability` (a linear order) and `EndpointScope<T>` (a
  bounded lattice on `Finset T` with `None`=⊥, `All`=⊤).
- `meet` proven **commutative, associative, idempotent**, and a **lower bound**.
- The headline theorem: `Effective ⊆ OperatorCeiling` for all categories,
  generalizing `activation_subset_ceiling` and composing with the existing
  `CommandPolicy` and `Skills` proofs (which become instances/consumers of the
  general lattice).
- **Decision deferred to SP1 spec:** whether `RuntimeAvailability` is modeled as a
  first-class lattice element now (full three-way composition) or left as an external
  assumption with `Effective ⊆ BehaviorPolicy ⊓ Ceiling` proven and runtime folded in
  as a follow-up. The shape supports either; SP1 picks based on proof cost.

Standard: zero `sorry`s; conformance tests mirror the model per the repo's
`tests/conformance/` structure fence.

## 5. Decomposition

| Sub-project | Scope | Depends on |
|---|---|---|
| **SP1 — Unified policy model** | The `Capability` / `EndpointScope` vocabulary; retyped `ToolSelection`; category-complete operator ceiling; single `from_selection_*` resolution seam; `ToolSurfaceExplanation`; the Lean lattice model + `Effective ⊆ Ceiling` theorem + conformance mirror. The spine. | — |
| **SP2 — Parity + presets + migration** | Wire every field through `ToolSelectionRow` (add `write_tools`, `subagent_default_await_mode`) and desktop mutations (`enable_defra_query` / `defra_query_collections` / `write_tools` — fixes silent data loss) and the CLI builder API; ship the secure-minimal default + the `wide-open` preset; the unset-default-flip migration. | SP1 |
| **SP3 — Straggler governance** | Bring `context_budget`, memory quota, and session-history limits under the vocabulary as first-class categories. | SP1 |

Each sub-project gets its own spec → plan → implementation cycle. SP1 is specced in
detail next.

## 6. Key files

- `crates/defra-agent/src/tool_surface/modes.rs:62-134` — ceiling shape (to generalize)
- `crates/defra-agent/src/tool_surface/behavior_config.rs:84-155` — the one real
  intersection today; becomes the single seam
- `crates/defra-agent/src/tool_surface/build.rs:14-168` — downgrade pipeline
- `crates/defra-agent/src/tool_surface/selection.rs:100-270` — current defaults/parse
- `crates/defra-agent/src/tool_surface/mod.rs:145-218` — final assembly
- `crates/defra-agent/src/tool_surface/explain.rs` — explanation surface to formalize
- `crates/defra-agent/proofs/Proofs/Skills.lean:40-88`,
  `proofs/Proofs/ToolExecution/Policy.lean`, `CommandPolicy/*.lean` — existing proofs
  to generalize/compose
- `crates/defra-agent-protocol/src/row.rs:580-637` — protocol parity gap
- `crates/defra-agent-desktop-core/src/client/mutations/manage/tools.rs` — desktop gap
- `crates/defra-agent-schemas/schemas/agent/tool_selection.graphql`,
  `agent_behavior.graphql` — schema layer
