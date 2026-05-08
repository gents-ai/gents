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
| INV-UNIQUE — distinct callIds in `tools` | `ComposedState.uniqueCallIds_preserved` | `../Composed.lean` |
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

- **`UniqueCallIds` BridgedState lift.** Path (a) is now landed: `ComposedState.UniqueCallIds` is a structural invariant proven preserved by every `Composed.Transition` constructor (`uniqueCallIds_preserved`). B3' (`detach_does_not_cancel_child`) consumes `pre.parent.UniqueCallIds` directly (via `UniqueCallIds.eq_of_callId_eq`), and `bridge_spawn` carries an `h_callId_fresh` precondition. The remaining lift — a full BridgedState-level "parent and child UniqueCallIds preserved across any trace" theorem — requires tightening the `bridge_complete` / `bridge_failure` constructors to fully describe `post.parent.tools` (today they only specify a witness + non-bridge-tool survival, which underdetermines post). Tracked via [TODO: file issue].
- **Spec doc tightenings.** Three divergences between spec and implementation worth folding back into the spec doc: (i) `bridge_complete` / `bridge_failure` carry `childRequestId = some pre.child.requestId` in their post-tool existential, (ii) `bridge_spawn` carries `post.child.request.interruptRequestedAt = none`, and (iii) `bridge_spawn` now carries `h_callId_fresh : ∀ t ∈ pre.parent.tools, t.callId ≠ post.bridgeCallId` (callId freshness, supports INV-UNIQUE preservation). All three are load-bearing for INV-LINK / B3' / INV-UNIQUE and should be reflected in the design doc.
