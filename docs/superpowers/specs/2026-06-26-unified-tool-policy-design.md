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

5. **Parity drift across schema → protocol → desktop.** The protocol
   `ToolSelectionRow` *does* carry `enable_defra_query`
   (`defra-agent-protocol/src/row.rs:634`) and `defra_query_collections` (`:636`), but
   still **lacks `write_tools` and `subagent_default_await_mode`**. Separately, the
   desktop save path **silently drops** `enable_defra_query` /
   `defra_query_collections` / `write_tools` — not because the row can't hold them, but
   because the Tauri `ToolSelectionSaveRequest` struct omits those fields
   (`apps/desktop-tauri/src-tauri/src/bridge/types/requests.rs:~114`) and the mutation
   builder never sets them
   (`crates/defra-agent-desktop-core/src/client/mutations/manage/tools.rs:31`). Net:
   a query allowlist configured in the UI **reverts on save → data loss**.

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

Every category is expressed with one of three typed atoms:

- **`Capability`** — a ranked mode for *how much*:
  - file: `Off ≤ ReadOnly ≤ ReadWrite`
  - boolean capabilities (`Off ≤ On`): meta-tools, defra_query-enabled, subagent
    spawn / steering / background / orchestration / cross-deployment, memory,
    session-history, **context_budget** (its on/off gate; see §5 — gate is SP1,
    quota/limits are SP3).
  - Meet (`⊓`) = `min` on the rank.

- **`BashPolicy`** — bash is **not** a single rank, because it carries a
  `CommandExecutionPolicy` (`toolset/shared/command.rs:89` =
  `{mode, allowed_argv_prefixes, forbidden_argv_prefixes, network_mode,
  read_only_allowlist}`). Its meet is a **product over those fields**, each chosen so
  the result is no more permissive than either operand:
  - `mode`: `min` on `Off ≤ ReadOnly ≤ Full`.
  - `network_mode`: stricter value wins (`min` on the network-permissiveness order).
  - `forbidden_argv_prefixes`: **union** (a prefix forbidden by either is forbidden).
  - `allowed_argv_prefixes` / `read_only_allowlist`: **`EndpointScope` meet, not raw
    set intersection** — because today an **empty** `allowed_argv_prefixes` means *no
    allowed-prefix gate* (allow-all), since validation only enforces it when the vec is
    non-empty (`command.rs:288`). So empty ⇒ `All`, non-empty ⇒ `Only(set)`, and the
    meet follows `EndpointScope` rules (`All ⊓ Only(B) = Only(B)`;
    `Only(A) ⊓ Only(B) = Only(A∩B)`). Raw intersection would invert the semantics
    (`∅ ∩ X = ∅` would read as deny-all). A `forbidden` prefix overrides an `allowed`
    one.
  - sandbox availability (carried in `mode` / runtime): **fail-closed** — if either
    side says unavailable, the result is unavailable.
  - This product is itself a bounded meet-semilattice; `Effective ⊆ Ceiling` for bash
    is then a per-field consequence, not hand-waving.

- **`EndpointScope<K, V>`** — `None ≤ Only(map) ≤ All` for *which things*. The element
  type is a **typed key `K` plus an authority-bearing value `V`**, so the meet is a
  keyed map-meet, not a bare set intersection:
  - **Simple keys (V = unit):** MCP service ids, defra_query collection names,
    backgroundable tool names, cli tool names (key = tool name). Map-meet degenerates
    to set intersection.
  - **Structured values:** the riskiest categories carry value authority that the
    ceiling *narrows*:
    - **`write_tools`** — key = **`(tool_name, collection_id)`** (a pair); value =
      `allowed_field_set`. The collection is part of the *key*, not the value — this
      matches the Lean model `EndpointScope (String × String) (Finset String)`
      (`ToolPolicy/Types.lean`). A behavior *requests* a write tool against a
      `(tool, collection)`; the ceiling *constrains* the permitted field set for that
      exact pair; the effective declaration is the **constrained intersection**
      (effective fields = requested ∩ ceiling-allowed). **A `(tool, collection)`
      mismatch — behavior requests `(wt, coll1)`, ceiling allows `(wt, coll2)` — yields
      an EMPTY effective key set (deny), not a merge.** Keying by tool-name-only would
      silently keep a write tool the ceiling meant to deny for a different collection —
      do not do that. Exact-declaration equality is rejected as too brittle; name-only
      is rejected as too weak *and unsound*.
    - **`subagent_targets`** — key = `(did, behavior)`; value carries await/mode prefs.
      Meet = key intersection; value prefs are behavior-authoritative (not narrowed),
      bounded separately by the `cross_deployment` capability.
    - **`cli_tools`** — key = tool name; value = `{binary_path, working_dir, env}`,
      which today is **not** root-clamped (audit gap); the ceiling value narrows
      `working_dir`/`binary_path` to the effective root.
  - Meet on the lattice skeleton: `Only(A) ⊓ Only(B) = Only(keymeet(A,B))`;
    `Only(A) ⊓ All = Only(A)`; `x ⊓ None = None`; `All ⊓ All = All`.

`None`, `Only`, and `All` are **distinct values**. This is the single change that
eliminates the empty=allow-all ambiguity (problem #2): there is no longer a bare empty
list with an overloaded meaning anywhere.

**Category-completeness carve-out (`custom_tools`).** "Category-complete" means complete
over the **document-driven** tool categories — every tool a `ToolSelection` document can
configure, which the operator ceiling therefore governs. It deliberately excludes
`custom_tools` (`tool_surface/mod.rs` `CustomToolFactory`): those are **code-injected** at
runtime-construction time, not configured by any document, so they sit at a higher trust
boundary (whoever links the binary) than the document control plane. They are an intentional
out-of-band extension point, not an escape hatch. SP1-Rust must not silently treat them as
ceiling-governed; capping code-injected tools by the document ceiling is a separate explicit
decision (add a `custom` Surface field first). This carve-out is documented on `Surface` in
`ToolPolicy/Types.lean`.

**`context_budget` reconciliation.** The model makes `context_budget` a gateable boolean
capability (correct, forward-looking). The runtime gained its `enable_context_budget` gate
in PR #526, which post-dates this branch's base — so SP1-Rust must rebase onto current
`main` (or carry #526) so the runtime honors the gate the model assumes; otherwise the tool
stays unconditionally on and the model↔runtime contract is violated.

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
  - **Storage constraint:** the runtime document view loads `ToolSelection` rows
    scoped by `agent_did` (`list_tool_selection_records(node, agent_did)`,
    `agent/document_view/load.rs:43`) and **rejects cross-agent referenced selections**
    with a warning (`load.rs:205`), so a single *global* preset row is invisible to
    other principals. The chosen no-schema-change route is
    **per-principal seeded preset rows with canonical IDs** (e.g. a `wide-open`
    selection id seeded per DID at provisioning). "One pointer away" therefore means
    "point at the canonical preset id for your principal", not a shared global row.
    (Cross-principal shared presets would require relaxing the `agent_did` hydration
    filter — explicitly out of scope here.)

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
  - **Source & watcher contract (SP1 must specify, not leave implicit).** Today MCP
    presence is probed *inside* resolution via `has_registered_mcp_services`; that
    moves out. The snapshot's MCP-online set is built from **`ServiceHealthMap`**
    (`health_checker.rs:218`), which already derives online services from
    `ToolServiceRegistry` rows (`health_checker.rs:404-431`) plus live probe health —
    so availability = *registered AND healthy*. **Note this is a deliberate
    strictness change** the SP1 plan must ratify with tests: today's call-time
    `enforce_health_gate` (`meta_tools/shared.rs:366`) rejects only `Unreachable` and
    lets `Stale` through with a warning. SP1 decides whether availability is
    strict-`Healthy`-only or `not-Unreachable`, and either way the dropped/degraded
    state appears as an explicit `ToolSurfaceExplanation` drop reason rather than a log
    warning. The gaps SP1 closes: (a) the document runtime view does not currently load `ToolServiceRegistry`
    and the control watcher does not handle those rows — SP1 wires that load; (b) SP1
    defines the **republish trigger**: a change in the `ServiceHealthMap` online-set
    (registry add/remove or health transition) bumps the runtime generation and
    recomputes affected behaviors' effective surfaces. Active-behavior ids come from
    the existing deployment view; feature flags are compile-time constants. The
    snapshot is thus a value with a defined provenance and a defined invalidation
    event, not an ambient query.
- **`ToolSurfaceExplanation`** records, per category, `requested → ceiling → runtime →
  effective` with a drop reason for anything removed. It replaces the four silent
  `warn`-and-drop sites (problem #3) and is the artifact consumed by CLI `explain`
  and the desktop UI.
  - **Post-build generated tools must be classified, not invisible.** `load_skill` is
    appended *after* `tool_surface.build_tools()` (`agent/runtime/context.rs:98`),
    already scoped to the behavior's skill+tool ceiling. For "category-complete"
    control and a truthful explanation, SP1 classifies it explicitly — either as a
    `skills` capability or as a named *generated system tool* — and includes it in
    `ToolSurfaceExplanation` rather than letting it bypass the manifest.

### 3.4 Defaults & migration

- **Secure-minimal is the default value-set**: an unset field resolves to deny / `Off`
  / `None`, not today's `true` / allow-all.
- **`wide-open` is a seeded preset** reproducing today's permissive behavior.
- This **flips the unset-defaults** for `enable_meta_tools` and `enable_defra_query`
  from `true` to secure — a deliberate breaking change.

**The decode is a soundness problem, and it gates the flip — so it lives in SP1, not
SP2.** Today's fields are nullable, so a `null` cannot distinguish *"unset, relying on
the permissive legacy default"* from *"intentionally unset by a partial writer"*. If
SP1 simply flips the default-for-`null`, it **silently changes the behavior of every
existing document** — most dangerously behaviors with **no `tool_selection_id` at
all**, which resolve entirely from defaults. We cannot flip safely on top of ambiguous
data. SP1 therefore owns the decode foundation:

1. **A policy schema version** stamped on `ToolSelection` (and on the resolution
   input). Legacy/unversioned documents decode under *legacy-permissive* semantics
   (preserving today's behavior bit-for-bit); only documents at the new version decode
   under secure-minimal.
2. **A one-time backfill** that rewrites legacy documents to explicit values (the
   permissive defaults they were silently relying on) and stamps the version, so that
   after backfill every `null` is unambiguously *intentional*.
3. Only **after** a document is versioned/backfilled does the secure-default-for-unset
   semantics apply to it. Behaviors with no `tool_selection_id` get an explicit seeded
   default selection during backfill rather than inheriting an implicit flip.

SP2 then layers the *ergonomics* on this foundation (the `wide-open` preset wiring,
release-note guidance, desktop/CLI surfacing of the version). The semantic flip itself
is SP1-gated and backfill-gated.

## 4. The proof obligation (Lean)

Per "decide the shape first, then Lean" — the shape is now fixed and the Lean scope is
decided **inside SP1**. The model:

- A per-category lattice with three atom families: `Capability` (a linear order),
  `BashPolicy` (the **product lattice** over `{mode, network_mode, allowed/forbidden
  argv prefixes, read-only allowlist, sandbox-availability}` defined in §3.1, each
  factor a bounded meet-semilattice), and `EndpointScope<K,V>` (a bounded lattice;
  skeleton `None`=⊥, `All`=⊤, with `Only(map)` carrying a keyed map whose values meet
  per the value authority in §3.1).
- `meet` proven as a **lower bound**, with idempotence and the load-bearing
  commutativity/idempotence facts used by the implementation. Full associativity is
  intentionally not load-bearing because `effective` fixes the composition order
  `(behavior ⊓ ceiling) ⊓ runtime`; the product/keyed lower-bound facts are exactly
  why bash and `write_tools` get `Effective ⊆ Ceiling` non-hand-wavily.
- The headline theorem: `Effective ⊆ OperatorCeiling` for all categories,
  generalizing `activation_subset_ceiling` and composing with the existing
  `CommandPolicy` and `Skills` proofs (which become instances/consumers of the
  general lattice — `CommandPolicy` is the `BashPolicy` product instance).
- **Decision deferred to SP1 spec:** whether `RuntimeAvailability` is modeled as a
  first-class lattice element now (full three-way composition) or left as an external
  assumption with `Effective ⊆ BehaviorPolicy ⊓ Ceiling` proven and runtime folded in
  as a follow-up. The shape supports either; SP1 picks based on proof cost.

Standard: zero `sorry`s; conformance tests mirror the model per the repo's
`tests/conformance/` structure fence.

## 5. Decomposition

| Sub-project | Scope | Depends on |
|---|---|---|
| **SP1 — Unified policy model** | The `Capability` / `BashPolicy` / `EndpointScope` vocabulary (incl. the `context_budget` **on/off gate** — required for "category-complete"); retyped `ToolSelection`; **category-complete operator ceiling** (all 11 categories, not native-only); the **policy schema version + decode + one-time backfill** (the soundness foundation that gates the secure-default flip, §3.4); the **`RuntimeAvailability` input type** with its `ServiceHealthMap`-backed source + republish contract (§3.3); single pure `from_selection_*` resolution seam; the **`ToolSurfaceExplanation` contract** (`requested → ceiling → runtime → effective` + drop reasons); the Lean lattice model + `Effective ⊆ Ceiling` theorem + conformance bridge into the production resolver. The spine. | — |
| **SP2 — Parity + presets + ergonomics** | Wire every field through `ToolSelectionRow` (add `write_tools`, `subagent_default_await_mode`) and the desktop save path (`enable_defra_query` / `defra_query_collections` / `write_tools` — fixes the `ToolSelectionSaveRequest`/mutation-builder omission and its silent data loss) and the CLI builder API; seed the per-principal `wide-open` preset rows + canonical IDs (§3.2); release-note guidance; desktop/CLI surfacing of the policy version + `ToolSurfaceExplanation`. (The semantic flip + backfill themselves are SP1.) | SP1 |
| **SP3 — Straggler quotas** | ~~Bring `context_budget` **quota/limits** (the gate is SP1), memory per-agent quota, and session-history limits under the vocabulary as governed values.~~ **De-scoped 2026-06-30 (see below).** | SP1 |

Each sub-project gets its own spec → plan → implementation cycle. SP1 is specced in
detail next.

### SP3 — de-scoped (2026-06-30)

The "governed numeric quota" atom was considered and **rejected as YAGNI** after a
current-state audit. The three candidate consumers did not justify a new operator-ceiling
primitive:

- **Memory per-agent quota — dropped.** Memory is per-agent unbounded today (only per-entry
  caps: 256-char key / 32k-char value), but it is a trivial protection unlikely to bind in
  production. Not worth a new policy atom.
- **`context_budget` — out of scope.** It is not an operator quota: the real numbers already
  live on the per-behavior `InferenceProfile` (`context_window` / `max_output_tokens` /
  `max_turns`) and `compaction_threshold`, and they already drive compaction per conversation
  (`compaction.rs::needs_compaction`, fed by `behavior.context_window`). The `context_budget`
  tool already *surfaces* per-session utilization. No change needed; no per-session override.
- **Session-history limits — reduced to a de-cap.** Replace the hardcoded
  `MAX_LIMIT` (50→1000) and `REQUEST_SCAN_LIMIT` (500→5000) backstops in
  `toolset/session_history.rs` with large values so the caller-requested count is honored in
  practice; no new document fields, no operator ceiling, no Lean. (`REQUEST_SCAN_LIMIT` stays
  `>= MAX_LIMIT` so the larger cap is reachable.) This is all SP3 ships.

### SP1-Rust handoff notes

The SP1 Lean/conformance slice fixes the model boundary and emitted cases, but SP1-Rust
must carry the runtime-specific representation choices:

- Build the `RuntimeAvailability` snapshot from the service registry and decide the
  MCP health strictness used before feeding availability into `effective`.
- Represent `EndpointScope.Only(∅)` for bash allowed-prefix gates as deny-all on the
  wire; it is distinct from an empty `allowed_argv_prefixes` list, which remains
  allow-all. When translating the model back to today's validator, also preserve or
  explicitly revise the read-only-mode distinction that an empty wire list skips the
  prefix requirement but does not make `allowed_prefix_matched` true.
- Keep the `tool_policy_mirror.rs` conformance filename only as a compatibility
  adapter: it translates Lean's compact JSON view into the production
  `ToolPolicySurface` resolver, not an independent Rust meet.
- Preserve the structured lookup/root-narrowing semantics, including precise CLI
  path-prefix containment where the Lean SP1 model uses finite-set intersection as the
  reduced proof stand-in.
- **Bash `forbidden` / `read_only_allowlist` ceiling-narrowing — DONE (2026-06-30).**
  The production `ToolPolicyBash` now mirrors all six factors: `forbidden_argv_prefixes`
  (union meet) and `read_only_allowlist` (`EndpointScope` intersection meet) joined
  `mode`/`network`/`allowed`/`sandbox`. `from_selection` reads them from the behavior's
  `CommandExecutionPolicy` (empty `read_only_allowlist` → `All` top, asymmetric with the
  allowed-prefix gate), and the conformance case
  `bash_forbidden_union_and_readonly_intersection` exercises the proven
  `bash_meet_forbidden_superset` / `bash_meet_readonly_*` bounds end-to-end through the
  real resolver. **Executable projection — DONE (2026-06-30):** the effective bash policy
  now binds at command time. `build_host_tools` threads `static_policy.bash` into
  `constrain_command_policy_to_effective_bash`, which overlays the meet onto the executable
  `CommandExecutionPolicy` (forbidden union, allowed-prefix narrowing, read-only allowlist,
  mode/network). An `Only(∅)` allowed scope is carried by a new `deny_all_argv` sentinel on
  `CommandExecutionPolicy` — an empty allowed list means allow-all, so deny-all needs an
  explicit flag (the `Only(∅) ≠ All` trap at the executable boundary). Today's behavior is
  preserved when nothing narrows (no command policy + unconstrained effective bash → no
  executable policy). Residual edge: mode/network-only ceiling narrowing of a behavior that
  itself sets no command policy is not synthesized (never enforced for such a behavior; no
  regression).

## 6. Key files

- `crates/defra-agent/src/tool_surface/modes.rs:62-134` — ceiling shape (to generalize)
- `crates/defra-agent/src/tool_surface/policy.rs` — production `EndpointScope` /
  `ToolPolicySurface` vocabulary and meet
- `crates/defra-agent/src/tool_surface/behavior_config.rs:84-155` — the one real
  intersection today; becomes the single seam
- `crates/defra-agent/src/tool_surface/build.rs:14-168` — downgrade pipeline
- `crates/defra-agent/src/tool_surface/selection.rs:100-270` — current defaults/parse
- `crates/defra-agent/src/tool_surface/mod.rs:145-218` — final assembly
- `crates/defra-agent/src/tool_surface/explain.rs` — explanation surface to formalize
- `crates/defra-agent/src/agent/document_view/load.rs:43,205` — `agent_did`-scoped
  selection load + cross-agent rejection (the preset-storage constraint, §3.2)
- `crates/defra-agent/src/meta_tools/shared.rs:366` — `enforce_health_gate` (today:
  rejects `Unreachable` only, `Stale` passes — the §3.3 health-strictness decision)
- `crates/defra-agent/src/agent/runtime/context.rs:98` — `load_skill` appended after
  `build_tools()` (the §3.3 explanation-classification item)
- `crates/defra-agent/src/toolset/shared/command.rs:89` — `CommandExecutionPolicy`
  fields (the `BashPolicy` product meet, §3.1)
- `crates/defra-agent/src/health_checker.rs:218,404-431` — `ServiceHealthMap` +
  `ToolServiceRegistry` (the `RuntimeAvailability` source, §3.3)
- `crates/defra-agent/proofs/Proofs/Skills.lean:40-88`,
  `proofs/Proofs/ToolExecution/Policy.lean`, `CommandPolicy/*.lean` — existing proofs
  to generalize/compose
- `crates/defra-agent-protocol/src/row.rs:634,636` — defra_query fields present;
  `write_tools` / `subagent_default_await_mode` still absent (parity gap)
- `apps/desktop-tauri/src-tauri/src/bridge/types/requests.rs:~114`
  (`ToolSelectionSaveRequest`) and
  `crates/defra-agent-desktop-core/src/client/mutations/manage/tools.rs:31` — desktop
  save omits defra_query / write_tools fields → data loss
- `crates/defra-agent-schemas/schemas/agent/tool_selection.graphql`,
  `agent_behavior.graphql` — schema layer
