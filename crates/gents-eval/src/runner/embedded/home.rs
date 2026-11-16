pub(crate) use gents::runtime::embedded::*;

#[cfg(test)]
pub(super) fn bind_test_evaluator(
    home: &mut EmbeddedHome,
) -> std::sync::Arc<gents_store::identity::KeyIdentity> {
    let identity = std::sync::Arc::new(
        gents_store::identity::KeyIdentity::load_or_create(home.path().join("evaluator.key"), None)
            .unwrap(),
    );
    home.set_evaluator(
        identity.clone(),
        identity.client_signing_capability().unwrap(),
    );
    identity
}
