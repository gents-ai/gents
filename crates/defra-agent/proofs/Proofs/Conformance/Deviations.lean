import Proofs.Conformance.Boundaries

/-!
# Conformance Deviations

This file is reserved for real unresolved mismatches between the Lean product
specification and the Rust/DefraDB implementation.

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
  , { id := "defradb_rs_p2p_subscription_state_not_durable"
    , domain := "reverse_pairing"
    , subject := "DefraDB P2P subscription persistence"
    , statement :=
        "Reverse-pairing convergence assumes receiver-side P2P gossipsub " ++
        "subscription state survives process restart. Go DefraDB persists that " ++
        "state, but defradb.rs currently keeps iroh/libp2p subscription " ++
        "registration in memory unless a higher-level path re-installs it."
    , acceptedFailureMode :=
        some "receiver_restart_drops_subscription_delivery_contract"
    , acceptedFollowUp :=
        some "Track upstream at sourcenetwork/defradb.rs#957; downstream at sourcenetwork/defra-agent#166."
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
