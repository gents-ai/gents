import Proofs.ScopeTemplates.State

namespace ScopeTemplates.AppFilters

structure FieldFilter where
  collection : String
  field : String
  value : String
  deriving DecidableEq, Repr

structure FieldAuthority where
  collection : String
  field : String
  immutable : Bool
  deriving DecidableEq, Repr

def permitted (collections : List String) (schema : List FieldAuthority)
    (filter : FieldFilter) : Bool :=
  collections.contains filter.collection &&
    !filter.field.isEmpty &&
    schema.any (fun field => field.collection == filter.collection &&
      field.field == filter.field && field.immutable)

def validate (collections : List String) (schema : List FieldAuthority)
    (filters : List FieldFilter) : Option (List FieldFilter) :=
  let names := filters.map FieldFilter.collection
  if filters.all (permitted collections schema) && names.eraseDups.length == names.length
  then some filters else none

theorem valid_filter_has_collection (collections : List String) (schema : List FieldAuthority)
    (filter : FieldFilter) (h : permitted collections schema filter = true) :
    collections.contains filter.collection = true := by
  simp only [permitted, Bool.and_eq_true] at h
  exact h.1.1

example : validate ["Post"] [{ collection := "Post", field := "to_host", immutable := true }]
    [{ collection := "Post", field := "to_host", value := "node" }] ≠ none := by decide

example : validate ["Post"] [{ collection := "Post", field := "to_host", immutable := false }]
    [{ collection := "Post", field := "to_host", value := "node" }] = none := by decide

example : validate ["Post"] [{ collection := "Other", field := "to_host", immutable := true }]
    [{ collection := "Other", field := "to_host", value := "node" }] = none := by decide

example : validate ["Post"] [{ collection := "Post", field := "to_host", immutable := true }]
    [{ collection := "Post", field := "to_host", value := "node" },
     { collection := "Post", field := "to_host", value := "other" }] = none := by decide

end ScopeTemplates.AppFilters
