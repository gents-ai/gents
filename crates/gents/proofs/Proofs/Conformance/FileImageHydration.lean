import Proofs.SessionHydration.FileImages
import Proofs.ScopeTemplates
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.FileImageHydrationContracts
open SessionHydration SessionHydration.FileImages Conformance.Contracts

private def admission : Request :=
  ⟨"image-demand", "peer-1", "requester-1", "node-1", "session-1", 0⟩
private def demand : Demand := ⟨admission, "request-document-1", "tool-document-1"⟩
private def scope : Scope := scopeFor demand
private def tool : Tool := ⟨demand.toolDocId, demand.requestDocId, scope, true, true⟩
private def content : Content := ⟨"file-document-1", demand.toolDocId, scope⟩
private def image : Image := ⟨"image-document-1", demand.toolDocId, demand.requestDocId, scope, [content]⟩
structure AdmissionFacts where
  appliedPairingRoutes : List AppliedPairingRoute
  selectedNetwork : String
  verifiedActiveMemberships : List VerifiedActiveMembership
  sessions : List SessionOwner

def AdmissionFacts.toCatalog (facts : AdmissionFacts) : Catalog :=
  { appliedPairingRoutes := facts.appliedPairingRoutes.toFinset
  , selectedNetwork := facts.selectedNetwork
  , verifiedActiveMemberships := facts.verifiedActiveMemberships.toFinset
  , sessions := facts.sessions.toFinset
  , closureInputs := [] }

private def catalog : AdmissionFacts :=
  { appliedPairingRoutes := [appliedPairingRoute admission]
  , selectedNetwork := "network-1"
  , verifiedActiveMemberships := [⟨"network-1", admission.requester⟩]
  , sessions := [ownedSession admission] }

structure Case where
  name : String
  demand : Demand := demand
  catalog : AdmissionFacts := catalog
  tools : List Tool := [tool]
  images : List Image := [image]
  expected : Selection := .rejected

def cases : List Case :=
  [ {name := "exact_terminal_tool", expected := .selected image.docId}
  , {name := "missing_images_after_terminal", images := [], expected := .missing}
  , {name := "unpaired", catalog := {catalog with appliedPairingRoutes := []}}
  , {name := "wrong_pairing_requester", catalog := {catalog with appliedPairingRoutes :=
      [⟨admission.peer, "other-requester", admission.node⟩]}}
  , {name := "wrong_pairing_node", catalog := {catalog with appliedPairingRoutes :=
      [⟨admission.peer, admission.requester, "other-node"⟩]}}
  , {name := "membership_revoked", catalog := {catalog with verifiedActiveMemberships := []}}
  , {name := "membership_wrong_network", catalog := {catalog with selectedNetwork := "other-network"}}
  , {name := "session_not_owned", catalog := {catalog with sessions := []}}
  , {name := "empty_request_identity", demand := {demand with requestDocId := ""}}
  , {name := "empty_tool_identity", demand := {demand with toolDocId := ""}}
  , {name := "tool_missing", tools := []}
  , {name := "unaccepted_tool", tools := [{tool with accepted := false}]}
  , {name := "running_tool", tools := [{tool with terminal := false}]}
  , {name := "wrong_physical_tool", tools := [{tool with docId := "other-tool"}]}
  , {name := "tool_wrong_request", tools := [{tool with requestDocId := "other-request"}]}
  , {name := "tool_wrong_requester", tools := [{tool with scope := {scope with requester := "other-requester"}}]}
  , {name := "tool_wrong_node", tools := [{tool with scope := {scope with node := "other-node"}}]}
  , {name := "tool_wrong_session", tools := [{tool with scope := {scope with session := "other-session"}}]}
  , {name := "ambiguous_physical_tool", tools := [tool, {tool with accepted := false}]}
  , {name := "image_empty_identity", images := [{image with docId := ""}]}
  , {name := "image_wrong_request", images := [{image with requestDocId := "other-request"}]}
  , {name := "image_wrong_requester", images := [{image with scope := {scope with requester := "other-requester"}}]}
  , {name := "image_wrong_node", images := [{image with scope := {scope with node := "other-node"}}]}
  , {name := "image_wrong_session", images := [{image with scope := {scope with session := "other-session"}}]}
  , {name := "conflicting_image_observations", images := [image, {image with docId := "image-document-2"}]}
  , {name := "foreign_image_does_not_expand_selection", images := [image, {image with toolDocId := "other-tool"}],
      expected := .selected image.docId}
  , {name := "foreign_image_is_not_a_selected_snapshot", images := [{image with toolDocId := "other-tool"}],
      expected := .missing}
  , {name := "image_without_file_documents", images := [{image with contents := []}],
      expected := .selected image.docId}
  , {name := "file_document_wrong_session", images := [{image with contents :=
      [{content with scope := {scope with session := "other-session"}}]}]}
  , {name := "file_document_wrong_requester", images := [{image with contents :=
      [{content with scope := {scope with requester := "other-requester"}}]}]}
  , {name := "file_document_wrong_node", images := [{image with contents :=
      [{content with scope := {scope with node := "other-node"}}]}]}
  , {name := "file_document_other_tool", images := [{image with contents := [{content with toolDocId := "other-tool"}]}]}
  , {name := "file_document_empty_identity", images := [{image with contents := [{content with docId := ""}]}]}
  , {name := "one_foreign_file_document_rejects_all", images := [{image with contents :=
      [content, {content with docId := "file-document-2", scope := {scope with session := "other-session"}}]}]}
  , {name := "foreign_tool_does_not_expand_selection", tools := [tool, {tool with docId := "other-tool"}],
      expected := .selected image.docId}
  ]

theorem regression_selections : cases.all (fun c =>
    select c.demand c.catalog.toCatalog c.tools c.images == c.expected) = true := by decide

private def boolJson (value : Bool) := if value then "true" else "false"
private def scopeJson (value : Scope) :=
  "{\"requester_did\":" ++ jsonString value.requester ++
  ",\"node_did\":" ++ jsonString value.node ++
  ",\"session_id\":" ++ jsonString value.session ++ "}"
private def requestJson (value : Request) :=
  "{\"request_key\":" ++ jsonString value.key ++
  ",\"peer_id\":" ++ jsonString value.peer ++
  ",\"requester_did\":" ++ jsonString value.requester ++
  ",\"node_did\":" ++ jsonString value.node ++
  ",\"session_id\":" ++ jsonString value.session ++ "}"
private def toolJson (value : Tool) :=
  "{\"doc_id\":" ++ jsonString value.docId ++
  ",\"request_doc_id\":" ++ jsonString value.requestDocId ++
  ",\"scope\":" ++ scopeJson value.scope ++
  ",\"accepted\":" ++ boolJson value.accepted ++
  ",\"terminal\":" ++ boolJson value.terminal ++ "}"
private def imageJson (value : Image) :=
  "{\"doc_id\":" ++ jsonString value.docId ++
  ",\"tool_call_doc_id\":" ++ jsonString value.toolDocId ++
  ",\"request_doc_id\":" ++ jsonString value.requestDocId ++
  ",\"scope\":" ++ scopeJson value.scope ++
  ",\"contents\":" ++ jsonArray (value.contents.map fun content =>
    "{\"doc_id\":" ++ jsonString content.docId ++ ",\"tool_call_doc_id\":" ++ jsonString content.toolDocId ++ ",\"scope\":" ++ scopeJson content.scope ++ "}") ++ "}"
private def catalogJson (value : AdmissionFacts) :=
  "{\"routes\":" ++ jsonArray (value.appliedPairingRoutes.map fun route =>
    "{\"peer_id\":" ++ jsonString route.peer ++ ",\"requester_did\":" ++ jsonString route.requester ++
      ",\"node_did\":" ++ jsonString route.node ++ "}") ++
  ",\"network\":" ++ jsonString value.selectedNetwork ++
  ",\"memberships\":" ++ jsonArray (value.verifiedActiveMemberships.map fun membership =>
    "{\"network_id\":" ++ jsonString membership.network ++
      ",\"member_did\":" ++ jsonString membership.member ++ "}") ++
  ",\"sessions\":" ++ jsonArray (value.sessions.map fun session =>
    "{\"scope\":" ++ scopeJson ⟨session.requester, session.node, session.session⟩ ++
      ",\"native_session\":" ++ toString session.nativeSession ++ "}") ++ "}"
private def selectionJson : Selection → String
  | .rejected => "{\"kind\":\"rejected\"}"
  | .missing => "{\"kind\":\"missing\"}"
  | .selected document => "{\"kind\":\"selected\",\"document\":" ++ jsonString document ++ "}"
private def caseJson (c : Case) :=
  let selection := select c.demand c.catalog.toCatalog c.tools c.images
  let delivered := match selection with
    | .selected document => (c.images.find? (fun image : Image => image.docId == document)).map deliveryDocuments
    | _ => none
  "{\"name\":" ++ jsonString c.name ++
  ",\"native_session\":" ++ toString c.demand.admission.nativeSession ++
  ",\"demand\":{\"admission\":" ++ requestJson c.demand.admission ++
    ",\"request_doc_id\":" ++ jsonString c.demand.requestDocId ++
    ",\"tool_call_doc_id\":" ++ jsonString c.demand.toolDocId ++ "}" ++
  ",\"catalog\":" ++ catalogJson c.catalog ++
  ",\"tools\":" ++ jsonArray (c.tools.map toolJson) ++
  ",\"images\":" ++ jsonArray (c.images.map imageJson) ++
  ",\"expected\":" ++ selectionJson selection ++
  ",\"delivery_documents\":" ++ (delivered.map (jsonArray ∘ List.map jsonString)).getD "null" ++ "}"

def casesJson := jsonArray (cases.map caseJson)

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
  (ScopeTemplates.explicitDeliveryCollections ++ ScopeTemplates.transcriptCollections).map fun collection =>
    "{\"collection\":" ++ jsonString collection ++
    ",\"explicit_delivery\":" ++ boolJson (ScopeTemplates.explicitDeliveryCollections.contains collection) ++ "}")

private def deliveryJson : DeliveryResult → String
  | .confirmed => "\"confirmed\"" | .indeterminate => "\"indeterminate\""
private def writeJson : TerminalWriteResult → String
  | .committed => "\"committed\"" | .failed => "\"failed\"" | .notAttempted => "\"notAttempted\""
private def completionCaseJson (selection : Selection) (beforeDelivery afterDelivery : Bool)
    (delivery : DeliveryResult) (write : TerminalWriteResult) :=
  "{\"selection\":" ++ selectionJson selection ++
  ",\"before_delivery\":" ++ boolJson beforeDelivery ++
  ",\"after_delivery\":" ++ boolJson afterDelivery ++
  ",\"delivery\":" ++ deliveryJson delivery ++ ",\"write\":" ++ writeJson write ++
  ",\"target\":" ++ ((deliveryTarget selection beforeDelivery).map jsonString).getD "null" ++
  ",\"expected\":" ++ ((complete selection beforeDelivery afterDelivery delivery write).map selectionJson).getD "null" ++ "}"

def completionCasesJson := jsonArray (
  ([Selection.selected "image-document-1", .missing, .rejected]).flatMap fun selection =>
    [true, false].flatMap fun beforeDelivery =>
      [true, false].flatMap fun afterDelivery =>
        [DeliveryResult.confirmed, .indeterminate].flatMap fun delivery =>
          [TerminalWriteResult.committed, .failed, .notAttempted].map fun write =>
            completionCaseJson selection beforeDelivery afterDelivery delivery write)

private def attemptCaseJson (prior : Option Selection) (candidate : Selection)
    (authorized : Bool) (write : TerminalWriteResult) :=
  "{\"prior\":" ++ (prior.map selectionJson).getD "null" ++
  ",\"candidate\":" ++ selectionJson candidate ++
  ",\"authorized\":" ++ boolJson authorized ++
  ",\"write\":" ++ writeJson write ++
  ",\"target\":" ++ ((attemptDeliveryTarget prior candidate authorized).map jsonString).getD "null" ++
  ",\"expected\":" ++ ((recordOutcome prior candidate write).map selectionJson).getD "null" ++ "}"

def attemptCasesJson := jsonArray (
  [none, some Selection.rejected, some .missing, some (.selected "original-image")].flatMap fun prior =>
    [Selection.rejected, .missing, .selected "candidate-image"].flatMap fun candidate =>
      [true, false].flatMap fun authorized =>
        [TerminalWriteResult.committed, .failed, .notAttempted].map fun write =>
          attemptCaseJson prior candidate authorized write)

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
