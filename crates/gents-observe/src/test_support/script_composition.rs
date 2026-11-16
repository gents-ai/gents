use gents_store::identity::NodeIdentity;
pub async fn timeline(
    fixture: &gents_toolcalls::test_support::background_hook::BudgetHookFixture,
) -> Vec<crate::observe::run_timeline::TimelineToolCallRow> {
    crate::observe::run_timeline_fetch::load_session_tool_calls(
        &fixture.access,
        fixture.db.node_identity.did(),
        &fixture.session_id,
        Some(fixture.user.did()),
    )
    .await
    .expect("one script call never fails the session timeline")
}
