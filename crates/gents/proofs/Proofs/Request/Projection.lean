namespace Request.Projection

/-- Admission is concerned with the principal/behavior binding, not with how
    many replicated projection documents currently carry that binding. -/
def behaviorCompatible (requested : String) (persisted : List String) : Bool :=
  persisted.all (· == requested)

theorem duplicate_equal_bindings_are_compatible (requested : String) :
    behaviorCompatible requested [requested, requested] = true := by
  simp [behaviorCompatible]

theorem conflicting_binding_is_rejected (requested existing : String)
    (h : existing ≠ requested) :
    behaviorCompatible requested [requested, existing] = false := by
  simp [behaviorCompatible, h]

/-- Projection writes converge every replicated row's value. A missing
    projection is repaired by creating exactly one row; duplicates are updated
    in place rather than becoming an admission failure. -/
def projectAll {α : Type} (value : α) (rows : List α) : List α :=
  if rows.isEmpty then [value] else rows.map fun _ => value

theorem projection_is_present_after_write {α : Type} (value : α) (rows : List α) :
    projectAll value rows ≠ [] := by
  cases rows <;> simp [projectAll]

theorem duplicate_projection_values_converge {α : Type} (oldA oldB value : α) :
    projectAll value [oldA, oldB] = [value, value] := by
  simp [projectAll]

/-- AgentConversation is a repairable read projection. Its cardinality cannot
    veto the authoritative AgentRequest terminal transition. -/
def terminalRequestCommits (_conversationRows : Nat) : Bool := true

theorem terminal_request_independent_of_projection_cardinality (rows : Nat) :
    terminalRequestCommits rows = true := by
  rfl

/-- A projection statement error discards the atomic attempt and selects a
    fresh request-only transaction; it never turns projection failure into a
    veto over the authoritative terminal edge. -/
def terminalCommitAfterProjectionAttempt (_projectionSucceeded : Bool) : Bool := true

theorem terminal_request_commits_after_projection_error :
    terminalCommitAfterProjectionAttempt false = true := by
  rfl

/-- An already-terminal request still drives its repairable projection, so an
    idempotent retry converges the conversation instead of becoming a no-op. -/
def projectionRunsAfterRequestAttempt (_requestChanged : Bool) : Bool := true

theorem idempotent_terminal_retry_repairs_projection :
    projectionRunsAfterRequestAttempt false = true := by
  rfl

end Request.Projection
