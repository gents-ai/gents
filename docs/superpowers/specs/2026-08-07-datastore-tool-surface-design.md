# Datastore tool surface (design)

## Status

**Draft — iterate on this PR.** No implementation yet. Companion to the
EventTrigger graph experiments design
(`2026-08-07-event-trigger-graph-experiments-design.md`): that work proved
document-pipeline DAGs with **hand-authored** `write_tools`; this design makes
granting those tools reusable without re-listing fields on every tool
selection.

## Problem

Document-pipeline multi-agent work needs a tight loop:

1. Author domain collections (seed, findings, claims, …).
2. Give models **narrow create tools** for those collections.
3. Wire **Tasks** + **EventTriggers** so creates advance the graph.

Today step 2 is copy-paste: each `ToolSelection` carries a
`write_tools: [String]` column of JSON `WriteToolDecl`s (tool name, collection,
description, fields). That works — the pipeline experiment used it — but:

- Every experiment/arm re-declares the same field list already fixed by SDL.
- Sharing one “research write surface” across behaviors means duplicating the
  same decls on every tool selection.
- Authors who want another collection must know the `WriteToolDecl` shape, not
  just the schema they already wrote.

We do **not** want to expose full GraphQL / Defra to the model. The model should
see ordinary tools with JSON-schema args; the runtime should perform one
validated create into one collection (existing `BoundedWriteTool` path).

## Decision

Add a small **config document** that names a reusable set of datastore create
tools, and **link it from `ToolSelection`**. At tool-surface resolve time, the
runtime expands the link into the same `WriteToolDecl` / `BoundedWriteTool`
machinery already used today.

```text
DatastoreToolSurface (desired-state / live config doc)
  surface_id, agent_did, entries[]  →  create tools for allowlisted collections
           │
           ▼
ToolSelection.datastore_tool_surface_ids: [String]
           │
           ▼
BehaviorToolConfig / tool surface build
           │
           ▼
BoundedWriteTool  (unchanged execution path)
```

**Minimum code:** schema + desired-state load/validate/apply + resolve-time
expansion into existing write-tool construction. No new tool runtime, no new
GraphQL mutation family for agents, no new trigger kinds.

## Non-goals (v1)

- **Richer EventTrigger conditions** (filters beyond today’s fragment, status
  transitions, barriers/joins, `event_kind: updated`). Separate work on top of
  document pipelines; this design does not depend on them.
- Auto-tooling every collection on the node (must be **explicit allowlist** on
  the surface).
- Update / delete / arbitrary query tools (create-only v1).
- Full `defra_query` replacement or free-form GraphQL for models.
- Generating SDL from the surface (SDL remains source of truth for types;
  surface only **references** collections/fields).
- Changing EventTrigger, Task, or fan_out_and_synthesize semantics.

## Shape

### New collection: `DatastoreToolSurface`

Apply-owned config (same ownership pattern as `ToolSelection` / `Skill`):
operators write it via desired-state or GraphQL; runtime does not mutate it.

Suggested fields (names can shift in review):

| Field | Role |
| --- | --- |
| `surface_id` | Stable unique id (desired-state handle) |
| `agent_did` | Owner principal (same scoping as other agent config) |
| `display_name` | Optional |
| `enabled` | Soft disable without unlinking |
| `entries` | List of create-tool entries (see below) |

**Entry** (one create tool):

| Field | Role |
| --- | --- |
| `tool_name` | Model-visible name (stable; e.g. `record_experiment_finding`) |
| `collection` | Existing GraphQL collection name (must exist when validated live) |
| `description` | Tool description for the model |
| `fields` | `{ name, required }[]` — same as today’s `WriteToolField` |

Storage options (pick the one that matches existing patterns with least churn):

- **Preferred for v1:** store `entries` as `[String]` of JSON-serialized entry
  objects — same precedent as `write_tools` / `subagent_targets` on
  `ToolSelection`, avoiding a nested Defra type if that is painful.
- Alternative: first-class nested type if the schema toolkit already makes that
  easy.

**Normative expand rule:** each enabled entry expands 1:1 to a `WriteToolDecl`
`{ tool_name, collection, description, fields }` and is merged into the
behavior’s write-tool list at surface build (same code path that already
advertises `write_tools`).

### Link from `ToolSelection`

Add:

```text
datastore_tool_surface_ids: [String]   # surface_id refs, same agent
```

Semantics:

- Empty / absent → no change from today (only inline `write_tools`).
- Non-empty → for each id, load `DatastoreToolSurface` for this agent; if
  missing/disabled, **fail closed** at validate (desired-state) or skip with a
  clear reconcile/unavailable signal at runtime (prefer validate-time fail for
  apply).
- Expanded decls **union** with inline `write_tools`.
- **Conflict:** two tools with the same `tool_name` (across surfaces or vs
  inline) → validate error. Do not silently overwrite.

Optional later (not v1): single `datastore_tool_surface_id` if we want one only;
list is better for composing “research writes” + “ops writes.”

### Desired-state layout

Mirror other per-doc collections:

```text
datastore-tool-surfaces/<surface_id>/object.json
```

`ToolSelection` object gains `datastore_tool_surface_ids: ["…"]`.

`Collection` enum + `CONFIG_APPLY_ORDER` / Lean apply order: place near
`ToolSelection` (surface docs before or with tool selections so apply can
resolve refs — if validate is static-only, order can match skills/MCP: write
surfaces before tool selections).

## Runtime resolve (implementation sketch)

Today (simplified):

```text
ToolSelection.write_tools → Vec<WriteToolDecl> → BoundedWriteTool per decl
```

v1 addition:

```text
for surface_id in tool_selection.datastore_tool_surface_ids:
    surface = load DatastoreToolSurface(surface_id)
    assert surface.agent_did == tool_selection.agent_did  # or allow shared library later
    assert surface.enabled
    for entry in surface.entries:
        decls.push(WriteToolDecl::from(entry))
// then existing build path on decls ∪ inline write_tools
```

No change to:

- `BoundedWriteTool` execution / GraphQL create
- Tool call lifecycle
- EventTrigger / Task / template scopes (`doc` / `event` / …)

## Validation

Static / apply-time (desired-state `validate`):

- `surface_id` unique; `agent_did` present.
- Each entry: non-empty `tool_name`, `collection`, at least a description
  (may be empty string if we allow — prefer non-empty).
- Field names non-empty; no duplicate field names within an entry.
- No duplicate `tool_name` within a surface or across linked surfaces + inline
  `write_tools` for a selection.
- Tool selection refs must resolve to a surface in the manifest (and same agent).

Live validate (when GraphQL/home available — same class as EventTrigger source
collection checks):

- `collection` exists on the node.
- Each `fields[].name` is a projectable field on that collection (optional v1
  if introspection is expensive; **at least** collection existence).

Do **not** require experiment collections to live in product `gents-schemas`;
surfaces may target app/experiment SDL registered on the node (same as today’s
write tools + EventTrigger sources).

## Authoring story (experiments / DAGs)

With this in place:

1. Apply domain SDL (`ExperimentFinding`, …).
2. Author one `DatastoreToolSurface` listing create tools for those collections.
3. Point stage tool selections at that surface id (plus any inline tools).
4. Tasks + EventTriggers as in the graph experiments design.

Fan-out, multi-stage pipelines, and deep-research–shaped graphs remain
**document creates + triggers** — this feature only removes write-tool
boilerplate. Barrier fan-in and richer trigger conditions stay **separate
workstreams**.

## Alternatives considered

| Alternative | Why not for v1 |
| --- | --- |
| Keep only inline `write_tools` | Works; poorly shareable and duplicates SDL. |
| Auto-tools for every collection | Too wide; models need explicit grant. |
| Generate tools from SDL with no surface doc | No stable place for tool_name/description/allowlist; harder to review in desired-state. |
| Give models `defra_query` + raw mutations | Full API surface; wrong trust model. |
| Fold surface into Skill | Skills are instruction+grant bundles; surfaces are pure tool materialization. Can *reference* a surface from a skill later. |

## PR plan (when implementing)

| PR | Contents |
| --- | --- |
| **This PR** | Design doc only (iterate). |
| A | Schema `DatastoreToolSurface` + `ToolSelection.datastore_tool_surface_ids`; desired-state load/validate/apply/export; empty expand (link stored, no runtime merge yet) **or** full expand if small. |
| B | Runtime expand into write-tool build; unit/conformance tests; one experiment arm or fixture uses a surface instead of inline `write_tools`. |
| C (optional) | Live validate collection/field introspection; docs in `experiments/README` / operator guide. |

Prefer A+B as one PR if the expand hook is a few dozen lines.

## Success criteria (implementation)

- A surface can be applied as desired-state and linked from a tool selection.
- Models on that selection see the same tools they would with equivalent inline
  `write_tools`.
- Creates still go through `BoundedWriteTool` (no second write path).
- Validate rejects duplicate tool names and missing surface refs.
- Graph experiment pipeline can drop inline finding write decl in favor of a
  shared surface without behavior change.

## Related code

- `WriteToolDecl` / `WriteToolField`: `crates/gents/src/document_config/tool_selection.rs`
- `BoundedWriteTool`: `crates/gents/src/defra_write/`
- Tool surface build: `crates/gents/src/tool_surface/`
- `ToolSelection` schema: `crates/gents-schemas/schemas/agent/tool_selection.graphql`
- Desired-state: `crates/gents-cli/src/desired_state/`
- Graph experiments (motivation): `docs/superpowers/specs/2026-08-07-event-trigger-graph-experiments-design.md`,
  `experiments/`
