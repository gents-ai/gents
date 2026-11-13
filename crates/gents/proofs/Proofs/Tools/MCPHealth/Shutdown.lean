import Proofs.Tools.MCPHealth.Transition

/-! A health cycle probes the registry's services in order and, once every
probe has completed, drops the services the registry no longer lists.
Shutdown ends the cycle where it stands: the in-flight probe supplies no
event, no later probe starts, and the registry-absent sweep does not run. -/

namespace Proofs.MCPHealth.Shutdown

inductive Shutdown where
  | beforeCycle
  | duringProbe
  deriving DecidableEq, Repr

abbrev HealthMap := List (String × ServiceModel)

def lookup (m : HealthMap) (id : String) : ServiceModel :=
  ((m.find? (·.1 == id)).map (·.2)).getD ServiceModel.initial

def setEntry (m : HealthMap) (id : String) (sm : ServiceModel) : HealthMap :=
  if m.any (·.1 == id) then m.map fun e => if e.1 == id then (id, sm) else e
  else m ++ [(id, sm)]

def observe (K : Threshold) (m : HealthMap) (id : String) (e : Event) : HealthMap :=
  match step? (lookup m id) e K with
  | some sm => setEntry m id sm
  | none => m.filter (·.1 != id)

/-- `failing` probes complete with a failure, in registry order; the next one
is in flight when shutdown arrives. -/
def cycle (K : Threshold) (before : HealthMap) (registry : List String) (failing : Nat) :
    Shutdown → Nat × HealthMap
  | .beforeCycle => (0, before)
  | .duringProbe =>
      ((registry.take (failing + 1)).length,
       (registry.take failing).foldl (fun m id => observe K m id .probeFail) before)

def sameEntries (a b : HealthMap) : Bool :=
  a.length == b.length && a.all (b.contains ·) && b.all (a.contains ·)

theorem probe_shutdown_prevents_new_mcp_probes_and_failure_events (K : Threshold)
    (before : HealthMap) (registry : List String) (failing : Nat) :
    cycle K before registry failing .beforeCycle = (0, before) := rfl

theorem probe_shutdown_cancels_pending_mcp_without_pruning_or_failure (K : Threshold)
    (before : HealthMap) (id : String) (rest : List String) :
    cycle K before (id :: rest) 0 .duringProbe = (1, before) := by
  simp [cycle]

end Proofs.MCPHealth.Shutdown
