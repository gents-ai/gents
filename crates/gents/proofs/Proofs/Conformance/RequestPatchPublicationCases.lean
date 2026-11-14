import Proofs.Clients.ClientShell.RequestPatchPublication
import Proofs.Conformance.Family

namespace Conformance.RequestPatchPublicationCases

open Lean ClientShell.RequestPatchPublication

deriving instance ToJson for CacheState, FreshReadAction

structure PublicationInput where
  name : String
  before : CacheState
  accepted : Bool
  deriving ToJson

structure PublicationOutput where
  after : CacheState
  deriving ToJson

@[conformance_family]
def publications : Family PublicationInput PublicationOutput where
  inputs :=
    [ { name := "rejected_empty_publication", before := .empty, accepted := false }
    , { name := "accepted_empty_publication", before := .empty, accepted := true }
    , { name := "rejected_retry_preserves_existing_cache", before := .memoized, accepted := false } ]
  model := fun i => { after := publish i.before i.accepted }

structure ReuseInput where
  name : String
  memoized : Bool
  sameSignature : Bool
  fenceAccepted : Bool
  rowsCurrent : Bool
  deriving ToJson

structure ReuseOutput where
  reuse : Bool
  deriving ToJson

@[conformance_family]
def reuse : Family ReuseInput ReuseOutput where
  inputs :=
    [ { name := "current_cache_reuses", memoized := true, sameSignature := true,
        fenceAccepted := true, rowsCurrent := true }
    , { name := "rejected_publication_not_cached", memoized := false, sameSignature := true,
        fenceAccepted := true, rowsCurrent := true }
    , { name := "different_signature_requires_read", memoized := true, sameSignature := false,
        fenceAccepted := true, rowsCurrent := true }
    , { name := "stale_read_fence_requires_read", memoized := true, sameSignature := true,
        fenceAccepted := false, rowsCurrent := true }
    , { name := "changed_physical_rows_require_read", memoized := true, sameSignature := true,
        fenceAccepted := true, rowsCurrent := false } ]
  model := fun i =>
    let state := if i.memoized then CacheState.memoized else CacheState.empty
    { reuse := reusable state i.sameSignature i.fenceAccepted i.rowsCurrent }

structure FreshReadInput where
  name : String
  fenceAccepted : Bool
  patchAvailable : Bool
  deriving ToJson

structure FreshReadOutput where
  action : FreshReadAction
  deriving ToJson

@[conformance_family]
def freshRead : Family FreshReadInput FreshReadOutput where
  inputs :=
    [ { name := "cache_miss_current_fence_publishes_fresh_patch", fenceAccepted := true,
        patchAvailable := true }
    , { name := "cache_miss_stale_fence_rejects_fresh_patch", fenceAccepted := false,
        patchAvailable := true }
    , { name := "missing_fresh_patch_has_no_publication", fenceAccepted := true,
        patchAvailable := false } ]
  model := fun i => { action := freshReadAction i.fenceAccepted i.patchAvailable }

end Conformance.RequestPatchPublicationCases
