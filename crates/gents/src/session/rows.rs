#[derive(Debug, Clone)]
pub struct SessionOwnerRow {
    pub doc_id: String,
    pub session: gents_protocol::session::AgentSession,
}

pub(crate) fn dedupe_paths(paths: &mut Vec<String>) {
    paths.sort();
    paths.dedup();
}
