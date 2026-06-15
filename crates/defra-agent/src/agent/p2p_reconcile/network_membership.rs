//! Pure network-membership decision functions: the Rust mirror of the
//! membership / endpoint trust predicates in the Lean model
//! `Proofs/PeerRegistryDiscovery/` (`Transition.lean`).
//!
//! These are *pure decision functions* — no DB, no GraphQL, no reconciler.
//! Each fn is a boolean conjunction matching its Lean predicate exactly, and is
//! fenced by the conformance suite. The single-row vs. existential split mirrors
//! the Lean shape: `admittedMember` is existential over the membership set, so
//! the per-row trust check ([`membership_admits_did`]) is factored out from the
//! existential wrapper ([`admitted_member`]).
//!
//! Mirrored Lean predicates:
//! - [`valid_network`] ↔ `validNetwork`
//! - [`admin_signed_membership`] ↔ `adminSignedMembership`
//! - [`membership_admits_did`] ↔ the per-row body of `admittedMember`'s ∃
//! - [`admitted_member`] ↔ `admittedMember` (existential over memberships)
//! - [`member_signed_endpoint`] ↔ `memberSignedEndpoint`
//! - [`materializable_endpoint`] ↔ `materializableEndpoint`

/// The network record as seen by the membership trust predicates. Mirrors the
/// fields of the Lean `Network` that the membership predicates read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkDecision {
    /// The network admin's DID (`Network.adminDid`).
    pub admin_did: String,
    /// Whether the network record's admin self-attestation verifies
    /// (`Network.adminSigValid`).
    pub admin_sig_valid: bool,
}

/// One membership row as seen by the membership trust predicates. Mirrors the
/// fields of the Lean `Membership` the predicates read. `network_match` models
/// the Lean `m.networkId = n.networkId` equality (resolved by the caller).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MembershipDecision {
    /// The DID this membership row is for (`Membership.memberDid`).
    pub member_did: String,
    /// Whether this row's network id equals the network's (`m.networkId =
    /// n.networkId`).
    pub network_match: bool,
    /// Whether the membership is active (`Membership.active`).
    pub active: bool,
    /// Whether the membership's admin signature verifies
    /// (`Membership.adminSigValid`).
    pub admin_sig_valid: bool,
    /// The DID that signed this membership row (`Membership.signedBy`).
    pub signed_by: String,
}

/// One endpoint announcement as seen by the endpoint trust predicates. Mirrors
/// the fields of the Lean `Endpoint` the predicates read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointDecision {
    /// The announcing node's DID (`Endpoint.did`).
    pub did: String,
    /// The announced peer address (`Endpoint.peer`).
    pub peer: String,
    /// Whether the endpoint's member self-signature verifies
    /// (`Endpoint.memberSigValid`).
    pub member_sig_valid: bool,
    /// Whether the announcement is fresh — heartbeat within window
    /// (`Endpoint.fresh`).
    pub fresh: bool,
    /// Whether the announced peer is self (`ep.peer = s.self`).
    pub peer_is_self: bool,
}

/// Mirrors Lean `PeerRegistryDiscovery.validNetwork`:
/// `validNetwork n := n.adminSigValid = true`.
pub fn valid_network(n: &NetworkDecision) -> bool {
    n.admin_sig_valid
}

/// Mirrors Lean `PeerRegistryDiscovery.adminSignedMembership`:
/// `adminSignedMembership m n := m.adminSigValid = true ∧ m.signedBy = n.adminDid
/// ∧ m.networkId = n.networkId`. (`network_match` models the networkId equality.)
pub fn admin_signed_membership(n: &NetworkDecision, m: &MembershipDecision) -> bool {
    m.admin_sig_valid && m.signed_by == n.admin_did && m.network_match
}

/// Mirrors the per-row body of the existential in Lean
/// `PeerRegistryDiscovery.admittedMember`: a single membership row admits `did`
/// iff it is admin-signed for `n`, active, and is for `did`
/// (`adminSignedMembership m n ∧ m.active = true ∧ m.memberDid = did`).
pub fn membership_admits_did(n: &NetworkDecision, m: &MembershipDecision, did: &str) -> bool {
    admin_signed_membership(n, m) && m.active && m.member_did == did
}

/// Mirrors Lean `PeerRegistryDiscovery.admittedMember`:
/// `admittedMember did s := validNetwork s.network ∧ ∃ m ∈ s.memberships,
/// m.memberDid = did ∧ m.active = true ∧ adminSignedMembership m s.network`.
/// The existential is realized as `iter().any(..)` over the membership slice.
pub fn admitted_member(n: &NetworkDecision, memberships: &[MembershipDecision], did: &str) -> bool {
    valid_network(n) && memberships.iter().any(|m| membership_admits_did(n, m, did))
}

/// Mirrors Lean `PeerRegistryDiscovery.memberSignedEndpoint`:
/// `memberSignedEndpoint ep := ep.memberSigValid = true ∧ ep.fresh = true`.
pub fn member_signed_endpoint(ep: &EndpointDecision) -> bool {
    ep.member_sig_valid && ep.fresh
}

/// Mirrors Lean `PeerRegistryDiscovery.materializableEndpoint`:
/// `materializableEndpoint ep s := admittedMember ep.did s ∧ memberSignedEndpoint
/// ep ∧ ep.peer ≠ s.self`.
pub fn materializable_endpoint(
    n: &NetworkDecision,
    memberships: &[MembershipDecision],
    ep: &EndpointDecision,
) -> bool {
    admitted_member(n, memberships, &ep.did) && member_signed_endpoint(ep) && !ep.peer_is_self
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admitted_is_existential_over_signed_active_memberships() {
        let net = NetworkDecision { admin_did: "did:a".into(), admin_sig_valid: true };
        let good = MembershipDecision {
            member_did: "did:x".into(),
            network_match: true,
            active: true,
            admin_sig_valid: true,
            signed_by: "did:a".into(),
        };
        assert!(membership_admits_did(&net, &good, "did:x"));
        assert!(!membership_admits_did(
            &net,
            &MembershipDecision { signed_by: "did:evil".into(), ..good.clone() },
            "did:x"
        ));
        assert!(!membership_admits_did(
            &net,
            &MembershipDecision { active: false, ..good.clone() },
            "did:x"
        ));
        assert!(!admitted_member(&net, &[], "did:x"));
        assert!(admitted_member(&net, &[good.clone()], "did:x"));
        assert!(!admitted_member(&net, &[good.clone()], "did:other"));
        assert!(!admitted_member(
            &NetworkDecision { admin_sig_valid: false, ..net.clone() },
            &[good],
            "did:x"
        ));
    }

    #[test]
    fn network_match_and_member_signature_gate_admission() {
        let net = NetworkDecision { admin_did: "did:a".into(), admin_sig_valid: true };
        let base = MembershipDecision {
            member_did: "did:x".into(),
            network_match: true,
            active: true,
            admin_sig_valid: true,
            signed_by: "did:a".into(),
        };
        // networkId mismatch denies even an active, admin-signed row.
        assert!(!membership_admits_did(
            &net,
            &MembershipDecision { network_match: false, ..base.clone() },
            "did:x"
        ));
        // membership admin signature must verify.
        assert!(!membership_admits_did(
            &net,
            &MembershipDecision { admin_sig_valid: false, ..base },
            "did:x"
        ));
    }

    #[test]
    fn materializable_requires_admitted_signed_and_not_self() {
        let net = NetworkDecision { admin_did: "did:a".into(), admin_sig_valid: true };
        let good = MembershipDecision {
            member_did: "did:x".into(),
            network_match: true,
            active: true,
            admin_sig_valid: true,
            signed_by: "did:a".into(),
        };
        let ep = EndpointDecision {
            did: "did:x".into(),
            peer: "peerX".into(),
            member_sig_valid: true,
            fresh: true,
            peer_is_self: false,
        };
        assert!(member_signed_endpoint(&ep));
        assert!(materializable_endpoint(&net, &[good.clone()], &ep));
        // not member-signed.
        assert!(!member_signed_endpoint(&EndpointDecision { member_sig_valid: false, ..ep.clone() }));
        assert!(!materializable_endpoint(
            &net,
            &[good.clone()],
            &EndpointDecision { member_sig_valid: false, ..ep.clone() }
        ));
        // not fresh.
        assert!(!materializable_endpoint(
            &net,
            &[good.clone()],
            &EndpointDecision { fresh: false, ..ep.clone() }
        ));
        // self peer.
        assert!(!materializable_endpoint(
            &net,
            &[good.clone()],
            &EndpointDecision { peer_is_self: true, ..ep.clone() }
        ));
        // announcing DID is not an admitted member.
        assert!(!materializable_endpoint(
            &net,
            &[good],
            &EndpointDecision { did: "did:other".into(), ..ep }
        ));
    }
}
