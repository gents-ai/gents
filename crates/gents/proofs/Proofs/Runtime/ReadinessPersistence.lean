import Lean

namespace Runtime.ReadinessPersistence
open Lean

structure Update where
  process : String
  writes : List String
  cancelled : Bool := false
  deriving ToJson

structure Input where
  updates : List Update
  deriving ToJson

structure Observation where
  accepted : Bool
  storageFull : Bool
  process : String
  sourceGeneration : Nat
  demotions : List String
  persisted : List String
  deriving ToJson

/-- The ordered writer bounds a command to five attempts. Full storage is fatal
on its first observation. Only a successful write publishes the candidate;
a failed command leaves the admission observation and committed state intact.
A command stopped by full storage reports that cause to its caller. -/
def succeeds : Nat → List String → Bool
  | 0, _ => false
  | _, [] => false
  | fuel + 1, result :: rest =>
    if result == "ok" then true else if result == "full" then false
    else succeeds fuel rest

def fullStorage : Nat → List String → Bool
  | 0, _ => false
  | _, [] => false
  | fuel + 1, result :: rest =>
    if result == "ok" then false else if result == "full" then true
    else fullStorage fuel rest

def run (input : Input) : List Observation := Id.run do
  let mut process := "uninitialized"
  let mut persisted := []
  let mut result := []
  for update in input.updates do
    let accepted := !update.cancelled && (process == update.process || succeeds 5 update.writes)
    if accepted then
      if process != update.process then persisted := persisted ++ [update.process]
      process := update.process
    let storageFull := !accepted && !update.cancelled && fullStorage 5 update.writes
    result := result ++ [⟨accepted, storageFull, process, 0, [], persisted⟩]
  return result

end Runtime.ReadinessPersistence
