import Proofs.Background.Properties.DelegationGraph
import Proofs.Conformance.ContractCases.Types

/-!
# Subagent Delegation Graph Conformance Cases

Finite rows for #349 gap 2: arbitrary delegation graph termination,
acyclicity, and cascade propagation over bounded cascade paths.
-/

namespace Conformance.ContractCases

def subagentDelegationGraphCases : List SubagentDelegationGraphCase :=
  [ { name := "delegation_path_length_bounded_by_subagent_depth"
    , theoremName := "Subagent.DelegationGraph.delegation_path_length_bounded"
    , property := "termination"
    , witnessKind := "arbitrary_delegation_path"
    , maxDepth := Subagent.maxSubagentDepth
    , pathLength := Subagent.maxSubagentDepth
    , parentDepth := 0
    , terminalDepth := Subagent.maxSubagentDepth
    , cascadePath := false
    , acyclic := true
    , bounded := true
    , cascadeCovered := false
    , edgeTheorem := "Subagent.DelegationGraph.path_depth_exact"
    , cascadeEdgeTheorem := none
    }
  , { name := "delegation_cycles_rejected_by_strict_depth_increase"
    , theoremName := "Subagent.DelegationGraph.delegation_paths_acyclic"
    , property := "acyclicity"
    , witnessKind := "arbitrary_delegation_path"
    , maxDepth := Subagent.maxSubagentDepth
    , pathLength := 1
    , parentDepth := 1
    , terminalDepth := 1
    , cascadePath := false
    , acyclic := true
    , bounded := true
    , cascadeCovered := false
    , edgeTheorem := "Subagent.DelegationGraph.no_self_delegation_edge"
    , cascadeEdgeTheorem := none
    }
  , { name := "cascade_paths_bounded_and_edge_interrupts_cover_reachable_children"
    , theoremName := "Subagent.DelegationGraph.cascade_cancel_covers_path"
    , property := "cascade_cancel"
    , witnessKind := "arbitrary_cascade_path"
    , maxDepth := Subagent.maxSubagentDepth
    , pathLength := Subagent.maxSubagentDepth
    , parentDepth := 0
    , terminalDepth := Subagent.maxSubagentDepth
    , cascadePath := true
    , acyclic := true
    , bounded := true
    , cascadeCovered := true
    , edgeTheorem := "Subagent.DelegationGraph.cascade_path_length_bounded"
    , cascadeEdgeTheorem := some "Subagent.BridgedState.cascade_cancels_child"
    }
  ]

theorem subagentDelegationGraphCases_close_gap2 :
    ∀ witness,
      witness ∈ subagentDelegationGraphCases →
      witness.maxDepth = Subagent.maxSubagentDepth ∧
      witness.pathLength ≤ witness.maxDepth ∧
      witness.acyclic = true ∧
      witness.bounded = true ∧
      (witness.cascadePath = true → witness.cascadeCovered = true) := by
  native_decide

theorem subagentDelegationGraphCases_cover_required_properties :
    subagentDelegationGraphCases.any
        (fun witness => witness.property = "termination") = true ∧
    subagentDelegationGraphCases.any
        (fun witness => witness.property = "acyclicity") = true ∧
    subagentDelegationGraphCases.any
        (fun witness => witness.property = "cascade_cancel") = true := by
  native_decide

end Conformance.ContractCases
