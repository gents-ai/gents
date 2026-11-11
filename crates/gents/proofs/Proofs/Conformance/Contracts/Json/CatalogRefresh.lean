import Proofs.Documents.Configuration.CatalogRefresh
import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.Family

namespace Conformance.Contracts.CatalogRefresh
open Lean Configuration.CatalogRefresh

/-- Canonical native AdvertisedModel serialization, including absent options. -/
def advertisedPayload (name : String) (window : Nat) : String :=
  "[{\"model_name\":" ++ jsonString name ++
  ",\"display_name\":null,\"context_window\":" ++ toString window ++
  ",\"max_context_window\":null,\"max_output_tokens\":null,\"reasoning_efforts\":null}]"

structure Refresh where
  payload : String
  observedAt : Nat
  deriving ToJson

structure Input where
  witness : String
  refreshes : List Refresh
  deriving ToJson

structure Stored where
  payload : String
  contentAt : Nat
  observedAt : Nat
  deriving ToJson

structure Output where
  expected : Option Stored
  contentWrites : Nat
  watermarkWrites : Nat
  deriving ToJson

def observeAll (refreshes : List Refresh) : Option State × Nat × Nat :=
  refreshes.foldl
    (fun (acc : Option State × Nat × Nat) (refresh : Refresh) =>
      let next := observe acc.1 refresh.payload refresh.observedAt
      (some next,
        acc.2.1 + if contentChanged acc.1 next then 1 else 0,
        acc.2.2 + if watermarkChanged acc.1 next then 1 else 0))
    (none, 0, 0)

def outcome (input : Input) : Output :=
  let (state, contentWrites, watermarkWrites) := observeAll input.refreshes
  { expected := state.map fun value =>
      ({ payload := value.payload, contentAt := value.contentAt, observedAt := value.observedAt } : Stored)
  , contentWrites := contentWrites
  , watermarkWrites := watermarkWrites }

def scenario (witness : String) (refreshes : List (String × Nat)) : Input :=
  { witness := witness
  , refreshes := refreshes.map fun (payload, observedAt) =>
      ({ payload := payload, observedAt := observedAt } : Refresh) }

@[conformance_family]
def scenarios : Family Input Output where
  inputs :=
    let a := advertisedPayload "model-a" 128
    let b := advertisedPayload "model-b" 256
    [ scenario "catalog.empty" []
    , scenario "catalog.first" [(a, 10)]
    , scenario "catalog.identical-newer" [(a, 10), (a, 12)]
    , scenario "catalog.identical-stale" [(a, 10), (a, 9)]
    , scenario "catalog.delayed-change-after-identical" [(a, 10), (a, 12), (b, 11)]
    , scenario "catalog.fresh-change" [(a, 10), (a, 12), (b, 13)]
    , scenario "catalog.exact-repeat" [(a, 10), (a, 10)]
    , scenario "catalog.same-time-change" [(a, 10), (b, 10)]
    , scenario "catalog.capability-change" [(a, 10), (advertisedPayload "model-a" 256, 11)]
    , scenario "catalog.successful-empty" [(a, 10), ("[]", 12)]
    , scenario "catalog.stale-empty" [(a, 10), ("[]", 9)]
    ]
  model := outcome

end Conformance.Contracts.CatalogRefresh
