use super::*;
use crate::graphql::CommitSignature;

fn ed25519_signer() -> (String, CommitSignature) {
    let key = crypto::generate_ed25519().unwrap();
    let public = crypto::PrivateKey::public_key(&key);
    let signature = CommitSignature {
        identity: crypto::Key::to_hex_string(public),
        signature_type: "EdDSA".into(),
    };
    (crypto::PublicKey::did(public).unwrap(), signature)
}

fn commit(cid: &str, height: i64, signature: Option<&CommitSignature>) -> CompositeCommit {
    CompositeCommit {
        cid: cid.into(),
        height,
        field_name: "_C".into(),
        signature: signature.cloned(),
    }
}

#[test]
fn an_unreadable_or_partial_history_is_never_attributed() {
    let (alice, alice_sig) = ed25519_signer();
    let (bob, bob_sig) = ed25519_signer();
    let signers = |commits| signers_of("doc", commits).unwrap();

    assert_eq!(
        signers(Vec::new()),
        DocumentSigners {
            unsigned: true,
            ..DocumentSigners::default()
        },
        "an empty history, as DefraDB returns for a document the reader lost, is not clean"
    );
    assert_eq!(
        signers(vec![
            commit("c3", 3, Some(&alice_sig)),
            commit("c2", 2, Some(&alice_sig)),
        ]),
        DocumentSigners {
            created: None,
            latest: Some(alice.clone()),
            writers: BTreeSet::from([alice.clone()]),
            unsigned: true,
        },
        "a history without its genesis may have lost a writer"
    );
    assert_eq!(
        signers(vec![
            commit("b", 2, Some(&bob_sig)),
            commit("a", 2, Some(&alice_sig)),
            commit("g", 1, Some(&alice_sig)),
        ]),
        DocumentSigners {
            created: Some(alice.clone()),
            latest: None,
            writers: BTreeSet::from([alice.clone(), bob.clone()]),
            unsigned: false,
        },
        "concurrent heads have no single latest signer"
    );
    assert_eq!(
        signers(vec![commit("u", 2, None), commit("g", 1, Some(&alice_sig)),]),
        DocumentSigners {
            created: Some(alice.clone()),
            latest: None,
            writers: BTreeSet::from([alice]),
            unsigned: true,
        },
        "an unsigned version is reported"
    );
}

#[test]
fn a_compressed_header_key_names_the_signers_did() {
    for (key, signature_type) in [
        (
            Box::new(crypto::generate_secp256k1().unwrap()) as Box<dyn crypto::PrivateKey>,
            "ES256K",
        ),
        (Box::new(crypto::generate_secp256r1().unwrap()), "ES256"),
    ] {
        let public = key.public_key();
        assert_eq!(
            crypto::Key::raw(public).len(),
            33,
            "{signature_type} raw keys are compressed"
        );
        assert_eq!(
            commit_signer_did(&crypto::Key::to_hex_string(public), signature_type).unwrap(),
            crypto::PublicKey::did(public).unwrap(),
            "{signature_type}: a compressed header key names the DID DefraDB verified"
        );
    }
}
