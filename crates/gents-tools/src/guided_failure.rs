use serde_json::{json, Value};

/// A tool failure whose model-facing text names the call that recovers from
/// it.
#[derive(Debug, thiserror::Error)]
#[error("{reason}; next call: {tool} {call}")]
pub(crate) struct GuidedFailure {
    reason: String,
    tool: &'static str,
    call: Value,
}

impl GuidedFailure {
    pub(crate) fn new(tool: &'static str, reason: impl Into<String>, call: Value) -> Self {
        Self {
            reason: reason.into(),
            tool,
            call,
        }
    }

    /// `error` as the model reads it: unchanged when it carries a guided
    /// failure as its root or as a context value, otherwise its text followed
    /// by `tool`'s help call.
    pub(crate) fn or_help(tool: &'static str, error: anyhow::Error) -> anyhow::Error {
        if error.downcast_ref::<Self>().is_some() {
            return error;
        }
        Self::new(tool, format!("{error:#}"), json!({"action": "help"})).into()
    }
}

#[cfg(test)]
mod tests {
    use super::GuidedFailure;
    use anyhow::Context;
    use serde_json::json;

    fn rendered(error: anyhow::Error) -> String {
        format!("{:#}", GuidedFailure::or_help("p2p", error))
    }

    #[test]
    fn a_failure_naming_no_call_ends_with_the_tool_help() {
        let unguided = anyhow::anyhow!("denied").context("reading");
        assert_eq!(
            rendered(unguided),
            r#"reading: denied; next call: p2p {"action":"help"}"#
        );
    }

    #[test]
    fn a_failure_naming_its_call_keeps_it_wherever_it_is_carried() {
        let guide = || GuidedFailure::new("p2p", "invalid peer", json!({"action":"network_list"}));
        let root = anyhow::Error::from(guide()).context("reading");
        assert_eq!(
            rendered(root),
            r#"reading: invalid peer; next call: p2p {"action":"network_list"}"#
        );
        let context = Err::<(), _>(std::io::Error::other("denied"))
            .context(guide())
            .context("outer")
            .unwrap_err();
        assert_eq!(
            rendered(context),
            r#"outer: invalid peer; next call: p2p {"action":"network_list"}: denied"#
        );
        let absent = None::<()>.with_context(guide).unwrap_err();
        assert_eq!(
            rendered(absent),
            r#"invalid peer; next call: p2p {"action":"network_list"}"#
        );
    }
}
