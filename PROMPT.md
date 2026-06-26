# PROMPT — #555: promote composed-state invariants to global, preserved-from-initial

> Worktree kickoff brief. Part of the Lean-model review umbrella **#551**.
> Issue: **#555** (merges review findings **C + H**). Heaviest Lean proof work
> of the set.

## Foundation flow (CLAUDE.md)

Lean-first; **zero `sorry`s**. This worktree already has the parent's
`.lake/packages` symlinked in, so `lake build` is fast. Build from
`crates/defra-agent/proofs`. All anchors below are in
`Proofs/CrossMachineComposed/`.

## The problem (validated)

Composed-state well-formedness is supplied as **per-tool hypotheses at each
theorem's use site**, not proven as a **global invariant established from an
initial state**. The core composed state admits incoherent/malformed tool lists
unless a caller threads the right hypotheses in.

Anchors (this branch):
- `State.lean:62` — `def Coherent (pre) (toolPre)` is **per-tool** (requestId /
  deadline / currentTime equalities for one tool). No list-level "all tools
  coherent" predicate exists.
- `State.lean:59` — `Future work (B4 persistent processes) will introduce a
  complementary Persistent coherence predicate` (finding H: TODO lives **inside**
  the live predicate's docstring).
- `State.lean:41,44` — `findToolByCallId`; comment says call-ids are *intended*
  unique "but we don't enforce that as a Prop here".
- `UniqueCallIds.lean:18` — `def UniqueCallIds`; `:59` — `theorem
  uniqueCallIds_preserved` proves **preservation under Transition** but there is
  **no `initial.UniqueCallIds` establishment**, and it is **not composed** into
  the C-theorems.
- `ToolTermination.lean` — the conditional C-theorems, each taking
  `h_coherent : Coherent pre toolPre` as a hypothesis:
  - `:54` `interrupted_request_cancels_live_linked_tools` (C2)
  - `:108` `deadline_exceeded_request_timesOut_running_tools` (C1)
  - `:162` `deadline_exceeded_request_cancels_pending_tools` (C1')
  - `:202` `all_tools_terminal_unblocks_request_progress` (C3 — quantifies over
    all terminal tools; doesn't need per-tool coherence but also isn't derived
    from an initial invariant)

## Proposed work

1. **Define list-level invariants** over `ComposedState`:
   - all-tools-coherent: `∀ t ∈ pre.tools, Coherent pre t`
   - `UniqueCallIds` (reuse the existing predicate)
   - linked request ids (every tool's `requestId = pre.requestId`)
   - "no duplicate foreground live tool"
   Consider one bundling `WellFormed` structure/Prop.
2. **Prove established at `ComposedState.initial`** and **preserved by every
   `Transition`** (compose `uniqueCallIds_preserved` and the new lemmas).
3. **Re-derive C1/C1'/C2** from the global invariant so the per-tool
   `h_coherent` hypotheses fall out instead of being assumed at the call site.
4. **Resolve finding H**: land the `B4 Persistent` coherence predicate, or move
   the TODO out of the `Coherent` docstring into an issue reference once the
   shape is settled.

## Definition of done

- A global well-formedness invariant holds at `initial` and is preserved by
  `Transition`; C-theorems consume it rather than ad-hoc hypotheses.
- `UniqueCallIds` is composed into the main theorems, not standalone.
- `lake build` green, **zero sorry**; `cargo test -p defra-agent` green.

## Watch out

- There are **pre-existing lint warnings** in `UniqueCallIds.lean` (lines ~79–95,
  `unnecessarySeqFocus` — `tac1 <;> tac2` where `(tac1; tac2)` suffices). Cheap
  to clean up while you're in the file, but they're not from #555.
- Keep the `Coherent` shape stable where the README claims "existing theorem
  bodies don't need to change" — verify that promise survives your refactor.
