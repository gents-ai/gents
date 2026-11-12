import Proofs.Documents.ScopeTemplates.State
import Proofs.Conformance.Family

namespace Conformance.ScopeCollectionRules

open Lean ScopeTemplates

deriving instance ToJson for DidSource, CollectionRule

structure TemplateInput where
  template : String
  deriving ToJson

@[conformance_family]
def templateRules : Family TemplateInput (Option (List CollectionRule)) where
  inputs := builtinCatalog.map fun template => { template := template.id }
  model := fun i => (builtinCatalog.find? (·.id == i.template)).bind fun template =>
    match template.scope with
    | .perCollection rules => some rules
    | .clientRoute => none

end Conformance.ScopeCollectionRules
