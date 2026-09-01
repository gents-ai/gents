import Proofs.Enrollment.Executable

namespace Enrollment

theorem readWireLength_encodeWireLength (length : Nat) (suffix : WireBytes) :
    readWireLength (encodeWireLength length ++ suffix) = some (length, suffix) := by
  induction length with
  | zero => simp [encodeWireLength, readWireLength]
  | succ length ih => simp [encodeWireLength, readWireLength, ih, Nat.add_comm]

theorem decodeWireFields_serializeWireFieldList
    (fields : CanonicalFields) (suffix : WireBytes) :
    decodeWireFields fields.length (serializeWireFieldList fields ++ suffix) =
      some (fields, suffix) := by
  induction fields with
  | nil => simp [decodeWireFields, serializeWireFieldList]
  | cons field fields ih =>
      simp [decodeWireFields, serializeWireFieldList, frameWireField,
        List.append_assoc, readWireLength_encodeWireLength, ih]

theorem deserializeWireFields_serializeWireFields (fields : CanonicalFields) :
    deserializeWireFields (serializeWireFields fields) = some fields := by
  unfold deserializeWireFields serializeWireFields
  simp only [readWireLength_encodeWireLength]
  have hdecode : decodeWireFields fields.length (serializeWireFieldList fields) =
      some (fields, []) := by
    simpa using decodeWireFields_serializeWireFieldList fields []
  rw [hdecode]

/-- Actual length-framed, concatenated wire bytes are injective. -/
theorem serializeWireFields_injective : Function.Injective serializeWireFields := by
  intro a b h
  have := congrArg deserializeWireFields h
  simpa [deserializeWireFields_serializeWireFields] using this

theorem canonicalSerializedFields_injective :
    Function.Injective canonicalSerializedFields := by
  intro a b h
  have hscoped := serializeWireFields_injective h
  exact (List.cons.inj hscoped).2

theorem hexAsciiNibble_toNat (n : Nat) (h : n < 16) :
    (hexAsciiNibble n).toNat = if n < 10 then 48 + n else 87 + n := by
  unfold hexAsciiNibble
  split <;> simp_all [Nat.mod_eq_of_lt] <;> omega

theorem hexBytePair_injective :
    Function.Injective (fun byte : UInt8 => (hexHighByte byte, hexLowByte byte)) := by
  intro a b h
  have hhigh := congrArg (fun pair => pair.1.toNat) h
  have hlow := congrArg (fun pair => pair.2.toNat) h
  have haBound : a.toNat < 256 := a.toNat_lt
  have hbBound : b.toNat < 256 := b.toNat_lt
  have haHigh : a.toNat / 16 < 16 := by omega
  have hbHigh : b.toNat / 16 < 16 := by omega
  have haLow : a.toNat % 16 < 16 := Nat.mod_lt _ (by omega)
  have hbLow : b.toNat % 16 < 16 := Nat.mod_lt _ (by omega)
  simp only [hexHighByte, hexLowByte, hexAsciiNibble_toNat _ haHigh,
    hexAsciiNibble_toNat _ hbHigh, hexAsciiNibble_toNat _ haLow,
    hexAsciiNibble_toNat _ hbLow] at hhigh hlow
  apply UInt8.eq_of_toBitVec_eq
  apply BitVec.eq_of_toNat_eq
  change a.toNat = b.toNat
  split at hhigh <;> split at hhigh <;> split at hlow <;> split at hlow <;>
  omega

/-- Actual rendered lower-case ASCII/UTF-8 hex bytes are injective. -/
theorem utf8HexBytes_injective : Function.Injective utf8HexBytes := by
  intro a
  induction a with
  | nil =>
      intro b h
      cases b with
      | nil => rfl
      | cons byte bytes => simp [utf8HexBytes] at h
  | cons byte bytes ih =>
      intro b h
      cases b with
      | nil => simp [utf8HexBytes] at h
      | cons other rest =>
          simp only [utf8HexBytes] at h
          have hhigh := (List.cons.inj h).1
          have hrest := (List.cons.inj h).2
          have hlow := (List.cons.inj hrest).1
          have htail := (List.cons.inj hrest).2
          have hbyte : byte = other :=
            hexBytePair_injective (Prod.ext hhigh hlow)
          subst other
          exact congrArg (List.cons byte) (ih htail)

theorem canonicalDigestFromFields_injective :
    Function.Injective canonicalDigestFromFields := by
  intro a b h
  have hserialized := congrArg Digest.serializedBytes h
  exact canonicalSerializedFields_injective hserialized

/-- Equality of the actual wire-rendered digest bytes implies exact canonical fields. -/
theorem renderDigest_canonical_injective :
    Function.Injective (fun fields => renderDigest (canonicalDigestFromFields fields)) := by
  intro a b h
  have hhex : utf8HexBytes (canonicalSerializedFields a) =
      utf8HexBytes (canonicalSerializedFields b) := List.append_cancel_left h
  exact canonicalSerializedFields_injective (utf8HexBytes_injective hhex)

theorem canonicalRequestDigest_eq_implies_fields_eq {a b : Request}
    (h : canonicalRequestDigest a = canonicalRequestDigest b) :
    canonicalRequestFields a = canonicalRequestFields b :=
  canonicalDigestFromFields_injective h

/-- Collision resistance is supplied by the production hash, never asserted internally. -/
theorem production_hash_equality_implies_fields_eq
    (hash : WireBytes → String) (hhash : ProductionHashCollisionResistant hash)
    {a b : CanonicalFields}
    (h : hash (canonicalSerializedFields a) = hash (canonicalSerializedFields b)) :
    a = b :=
  canonicalSerializedFields_injective (hhash h)

/-- Transport observations and verification results are derived, never signed payload fields. -/
theorem canonical_fields_exclude_observed_peer (r : Request) (value : String) :
    canonicalRequestFields { r with observedCandidatePeer := value } =
      canonicalRequestFields r := by rfl

theorem canonical_fields_exclude_resolved_did (r : Request) (value : String) :
    canonicalRequestFields { r with resolvedCandidateDid := value } =
      canonicalRequestFields r := by rfl

theorem canonical_fields_exclude_ticket_peer (r : Request) (value : String) :
    canonicalRequestFields { r with candidateTicketPeer := value } =
      canonicalRequestFields r := by rfl

theorem canonical_fields_exclude_signature_observation (r : Request) (value : Bool) :
    canonicalRequestFields { r with candidateSigned := value } =
      canonicalRequestFields r := by rfl

theorem canonical_fields_exclude_freshness_observation (r : Request) (value : Bool) :
    canonicalRequestFields { r with fresh := value } =
      canonicalRequestFields r := by rfl

theorem status_grants_nothing (s : State) (o : Offer) :
    (observeOffer s o).adminPins = s.adminPins ∧
    (observeOffer s o).memberships = s.memberships ∧
    (observeOffer s o).authorizations = s.authorizations ∧
    (observeOffer s o).appliedRoutes = s.appliedRoutes := by
  exact ⟨rfl, rfl, rfl, rfl⟩

theorem unobserved_offer_cannot_pin {s : State} {o : Offer}
    (h : o ∉ s.observedOffers) : confirmAdminPin s o = s := by
  simp [confirmAdminPin, adminPinAdmissible, h]

theorem confirmed_pin_is_exact {s : State} {o : Offer}
    (h : adminPinAdmissible s o) :
    adminPinFor o ∈ (confirmAdminPin s o).adminPins := by
  simp [confirmAdminPin, h]

theorem conflicting_admin_pin_fails_closed {s : State} {o : Offer}
    (h : adminPinConflict s o) : confirmAdminPin s o = s := by
  simp [confirmAdminPin, adminPinAdmissible, h]

theorem different_network_pin_is_preserved {s : State} {o : Offer} {pin : NetworkAdminPin}
    (hpin : pin ∈ s.adminPins)
    (hadmit : adminPinAdmissible s o) :
    pin ∈ (confirmAdminPin s o).adminPins := by
  simp [confirmAdminPin, hadmit, hpin]

theorem different_network_pin_does_not_conflict {o : Offer} {pin : NetworkAdminPin}
    (hnetwork : pin.networkId ≠ o.networkId) :
    ¬ adminPinConflict ({ adminPins := {pin} } : State) o := by
  simp [adminPinConflict, hnetwork]

theorem unobserved_offer_cannot_admit {s : State} {o : Offer} {r : Request}
    (h : o ∉ s.observedOffers) : ¬ requestAdmissible s o r := by
  intro hadmit
  exact h hadmit.1

theorem unpinned_offer_cannot_admit {s : State} {o : Offer} {r : Request}
    (h : adminPinFor o ∉ s.adminPins) : ¬ requestAdmissible s o r := by
  intro hadmit
  exact h hadmit.2.1.1

theorem request_grants_nothing (s : State) (o : Offer) (r : Request) :
    (acceptRequest s o r).memberships = s.memberships ∧
    (acceptRequest s o r).authorizations = s.authorizations ∧
    (acceptRequest s o r).appliedRoutes = s.appliedRoutes := by
  unfold acceptRequest
  split <;> simp

theorem invalid_request_grants_nothing (s : State) (o : Offer) (r : Request)
    (h : ¬ requestAdmissible s o r) : acceptRequest s o r = s := by
  simp [acceptRequest, h]

theorem exact_request_replay_is_idempotent (s : State) (o : Offer) (r : Request)
    (h : requestAdmissible s o r) :
    acceptRequest (acceptRequest s o r) o r = acceptRequest s o r := by
  have hnext : requestAdmissible (acceptRequest s o r) o r := by
    rcases h with ⟨hobserved, hpin, hos, hof, hschema, hst, hsd, hcs, hrf, hdigest,
      hchannel, hcandidate, hct, hprofile, hmatch, hchallenge, hrequest⟩
    have hadmit : requestAdmissible s o r :=
      ⟨hobserved, hpin, hos, hof, hschema, hst, hsd, hcs, hrf, hdigest,
        hchannel, hcandidate, hct, hprofile, hmatch, hchallenge, hrequest⟩
    refine ⟨?_, ?_, hos, hof, hschema, hst, hsd, hcs, hrf, hdigest,
      hchannel, hcandidate, hct, hprofile, hmatch, ?_, ?_⟩
    · simp [acceptRequest, requestAdmissible, hobserved, hpin, hos, hof, hschema, hst,
        hsd, hcs, hrf, hdigest, hchannel, hcandidate, hct, hprofile, hmatch,
        hchallenge, hrequest, hobserved]
    · rw [acceptRequest, if_pos hadmit]
      exact hpin
    · rintro ⟨binding, hmem, heq, hne⟩
      simp [acceptRequest, requestAdmissible, hobserved, hpin, hos, hof, hschema, hst,
        hsd, hcs, hrf, hdigest, hchannel, hcandidate, hct, hprofile, hmatch,
        hchallenge, hrequest] at hmem
      rcases hmem with rfl | hpre
      · exact hne rfl
      · exact hchallenge ⟨binding, hpre, heq, hne⟩
    · rintro ⟨binding, hmem, heq, hne⟩
      simp [acceptRequest, requestAdmissible, hobserved, hpin, hos, hof, hschema, hst,
        hsd, hcs, hrf, hdigest, hchannel, hcandidate, hct, hprofile, hmatch,
        hchallenge, hrequest] at hmem
      rcases hmem with rfl | hpre
      · exact hne rfl
      · exact hrequest ⟨binding, hpre, heq, hne⟩
  simp [acceptRequest, hnext, h]

theorem request_id_collision_rejected {s : State} {o : Offer} {first second : Request}
    (hadmit : requestAdmissible s o first)
    (hid : second.requestId = first.requestId)
    (hdifferent : requestBindingFor second ≠ requestBindingFor first) :
    ¬ requestAdmissible (acceptRequest s o first) o second := by
  intro hsecond
  rcases hsecond with ⟨_, _, _, _, _, _, _, _, _, _, _, _, _, _, _, _, hrequest⟩
  exact hrequest
    ⟨requestBindingFor first, by simp [acceptRequest, hadmit], hid.symm,
      fun heq => hdifferent heq.symm⟩

theorem challenge_replay_rejected_across_requests {s : State} {o : Offer}
    {first second : Request} (hadmit : requestAdmissible s o first)
    (hchallenge : second.challenge = first.challenge)
    (hdifferent : challengeBindingFor second ≠ challengeBindingFor first) :
    ¬ requestAdmissible (acceptRequest s o first) o second := by
  intro hsecond
  rcases hsecond with ⟨_, _, _, _, _, _, _, _, _, _, _, _, _, _, _, hbound, _⟩
  exact hbound
    ⟨challengeBindingFor first, by simp [acceptRequest, hadmit], hchallenge.symm,
      fun heq => hdifferent heq.symm⟩

theorem first_decision_is_terminal {s : State} {r : Request} {d : Decision}
    (hadmit : decisionAdmissible s r d) (next : Decision) :
    decideRequest (decideRequest s r d) r next = decideRequest s r d := by
  have hdmem : d ∈ (decideRequest s r d).decisions := by
    by_cases hkind : d.kind = .approved
    · simp [decideRequest, hadmit, hkind]
    · simp [decideRequest, hadmit, hkind]
  have hterminal : terminalDecisionFor (decideRequest s r d) r.requestId :=
    ⟨d, hdmem, hadmit.2.1.1⟩
  have hnot : ¬ decisionAdmissible (decideRequest s r d) r next := by
    intro hnext
    exact hnext.2.2.2.2.2.1 hterminal
  rw [decideRequest, if_neg hnot]

theorem membership_growth_requires_current_admin_approval {s : State} {r : Request}
    {d : Decision} {m : Membership}
    (hnew : m ∈ (materializeMembership s r d).memberships)
    (hold : m ∉ s.memberships) :
    currentApproval s r d ∧ m = membershipFor r d := by
  unfold materializeMembership at hnew
  split at hnew
  · rename_i hcurrent
    simp only [Finset.mem_insert] at hnew
    rcases hnew with hnew | hnew
    · exact ⟨hcurrent, hnew⟩
    · exact False.elim (hold hnew)
  · exact False.elim (hold hnew)

/-- A legacy desired-row observation is not a transition and cannot grant peer admission. -/
theorem legacy_pairing_materialization_cannot_grant_peer_admission
    (s : State) (legacyDesiredPeers : Finset Did) (memberDid injected : Did) :
    projectsPeerAdmission s (insert injected legacyDesiredPeers) memberDid ↔
      projectsPeerAdmission s legacyDesiredPeers memberDid := Iff.rfl

theorem current_enrollment_grants_peer_admission {s : State} {r : Request} {d : Decision}
    (hcurrent : currentApproval s r d) :
    peerOperationallyAuthorized s r.candidateDid := by
  exact ⟨r, hcurrent.1, d, hcurrent.2.1, hcurrent, rfl⟩

theorem empty_authorization_wellFormed : AuthorizationWellFormed ({} : State) := by
  simp [AuthorizationWellFormed]

/-- A restore merge preserves serial well-formedness exactly when its tie is identical. -/
theorem mergeAuthorization_preserves_wellFormed
    {s : State} {revision : AuthorizationRevision}
    (hwf : AuthorizationWellFormed s)
    (hcompatible : ∀ current ∈ s.authorizations,
      sameMember current revision → current.sequence = revision.sequence →
      current = revision) :
    AuthorizationWellFormed (mergeAuthorization s revision) := by
  intro a ha b hb hsame hseq
  simp only [mergeAuthorization, Finset.mem_insert] at ha hb
  rcases ha with rfl | ha <;> rcases hb with rfl | hb
  · rfl
  · exact (hcompatible b hb ⟨hsame.1.symm, hsame.2.symm⟩ hseq.symm).symm
  · exact hcompatible a ha hsame hseq
  · exact hwf a ha b hb hsame hseq

theorem conflicting_merge_is_not_wellFormed
    {s : State} {a conflict : AuthorizationRevision}
    (ha : a ∈ s.authorizations) (hsame : sameMember a conflict)
    (hseq : a.sequence = conflict.sequence) (hne : a ≠ conflict) :
    ¬ AuthorizationWellFormed (mergeAuthorization s conflict) := by
  intro hwf
  have hconflict : conflict ∈ (mergeAuthorization s conflict).authorizations := by
    simp [mergeAuthorization]
  have ha' : a ∈ (mergeAuthorization s conflict).authorizations := by
    simp [mergeAuthorization, ha]
  exact hne (hwf a ha' conflict hconflict hsame hseq)

theorem observeOffer_preserves_wellFormed {s : State} {o : Offer}
    (hwf : AuthorizationWellFormed s) :
    AuthorizationWellFormed (observeOffer s o) := hwf

theorem confirmAdminPin_preserves_wellFormed {s : State} {o : Offer}
    (hwf : AuthorizationWellFormed s) :
    AuthorizationWellFormed (confirmAdminPin s o) := by
  unfold confirmAdminPin
  split <;> exact hwf

theorem acceptRequest_preserves_wellFormed {s : State} {o : Offer} {r : Request}
    (hwf : AuthorizationWellFormed s) :
    AuthorizationWellFormed (acceptRequest s o r) := by
  unfold acceptRequest
  split <;> exact hwf

theorem materializeMembership_preserves_wellFormed
    {s : State} {r : Request} {d : Decision}
    (hwf : AuthorizationWellFormed s) :
    AuthorizationWellFormed (materializeMembership s r d) := by
  unfold materializeMembership
  split <;> exact hwf

theorem materializeClientRoute_preserves_wellFormed
    {s : State} {r : Request} {d : Decision}
    (hwf : AuthorizationWellFormed s) :
    AuthorizationWellFormed (materializeClientRoute s r d) := by
  unfold materializeClientRoute
  split <;> exact hwf

theorem recordServerRouteReceipt_preserves_wellFormed
    {s : State} {r : Request} {d : Decision} {receipt : RouteReceipt}
    (hwf : AuthorizationWellFormed s) :
    AuthorizationWellFormed (recordServerRouteReceipt s r d receipt) := by
  unfold recordServerRouteReceipt
  split <;> exact hwf

theorem decideRequest_preserves_wellFormed
    {s : State} {r : Request} {d : Decision}
    (hwf : AuthorizationWellFormed s) :
    AuthorizationWellFormed (decideRequest s r d) := by
  unfold decideRequest
  split
  · rename_i hadmit
    split
    · rename_i happroved
      apply mergeAuthorization_preserves_wellFormed hwf
      intro current hcurrent hsame hsequence
      have hnondominating : ¬ dominatingRevisionExists s (revisionForApproval r d) := by
        rcases hadmit.2.2.2.2.2.2 with hdenied | happroval
        · exact False.elim (DecisionKind.noConfusion (hdenied.symm.trans happroved))
        · exact happroval.2
      exact False.elim <| hnondominating
        ⟨current, hcurrent, hsame, by omega⟩
    · exact hwf
  · exact hwf

theorem revoke_preserves_wellFormed
    {s : State} {r : Request} {revision : AuthorizationRevision}
    (hwf : AuthorizationWellFormed s) :
    AuthorizationWellFormed (revoke s r revision) := by
  unfold revoke
  split
  · rename_i hadmit
    apply mergeAuthorization_preserves_wellFormed hwf
    intro current hcurrent hsame hsequence
    exact False.elim <| hadmit.2.2.2.2.2
      ⟨current, hcurrent, hsame, by omega⟩
  · exact hwf

/-- Every serial operator transition preserves well-formedness. -/
theorem SerialTransition.preserves_wellFormed
    {pre post : State} (step : SerialTransition pre post)
    (hwf : AuthorizationWellFormed pre) : AuthorizationWellFormed post := by
  rcases step with ⟨o, rfl⟩ | ⟨o, rfl⟩ | ⟨o, r, rfl⟩ |
    ⟨r, d, rfl⟩ | ⟨r, d, rfl⟩ | ⟨r, d, rfl⟩ |
    ⟨r, d, receipt, rfl⟩ | ⟨r, revision, rfl⟩
  · exact observeOffer_preserves_wellFormed hwf
  · exact confirmAdminPin_preserves_wellFormed hwf
  · exact acceptRequest_preserves_wellFormed hwf
  · exact decideRequest_preserves_wellFormed hwf
  · exact materializeMembership_preserves_wellFormed hwf
  · exact materializeClientRoute_preserves_wellFormed hwf
  · exact recordServerRouteReceipt_preserves_wellFormed hwf
  · exact revoke_preserves_wellFormed hwf

theorem SerialReachable.preserves_wellFormed
    {start finish : State} (trace : SerialReachable start finish)
    (hwf : AuthorizationWellFormed start) : AuthorizationWellFormed finish := by
  induction trace with
  | refl => exact hwf
  | tail previous step ih => exact step.preserves_wellFormed ih

/-- Equal-sequence conflicting replicas invalidate both contenders. -/
theorem equal_sequence_conflict_has_no_unique_maximum
    {s : State} {a b : AuthorizationRevision}
    (ha : a ∈ s.authorizations) (hb : b ∈ s.authorizations)
    (hsame : sameMember a b) (hseq : a.sequence = b.sequence) (hne : a ≠ b) :
    ¬ uniqueMaximumRevision s a ∧ ¬ uniqueMaximumRevision s b := by
  constructor
  · rintro ⟨_, hmax⟩
    rcases hmax b hb ⟨hsame.1.symm, hsame.2.symm⟩ with hlt | heq
    · exact (Nat.lt_irrefl a.sequence) (hseq ▸ hlt)
    · exact hne heq.symm
  · rintro ⟨_, hmax⟩
    rcases hmax a ha hsame with hlt | heq
    · exact (Nat.lt_irrefl b.sequence) (hseq.symm ▸ hlt)
    · exact hne heq

theorem conflicting_merge_retracts_current_projection
    {s : State} {r : Request} {d : Decision} {conflict : AuthorizationRevision}
    (hcurrent : currentApproval s r d)
    (hsame : sameMember conflict (revisionForApproval r d))
    (hseq : conflict.sequence = (revisionForApproval r d).sequence)
    (hne : conflict ≠ revisionForApproval r d) :
    ¬ currentApproval (mergeAuthorization s conflict) r d := by
  intro hafter
  have hbase : revisionForApproval r d ∈ s.authorizations :=
    hcurrent.2.2.2.2.2.2.2.1
  have hconflict : conflict ∈ (mergeAuthorization s conflict).authorizations := by
    simp [mergeAuthorization]
  have hbase' : revisionForApproval r d ∈ (mergeAuthorization s conflict).authorizations := by
    simp [mergeAuthorization, hbase]
  exact (equal_sequence_conflict_has_no_unique_maximum hconflict hbase'
    hsame hseq hne).2 hafter.2.2.2.2.2.2.2

theorem stale_approval_cannot_materialize_after_revocation
    {s : State} {r : Request} {d : Decision} {revision : AuthorizationRevision}
    (hcurrent : currentApproval s r d)
    (hrevoke : revokeAdmissible s r revision)
    (hnewer : (revisionForApproval r d).sequence < revision.sequence) :
    materializeMembership (revoke s r revision) r d = revoke s r revision ∧
      revisionForApproval r d ∈ s.authorizations := by
  have hrevIn : revision ∈ (revoke s r revision).authorizations := by
    simp [revoke, hrevoke]
  have hsame : sameMember revision (revisionForApproval r d) := by
    rcases hrevoke.2.1 with ⟨_, _, hrnetwork, _, hrmember, _, _, _⟩
    exact ⟨hrnetwork, hrmember⟩
  have hnot : ¬ currentApproval (revoke s r revision) r d := by
    intro hafter
    rcases hafter.2.2.2.2.2.2.2.2 revision hrevIn hsame with hlt | heq
    · exact (Nat.not_lt_of_ge (Nat.le_of_lt hnewer)) hlt
    · have hseq := congrArg AuthorizationRevision.sequence heq
      exact (Nat.ne_of_lt hnewer) hseq.symm
  exact ⟨by simp [materializeMembership, hnot], hcurrent.2.2.2.2.2.2.2.1⟩

theorem revocation_retracts_exact_member_routes
    {s : State} {r : Request} {d : Decision} {revision : AuthorizationRevision}
    (hrevoke : revokeAdmissible s r revision) :
    clientToServerRoute r d ∉ (revoke s r revision).appliedRoutes ∧
    serverToClientRoute r d ∉ (revoke s r revision).appliedRoutes := by
  simp [revoke, hrevoke, routeOwnedBy, clientToServerRoute, serverToClientRoute]

theorem revocation_preserves_unrelated_operations
    {s : State} {r : Request} {revision : AuthorizationRevision}
    {membership : Membership} {route : AppliedRoute}
    (hrevoke : revokeAdmissible s r revision)
    (hmembership : membership ∈ s.memberships) (hroute : route ∈ s.appliedRoutes)
    (hmembershipOther : ¬ membershipOwnedBy r membership)
    (hrouteOther : ¬ routeOwnedBy r route) :
    membership ∈ (revoke s r revision).memberships ∧
      route ∈ (revoke s r revision).appliedRoutes := by
  simp [revoke, hrevoke, hmembership, hroute, hmembershipOther, hrouteOther]

theorem client_route_growth_is_exact {s : State} {r : Request} {d : Decision}
    {route : AppliedRoute}
    (hnew : route ∈ (materializeClientRoute s r d).appliedRoutes)
    (hold : route ∉ s.appliedRoutes) :
    route = serverToClientRoute r d := by
  unfold materializeClientRoute at hnew
  split at hnew
  · simp only [Finset.mem_insert] at hnew
    rcases hnew with hserver | hpre
    · exact hserver
    · exact False.elim (hold hpre)
  · exact False.elim (hold hnew)

theorem receipt_route_growth_is_exact {s : State} {r : Request} {d : Decision}
    {receipt : RouteReceipt} {route : AppliedRoute}
    (hnew : route ∈ (recordServerRouteReceipt s r d receipt).appliedRoutes)
    (hold : route ∉ s.appliedRoutes) :
    route = clientToServerRoute r d := by
  unfold recordServerRouteReceipt at hnew
  split at hnew
  · simp only [Finset.mem_insert] at hnew
    rcases hnew with hclient | hpre
    · exact hclient
    · exact False.elim (hold hpre)
  · exact False.elim (hold hnew)

theorem enrollment_ready_refines_directional_hydration_admission
    {s : State} {r : Request} {session : String} {direction : RouteDirection}
    {sessions : Finset SessionHydration.SessionOwner}
    (hready : enrollmentReady s r)
    (howner : SessionHydration.ownedSession
      (hydrationRequestForDirection r session direction) ∈ sessions) :
    SessionHydration.admits (projectedHydrationCatalogFor s r.networkId direction sessions)
      (hydrationRequestForDirection r session direction) := by
  rcases hready with ⟨d, hd, hcurrent, hmembership, _, hclient, hserver⟩
  have hrequest : r ∈ s.acceptedRequests := hcurrent.1
  cases direction with
  | clientToServer =>
      refine ⟨?_, ?_, howner⟩
      · unfold projectedHydrationCatalogFor SessionHydration.appliedPairingRoute
          hydrationRequestForDirection
        simp only [Finset.mem_image, Finset.mem_filter]
        exact ⟨clientToServerRoute r d,
          ⟨hclient, rfl, rfl, rfl,
            ⟨r, hrequest, d, hd, hcurrent, hmembership, rfl⟩⟩, rfl⟩
      · unfold projectedHydrationCatalogFor SessionHydration.verifiedMembership
          hydrationRequestForDirection
        simp only [Finset.mem_image, Finset.mem_filter]
        exact ⟨membershipFor r d,
          ⟨hmembership, rfl, rfl, rfl, rfl,
            ⟨r, hrequest, d, hd, hcurrent, rfl⟩⟩, rfl⟩
  | serverToClient =>
      refine ⟨?_, ?_, howner⟩
      · unfold projectedHydrationCatalogFor SessionHydration.appliedPairingRoute
          hydrationRequestForDirection
        simp only [Finset.mem_image, Finset.mem_filter]
        exact ⟨serverToClientRoute r d,
          ⟨hserver, rfl, rfl, rfl,
            ⟨r, hrequest, d, hd, hcurrent, hmembership, rfl⟩⟩, rfl⟩
      · unfold projectedHydrationCatalogFor SessionHydration.verifiedMembership
          hydrationRequestForDirection
        simp only [Finset.mem_image, Finset.mem_filter]
        exact ⟨membershipFor r d,
          ⟨hmembership, rfl, rfl, rfl, rfl,
            ⟨r, hrequest, d, hd, hcurrent, rfl⟩⟩, rfl⟩

theorem client_ready_refines_hydration_admission
    {s : State} {r : Request} {session : String}
    {sessions : Finset SessionHydration.SessionOwner}
    (hready : enrollmentReady s r)
    (howner : SessionHydration.ownedSession (hydrationRequestFor r session) ∈ sessions) :
    SessionHydration.admits (projectedClientToServerHydrationCatalog s r.networkId sessions)
      (hydrationRequestFor r session) := by
  exact enrollment_ready_refines_directional_hydration_admission hready howner

theorem server_ready_refines_hydration_admission
    {s : State} {r : Request} {session : String}
    {sessions : Finset SessionHydration.SessionOwner}
    (hready : enrollmentReady s r)
    (howner : SessionHydration.ownedSession (reverseHydrationRequestFor r session) ∈ sessions) :
    SessionHydration.admits (projectedServerToClientHydrationCatalog s r.networkId sessions)
      (reverseHydrationRequestFor r session) := by
  exact enrollment_ready_refines_directional_hydration_admission hready howner

/-- Every directional route admitted by projection belongs to an exact unique current approval. -/
theorem projected_hydration_admission_requires_exact_current_approval
    {s : State} {selectedNetwork : String} {direction : RouteDirection}
    {sessions : Finset SessionHydration.SessionOwner}
    {request : SessionHydration.Request}
    (hadmits : SessionHydration.admits
      (projectedHydrationCatalogFor s selectedNetwork direction sessions) request) :
    ∃ r ∈ s.acceptedRequests, ∃ d ∈ s.decisions,
      currentApproval s r d ∧ membershipFor r d ∈ s.memberships ∧
      r.networkId = selectedNetwork ∧
      toHydrationRoute (routeForDirection r d direction) =
        SessionHydration.appliedPairingRoute request := by
  rcases hadmits.1 with hroute
  unfold projectedHydrationCatalogFor at hroute
  simp only [Finset.mem_image, Finset.mem_filter] at hroute
  rcases hroute with ⟨route, ⟨_, hnetwork, _, _, hauthorized⟩, heq⟩
  rcases hauthorized with ⟨r, hr, d, hd, hcurrent, hmembership, hrouteeq⟩
  subst route
  have hrnetwork : r.networkId = selectedNetwork := by
    cases direction <;>
      simpa [routeForDirection, clientToServerRoute, serverToClientRoute] using hnetwork
  exact ⟨r, hr, d, hd, hcurrent, hmembership, hrnetwork, heq⟩

theorem client_hydration_admission_requires_exact_current_approval
    {s : State} {selectedNetwork : String}
    {sessions : Finset SessionHydration.SessionOwner}
    {request : SessionHydration.Request}
    (hadmits : SessionHydration.admits
      (projectedClientToServerHydrationCatalog s selectedNetwork sessions) request) :
    ∃ r ∈ s.acceptedRequests, ∃ d ∈ s.decisions,
      currentApproval s r d ∧ membershipFor r d ∈ s.memberships ∧
      r.networkId = selectedNetwork ∧
      toHydrationRoute (clientToServerRoute r d) =
        SessionHydration.appliedPairingRoute request := by
  exact projected_hydration_admission_requires_exact_current_approval hadmits

theorem server_hydration_admission_requires_exact_current_approval
    {s : State} {selectedNetwork : String}
    {sessions : Finset SessionHydration.SessionOwner}
    {request : SessionHydration.Request}
    (hadmits : SessionHydration.admits
      (projectedServerToClientHydrationCatalog s selectedNetwork sessions) request) :
    ∃ r ∈ s.acceptedRequests, ∃ d ∈ s.decisions,
      currentApproval s r d ∧ membershipFor r d ∈ s.memberships ∧
      r.networkId = selectedNetwork ∧
      toHydrationRoute (serverToClientRoute r d) =
        SessionHydration.appliedPairingRoute request := by
  exact projected_hydration_admission_requires_exact_current_approval hadmits

end Enrollment
