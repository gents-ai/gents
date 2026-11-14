use anyhow::Result;
use serde_json::json;

use super::selected_agent;
use crate::desired_state::selection::tests::canonical;
use gents_store::collection::Collection;

#[test]
fn task_selection_edge_is_the_task_target_agent() -> Result<()> {
    let node = "did:key:node";
    assert_eq!(selected_agent(node, None)?, None);
    for agent in ["did:key:before", "did:key:after"] {
        let task = canonical(Collection::Task, node, json!({"agent_did": agent}));
        assert_eq!(selected_agent(node, Some(&task))?.as_deref(), Some(agent));
    }
    Ok(())
}
