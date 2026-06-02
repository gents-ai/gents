# Skills Integration Investigation (#340)

**Branch:** `design/issue-340-skills-investigation` (off `origin/main` @ df67ae63)
**Issue:** sourcenetwork/defra-agent#340 · follow-up from #333 (Codex frontend)
**Type:** Investigation / design-first. Deliverable is a design plan + follow-up impl issues, **not** code.

## Mission

Codex (now usable as a defra-agent frontend via #333) has a **skills** system — reusable instructions/workflows/tool integrations activated by description. Study it as a guide, then propose a **Defra-native skill representation**: how skills should be stored, activated, and how they interact with behavior config, prompts, tool policy, and remote/fleet operation.

The north star is defra-agent's core principle (CLAUDE.md): **the data store *is* the control plane — everything is a DefraDB document.** A skill should almost certainly be a document type, not a filesystem convention. The investigation should pressure-test that.

## Grounding — what already exists (read these first)

- **Codex integration:** `crates/defra-agent-cli/src/commands/codex_shim/` (extensive: `protocol.rs`, `handlers/`, `turn_projection.rs`, `thread_projection.rs`, `store.rs`, `compat.rs`, …) and `crates/defra-agent/src/chatgpt_codex.rs`. Understand how the shim projects defra-agent's request/response model onto Codex's turn lifecycle — that's where Codex skills would surface.
- **Behavior / tool surface (where skills attach):**
  - Schema `crates/defra-agent-protocol/schemas/agent/agent_behavior.graphql` — `AgentBehavior` (prompt, tools, model, backend policy). Per CLAUDE.md, behaviors are the reusable interface layer (vs. `AgentPrincipal` the identity boundary).
  - Schema `.../agent/tool_selection.graphql` and `crates/defra-agent/src/tool_surface/` (`behavior_config.rs`) — how tool policy is expressed per behavior today.
  - Schema `.../agent/task.graphql` — `Task` (reusable prompt template + target behavior + output schema). **A "skill" overlaps conceptually with both `AgentBehavior` and `Task`** — the investigation must articulate the boundary: is a skill a new document type, a facet of behavior, a kind of task, or a composition primitive across them?
- **No `skill` concept exists today** (confirmed — current `skill` grep hits in the codebase are incidental). This is greenfield.

## Investigation tasks (from #340)

1. **Study the Codex skills model.** How does Codex represent a skill (format, frontmatter, activation/description-matching, tool bundling, instructions)? Which parts map directly to defra-agent concepts, which don't?
2. **Propose a Defra-native skill representation:** storage model (a `Skill` collection? fields?), activation rules (description-match? explicit binding? trigger-driven?), and versioning/ownership (which principal owns a skill; least-privilege per CLAUDE.md's principal/behavior split).
3. **Define interactions:** how skills compose with behavior config, prompt assembly (`crates/defra-agent/src/prompt.rs`), tool policy (`tool_surface/`), and **remote/fleet operation** (skills replicate as documents — what are the P2P/permission implications? cross-ref the deployment routing model and #107/#180 P2P-admin work).
4. **Migration/import:** path for users who already have Codex skills → Defra skills.
5. **Test/conformance surface:** what needs covering before implementation. If skills touch any state-machine/lifecycle behavior, that starts in Lean (CLAUDE.md dev flow). If skills are purely declarative documents (config + prompt assembly), the conformance surface is apply-time validation + prompt-assembly tests — say which it is.

## Deliverable

A short design plan written to `docs/superpowers/specs/2026-06-02-skills-integration-design.md` (match the existing spec format in that dir), covering: the Codex→Defra mapping, the recommended skill document model, activation + tool-policy interaction, fleet/permission implications, migration path, and the test/conformance surface. End with **recommended implementation slices** and **open questions**, then file follow-up impl issues if the plan is accepted.

## Open framing questions to resolve in the plan

- Skill vs. `AgentBehavior` vs. `Task` — three overlapping reusable-instruction concepts. Where are the boundaries? Should a skill be *composed into* a behavior's prompt/tool-policy at activation time?
- Activation: model-driven (description-match, like superpowers/Codex) vs. operator-bound (declared on a behavior) vs. trigger-driven (an `EventTrigger`/`Schedule` activates a skill)? Probably some mix — specify it.
- Permissions: a skill that bundles tools is a privilege grant. How does that reconcile with per-behavior `tool_selection` and the principal least-privilege boundary?

## Provenance

Worktree staged during the 2026-06-02 triage session, alongside an observability-scope (#338) investigation happening in parallel on `main`.
