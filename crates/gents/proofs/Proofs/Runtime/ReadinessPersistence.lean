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
  process : String
  sourceGeneration : Nat
  demotions : List String
  persisted : List String
  deriving ToJson

/-- The ordered writer bounds a command to five attempts. Full storage is fatal
on its first observation. Only a successful write publishes the candidate;
a failed command leaves the admission observation and committed state intact. -/
def succeeds : Nat → List String → Bool
  | 0, _ => false
  | _, [] => false
  | fuel + 1, result :: rest =>
    if result == "ok" then true else if result == "full" then false
    else succeeds fuel rest

def run (input : Input) : List Observation := Id.run do
  let mut process := "uninitialized"
  let mut persisted := []
  let mut result := []
  for update in input.updates do
    let accepted := !update.cancelled && (process == update.process || succeeds 5 update.writes)
    if accepted then
      if process != update.process then persisted := persisted ++ [update.process]
      process := update.process
    result := result ++ [⟨accepted, process, 0, [], persisted⟩]
  return result

inductive StartupCommand where
  | source
  | ready
  deriving DecidableEq, Repr, ToJson

/-- The write results a startup run meets: those of its agent readiness source,
published while Recovering, then those of its Ready command. -/
structure StartupRun where
  sourceWrites : List String
  readyWrites : List String
  deriving ToJson

def attemptsUsed : Nat → List String → Nat
  | 0, _ => 0
  | _, [] => 0
  | fuel + 1, result :: rest =>
    if result == "ok" || result == "full" then 1 else 1 + attemptsUsed fuel rest

/-- Startup publishes its source, then Ready once the barrier releases. The
first command that exhausts its bounded attempts ends the run with that
command's own error, and no later command is issued. -/
def startupFailure (run : StartupRun) : Option StartupCommand :=
  if !succeeds 5 run.sourceWrites then some .source
  else if !succeeds 5 run.readyWrites then some .ready
  else none

def startupAttempts (run : StartupRun) : Nat :=
  match startupFailure run with
  | some .source => attemptsUsed 5 run.sourceWrites
  | some .ready => attemptsUsed 5 run.readyWrites
  | none => 0

/-- A failed command leaves committed readiness intact, so the publisher still
flushes terminal Shutdown after it. -/
def startupUpdates (run : StartupRun) : List Update :=
  let shutdown : Update := ⟨"shutdown", ["ok"], false⟩
  match startupFailure run with
  | some .source => [⟨"recovering", run.sourceWrites, false⟩, shutdown]
  | _ => [⟨"recovering", run.sourceWrites, false⟩, ⟨"ready", run.readyWrites, false⟩, shutdown]

def startupPersisted (run : StartupRun) : List String :=
  ((Runtime.ReadinessPersistence.run ⟨startupUpdates run⟩).getLast?.map (·.persisted)).getD []

def startupReadyObserved (run : StartupRun) : Bool :=
  (Runtime.ReadinessPersistence.run ⟨startupUpdates run⟩).any
    fun observation => observation.process == "ready"

theorem startup_source_persistence_exhaustion_fails_closed_and_is_the_exact_run_error :
    let run : StartupRun := ⟨List.replicate 5 "fail", ["ok"]⟩
    startupFailure run = some .source ∧ startupAttempts run = 5 ∧
      "ready" ∉ startupPersisted run ∧ "shutdown" ∈ startupPersisted run ∧
      startupReadyObserved run = false := by native_decide

theorem ready_persistence_exhaustion_is_fatal_and_never_opens_runtime_readiness :
    let run : StartupRun := ⟨["ok"], List.replicate 5 "fail"⟩
    startupFailure run = some .ready ∧ startupAttempts run = 5 ∧
      "ready" ∉ startupPersisted run ∧ "shutdown" ∈ startupPersisted run ∧
      startupReadyObserved run = false := by native_decide

end Runtime.ReadinessPersistence
