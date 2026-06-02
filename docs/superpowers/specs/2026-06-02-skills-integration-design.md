# Defra-native skills: document model, activation, and tool-policy composition

Status: design pass — investigation deliverable for #340; implementation deferred to follow-up slices.
Date: 2026-06-02
Tracking: sourcenetwork/defra-agent#340 (follow-up from #333, the Codex frontend).
Related specs: `docs/superpowers/specs/2026-05-15-issue-193-principal-behavior-deployment-design.md`
(principal/behavior split), `docs/superpowers/specs/2026-05-19-identity-permission-runtime-design.md`
(ACP as the permission decider), `docs/superpowers/specs/2026-05-15-issue-56-transactional-apply-design.md`
(transactional apply), `docs/superpowers/specs/2026-05-13-issue-107-p2p-admin-rpc-design.md`
(fleet / P2P-admin).
Reference: OpenAI Codex skills system at commit `e5afe5bf8c` (`codex-rs/core-skills/`).

## Goal

Codex — now usable as a defra-agent frontend (#333) — ships a **skills** system: reusable
instructions activated by description, optionally declaring tool dependencies. This spec
studies that model, then proposes a **Defra-native skill representation** consistent with the
project's north star (CLAUDE.md): *the data store is the control plane — everything is a
DefraDB document.* A skill is a new document type, not a filesystem convention.

The deliverable is a design plan + recommended implementation slices + follow-up issues, **not
code**. Four design decisions were resolved with the operator during the investigation and are
recorded inline below (Decision D1–D4).

## What Codex does (study)

Codex's skill is its own **first-class, atomic concept** — distinct from any "agent",
"profile", "hook", "plugin", or MCP-server concept. Codex has no agent/profile notion at all;
skills *are* its custom-prompt mechanism. The key facts:

- **On-disk format** (`codex-rs/core-skills/src/loader.rs:39-94`): a directory with
  `SKILL.md` (YAML frontmatter `name` + `description`, markdown instruction body) plus an
  optional `agents/openai.yaml` carrying `interface` (UI), `dependencies.tools[]`, and
  `policy` (`allow_implicit_invocation`).
- **Parsed model** `SkillMetadata` (`codex-rs/core-skills/src/model.rs:12-23`): `name`,
  `description`, `short_description`, `interface`, `dependencies`, `policy`, `scope`
  (`User | Repo | System | Admin`), `path_to_skills_md`. The wire form
  (`app-server-protocol/src/protocol/v2/plugin.rs:398-417`) adds an `enabled` flag.
- **Tools are declared as `dependencies`, never granted.** `dependencies.tools[]` is
  descriptive ("this skill needs MCP tool X"); the tool must already be available. Codex never
  escalates privilege through a skill.
- **Activation has three paths, no semantic routing:**
  1. *Explicit* — `$skill-name` text token or a structured `UserInput::Skill { name, path }`
     (`codex-rs/protocol/src/user_input.rs:42-46`).
  2. *Implicit* — `policy.allow_implicit_invocation` + the user runs a script in the skill's
     `scripts/` dir or reads its `SKILL.md` (`core-skills/src/invocation_utils.rs:29-42`).
  3. *Model-heuristic, in-context* — Codex renders the full candidate list (name + description
     + path) into system context with a trigger rule: *"if the user names a skill OR the task
     clearly matches a skill's description, use that skill"* (`core-skills/src/render.rs:28`).
- **Injection** (`core-skills/src/injection.rs:31-86`): when selected, the **full `SKILL.md`
  body** is read from disk and injected as a turn context item. The candidate *listing* and the
  activated *body* are two separate injections — classic progressive disclosure.
- **Protocol surface** (`app-server-protocol/.../v2/plugin.rs`): `skills/list` (params:
  `cwds`, `force_reload`), `skills/config/write` (enable/disable by `path`|`name`),
  `skills/changed` notification.

**Already wired in defra-agent's Codex shim, currently stubbed:** `SkillsList` returns `[]`
(`crates/defra-agent-cli/src/commands/codex_shim/handlers/basic.rs:180`), `SkillsConfigWrite`
is a no-op (`.../codex_shim/compat.rs:153`), and `UserInput::Skill` is filtered out of turn
text (`.../codex_shim/protocol.rs:240`, recorded-but-unprocessed at `thread_routes.rs:206`).
The frontend hooks exist; the backend is greenfield.

## Codex → Defra concept mapping

| Codex | Defra-native | Notes |
|-------|--------------|-------|
| Skill directory + `SKILL.md` | **`Skill` document** (new collection) | Decision D1. |
| Frontmatter `name` / `description` | `name` / `description` fields | `description` drives activation matching. |
| `SKILL.md` markdown body | `instructions` field | Injected as a system-reminder when active. |
| `dependencies.tools[]` | `tool_refs` field | Declarative deps; ∩ behavior ceiling (D3). |
| `policy.allow_implicit_invocation` | `allow_implicit_invocation` (parked, see Open Q) | No filesystem in defra; v1 omits. |
| `interface` (icons, display name) | `display_name` + opaque `interface_json` | UI metadata; not load-bearing. |
| `scope` (User/Repo/System/Admin) | `Skill.scope` (`principal`\|`behavior`) within owning principal | Decision D5; cross-principal via import (D6). |
| `enabled` flag | `Skill.enabled` + behavior `skill_refs`/`skill_excludes` | Operator-bound candidate set (D2/D5). |
| `skills/list` RPC | query `Skill` collection for bound behavior | Shim wiring. |
| `UserInput::Skill { name, path }` | resolve `name` → `Skill` doc → activation | Shim wiring. |
| `skills/changed` | DefraDB Update event on `Skill` collection | Already event-native. |

## Decision D1 — a skill is a new `Skill` collection (composition primitive)

A skill is **not** a facet of `AgentBehavior`, **not** a kind of `Task`, and **not** a
`ToolSelection`. It is its own document type, composed *into* a behavior's prompt and tool
surface at activation time. This mirrors Codex (skill = atomic concept) and preserves the
existing boundaries CLAUDE.md draws:

- `AgentPrincipal` — DID identity / permission + audit boundary.
- `AgentBehavior` — the reusable *interface* (`system_prompt`, `tool_selection_id`, `model`;
  `crates/defra-agent-protocol/schemas/agent/agent_behavior.graphql`).
- `Task` — a *triggered unit of work* (`prompt_template` + `behavior_id` + `output_schema`).
- `Skill` — a reusable *instruction + tool-dependency fragment*, composed into a behavior.

Proposed schema (matching house conventions — `{entity}_id` PK, `agent_did` owner index,
`enabled` index, `created_at`/`updated_at`, `@branchable` for versioning):

```graphql
type Skill @branchable {
  skill_id: String @index(unique: true)
  agent_did: String @index            # owning principal == the node identity (D5)
  scope: String @index                # "principal" (all the principal's behaviors) | "behavior" (D5)
  name: String @index                 # activation token ($name) + display
  description: String                  # activation matching key (shown to model)
  instructions: String                # the body, injected when active
  tool_refs: [String!]                # declared tool dependencies (host kinds, mcp svc ids, cli names)
  display_name: String
  interface_json: String              # opaque UI metadata (icons, brand, default_prompt)
  enabled: Boolean @index
  created_at: DateTime @index(direction: DESC)
  updated_at: DateTime @index(direction: DESC)
}
```

Behaviors refine the inherited set (D5): `skill_refs` opt *in* to `scope: behavior` skills;
`skill_excludes` opt *out* of inherited `scope: principal` skills. Add to `AgentBehavior`:

```graphql
  skill_refs: [String!]      # narrow (scope:behavior) skill_ids this behavior opts into
  skill_excludes: [String!]  # inherited (scope:principal) skill_ids this behavior opts out of
```

`tool_refs` uses the existing `ToolSelection` vocabulary so deps are checkable against the
ceiling: host tool kinds (`file`, `bash`), `cli_tool_names`, and `allowed_mcp_service_ids`
(`crates/defra-agent-protocol/schemas/agent/tool_selection.graphql`).

## Decision D2 — hybrid activation: operator-bound set + model self-selection

Activation is the **per-request composition** of three filters, exactly paralleling Codex but
expressed over documents rather than a filesystem scan:

1. **Operator-bound candidate set** — computed by scope-on-skill inheritance (D5), not a
   hand-maintained per-behavior list. For behavior *B* under principal *P*:
   `{ s : s.agent_did == P ∧ s.enabled ∧ (s.scope == principal ∨ s.skill_id ∈ B.skill_refs) } −
   B.skill_excludes`. This is the candidate ceiling on *what skills can even be considered*; the
   `Skill.enabled` flag and `skills/config/write` toggle membership.
2. **Model self-selection** — the candidate set is rendered as a name+description listing into
   the prompt (the cached behavior-context layer, see below), with the same trigger rule Codex
   uses. The model picks; the activated skill's `instructions` body is injected.
3. **Explicit / trigger-forced** — a `UserInput::Skill { name }` from the Codex frontend forces
   a specific skill; a `Task` may name `skill_refs` to force-activate for trigger-materialized
   requests (`caused_by_trigger_*` lineage already exists, `defra-agent-protocol/src/row.rs:203`).

Activation is a **pure function of (behavior, candidate skill set, request input)** recomputed
per request — *not* sticky session state (D4). This is the declarative property that keeps the
lifecycle out of Lean.

### Prompt composition (`crates/defra-agent/src/prompt.rs`)

The layered builder already separates a cached preamble from per-turn content. Skills slot in
without busting the KV cache:

- **Candidate listing** (names + descriptions) → the cached **behavior-context layer**
  (`build_preamble`, `prompt.rs:192`, alongside tool guidance at `:226`). It is a pure function
  of `skill_refs`, so it is cache-stable per behavior.
- **Activated skill body** → injected as a `<system-reminder>` in the per-turn layer
  (`prompt.rs:150`), exactly mirroring Codex's "inject full SKILL.md as a context item." Because
  reminders ride the uncached layer, dynamic per-request activation never invalidates the
  preamble cache.

A *hot* skill that is always-on for a behavior may later be promoted into the cached layer; the
threshold is an Open Question.

## Decision D3 — tool refs are declared dependencies, intersected with the ceiling (degrade)

A skill **declares** the tools it needs; it never **grants** them. Resolution (in
`crates/defra-agent/src/tool_surface/`):

- The behavior's `ToolSelection` resolves to a `ToolSurface` ceiling exactly as today
  (`behavior_config.rs:42`, `mod.rs:84`). **Skill activation does not add tools to this surface.**
- For each `tool_ref` in an active skill, resolution checks membership in the ceiling. Present →
  the tool is already available; the skill uses it. **Absent → degrade**: the skill still
  activates, and the injected body is prefixed with a generated note listing the unavailable
  capabilities so the model adapts (Codex-faithful: deps are advisory).
- This makes activation **privilege-monotone**: the tool surface with any set of skills active
  equals the behavior ceiling — skills can only ever be a subset, never a superset. Union /
  grant semantics (the privilege-escalation path Codex deliberately avoids) is rejected.

The mix variant (required vs optional deps → gate vs degrade) was considered and deferred; v1 is
uniform degrade. Revisit if operators report silent under-provisioning is hard to diagnose.

## Decision D4 — formalize the privilege algebra in Lean; keep the lifecycle declarative

Per CLAUDE.md, anything that changes *what transitions are legal* or *what invariants hold*
starts in Lean. The skill **lifecycle** is declarative (D2: a pure per-request function, no new
request-lifecycle states, no temporal FSM) — so it introduces no new state machine. But the
**privilege invariant** from D3 is a genuine safety property of the same algebraic shape Lean
already proves for identity (`Permission.lean`'s `RespectsPrincipal`, `:25`) and tool ceilings
(`ToolSelection` downgrade monotonicity). That is where the formal effort goes.

New `proofs/Proofs/Skills/Skills.lean`:

- Model `resolveSurface : Behavior → ToolSurface` and
  `activate : Behavior → Set Skill → ToolSurface`.
- **S-Skill-1 (subsurface / privilege monotonicity):**
  `∀ b S, activate b S ⊆ resolveSurface b`. The backbone of D3 — skill activation never widens
  privilege beyond the behavior ceiling.
- **S-Skill-2 (composition closure):** the union of any set of activated skills is still
  `⊆ resolveSurface b` (no combination of skills escalates).
- **S-Skill-3 (candidate-set respects principal):** activation only ever ranges over the owning
  principal's skills resolved through the D5 effective-set formula — a skill with a different
  `agent_did`, a `scope: behavior` skill not in `skill_refs`, or one in `skill_excludes` is never
  in the activation result (ties activation to the binding gate; trivial under D6's owned-copy
  model since there is no cross-principal reference to reason about).

Extend the apply-ordering proofs (`ApplyReconcile.lean`, currently draft per the reconcile
landed-vs-draft note) so the **`Skill` collection** participates in `CONFIG_APPLY_ORDER` prefix
safety: `Skill` is written after `ToolServiceRegistry`/`ToolSelection` (it references mcp service
ids / tool kinds) and before `AgentBehavior` (behaviors reference `skill_refs`). New order:

```
InferenceBackend → InferenceProfile → ToolServiceRegistry → ToolSelection
  → Skill → AgentBehavior → Task → Schedule → EventTrigger → AgentPrincipal
```

(`crates/defra-agent-cli/src/config_import.rs:36-46`). The prefix-retry idempotence property
extends unchanged.

## Decision D5 — scope-on-skill inheritance, refined per behavior

A node *is* a principal (#193: single-principal-per-process; node identity == `agent_did`), so
"skills associated with a node" are simply the principal's own `Skill` documents — there is no
separate node tier. The ergonomic question is how those filter into the principal's behaviors.

The scope lives **on the skill**, not as a hand-maintained list on each behavior (mirroring
Codex's `scope`):

- `scope: principal` — inherited by *every* behavior of the owning principal. Authoring a
  node-wide skill is one document write; all behaviors pick it up.
- `scope: behavior` — a candidate only where a behavior names it in `skill_refs`.
- `AgentBehavior.skill_excludes` — opt a behavior *out* of an inherited principal-scoped skill.

Effective candidate set is the D2 formula above. **This is safe to be ergonomic because of
D3/S-Skill-1**: principal-wide inheritance carries *instructions* to every behavior, but a
skill's *tools* still degrade against each behavior's `tool_selection` ceiling. The per-behavior
ceiling — not the skill binding — is the real privilege boundary, so broad instruction
inheritance never escalates capability. Binding (what's a candidate) and ceiling (what's
grantable) are cleanly separated concerns.

## Decision D6 — cross-principal sharing is import/copy, not live reference

Sharing a skill across *different* principals (a published "system" library) is **distribution,
not a runtime cross-principal read**. Each principal owns its `Skill` documents outright; a
shared skill is brought in by import (`config skill import`, below) or export/import between
principals, producing an owned copy. This:

- preserves the sovereign-data model — a node's skill set is always literally its principal's
  own documents, never borrowed references;
- sidesteps an ACP cross-principal-read policy for v1 (the #180-adjacent NAC gap); and
- keeps S-Skill-3 simple — activation only ever ranges over the owning principal's skills.

Live cross-principal references (single source of truth, ACP-gated) are deferred; if demand
appears, they layer on top without changing the owned-copy default.

## Fleet / permission implications

Skills are documents, so they replicate like any other config. Three consequences:

- **Ownership = the owning principal's DID** (`Skill.agent_did`). This is the defra analogue of
  Codex's `scope`. ACP is the decider (`docs/superpowers/specs/2026-05-19-identity-permission-runtime-design.md`):
  read/activate is gated by `DocumentACP::check_doc_access` against the actor identity. A skill
  authored under one principal is, by default, private to it.
- **Replication scope** rides the existing `PeerPairingDesired` model
  (`crates/defra-agent-protocol/schemas/agent/peer_pairing_desired.graphql`): adding the `Skill`
  collection to a pairing's `collections` list makes a principal's skills available on each of
  its deployments. Per the deployment-routing model (each `(did, behavior_id)` lives on exactly
  one deployment), skill activation needs **no cross-replica coordination** — a request is
  served by one deployment, which composes from its locally-replicated skill set.
- **Privilege escalation is structurally impossible** (D3 + S-Skill-1): even if a skill
  replicates onto a deployment whose behavior has a narrow `tool_selection`, activation cannot
  grant tools beyond that ceiling. This is what makes D5's principal-wide inheritance safe.
  Cross-principal "system" skills (Codex's `Admin`/`System` scope) are handled by import/copy,
  not live cross-principal read (D6) — so no ACP cross-read policy is needed in v1.
- **Authoring authority** — who may write a `Skill` and who may add it to a behavior's
  `skill_refs` is an operator-identity (NAC) concern, the same gap tracked for P2P-admin in
  #180. The apply path's `DesiredApplyBundle` type-fence (`desired_state/apply_bundle.rs:16`)
  already prevents untrusted JSON from reaching the writer.

## Migration / import path

Codex users have on-disk `SKILL.md` (+ `agents/openai.yaml`) trees. Provide a CLI importer that
parses them into `Skill` documents, parallel to `config apply` and reusing its transaction
(`crates/defra-agent-cli/src/commands/config/apply.rs:71`):

`config skill import <dir> [--principal <did>]` — for each discovered `SKILL.md`:
frontmatter `name`/`description` → `name`/`description`; body → `instructions`;
`dependencies.tools[]` → `tool_refs` (mapping `type: mcp` → `allowed_mcp_service_ids` entries,
shell/script deps → `cli_tool_names`/`bash`); `interface` → `display_name` + `interface_json`;
`policy.allow_implicit_invocation` → recorded but inert in v1; `scope` → defaults to
`--principal`. Bundled `scripts/`/`assets/` are **not** imported (see scoping divergence).

Reverse direction (export defra `Skill` → `SKILL.md`) is symmetric. Import/export is also the
**cross-principal sharing mechanism** (D6): bringing a published skill into a principal, or
copying one principal's skill to another, is the same code path producing an owned copy — no
live cross-principal reference.

### Deliberate divergence from Codex

Codex skills can bundle executable `scripts/` and `assets/`, and implicit invocation keys off
running those scripts. Defra skills are **pure documents** — there is no skill-local filesystem.
v1 therefore scopes skills to *instructions + declared tool deps* and **omits** bundled
scripts/assets and script-triggered implicit invocation. If bundled artifacts are needed later,
they want their own story (blob fields, a sidecar collection, or an MCP service), tracked
separately.

## Test / conformance surface

This is the answer to #340 task 5. Skills are **declarative documents** (D4) — so apart from the
privilege-algebra Lean above, the conformance surface is apply-time validation + assembly tests,
*not* a new state machine.

- **Lean** — `Skills.lean` (S-Skill-1/2/3); `ApplyReconcile.lean` extended for the `Skill`
  collection; a `CoverageLedger.lean` row for the new spec. A Rust conformance test binds the
  Lean privilege cases to the actual `tool_surface` resolver (the pattern at
  `crates/defra-agent/tests/identity_conformance.rs`): construct behavior + skill fixtures,
  assert `activate ⊆ ceiling` agrees with the Lean expectations.
- **Apply-time validation** (`crates/defra-agent-cli/src/config_import.rs:84` +
  `validate_manifest_against_live`): `skill_refs`/`skill_excludes` resolve to existing `Skill`
  docs owned by the behavior's principal; `Skill.scope` is one of `principal|behavior`;
  `tool_refs` reference known host-tool kinds / registered mcp service ids / cli names. A
  `tool_ref` *not* in the behavior ceiling is a **warning, not an error** (degrade semantics,
  D3). A `skill_ref` to a foreign principal's skill is an error (D6: import it first).
- **Prompt-assembly tests** (`prompt.rs`): candidate listing renders in the cached layer and is
  cache-stable per behavior; an activated skill body injects as a system-reminder; activating a
  skill does not change the preamble cache key.
- **Tool-surface tests** (`tool_surface/`): the Rust consumer of S-Skill-1 — activating any skill
  set never adds a tool absent from the ceiling; a skill with an unmet `tool_ref` activates with
  a degrade note.
- **Shim tests** (`codex_shim/`): `SkillsList` returns the bound behavior's candidate set;
  `UserInput::Skill` resolves a name to a `Skill` and forces activation; `SkillsConfigWrite`
  toggles `enabled`.

## Recommended implementation slices

1. **Declarative core** — `Skill` schema (incl. `scope`) + `AgentBehavior.skill_refs`/
   `skill_excludes`; `document_config` loader; effective-set resolver (D5); `CONFIG_APPLY_ORDER`
   slot + apply-time validation. (No prompt/tool wiring yet.)
2. **Composition** — prompt candidate-listing (cached layer) + activated-body system-reminder
   injection; tool-ref ∩ ceiling resolution with degrade notes.
3. **Lean** — `Skills.lean` (S-Skill-1/2/3) + `ApplyReconcile` extension + `CoverageLedger` row +
   Rust conformance binding. Per CLAUDE.md this can precede slice 2's tool-surface code; the
   spec change drives the test.
4. **Codex shim wiring** — `SkillsList` query, `UserInput::Skill` activation, `SkillsConfigWrite`
   enable/disable, `skills/changed` from the DefraDB Update event.
5. **Migration** — `config skill import` (+ export follow-up).
6. **Trigger-driven activation (optional)** — `Task.skill_refs` force-activation for
   materialized requests; revisit fleet/ACP for cross-principal "system" skills.

## Open questions

- **Activation precedence / conflict.** Model picks a disabled skill → bind/enable gate wins
  (already implied). Two active skills with conflicting instructions → ordering? Is there a cap
  on simultaneously active skills (prompt budget)?
- **Cache promotion threshold.** When does an always-on skill move from per-request reminder into
  the cached behavior-context layer?
- **Live cross-principal references.** Resolved for v1 as import/copy (D6); a single-source-of-
  truth ACP-gated cross-read remains a future option if demand appears.
- **Required vs optional tool deps.** v1 is uniform degrade (D3). Promote to a per-dep
  required/optional flag if silent under-provisioning proves hard to diagnose.
- **Bundled scripts/assets + implicit invocation.** Out of scope v1 (pure documents). What's the
  artifact story if/when needed?
- **Authoring authority (NAC).** Who may write a `Skill` / edit `skill_refs`? Shares the #180
  operator-identity gap.
- **Versioning.** `@branchable` is proposed; confirm whether skills need branch/version pinning
  per behavior (a behavior pinning skill@rev) or always track latest.

## Proposed follow-up issues (file on plan acceptance)

1. *Skill collection + behavior.skill_refs + apply ordering/validation* (slice 1).
2. *Skill prompt composition + tool-ref ∩ ceiling resolution* (slice 2).
3. *Skills.lean privilege algebra + ApplyReconcile extension + conformance binding* (slice 3).
4. *Codex shim: wire SkillsList / UserInput::Skill / SkillsConfigWrite / skills/changed* (slice 4).
5. *`config skill import`/`export` (Codex SKILL.md ↔ Skill docs; doubles as cross-principal
   sharing via owned copy, D6)* (slice 5).
6. *NAC authoring authority: who may write a `Skill` / edit `skill_refs`/`skill_excludes`*
   (depends on #180).
