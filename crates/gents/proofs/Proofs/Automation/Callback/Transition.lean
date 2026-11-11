import Proofs.Callback.Types

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

end CallbackInvocation
