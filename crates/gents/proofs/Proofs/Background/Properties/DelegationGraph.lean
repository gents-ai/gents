import Proofs.Background.State

namespace Subagent
namespace DelegationGraph

structure Graph where
  depthOf : RequestId → Nat
  delegatesTo : RequestId → RequestId → Prop
  cascadeDelegatesTo : RequestId → RequestId → Prop
  h_edge_depth :
    ∀ {parent child : RequestId},
      delegatesTo parent child →
      depthOf child = depthOf parent + 1 ∧ depthOf child ≤ maxSubagentDepth
  h_cascade_subset :
    ∀ {parent child : RequestId},
      cascadeDelegatesTo parent child → delegatesTo parent child

inductive Path (g : Graph) : RequestId → RequestId → Nat → Prop where
  | edge {parent child : RequestId}
      (h_edge : g.delegatesTo parent child) :
      Path g parent child 1
  | step {parent child terminal : RequestId} {length : Nat}
      (h_edge : g.delegatesTo parent child)
      (tail : Path g child terminal length) :
      Path g parent terminal (length + 1)

theorem path_length_positive
    {g : Graph} {parent child : RequestId} {length : Nat}
    (h_path : Path g parent child length) :
    length > 0 := by
  cases h_path <;> omega

theorem path_depth_exact
    {g : Graph} {parent child : RequestId} {length : Nat}
    (h_path : Path g parent child length) :
    g.depthOf child = g.depthOf parent + length ∧
      g.depthOf child ≤ maxSubagentDepth := by
  induction h_path with
  | edge h_edge =>
      exact g.h_edge_depth h_edge
  | step h_edge _ ih =>
      obtain ⟨h_edge_depth, _h_child_bound⟩ := g.h_edge_depth h_edge
      obtain ⟨h_tail_depth, h_tail_bound⟩ := ih
      refine ⟨?_, h_tail_bound⟩
      rw [h_tail_depth, h_edge_depth]
      omega

theorem delegation_path_length_bounded
    {g : Graph} {parent child : RequestId} {length : Nat}
    (h_path : Path g parent child length) :
    length ≤ maxSubagentDepth := by
  obtain ⟨h_depth, h_bound⟩ := path_depth_exact h_path
  omega

theorem delegation_paths_acyclic
    {g : Graph} {request : RequestId} {length : Nat}
    (h_path : Path g request request length) :
    False := by
  obtain ⟨h_depth, _h_bound⟩ := path_depth_exact h_path
  have h_positive := path_length_positive h_path
  omega

theorem no_self_delegation_edge
    {g : Graph} {request : RequestId}
    (h_edge : g.delegatesTo request request) :
    False := by
  obtain ⟨h_depth, _h_bound⟩ := g.h_edge_depth h_edge
  omega

inductive CascadePath (g : Graph) : RequestId → RequestId → Nat → Prop where
  | edge {parent child : RequestId}
      (h_edge : g.cascadeDelegatesTo parent child) :
      CascadePath g parent child 1
  | step {parent child terminal : RequestId} {length : Nat}
      (h_edge : g.cascadeDelegatesTo parent child)
      (tail : CascadePath g child terminal length) :
      CascadePath g parent terminal (length + 1)

theorem cascade_path_to_delegation_path
    {g : Graph} {parent child : RequestId} {length : Nat}
    (h_path : CascadePath g parent child length) :
    Path g parent child length := by
  induction h_path with
  | edge h_edge =>
      exact Path.edge (g.h_cascade_subset h_edge)
  | step h_edge _ ih =>
      exact Path.step (g.h_cascade_subset h_edge) ih

theorem cascade_path_length_bounded
    {g : Graph} {parent child : RequestId} {length : Nat}
    (h_path : CascadePath g parent child length) :
    length ≤ maxSubagentDepth :=
  delegation_path_length_bounded (cascade_path_to_delegation_path h_path)

theorem cascade_paths_acyclic
    {g : Graph} {request : RequestId} {length : Nat}
    (h_path : CascadePath g request request length) :
    False :=
  delegation_paths_acyclic (cascade_path_to_delegation_path h_path)

def EdgeInterruptCovered
    (g : Graph)
    (interrupts : RequestId → RequestId → Prop) : Prop :=
  ∀ {parent child : RequestId},
    g.cascadeDelegatesTo parent child → interrupts parent child

inductive CascadeInterruptTrace
    (g : Graph)
    (interrupts : RequestId → RequestId → Prop) :
    RequestId → RequestId → Nat → Prop where
  | edge {parent child : RequestId}
      (h_edge : g.cascadeDelegatesTo parent child)
      (h_interrupt : interrupts parent child) :
      CascadeInterruptTrace g interrupts parent child 1
  | step {parent child terminal : RequestId} {length : Nat}
      (h_edge : g.cascadeDelegatesTo parent child)
      (h_interrupt : interrupts parent child)
      (tail : CascadeInterruptTrace g interrupts child terminal length) :
      CascadeInterruptTrace g interrupts parent terminal (length + 1)

theorem cascade_cancel_covers_path
    {g : Graph}
    {interrupts : RequestId → RequestId → Prop}
    (h_covered : EdgeInterruptCovered g interrupts)
    {parent child : RequestId} {length : Nat}
    (h_path : CascadePath g parent child length) :
    CascadeInterruptTrace g interrupts parent child length := by
  induction h_path with
  | edge h_edge =>
      exact CascadeInterruptTrace.edge h_edge (h_covered h_edge)
  | step h_edge _ ih =>
      exact CascadeInterruptTrace.step h_edge (h_covered h_edge) ih

end DelegationGraph
end Subagent
