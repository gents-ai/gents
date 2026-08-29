import Proofs.Enrollment.State

namespace Enrollment

def observeOffer (s : State) (o : Offer) : State :=
  { s with observedOffers := insert o s.observedOffers }

def confirmAdminPin (s : State) (o : Offer) : State :=
  if adminPinAdmissible s o then
    { s with adminPins := insert (adminPinFor o) s.adminPins }
  else s

def acceptRequest (s : State) (o : Offer) (r : Request) : State :=
  if requestAdmissible s o r then
    { s with
      acceptedRequests := insert r s.acceptedRequests
      challengeBindings := insert (challengeBindingFor r) s.challengeBindings
      requestBindings := insert (requestBindingFor r) s.requestBindings }
  else s

def decideRequest (s : State) (r : Request) (d : Decision) : State :=
  if decisionAdmissible s r d then
    if d.kind = .approved then
      { s with
        decisions := insert d s.decisions
        authorizations := insert (revisionForApproval r d) s.authorizations
        memberships := s.memberships.filter (fun membership => ¬ membershipOwnedBy r membership)
        appliedRoutes := s.appliedRoutes.filter (fun route => ¬ routeOwnedBy r route) }
    else
      { s with decisions := insert d s.decisions }
  else s

def materializeMembership (s : State) (r : Request) (d : Decision) : State :=
  if currentApproval s r d then
    { s with memberships := insert (membershipFor r d) s.memberships }
  else s

def materializeRoutes (s : State) (r : Request) (d : Decision) : State :=
  if currentApproval s r d ∧ membershipFor r d ∈ s.memberships then
    { s with appliedRoutes :=
        insert (clientToServerRoute r d) (insert (serverToClientRoute r d) s.appliedRoutes) }
  else s

def revokeAdmissible (s : State) (r : Request) (revision : AuthorizationRevision) : Prop :=
  r ∈ s.acceptedRequests ∧ revisionMatchesRequest r revision ∧
  revision.kind = .revoked ∧ revision.adminSigned = true ∧
  ¬ dominatingRevisionExists s revision

instance (s : State) (r : Request) (revision : AuthorizationRevision) : Decidable
    (revokeAdmissible s r revision) := by
  unfold revokeAdmissible; infer_instance

def revoke (s : State) (r : Request) (revision : AuthorizationRevision) : State :=
  if revokeAdmissible s r revision then
    { s with
      authorizations := insert revision s.authorizations
      memberships := s.memberships.filter (fun membership => ¬ membershipOwnedBy r membership)
      appliedRoutes := s.appliedRoutes.filter (fun route => ¬ routeOwnedBy r route) }
  else s

inductive Transition : State → State → Prop where
  | observe {pre post : State} (o : Offer) :
      post = observeOffer pre o → Transition pre post
  | confirmPin {pre post : State} (o : Offer) :
      post = confirmAdminPin pre o → Transition pre post
  | request {pre post : State} (o : Offer) (r : Request) :
      post = acceptRequest pre o r → Transition pre post
  | decide {pre post : State} (r : Request) (d : Decision) :
      post = decideRequest pre r d → Transition pre post
  | membership {pre post : State} (r : Request) (d : Decision) :
      post = materializeMembership pre r d → Transition pre post
  | routes {pre post : State} (r : Request) (d : Decision) :
      post = materializeRoutes pre r d → Transition pre post
  | revoke {pre post : State} (r : Request) (revision : AuthorizationRevision) :
      post = Enrollment.revoke pre r revision → Transition pre post
  | merge {pre post : State} (revision : AuthorizationRevision) :
      post = mergeAuthorization pre revision → Transition pre post

/-- The serial operator path; restore/replication merge is deliberately separate. -/
def SerialTransition (pre post : State) : Prop :=
  (∃ o, post = observeOffer pre o) ∨
  (∃ o, post = confirmAdminPin pre o) ∨
  (∃ o r, post = acceptRequest pre o r) ∨
  (∃ r d, post = decideRequest pre r d) ∨
  (∃ r d, post = materializeMembership pre r d) ∨
  (∃ r d, post = materializeRoutes pre r d) ∨
  (∃ r revision, post = revoke pre r revision)

inductive SerialReachable : State → State → Prop where
  | refl (state : State) : SerialReachable state state
  | tail {start middle finish : State} :
      SerialReachable start middle → SerialTransition middle finish →
      SerialReachable start finish

end Enrollment
