use crate::shared::ConfigExportBundle;

/// A [`ConfigExportBundle`] whose contents are guaranteed to have been
/// produced from a typed [`crate::desired_state::DesiredStateManifest`] —
/// i.e. every field-carrying value in the bundle originated from a type
/// implementing [`gents::DesiredFields`].
///
/// The only constructor lives in
/// [`crate::desired_state::export_bundle_from_manifest`], inside the
/// `desired_state` module. External modules (including elsewhere in the
/// same crate) cannot construct a `DesiredApplyBundle` directly — this
/// makes it statically impossible to route an arbitrary
/// `ConfigExportBundle` (for example, one loaded from a user-supplied
/// JSON file via `read_config_import_bundle`) into
/// [`crate::apply_desired_state_changes`].
#[derive(Debug, Clone)]
pub(crate) struct DesiredApplyBundle {
    inner: ConfigExportBundle,
}

impl DesiredApplyBundle {
    pub(super) fn from_trusted_bundle(inner: ConfigExportBundle) -> Self {
        Self { inner }
    }

    pub(crate) fn as_bundle(&self) -> &ConfigExportBundle {
        &self.inner
    }
}
