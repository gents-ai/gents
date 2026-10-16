use crate::shared::ConfigExportBundle;

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
