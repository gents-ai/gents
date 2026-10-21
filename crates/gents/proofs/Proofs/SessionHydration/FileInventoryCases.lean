import Proofs.SessionHydration.FileInventory

namespace SessionHydration.FileInventory.Examples

open ToolExecution

def admission : Request := ⟨"inventory-1", "peer-1", "reader-1", "node-1", "session-1", 2⟩
def scope : FileImages.Scope := ⟨admission.requester, admission.node, admission.session⟩

def tool (id : Nat) : Tool := ⟨id, 1000 + id, scope, true, .completed⟩
def change (id : Nat) : CapturedChange :=
  ⟨"workspace", "src/" ++ toString id ++ ".rs", .file "before" "old bytes",
    .file "after" "new bytes"⟩
def observation (id : Nat) : Observation :=
  ⟨2000 + id, id, (tool id).request, scope, [change id]⟩
def projection (id : Nat) : Projection := (observation id).metadata (3000 + id)

def input (ids : List Nat) : ClosureInput :=
  { request := admission, bases := ids.map (.toolCall ·), roots := [], requirements := []
  , messages := [], segments := []
  , access := ids.map fun id => ⟨admission.authorizationScope, ⟨.agentToolCall, id⟩, .authorized⟩
  , deniedHeaders := [], deniedSegments := [] }

structure CatalogInput where
  appliedPairingRoutes : List AppliedPairingRoute
  selectedNetwork : String
  verifiedActiveMemberships : List VerifiedActiveMembership
  sessions : List SessionOwner
  closureInputs : List ClosureInput

/-- List inputs retain their wire representation; the existing catalog owns
set membership and canonical closure interpretation. -/
def CatalogInput.model (input : CatalogInput) : Catalog :=
  ⟨input.appliedPairingRoutes.toFinset, input.selectedNetwork,
    input.verifiedActiveMemberships.toFinset, input.sessions.toFinset, input.closureInputs⟩

structure SourceInput where
  catalog : CatalogInput
  tools : List Tool
  projections : List Projection
  boundary : ReadBoundary

def SourceInput.model (input : SourceInput) : Source :=
  ⟨input.catalog.model, input.tools, input.projections, input.boundary⟩

def catalog (ids : List Nat) : CatalogInput :=
  { appliedPairingRoutes := [appliedPairingRoute admission]
  , selectedNetwork := "network-1"
  , verifiedActiveMemberships := [⟨"network-1", admission.requester⟩]
  , sessions := [ownedSession admission]
  , closureInputs := [input ids] }

def source (count : Nat) : SourceInput :=
  let ids := (List.range count).map (· + 1)
  ⟨catalog ids, ids.map tool, ids.map projection, .atomicExhaustive⟩

inductive Summary where
  | rejected
  | unknown
  | selected (entries pending unknown : Nat) (knownEmpty : Bool)
  deriving DecidableEq, Repr

def summary (value : SourceInput) (request : Request := admission) : Summary :=
  match snapshot value.model request with
  | .error .rejected => .rejected
  | .error .unknown => .unknown
  | .ok value => .selected value.entries.length value.pending.length value.unknown.length value.knownEmpty

structure SelectionCase where
  name : String
  source : SourceInput := source 1
  expected : Summary := .rejected

def selectionCases : List SelectionCase :=
  [ ⟨"accepted_terminal_capture", source 1, .selected 1 0 0 false⟩
  , ⟨"valid_empty_closure", source 0, .selected 0 0 0 true⟩
  , ⟨"legacy_missing_capture_is_unknown", {source 1 with projections := []}, .selected 0 0 1 false⟩
  , ⟨"active_missing_capture_is_pending",
      {source 1 with tools := [{tool 1 with state := .running}], projections := []}, .selected 0 1 0 false⟩
  , ⟨"missing_accepted_binding", {source 1 with tools := []}, .unknown⟩
  , ⟨"incomplete_database_enumeration", {source 1 with boundary := .incomplete}, .unknown⟩
  , ⟨"mixed_revision_database_reads", {source 1 with boundary := .nonAtomic}, .unknown⟩
  , ⟨"missing_closure_input", {source 1 with catalog := {catalog [1] with closureInputs := []}}, .unknown⟩
  , ⟨"ambiguous_closure_input", {source 1 with catalog :=
        {catalog [1] with closureInputs := [input [1], input [1]]}}, .unknown⟩
  , ⟨"canonical_missing_provenance", {source 1 with catalog :=
        {catalog [1] with closureInputs := [{input [1] with access := []}]}}, .unknown⟩
  , ⟨"canonical_denied_provenance", {source 1 with catalog :=
        {catalog [1] with closureInputs := [{input [1] with access :=
          [⟨admission.authorizationScope, ⟨.agentToolCall, 1⟩, .denied⟩]}]}}, .rejected⟩
  , ⟨"revoked_membership", {source 1 with catalog :=
        {catalog [1] with verifiedActiveMemberships := []}}, .rejected⟩
  , ⟨"missing_applied_route", {source 1 with catalog :=
        {catalog [1] with appliedPairingRoutes := []}}, .rejected⟩
  , ⟨"foreign_session_owner", {source 1 with catalog :=
        {catalog [1] with sessions := [⟨admission.session, 2, "foreign", admission.node⟩]}}, .rejected⟩
  , ⟨"physical_binding_not_accepted", {source 1 with tools := [{tool 1 with accepted := false}]}, .rejected⟩
  , ⟨"duplicate_physical_tool", {source 1 with tools := [tool 1, tool 1]}, .rejected⟩
  , ⟨"duplicate_capture_projection", {source 1 with projections := [projection 1, projection 1]}, .rejected⟩
  , ⟨"uncommitted_capture_before_terminal", {source 1 with tools := [{tool 1 with state := .running}]}, .rejected⟩
  , ⟨"projection_wrong_request", {source 1 with projections := [{projection 1 with request := 99}]}, .rejected⟩
  , ⟨"projection_wrong_actual_session", {source 1 with projections :=
        [{projection 1 with scope := {scope with session := "foreign"}}]}, .rejected⟩
  , ⟨"projection_wrong_actual_requester", {source 1 with projections :=
        [{projection 1 with scope := {scope with requester := "foreign"}}]}, .rejected⟩
  , ⟨"projection_wrong_actual_node", {source 1 with projections :=
        [{projection 1 with scope := {scope with node := "foreign"}}]}, .rejected⟩
  , ⟨"projection_foreign_stored_owner", {source 1 with projections :=
        [{projection 1 with owner := "foreign"}]}, .rejected⟩
  , ⟨"invalid_empty_capture_not_empty_inventory", {source 1 with projections :=
        [{projection 1 with changes := []}]}, .rejected⟩
  , ⟨"duplicate_paths_in_capture", {source 1 with projections :=
        [{projection 1 with changes := [(change 1).metadata, (change 1).metadata]}]}, .rejected⟩
  , ⟨"projection_id_reused_across_tools", {source 2 with projections :=
        [projection 1, {projection 2 with id := (projection 1).id}]}, .rejected⟩
  , ⟨"observation_id_reused_across_tools", {source 2 with projections :=
        [projection 1, {projection 2 with observation := (projection 1).observation}]}, .rejected⟩
  , ⟨"unselected_tools_do_not_expand_inventory",
        {source 1 with tools := [tool 1, tool 99], projections := [projection 1, projection 99]},
        .selected 1 0 0 false⟩
  , ⟨"canonical_membership_order_is_stable",
        { source 2 with
          catalog := catalog [2, 1]
          tools := [tool 2, tool 1]
          projections := [projection 2, projection 1] }, .selected 2 0 0 false⟩ ]

theorem selection_regressions : selectionCases.all
    (fun test => summary test.source == test.expected) = true := by native_decide

def running : ToolCallContext :=
  { callId := 1, requestId := (tool 1).request, state := .running, operation := .nativeCommand
  , deadline := 10, currentTime := 11, persistence := .committed }

structure PublicationCase where
  name : String
  pre : ToolCallContext := running
  action : ToolCallContext.Action
  won : Bool := true
  write : TerminalWriteResult := .committed
  actual : Tool := {tool 1 with state := .running}
  observation : Observation := observation 1
  expected : Bool

def publicationCases : List PublicationCase :=
  [ {name := "completed_retains_capture", action := .complete, expected := true}
  , {name := "failed_retains_partial_capture", action := .fail .external, expected := true}
  , {name := "timeout_retains_partial_capture", action := .timeout, expected := true}
  , {name := "cancelled_retains_partial_capture", action := .cancelDuringRun .userCancelled, expected := true}
  , {name := "lost_terminal_cas_does_not_publish", action := .complete, won := false, expected := false}
  , {name := "rollback_does_not_publish", action := .complete, write := .failed, expected := false}
  , {name := "unattempted_write_does_not_publish", action := .complete, write := .notAttempted, expected := false}
  , {name := "nonterminal_action_does_not_publish", action := .background, expected := false}
  , {name := "terminal_replay_does_not_publish", pre := {running with state := .completed},
      action := .complete, expected := false}
  , {name := "capture_wrong_actual_session", action := .complete,
      observation := {observation 1 with scope := {scope with session := "foreign"}}, expected := false}
  , {name := "capture_wrong_actual_node", action := .complete,
      observation := {observation 1 with scope := {scope with node := "foreign"}}, expected := false}
  , {name := "capture_wrong_actual_requester", action := .complete,
      observation := {observation 1 with scope := {scope with requester := "foreign"}}, expected := false}
  , {name := "capture_different_physical_tool", action := .complete,
      actual := {tool 2 with state := .running}, expected := false}
  , {name := "capture_unaccepted_tool", action := .complete,
      actual := {tool 1 with state := .running, accepted := false}, expected := false} ]

theorem publication_regressions : publicationCases.all (fun test =>
    (publish test.pre test.action test.won test.write test.actual test.observation 3001).isSome ==
      test.expected) = true := by native_decide

def partialObservation : Observation :=
  {observation 1 with changes := [{change 1 with after := .unavailable .mutationIncomplete}]}

theorem cancellation_preserves_unavailable_after_image :
    publish running (.cancelDuringRun .userCancelled) true .committed {tool 1 with state := .running} partialObservation 3001 =
      some (partialObservation.metadata 3001) := by native_decide

def forkRequest : Request := {admission with session := "fork-session", nativeSession := 3}
def originDelivery := CanonicalOutput.Hydration.Examples.parentDelivery
def forkDelivery : CanonicalOutput.MessageEnvelope :=
  { originDelivery with
    header := CanonicalOutput.forkHeader originDelivery.header 221 3
    key := "fork-delivery" }
def forkInput : ClosureInput :=
  { input [] with
    request := forkRequest
    roots := [221]
    messages := [originDelivery, forkDelivery]
    segments := [CanonicalOutput.Hydration.Examples.parentOutput]
    access := [⟨.agentRequest, 10⟩, ⟨.agentMessage, 220⟩, ⟨.agentMessage, 221⟩,
      ⟨.agentToolCall, 300⟩, ⟨.agentOutputSegment, 301⟩].map fun key =>
        ⟨forkRequest.authorizationScope, key, .authorized⟩ }
def forkSource : SourceInput :=
  let originTool : Tool := ⟨300, 10, scope, true, .completed⟩
  let retained := {(projection 300) with request := 10}
  { source 0 with
    tools := [originTool, tool 99]
    projections := [retained, projection 99]
    catalog := { catalog [] with
      sessions := [ownedSession forkRequest]
      closureInputs := [forkInput] } }

theorem fork_retains_only_authorized_physical_origin :
    summary forkSource forkRequest = .selected 1 0 0 false := by native_decide

def getSnapshot (value : SourceInput) : Snapshot :=
  (snapshot value.model admission).toOption.getD ⟨admission, [], []⟩

def large : SourceInput := source 65
def largeSnapshot : Snapshot := getSnapshot large
def firstPage : Page := makePage largeSnapshot 0
def secondPage : Page := makePage largeSnapshot 64

theorem pages_flatten_then_bound : firstPage.entries.length = 64 ∧
    secondPage.entries.length = 1 ∧ firstPage.next = some ⟨largeSnapshot, 64⟩ ∧
    secondPage.next = none := by native_decide

def oneLargeCapture : SourceInput :=
  {source 1 with projections := [{projection 1 with
    changes := (List.range 65).map (fun id => (change id).metadata)}]}

theorem one_large_capture_still_pages_entries :
    (makePage (getSnapshot oneLargeCapture) 0).entries.length = 64 ∧
    (makePage (getSnapshot oneLargeCapture) 64).entries.length = 1 := by native_decide

def lateAfter : SourceInput := source 66
def lateBefore : SourceInput :=
  { lateAfter with
    tools := lateAfter.tools.map (fun value => if value.id = 1 then
      {value with state := .running} else value)
    projections := lateAfter.projections.filter (fun value => value.tool != 1) }
def lateSnapshot : Snapshot := getSnapshot lateBefore

theorem old_cursor_was_legitimately_issued :
    (makePage lateSnapshot 0).next = some ⟨lateSnapshot, 64⟩ := by native_decide

theorem old_terminal_behind_cursor_is_stale :
    page lateAfter.model admission (.metadata lateSnapshot) (some ⟨lateSnapshot, 64⟩) = .stale := by
  native_decide

theorem membership_change_is_stale :
    page (source 66).model admission (.metadata largeSnapshot) (some ⟨largeSnapshot, 64⟩) = .stale := by
  native_decide

theorem metadata_change_is_stale :
    page ({large with projections := large.projections.map (fun value =>
      if value.tool = 1 then {value with observation := 9999} else value)} : SourceInput).model
      admission (.metadata largeSnapshot) (some ⟨largeSnapshot, 64⟩) = .stale := by native_decide

theorem v1_receipt_is_not_upgraded_to_snapshot :
    page large.model admission (.transcriptV1 admission largeSnapshot.documents) none = .stale := by
  native_decide

theorem cursor_scope_cannot_be_replayed :
    page large.model admission (.metadata largeSnapshot)
      (some ⟨{largeSnapshot with admission := forkRequest}, 64⟩) = .stale := by native_decide

theorem cursor_cannot_skip_partial_page :
    page large.model admission (.metadata largeSnapshot) (some ⟨largeSnapshot, 1⟩) = .rejected := by
  native_decide

def receiving : Receiver := ⟨largeSnapshot, [], false⟩
def receivedBoth : Option Receiver := do
  let state ← receive receiving firstPage
  receive state secondPage

theorem receiver_requires_contiguous_prefix : receive receiving secondPage = none := by native_decide
theorem receiver_rejects_truncated_signed_page :
    receive receiving {firstPage with entries := firstPage.entries.take 1} = none := by native_decide
theorem receiver_completes_exact_inventory : receivedBoth =
    some ⟨largeSnapshot, largeSnapshot.entries, true⟩ := by native_decide
theorem receiver_rejects_corrupt_retained_prefix :
    receive {receiving with entries := (firstPage.entries.reverse)} secondPage = none := by native_decide
theorem unknown_capture_survives_end_of_pages :
    let value := getSnapshot {source 1 with projections := []}
    (receive ⟨value, [], false⟩ (makePage value 0)).map knownInventory = some false := by
  native_decide

theorem physical_input_reordering_preserves_snapshot :
    (snapshot ({ source 2 with
      catalog := catalog [2, 1]
      tools := [tool 2, tool 1]
      projections := [projection 2, projection 1] } : SourceInput).model admission).toOption =
      (snapshot (source 2).model admission).toOption := by native_decide

end SessionHydration.FileInventory.Examples
