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

def casesJson : String :=
  "{\"publications\":[" ++ String.intercalate "," (publicationCases.map publicationJson) ++
    "],\"reuse\":[" ++ String.intercalate "," (reuseCases.map reuseJson) ++ "]}"

end Conformance.RequestPatchPublicationCases
