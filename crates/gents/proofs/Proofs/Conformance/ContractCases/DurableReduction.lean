import Proofs.Conformance.ContractCases.Types
import Proofs.Compaction.DurableReduction

namespace Conformance.ContractCases

open Compaction.DurableReduction

structure DurableReductionCase where
  name : String
  requestDocId : Nat
  turnIndex : Nat
  ordinal : Nat
  checkpoint : Nat
  claimCommit : Nat
  priorCheckpoint : Option Nat
  priorClaimCommit : Option Nat
  pairClosed : Bool
  inferenceCites : Bool
  inferenceSupported : Bool
  titleCites : Bool
  consumed : Bool
  outcome : String
  durableAfter : Bool
  sendPermitted : Bool
  deriving Repr

private def key (requestDocId turnIndex ordinal : Nat) : ReductionKey :=
  { agentDid := 7, sessionId := 11, requestDocId, turnIndex, ordinal }

private def fact (checkpoint claimCommit : Nat) (pairClosed : Bool) : Fact :=
  { claimCommit
  , sourceBoundary := { value := 41 }
  , sourceProjection := { value := 42 }
  , checkpoint := { value := checkpoint }
  , producerCall := some 99
  , parent := none
  , pairClosed
  }

private def reductionCase (name : String) (requestDocId turnIndex ordinal checkpoint claimCommit : Nat)
    (priorCheckpoint priorClaimCommit : Option Nat)
    (pairClosed inferenceCites inferenceSupported titleCites : Bool) :
    DurableReductionCase :=
  let reductionKey := key requestDocId turnIndex ordinal
  let scenario : Scenario :=
    { key := reductionKey
    , fact := fact checkpoint claimCommit pairClosed
    , prior := priorCheckpoint.map (fun prior => fact prior (priorClaimCommit.getD claimCommit) pairClosed)
    }
  let captures : List CaptureCitation :=
    (if inferenceCites then
      [{ kind := .inference, supported := inferenceSupported, reductionKeys := [reductionKey] }]
    else []) ++
    (if titleCites then
      [{ kind := .title, supported := true, reductionKeys := [reductionKey] }]
    else [])
  { name, requestDocId, turnIndex, ordinal, checkpoint, claimCommit, priorCheckpoint,
    priorClaimCommit, pairClosed, inferenceCites, inferenceSupported, titleCites
  , outcome := scenario.outcome.toContract
  , durableAfter := scenario.durableAfter
  , sendPermitted := scenario.sendPermitted
  , consumed := consumedBy reductionKey captures
  }

def durableReductionCases : List DurableReductionCase :=
  [ reductionCase "fresh_reduction_is_durable_before_send" 23 0 1 100 51 none none true false true false
  , reductionCase "identical_redelivery_is_idempotent" 23 0 1 100 51 (some 100) (some 51) true false true true
  , reductionCase "conflicting_rebinding_blocks_send" 23 0 1 100 51 (some 101) (some 51) true true true false
  , reductionCase "pair_open_checkpoint_blocks_send" 23 0 1 100 51 none none false false true false
  , reductionCase "later_claim_can_create_next_ordered_fact" 23 4 2 102 52 none none true false true false
  , reductionCase "concurrent_request_is_a_distinct_fact" 24 0 1 103 51 none none true false true false
  , reductionCase "unsupported_inference_is_not_consumption_evidence" 25 0 1 104 51 none none true true false false
  ]

theorem durableReductionCases_pinned :
    durableReductionCases.map
      (fun row => (row.name, row.outcome, row.durableAfter, row.sendPermitted)) =
      [ ("fresh_reduction_is_durable_before_send", "fresh", true, true)
      , ("identical_redelivery_is_idempotent", "idempotent", true, true)
      , ("conflicting_rebinding_blocks_send", "conflict", false, false)
      , ("pair_open_checkpoint_blocks_send", "pair_open", false, false)
      , ("later_claim_can_create_next_ordered_fact", "fresh", true, true)
      , ("concurrent_request_is_a_distinct_fact", "fresh", true, true)
      , ("unsupported_inference_is_not_consumption_evidence", "fresh", true, true)
      ] := by
  rfl

theorem durableReductionCases_no_fail_open :
    durableReductionCases.all
      (fun row => !row.sendPermitted || (row.durableAfter && row.pairClosed)) = true := by
  rfl

theorem durableReductionCases_consumption_is_inference_only :
    durableReductionCases.map (fun row => (row.name, row.consumed)) =
      [ ("fresh_reduction_is_durable_before_send", false)
      , ("identical_redelivery_is_idempotent", false)
      , ("conflicting_rebinding_blocks_send", true)
      , ("pair_open_checkpoint_blocks_send", false)
      , ("later_claim_can_create_next_ordered_fact", false)
      , ("concurrent_request_is_a_distinct_fact", false)
      , ("unsupported_inference_is_not_consumption_evidence", false)
      ] := by
  rfl

/-- Each case invokes the existing immutable Fact owner after preparing one
full-input rewrite. The prior fact, when present, is part of the historical
lineage even if an inference capture has consumed its active checkpoint. -/
structure DurableFullInputRewriteCase where
  name : String
  key : ReductionKey
  lineage : List ReductionKey
  prior : Option (ReductionKey × Fact)
  observations : List SourceObservation
  fact : Fact
  captures : List CaptureCitation
  priorConsumed : Bool
  outcome : String
  retired : List PromptAssembly.ClaudeMap.ReplayTag
  deriving Repr

private def rewriteTag (turn : Nat) : PromptAssembly.ClaudeMap.ReplayTag :=
  { request := 23, source := .provider 2 turn 1 }

private def rewriteRow (turn : Nat) : PromptAssembly.ClaudeMap.TaggedReplayRow :=
  { source := some (rewriteTag turn), physicalHeader := some ("header-" ++ toString turn),
    blockIndices := [0], blocks := [.reasoning none
      [.text [UInt8.ofNat turn] (some ("signature-" ++ toString turn))]] }

private def rewriteObservation (turn : Nat) : SourceObservation :=
  { tag := rewriteTag turn, agentDid := 7, sessionId := 11,
    sourceBoundary := { value := 41 } }

private def rewriteProjection (value : Nat)
    (rows : Option (List PromptAssembly.ClaudeMap.TaggedReplayRow))
    (retired : List PromptAssembly.ClaudeMap.ReplayTag := []) : Projection :=
  { value, taggedRows := rows, retired }

private def rewriteFact (source post : Projection) (pairClosed : Bool := true) : Fact :=
  { claimCommit := 51, sourceBoundary := { value := 41 },
    sourceProjection := source, checkpoint := post,
    producerCall := some 99, parent := none, pairClosed }

private def fullInputRewriteCase (name : String) (key : ReductionKey)
    (prior : Option (ReductionKey × Fact)) (lineage : List ReductionKey)
    (observations : List SourceObservation) (fact : Fact)
    (captures : List CaptureCitation := []) : DurableFullInputRewriteCase :=
  let store := match prior with
    | none => Store.empty
    | some (priorKey, priorFact) => Store.bind Store.empty priorKey priorFact
  let priorConsumed := match prior with
    | none => false
    | some (priorKey, _) => consumedBy priorKey captures
  match persistFullInput store lineage key observations fact with
  | .error error =>
      { name, key, prior, lineage, observations, fact, captures, priorConsumed,
        outcome := error.toContract,
        retired := [] }
  | .ok (result, updated) =>
      { name, key, prior, lineage, observations, fact, captures, priorConsumed,
        outcome := result.toContract,
        retired := retirementFrontier updated (lineage ++ [key]) }

def durableFullInputRewriteCases : List DurableFullInputRewriteCase :=
  let currentKey := key 23 2 1
  let priorKey := key 23 1 1
  let first := rewriteRow 1
  let second := rewriteRow 2
  let older := rewriteTag 0
  let priorFact := rewriteFact
    (rewriteProjection 80 (some []))
    (rewriteProjection 81 (some []) [older])
  let source := rewriteProjection 100 (some [first, second])
  let post := rewriteProjection 101 (some [second])
  let observations := [rewriteObservation 1, rewriteObservation 2]
  [ fullInputRewriteCase "full-input-rewrite-retires-retained-tail" currentKey none []
      observations (rewriteFact source post)
  , fullInputRewriteCase "consumed-prior-fact-still-retired" currentKey
      (some (priorKey, priorFact)) [priorKey] observations
      (rewriteFact { source with retired := [older] } post)
      [{ kind := .inference, supported := true, reductionKeys := [priorKey] }]
  , fullInputRewriteCase "unknown-source-association-rejected" currentKey none []
      observations (rewriteFact { source with taggedRows := none } post)
  , fullInputRewriteCase "duplicate-physical-source-rejected" currentKey none []
      observations (rewriteFact { source with taggedRows := some [first, first] } post)
  , fullInputRewriteCase "foreign-principal-observation-rejected" currentKey none []
      [{ rewriteObservation 1 with agentDid := 8 }, rewriteObservation 2]
      (rewriteFact source post)
  , fullInputRewriteCase "invented-post-source-rejected" currentKey none []
      observations (rewriteFact source
        { post with taggedRows := some [rewriteRow 3] })
  , fullInputRewriteCase "changed-physical-header-rejected" currentKey none []
      observations (rewriteFact source
        { post with taggedRows := some [{ second with physicalHeader := some "forged" }] })
  , fullInputRewriteCase "reindexed-retained-block-rejected" currentKey none []
      observations (rewriteFact source
        { post with taggedRows := some [{ second with blockIndices := [1] }] })
  , fullInputRewriteCase "changed-signed-reasoning-rejected" currentKey none []
      observations (rewriteFact source
        { post with taggedRows := some [{ second with blocks :=
          [.reasoning none [.text [UInt8.ofNat 2] (some "forged-signature")]] }] })
  , fullInputRewriteCase "stale-lineage-frontier-rejected" currentKey
      (some (priorKey, priorFact)) [priorKey] observations
      (rewriteFact source post)
  , fullInputRewriteCase "open-tool-pair-still-rejected" currentKey none []
      observations (rewriteFact source post false)
  ]

theorem durableFullInputRewriteCases_pinned :
    durableFullInputRewriteCases.map (fun row => (row.name, row.outcome)) =
    [ ("full-input-rewrite-retires-retained-tail", "fresh")
    , ("consumed-prior-fact-still-retired", "fresh")
    , ("unknown-source-association-rejected", "unknown_projection")
    , ("duplicate-physical-source-rejected", "invalid_sidecar")
    , ("foreign-principal-observation-rejected", "unbound_source")
    , ("invented-post-source-rejected", "invented_source")
    , ("changed-physical-header-rejected", "changed_association")
    , ("reindexed-retained-block-rejected", "changed_association")
    , ("changed-signed-reasoning-rejected", "changed_association")
    , ("stale-lineage-frontier-rejected", "stale_frontier")
    , ("open-tool-pair-still-rejected", "pair_open")
    ] := by
  native_decide

theorem durableFullInputRewriteCases_retirement_and_consumption :
    durableFullInputRewriteCases.map (fun row => (row.priorConsumed, row.retired)) =
    [ (false, [rewriteTag 1, rewriteTag 2])
    , (true, [rewriteTag 0, rewriteTag 1, rewriteTag 2])
    , (false, [])
    , (false, [])
    , (false, [])
    , (false, [])
    , (false, [])
    , (false, [])
    , (false, [])
    , (false, [])
    , (false, [])
    ] := by
  native_decide

structure DurableSessionRewriteAction where
  key : ReductionKey
  lineage : List ReductionKey
  observations : List SourceObservation
  fact : Fact
  cursor : Nat
  deriving Repr

structure DurableSessionRewriteCase where
  name : String
  actions : List DurableSessionRewriteAction
  outcomes : List String
  entries : List SessionCursorEntry
  storedKeys : List ReductionKey
  deriving Repr

private def sessionRewriteCase (name : String)
    (actions : List DurableSessionRewriteAction) : DurableSessionRewriteCase :=
  let initial : SessionRewriteState := { store := Store.empty, entries := [] }
  let (final, outcomes) := actions.foldl (fun (state, outcomes) action =>
    let (outcome, next) := commitSessionRewrite state action.lineage action.key
      action.observations action.fact action.cursor
    (next, outcomes ++ [outcome.toContract])) (initial, [])
  let keys := (actions.map (·.key)).eraseDups
  { name, actions, outcomes, entries := final.entries,
    storedKeys := keys.filter (fun key => (final.store key).isSome) }

def durableSessionRewriteCases : List DurableSessionRewriteCase :=
  let firstKey := key 23 2 1
  let laterKey := key 23 3 1
  let first := rewriteRow 1
  let second := rewriteRow 2
  let observations := [rewriteObservation 1, rewriteObservation 2]
  let source := rewriteProjection 100 (some [first, second])
  let post := rewriteProjection 101 (some [second])
  let firstFact := rewriteFact source post
  let firstAction : DurableSessionRewriteAction :=
    { key := firstKey, lineage := [], observations, fact := firstFact, cursor := 5 }
  let laterAction : DurableSessionRewriteAction :=
    { key := laterKey, lineage := [firstKey], observations,
      fact := rewriteFact { source with retired := [rewriteTag 1, rewriteTag 2] }
        { post with value := 102 }, cursor := 8 }
  let sameCursorAction := { laterAction with cursor := 5 }
  let conflictAction := { firstAction with
    fact := rewriteFact source { post with value := 103 } }
  let invalidAction := { firstAction with
    fact := rewriteFact { source with taggedRows := none } post }
  let pairOpenAction := { firstAction with fact := rewriteFact source post false }
  [ sessionRewriteCase "fresh-cursor-and-fact-commit-together" [firstAction]
  , sessionRewriteCase "same-pair-replay-is-idempotent" [firstAction, firstAction]
  , sessionRewriteCase "old-pair-replay-after-newer-cursor-is-idempotent"
      [firstAction, laterAction,
       { firstAction with lineage := [firstKey, laterKey] }]
  , sessionRewriteCase "same-cursor-new-fact-rejected" [firstAction, sameCursorAction]
  , sessionRewriteCase "conflicting-fact-does-not-move-cursor" [firstAction, conflictAction]
  , sessionRewriteCase "invalid-sidecar-commits-neither" [invalidAction]
  , sessionRewriteCase "open-pair-commits-neither" [pairOpenAction]
  ]

theorem durableSessionRewriteCases_pinned :
    durableSessionRewriteCases.map (fun row => (row.name, row.outcomes,
      row.entries.map SessionCursorEntry.cursor, row.storedKeys.length)) =
    [ ("fresh-cursor-and-fact-commit-together", ["fresh"], [5], 1)
    , ("same-pair-replay-is-idempotent", ["fresh", "idempotent"], [5], 1)
    , ("old-pair-replay-after-newer-cursor-is-idempotent",
        ["fresh", "fresh", "idempotent"], [5, 8], 2)
    , ("same-cursor-new-fact-rejected", ["fresh", "cursor_conflict"], [5], 1)
    , ("conflicting-fact-does-not-move-cursor", ["fresh", "fact_conflict"], [5], 1)
    , ("invalid-sidecar-commits-neither", ["unknown_projection"], [], 0)
    , ("open-pair-commits-neither", ["pair_open"], [], 0)
    ] := by
  native_decide

end Conformance.ContractCases
