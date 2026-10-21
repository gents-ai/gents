import Proofs.SessionHydration.FileInventoryCases
import Proofs.Conformance.Contracts.Json.SessionHydration
import Proofs.Conformance.Contracts.Json.NativeExecution

namespace Conformance.FileInventoryContracts

open SessionHydration SessionHydration.FileInventory
open SessionHydration.FileInventory.Examples
open Conformance.Contracts
open Conformance.ContractCases

private def object (fields : List (String × String)) : String :=
  "{" ++ String.intercalate "," (fields.map fun (key, value) => jsonString key ++ ":" ++ value) ++ "}"

private def optional {α : Type} (encode : α → String) : Option α → String
  | none => "null"
  | some value => encode value

private def tagged (kind : String) (fields : List (String × String) := []) : String :=
  object (("kind", jsonString kind) :: fields)

private def scopeJson (value : FileImages.Scope) : String := object
  [("requester", jsonString value.requester), ("node", jsonString value.node),
   ("session", jsonString value.session)]

private def failureName : ImageFailure → String
  | .readFailed => "read_failed"
  | .changedDuringRead => "changed_during_read"
  | .tooLarge => "too_large"
  | .unsupportedEntry => "unsupported_entry"
  | .mutationIncomplete => "mutation_incomplete"

private def imageJson : CapturedImage → String
  | .missing => tagged "missing"
  | .file hash data => tagged "file" [("content_hash", jsonString hash), ("data", jsonString data)]
  | .unavailable reason => tagged "unavailable" [("reason", jsonString (failureName reason))]

private def imageMetadataJson : ImageMetadata → String
  | .missing => tagged "missing"
  | .file hash => tagged "file" [("content_hash", jsonString hash)]
  | .unavailable reason => tagged "unavailable" [("reason", jsonString (failureName reason))]

private def capturedChangeJson (value : CapturedChange) : String := object
  [("root", jsonString value.root), ("path", jsonString value.path),
   ("before", imageJson value.before), ("after", imageJson value.after)]

private def changeJson (value : Change) : String := object
  [("root", jsonString value.root), ("path", jsonString value.path),
   ("before", imageMetadataJson value.before), ("after", imageMetadataJson value.after)]

private def observationJson (value : Observation) : String := object
  [("id", toString value.id), ("tool", toString value.tool), ("request", toString value.request),
   ("scope", scopeJson value.scope), ("changes", jsonArray (value.changes.map capturedChangeJson))]

private def projectionJson (value : Projection) : String := object
  [("id", toString value.id), ("observation", toString value.observation),
   ("tool", toString value.tool), ("request", toString value.request),
   ("scope", scopeJson value.scope), ("owner", jsonString value.owner),
   ("changes", jsonArray (value.changes.map changeJson))]

private def toolJson (value : Tool) : String := object
  [("id", toString value.id), ("request", toString value.request), ("scope", scopeJson value.scope),
   ("accepted", boolString value.accepted), ("state", jsonString value.state.toDefraDB)]

private def terminalSelectionJson : CanonicalOutput.TerminalSelection → String
  | .noMessage => tagged "no_message"
  | .message id => tagged "message" [("id", toString id)]

/-- Serialization preserves every closure input field. Raw effects remain
unselected inputs to the canonical owner rather than normalized output. -/
private def closureJson (value : ClosureInput) : String := object
  [("request", hydrationRequestJson value.request),
   ("bases", jsonArray (value.bases.map (hydrationDocumentKeyJson ·.key))),
   ("roots", jsonArray (value.roots.map toString)),
   ("requirements", jsonArray (value.requirements.map fun row => object
      [("request", toString row.request), ("session", toString row.session),
       ("selection", optional terminalSelectionJson row.selection)])),
   ("messages", jsonArray (value.messages.map canonicalMessageJson)),
   ("segments", jsonArray (value.segments.map canonicalSegmentJson)),
   ("access", jsonArray (value.access.map hydrationAccessJson)),
   ("denied_headers", jsonArray (value.deniedHeaders.map toString)),
   ("denied_segments", jsonArray (value.deniedSegments.map toString)),
   ("dependency_denials", jsonArray (value.dependencyDenials.map fun row => object
      [("root_close_id", toString row.rootCloseId), ("denied_doc_id", toString row.deniedDocId)])),
   ("effects", jsonArray (value.effects.map hydrationEffectJson))]

private def catalogJson (value : CatalogInput) : String := object
  [("applied_pairing_routes", jsonArray (value.appliedPairingRoutes.map fun row => object
      [("peer", jsonString row.peer), ("requester", jsonString row.requester), ("node", jsonString row.node)])),
   ("selected_network", jsonString value.selectedNetwork),
   ("verified_active_memberships", jsonArray (value.verifiedActiveMemberships.map fun row => object
      [("network", jsonString row.network), ("member", jsonString row.member)])),
   ("sessions", jsonArray (value.sessions.map fun row => object
      [("session", jsonString row.session), ("native_session", toString row.nativeSession),
       ("requester", jsonString row.requester), ("node", jsonString row.node)])),
   ("closure_inputs", jsonArray (value.closureInputs.map closureJson))]

private def boundaryName : ReadBoundary → String
  | .atomicExhaustive => "atomic_exhaustive"
  | .incomplete => "incomplete"
  | .nonAtomic => "non_atomic"

private def sourceJson (value : SourceInput) : String := object
  [("catalog", catalogJson value.catalog), ("tools", jsonArray (value.tools.map toolJson)),
   ("projections", jsonArray (value.projections.map projectionJson)),
   ("boundary", jsonString (boundaryName value.boundary))]

private def captureJson : Capture → String
  | .pending => tagged "pending"
  | .unknown => tagged "unknown"
  | .observed projection => tagged "observed" [("projection", projectionJson projection)]

private def snapshotJson (value : Snapshot) : String := object
  [("admission", hydrationRequestJson value.admission),
   ("documents", jsonArray (value.documents.map hydrationDocumentKeyJson)),
   ("tools", jsonArray (value.tools.map fun row => object
      [("tool", toolJson row.tool), ("capture", captureJson row.capture)]))]

private def selectionJson (source : SourceInput) (request : Request) : String :=
  match snapshot source.model request with
  | .error .rejected => tagged "rejected"
  | .error .unknown => tagged "unknown"
  | .ok value => tagged "selected" [("snapshot", snapshotJson value),
      ("known_empty", boolString value.knownEmpty)]

private def selectionCaseJson (value : SelectionCase) : String := object
  [("name", jsonString value.name), ("source", sourceJson value.source),
   ("admission", hydrationRequestJson admission), ("expected", selectionJson value.source admission)]

private def actionJson : ToolExecution.ToolCallContext.Action → String
  | .dispatch => tagged "dispatch"
  | .spawnFailed failure => tagged "spawn_failed" [("failure", jsonString failure.toDefraDB)]
  | .complete => tagged "complete"
  | .fail failure => tagged "fail" [("failure", jsonString failure.toDefraDB)]
  | .timeout => tagged "timeout"
  | .background => tagged "background"
  | .foreground => tagged "foreground"
  | .cancelBeforeDispatch cause => tagged "cancel_before_dispatch" [("cause", jsonString cause.toDefraDB)]
  | .cancelDuringRun cause => tagged "cancel_during_run" [("cause", jsonString cause.toDefraDB)]

private def publicationCaseJson (value : PublicationCase) : String := object
  [("name", jsonString value.name),
   ("pre", "{" ++ Conformance.NativeExecutionContracts.contextFieldsJson value.pre ++ "}"),
   ("action", actionJson value.action), ("won", boolString value.won),
   ("write", jsonString (terminalWriteString value.write)), ("actual", toolJson value.actual),
   ("observation", observationJson value.observation), ("projection_id", "3001"),
   ("expected", optional projectionJson
      (publish value.pre value.action value.won value.write value.actual value.observation 3001))]

private def entryJson (value : Entry) : String := object
  [("tool", toString value.tool), ("request", toString value.request), ("scope", scopeJson value.scope),
   ("projection", toString value.projection), ("observation", toString value.observation),
   ("change", changeJson value.change)]

private def cursorJson (value : Cursor) : String := object
  [("snapshot", snapshotJson value.snapshot), ("offset", toString value.offset)]

private def pageJson (value : Page) : String := object
  [("snapshot", snapshotJson value.snapshot), ("offset", toString value.offset),
   ("entries", jsonArray (value.entries.map entryJson)), ("next", optional cursorJson value.next)]

private def receiptJson : VerifiedReceipt → String
  | .transcriptV1 request documents => tagged "transcript_v1"
      [("admission", hydrationRequestJson request),
       ("documents", jsonArray (documents.map hydrationDocumentKeyJson))]
  | .metadata value => tagged "metadata" [("snapshot", snapshotJson value)]

private def pageResultJson : PageResult → String
  | .rejected => tagged "rejected"
  | .unknown => tagged "unknown"
  | .stale => tagged "stale"
  | .page value => tagged "page" [("page", pageJson value)]

structure PageInput where
  name : String
  source : SourceInput := large
  admission : Request := Examples.admission
  receipt : VerifiedReceipt := .metadata largeSnapshot
  cursor : Option Cursor := none

def pageInputs : List PageInput :=
  [ {name := "initial_bounded_page"}
  , {name := "continuation_page", cursor := some ⟨largeSnapshot, 64⟩}
  , {name := "wrong_cursor_offset", cursor := some ⟨largeSnapshot, 1⟩}
  , {name := "cursor_wrong_scope", cursor := some ⟨{largeSnapshot with admission := forkRequest}, 64⟩}
  , {name := "v1_is_not_metadata", receipt := .transcriptV1 admission largeSnapshot.documents}
  , {name := "late_terminal_behind_issued_cursor", source := lateAfter,
      receipt := .metadata lateSnapshot, cursor := some ⟨lateSnapshot, 64⟩}
  , {name := "membership_changed", source := Examples.source 66, cursor := some ⟨largeSnapshot, 64⟩}
  , { name := "projection_identity_changed"
    , source := { large with
        projections := large.projections.map (fun value => if value.tool = 1 then
          {value with observation := 9999} else value) }
    , cursor := some ⟨largeSnapshot, 64⟩ }
  , { name := "projection_content_changed"
    , source := { large with
        projections := large.projections.map (fun value => if value.tool = 1 then
          {value with changes := [{(change 1).metadata with path := "other.rs"}]} else value) } }
  , {name := "revoked_before_page", source := {large with catalog :=
      {large.catalog with verifiedActiveMemberships := []}}}
  , {name := "incomplete_read_cannot_certify", source := {large with boundary := .incomplete}}
  , {name := "valid_empty_inventory", source := source 0, receipt := .metadata (getSnapshot (source 0))}
  , {name := "legacy_unknown_coverage", source := {source 1 with projections := []},
      receipt := .metadata (getSnapshot {source 1 with projections := []})}
  , {name := "one_capture_exceeds_one_page", source := oneLargeCapture,
      receipt := .metadata (getSnapshot oneLargeCapture)} ]

private def pageInputJson (value : PageInput) : String := object
  [("name", jsonString value.name), ("source", sourceJson value.source),
   ("admission", hydrationRequestJson value.admission), ("receipt", receiptJson value.receipt),
   ("cursor", optional cursorJson value.cursor),
   ("expected", pageResultJson (page value.source.model value.admission value.receipt value.cursor))]

private def receiverJson (value : Receiver) : String := object
  [("snapshot", snapshotJson value.snapshot), ("entries", jsonArray (value.entries.map entryJson)),
   ("complete", boolString value.complete), ("known_inventory", boolString (knownInventory value))]

structure ReceiverInput where
  name : String
  state : Receiver := receiving
  page : Page := firstPage

def receiverInputs : List ReceiverInput :=
  [ {name := "first_page"}
  , {name := "cannot_skip_prefix", page := secondPage}
  , {name := "truncated_page", page := {firstPage with entries := firstPage.entries.take 1}}
  , {name := "corrupt_retained_prefix", state := {receiving with entries := firstPage.entries.reverse},
      page := secondPage}
  , {name := "complete_contiguous_inventory", state := ⟨largeSnapshot, firstPage.entries, false⟩,
      page := secondPage}
  , {name := "unknown_survives_complete_page", state := ⟨getSnapshot {source 1 with projections := []}, [], false⟩,
      page := makePage (getSnapshot {source 1 with projections := []}) 0} ]

private def receiverInputJson (value : ReceiverInput) : String := object
  [("name", jsonString value.name), ("state", receiverJson value.state), ("page", pageJson value.page),
   ("expected", optional receiverJson (receive value.state value.page))]

/-- IDs, DIDs and image hashes are the existing models' abstract names. Native
adapters bind them to actual Defra-created physical IDs, authenticated key
identities and validated captures through their existing decode owners. Empty
or malformed native IDs cannot become modeled observations; abstract hash
labels cannot bypass the capture decoder's byte/hash checks. These are
executable inputs/outputs, not a production wire-format proposal. -/
def casesJson : String := object
  [("selection", jsonArray (selectionCases.map selectionCaseJson)),
   ("publication", jsonArray (publicationCases.map publicationCaseJson)),
   ("fork", object [("source", sourceJson forkSource), ("admission", hydrationRequestJson forkRequest),
      ("expected", selectionJson forkSource forkRequest)]),
   ("paging", jsonArray (pageInputs.map pageInputJson)),
   ("receiver", jsonArray (receiverInputs.map receiverInputJson))]

end Conformance.FileInventoryContracts
