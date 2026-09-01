//! Canonical signing payloads retained by authenticated enrollment.

use serde::{Deserialize, Serialize};

pub fn derive_network_id(admin_did: &str, name: &str) -> String {
    format!("net-{}", digest16_base58(admin_did, name))
}

fn digest16_base58(left: &str, right: &str) -> String {
    use sha2::{Digest, Sha256};

    let mut hash = Sha256::new();
    hash.update(left.as_bytes());
    hash.update(b"\x1f");
    hash.update(right.as_bytes());
    let digest = hash.finalize();
    bs58::encode(&digest[..16]).into_string()
}

/// Canonical signing form of the enrollment server's `AgentNetwork` root.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkRecord {
    pub network_id: String,
    pub admin_did: String,
    pub display_name: String,
    pub default_template: String,
    pub created_at: String,
    pub sig: Vec<u8>,
}

/// Canonical signing form of a transport endpoint observation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointRecord {
    pub did: String,
    pub node_id: String,
    pub address: String,
    pub updated_at: String,
    pub sig: Vec<u8>,
}

macro_rules! signing_payload_impl {
    ($t:ty) => {
        impl $t {
            pub fn signing_payload(&self) -> Vec<u8> {
                let mut unsigned = self.clone();
                unsigned.sig = Vec::new();
                let mut bytes = Vec::new();
                ciborium::ser::into_writer(&unsigned, &mut bytes)
                    .expect("CBOR serialisation of signing payload is infallible");
                bytes
            }
        }
    };
}

signing_payload_impl!(NetworkRecord);
signing_payload_impl!(EndpointRecord);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_id_is_deterministic_and_admin_bound() {
        let a = derive_network_id("did:key:zAdmin", "Fleet One");
        assert_eq!(a, derive_network_id("did:key:zAdmin", "Fleet One"));
        assert_ne!(a, derive_network_id("did:key:zOther", "Fleet One"));
        assert_ne!(a, derive_network_id("did:key:zAdmin", "Fleet Two"));
        assert!(a.starts_with("net-"));
    }

    #[test]
    fn signing_payloads_exclude_signature_and_cover_content() {
        let mut network = NetworkRecord {
            network_id: "network-1".into(),
            admin_did: "did:key:admin".into(),
            display_name: "Default".into(),
            default_template: "client".into(),
            created_at: "2026-06-15T00:00:00Z".into(),
            sig: vec![1],
        };
        let baseline = network.signing_payload();
        network.sig = vec![2];
        assert_eq!(baseline, network.signing_payload());
        network.network_id = "network-2".into();
        assert_ne!(baseline, network.signing_payload());

        let mut endpoint = EndpointRecord {
            did: "did:key:member".into(),
            node_id: "peer-1".into(),
            address: "ticket-1".into(),
            updated_at: "2026-06-15T00:00:00Z".into(),
            sig: vec![1],
        };
        let baseline = endpoint.signing_payload();
        endpoint.sig = vec![2];
        assert_eq!(baseline, endpoint.signing_payload());
        endpoint.address = "ticket-2".into();
        assert_ne!(baseline, endpoint.signing_payload());
    }
}
