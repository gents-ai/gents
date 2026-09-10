use crate::shared::ConfigExportBundle;

/// A config bundle prepared by the desired-state command path.
/// The payload uses canonical PackConfig; the shared transaction owner validates
/// fields, principal scope and the complete retained reference set before writing.
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
