#[derive(Debug, thiserror::Error)]
pub enum EnrollmentError {
    #[error("unsupported enrollment request version {version}")]
    UnsupportedRequestVersion { version: u8 },
    #[error("request offer_id mismatch")]
    RequestOfferIdMismatch,
    #[error("request challenge mismatch")]
    RequestChallengeMismatch,
    #[error("request network_id mismatch")]
    RequestNetworkIdMismatch,
    #[error("request admin_did mismatch")]
    RequestAdminDidMismatch,
    #[error("request server_peer mismatch")]
    RequestServerPeerMismatch,
    #[error("request owner_agent mismatch")]
    RequestOwnerAgentMismatch,
    #[error("request profile mismatch")]
    RequestProfileMismatch,
    #[error("request cannot extend the signed offer lifetime")]
    RequestExtendsOffer,
    #[error("request digest mismatch")]
    RequestDigestMismatch,
    #[error("request_id mismatch")]
    RequestIdMismatch,
    #[error("enrollment offer {name} is empty")]
    OfferFieldEmpty { name: &'static str },
    #[error("enrollment offer {name} has whitespace")]
    OfferFieldWhitespace { name: &'static str },
    #[error("enrollment offer {name} exceeds the size limit")]
    OfferFieldTooLarge { name: &'static str },
    #[error("invalid enrollment request signature length")]
    RequestSignatureLength,
    #[error("request issuance is outside the signed offer window")]
    RequestOutsideOfferWindow,
    #[error("unsupported operator decision command version {version}")]
    UnsupportedOperatorDecisionVersion { version: u8 },
    #[error("operator decision command {name} contains control characters")]
    OperatorDecisionControlCharacters { name: &'static str },
    #[error("invalid operator decision command signature length")]
    OperatorDecisionSignatureLength,
    #[error("approval lease_seconds is outside the supported range")]
    ApprovalLeaseOutOfRange,
    #[error("deny/revoke command cannot allocate a lease")]
    DenyOrRevokeWithLease,
    #[error("operator decision command is issued in the future")]
    OperatorDecisionFromFuture,
    #[error("operator decision command expired")]
    OperatorDecisionExpired,
    #[error("{name} must use canonical UTC form")]
    TimestampNotUtc { name: &'static str },
    #[error("{name} must use canonical UTC seconds form")]
    TimestampNotCanonical { name: &'static str },
    #[error("parsing {name}")]
    TimestampParse {
        name: &'static str,
        #[source]
        source: chrono::ParseError,
    },
    #[error("unsupported operator query command version {version}")]
    UnsupportedOperatorQueryVersion { version: u8 },
    #[error("operator query command {name} contains control characters")]
    OperatorQueryControlCharacters { name: &'static str },
    #[error("invalid operator query command signature length")]
    OperatorQuerySignatureLength,
    #[error("operator query command is issued in the future")]
    OperatorQueryFromFuture,
    #[error("operator query command expired")]
    OperatorQueryExpired,
    #[error("unsupported enrollment decision version {version}")]
    UnsupportedDecisionVersion { version: u8 },
    #[error("decision request_id mismatch")]
    DecisionRequestIdMismatch,
    #[error("decision request_digest mismatch")]
    DecisionRequestDigestMismatch,
    #[error("decision network_id mismatch")]
    DecisionNetworkIdMismatch,
    #[error("decision admin_did mismatch")]
    DecisionAdminDidMismatch,
    #[error("decision candidate_did mismatch")]
    DecisionCandidateDidMismatch,
    #[error("decision candidate_peer mismatch")]
    DecisionCandidatePeerMismatch,
    #[error("decision owner_agent mismatch")]
    DecisionOwnerAgentMismatch,
    #[error("decision signer mismatch")]
    DecisionSignerMismatch,
    #[error("decision_id mismatch")]
    DecisionIdMismatch,
    #[error("approved decision requires a positive authorization sequence")]
    ApprovedWithoutSequence,
    #[error("approval lease must expire after its decision")]
    ApprovalLeaseNotAfterDecision,
    #[error("approval lease exceeds the supported maximum")]
    ApprovalLeaseTooLong,
    #[error("denied decision cannot allocate an authorization sequence")]
    DeniedWithSequence,
    #[error("denied decision must use its decision time as its non-leased boundary")]
    DeniedLeaseNotDecision,
    #[error("decision authorization sequence exceeds the DefraDB Int range")]
    DecisionSequenceOverflow,
    #[error("invalid decision signature length")]
    DecisionSignatureLength,
    #[error("decision is outside the signed request window")]
    DecisionOutsideRequestWindow,
    #[error("unsupported authorization revision version {version}")]
    UnsupportedRevisionVersion { version: u8 },
    #[error("authorization revision requires an approved decision")]
    RevisionNotApproved,
    #[error("revision request_id mismatch")]
    RevisionRequestIdMismatch,
    #[error("revision request_digest mismatch")]
    RevisionRequestDigestMismatch,
    #[error("revision network_id mismatch")]
    RevisionNetworkIdMismatch,
    #[error("revision admin_did mismatch")]
    RevisionAdminDidMismatch,
    #[error("revision member_did mismatch")]
    RevisionMemberDidMismatch,
    #[error("revision member_peer mismatch")]
    RevisionMemberPeerMismatch,
    #[error("revision owner_agent mismatch")]
    RevisionOwnerAgentMismatch,
    #[error("revision signer mismatch")]
    RevisionSignerMismatch,
    #[error("authorization revision sequence must be positive")]
    RevisionSequenceNotPositive,
    #[error("authorization revision sequence exceeds the DefraDB Int range")]
    RevisionSequenceOverflow,
    #[error("revision authorization lease mismatch")]
    RevisionLeaseMismatch,
    #[error("revision_id mismatch")]
    RevisionIdMismatch,
    #[error("active revision sequence must equal its approval")]
    ActiveSequenceMismatch,
    #[error("active revision timestamp must equal its approval")]
    ActiveTimestampMismatch,
    #[error("revocation must dominate its approval")]
    RevocationNotDominant,
    #[error("revocation cannot predate its approval")]
    RevocationBeforeApproval,
    #[error("invalid revision signature length")]
    RevisionSignatureLength,
    #[error("unsupported enrollment route receipt version {version}")]
    UnsupportedRouteReceiptVersion { version: u8 },
    #[error("route receipt requires an approved decision")]
    RouteReceiptNotApproved,
    #[error("receipt request_id mismatch")]
    ReceiptRequestIdMismatch,
    #[error("receipt request_digest mismatch")]
    ReceiptRequestDigestMismatch,
    #[error("receipt network_id mismatch")]
    ReceiptNetworkIdMismatch,
    #[error("receipt admin_did mismatch")]
    ReceiptAdminDidMismatch,
    #[error("receipt member_did mismatch")]
    ReceiptMemberDidMismatch,
    #[error("receipt member_peer mismatch")]
    ReceiptMemberPeerMismatch,
    #[error("receipt server_peer mismatch")]
    ReceiptServerPeerMismatch,
    #[error("receipt owner_agent mismatch")]
    ReceiptOwnerAgentMismatch,
    #[error("receipt authorization sequence mismatch")]
    ReceiptSequenceMismatch,
    #[error("receipt authorization lease mismatch")]
    ReceiptLeaseMismatch,
    #[error("receipt signer mismatch")]
    ReceiptSignerMismatch,
    #[error("receipt_id mismatch")]
    ReceiptIdMismatch,
    #[error("route receipt cannot predate approval")]
    RouteReceiptBeforeApproval,
    #[error("route receipt cannot be issued after lease expiry")]
    RouteReceiptAfterLease,
    #[error("invalid route receipt signature length")]
    RouteReceiptSignatureLength,
    #[error("encoding enrollment offer")]
    EncodeOffer(#[source] ciborium::ser::Error<std::io::Error>),
    #[error("decoding enrollment offer")]
    DecodeOfferBase58(#[source] bs58::decode::Error),
    #[error("parsing enrollment offer")]
    ParseOffer(#[source] ciborium::de::Error<std::io::Error>),
    #[error("enrollment offer is empty")]
    OfferEmpty,
    #[error("enrollment offer has whitespace")]
    OfferWhitespace,
    #[error("enrollment offer exceeds the size limit")]
    OfferTooLarge,
    #[error("decoded enrollment offer exceeds the size limit")]
    DecodedOfferTooLarge,
    #[error("enrollment offer contains trailing data")]
    OfferTrailingData,
    #[error("unsupported enrollment offer version {version}")]
    UnsupportedOfferVersion { version: u8 },
    #[error("unsupported enrollment profile")]
    UnsupportedOfferProfile,
    #[error("invalid enrollment offer signature length")]
    OfferSignatureLength,
}

pub(super) type Result<T, E = EnrollmentError> = std::result::Result<T, E>;
