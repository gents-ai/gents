import Proofs.Retrieval.Theorems

namespace Retrieval.ContractCases

open Retrieval

structure IndexCase where
  name : String
  messages : List Message
  state : SessionIndex
  step : Step
  pendingAfter : Nat
  deriving Repr

def settled (seqs : List Nat) : List Message :=
  seqs.map fun s => { sequence := s, settled := true }

def mkIndex (name : String) (messages : List Message) (state : SessionIndex) : IndexCase :=
  let result := Retrieval.step messages state
  { name := name
  , messages := messages
  , state := state
  , step := result
  , pendingAfter := pending messages result.next }

def indexCases : List IndexCase :=
  [ mkIndex "empty_session_is_idle" [] SessionIndex.initial
  , mkIndex "backfill_indexes_every_settled_message" (settled [1, 2, 3]) SessionIndex.initial
  , mkIndex "in_flight_response_waits"
      (settled [1, 2] ++ [{ sequence := 3, settled := false }]) SessionIndex.initial
  , mkIndex "settled_response_is_indexed_on_next_pass" (settled [1, 2, 3])
      { indexed := [1, 2], watermark := 3 }
  , mkIndex "gap_holds_watermark_but_not_later_messages" (settled [1, 2, 4])
      SessionIndex.initial
  , mkIndex "arrived_gap_releases_watermark" (settled [1, 2, 3, 4])
      { indexed := [1, 2, 4], watermark := 3 }
  , mkIndex "resumed_backfill_starts_at_watermark" (settled [1, 2, 3, 4, 5])
      { indexed := [1, 2], watermark := 3 }
  , mkIndex "deleted_message_drops_its_chunks" (settled [1, 3])
      { indexed := [1, 2, 3], watermark := 4 }
  , mkIndex "deleted_session_drops_everything" []
      { indexed := [1, 2], watermark := 3 }
  , mkIndex "crash_after_chunks_replays_without_new_work" (settled [1, 2, 3])
      (crashedAfterChunks (settled [1, 2, 3]) SessionIndex.initial) ]

structure HitCase where
  name : String
  candidates : List Candidate
  authorized : List Nat
  limit : Nat
  hits : List Candidate
  deriving Repr

def mkHits (name : String) (candidates : List Candidate) (authorized : List Nat)
    (limit : Nat) : HitCase :=
  { name := name
  , candidates := candidates
  , authorized := authorized
  , limit := limit
  , hits := selectHits candidates authorized limit }

def hitCases : List HitCase :=
  [ mkHits "unmaterialized_sources_are_dropped"
      [⟨1, 90⟩, ⟨2, 80⟩, ⟨3, 70⟩] [1, 3] 10
  , mkHits "nearest_first_with_reference_ties"
      [⟨5, 40⟩, ⟨2, 60⟩, ⟨4, 60⟩, ⟨1, -10⟩] [1, 2, 4, 5] 10
  , mkHits "limit_keeps_the_nearest" [⟨1, 10⟩, ⟨2, 30⟩, ⟨3, 20⟩] [1, 2, 3] 2
  , mkHits "nothing_authorized_returns_nothing" [⟨1, 10⟩, ⟨2, 20⟩] [] 5 ]

end Retrieval.ContractCases
