import Proofs.Identity.Household
import Lean

namespace Identity.EvalRecords
open Identity.Household Lean

/-- The creator owns each audit record. The physical Node parent is published
with the record in the same transaction; knowing its DID is not authority to
read a different Node or to substitute the runtime actor for the creator. -/
def publish (before : State) (creator id node resource : String)
    (nodeVisible created related committed : Bool) : State :=
  if nodeVisible && created && related && committed then
    apply (directCreate before creator resource id true true)
      (.grantAs creator ⟨resource, id, "parent", .object "nodes" node⟩)
  else before

def initial : State :=
  { objects := [⟨"nodes", "home", "user"⟩],
    relations := [⟨"nodes", "home", "runtime_reader", .actor "runtime"⟩], enrolled := [] }

theorem failed_relationship_aborts (s : State) (creator id node resource : String)
    (visible created committed : Bool) :
    publish s creator id node resource visible created false committed = s := by
  simp [publish]

theorem creator_is_not_replaced_by_runtime :
    let s := publish initial "evaluator" "trial" "home" "operations" true true true true
    (allowed 5 s "evaluator" "operations" "trial" "update",
     allowed 5 s "user" "operations" "trial" "read",
     allowed 5 s "user" "operations" "trial" "update",
     allowed 5 s "stranger" "operations" "trial" "read") = (true, true, false, false) := by
  native_decide

structure Case where
  name : String
  creator : String
  resource : String
  nodeVisible : Bool
  related : Bool
  committed : Bool

def cases : List Case :=
  [ ⟨"definition", "user", "configuration", true, true, true⟩
  , ⟨"run", "user", "operations", true, true, true⟩
  , ⟨"trial", "user", "operations", true, true, true⟩
  , ⟨"verdict", "user", "operations", true, true, true⟩
  , ⟨"optimization", "user", "operations", true, true, true⟩
  , ⟨"agent-creator", "agent", "operations", true, true, true⟩
  , ⟨"runtime-creator", "runtime", "operations", true, true, true⟩
  , ⟨"foreign-node", "other", "operations", false, true, true⟩
  , ⟨"relationship-failed", "user", "operations", true, false, true⟩
  , ⟨"commit-failed", "user", "operations", true, true, false⟩ ]

def caseJson (c : Case) : Json :=
  let s := publish initial c.creator c.name "home" c.resource c.nodeVisible true c.related c.committed
  Json.mkObj [("name", toJson c.name), ("creator", toJson c.creator),
    ("resource", toJson c.resource), ("node_visible", toJson c.nodeVisible),
    ("relationship_succeeded", toJson c.related), ("commit_succeeded", toJson c.committed),
    ("expected_created", toJson (s.objects.any (fun x => x.resource == c.resource && x.id == c.name))),
    ("expected_creator_update", toJson (allowed 5 s c.creator c.resource c.name "update")),
    ("expected_user_read", toJson (allowed 5 s "user" c.resource c.name "read")),
    ("expected_stranger_read", toJson (allowed 5 s "stranger" c.resource c.name "read"))]

def casesJson : String := (toJson (cases.map caseJson)).compress

/-- A reader can prove registration from a visible native registration bit,
without learning the owner. An older caller-relative endpoint proves it only
when the authenticated caller is the owner; malformed claims fail closed. -/
def registration (visible own : Bool) (registered : Option Bool) : Option Bool :=
  if (own && !visible) || ((registered == some true) && !visible) ||
      ((registered == some false) && own) then none
  else some (visible && (own || registered.getD false))

theorem hidden_never_proves_registration (own : Bool) (registered : Option Bool) :
    registration false own registered ≠ some true := by
  cases own <;> cases registered <;> simp [registration] <;> split <;> simp_all

def registrationCasesJson : String :=
  let cases : List (String × Bool × Bool × Option Bool) :=
    [("owner", true, true, some true), ("reader", true, false, some true),
     ("unregistered", true, false, some false), ("hidden", false, false, some false),
     ("legacy-owner", true, true, none), ("legacy-reader", true, false, none),
     ("invalid-hidden-owner", false, true, some false),
     ("invalid-hidden-registered", false, false, some true),
     ("invalid-unregistered-owner", true, true, some false)]
  (toJson (cases.map fun (name, visible, own, registered) => Json.mkObj
    [("name", toJson name), ("visible", toJson visible), ("is_owner", toJson own),
     ("owner_registered", toJson registered),
     ("expected_registered", toJson (registration visible own registered))])).compress
end Identity.EvalRecords
