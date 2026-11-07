import Proofs.ScopeTemplates.State

namespace ScopeTemplates.AppFilters

structure FieldFilter where
  collection : String
  field : String
  value : String
  version : String := "v1"
  deriving DecidableEq, Repr

structure FieldAuthority where
  collection : String
  field : String
  immutable : Bool
  version : String := "v1"
  deriving DecidableEq, Repr

def permitted (collections : List String) (schema : List FieldAuthority)
    (filter : FieldFilter) : Bool :=
  collections.contains filter.collection &&
    !filter.field.isEmpty &&
    !filter.version.isEmpty &&
    schema.any (fun field => field.collection == filter.collection &&
      field.field == filter.field && field.version == filter.version && field.immutable)

def validate (collections : List String) (schema : List FieldAuthority)
    (filters : List FieldFilter) : Option (List FieldFilter) :=
  let names := filters.map FieldFilter.collection
  if filters.all (permitted collections schema) && names.eraseDups.length == names.length
  then some filters else none

def subscriptions (collections : List String) (filters : List FieldFilter) : List String :=
  collections.filter (fun collection => !(filters.map FieldFilter.collection).contains collection)

theorem filtered_collection_not_subscribed (collections : List String)
    (filters : List FieldFilter) (collection : String)
    (h : collection ∈ filters.map FieldFilter.collection) :
    collection ∉ subscriptions collections filters := by
  intro member
  have condition := (List.mem_filter.mp member).2
  have present : (filters.map FieldFilter.collection).contains collection = true := by
    simpa using h
  rw [present] at condition
  contradiction

inductive PairingMode where
  | unfiltered
  | filtered
  deriving DecidableEq, Repr

/-- A filtered pairing must not fall back to an unfiltered route when its
    filter document is absent, invalid, or empty. -/
def resolve (mode : PairingMode) (collections : List String)
    (schema : List FieldAuthority) (filters : Option (List FieldFilter)) :
    Option (List FieldFilter) :=
  match mode, filters with
  | .unfiltered, none => some []
  | .filtered, some (head :: tail) => validate collections schema (head :: tail)
  | _, _ => none

theorem filtered_missing_refused (collections : List String) (schema : List FieldAuthority) :
    resolve .filtered collections schema none = none := rfl

theorem filtered_empty_refused (collections : List String) (schema : List FieldAuthority) :
    resolve .filtered collections schema (some []) = none := rfl

theorem unfiltered_with_filters_refused (collections : List String) (schema : List FieldAuthority)
    (filters : List FieldFilter) :
    resolve .unfiltered collections schema (some filters) = none := rfl

theorem valid_filter_has_collection (collections : List String) (schema : List FieldAuthority)
    (filter : FieldFilter) (h : permitted collections schema filter = true) :
    collections.contains filter.collection = true := by
  simp only [permitted, Bool.and_eq_true] at h
  exact h.1.1.1

example : validate ["Post"] [{ collection := "Post", field := "to_host", immutable := true }]
    [{ collection := "Post", field := "to_host", value := "node" }] ≠ none := by decide

example : validate ["Post"] [{ collection := "Post", field := "to_host", immutable := false }]
    [{ collection := "Post", field := "to_host", value := "node" }] = none := by decide

example : validate ["Post"] [{ collection := "Other", field := "to_host", immutable := true }]
    [{ collection := "Other", field := "to_host", value := "node" }] = none := by decide

example : validate ["Post"] [{ collection := "Post", field := "to_host", immutable := true }]
    [{ collection := "Post", field := "to_host", value := "node" },
     { collection := "Post", field := "to_host", value := "other" }] = none := by decide

example : validate ["Post"] [{ collection := "Post", field := "to_host", immutable := true, version := "v2" }]
    [{ collection := "Post", field := "to_host", value := "node" }] = none := by decide

end ScopeTemplates.AppFilters
