use gents_lean_vocab::families::identity_held_audience_home_statuses::{ReportCase, ReportOutput};
use gents_protocol::client_authentication::ClientAuthentication;
use gents_protocol::delegation_audience::{DelegationAudience, ReportedDelegationAudience};
use gents_protocol::home_attestation::{
    AttestedAudience, HomeAttestation, HomeNonce, HomeStatement, HOME_ATTESTATION_VERSION,
    STATUS_HOME_ATTESTATION_FIELD,
};
use serde::{Deserialize, Serialize};

use crate::identity::NodeIdentity;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Reported {
    Absent,
    Named,
    Unavailable,
}

const WORKER: &str = "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK";

fn nonce(number: u64) -> HomeNonce {
    HomeNonce::from_bytes([u8::try_from(number).expect("a small case nonce"); 32])
}

fn statement(node: &str, nonce: &HomeNonce, report: &ReportCase<Reported>) -> HomeStatement {
    let audience = DelegationAudience {
        worker: WORKER.into(),
        deployment: report.deployment,
        genesis_id: None,
    };
    let (client_authentication, delegation_audience) = match report.reported {
        Reported::Absent => (ClientAuthentication::Local, AttestedAudience::Absent),
        Reported::Named => (
            ClientAuthentication::Vera {
                shared_household: false,
                worker: WORKER.into(),
                deployment: report.deployment,
            },
            AttestedAudience::Named { audience },
        ),
        Reported::Unavailable => (
            ClientAuthentication::Unavailable,
            AttestedAudience::Unavailable,
        ),
    };
    HomeStatement {
        version: HOME_ATTESTATION_VERSION,
        nonce: nonce.as_str().to_owned(),
        node_did: node.to_owned(),
        client_authentication,
        delegation_audience,
        provider_time: report.provider_time,
    }
}

/// Another kind of statement the node key signs over the same fields.
fn other_statement_payload(statement: &HomeStatement) -> Vec<u8> {
    let payload = statement.signing_payload();
    let mut other = gents_protocol::enrollment::canonical_domain_payload(
        "gents-account-usage-read-v1",
        std::iter::empty(),
    );
    other.extend_from_slice(&payload);
    other
}

#[tokio::test]
async fn a_status_counts_for_its_home_as_the_lean_model_says() {
    let (home, other) = tokio::task::spawn_blocking(|| {
        (
            crate::test_support::test_identity("status-home"),
            crate::test_support::test_identity("status-other"),
        )
    })
    .await
    .unwrap();
    let mut cases = Vec::new();
    for case in
        gents_lean_vocab::families::identity_held_audience_home_statuses::cases::<Reported>()
    {
        let input = &case.input;
        let node = if input.names_home { &home } else { &other };
        let mut status = serde_json::json!({});
        let shown = statement(node.did(), &nonce(1), &input.report);
        if let Some(signature) = &input.signature {
            let signer = if signature.by_home { &home } else { &other };
            let named = if signature.names_home { &home } else { &other };
            let covered = statement(named.did(), &nonce(signature.nonce), &signature.covers);
            let payload = if signature.home_status {
                covered.signing_payload()
            } else {
                other_statement_payload(&covered)
            };
            let signed = signer.sign(&payload).await.unwrap();
            status[STATUS_HOME_ATTESTATION_FIELD] =
                serde_json::json!(HomeAttestation::signed(shown, &signed));
        }
        cases.push((case, status));
    }
    for (case, status) in cases {
        let reported = super::authenticate(&status, home.did(), &nonce(1))
            .map(|home| home.reported().clone())
            .unwrap_or(gents_protocol::delegation_audience::ReportedHome {
                audience: ReportedDelegationAudience::Unavailable,
                provider_time: None,
            });
        case.check(ReportOutput {
            reported: match &reported.audience {
                ReportedDelegationAudience::Absent => Reported::Absent,
                ReportedDelegationAudience::Named(_) => Reported::Named,
                ReportedDelegationAudience::Unavailable => Reported::Unavailable,
            },
            deployment: match &reported.audience {
                ReportedDelegationAudience::Named(audience) => Some(audience.deployment),
                _ => None,
            },
            provider_time: reported.provider_time,
        });
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Advertised {
    Local,
    Vera,
    Unavailable,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Reconciled {
    Kept,
    Unavailable,
    Contradictory,
}

#[test]
fn a_signed_status_is_reconciled_as_the_lean_model_says() {
    gents_lean_vocab::families::identity_held_audience_reconcile::replay::<
        Advertised,
        Reconciled,
        Reported,
    >(|input| {
        let authentication = match input.advertised {
            Advertised::Local => ClientAuthentication::Local,
            Advertised::Vera => ClientAuthentication::Vera {
                shared_household: false,
                worker: WORKER.into(),
                deployment: 2,
            },
            Advertised::Unavailable => ClientAuthentication::Unavailable,
        };
        let audience = match input.stated {
            Reported::Absent => ReportedDelegationAudience::Absent,
            Reported::Named => ReportedDelegationAudience::Named(DelegationAudience {
                worker: WORKER.into(),
                deployment: if input.same_deployment { 2 } else { 3 },
                genesis_id: Some("0x01".into()),
            }),
            Reported::Unavailable => ReportedDelegationAudience::Unavailable,
        };
        let home = super::AuthenticatedHome {
            authentication,
            reported: gents_protocol::delegation_audience::ReportedHome {
                audience,
                provider_time: None,
            },
        };
        match home.resolved() {
            Ok(Some(_)) => Reconciled::Kept,
            Ok(None) => Reconciled::Unavailable,
            Err(gents_protocol::home_attestation::HomeStatusRefusal::Contradictory) => {
                Reconciled::Contradictory
            }
            Err(refusal) => panic!("reconciling refuses only a contradiction: {refusal}"),
        }
    });
}
