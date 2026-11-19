use base64::Engine as _;
use gents_protocol::delegation_audience::{
    DelegationAudience, ReportedDelegationAudience, STATUS_DELEGATION_AUDIENCE_FIELD,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{
    ClientSigningCapability, ConfigClientError, DelegationError, GraphqlEndpoint,
    VERA_DELEGATION_LIFETIME,
};

const URL: &str = "http://127.0.0.1:1/api/v0/graphql";
const CALLER_BEARER: &str = "Bearer presented-by-the-caller";
const DELEGATION_TYPE: &str = "vera-delegation-v1+jwt";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Credential {
    Anonymous,
    CallerBearer,
    SignerRequired,
    IdentityToken,
    Delegation,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Reported {
    Absent,
    Named,
    Unavailable,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Resolution {
    IdentityToken,
    Delegation,
    Refused,
}

fn held_key() -> ClientSigningCapability {
    let keys = tempfile::tempdir().unwrap();
    crate::identity::KeyIdentity::load_or_create(keys.path().join("user.key"), None)
        .unwrap()
        .client_signing_capability()
        .unwrap()
}

fn audience() -> DelegationAudience {
    DelegationAudience {
        worker: "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK".into(),
        deployment: 4_503_599_627_370_495,
        genesis_id: None,
    }
}

fn segment(token: &str, index: usize) -> Value {
    let segment = token
        .split('.')
        .nth(index)
        .expect("a JWT has three segments");
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(segment)
        .expect("a JWT segment is base64url");
    serde_json::from_slice(&bytes).expect("a JWT segment is JSON")
}

fn presented(endpoint: &GraphqlEndpoint, actor: &str) -> Credential {
    match endpoint.authorization() {
        Ok(None) => Credential::Anonymous,
        Ok(Some(bearer)) if bearer == CALLER_BEARER => Credential::CallerBearer,
        Ok(Some(bearer)) => {
            let token = bearer.strip_prefix("Bearer ").expect("a bearer");
            if segment(token, 0)["typ"] != DELEGATION_TYPE {
                return Credential::IdentityToken;
            }
            let claims = segment(token, 1);
            let node = audience();
            assert_eq!(claims["iss"], actor);
            assert_eq!(claims["sub"], node.worker);
            assert_eq!(claims["aud"], format!("vera:{}", node.deployment));
            assert_eq!(
                claims["exp"].as_u64().unwrap() - claims["iat"].as_u64().unwrap(),
                VERA_DELEGATION_LIFETIME.as_secs()
            );
            Credential::Delegation
        }
        Err(ConfigClientError::ClientAgentSignerRequired) => Credential::SignerRequired,
        Err(error) => panic!("{error}"),
    }
}

#[test]
fn the_minted_credential_matches_the_lean_model() {
    let key = held_key();
    gents_lean_vocab::families::identity_user_delegation_credential::replay::<Credential>(
        |input| {
            let endpoint = if input.forwards_caller_bearer {
                GraphqlEndpoint::with_delegated_authorization(URL, CALLER_BEARER).unwrap()
            } else if !input.principal {
                GraphqlEndpoint::anonymous(URL)
            } else if input.holds_key {
                GraphqlEndpoint::as_principal(URL, key.did())
                    .with_client_signing_capability(key.clone())
                    .unwrap()
            } else {
                GraphqlEndpoint::as_principal(URL, key.did())
            };
            let reported = if input.node_names_audience {
                ReportedDelegationAudience::Named(audience())
            } else {
                ReportedDelegationAudience::Absent
            };
            let endpoint = endpoint
                .with_reported_delegation_audience(reported)
                .unwrap();
            presented(&endpoint, key.did())
        },
    );
}

#[test]
fn resolving_an_endpoint_matches_the_lean_model() {
    let key = held_key();
    gents_lean_vocab::families::identity_user_delegation_resolve::replay::<Reported, Resolution>(
        |input| {
            let status = match input.reported {
                Reported::Absent => json!({ "service": "gents" }),
                Reported::Named => json!({ STATUS_DELEGATION_AUDIENCE_FIELD: audience() }),
                Reported::Unavailable => json!({ STATUS_DELEGATION_AUDIENCE_FIELD: null }),
            };
            let resolved = GraphqlEndpoint::as_principal(URL, key.did())
                .with_client_signing_capability(key.clone())
                .unwrap()
                .with_reported_delegation_audience(ReportedDelegationAudience::observe(&status));
            match resolved {
                Ok(endpoint) => match presented(&endpoint, key.did()) {
                    Credential::IdentityToken => Resolution::IdentityToken,
                    Credential::Delegation => Resolution::Delegation,
                    other => panic!("{other:?}"),
                },
                Err(ConfigClientError::Delegation(DelegationError::AudienceUnavailable)) => {
                    Resolution::Refused
                }
                Err(error) => panic!("{error}"),
            }
        },
    );
}
