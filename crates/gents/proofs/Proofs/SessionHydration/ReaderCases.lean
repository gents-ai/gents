import Proofs.SessionHydration.FileInventoryCases

namespace SessionHydration.ReaderExamples

open FileInventory FileInventory.Examples

def canonicalSource : SessionOwner :=
  { ownedSession admission with requester := "canonical-author" }

def readObservation : SessionReadObservation :=
  ⟨admission.authorizationScope, canonicalSource, true⟩

def shared : SourceInput :=
  { source 1 with
    catalog := { catalog [1] with sessions := [canonicalSource], sessionReads := [readObservation] }
    tools := [{ tool 1 with scope := { scope with requester := canonicalSource.requester } }]
    projections := [{ projection 1 with scope := { scope with requester := canonicalSource.requester } }]
    boundary := .readerBound admission.authorizationScope }

private def withRead (observation : SessionReadObservation) : SourceInput :=
  { shared with catalog := { shared.catalog with sessionReads := [observation] } }

def cases : List SelectionCase :=
  [ ⟨"shared_native_root_read", shared, .selected 1 0 0 false⟩
  , ⟨"shared_authorized_empty_root", { source 0 with
      catalog := { catalog [] with sessions := [canonicalSource], sessionReads := [readObservation] }
      boundary := .readerBound admission.authorizationScope }, .selected 0 0 0 true⟩
  , ⟨"shared_pairing_without_root_read", {shared with catalog :=
      {shared.catalog with sessionReads := []}}, .rejected⟩
  , ⟨"shared_root_read_denied", withRead {readObservation with visible := false}, .rejected⟩
  , ⟨"shared_read_for_another_reader", withRead {readObservation with
      scope := {readObservation.scope with requester := "another-reader"}}, .rejected⟩
  , ⟨"shared_read_for_another_peer", withRead {readObservation with
      scope := {readObservation.scope with peer := "another-peer"}}, .rejected⟩
  , ⟨"shared_read_for_another_node", withRead {readObservation with
      scope := {readObservation.scope with node := "another-node"}}, .rejected⟩
  , ⟨"shared_read_for_another_physical_root", withRead {readObservation with
      scope := {readObservation.scope with nativeSession := 99}}, .rejected⟩
  , ⟨"shared_read_observes_another_canonical_author", withRead {readObservation with
      source := {canonicalSource with requester := admission.requester}}, .rejected⟩
  , ⟨"shared_root_physical_identity_changed", {shared with catalog :=
      {shared.catalog with sessions := [{canonicalSource with nativeSession := 99}]}}, .rejected⟩
  , ⟨"shared_ambiguous_logical_root", {shared with catalog := {shared.catalog with
      sessions := [canonicalSource, {canonicalSource with nativeSession := 99}]}}, .rejected⟩
  , ⟨"shared_membership_revoked", {shared with catalog :=
      {shared.catalog with verifiedActiveMemberships := []}}, .rejected⟩
  , ⟨"shared_root_read_cannot_reuse_privileged_metadata", {shared with
      boundary := .atomicExhaustive}, .unknown⟩
  , ⟨"shared_metadata_read_is_bound_to_its_reader", {shared with
      boundary := .readerBound {admission.authorizationScope with requester := canonicalSource.requester}}, .unknown⟩
  , ⟨"shared_unknown_closure_is_not_empty_complete", {shared with catalog :=
      {shared.catalog with closureInputs := [{input [1] with access := []}]}}, .unknown⟩
  , ⟨"shared_missing_capture_is_not_empty_complete", {shared with projections := []},
      .selected 0 0 1 false⟩
  , ⟨"shared_unreadable_projection_owner_is_not_trusted", {shared with projections :=
      [{projection 1 with scope := {scope with requester := canonicalSource.requester}, owner := ""}]}, .rejected⟩
  , ⟨"shared_projection_cannot_impersonate_the_reader", {shared with projections :=
      [{projection 1 with scope := scope}]}, .rejected⟩ ]

theorem regressions : cases.all (fun test => summary test.source == test.expected) = true := by
  native_decide

theorem canonical_requester_is_preserved :
    (snapshot shared.model admission).toOption.any (fun value =>
      value.entries.all (fun entry => entry.scope.requester == canonicalSource.requester) &&
        value.admission.requester == admission.requester) = true := by
  native_decide

theorem metadata_does_not_authorize_an_unbound_image :
    FileImages.select ⟨admission, "request-document", "tool-document"⟩ shared.catalog.model
      [⟨"tool-document", "request-document", ⟨canonicalSource.requester, admission.node,
          admission.session⟩, true, true⟩]
      [⟨"image-document", "tool-document", "request-document",
          ⟨canonicalSource.requester, admission.node, admission.session⟩⟩] = .rejected := by
  native_decide

def imagePage : PageEnvelope :=
  ⟨.fileInventoryV2, "metadata-page-key", admission.node, true,
    (makePage (getSnapshot shared) 0).toWire "reader-snapshot"⟩

def imageDemand : ImageDemand :=
  ⟨⟨{admission with key := "image-attempt"}, "1001", "1"⟩,
    ⟨admission, "metadata-attempt", none⟩, "reader-snapshot", 0⟩

def imageRead : SelectedImageRead :=
  ⟨admission.authorizationScope, 3001, 2001, true, true, admission.node, admission.node⟩

def selectedImageTool : FileImages.Tool :=
  ⟨"1", "1001", ⟨canonicalSource.requester, admission.node, admission.session⟩, true, true⟩

def imageTools : List FileImages.Tool := [selectedImageTool]

def selectedImage : FileImages.Image :=
  ⟨"2001", "1", "1001", ⟨canonicalSource.requester, admission.node, admission.session⟩⟩

def images : List FileImages.Image := [selectedImage]

structure ImageCase where
  name : String
  demand : ImageDemand := imageDemand
  catalog : CatalogInput := shared.catalog
  page : PageEnvelope := imagePage
  nonceMatches : Bool := true
  pageOwner : String := admission.node
  read : SelectedImageRead := imageRead
  tools : List FileImages.Tool := imageTools
  images : List FileImages.Image := ReaderExamples.images
  expected : FileImages.Selection := .rejected

def imageCases : List ImageCase :=
  [ { name := "shared_image_selected", expected := .selected "2001" }
  , { name := "metadata_permission_is_not_image_permission", read := {imageRead with imageVisible := false} }
  , { name := "projection_permission_revoked", read := {imageRead with projectionVisible := false} }
  , { name := "image_permission_observed_for_another_reader", read := {imageRead with
      scope := {imageRead.scope with requester := canonicalSource.requester}} }
  , { name := "image_permission_observed_for_another_root", read := {imageRead with
      scope := {imageRead.scope with nativeSession := 99}} }
  , { name := "image_permission_observed_for_another_projection", read := {imageRead with projection := 99} }
  , { name := "image_permission_observed_for_another_capture", read := {imageRead with observation := 99} }
  , { name := "image_owned_by_another_node", read := {imageRead with imageOwner := "another-node"} }
  , { name := "metadata_page_owned_by_another_node", pageOwner := "another-node" }
  , { name := "metadata_page_signature_unverified", page := {imagePage with signatureVerified := false} }
  , { name := "metadata_page_has_another_purpose", page := {imagePage with purpose := .transcriptV1} }
  , { name := "metadata_page_nonce_mismatch", nonceMatches := false }
  , { name := "metadata_page_digest_mismatch", demand := {imageDemand with digest := "another-snapshot"} }
  , { name := "metadata_entry_out_of_range", demand := {imageDemand with entryIndex := 1} }
  , { name := "image_request_crosses_physical_request", demand := {imageDemand with
      image := {imageDemand.image with requestDocId := "another-request"}} }
  , { name := "image_request_crosses_physical_root", demand := {imageDemand with
      image := {imageDemand.image with admission := {admission with nativeSession := 99}},
      metadata := {imageDemand.metadata with admission := {admission with nativeSession := 99}}} }
  , { name := "missing_selected_image_is_not_empty_success", images := [] }
  , { name := "ambiguous_tool_images_are_not_selected", images := [selectedImage,
      {selectedImage with docId := "another-observation"}] }
  , { name := "image_scope_cannot_impersonate_the_reader", images := [{selectedImage with
      scope := scope}] }
  , { name := "image_permission_reauthorized", expected := .selected "2001" }
  , { name := "canonical_reader_can_use_a_bound_image", catalog := catalog [1],
      page := {imagePage with page := (makePage (getSnapshot (source 1)) 0).toWire "reader-snapshot"},
      tools := [{selectedImageTool with scope := scope}],
      images := [{selectedImage with scope := scope}], expected := .selected "2001" } ]

def ImageCase.result (test : ImageCase) : FileImages.Selection :=
  selectImage test.demand test.catalog.model test.page test.nonceMatches test.pageOwner test.read
    test.tools test.images

theorem selected_image_regressions : imageCases.all (fun test => test.result == test.expected) = true := by
  native_decide

theorem legacy_image_cannot_impersonate_a_shared_requester :
    FileImages.select ⟨admission, "1001", "1"⟩ shared.catalog.model
      [{selectedImageTool with scope := scope}] [{selectedImage with scope := scope}] = .rejected := by
  native_decide

end SessionHydration.ReaderExamples
