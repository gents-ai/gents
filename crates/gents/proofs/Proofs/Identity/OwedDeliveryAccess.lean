import Proofs.Identity.Household

namespace Identity.OwedDeliveryAccess

open Identity.Household

/-- A Node owned by `user` with an administrator `admin`, an Engineer the user
made a Node administrator, a `runtime_reader`, an `agent_manager`, and one
Agent `a` with a session `s` it works in. The node key `runtime-key` has
published one owed delivery `owed` of a halt `a` performed on `s`, linked to
the Node by `parent`. `a` is that delivery's recorded actor; the plugin whose
delivery it is has no identity of its own and acts only as the recorded actor
(`Hooks.Safety.perform`). -/
def fixture : State :=
  let base : State :=
    { objects := [⟨"nodes", "node", "user"⟩, ⟨"sessions", "s", "a"⟩,
        ⟨"owed_deliveries", "owed", "runtime-key"⟩]
    , relations := [⟨"nodes", "node", "administrator", .actor "admin"⟩,
        ⟨"nodes", "node", "administrator", .actor "engineer"⟩,
        ⟨"nodes", "node", "runtime_reader", .actor "reader"⟩,
        ⟨"nodes", "node", "agent_manager", .actor "manager"⟩,
        ⟨"sessions", "s", "parent", .object "agents" "a"⟩,
        ⟨"sessions", "s", "working_agent", .actor "a"⟩,
        ⟨"owed_deliveries", "owed", "parent", .object "nodes" "node"⟩]
    , enrolled := [] }
  commitAgentCreation base "a" "user" "a" "node" true

def access (actor : String) : Bool × Bool × Bool :=
  (allowed 8 fixture actor "owed_deliveries" "owed" "read",
   allowed 8 fixture actor "owed_deliveries" "owed" "update",
   allowed 8 fixture actor "owed_deliveries" "owed" "delete")

/-- The node key, the record's native owner, is the only actor that updates
or deletes it, and reads it. Creation is the native create gate: the creator
becomes the owner (`directCreate`), and the redrive trusts a record only when
that owner is the node key (`forged_record_is_not_runtime_owned`). -/
theorem runtime_key_controls_owed_delivery :
    access "runtime-key" = (true, true, true) := by
  native_decide

/-- Node administration reads the record and nothing more: the Node owner, an
administrator, and an Engineer the user made an administrator read it, and
none of them updates or deletes it. -/
theorem node_administration_reads_owed_delivery :
    ["user", "admin", "engineer"].map access =
      [(true, false, false), (true, false, false), (true, false, false)] := by
  native_decide

/-- An Agent, including the delivery's own recorded actor and the session it
works in, neither reads nor writes the record; nor do the narrow Node readers
(`runtime_reader`, `agent_manager`) or an unrelated actor. -/
theorem agents_and_non_administrators_cannot_access_owed_delivery :
    ["a", "reader", "manager", "stranger"].map access =
      [(false, false, false), (false, false, false), (false, false, false),
       (false, false, false)] := by
  native_decide

/-- Only the node key manages the record's relations: `parent` has no manager
relation, so the Node owner and administrators cannot grant it onward. -/
theorem owed_delivery_relations_are_runtime_managed :
    ["user", "admin", "a"].map (fun actor =>
      mayManage fixture actor ⟨"owed_deliveries", "owed", "parent", .object "nodes" "other"⟩) =
      [false, false, false] ∧
    mayManage fixture "runtime-key"
      ⟨"owed_deliveries", "owed", "parent", .object "nodes" "node"⟩ = true := by
  native_decide

/-- No resource names `owed_deliveries` as a relation type, and none inherits a
permission from it, so a record is never a channel to another document: a
plugin's delivery reaches only what its recorded actor's own grants reach. -/
theorem owed_deliveries_reach_nothing :
    policy.all (fun resource => resource.relations.all (fun relation =>
      relation.types.all (fun type =>
        !(type == "owed_deliveries" || type.startsWith "owed_deliveries->")))) = true := by
  native_decide

/-- The redrive acts on a record only when its native owner is the node key. -/
def runtimeOwned (state : State) (runtime id : String) : Bool :=
  state.objects.any fun object =>
    object.resource == "owed_deliveries" && object.id == id && object.owner == runtime

/-- An actor that passes the node's create gate can create a document in the
collection, but it owns that document, so the redrive does not act on it. -/
theorem forged_record_is_not_runtime_owned :
    runtimeOwned (directCreate fixture "a" "owed_deliveries" "forged" true true)
        "runtime-key" "forged" = false ∧
    runtimeOwned fixture "runtime-key" "owed" = true := by
  native_decide

end Identity.OwedDeliveryAccess
