import Proofs.ApplyReconcile.ContractCases.Diff

/-! Concrete apply/reconcile scenarios grouped as executable contract fixtures. -/

namespace ApplyReconcile.ContractCases

open Conformance.Contracts

def doc (collection : Collection) (id : String) : DocRef :=
  { collection := collection, id := id }

def desired
    (collection : Collection)
    (id content : String)
    (refs : List DocRef := []) : ContractDoc :=
  { ref := doc collection id, content := content, refs := refs }

def live
    (collection : Collection)
    (id content : String) : ContractLiveDoc :=
  { ref := doc collection id, content := content }

def backendA : DocRef := doc .inferenceBackend "backend-a"
def backendB : DocRef := doc .inferenceBackend "backend-b"
def selectionA : DocRef := doc .toolSelection "selection-a"
def profileA : DocRef := doc .inferenceProfile "profile-a"
def serviceA : DocRef := doc .toolServiceRegistry "service-a"
def behaviorA : DocRef := doc .agentBehavior "behavior-a"
def taskA : DocRef := doc .task "task-a"
def scheduleA : DocRef := doc .schedule "schedule-a"
def eventTriggerA : DocRef := doc .eventTrigger "trigger-a"
def principalA : DocRef := doc .agentPrincipal "did:example:agent"

def applyReconcileScenarios : List ApplyReconcileScenario :=
  [ { name := "empty_manifest"
    , manifest := []
    , preDesired := []
    , preLive := []
    , prefixLen := 0
    }
  , { name := "backend_before_behavior_ordering"
    , manifest :=
        [ desired .agentBehavior "behavior-a" "behavior-desired" [backendA]
        , desired .inferenceBackend "backend-a" "backend-desired"
        ]
    , preDesired := []
    , preLive := []
    , prefixLen := 0
    }
  , { name := "update_existing_backend"
    , manifest := [desired .inferenceBackend "backend-a" "backend-new"]
    , preDesired := [desired .inferenceBackend "backend-a" "backend-old"]
    , preLive := [live .inferenceBackend "backend-a" "runtime-probe"]
    , prefixLen := 0
    }
  , { name := "live_only_no_op"
    , manifest := []
    , preDesired := [desired .inferenceBackend "backend-b" "orphan-desired"]
    , preLive := [live .inferenceBackend "backend-b" "orphan-runtime"]
    , prefixLen := 0
    }
  , { name := "prefix_retry_convergence_idempotence"
    , manifest :=
        [ desired .task "task-a" "task-desired" [behaviorA]
        , desired .agentBehavior "behavior-a" "behavior-desired" [backendA]
        , desired .inferenceBackend "backend-a" "backend-desired"
        ]
    , preDesired := []
    , preLive := [live .agentBehavior "behavior-a" "runtime-live"]
    , prefixLen := 1
    }
  , { name := "referrer_closure"
    , manifest :=
        [ desired .agentPrincipal "did:example:agent" "principal-desired" [behaviorA]
        , desired .task "task-a" "task-desired" [behaviorA]
        , desired .agentBehavior "behavior-a" "behavior-desired"
            [backendA, selectionA, profileA]
        , desired .toolSelection "selection-a" "selection-desired"
        , desired .inferenceProfile "profile-a" "profile-desired"
        , desired .inferenceBackend "backend-a" "backend-desired"
        ]
    , preDesired := []
    , preLive := []
    , prefixLen := 4
    }
  , { name := "production_write_boundary_all_collections"
    , manifest :=
        [ desired .inferenceBackend "backend-a" "backend-desired"
        , desired .inferenceProfile "profile-a" "profile-desired"
        , desired .toolServiceRegistry "service-a" "service-desired"
        , desired .toolSelection "selection-a" "selection-desired"
        , desired .agentBehavior "behavior-a" "behavior-desired"
            [backendA, selectionA, profileA, serviceA]
        , desired .task "task-a" "task-desired" [behaviorA]
        , desired .schedule "schedule-a" "schedule-desired"
        , desired .eventTrigger "trigger-a" "trigger-desired" [taskA]
        , desired .agentPrincipal "did:example:agent" "principal-desired" [behaviorA]
        ]
    , preDesired := []
    , preLive := []
    , prefixLen := 6
    }
  ]

def applyReconcileCases : List ApplyReconcileCase :=
  applyReconcileScenarios.map buildCase

end ApplyReconcile.ContractCases
