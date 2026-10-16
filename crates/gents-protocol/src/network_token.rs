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

/// Canonical signing form of the enrollment server's `Network` root.
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
