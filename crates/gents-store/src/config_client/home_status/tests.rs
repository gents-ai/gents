use gents_protocol::delegation_audience::{DelegationAudience, ReportedDelegationAudience};
use gents_protocol::home_attestation::{
    HomeAttestation, HomeNonce, HomeStatusRefusal, UnsignedHomeStatus,
    STATUS_HOME_ATTESTATION_FIELD,
};

use crate::identity::NodeIdentity;

fn named(deployment: u64) -> ReportedDelegationAudience {
    ReportedDelegationAudience::Named(DelegationAudience {
        worker: "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK".into(),
        deployment,
        genesis_id: Some(format!("0x{}", "ab".repeat(32))),
    })
}

async fn home() -> crate::identity::KeyIdentity {
    tokio::task::spawn_blocking(|| crate::test_support::test_identity("signing-home"))
        .await
        .unwrap()
}

/// What a test alters of a signed statement, and how.
type Alteration = (&'static str, fn(&mut serde_json::Value));

fn statement_field(status: &mut serde_json::Value) -> &mut serde_json::Value {
    &mut status[STATUS_HOME_ATTESTATION_FIELD]["statement"]
}

#[tokio::test]
async fn a_signed_status_keeps_exactly_what_the_home_signed() {
    let home = home().await;
    let nonce = super::fresh_nonce();
    let status =
        crate::test_support::signed_home_status(&home, &named(7), Some(1_000), &nonce).await;
    let kept = super::authenticate(&status, home.did(), &nonce).unwrap();
    assert_eq!(kept.reported().audience, named(7));
    assert_eq!(kept.reported().provider_time, Some(1_000));
    assert_ne!(
        super::fresh_nonce(),
        nonce,
        "every read sends its own nonce"
    );
}

#[tokio::test]
async fn a_status_altered_after_signing_is_refused() {
    let home = home().await;
    let nonce = super::fresh_nonce();
    let signed =
        crate::test_support::signed_home_status(&home, &named(7), Some(1_000), &nonce).await;
    let alterations: [Alteration; 5] = [
        ("deployment", |statement| {
            statement["delegation_audience"]["audience"]["deployment"] = 8.into();
        }),
        ("genesis", |statement| {
            statement["delegation_audience"]["audience"]["genesis_id"] = serde_json::Value::Null;
        }),
        ("clock", |statement| {
            statement["provider_time"] = 2_000.into()
        }),
        ("authentication", |statement| {
            statement["client_authentication"] = serde_json::json!({"kind": "local"});
        }),
        ("audience", |statement| {
            statement["delegation_audience"] = serde_json::json!({"state": "absent"});
        }),
    ];
    for (altered, alter) in alterations {
        let mut status = signed.clone();
        alter(statement_field(&mut status));
        assert_eq!(
            super::authenticate(&status, home.did(), &nonce),
            Err(HomeStatusRefusal::BadSignature),
            "{altered}"
        );
    }
}

#[tokio::test]
async fn a_status_is_refused_for_each_reason_it_is_not_the_homes() {
    let home = home().await;
    let other = tokio::task::spawn_blocking(|| crate::test_support::test_identity("other-home"))
        .await
        .unwrap();
    let nonce = super::fresh_nonce();
    let signed =
        crate::test_support::signed_home_status(&home, &named(7), Some(1_000), &nonce).await;

    let mut unsigned = signed.clone();
    unsigned
        .as_object_mut()
        .unwrap()
        .remove(STATUS_HOME_ATTESTATION_FIELD);
    assert_eq!(
        super::authenticate(&unsigned, home.did(), &nonce),
        Err(HomeStatusRefusal::NotSigned)
    );

    assert_eq!(
        super::authenticate(&signed, home.did(), &super::fresh_nonce()),
        Err(HomeStatusRefusal::OtherNonce),
        "a status signed for an earlier read is not answered again"
    );
    assert_eq!(
        super::authenticate(&signed, other.did(), &nonce),
        Err(HomeStatusRefusal::OtherNode)
    );

    let mut forged =
        crate::test_support::signed_home_status(&other, &named(7), Some(1_000), &nonce).await;
    statement_field(&mut forged)["node_did"] = home.did().into();
    assert_eq!(
        super::authenticate(&forged, home.did(), &nonce),
        Err(HomeStatusRefusal::BadSignature)
    );

    let mut version = signed.clone();
    statement_field(&mut version)["version"] = 2.into();
    assert_eq!(
        super::authenticate(&version, home.did(), &nonce),
        Err(HomeStatusRefusal::OtherVersion(2))
    );

    let mut malformed = signed.clone();
    malformed[STATUS_HOME_ATTESTATION_FIELD]["signature"] = "not base64!".into();
    assert_eq!(
        super::authenticate(&malformed, home.did(), &nonce),
        Err(HomeStatusRefusal::Malformed)
    );

    for reason in [
        UnsignedHomeStatus::RuntimeNotReady,
        UnsignedHomeStatus::NodeKeyUnavailable,
    ] {
        let mut cannot = signed.clone();
        cannot[STATUS_HOME_ATTESTATION_FIELD] =
            serde_json::json!(HomeAttestation::Unsigned { reason });
        assert_eq!(
            super::authenticate(&cannot, home.did(), &nonce),
            Err(HomeStatusRefusal::Unsigned(reason))
        );
    }
}

#[tokio::test]
async fn a_home_whose_runtime_has_not_started_says_it_cannot_sign() {
    let nonce = super::fresh_nonce();
    let mut status = serde_json::json!({ "node_did": "did:key:home" });
    crate::identity::home_status::attest(None, &nonce, &mut status).await;
    assert_eq!(
        HomeAttestation::of_status(&status),
        Ok(HomeAttestation::Unsigned {
            reason: UnsignedHomeStatus::RuntimeNotReady
        })
    );
}

#[test]
fn a_nonce_is_32_bytes_of_unpadded_base64url() {
    let nonce = HomeNonce::from_bytes([1; 32]);
    assert_eq!(HomeNonce::parse(nonce.as_str()), Ok(nonce.clone()));
    for malformed in [
        "",
        "short",
        &format!("{}=", nonce.as_str()),
        &nonce.as_str()[1..],
        &"A".repeat(44),
    ] {
        assert_eq!(
            HomeNonce::parse(malformed),
            Err(HomeStatusRefusal::Malformed),
            "{malformed:?}"
        );
    }
}
