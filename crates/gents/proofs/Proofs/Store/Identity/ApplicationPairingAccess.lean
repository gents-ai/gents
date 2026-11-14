import Proofs.Store.Identity.Household

namespace Identity.ApplicationPairingAccess

open Identity.Household

/-- A Node owned by `user`, with the Engineer `engineer` as a Node
administrator, the node key `runtime` as its runtime reader, and an
`agent_manager` `manager`. The Agent `author` created the pairing overlay
`overlay` and linked it to the Node; `sibling` is another Agent of the same
Node with no relation to it. -/
def fixture : State :=
  { objects := [⟨"nodes", "node", "user"⟩, ⟨"application_pairings", "overlay", "author"⟩],
    relations := [⟨"nodes", "node", "administrator", .actor "engineer"⟩,
      ⟨"nodes", "node", "runtime_reader", .actor "runtime"⟩,
      ⟨"nodes", "node", "agent_manager", .actor "manager"⟩,
      ⟨"application_pairings", "overlay", "parent", .object "nodes" "node"⟩],
    enrolled := [] }

def access (actor : String) : Bool × Bool × Bool :=
  (allowed 8 fixture actor "application_pairings" "overlay" "read",
   allowed 8 fixture actor "application_pairings" "overlay" "update",
   allowed 8 fixture actor "application_pairings" "overlay" "delete")

/-- The authoring Agent reads, rewrites and deletes its own pairing. -/
theorem author_controls_its_pairing : access "author" = (true, true, true) := by
  native_decide

/-- Only the author rewrites a pairing: the Node owner, the Engineer, the
runtime, a catalog holder and a sibling Agent cannot re-bind or widen it. -/
theorem only_the_author_updates :
    ["user", "engineer", "runtime", "manager", "sibling"].map (fun actor => (access actor).2.1) =
      [false, false, false, false, false] := by
  native_decide

/-- Node administration reads every pairing and stops any of them: whoever can
grant or withdraw an Agent's P2P mutation can also delete its pairing. -/
theorem node_administration_reads_and_deletes :
    ["user", "engineer"].map access = [(true, false, true), (true, false, true)] := by
  native_decide

/-- The reconciler, acting as the runtime reader, reads pairings and changes
none. -/
theorem runtime_reads_only : access "runtime" = (true, false, false) := by
  native_decide

/-- A sibling Agent and an `agent_manager` neither see nor change another
Agent's pairing, even when they hold the same P2P mutation grant: that grant is
node-wide `delete-document`, and document ACP confines it to the owner. -/
theorem siblings_and_catalog_holders_have_no_access :
    ["sibling", "manager", "stranger"].map access =
      [(false, false, false), (false, false, false), (false, false, false)] := by
  native_decide

/-- Only the author links its pairing to a Node: `parent` has no manager
relation, so neither the Node's administration nor a sibling can re-parent it. -/
theorem only_the_author_relates_its_pairing :
    ["user", "engineer", "sibling"].map (fun actor =>
      mayManage fixture actor ⟨"application_pairings", "overlay", "parent", .object "nodes" "other"⟩) =
      [false, false, false] ∧
    mayManage fixture "author" ⟨"application_pairings", "overlay", "parent", .object "nodes" "node"⟩ =
      true := by
  native_decide

/-- No resource names `application_pairings` as a relation type and none
inherits a permission from it, so holding a pairing is never a channel to
another document. -/
theorem application_pairings_reach_nothing :
    policy.all (fun resource => resource.relations.all (fun relation =>
      relation.types.all (fun type =>
        !(type == "application_pairings" || type.startsWith "application_pairings->")))) = true := by
  native_decide

/-- The same Node before the author opts in: the author Agent, its Context and
the Node's other configuration, with nothing paired yet. -/
def unpaired : State :=
  commitAgentCreation
    { objects := [⟨"nodes", "node", "user"⟩, ⟨"configuration", "config", "user"⟩],
      relations := [⟨"nodes", "node", "administrator", .actor "engineer"⟩,
        ⟨"nodes", "node", "runtime_reader", .actor "runtime"⟩,
        ⟨"configuration", "config", "parent", .object "nodes" "node"⟩],
      enrolled := [] }
    "a" "user" "author" "node" true

/-- `pairings_apply` as document ACP sees it: the author creates the overlay
through the native create gate and links it to its Node, both in the one
native transaction that creates it. External premise (defradb.rs #2770):
outside a transaction a relationship change also requires node
`get-collection`, which the author Agent does not hold, so the `parent` grant
below stands only for the in-transaction link. -/
def paired : State :=
  apply (directCreate unpaired "author" "application_pairings" "overlay" true true)
    (.grantAs "author" ⟨"application_pairings", "overlay", "parent", .object "nodes" "node"⟩)

/-- Opting in changes no actor's access to any other document: the overlay
adds one owned document and its own `parent` tuple, and grants nothing on the
Node, the Agent, configuration or the collections it names. What reaches the
peer is bounded by the route
(`PairingReconcile.ApplicationRoute.route_carries_only_base_and_named`). -/
theorem opting_in_changes_no_other_access :
    (["user", "engineer", "runtime", "author", "sibling"].flatMap fun actor =>
      [("nodes", "node"), ("agents", "a"), ("configuration", "config")].flatMap fun (resource, id) =>
        ["read", "update", "delete"].map fun permission =>
          allowed 8 paired actor resource id permission ==
            allowed 8 unpaired actor resource id permission).all id = true := by
  native_decide

theorem paired_overlay_is_the_authors :
    allowed 8 paired "author" "application_pairings" "overlay" "update" = true ∧
    allowed 8 paired "user" "application_pairings" "overlay" "delete" = true ∧
    allowed 8 paired "sibling" "application_pairings" "overlay" "read" = false := by
  native_decide

end Identity.ApplicationPairingAccess
