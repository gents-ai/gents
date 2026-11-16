import Mathlib.Data.Nat.Basic

namespace ToolPolicy.PluginApproval

inductive Answer where
  | once | path | folder | deny
  deriving DecidableEq, Repr

structure Question where
  id : String
  created : Nat
  expires : Nat
  directory : Bool
  write : Bool
  deriving DecidableEq, Repr

structure State where
  question : Option Question
  decision : Option Bool
  pathGrant : Option Bool
  folderGrant : Option Bool
  deriving DecidableEq, Repr

/-- Approval files and cancellation share one native exclusive lock. The
question IDs are fresh UUIDs for each waiting call, including retries. The
wall-clock expiry is published by the waiting call before it exposes the
question; answering assumes the host clock has not moved backwards. The
waiter separately owns its monotonic timeout and removes the question on
cancellation. No expired, mismatched, removed or already answered question
authorizes a persistent grant. -/
def admits (s : State) (id : String) (now : Nat) : Bool :=
  s.decision.isNone && s.question.any fun q =>
    !id.isEmpty && q.id == id && q.created ≤ now && now < q.expires

def widen (prior : Option Bool) (write : Bool) : Option Bool :=
  some (prior.getD false || write)

/-- These are successful filesystem effects after admission, not a claim of
atomicity across the grant and answer files. A persistent grant is the
operator's independent permission: it can survive a subsequent answer-write
failure or cancellation. Native I/O failures propagate to the answering
surface and must never be reported as a delivered answer. -/
def answer (s : State) (id : String) (now : Nat) (choice : Answer) : State × Bool :=
  if admits s id now then
    match s.question with
    | none => (s, false)
    | some q =>
      let granted := match choice with
        | .path => if q.directory then
            { s with folderGrant := widen s.folderGrant q.write }
          else { s with pathGrant := widen s.pathGrant q.write }
        | .folder => { s with folderGrant := widen s.folderGrant q.write }
        | .once | .deny => s
      ({ granted with decision := some (choice != .deny) }, true)
  else (s, false)

def cancel (s : State) : State := { s with question := none, decision := none }

theorem rejected_has_no_effect (s : State) (id : String) (now : Nat) (choice : Answer)
    (h : admits s id now = false) : answer s id now choice = (s, false) := by
  simp [answer, h]

theorem answered_cannot_be_replaced (s : State) (prior : Bool) (id : String)
    (now : Nat) (choice : Answer) :
    answer { s with decision := some prior } id now choice =
      ({ s with decision := some prior }, false) := by
  simp [answer, admits]

theorem expiry_prevents_admission (s : State) (q : Question) (id : String) (now : Nat)
    (h : s.question = some q) (expired : q.expires ≤ now) : admits s id now = false := by
  simp [admits, h, Nat.not_lt.mpr expired]

theorem cancellation_prevents_late_grants (s : State) (id : String) (now : Nat)
    (choice : Answer) : answer (cancel s) id now choice = (cancel s, false) := by
  simp [answer, admits, cancel]

theorem cancellation_keeps_grants (s : State) :
    (cancel s).pathGrant = s.pathGrant ∧ (cancel s).folderGrant = s.folderGrant := ⟨rfl, rfl⟩

theorem wider_grant_is_preserved (write : Bool) : widen (some true) write = some true := by
  simp [widen]

end ToolPolicy.PluginApproval
