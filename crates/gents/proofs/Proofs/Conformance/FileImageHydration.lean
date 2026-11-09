import Proofs.FileInventory.FileImages
import Proofs.ScopeTemplates
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.FileImageHydrationContracts

open FileInventory.FileImages Conformance.Contracts

private def boolJson (value : Bool) := if value then "true" else "false"

private def predicateJson (predicate : ScopeTemplates.CollectionPredicate) :=
  "{\"collection\":" ++ jsonString predicate.collection ++
  ",\"clauses\":" ++ jsonArray (predicate.clauses.map fun clause =>
    "{\"field\":" ++ jsonString clause.field ++
    ",\"operator\":" ++ jsonString clause.operator ++
    ",\"value\":" ++ jsonString clause.value ++ "}") ++ "}"

private def routeCaseJson (direction : ScopeTemplates.RouteDirection)
    (requester owner demandCollection imageCollection : String) :=
  let filters := ScopeTemplates.clientRouteFilters direction requester owner
  let expected := filters.find? (fun predicate => predicate.collection == demandCollection)
  "{\"direction\":" ++ jsonString (match direction with
    | .clientToRuntime => "client-to-runtime" | .runtimeToClient => "runtime-to-client") ++
  ",\"requester\":" ++ jsonString requester ++ ",\"owner\":" ++ jsonString owner ++
  ",\"demand_collection\":" ++ jsonString demandCollection ++
  ",\"image_collection\":" ++ jsonString imageCollection ++
  ",\"expected\":" ++ (expected.map predicateJson).getD "null" ++
  ",\"images_granted\":" ++ boolJson ((ScopeTemplates.clientRouteCollections direction).contains imageCollection) ++
  ",\"image_filter_installed\":" ++ boolJson (filters.any (fun predicate => predicate.collection == imageCollection)) ++ "}"

def routeCasesJson := jsonArray (
  ([ScopeTemplates.RouteDirection.clientToRuntime, .runtimeToClient]).flatMap fun direction =>
    [routeCaseJson direction "requester-1" "node-1" "FileImageHydrationRequest" "ToolFileChange",
     routeCaseJson direction "requester-\"quoted" "node-\\path" "FileImageHydrationRequest" "ToolFileChange",
     routeCaseJson direction "requester-1" "node-1" "FileInventoryRequest" "FileInventoryPage",
     routeCaseJson direction "requester-\"quoted" "node-\\path" "FileInventoryRequest" "FileInventoryPage"])

def subscriptionCasesJson := jsonArray (
  ScopeTemplates.transcriptCollections.map fun collection =>
    "{\"collection\":" ++ jsonString collection ++
    ",\"replicated\":" ++ boolJson false ++ "}")

def storageCasesJson := jsonArray (
  ["node", "client"].flatMap fun owner =>
    [some "node", some "client", some "outsider", none].map fun actor =>
      "{\"owner\":" ++ jsonString owner ++
      ",\"actor\":" ++ (actor.map jsonString).getD "null" ++
      ",\"expected\":" ++ boolJson (storedImageReadable "node" "client" (owner == "client") actor) ++ "}")

def readGrantCasesJson := jsonArray (
  ["node", ""].flatMap fun source =>
    ["client", ""].flatMap fun requester =>
      ["node", "other"].flatMap fun signer =>
        ["client", "other"].flatMap fun localActor =>
          ["node", "other", ""].flatMap fun storedCreator =>
            [true, false].map fun verified =>
              "{\"source\":" ++ jsonString source ++
              ",\"requester\":" ++ jsonString requester ++
              ",\"signer\":" ++ jsonString signer ++
              ",\"local_actor\":" ++ jsonString localActor ++
              ",\"stored_creator\":" ++ jsonString storedCreator ++
              ",\"verified\":" ++ boolJson verified ++
              ",\"expected\":" ++ boolJson (authorizesReadGrant source requester signer localActor storedCreator verified) ++ "}")

end Conformance.FileImageHydrationContracts
