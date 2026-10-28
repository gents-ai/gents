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

end SessionHydration.ReaderExamples
