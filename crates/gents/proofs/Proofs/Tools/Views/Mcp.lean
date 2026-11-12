import Proofs.Store.Views

namespace McpViews

open Lean

inductive Reader where
  | owner
  | guest
  deriving DecidableEq, Repr, ToJson

inductive Node where
  | local
  | other
  deriving DecidableEq, Repr, ToJson

inductive Selection where
  | all
  | known
  | missing
  deriving DecidableEq, Repr, ToJson

structure Service where
  node : Node
  id : String
  enabled : Option Bool
  guestReadable : Bool
  deriving DecidableEq, Repr

def Reader.did : Reader → String
  | .owner => "owner"
  | .guest => "guest"

def Node.did : Node → String
  | .local => "local"
  | .other => "other"

def readable (reader : Reader) (service : Service) : Bool :=
  reader == .owner || service.guestReadable

def selected (selection : Selection) (service : Service) : Bool :=
  match selection with
  | .all => true
  | .known => service.id == "z"
  | .missing => service.id == "missing"

/-- Null availability is enabled. Disabled services remain in an inventory;
only an explicit enabled-service request excludes them. Scope never grants
access: DefraDB evaluates the reading actor's ACP permissions first. -/
def includes (reader : Reader) (node : Node) (enabledOnly : Bool)
    (selection : Selection) (service : Service) : Bool :=
  ReadViews.visible .ownNode node.did reader.did service.node.did ""
      (readable reader service) &&
    (!enabledOnly || service.enabled.getD true) && selected selection service

def documents : List Service :=
  [.local, .other].flatMap fun node =>
    [⟨node, "a", none, true⟩, ⟨node, "d", some false, false⟩,
     ⟨node, "z", some true, false⟩]

def rows (reader : Reader) (node : Node) (enabledOnly : Bool)
    (selection : Selection) (services : List Service) : List Service :=
  services.filter (includes reader node enabledOnly selection)

theorem rows_subset_readable (reader : Reader) (node : Node) (enabledOnly : Bool)
    (selection : Selection) (services : List Service) (service : Service)
    (member : service ∈ rows reader node enabledOnly selection services) :
    readable reader service = true := by
  have included := (List.mem_filter.mp member).2
  simp only [includes, Bool.and_eq_true] at included
  exact ReadViews.visible_requires_readable _ _ _ _ _ _ included.1.1

def inventory (reader : Reader) (node : Node) (enabledOnly : Bool)
    (selection : Selection) : List String :=
  (rows reader node enabledOnly selection documents).map Service.id

end McpViews

namespace McpViews

inductive Cursor where
  | start
  | a
  | z
  deriving DecidableEq, Repr, Lean.ToJson

inductive PageSize where
  | one
  | two
  deriving DecidableEq, Repr, Lean.ToJson

def Cursor.value : Cursor → String
  | .start => ""
  | .a => "a"
  | .z => "z"

def PageSize.count : PageSize → Nat
  | .one => 1
  | .two => 2

def page (reader : Reader) (node : Node) (cursor : Cursor) (pageSize : PageSize) : List String :=
  ((inventory reader node false .all).filter (cursor.value < ·)).take pageSize.count

structure Health where
  node : Node
  serviceId : String
  status : String
  guestReadable : Bool
  deriving DecidableEq, Repr

/-- Observations arrive in the query's service-ID, descending update-time and
ascending document-ID order. ACP removes unreadable observations before the
latest remaining observation for each service is selected. -/
def observations : List Health :=
  [.local, .other].flatMap fun node =>
    [⟨node, "a", "healthy", false⟩, ⟨node, "a", "stale", true⟩,
     ⟨node, "z", "healthy", false⟩]

def healthRows (reader : Reader) (node : Node) : List Health :=
  ReadViews.rows .ownNode node.did (some reader.did) (fun row => row.node.did)
    (fun _ => "") (fun actor row => actor == Reader.owner.did || row.guestReadable)
    observations

theorem healthRows_subset_readable (reader : Reader) (node : Node) (row : Health)
    (included : row ∈ healthRows reader node) :
    (reader.did == Reader.owner.did || row.guestReadable) = true := by
  obtain ⟨actor, actor_eq, allowed⟩ := ReadViews.rows_subset_readable
    .ownNode node.did (some reader.did) (fun (entry : Health) => entry.node.did) (fun _ => "")
    (fun actor (entry : Health) => actor == Reader.owner.did || entry.guestReadable)
    observations row included
  have same : reader.did = actor := Option.some.inj actor_eq
  simpa [← same] using allowed

def health (reader : Reader) (node : Node) : List String :=
  ["a", "z"].filterMap fun serviceId =>
    ((healthRows reader node).find? (·.serviceId == serviceId)).map fun row =>
      row.serviceId ++ "=" ++ row.status

end McpViews
