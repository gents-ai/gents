import Proofs.Basic

namespace InferenceCall.LimitObservation

inductive Trigger where
  | cached | open | refresh
  deriving DecidableEq, Repr

/-- Limits are observations only. A cached read never contacts a provider;
explicit refresh bypasses the five-minute automatic-read throttle. -/
def shouldRead (trigger : Trigger) (now : Int) (lastRead : Option Int) : Bool :=
  match trigger with
  | .cached => false
  | .refresh => true
  | .open => lastRead.all (fun previous => now - previous ≥ 300)

/-- Bindings are physical credential rows or the existing keyed backend
connection identity. A replacement cannot inherit an in-flight observation. -/
def sameBinding (observed current : String) : Bool := observed == current

theorem cached_never_contacts_provider (now : Int) (lastRead : Option Int) :
    shouldRead .cached now lastRead = false := rfl

theorem explicit_refresh_bypasses_throttle (now : Int) (lastRead : Option Int) :
    shouldRead .refresh now lastRead = true := rfl

end InferenceCall.LimitObservation
