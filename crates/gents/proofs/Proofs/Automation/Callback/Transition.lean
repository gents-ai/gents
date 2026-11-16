import Proofs.Automation.Callback.Types

namespace CallbackInvocation

def interruptJournal (journal : List ActionJournalEntry) : List ActionJournalEntry :=
  journal.map fun e => { e with state := ActionJournalState.markInterrupted e.state }

/-- A failed invocation may run again only while it has attempts left and
nothing it did could repeat. -/
def retryAllowed (inv : CallbackInvocation) (maxAttempts : Nat) : Bool :=
  decide (inv.state = .failed) && decide (inv.attempts < maxAttempts) &&
    inv.journal.all (fun e => !ActionJournalState.effectful e.state)

inductive Transition : CallbackInvocation → CallbackInvocation → Prop where
  | claim {pre post : CallbackInvocation} :
      pre.state = .pending →
      post = { pre with state := .claimed, attempts := pre.attempts + 1 } →
      Transition pre post
  | run {pre post : CallbackInvocation} :
      pre.state = .claimed →
      post = { pre with state := .running } →
      Transition pre post
  | succeed {pre post : CallbackInvocation} :
      pre.state = .running →
      pre.journal.all (fun e => decide (e.state = .resultDocsWritten)) = true →
      post = { pre with state := .succeeded, resultEmitted := true } →
      Transition pre post
  /-- The attempt observed its own failure; recovery never takes this step. -/
  | fail {pre post : CallbackInvocation} :
      pre.state = .running →
      post = { pre with state := .failed, resultEmitted := false } →
      Transition pre post
  | interrupt {pre post : CallbackInvocation} :
      pre.state = .running →
      post = { pre with
        state := .failed, journal := interruptJournal pre.journal, resultEmitted := false } →
      Transition pre post
  | deny_claimed {pre post : CallbackInvocation} :
      pre.state = .claimed →
      pre.journal = [] →
      post = { pre with state := .denied, resultEmitted := false } →
      Transition pre post
  | deny_running {pre post : CallbackInvocation} :
      pre.state = .running →
      pre.journal = [] →
      post = { pre with state := .denied, resultEmitted := false } →
      Transition pre post
  | retry {pre post : CallbackInvocation} (maxAttempts : Nat) :
      retryAllowed pre maxAttempts = true →
      post = { pre with state := .pending, journal := [], resultEmitted := false } →
      Transition pre post

/-- Recovery of a running invocation with a non-empty journal fails it with
executing actions marked interrupted, never with a plain `fail`; a denial after
actions started does the same. -/
def recover (inv : CallbackInvocation) : CallbackInvocation :=
  if inv.state = .running ∧ inv.journal ≠ [] then
    { inv with state := .failed, journal := interruptJournal inv.journal, resultEmitted := false }
  else inv

def deny (inv : CallbackInvocation) : CallbackInvocation :=
  if inv.journal = [] then { inv with state := .denied, resultEmitted := false }
  else recover inv

/-- The executor's lifecycle steps; `step?` is `none` where the step is not
legal from the invocation's state. -/
inductive Step where
  | claim
  | run
  | succeed
  | fail
  | interrupt
  | denyClaimed
  | denyRunning
  | retry
  deriving DecidableEq, Repr

def step? (maxAttempts : Nat) (pre : CallbackInvocation) : Step → Option CallbackInvocation
  | .claim =>
      if pre.state = .pending then some { pre with state := .claimed, attempts := pre.attempts + 1 }
      else none
  | .run => if pre.state = .claimed then some { pre with state := .running } else none
  | .succeed =>
      if pre.state = .running ∧ pre.journal.all (fun e => decide (e.state = .resultDocsWritten)) = true
      then some { pre with state := .succeeded, resultEmitted := true }
      else none
  | .fail =>
      if pre.state = .running then some { pre with state := .failed, resultEmitted := false }
      else none
  | .interrupt =>
      if pre.state = .running then
        some { pre with
          state := .failed, journal := interruptJournal pre.journal, resultEmitted := false }
      else none
  | .denyClaimed =>
      if pre.state = .claimed ∧ pre.journal = [] then
        some { pre with state := .denied, resultEmitted := false }
      else none
  | .denyRunning =>
      if pre.state = .running ∧ pre.journal = [] then
        some { pre with state := .denied, resultEmitted := false }
      else none
  | .retry =>
      if retryAllowed pre maxAttempts = true then
        some { pre with state := .pending, journal := [], resultEmitted := false }
      else none

theorem step?_sound (maxAttempts : Nat) (pre post : CallbackInvocation) (s : Step)
    (h : step? maxAttempts pre s = some post) : Transition pre post := by
  cases s <;> simp only [step?] at h <;> split at h <;> (try contradiction)
  · next hs => exact .claim hs (Option.some.inj h).symm
  · next hs => exact .run hs (Option.some.inj h).symm
  · next hs => exact .succeed hs.1 hs.2 (Option.some.inj h).symm
  · next hs => exact .fail hs (Option.some.inj h).symm
  · next hs => exact .interrupt hs (Option.some.inj h).symm
  · next hs => exact .deny_claimed hs.1 hs.2 (Option.some.inj h).symm
  · next hs => exact .deny_running hs.1 hs.2 (Option.some.inj h).symm
  · next hs => exact .retry maxAttempts hs (Option.some.inj h).symm

/-- The executor journals the next planned action as executing before the host
runs it. -/
def beginAction (inv : CallbackInvocation) : CallbackInvocation :=
  { inv with journal := inv.journal ++ [{ index := inv.journal.length, state := .executing }] }

/-- The executor records the result documents of the actions it was executing. -/
def writeResults (inv : CallbackInvocation) : CallbackInvocation :=
  { inv with journal := inv.journal.map fun e =>
      if e.state = .executing then { e with state := .resultDocsWritten } else e }

end CallbackInvocation
