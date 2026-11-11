import Proofs.Documents.SchemaObligations
import Proofs.Conformance.Family

namespace Conformance.SchemaObligations

open Lean _root_.SchemaObligations

deriving instance ToJson for Scalar, FieldType, FieldDecl, Payload

/-- The configuration document that names the field. A datastore surface asks
only for a count; an event source may also ask for a correlation. -/
inductive Document where
  | surface
  | eventSource
  deriving ToJson

structure Step where
  create : Payload
  replacement : Payload
  deriving ToJson

structure Input where
  name : String
  document : Document
  schema : List FieldDecl
  steps : List Step
  deriving ToJson

structure StepOutput where
  preflight : Bool
  accepted : Bool
  live : Option Payload
  deriving ToJson

structure Output where
  steps : List StepOutput
  deriving ToJson

private def schema : List FieldDecl :=
  [ ⟨"total", .int, false⟩, ⟨"required_total", .int, true⟩, ⟨"ratio", .float32, false⟩
  , ⟨"amount", .float64, false⟩, ⟨"label", .string, false⟩, ⟨"required_label", .string, true⟩
  , ⟨"marker", .id, false⟩, ⟨"attachment", .blob, false⟩, ⟨"payload", .json, false⟩
  , ⟨"observed_at", .dateTime, false⟩, ⟨"required_observed_at", .dateTime, true⟩
  , ⟨"flag", .boolean, false⟩, ⟨"tags", .stringList, false⟩ ]

private def counting (field : String) (registered : Bool := true) : Payload :=
  { registered, count := some field, correlation := none }

private def grouped (field : String) (registered : Bool := true) : Payload :=
  { registered, count := some field, correlation := some "label" }

private def correlated (field : String) (registered : Bool := true) : Payload :=
  { registered, count := none, correlation := some field }

private def once (payload : Payload) : List Step := [⟨payload, payload⟩]

private def scenario (name : String) (document : Document) (steps : List Step) : Input :=
  { name, document, schema, steps }

private def creationThenReplacement (payload : String → Payload) : List Step :=
  [ ⟨payload "flag", payload "total"⟩, ⟨payload "total", payload "flag"⟩
  , ⟨payload "amount", payload "flag"⟩, ⟨payload "flag", payload "amount"⟩ ]

private def inputs : List Input :=
  schema.map (fun field =>
    scenario s!"surface_count_in_{field.name}" .surface (once (counting field.name))) ++
  schema.map (fun field =>
    scenario s!"event_source_count_in_{field.name}" .eventSource (once (grouped field.name))) ++
  schema.map (fun field =>
    scenario s!"event_source_correlated_by_{field.name}" .eventSource
      (once (correlated field.name))) ++
  [ scenario "surface_count_in_a_missing_field" .surface (once (counting "missing"))
  , scenario "surface_on_an_unregistered_collection" .surface (once (counting "missing" false))
  , scenario "surface_creation_then_replacement" .surface (creationThenReplacement counting)
  , scenario "event_source_count_in_a_missing_field" .eventSource (once (grouped "missing"))
  , scenario "event_source_correlated_by_a_missing_field" .eventSource
      (once (correlated "missing"))
  , scenario "event_source_on_an_unregistered_collection" .eventSource
      (once { registered := false, count := some "missing", correlation := some "missing" })
  , scenario "event_source_good_count_bad_correlation" .eventSource
      (once { registered := true, count := some "total", correlation := some "total" })
  , scenario "event_source_creation_then_replacement" .eventSource
      (creationThenReplacement grouped)
  , scenario "event_source_correlation_creation_then_replacement" .eventSource
      [ ⟨correlated "total", correlated "label"⟩, ⟨correlated "label", correlated "total"⟩
      , ⟨correlated "required_label", correlated "flag"⟩
      , ⟨correlated "flag", correlated "required_label"⟩ ] ]

private def run (schema : List FieldDecl) : Option Payload → List Step → List StepOutput
  | _, [] => []
  | live, step :: rest =>
      let outcome := publish schema live step.create step.replacement
      { preflight := preflight schema live step.create step.replacement
        accepted := outcome.accepted
        live := outcome.live } :: run schema outcome.live rest

@[conformance_family]
def publication : Family Input Output where
  inputs := inputs
  model := fun i => { steps := run i.schema none i.steps }

end Conformance.SchemaObligations
