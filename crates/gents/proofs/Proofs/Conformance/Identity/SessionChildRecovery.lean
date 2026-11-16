import Proofs.Store.Identity.SessionChild
import Proofs.Conformance.Family

namespace Conformance.SessionChildRecovery

open Lean Identity.Household Identity.SessionChild

structure RecoveryInput where
  name : String
  committed : Bool
  parentGranted : Bool
  parentLinked : Bool
  owner : Option String
  creator : Option String
  actor : Option String
  grant : Bool
  mark : Bool
  deriving ToJson

structure RecoveryOutput where
  granted : Bool
  linked : Bool
  deriving ToJson

def recoveryCases : List RecoveryInput :=
  let agent := some "agent"
  let start := beginParentPublication
  [{ name := "complete", committed := start.committed, parentGranted := start.parentGranted
     parentLinked := start.parentLinked, owner := agent, creator := agent, actor := agent
     grant := true, mark := true },
   { name := "grant_failed", committed := start.committed, parentGranted := start.parentGranted
     parentLinked := start.parentLinked, owner := agent, creator := agent, actor := agent
     grant := false, mark := true },
   { name := "mark_failed", committed := start.committed, parentGranted := start.parentGranted
     parentLinked := start.parentLinked, owner := agent, creator := agent, actor := agent
     grant := true, mark := false },
   { name := "wrong_owner", committed := start.committed, parentGranted := start.parentGranted
     parentLinked := start.parentLinked, owner := some "other", creator := agent, actor := agent
     grant := true, mark := true },
   { name := "completed_revoked", committed := true, parentGranted := false, parentLinked := true
     owner := agent, creator := agent, actor := agent, grant := true, mark := true }]

@[conformance_family]
def cases : Family RecoveryInput RecoveryOutput where
  inputs := recoveryCases
  model := fun i =>
    let result := recoverSessionChild ⟨i.committed, i.parentGranted, i.parentLinked⟩ i.owner
      i.creator i.actor i.grant i.mark
    { granted := result.parentGranted, linked := result.parentLinked }

end Conformance.SessionChildRecovery
