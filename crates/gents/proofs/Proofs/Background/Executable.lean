import Proofs.Background.Transition

/-!
# Subagent Bridge Executable Semantics (#937)

Executable single-step surface for the bridge-local transitions. Previously
`step` rejected every event and the refinement theorem was vacuous, so every
backgrounding conformance row was hand-authored. This version executes the
three bridge-local events on the subagent leg:

* `bridge_complete` — durable child `.completed` projects onto the running,
  committed bridge tool;
* `bridge_failure` — a non-completed durable child terminal projects onto the
  running bridge tool via `ChildTerminal.projectedToolState`
  (interrupted → cancelled, everything else → failed);
* `bridge_cancel_cascade` — a terminal parent (or already-cancelled bridge
  tool) with a cascade-policy bridge sets the child's
  `interruptRequestedAt`.

`parent_step` / `child_step` carry opaque composed-state payloads and
`bridge_spawn`'s post-state mints fresh ids from outside the model, so those
remain relational-only (`step` returns `none`; the constructors are the
contract).

The native background tool (R6, childless row) is deliberately **not** this
surface: its single-row lifecycle is executable through
`ToolExecution.Executable.step?` (dispatch/complete/fail/cancelDuringRun and
the `background` mode arm); Rust's `bridge_complete`/`bridge_failure` on a
childless row refine those single-row transitions at the same persistence
seam.
-/

namespace Subagent
namespace BridgedState

/-- An event that selects which bridge Transition to apply. -/
inductive Event where
  | parent_step           (innerEventOpaque : Unit)
                            -- Opaque composed-state event payload.
  | child_step            (innerEventOpaque : Unit)
  | bridge_spawn          (newCallId : ToolExecution.ToolCallId)
                          (newChildRid : RequestId)
  | bridge_complete
  | bridge_failure
  | bridge_cancel_cascade
  deriving Repr

/-- First tool slot carrying the bridge callId: index plus the row. CallIds
    are unique within a well-formed composed state (`UniqueCallIds`), so
    "first" is "the" bridge slot on reachable states. -/
def findBridgeSlot? (tools : List ToolExecution.ToolCallContext)
    (callId : ToolExecution.ToolCallId) :
    Option (Nat × ToolExecution.ToolCallContext) :=
  match tools with
  | [] => none
  | t :: rest =>
      if t.callId = callId then
        some (0, t)
      else
        match findBridgeSlot? rest callId with
        | none => none
        | some (idx, u) => some (idx + 1, u)

theorem findBridgeSlot?_spec
    (tools : List ToolExecution.ToolCallContext)
    (callId : ToolExecution.ToolCallId)
    {idx : Nat} {t : ToolExecution.ToolCallContext}
    (h : findBridgeSlot? tools callId = some (idx, t)) :
    tools[idx]? = some t ∧ t.callId = callId := by
  induction tools generalizing idx with
  | nil => simp [findBridgeSlot?] at h
  | cons head rest ih =>
    by_cases h_id : head.callId = callId
    · simp [findBridgeSlot?, h_id] at h
      obtain ⟨h_idx, h_t⟩ := h
      subst h_idx h_t
      exact ⟨rfl, h_id⟩
    · simp only [findBridgeSlot?, if_neg h_id] at h
      cases h_rest : findBridgeSlot? rest callId with
      | none => rw [h_rest] at h; cases h
      | some slot =>
          obtain ⟨restIdx, restTool⟩ := slot
          rw [h_rest] at h
          simp only [Option.some.injEq, Prod.mk.injEq] at h
          obtain ⟨h_idx, h_t⟩ := h
          subst h_t
          obtain ⟨h_get, h_call⟩ := ih h_rest
          subst h_idx
          exact ⟨by simpa using h_get, h_call⟩

/-- The bridge slot's row is a member of the tool list. -/
theorem findBridgeSlot?_mem
    (tools : List ToolExecution.ToolCallContext)
    (callId : ToolExecution.ToolCallId)
    {idx : Nat} {t : ToolExecution.ToolCallContext}
    (h : findBridgeSlot? tools callId = some (idx, t)) :
    t ∈ tools := by
  obtain ⟨h_get, _⟩ := findBridgeSlot?_spec tools callId h
  obtain ⟨h_lt, h_eq⟩ := List.getElem?_eq_some_iff.mp h_get
  exact h_eq ▸ List.getElem_mem h_lt

/-- `bridge_complete` guard + post-state for one located bridge slot. -/
def completePost (s : BridgedState) (idx : Nat)
    (tPre : ToolExecution.ToolCallContext) : Option BridgedState :=
  if s.terminalOf = .completed ∧
      tPre.state = .running ∧
      tPre.persistence = .committed ∧
      tPre.childRequestId = some s.child.requestId then
    some { s with parent := { s.parent with
      tools := s.parent.tools.set idx { tPre with state := .completed } } }
  else
    none

/-- `bridge_failure` guard + post-state for one located bridge slot. -/
def failurePost (s : BridgedState) (idx : Nat)
    (tPre : ToolExecution.ToolCallContext) : Option BridgedState :=
  if s.terminalOf.isFailure ∧
      tPre.state = .running ∧
      tPre.childRequestId = some s.child.requestId then
    some { s with parent := { s.parent with
      tools := s.parent.tools.set idx
        { tPre with state := s.terminalOf.projectedToolState } } }
  else
    none

/-- `bridge_cancel_cascade` guard + post-state for one located bridge slot. -/
def cascadePost (s : BridgedState)
    (tBridge : ToolExecution.ToolCallContext) : Option BridgedState :=
  if (isTerminal s.parent.request.state ∨ tBridge.state = .cancelled) ∧
      tBridge.cancelPolicy = .cascade then
    some { s with child := { s.child with request :=
      { s.child.request with
          interruptRequestedAt := some s.child.request.currentTime } } }
  else
    none

/-- Executable single-step for the bridge-local events (subagent leg). -/
def step (s : BridgedState) (e : Event) : Option BridgedState :=
  match e with
  | .bridge_complete =>
      match findBridgeSlot? s.parent.tools s.bridgeCallId with
      | none => none
      | some slot => completePost s slot.1 slot.2
  | .bridge_failure =>
      match findBridgeSlot? s.parent.tools s.bridgeCallId with
      | none => none
      | some slot => failurePost s slot.1 slot.2
  | .bridge_cancel_cascade =>
      match findBridgeSlot? s.parent.tools s.bridgeCallId with
      | none => none
      | some slot => cascadePost s slot.2
  | _ => none

/-- Soundness: every executable step refines a bridge Transition. -/
theorem step_refines_transition
    (s s' : BridgedState) (e : Event)
    (h : step s e = some s') :
    Transition s s' := by
  cases e with
  | parent_step _ => simp [step] at h
  | child_step _ => simp [step] at h
  | bridge_spawn _ _ => simp [step] at h
  | bridge_complete =>
      unfold step at h
      cases h_find : findBridgeSlot? s.parent.tools s.bridgeCallId with
      | none => simp [h_find] at h
      | some slot =>
          obtain ⟨idx, tPre⟩ := slot
          simp only [h_find] at h
          obtain ⟨h_get, h_callId⟩ :=
            findBridgeSlot?_spec s.parent.tools s.bridgeCallId h_find
          unfold completePost at h
          by_cases h_guard : s.terminalOf = .completed ∧
              tPre.state = .running ∧
              tPre.persistence = .committed ∧
              tPre.childRequestId = some s.child.requestId
          · rw [if_pos h_guard] at h
            obtain ⟨h_done, h_running, h_committed, h_child⟩ := h_guard
            cases h
            exact Transition.bridge_complete
              (idx := idx) (tPre := tPre)
              (tPost := { tPre with state := .completed })
              h_done h_get h_callId h_running h_committed h_child
              h_callId rfl h_child rfl rfl rfl rfl rfl
          · rw [if_neg h_guard] at h
            exact Option.noConfusion h
  | bridge_failure =>
      unfold step at h
      cases h_find : findBridgeSlot? s.parent.tools s.bridgeCallId with
      | none => simp [h_find] at h
      | some slot =>
          obtain ⟨idx, tPre⟩ := slot
          simp only [h_find] at h
          obtain ⟨h_get, h_callId⟩ :=
            findBridgeSlot?_spec s.parent.tools s.bridgeCallId h_find
          unfold failurePost at h
          by_cases h_guard : s.terminalOf.isFailure ∧
              tPre.state = .running ∧
              tPre.childRequestId = some s.child.requestId
          · rw [if_pos h_guard] at h
            obtain ⟨h_failed, h_running, h_child⟩ := h_guard
            cases h
            exact Transition.bridge_failure
              (idx := idx) (tPre := tPre)
              (tPost := { tPre with state := s.terminalOf.projectedToolState })
              h_failed h_get h_callId h_running h_child
              h_callId
              (ChildTerminal.projected_failure_state s.terminalOf h_failed)
              h_child rfl rfl rfl rfl rfl
          · rw [if_neg h_guard] at h
            exact Option.noConfusion h
  | bridge_cancel_cascade =>
      unfold step at h
      cases h_find : findBridgeSlot? s.parent.tools s.bridgeCallId with
      | none => simp [h_find] at h
      | some slot =>
          obtain ⟨idx, tBridge⟩ := slot
          simp only [h_find] at h
          obtain ⟨_, h_callId⟩ :=
            findBridgeSlot?_spec s.parent.tools s.bridgeCallId h_find
          have h_mem : tBridge ∈ s.parent.tools :=
            findBridgeSlot?_mem s.parent.tools s.bridgeCallId h_find
          unfold cascadePost at h
          by_cases h_guard :
              (isTerminal s.parent.request.state ∨ tBridge.state = .cancelled) ∧
                tBridge.cancelPolicy = .cascade
          · rw [if_pos h_guard] at h
            obtain ⟨h_term, h_policy⟩ := h_guard
            cases h
            refine Transition.bridge_cancel_cascade
              ?_ ⟨tBridge, h_mem, h_callId, h_policy⟩ (by simp)
              rfl rfl rfl rfl rfl rfl rfl
            cases h_term with
            | inl h_parent => exact Or.inl h_parent
            | inr h_cancelled =>
                exact Or.inr ⟨tBridge, h_mem, h_callId, h_cancelled⟩
          · rw [if_neg h_guard] at h
            exact Option.noConfusion h

end BridgedState
end Subagent
