pub mod admit;
pub mod ancestry;
pub mod control;
pub mod dispatch;
pub mod driver;
pub mod effects;
pub mod input;
pub mod keys;
pub mod origin;
pub mod records;
pub mod recovery;
pub mod settings;
pub mod view;

use rapidhash::RapidHashMap;

pub use gents_protocol::job;
use gents_protocol::job::PluginJobSettings;

use crate::document_config::PluginToolRef;

/// A Tools plugin entry whose installed plugin declares `job: true`.
#[derive(Debug, Clone, PartialEq)]
pub struct JobBinding {
    pub tool_name: String,
    pub plugin: PluginToolRef,
    /// The entry's `job` block; `None` binds under the empty ceiling.
    pub settings: Option<PluginJobSettings>,
}

impl JobBinding {
    /// Binds, by tool name, every entry whose resolved installed plugin
    /// declares `job: true`.
    pub(crate) fn bind_all(
        resolutions: &[(PluginToolRef, Option<crate::plugin::store::InstalledPlugin>)],
    ) -> RapidHashMap<String, JobBinding> {
        resolutions
            .iter()
            .filter(|(_, installed)| {
                installed
                    .as_ref()
                    .is_some_and(|record| record.declaration.job.0)
            })
            .map(|(plugin, _)| {
                let tool_name = plugin.tool_name().to_owned();
                let binding = JobBinding {
                    tool_name: tool_name.clone(),
                    plugin: plugin.clone(),
                    settings: plugin.job.clone(),
                };
                (tool_name, binding)
            })
            .collect()
    }
}
