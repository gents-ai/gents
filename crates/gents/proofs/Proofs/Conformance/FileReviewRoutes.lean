import Proofs.Documents.ScopeTemplates
import Proofs.Conformance.Family

namespace Conformance.FileReviewRouteContracts

open Lean

structure SubscriptionInput where
  collection : String
  deriving ToJson

structure SubscriptionOutput where
  replicated : Bool
  deriving ToJson

@[conformance_family]
def subscription : Family SubscriptionInput SubscriptionOutput where
  inputs := ScopeTemplates.transcriptCollections.map fun collection => { collection }
  model := fun i => { replicated := Identity.collectionReplicates i.collection }

deriving instance ToJson for ScopeTemplates.RouteDirection, ScopeTemplates.FilterClause,
  ScopeTemplates.CollectionPredicate

structure FileReviewRouteInput where
  direction : ScopeTemplates.RouteDirection
  requester : String
  owner : String
  demandCollection : String
  imageCollection : String
  deriving ToJson

structure FileReviewRouteOutput where
  expected : Option ScopeTemplates.CollectionPredicate
  imagesGranted : Bool
  imageFilterInstalled : Bool
  deriving ToJson

end Conformance.FileReviewRouteContracts

namespace Conformance

open FileReviewRouteContracts

@[conformance_family]
def fileReviewRoutes : Family FileReviewRouteInput FileReviewRouteOutput where
  inputs := [ScopeTemplates.RouteDirection.clientToRuntime, .runtimeToClient].flatMap fun direction =>
    [("requester-1", "node-1", "ToolFileChange"),
     ("requester-\"quoted", "node-\\path", "ToolFileChange"),
     ("requester-1", "node-1", "ToolFileInventory"),
     ("requester-\"quoted", "node-\\path", "ToolFileInventory")].map
      fun (requester, owner, image) =>
        { direction, requester, owner, demandCollection := "FileInventoryRequest",
          imageCollection := image }
  model := fun i =>
    let filters := ScopeTemplates.clientRouteFilters i.direction i.requester i.owner
    { expected := filters.find? (fun predicate => predicate.collection == i.demandCollection)
      imagesGranted := (ScopeTemplates.clientRouteCollections i.direction).contains i.imageCollection
      imageFilterInstalled := filters.any (fun predicate => predicate.collection == i.imageCollection) }

end Conformance
