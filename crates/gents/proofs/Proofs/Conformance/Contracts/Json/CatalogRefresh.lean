import Proofs.Configuration.CatalogRefresh
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.Contracts.CatalogRefresh
open Configuration.CatalogRefresh

/-- Canonical native AdvertisedModel serialization, including absent options. -/
def payload (name : String) (window : Nat) : String :=
  "[{\"model_name\":" ++ jsonString name ++
  ",\"display_name\":null,\"context_window\":" ++ toString window ++
  ",\"max_context_window\":null,\"max_output_tokens\":null,\"reasoning_efforts\":null}]"

def stateJson (state : Option State) : String :=
  match state with
  | none => "null"
  | some value => "{\"payload\":" ++ jsonString value.payload ++
      ",\"content_at\":" ++ toString value.contentAt ++
      ",\"observed_at\":" ++ toString value.observedAt ++ "}"

def caseJson (witness : String) (actions : List (String × Nat)) : String :=
  let (state, contentWrites, watermarkWrites) := actions.foldl
    (fun (acc : Option State × Nat × Nat) (action : String × Nat) =>
      let next := observe acc.1 action.1 action.2
      (some next,
        acc.2.1 + if contentChanged acc.1 next then 1 else 0,
        acc.2.2 + if watermarkChanged acc.1 next then 1 else 0))
    (none, 0, 0)
  "{\"witness\":" ++ jsonString witness ++ ",\"actions\":" ++
    jsonArray (actions.map fun action => "{\"payload\":" ++ jsonString action.1 ++
      ",\"observed_at\":" ++ toString action.2 ++ "}") ++
    ",\"expected\":" ++ stateJson state ++
    ",\"content_writes\":" ++ toString contentWrites ++
    ",\"watermark_writes\":" ++ toString watermarkWrites ++ "}"

def casesJson : String :=
  let a := payload "model-a" 128
  let b := payload "model-b" 256
  jsonArray [
    caseJson "catalog.empty" [],
    caseJson "catalog.first" [(a, 10)],
    caseJson "catalog.identical-newer" [(a, 10), (a, 12)],
    caseJson "catalog.identical-stale" [(a, 10), (a, 9)],
    caseJson "catalog.delayed-change-after-identical" [(a, 10), (a, 12), (b, 11)],
    caseJson "catalog.fresh-change" [(a, 10), (a, 12), (b, 13)],
    caseJson "catalog.exact-repeat" [(a, 10), (a, 10)],
    caseJson "catalog.same-time-change" [(a, 10), (b, 10)],
    caseJson "catalog.capability-change" [(a, 10), (payload "model-a" 256, 11)],
    caseJson "catalog.successful-empty" [(a, 10), ("[]", 12)],
    caseJson "catalog.stale-empty" [(a, 10), ("[]", 9)]
  ]

end Conformance.Contracts.CatalogRefresh
