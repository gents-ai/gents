import Proofs.Callback.Types

namespace CallbackInvocation

/-- A failed invocation may run again while it has attempts left and nothing
it did could repeat: no action observed its effect or wrote results. -/
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
  | fail {pre post : CallbackInvocation} :
      pre.state = .running →
      post = { pre with state := .failed, resultEmitted := false } →
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

end CallbackInvocation
