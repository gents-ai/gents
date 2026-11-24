import Proofs.Store.Identity.Household

/-!
Document access policies an Agent writes for application collections.

Rust owner: `crates/gents-store/src/identity/application_policy.rs`
(`ApplicationPolicy::admit` and `ApplicationPolicy::yaml`). The config tool's
`policy create` registers an admitted definition as the calling Agent, which
needs `add-dac-policy` (`GrantSets.engineer_holds_add_dac_policy`); the schema
tool binds it to a new application collection with `@policy`; `policy grant`
and `policy revoke` change its relations on one document.
-/
namespace Identity.ApplicationPolicy

open Identity.Household

/-- A policy as an Agent writes it: a name and resources in the Household
policy's shape. A permission is the union of the relations it names and of
`relation->permission` paths through object relations. No permission subtracts
or intersects, so its answers grow with its tuples (`Household.allowedIn_mono`)
and a revocation only narrows (`revokeIn_only_narrows`). -/
structure Definition where
  name : String
  resources : List ResourceDefinition

/-- The permissions DefraDB requires of a resource a collection binds.
External premise: the pinned DefraDB `validate_resource_interface`. -/
def documentPermissions : List String := ["read", "update", "delete"]

/-- Decision: a name beginning `Gents` is the runtime's. Every built-in policy
is named so (`Household.policyName`, and "Gents retained file images"), so an
Agent's policy never carries a runtime policy's name. -/
def reservedName (name : String) : Bool := name.startsWith "Gents"

theorem household_name_reserved : reservedName policyName = true := by native_decide

def nameChar (c : Char) : Bool := c.isAlphanum || c == ' ' || c == '-' || c == '_'

/-- A policy name is one line of ASCII letters, digits, spaces, hyphens and
underscores, at most 64 characters, starting with a letter and not ending with a
space; the rendered policy quotes it. -/
def validName (name : String) : Bool :=
  !name.isEmpty && name.length ≤ 64 && name.front.isAlpha && name.all nameChar &&
    !name.endsWith " "

def identifierChar (c : Char) : Bool := c.isLower || c.isDigit || c == '_'

/-- Resource, relation and permission names are lowercase identifiers. The
rendered policy writes them unquoted, so the YAML 1.2 core scalars `true`,
`false` and `null` are refused. External premise: the pinned DefraDB parses
policies with `serde_yaml` 0.9. -/
def identifier (word : String) : Bool :=
  !word.isEmpty && word.length ≤ 64 && word.front.isLower && word.all identifierChar &&
    !["true", "false", "null"].contains word

def relationNames (r : ResourceDefinition) : List String := r.relations.map (·.name)

def names (r : ResourceDefinition) : List String :=
  relationNames r ++ r.permissions.map (·.name)

def distinct (words : List String) : Bool := words.eraseDups.length == words.length

def resourceNamed (d : Definition) (name : String) : Option ResourceDefinition :=
  d.resources.find? (·.name == name)

/-- A relation type names a subject inside this policy: any actor, an object
of one of its resources, or the holders of a relation of one. No type reaches a
resource of another policy, so an Agent's policy grants nothing on a runtime
document. -/
def typeInside (d : Definition) (type : String) : Bool :=
  type == "actor" || d.resources.any fun r =>
    type == r.name || (relationNames r).any fun n => type == r.name ++ "->" ++ n

/-- A parent path `relation->name`: the relation is this resource's, every type
it declares is a resource of this policy, and each of those declares `name`. -/
def parentInside (d : Definition) (r : ResourceDefinition) (parent : String × String) : Bool :=
  match r.relations.find? (·.name == parent.1) with
  | none => false
  | some relation => !relation.types.isEmpty && relation.types.all fun type =>
      match resourceNamed d type with
      | none => false
      | some target => (names target).contains parent.2

inductive Refusal where
  | name
  | reservedName
  | noResources
  | identifier
  | owner
  | duplicate
  | documentPermissions
  | untyped
  | relationType
  | managed
  | permissionRelation
  | permissionParent
  deriving BEq, DecidableEq, Repr

def Refusal.wire : Refusal → String
  | .name => "name"
  | .reservedName => "reserved-name"
  | .noResources => "no-resources"
  | .identifier => "identifier"
  | .owner => "owner"
  | .duplicate => "duplicate"
  | .documentPermissions => "document-permissions"
  | .untyped => "untyped"
  | .relationType => "relation-type"
  | .managed => "managed"
  | .permissionRelation => "permission-relation"
  | .permissionParent => "permission-parent"

/-- The rules a definition passes before it is registered, in the order a
refusal names the first one it breaks. `owner` is DefraDB's own relation on
every resource: a document's creator owns it, and no grant or revoke reaches it
(`owner_is_never_managed`). Every relation declares a type, so none admits
every subject (`Household.typeAdmitted`). -/
def checks (d : Definition) : List (Refusal × Bool) :=
  [(.name, validName d.name),
   (.reservedName, !reservedName d.name),
   (.noResources, !d.resources.isEmpty),
   (.identifier, d.resources.all fun r => identifier r.name && (names r).all identifier),
   (.owner, d.resources.all fun r => !(names r).contains "owner"),
   (.duplicate, distinct (d.resources.map (·.name)) && d.resources.all fun r => distinct (names r)),
   (.documentPermissions, d.resources.all fun r =>
      documentPermissions.all fun p => (r.permissions.map (·.name)).contains p),
   (.untyped, d.resources.all fun r => r.relations.all fun rel => !rel.types.isEmpty),
   (.relationType, d.resources.all fun r => r.relations.all fun rel => rel.types.all (typeInside d)),
   (.managed, d.resources.all fun r => r.relations.all fun rel =>
      rel.manages.all fun m => (relationNames r).contains m),
   (.permissionRelation, d.resources.all fun r => r.permissions.all fun p =>
      p.relations.all fun n => (relationNames r).contains n),
   (.permissionParent, d.resources.all fun r => r.permissions.all fun p =>
      p.parents.all (parentInside d r))]

def refusal (d : Definition) : Option Refusal :=
  ((checks d).find? fun check => !check.2).map (·.1)

def admissible (d : Definition) : Bool := (refusal d).isNone

/-- The policy text registered for an admitted definition: the Household
rendering, with the name quoted. -/
def yaml (d : Definition) : String :=
  policyYamlNamed ("\"" ++ d.name ++ "\"") d.resources

theorem passes_of_admissible {d : Definition} (h : admissible d = true) {r : Refusal}
    {ok : Bool} (mem : (r, ok) ∈ checks d) : ok = true := by
  unfold admissible refusal at h
  simp only [Option.isNone_iff_eq_none, Option.map_eq_none'] at h
  have := List.find?_eq_none.mp h (r, ok) mem
  simpa using this

theorem admissible_name_is_not_reserved {d : Definition} (h : admissible d = true) :
    reservedName d.name = false := by
  have := passes_of_admissible h (r := .reservedName) (ok := !reservedName d.name) (by
    unfold checks
    repeat (first | exact List.mem_cons_self _ _ | apply List.mem_cons_of_mem))
  simpa using this

theorem admissible_declares_document_permissions {d : Definition} (h : admissible d = true) :
    ∀ r ∈ d.resources, ∀ p ∈ documentPermissions, (r.permissions.map (·.name)).contains p = true := by
  have := passes_of_admissible h (r := .documentPermissions)
    (ok := d.resources.all fun r =>
      documentPermissions.all fun p => (r.permissions.map (·.name)).contains p) (by
    unfold checks
    repeat (first | exact List.mem_cons_self _ _ | apply List.mem_cons_of_mem))
  simp only [List.all_eq_true] at this
  exact this

theorem admissible_types_stay_inside {d : Definition} (h : admissible d = true) :
    ∀ r ∈ d.resources, ∀ rel ∈ r.relations, ∀ type ∈ rel.types, typeInside d type = true := by
  have := passes_of_admissible h (r := .relationType)
    (ok := d.resources.all fun r => r.relations.all fun rel => rel.types.all (typeInside d)) (by
    unfold checks
    repeat (first | exact List.mem_cons_self _ _ | apply List.mem_cons_of_mem))
  simp only [List.all_eq_true] at this
  exact this

theorem admissible_relations_are_typed {d : Definition} (h : admissible d = true) :
    ∀ r ∈ d.resources, ∀ rel ∈ r.relations, rel.types.isEmpty = false := by
  have := passes_of_admissible h (r := .untyped)
    (ok := d.resources.all fun r => r.relations.all fun rel => !rel.types.isEmpty) (by
    unfold checks
    repeat (first | exact List.mem_cons_self _ _ | apply List.mem_cons_of_mem))
  simp only [List.all_eq_true] at this
  intro r hr rel hrel
  simpa using this r hr rel hrel

theorem admissible_never_names_owner {d : Definition} (h : admissible d = true) :
    ∀ r ∈ d.resources, (names r).contains "owner" = false := by
  have := passes_of_admissible h (r := .owner)
    (ok := d.resources.all fun r => !(names r).contains "owner") (by
    unfold checks
    repeat (first | exact List.mem_cons_self _ _ | apply List.mem_cons_of_mem))
  simp only [List.all_eq_true] at this
  intro r hr
  simpa using this r hr

/-! ### Relations on documents of an Agent's policy

Native relationship management, `Household.mayManage`, over any definitions: the
document's owner, or a holder of a relation that manages the one changed. -/

def mayManageIn (definitions : List ResourceDefinition) (state : State) (actor : DID)
    (relation : Relation) : Bool :=
  match definitions.find? (·.name == relation.resource),
      state.objects.find? (fun object => object.resource == relation.resource && object.id == relation.id) with
  | some definition, some object =>
      relation.relation != "owner" &&
        (object.owner == actor || definition.relations.any (fun manager =>
          manager.manages.contains relation.relation &&
          allowedIn definitions 8 state actor relation.resource relation.id manager.name))
  | _, _ => false

def grantIn (definitions : List ResourceDefinition) (state : State) (actor : DID)
    (relation : Relation) : State :=
  if mayManageIn definitions state actor relation then
    { state with relations := relation :: state.relations }
  else state

def revokeIn (definitions : List ResourceDefinition) (state : State) (actor : DID)
    (relation : Relation) : State :=
  if mayManageIn definitions state actor relation then
    { state with relations := state.relations.filter (· != relation) }
  else state

/-- A grant adds only the relation its grantor manages: within authority the
grantor holds. -/
theorem grantIn_adds_only_managed (definitions : List ResourceDefinition) (state : State)
    (actor : DID) (relation fact : Relation)
    (member : fact ∈ (grantIn definitions state actor relation).relations) :
    fact ∈ state.relations ∨ (fact = relation ∧ mayManageIn definitions state actor relation = true) := by
  unfold grantIn at member
  split at member
  · rename_i managed
    simp only [List.mem_cons] at member
    rcases member with eq | old
    · exact Or.inr ⟨eq, managed⟩
    · exact Or.inl old
  · exact Or.inl member

theorem revokeIn_only_narrows (definitions : List ResourceDefinition) (state : State)
    (revoker : DID) (relation : Relation) (fuel : Nat) (actor resource id permission : String)
    (h : allowedIn definitions fuel (revokeIn definitions state revoker relation) actor resource id
      permission = true) :
    allowedIn definitions fuel state actor resource id permission = true := by
  unfold revokeIn at h
  split at h
  · exact allowedIn_mono definitions fuel
      { state with relations := state.relations.filter (· != relation) } state rfl
      (fun r hr => (List.mem_filter.mp hr).1) _ _ _ _ h
  · exact h

theorem grantIn_keeps_objects (definitions : List ResourceDefinition) (state : State) (actor : DID)
    (relation : Relation) : (grantIn definitions state actor relation).objects = state.objects := by
  unfold grantIn; split <;> rfl

theorem revokeIn_keeps_objects (definitions : List ResourceDefinition) (state : State) (actor : DID)
    (relation : Relation) : (revokeIn definitions state actor relation).objects = state.objects := by
  unfold revokeIn; split <;> rfl

theorem owner_is_never_managed (definitions : List ResourceDefinition) (state : State) (actor : DID)
    (resource id : String) (subject : Subject) :
    mayManageIn definitions state actor ⟨resource, id, "owner", subject⟩ = false := by
  unfold mayManageIn
  split <;> simp

/-- A document's creator holds every permission its resource declares, whatever
relations exist; no grant or revoke changes who owns it (`grantIn_keeps_objects`,
`revokeIn_keeps_objects`), so an Agent never locks itself out of a document it
created. -/
theorem owner_holds_declared_permissions (definitions : List ResourceDefinition) (fuel : Nat)
    (state : State) (object : Object) (permission : String) (definition : PermissionDefinition)
    (found : state.objects.find? (fun o => o.resource == object.resource && o.id == object.id) =
      some object)
    (declared : (definitions.find? (·.name == object.resource)).bind
      (fun r => r.permissions.find? (·.name == permission)) = some definition) :
    allowedIn definitions (fuel + 1) state object.owner object.resource object.id permission = true := by
  simp only [allowedIn, found, declared, beq_self_eq_true, Bool.true_or]

theorem owner_keeps_access_after_any_revoke (definitions : List ResourceDefinition) (fuel : Nat)
    (state : State) (revoker : DID) (relation : Relation) (object : Object) (permission : String)
    (definition : PermissionDefinition)
    (found : state.objects.find? (fun o => o.resource == object.resource && o.id == object.id) =
      some object)
    (declared : (definitions.find? (·.name == object.resource)).bind
      (fun r => r.permissions.find? (·.name == permission)) = some definition) :
    allowedIn definitions (fuel + 1) (revokeIn definitions state revoker relation) object.owner
      object.resource object.id permission = true :=
  owner_holds_declared_permissions definitions fuel _ object permission definition
    (by rw [revokeIn_keeps_objects]; exact found) declared

/-- What a document operation observes, given whether the caller carries node
DAC bypass and what the document policy answers. External premise (the pinned
DefraDB `check_doc_permission_as` and the query path's `permission_filter`): a
request carrying an authenticated native admin's DAC bypass passes every read,
update and delete check on a collection whatever its policy. The init User is
the native admin (`GrantSets.householdOwner`); every request Gents makes as a
managed Agent denies itself the bypass (`Household.readObserved`). -/
def documentOperationObserved (bypass documentAcp : Bool) : Bool := bypass || documentAcp

/-- No policy an Agent writes, and no relation it grants or revokes, removes the
operator's document access. -/
theorem operator_is_never_locked_out (definitions : List ResourceDefinition) (fuel : Nat)
    (state : State) (operator resource id permission : String) :
    documentOperationObserved true (allowedIn definitions fuel state operator resource id permission) =
      true := by
  simp [documentOperationObserved]

/-! ### Which policy decides a document

External premise (the pinned DefraDB): a collection's policy binding is set when
the collection is created and cannot be patched. Decision: the schema tool
creates only application collections (`SchemaCollections`), so an Agent's
policy binds only a collection it creates; every runtime collection keeps the
built-in policy it was created with, and operator-only relations there (a
WorkspaceRoot's `reader`, an Agent's `administrator`) stay as the Household
policy manages them. -/

def boundPolicy (bindings : List (String × String)) (collection : String) : Option String :=
  (bindings.find? (·.1 == collection)).map (·.2)

def bindCollection (bindings : List (String × String)) (collection policyId : String) :
    Option (List (String × String)) :=
  match boundPolicy bindings collection with
  | some _ => none
  | none => some ((collection, policyId) :: bindings)

theorem creating_a_collection_keeps_every_binding (bindings next : List (String × String))
    (collection policyId other bound : String) (existing : boundPolicy bindings other = some bound)
    (created : bindCollection bindings collection policyId = some next) :
    boundPolicy next other = some bound := by
  unfold bindCollection at created
  split at created
  · exact absurd created (by simp)
  · rename_i fresh
    simp only [Option.some.injEq] at created
    subst created
    by_cases same : collection = other
    · subst same
      rw [fresh] at existing
      exact absurd existing (by simp)
    · have differ : (collection == other) = false := by simpa using same
      simp only [boundPolicy, List.find?_cons, differ] at existing ⊢
      exact existing

/-! ### Example: one order shared read-only -/

def ordersResource : ResourceDefinition :=
  { name := "orders"
    relations := [⟨"reader", ["actor"], []⟩, ⟨"writer", ["actor"], []⟩,
      ⟨"reader_manager", ["actor"], ["reader"]⟩]
    permissions := [permission "read" ["reader", "writer"] [],
      permission "update" ["writer"] [], permission "delete" [] []] }

def orders : Definition := { name := "Orders access", resources := [ordersResource] }

theorem orders_admissible : admissible orders = true := by native_decide

def ordersState : State :=
  { objects := [⟨"orders", "order-1", "engineer"⟩], relations := [], enrolled := [] }

def analystReads : Relation := ⟨"orders", "order-1", "reader", .actor "analyst"⟩

/-- The Engineer owns the order and grants the analyst `reader`: the analyst
reads and cannot update, cannot pass the relation on, and loses the read when
the Engineer revokes it; the Engineer keeps every permission throughout. -/
theorem orders_shared_read_only :
    let shared := grantIn orders.resources ordersState "engineer" analystReads
    let withdrawn := revokeIn orders.resources shared "engineer" analystReads
    [allowedIn orders.resources 5 shared "analyst" "orders" "order-1" "read",
     allowedIn orders.resources 5 shared "analyst" "orders" "order-1" "update",
     mayManageIn orders.resources shared "analyst" ⟨"orders", "order-1", "reader", .actor "other"⟩,
     allowedIn orders.resources 5 withdrawn "analyst" "orders" "order-1" "read",
     allowedIn orders.resources 5 withdrawn "engineer" "orders" "order-1" "update"] =
    [true, false, false, false, true] := by native_decide

/-- A relation that manages another lets its holder grant only that one. -/
theorem reader_manager_grants_only_reader :
    let managing := grantIn orders.resources ordersState "engineer"
      ⟨"orders", "order-1", "reader_manager", .actor "lead"⟩
    [mayManageIn orders.resources managing "lead" analystReads,
     mayManageIn orders.resources managing "lead" ⟨"orders", "order-1", "writer", .actor "analyst"⟩,
     allowedIn orders.resources 5 managing "lead" "orders" "order-1" "read"] =
    [true, false, false] := by native_decide

end Identity.ApplicationPolicy
