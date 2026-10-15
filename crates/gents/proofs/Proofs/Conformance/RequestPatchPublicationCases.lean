import Proofs.ClientShell.RequestPatchPublication

namespace Conformance.RequestPatchPublicationCases

open ClientShell.RequestPatchPublication

def stateJson : CacheState → String
  | .empty => "empty"
  | .memoized => "memoized"

def publicationCases : List (String × CacheState × Bool) :=
  [ ("rejected_empty_publication", .empty, false)
  , ("accepted_empty_publication", .empty, true)
  , ("rejected_retry_preserves_existing_cache", .memoized, false)
  ]

def publicationJson (entry : String × CacheState × Bool) : String :=
  let (name, before, accepted) := entry
  "{\"name\":\"" ++ name ++ "\",\"before\":\"" ++ stateJson before ++
    "\",\"accepted\":" ++ toString accepted ++
    ",\"expected_after\":\"" ++ stateJson (publish before accepted) ++ "\"}"

def reuseCases : List (String × Bool × Bool × Bool × Bool) :=
  [ ("current_cache_reuses", true, true, true, true)
  , ("rejected_publication_not_cached", false, true, true, true)
  , ("different_signature_requires_read", true, false, true, true)
  , ("stale_read_fence_requires_read", true, true, false, true)
  , ("changed_physical_rows_require_read", true, true, true, false)
  ]

def reuseJson (entry : String × Bool × Bool × Bool × Bool) : String :=
  let (name, memoized, sameSignature, fenceAccepted, rowsCurrent) := entry
  let state := if memoized then CacheState.memoized else CacheState.empty
  "{\"name\":\"" ++ name ++ "\",\"memoized\":" ++ toString memoized ++
    ",\"same_signature\":" ++ toString sameSignature ++
    ",\"fence_accepted\":" ++ toString fenceAccepted ++
    ",\"rows_current\":" ++ toString rowsCurrent ++
    ",\"expected_reuse\":" ++ toString (reusable state sameSignature fenceAccepted rowsCurrent) ++ "}"

def freshReadActionJson : FreshReadAction → String
  | .mergeFresh => "merge_fresh"
  | .rejectCandidate => "reject_candidate"
  | .noPatch => "no_patch"

def freshReadCases : List (String × Bool × Bool) :=
  [ ("cache_miss_current_fence_publishes_fresh_patch", true, true)
  , ("cache_miss_stale_fence_rejects_fresh_patch", false, true)
  , ("missing_fresh_patch_has_no_publication", true, false)
  ]

def freshReadJson (entry : String × Bool × Bool) : String :=
  let (name, fenceAccepted, patchAvailable) := entry
  "{\"name\":\"" ++ name ++
    "\",\"fence_accepted\":" ++ toString fenceAccepted ++
    ",\"patch_available\":" ++ toString patchAvailable ++
    ",\"expected_action\":\"" ++ freshReadActionJson
      (freshReadAction fenceAccepted patchAvailable) ++ "\"}"

def casesJson : String :=
  "{\"publications\":[" ++ String.intercalate "," (publicationCases.map publicationJson) ++
    "],\"reuse\":[" ++ String.intercalate "," (reuseCases.map reuseJson) ++
    "],\"fresh_read\":[" ++ String.intercalate "," (freshReadCases.map freshReadJson) ++ "]}"

end Conformance.RequestPatchPublicationCases
