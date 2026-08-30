import Proofs.ApplyReconcile.ContractCases.Diff

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
def skillA : DocRef := doc .skill "skill-a"
def behaviorA : DocRef := doc .agentBehavior "behavior-a"
def projectionBindingA : DocRef := doc .projectionAcpBinding "projection-binding-a"
def taskA : DocRef := doc .task "task-a"
def scheduleA : DocRef := doc .schedule "schedule-a"
def eventTriggerA : DocRef := doc .eventTrigger "trigger-a"
def principalA : DocRef := doc .agentPrincipal "did:example:agent"

def applyReconcileScenarios : List ApplyReconcileScenario :=
  [ { name := "empty_manifest"
    , manifest := []
    , preDesired := []
    , preLive := []
    , pruneMode := false
    , prefixLen := 0
    }
  , { name := "backend_before_behavior_ordering"
    , manifest :=
        [ desired .agentBehavior "behavior-a" "behavior-desired" [backendA]
        , desired .inferenceBackend "backend-a" "backend-desired"
        ]
    , preDesired := []
    , preLive := []
    , pruneMode := false
    , prefixLen := 0
    }
  , { name := "update_existing_backend"
    , manifest := [desired .inferenceBackend "backend-a" "backend-new"]
    , preDesired := [desired .inferenceBackend "backend-a" "backend-old"]
    , preLive := [live .inferenceBackend "backend-a" "runtime-probe"]
    , pruneMode := false
    , prefixLen := 0
    }
  , { name := "live_only_no_op"
    , manifest := []
    , preDesired := [desired .inferenceBackend "backend-b" "orphan-desired"]
    , preLive := [live .inferenceBackend "backend-b" "orphan-runtime"]
    , pruneMode := false
    , prefixLen := 0
    }
  , { name := "prune_live_only_unreferenced_backend"
    , manifest := []
    , preDesired := [desired .inferenceBackend "backend-b" "orphan-desired"]
    , preLive := [live .inferenceBackend "backend-b" "orphan-runtime"]
    , pruneMode := true
    , prefixLen := 1
    }
  , { name := "prune_blocks_referenced_dependency"
    , manifest := []
    , preDesired :=
        [ desired .agentBehavior "behavior-a" "behavior-live-only" [backendB]
        , desired .inferenceBackend "backend-b" "backend-live-only"
        ]
    , preLive := []
    , pruneMode := true
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
    , pruneMode := false
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
    , pruneMode := false
    , prefixLen := 4
    }
  , { name := "production_write_boundary_all_collections"
    , manifest :=
        [ desired .inferenceBackend "backend-a" "backend-desired"
        , desired .inferenceProfile "profile-a" "profile-desired"
        , desired .toolServiceRegistry "service-a" "service-desired"
        , desired .toolSelection "selection-a" "selection-desired"
        , desired .skill "skill-a" "skill-desired"
        , desired .agentBehavior "behavior-a" "behavior-desired"
            [backendA, selectionA, profileA, serviceA, skillA]
        , desired .projectionAcpBinding "projection-binding-a" "projection-binding-desired"
            [behaviorA]
        , desired .task "task-a" "task-desired" [behaviorA]
        , desired .schedule "schedule-a" "schedule-desired"
        , desired .eventTrigger "trigger-a" "trigger-desired" [taskA]
        , desired .agentPrincipal "did:example:agent" "principal-desired" [behaviorA]
        ]
    , preDesired := []
    , preLive := []
    , pruneMode := false
    , prefixLen := 6
    }
  ]

def applyReconcileCases : List ApplyReconcileCase :=
  applyReconcileScenarios.map buildCase

end ApplyReconcile.ContractCases
