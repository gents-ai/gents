//! Pure authenticated-enrollment transition and projection core.
//!
//! Durable I/O, signatures, transport challenges, and route materialization
//! remain outside this module. Their verified inputs feed this state machine;
//! the generated Lean enrollment traces fence its decisions and projections.

use std::collections::BTreeSet;

use super::session_hydration::{
    decide_hydration, AppliedPairingRoute, HydrationCatalog, HydrationRequest, HydrationVerdict,
    SessionOwner, VerifiedActiveMembership,
};

pub use gents_protocol::enrollment::{
    canonical_enrollment_digest, canonical_enrollment_payload, frame_enrollment_field,
    ENROLLMENT_DIGEST_DOMAIN, ENROLLMENT_DIGEST_PREFIX,
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct EnrollmentOffer {
    pub offer_id: String,
    pub challenge: String,
    pub network_id: String,
    pub admin_did: String,
    pub server_peer: String,
    pub server_ticket_peer: String,
    pub resolved_server_did: String,
    pub owner_agent: String,
    pub profile: String,
    pub schema_compatible: bool,
    pub admin_signed: bool,
    pub fresh: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct NetworkAdminPin {
    pub network_id: String,
    pub admin_did: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct EnrollmentRequest {
    pub request_id: String,
    pub digest: String,
    pub offer_id: String,
    pub challenge: String,
    pub network_id: String,
    pub admin_did: String,
    pub server_peer: String,
    pub candidate_did: String,
    pub candidate_peer: String,
    pub observed_candidate_peer: String,
    pub resolved_candidate_did: String,
    pub candidate_ticket_peer: String,
    pub owner_agent: String,
    pub profile: String,
    pub client_nonce: String,
    pub issued_at: String,
    pub expires_at: String,
    pub candidate_signed: bool,
    pub fresh: bool,
}

impl EnrollmentRequest {
    pub fn canonical_text_fields(&self) -> [&str; 13] {
        [
            &self.request_id,
            &self.offer_id,
            &self.challenge,
            &self.network_id,
            &self.admin_did,
            &self.server_peer,
            &self.candidate_did,
            &self.candidate_peer,
            &self.owner_agent,
            &self.profile,
            &self.client_nonce,
            &self.issued_at,
            &self.expires_at,
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EnrollmentDecisionKind {
    Approved,
    Denied,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct EnrollmentDecision {
    pub request_id: String,
    pub request_digest: String,
    pub network_id: String,
    pub admin_did: String,
    pub candidate_did: String,
    pub candidate_peer: String,
    pub owner_agent: String,
    pub kind: EnrollmentDecisionKind,
    pub authorization_sequence: usize,
    pub signer_did: String,
    pub admin_signed: bool,
    pub fresh: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AuthorizationRevisionKind {
    Active,
    Revoked,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AuthorizationRevision {
    pub request_id: String,
    pub request_digest: String,
    pub network_id: String,
    pub admin_did: String,
    pub member_did: String,
    pub member_peer: String,
    pub owner_agent: String,
    pub sequence: usize,
    pub kind: AuthorizationRevisionKind,
    pub signer_did: String,
    pub admin_signed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct EnrollmentMembership {
    pub request_id: String,
    pub request_digest: String,
    pub network_id: String,
    pub member_did: String,
    pub member_peer: String,
    pub owner_agent: String,
    pub authorization_sequence: usize,
    pub active: bool,
    pub admin_signed: bool,
    pub fresh: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EnrollmentRouteDirection {
    ClientToServer,
    ServerToClient,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct EnrollmentAppliedRoute {
    pub request_id: String,
    pub network_id: String,
    pub authorization_sequence: usize,
    pub direction: EnrollmentRouteDirection,
    pub peer: String,
    pub requester: String,
    pub agent: String,
    pub profile: String,
    pub live: bool,
}

/// Unordered durable enrollment documents restored from DefraDB.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DurableEnrollmentDocuments {
    pub offers: BTreeSet<EnrollmentOffer>,
    pub admin_pins: BTreeSet<NetworkAdminPin>,
    pub requests: BTreeSet<EnrollmentRequest>,
    pub decisions: BTreeSet<EnrollmentDecision>,
    pub revisions: BTreeSet<AuthorizationRevision>,
}

impl DurableEnrollmentDocuments {
    pub fn request_identity_unique(&self, request: &EnrollmentRequest) -> bool {
        self.requests.contains(request)
            && self.requests.iter().all(|other| {
                (other.request_id != request.request_id && other.challenge != request.challenge)
                    || other == request
            })
    }

    pub fn decision_identity_unique(&self, decision: &EnrollmentDecision) -> bool {
        self.decisions.contains(decision)
            && self
                .decisions
                .iter()
                .all(|other| other.request_id != decision.request_id || other == decision)
    }

    /// Order-independent restored authority projection.
    pub fn current_approval(
        &self,
        offer: &EnrollmentOffer,
        request: &EnrollmentRequest,
        decision: &EnrollmentDecision,
    ) -> bool {
        let state = EnrollmentState {
            observed_offers: self.offers.clone(),
            admin_pins: self.admin_pins.clone(),
            authorizations: self.revisions.clone(),
            ..EnrollmentState::default()
        };
        self.request_identity_unique(request)
            && state.request_admissible(offer, request)
            && self.decision_identity_unique(decision)
            && EnrollmentState::decision_matches_request(request, decision)
            && decision.kind == EnrollmentDecisionKind::Approved
            && decision.admin_signed
            && decision.fresh
            && state
                .unique_maximum_revision(&EnrollmentState::revision_for_approval(request, decision))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ChallengeBinding {
    challenge: String,
    request_id: String,
    digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RequestBinding {
    request_id: String,
    digest: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnrollmentState {
    pub observed_offers: BTreeSet<EnrollmentOffer>,
    pub admin_pins: BTreeSet<NetworkAdminPin>,
    pub accepted_requests: BTreeSet<EnrollmentRequest>,
    challenge_bindings: BTreeSet<ChallengeBinding>,
    request_bindings: BTreeSet<RequestBinding>,
    pub decisions: BTreeSet<EnrollmentDecision>,
    pub authorizations: BTreeSet<AuthorizationRevision>,
    pub memberships: BTreeSet<EnrollmentMembership>,
    pub applied_routes: BTreeSet<EnrollmentAppliedRoute>,
}

pub enum EnrollmentAction {
    ObserveOffer(EnrollmentOffer),
    ConfirmAdminPin(EnrollmentOffer),
    AcceptRequest(EnrollmentOffer, EnrollmentRequest),
    DecideRequest(EnrollmentRequest, EnrollmentDecision),
    MaterializeMembership(EnrollmentRequest, EnrollmentDecision),
    MaterializeRoutes(EnrollmentRequest, EnrollmentDecision),
    Revoke(EnrollmentRequest, AuthorizationRevision),
    MergeAuthorization(AuthorizationRevision),
}

impl EnrollmentState {
    pub fn apply(&mut self, action: EnrollmentAction) {
        match action {
            EnrollmentAction::ObserveOffer(offer) => {
                self.observed_offers.insert(offer);
            }
            EnrollmentAction::ConfirmAdminPin(offer) => {
                if self.admin_pin_admissible(&offer) {
                    self.admin_pins.insert(Self::admin_pin_for(&offer));
                }
            }
            EnrollmentAction::AcceptRequest(offer, request) => {
                if self.request_admissible(&offer, &request) {
                    self.challenge_bindings
                        .insert(Self::challenge_binding_for(&request));
                    self.request_bindings
                        .insert(Self::request_binding_for(&request));
                    self.accepted_requests.insert(request);
                }
            }
            EnrollmentAction::DecideRequest(request, decision) => {
                if self.decision_admissible(&request, &decision) {
                    if decision.kind == EnrollmentDecisionKind::Approved {
                        self.memberships
                            .retain(|membership| !Self::membership_owned_by(&request, membership));
                        self.applied_routes
                            .retain(|route| !Self::route_owned_by(&request, route));
                        self.authorizations
                            .insert(Self::revision_for_approval(&request, &decision));
                    }
                    self.decisions.insert(decision);
                }
            }
            EnrollmentAction::MaterializeMembership(request, decision) => {
                if self.current_approval(&request, &decision) {
                    self.memberships
                        .insert(Self::membership_for(&request, &decision));
                }
            }
            EnrollmentAction::MaterializeRoutes(request, decision) => {
                if self.current_approval(&request, &decision)
                    && self
                        .memberships
                        .contains(&Self::membership_for(&request, &decision))
                {
                    self.applied_routes
                        .insert(Self::client_route(&request, &decision));
                    self.applied_routes
                        .insert(Self::server_route(&request, &decision));
                }
            }
            EnrollmentAction::Revoke(request, revision) => {
                if self.revoke_admissible(&request, &revision) {
                    self.authorizations.insert(revision);
                    self.memberships
                        .retain(|membership| !Self::membership_owned_by(&request, membership));
                    self.applied_routes
                        .retain(|route| !Self::route_owned_by(&request, route));
                }
            }
            EnrollmentAction::MergeAuthorization(revision) => {
                self.authorizations.insert(revision);
            }
        }
    }

    pub fn observed_offer_count(&self) -> usize {
        self.observed_offers.len()
    }

    pub fn admin_pin_count(&self) -> usize {
        self.admin_pins.len()
    }

    pub fn challenge_binding_count(&self) -> usize {
        self.challenge_bindings.len()
    }

    pub fn request_binding_count(&self) -> usize {
        self.request_bindings.len()
    }

    pub fn admin_pin_present(&self, offer: &EnrollmentOffer) -> bool {
        self.admin_pins.contains(&Self::admin_pin_for(offer))
    }

    pub fn admin_pin_conflict(&self, offer: &EnrollmentOffer) -> bool {
        self.admin_pins
            .iter()
            .any(|pin| pin.network_id == offer.network_id && pin.admin_did != offer.admin_did)
    }

    pub fn challenge_binding_conflict(&self, request: &EnrollmentRequest) -> bool {
        let expected = Self::challenge_binding_for(request);
        self.challenge_bindings
            .iter()
            .any(|binding| binding.challenge == request.challenge && binding != &expected)
    }

    pub fn request_binding_conflict(&self, request: &EnrollmentRequest) -> bool {
        let expected = Self::request_binding_for(request);
        self.request_bindings
            .iter()
            .any(|binding| binding.request_id == request.request_id && binding != &expected)
    }

    pub fn revision_for_approval(
        request: &EnrollmentRequest,
        decision: &EnrollmentDecision,
    ) -> AuthorizationRevision {
        AuthorizationRevision {
            request_id: request.request_id.clone(),
            request_digest: request.digest.clone(),
            network_id: request.network_id.clone(),
            admin_did: request.admin_did.clone(),
            member_did: request.candidate_did.clone(),
            member_peer: request.candidate_peer.clone(),
            owner_agent: request.owner_agent.clone(),
            sequence: decision.authorization_sequence,
            kind: AuthorizationRevisionKind::Active,
            signer_did: decision.signer_did.clone(),
            admin_signed: decision.admin_signed,
        }
    }

    pub fn membership_for(
        request: &EnrollmentRequest,
        decision: &EnrollmentDecision,
    ) -> EnrollmentMembership {
        EnrollmentMembership {
            request_id: request.request_id.clone(),
            request_digest: request.digest.clone(),
            network_id: request.network_id.clone(),
            member_did: request.candidate_did.clone(),
            member_peer: request.candidate_peer.clone(),
            owner_agent: request.owner_agent.clone(),
            authorization_sequence: decision.authorization_sequence,
            active: true,
            admin_signed: true,
            fresh: true,
        }
    }

    pub fn client_route(
        request: &EnrollmentRequest,
        decision: &EnrollmentDecision,
    ) -> EnrollmentAppliedRoute {
        Self::route_for(request, decision, EnrollmentRouteDirection::ClientToServer)
    }

    pub fn server_route(
        request: &EnrollmentRequest,
        decision: &EnrollmentDecision,
    ) -> EnrollmentAppliedRoute {
        Self::route_for(request, decision, EnrollmentRouteDirection::ServerToClient)
    }

    pub fn current_approval(
        &self,
        request: &EnrollmentRequest,
        decision: &EnrollmentDecision,
    ) -> bool {
        let revision = Self::revision_for_approval(request, decision);
        self.accepted_requests.contains(request)
            && self.decisions.contains(decision)
            && decision.kind == EnrollmentDecisionKind::Approved
            && Self::decision_matches_request(request, decision)
            && decision.admin_signed
            && decision.fresh
            && self.network_admin_pair_pinned(&request.network_id, &request.admin_did)
            && self.unique_maximum_revision(&revision)
    }

    pub fn enrollment_ready(&self, request: &EnrollmentRequest) -> bool {
        self.decisions.iter().any(|decision| {
            self.current_approval(request, decision)
                && self
                    .memberships
                    .contains(&Self::membership_for(request, decision))
                && self
                    .applied_routes
                    .contains(&Self::client_route(request, decision))
                && self
                    .applied_routes
                    .contains(&Self::server_route(request, decision))
        })
    }

    pub fn hydration_admits(
        &self,
        request: &EnrollmentRequest,
        direction: EnrollmentRouteDirection,
    ) -> bool {
        let peer_id = match direction {
            EnrollmentRouteDirection::ClientToServer => request.candidate_peer.clone(),
            EnrollmentRouteDirection::ServerToClient => request.server_peer.clone(),
        };
        let hydration = HydrationRequest {
            request_key: request.request_id.clone(),
            peer_id,
            requester_did: request.candidate_did.clone(),
            agent_did: request.owner_agent.clone(),
            session_id: "session-1".to_string(),
        };
        let mut catalog = HydrationCatalog {
            selected_network_id: request.network_id.clone(),
            sessions: BTreeSet::from([SessionOwner {
                session_id: hydration.session_id.clone(),
                requester_did: hydration.requester_did.clone(),
                agent_did: hydration.agent_did.clone(),
            }]),
            ..HydrationCatalog::default()
        };
        for membership in &self.memberships {
            if membership.network_id == request.network_id
                && membership.active
                && membership.admin_signed
                && membership.fresh
                && self.membership_currently_authorized(membership)
            {
                catalog
                    .verified_active_memberships
                    .insert(VerifiedActiveMembership {
                        network_id: membership.network_id.clone(),
                        member_did: membership.member_did.clone(),
                    });
            }
        }
        for route in &self.applied_routes {
            if route.network_id == request.network_id
                && route.direction == direction
                && route.live
                && self.route_currently_authorized(direction, route)
            {
                catalog.applied_pairing_routes.insert(AppliedPairingRoute {
                    peer_id: route.peer.clone(),
                    requester_did: route.requester.clone(),
                    agent_did: route.agent.clone(),
                });
            }
        }
        matches!(
            decide_hydration(&hydration, &catalog),
            HydrationVerdict::Admit(_)
        )
    }

    fn admin_pin_for(offer: &EnrollmentOffer) -> NetworkAdminPin {
        NetworkAdminPin {
            network_id: offer.network_id.clone(),
            admin_did: offer.admin_did.clone(),
        }
    }

    fn admin_pin_admissible(&self, offer: &EnrollmentOffer) -> bool {
        self.observed_offers.contains(offer)
            && offer.admin_signed
            && offer.fresh
            && offer.server_ticket_peer == offer.server_peer
            && offer.resolved_server_did == offer.admin_did
            && !self.admin_pin_conflict(offer)
    }

    fn network_admin_pinned(&self, offer: &EnrollmentOffer) -> bool {
        self.admin_pin_present(offer) && !self.admin_pin_conflict(offer)
    }

    fn network_admin_pair_pinned(&self, network_id: &str, admin_did: &str) -> bool {
        self.admin_pins.contains(&NetworkAdminPin {
            network_id: network_id.to_string(),
            admin_did: admin_did.to_string(),
        }) && self
            .admin_pins
            .iter()
            .filter(|pin| pin.network_id == network_id)
            .all(|pin| pin.admin_did == admin_did)
    }

    fn request_admissible(&self, offer: &EnrollmentOffer, request: &EnrollmentRequest) -> bool {
        self.observed_offers.contains(offer)
            && self.network_admin_pinned(offer)
            && offer.admin_signed
            && offer.fresh
            && offer.schema_compatible
            && offer.server_ticket_peer == offer.server_peer
            && offer.resolved_server_did == offer.admin_did
            && request.candidate_signed
            && request.fresh
            && request.digest
                == canonical_enrollment_digest(request.canonical_text_fields().into_iter())
            && request.observed_candidate_peer == request.candidate_peer
            && request.resolved_candidate_did == request.candidate_did
            && request.candidate_ticket_peer == request.candidate_peer
            && offer.profile == "client"
            && Self::request_matches_offer(offer, request)
            && !self.challenge_binding_conflict(request)
            && !self.request_binding_conflict(request)
    }

    fn request_matches_offer(offer: &EnrollmentOffer, request: &EnrollmentRequest) -> bool {
        request.offer_id == offer.offer_id
            && request.challenge == offer.challenge
            && request.network_id == offer.network_id
            && request.admin_did == offer.admin_did
            && request.server_peer == offer.server_peer
            && request.owner_agent == offer.owner_agent
            && request.profile == offer.profile
    }

    fn decision_admissible(
        &self,
        request: &EnrollmentRequest,
        decision: &EnrollmentDecision,
    ) -> bool {
        self.accepted_requests.contains(request)
            && Self::decision_matches_request(request, decision)
            && decision.admin_signed
            && decision.fresh
            && self.network_admin_pair_pinned(&request.network_id, &request.admin_did)
            && !self
                .decisions
                .iter()
                .any(|current| current.request_id == request.request_id)
            && (decision.kind == EnrollmentDecisionKind::Denied
                || (decision.kind == EnrollmentDecisionKind::Approved
                    && !self.dominating_revision_exists(&Self::revision_for_approval(
                        request, decision,
                    ))))
    }

    fn decision_matches_request(
        request: &EnrollmentRequest,
        decision: &EnrollmentDecision,
    ) -> bool {
        decision.request_id == request.request_id
            && decision.request_digest == request.digest
            && decision.network_id == request.network_id
            && decision.admin_did == request.admin_did
            && decision.candidate_did == request.candidate_did
            && decision.candidate_peer == request.candidate_peer
            && decision.owner_agent == request.owner_agent
            && decision.signer_did == request.admin_did
    }

    fn revision_matches_request(
        request: &EnrollmentRequest,
        revision: &AuthorizationRevision,
    ) -> bool {
        revision.request_id == request.request_id
            && revision.request_digest == request.digest
            && revision.network_id == request.network_id
            && revision.admin_did == request.admin_did
            && revision.member_did == request.candidate_did
            && revision.member_peer == request.candidate_peer
            && revision.owner_agent == request.owner_agent
            && revision.signer_did == request.admin_did
    }

    fn same_member(left: &AuthorizationRevision, right: &AuthorizationRevision) -> bool {
        left.network_id == right.network_id && left.member_did == right.member_did
    }

    fn dominating_revision_exists(&self, revision: &AuthorizationRevision) -> bool {
        self.authorizations.iter().any(|current| {
            Self::same_member(current, revision) && revision.sequence <= current.sequence
        })
    }

    fn unique_maximum_revision(&self, revision: &AuthorizationRevision) -> bool {
        self.authorizations.contains(revision)
            && self.authorizations.iter().all(|other| {
                !Self::same_member(other, revision)
                    || other.sequence < revision.sequence
                    || other == revision
            })
    }

    fn revoke_admissible(
        &self,
        request: &EnrollmentRequest,
        revision: &AuthorizationRevision,
    ) -> bool {
        self.accepted_requests.contains(request)
            && Self::revision_matches_request(request, revision)
            && revision.kind == AuthorizationRevisionKind::Revoked
            && revision.admin_signed
            && !self.dominating_revision_exists(revision)
    }

    fn challenge_binding_for(request: &EnrollmentRequest) -> ChallengeBinding {
        ChallengeBinding {
            challenge: request.challenge.clone(),
            request_id: request.request_id.clone(),
            digest: request.digest.clone(),
        }
    }

    fn request_binding_for(request: &EnrollmentRequest) -> RequestBinding {
        RequestBinding {
            request_id: request.request_id.clone(),
            digest: request.digest.clone(),
        }
    }

    fn membership_owned_by(request: &EnrollmentRequest, membership: &EnrollmentMembership) -> bool {
        membership.network_id == request.network_id
            && membership.member_did == request.candidate_did
    }

    fn route_owned_by(request: &EnrollmentRequest, route: &EnrollmentAppliedRoute) -> bool {
        route.network_id == request.network_id && route.requester == request.candidate_did
    }

    fn membership_currently_authorized(&self, membership: &EnrollmentMembership) -> bool {
        self.accepted_requests.iter().any(|request| {
            self.decisions.iter().any(|decision| {
                self.current_approval(request, decision)
                    && membership == &Self::membership_for(request, decision)
            })
        })
    }

    fn route_currently_authorized(
        &self,
        direction: EnrollmentRouteDirection,
        route: &EnrollmentAppliedRoute,
    ) -> bool {
        self.accepted_requests.iter().any(|request| {
            self.decisions.iter().any(|decision| {
                self.current_approval(request, decision)
                    && self
                        .memberships
                        .contains(&Self::membership_for(request, decision))
                    && route == &Self::route_for(request, decision, direction)
            })
        })
    }

    fn route_for(
        request: &EnrollmentRequest,
        decision: &EnrollmentDecision,
        direction: EnrollmentRouteDirection,
    ) -> EnrollmentAppliedRoute {
        let peer = match direction {
            EnrollmentRouteDirection::ClientToServer => request.candidate_peer.clone(),
            EnrollmentRouteDirection::ServerToClient => request.server_peer.clone(),
        };
        EnrollmentAppliedRoute {
            request_id: request.request_id.clone(),
            network_id: request.network_id.clone(),
            authorization_sequence: decision.authorization_sequence,
            direction,
            peer,
            requester: request.candidate_did.clone(),
            agent: request.owner_agent.clone(),
            profile: request.profile.clone(),
            live: true,
        }
    }
}
