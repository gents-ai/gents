#[cfg(target_os = "macos")]
use std::sync::Arc;

#[cfg(target_os = "macos")]
use defra_core::signing::RemoteSigner;
#[cfg(target_os = "macos")]
use defra_core::signing::{SigningConfig, SigningKeyType};
#[cfg(target_os = "macos")]
use identity::{Identity as _, RawIdentity};

#[cfg(target_os = "macos")]
use super::{lowercase_hex, IdentityKeyUnavailable};
use super::{IdentityError, RegisteredIdentity, Result, ServiceAccount};

pub fn load_or_create_macos_secure_enclave_identity(
    label: &str,
    service_account: Option<ServiceAccount>,
) -> Result<RegisteredIdentity> {
    register_macos_secure_enclave_identity(label, true, service_account)
}

pub fn load_macos_secure_enclave_identity(
    label: &str,
    service_account: Option<ServiceAccount>,
) -> Result<RegisteredIdentity> {
    register_macos_secure_enclave_identity(label, false, service_account)
}

pub fn load_or_create_macos_keychain_identity(
    label: &str,
    service_account: Option<ServiceAccount>,
) -> Result<RegisteredIdentity> {
    register_macos_keychain_identity(label, true, service_account)
}

pub fn load_macos_keychain_identity(
    label: &str,
    service_account: Option<ServiceAccount>,
) -> Result<RegisteredIdentity> {
    register_macos_keychain_identity(label, false, service_account)
}

/// The only Keychain status that proves an item is absent; every other
/// failure leaves the item possibly intact.
pub(crate) const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;
#[cfg(target_os = "macos")]
const ERR_SEC_MISSING_ENTITLEMENT: i32 = -34018;

#[cfg(target_os = "macos")]
fn register_macos_secure_enclave_identity(
    label: &str,
    create_if_missing: bool,
    service_account: Option<ServiceAccount>,
) -> Result<RegisteredIdentity> {
    let signer = MacosSecureEnclaveSigner::load(label, create_if_missing)?;
    let did = signer.did.clone();
    let public_key_bytes = signer.public_key_bytes.clone();
    defra_core::signing::store_identity(
        &did,
        SigningConfig {
            key_type: SigningKeyType::Secp256r1,
            private_key_bytes: Vec::new(),
            public_key_bytes: public_key_bytes.clone(),
            public_key_hex: lowercase_hex(&public_key_bytes),
            remote_signer: Some(Arc::new(signer)),
            signing_authorization: None,
        },
    );
    RegisteredIdentity::from_registered_did(did, service_account)
}

#[cfg(not(target_os = "macos"))]
fn register_macos_secure_enclave_identity(
    _label: &str,
    _create_if_missing: bool,
    _service_account: Option<ServiceAccount>,
) -> Result<RegisteredIdentity> {
    Err(IdentityError::SecureEnclaveUnavailable)
}

#[cfg(target_os = "macos")]
fn register_macos_keychain_identity(
    label: &str,
    create_if_missing: bool,
    service_account: Option<ServiceAccount>,
) -> Result<RegisteredIdentity> {
    let identity = load_or_create_macos_keychain_raw_identity(label, create_if_missing)?;
    let did = identity.did()?.to_string();
    let public_key_bytes = identity.public_key_bytes();
    defra_core::signing::store_identity(
        &did,
        SigningConfig {
            key_type: SigningKeyType::Ed25519,
            private_key_bytes: SigningConfig::private_key_bytes_from_vec(
                identity.private_key_bytes(),
            ),
            public_key_bytes: public_key_bytes.clone(),
            public_key_hex: lowercase_hex(&public_key_bytes),
            remote_signer: None,
            signing_authorization: None,
        },
    );
    RegisteredIdentity::from_registered_did(did, service_account)
}

#[cfg(not(target_os = "macos"))]
fn register_macos_keychain_identity(
    _label: &str,
    _create_if_missing: bool,
    _service_account: Option<ServiceAccount>,
) -> Result<RegisteredIdentity> {
    Err(IdentityError::KeychainUnavailable)
}

#[cfg(target_os = "macos")]
fn load_or_create_macos_keychain_raw_identity(
    label: &str,
    create_if_missing: bool,
) -> Result<RawIdentity> {
    use crypto::Key as _;

    match macos_keychain_find(MACOS_KEYCHAIN_SERVICE, label)? {
        Ok(password) => {
            RawIdentity::from_bytes(crypto::KeyType::Ed25519, &password).map_err(|source| {
                IdentityError::LoadKeychainIdentity {
                    label: label.to_owned(),
                    source,
                }
            })
        }
        Err(ERR_SEC_ITEM_NOT_FOUND) => {
            if !create_if_missing {
                return Err(IdentityError::KeychainIdentityNotFound {
                    label: label.to_owned(),
                });
            }
            let private_key = crypto::generate_ed25519()?;
            let bytes = private_key.raw();
            macos_keychain_set(MACOS_KEYCHAIN_SERVICE, label, bytes).map_err(|source| {
                IdentityError::StoreKeychainIdentity {
                    label: label.to_owned(),
                    source: Box::new(source),
                }
            })?;
            RawIdentity::from_private_key(private_key).map_err(|source| {
                IdentityError::ConstructKeychainIdentity {
                    label: label.to_owned(),
                    source,
                }
            })
        }
        Err(code) => Err(IdentityError::KeychainReadFailed {
            label: label.to_owned(),
            source: IdentityKeyUnavailable {
                label: label.to_owned(),
                code,
            },
        }),
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn macos_keychain_find(
    service: &str,
    label: &str,
) -> Result<std::result::Result<k256::elliptic_curve::zeroize::Zeroizing<Vec<u8>>, i32>> {
    let keychain = security_framework::os::macos::keychain::SecKeychain::default()
        .map_err(IdentityError::DefaultKeychain)?;
    Ok(match keychain.find_generic_password(service, label) {
        Ok((password, _)) => Ok(k256::elliptic_curve::zeroize::Zeroizing::new(
            password.as_ref().to_vec(),
        )),
        Err(error) => Err(error.code()),
    })
}

#[cfg(target_os = "macos")]
pub(crate) fn macos_keychain_set(service: &str, label: &str, secret: &[u8]) -> Result<()> {
    security_framework::os::macos::keychain::SecKeychain::default()
        .map_err(IdentityError::DefaultKeychain)?
        .set_generic_password(service, label, secret)
        .map_err(IdentityError::Keychain)
}

/// `security-framework` discards `SecKeychainItemDelete`'s status, so
/// deletion is complete only after a fresh lookup returns `errSecItemNotFound`.
#[cfg(target_os = "macos")]
pub(crate) fn macos_keychain_delete(service: &str, label: &str) -> Result<()> {
    let keychain = security_framework::os::macos::keychain::SecKeychain::default()
        .map_err(IdentityError::DefaultKeychain)?;
    match keychain.find_generic_password(service, label) {
        Ok((_, item)) => {
            item.delete();
            match macos_keychain_find(service, label)? {
                Err(ERR_SEC_ITEM_NOT_FOUND) => Ok(()),
                Err(code) => Err(IdentityError::KeychainVerifyFailed {
                    label: label.to_owned(),
                    code,
                }),
                Ok(_) => Err(IdentityError::KeychainStillPresent {
                    label: label.to_owned(),
                }),
            }
        }
        Err(error) if error.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(()),
        Err(error) => Err(IdentityError::Keychain(error)),
    }
}

#[cfg(target_os = "macos")]
const MACOS_KEYCHAIN_SERVICE: &str = "com.source-inc.gents.identity";

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct MacosSecureEnclaveSigner {
    label: String,
    key: security_framework::key::SecKey,
    did: String,
    public_key_bytes: Vec<u8>,
}

#[cfg(target_os = "macos")]
impl MacosSecureEnclaveSigner {
    fn load(label: &str, create_if_missing: bool) -> Result<Self> {
        let key = match find_macos_secure_enclave_key(label)? {
            Some(key) => key,
            None if create_if_missing => create_macos_secure_enclave_key(label)?,
            None => {
                return Err(IdentityError::SecureEnclaveKeyNotFound {
                    label: label.to_owned(),
                })
            }
        };
        let public_key_bytes = public_key_bytes_for_sec_key(&key).map_err(|source| {
            IdentityError::SecureEnclavePublicKey {
                label: label.to_owned(),
                source: Box::new(source),
            }
        })?;
        let did = did_for_secp256r1_public_key(&public_key_bytes).map_err(|source| {
            IdentityError::SecureEnclaveDid {
                label: label.to_owned(),
                source: Box::new(source),
            }
        })?;
        Ok(Self {
            label: label.to_string(),
            key,
            did,
            public_key_bytes,
        })
    }
}

#[cfg(target_os = "macos")]
impl RemoteSigner for MacosSecureEnclaveSigner {
    fn sign_sync(
        &self,
        data: &[u8],
        _authorization: Option<&defra_core::signing::SigningAuthorization>,
    ) -> std::result::Result<Vec<u8>, String> {
        self.key
            .create_signature(
                security_framework::key::Algorithm::ECDSASignatureMessageX962SHA256,
                data,
            )
            .map_err(|error| {
                format!(
                    "Secure Enclave signing failed for label {}: {error}",
                    self.label
                )
            })
    }
}

#[cfg(target_os = "macos")]
fn find_macos_secure_enclave_key(label: &str) -> Result<Option<security_framework::key::SecKey>> {
    use security_framework::item::{ItemSearchOptions, KeyClass, Reference, SearchResult};

    let mut search = ItemSearchOptions::new();
    search
        .key_class(KeyClass::private())
        .label(label)
        .ignore_legacy_keychains()
        .load_refs(true)
        .limit(2);
    let keys = match search.search() {
        Ok(keys) => keys,
        Err(error) if error.code() == ERR_SEC_ITEM_NOT_FOUND => {
            return Ok(None);
        }
        Err(error) if error.code() == ERR_SEC_MISSING_ENTITLEMENT => {
            return Err(missing_secure_enclave_entitlement_error(label));
        }
        Err(error) => {
            return Err(IdentityError::SearchSecureEnclaveKey {
                label: label.to_owned(),
                source: IdentityKeyUnavailable {
                    label: label.to_owned(),
                    code: error.code(),
                },
            });
        }
    };
    let mut matches = keys.into_iter().filter_map(|item| match item {
        SearchResult::Ref(Reference::Key(key)) => Some(key),
        _ => None,
    });
    let first = matches.next();
    if matches.next().is_some() {
        return Err(IdentityError::MultipleSecureEnclaveKeys {
            label: label.to_owned(),
        });
    }
    Ok(first)
}

#[cfg(target_os = "macos")]
fn create_macos_secure_enclave_key(label: &str) -> Result<security_framework::key::SecKey> {
    use security_framework::access_control::SecAccessControl;
    use security_framework::item::Location;
    use security_framework::key::{GenerateKeyOptions, KeyType, SecKey, Token};
    use security_framework::passwords_options::AccessControlOptions;

    let access_control =
        SecAccessControl::create_with_flags(AccessControlOptions::PRIVATE_KEY_USAGE.bits())
            .map_err(IdentityError::SecureEnclaveAccessControl)?;
    let mut options = GenerateKeyOptions::default();
    options
        .set_key_type(KeyType::ec())
        .set_size_in_bits(256)
        .set_label(label)
        .set_access_control(access_control)
        .set_token(Token::SecureEnclave)
        .set_location(Location::DataProtectionKeychain);
    match SecKey::new(&options) {
        Ok(key) => Ok(key),
        Err(error) if error.code() == ERR_SEC_MISSING_ENTITLEMENT as isize => {
            Err(missing_secure_enclave_entitlement_error(label))
        }
        Err(error) => Err(IdentityError::CreateSecureEnclaveKey {
            label: label.to_owned(),
            error: error.to_string(),
        }),
    }
}

#[cfg(target_os = "macos")]
fn missing_secure_enclave_entitlement_error(label: &str) -> IdentityError {
    IdentityError::SecureEnclaveEntitlement {
        label: label.to_owned(),
    }
}

#[cfg(target_os = "macos")]
fn public_key_bytes_for_sec_key(key: &security_framework::key::SecKey) -> Result<Vec<u8>> {
    let public_key = key
        .public_key()
        .ok_or(IdentityError::SecureEnclaveNoPublicKey)?;
    let data = public_key
        .external_representation()
        .ok_or(IdentityError::SecureEnclaveNotExportable)?;
    Ok(data.to_vec())
}

#[cfg(target_os = "macos")]
fn did_for_secp256r1_public_key(public_key_bytes: &[u8]) -> Result<String> {
    let public_key = crypto::public_key_from_bytes(crypto::KeyType::Secp256r1, public_key_bytes)?;
    Ok(public_key.did()?)
}
