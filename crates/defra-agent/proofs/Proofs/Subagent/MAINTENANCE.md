# Subagent Lifecycle — Maintenance Obligations

Tracks the spec ↔ proof correspondence for the subagent lifecycle.

## Theorems

| Property | Lean theorem | File |
|---|---|---|
| B1 — child .completed propagates | `bridged_child_completion_propagates` | `Properties.lean` |
| B2 — child failure projects | `bridged_child_failure_projects` | `Properties.lean` |
| B3 — cascade cancels child | `cascade_cancels_child` | `Properties.lean` |
| B3' — detach does not cascade | `detach_does_not_cancel_child` | `Properties.lean` |
| B4 — depth bound | `subagent_depth_bounded` (alias of `inv_depth`) | `Properties.lean` |
| B5 — link symmetry | `bridge_link_symmetric` (alias of `inv_link`) | `Properties.lean` |
| B6 — foreground blocks parent | `foreground_blocks_parent_advance` | `Properties.lean` |

## Invariants

| Invariant | Lean theorem | File |
|---|---|---|
| INV-FG — single foreground non-terminal | `ComposedState.invFG_preserved` | `../Composed.lean` |
| INV-UNIQUE (Composed) — distinct callIds in `tools` | `ComposedState.uniqueCallIds_preserved` | `../Composed.lean` |
| INV-UNIQUE (Bridged) — both sides preserve UniqueCallIds | `BridgedState.bridgedUniqueCallIds_preserved` | `Properties.lean` |
| INV-DEPTH — depth ≤ maxSubagentDepth | `BridgedState.inv_depth` | `Properties.lean` |
| INV-LINK — symmetric link | `BridgedState.inv_link` | `Properties.lean` |

## Maintenance rule

Per `CLAUDE.md`: any change that alters legal transitions or the invariants
this folder asserts must (a) update the relevant `Subagent/State.lean`,
`Subagent/Bridge.lean`, or `Subagent/Transition.lean` file, (b) re-prove or
re-state the affected B-theorem in `Subagent/Properties.lean`, and (c)
update the spec at `docs/superpowers/specs/2026-05-08-subagent-lifecycle-design.md`
with the new shape. The Rust runtime and conformance JSON layers consume
these theorems via constructor enumeration; renaming a constructor without
updating both layers will fail the build.

## Open obligations

(none — full proof tree is sorry-free as of commit `34da664`.)

### Tracked follow-ups (not blocking, but worth filing)

- **Spec doc tightenings.** Several divergences between spec and implementation worth folding back into the spec doc:
  (i) `bridge_complete` / `bridge_failure` are now `set`-style: they bind `idx`, `tPre`, `tPost`, require `tPre.callId = pre.bridgeCallId ∧ tPre.state = .running ∧ tPre.childRequestId = some pre.child.requestId` (plus `tPre.persistence = .committed` for `bridge_complete`), require `tPost.callId = pre.bridgeCallId ∧ tPost.childRequestId = some pre.child.requestId` along with the appropriate post state, and pin `post.parent.tools = pre.parent.tools.set idx tPost`.
  (ii) `bridge_spawn` is now `append`-style: it binds an implicit `newTool` with `newTool.callId = post.bridgeCallId ∧ newTool.state = .pending ∧ newTool.childRequestId = some post.child.requestId`, pins `post.parent.tools = pre.parent.tools ++ [newTool]`, and requires `post.child.tools = []` (freshly minted child).
  (iii) `bridge_spawn` carries `post.child.request.interruptRequestedAt = none`.
  (iv) `bridge_spawn` carries `h_callId_fresh : ∀ t ∈ pre.parent.tools, t.callId ≠ post.bridgeCallId` (callId freshness, supports INV-UNIQUE).
  (v) `bridge_cancel_cascade` carries `post.child.tools = pre.child.tools` (structural-identity guard supporting INV-UNIQUE on the child side).
  All five are load-bearing for INV-LINK / INV-UNIQUE / B1 / B2 / B3' and should be reflected in the design doc.
