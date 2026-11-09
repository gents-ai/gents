import Lean
import Proofs.Identity.Automation

namespace Identity.PackPlacementReaders

open Identity.Household

/-- A pack's RepositoryPlacement is host configuration read by its callback
workspace effects as the selected Agent (`Automation.callbackPlacementReadable`).
The installing User, who signs the pack installation and publishes the
placement, grants `reader` on each placement the pack publishes to exactly the
Agents that pack provisions (its slot bindings), in the installation
transaction. No other subject is granted. Uninstall revokes those tuples before
the placement is removed. Like every other selection grant, a repeated install
grants only pairs that are new to the installation record, so a revocation of a
recorded pair is not restored. Native tuples carry no provenance: an identical
tuple granted independently by another owner is indistinguishable and is revoked
by uninstall. -/
def pairs (placements agents : List String) : List (String × String) :=
  placements.flatMap fun placement => agents.map fun agent => (placement, agent)

def readerRelation (pair : String × String) : Relation :=
  ⟨"configuration", pair.1, "reader", .actor pair.2⟩

/-- The pairs an installation grants: those of the new record that the prior
record did not hold. -/
def granted (priorPlacements priorAgents placements agents : List String) :
    List (String × String) :=
  (pairs placements agents).filter fun pair => !(pairs priorPlacements priorAgents).contains pair

/-- The pairs an installation revokes: those of the prior record it no longer
holds. Slots are never dropped by an install, so only a placement leaving the
pack revokes here. -/
def revoked (priorPlacements priorAgents placements agents : List String) :
    List (String × String) :=
  (pairs priorPlacements priorAgents).filter fun pair => !(pairs placements agents).contains pair

def install (state : State) (installer : String)
    (priorPlacements priorAgents placements agents : List String) : State :=
  let afterRevoke := (revoked priorPlacements priorAgents placements agents).foldl
    (fun state pair => apply state (.revokeAs installer (readerRelation pair))) state
  (granted priorPlacements priorAgents placements agents).foldl
    (fun state pair => apply state (.grantAs installer (readerRelation pair))) afterRevoke

def uninstall (state : State) (installer : String) (placements agents : List String) : State :=
  (pairs placements agents).foldl
    (fun state pair => apply state (.revokeAs installer (readerRelation pair))) state

theorem granted_subjects_are_provisioned_agents
    (priorPlacements priorAgents placements agents : List String)
    (placement agent : String)
    (member : (placement, agent) ∈ granted priorPlacements priorAgents placements agents) :
    placement ∈ placements ∧ agent ∈ agents := by
  simp [granted, pairs] at member
  exact ⟨member.1.1, member.1.2⟩

/-- Each granted relation is one `reader` actor tuple on one of the pack's
placements naming one of its provisioned Agents. -/
theorem granted_relation_shape
    (priorPlacements priorAgents placements agents : List String)
    (pair : String × String)
    (member : pair ∈ granted priorPlacements priorAgents placements agents) :
    (readerRelation pair).resource = "configuration" ∧
    (readerRelation pair).relation = "reader" ∧
    pair.1 ∈ placements ∧ pair.2 ∈ agents := by
  rcases pair with ⟨placement, agent⟩
  exact ⟨rfl, rfl, granted_subjects_are_provisioned_agents _ _ _ _ _ _ member⟩

theorem fresh_install_grants_every_slot (placements agents : List String) :
    granted [] [] placements agents = pairs placements agents := by
  simp [granted, pairs]

theorem unchanged_reinstall_grants_nothing (placements agents : List String) :
    granted placements agents placements agents = [] := by
  simp [granted]

theorem unchanged_reinstall_revokes_nothing (placements agents : List String) :
    revoked placements agents placements agents = [] := by
  simp [revoked]

/-- The User publishes the placement under its Node; three slot Agents and an
unrelated Agent exist. -/
def fixture : State :=
  { objects := [⟨"nodes", "node", "user"⟩, ⟨"configuration", "placement", "user"⟩],
    relations := [⟨"configuration", "placement", "parent", .object "nodes" "node"⟩],
    enrolled := [] }

def slots : List String := ["writer", "reviewer", "integrator"]

def installed : State := install fixture "user" [] [] ["placement"] slots
def reinstalledAfterRevoke : State :=
  install (apply installed (.revoke (readerRelation ("placement", "reviewer"))))
    "user" ["placement"] slots ["placement"] slots
def removed : State := uninstall installed "user" ["placement"] slots
def foreignInstall : State := install fixture "other" [] [] ["placement"] slots

def readable (state : State) (actor : String) : Bool :=
  Automation.callbackPlacementReadable state actor "placement"

theorem readers_are_exactly_the_provisioned_agents :
    ["writer", "reviewer", "integrator", "other", "runtime"].map (readable installed) =
      [true, true, true, false, false] := by native_decide

theorem uninstall_revokes_every_slot_reader :
    ["writer", "reviewer", "integrator", "other"].map (readable removed) =
      [false, false, false, false] ∧
    readable removed "user" = true := by native_decide

theorem reinstall_does_not_restore_revoked_reader :
    ["writer", "reviewer", "integrator"].map (readable reinstalledAfterRevoke) =
      [true, false, true] := by native_decide

/-- A subject that does not own the placement cannot grant its readers. -/
theorem only_the_placement_owner_grants :
    ["writer", "reviewer", "integrator"].map (readable foreignInstall) =
      [false, false, false] := by native_decide

def casesJson : String :=
  let states : List (String × State) :=
    [("installed", installed), ("reinstalled_after_revoke", reinstalledAfterRevoke),
     ("removed", removed)]
  (Lean.toJson (states.map fun (name, state) => Lean.Json.mkObj
    [("state", Lean.toJson name),
     ("readers", Lean.toJson (["user", "writer", "reviewer", "integrator", "other"].map fun actor =>
        Lean.Json.mkObj [("actor", Lean.toJson actor), ("read", Lean.toJson (readable state actor))]))])).compress

end Identity.PackPlacementReaders
