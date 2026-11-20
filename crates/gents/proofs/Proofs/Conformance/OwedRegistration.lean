import Proofs.Store.Identity.OwedRegistration
import Proofs.Conformance.Family
import Proofs.Conformance.UserDelegation

namespace Identity.OwedRegistration

deriving instance Lean.ToJson for Recorded, Standing, Handling, EntryStep, Refresh, GrantWait,
  DependentStep

attribute [conformance_function] standing handling servesPass entryStep adopt dependentStep
  observedGrant

end Identity.OwedRegistration

namespace Conformance.OwedRegistration

open Identity.OwedRegistration

structure RefreshInput where
  age : Nat
  deriving Lean.ToJson

/-- Ages around the refresh point and the lifetime. -/
@[conformance_family]
def refresh : Family RefreshInput Bool where
  inputs := [0, 1, 59, 60, 61, 119, 120, 121, 600].map fun age => { age }
  model := fun i => refreshDue i.age

structure ContinuesInput where
  online : Bool
  standings : List Standing
  deriving Lean.ToJson

private def standingLists : List (List Standing) :=
  [[], [.settled], [.failed], [.settled, .failed], [.sending], [.heldBehind],
   [.awaitingAuthority], [.settled, .awaitingAuthority], [.failed, .heldBehind]]

/-- Every short report a pass can leave, online and offline. -/
@[conformance_family]
def proceed : Family ContinuesInput Bool where
  inputs := [true, false].flatMap fun online =>
    standingLists.map fun standings => { online, standings }
  model := fun i => continues i.online i.standings

structure NoInput where
  deriving Lean.ToJson

structure Timing where
  retrySeconds : Nat
  refreshAfterSeconds : Nat
  deriving Lean.ToJson

@[conformance_family]
def timing : Family NoInput Timing where
  inputs := [{}]
  model := fun _ => { retrySeconds, refreshAfterSeconds }

end Conformance.OwedRegistration
