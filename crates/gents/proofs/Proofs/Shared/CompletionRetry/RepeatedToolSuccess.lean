import Proofs.Shared.CompletionRetry.ErrorIdentity

namespace CompletionRetry.RepeatedToolSuccess

/-- An execution-local stop after eight consecutive successful calls with the
same tool, argument text and stable result. The eighth result is persisted and
threaded before the existing stream-error owner ends execution; there is no
ninth dispatch or warning inserted into a tool's structured result. Command
output uses its owner's volatile-field premise from `ErrorIdentity`; other
output text remains significant. A failed, cancelled, skipped or unidentifiable
result resets this streak. It does not change the independent failure guard. -/
def limit : Nat := 8

structure Event where
  tool : Nat
  args : Nat
  result : Option ErrorIdentity.ErrorText
  commandEnvelope : Bool
  deriving Repr, DecidableEq

structure State where
  identity : Option (Nat × Nat × Nat) := none
  count : Nat := 0
  deriving Repr, DecidableEq

def stopped (s : State) : Bool := limit ≤ s.count

def record (s : State) (e : Event) : State :=
  if stopped s then s
  else match e.result with
    | none => {}
    | some result =>
        let identity := (e.tool, e.args, ErrorIdentity.errorIdentity e.commandEnvelope result)
        if s.identity == some identity then { s with count := s.count + 1 }
        else ⟨some identity, 1⟩

def run (s : State) : List Event → List Nat × Bool
  | [] => ([], stopped s)
  | e :: rest =>
      if stopped s then ([], true)
      else
        let next := record s e
        let (counts, ending) := run next rest
        (next.count :: counts, ending)

theorem record_bounded (s : State) (e : Event) (h : s.count ≤ limit) :
    (record s e).count ≤ limit := by
  unfold record
  split
  · exact h
  · rename_i active
    cases hr : e.result with
    | none => simp [hr, limit]
    | some result =>
        simp only [hr]
        split
        · have below : s.count < limit := by simpa [stopped] using active
          exact Nat.succ_le_of_lt below
        · simp [limit]

def same : Event := ⟨0, 0, some ⟨0, 0⟩, true⟩

theorem seven_identical_successes_continue :
    run {} (List.replicate 7 same) = ([1, 2, 3, 4, 5, 6, 7], false) := by decide

theorem eight_identical_successes_stop (rest : List Event) :
    run {} (List.replicate 8 same ++ rest) = ([1, 2, 3, 4, 5, 6, 7, 8], true) := by
  cases rest <;> simp [run, record, stopped, limit, same]

theorem a_non_success_resets (s : State) (e : Event) (h : stopped s = false)
    (missing : e.result = none) : record s e = {} := by
  simp [record, h, missing]

end CompletionRetry.RepeatedToolSuccess
