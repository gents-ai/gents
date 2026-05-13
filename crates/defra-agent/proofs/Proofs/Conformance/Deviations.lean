import Proofs.Conformance.Boundaries

/-!
# Conformance Deviations

This file is reserved for real unresolved mismatches between the Lean product
specification and the Rust/DefraDB implementation.

There are currently no known active spec deviations.

Closed historical issues, intentional product policies, reserved vocabulary,
and external storage/operational assumptions are documented in
`Proofs.Conformance.Boundaries` instead of being listed as deviations.
-/

namespace Conformance.Contracts

structure Deviation where
  id : String
  domain : String
  subject : String
  statement : String
  acceptedFailureMode : Option String := none
  acceptedFollowUp : Option String := none
  deriving Repr

def deviations : List Deviation :=
  [ { id := "event_source_lacks_periodic_rescan"
    , domain := "event_delivery"
    , subject := "EventSource"
    , statement :=
        "EventSource has no periodic introspection rescan in the live process. " ++
        "EventDelivery.D1 closes vacuously for this instance (rescanBoundedBy = 0). " ++
        "Adding a periodic rescan flips the binding to substantive D1."
    , acceptedFailureMode := some "missed_event_observation"
    , acceptedFollowUp :=
        some "Track at #187 PR description; deadline-audit followup #8."
    }
  , { id := "subagent_source_lacks_live_rescan"
    , domain := "event_delivery"
    , subject := "SubagentSource"
    , statement :=
        "SubagentSource has recover_orphan_subagent_children only at startup, " ++
        "not as a periodic loop in the live process. EventDelivery.D1 closes " ++
        "vacuously for this instance (rescanBoundedBy = 0). Lifting the existing " ++
        "recovery primitive to a periodic timer makes D1 substantive."
    , acceptedFailureMode := some "missed_subagent_spawn_observation_in_live_process"
    , acceptedFollowUp :=
        some "Track at #187 PR description; deadline-audit followup #5."
    }
  ]

def Deviation.toJson (deviation : Deviation) : String :=
  "{"
    ++ "\"id\":" ++ jsonString deviation.id ++ ","
    ++ "\"domain\":" ++ jsonString deviation.domain ++ ","
    ++ "\"subject\":" ++ jsonString deviation.subject ++ ","
    ++ "\"statement\":" ++ jsonString deviation.statement ++ ","
    ++ "\"accepted_failure_mode\":"
      ++ jsonOptionalString deviation.acceptedFailureMode ++ ","
    ++ "\"accepted_follow_up\":"
      ++ jsonOptionalString deviation.acceptedFollowUp
    ++ "}"

def deviationsJson : String :=
  jsonArray (deviations.map Deviation.toJson)

end Conformance.Contracts
