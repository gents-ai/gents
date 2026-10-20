use gents::config_client::ConfigAccess;
use gents::graphql::escape_graphql_string;
use gents_protocol::file_changes::{
    FileChangeImages, FileImage, FilePermissions, ToolFileChanges, FILE_CHANGES_VERSION,
};
use gents_protocol::job::{JobBudget, JobProgress, JobState};

use super::operations_job_control::seed_admission;
use super::support::{boot_core, create_session, seed_job_row, OPERATOR};

#[tokio::test]
async fn file_change_review_reads_the_bound_request_without_touching_a_local_checkout() {
    let (core, _dir) = boot_core().await;
    let session = "file-review-session";
    let call = "file-review-call";
    let request = format!("req-{call}");
    create_session(&core, OPERATOR, session, None).await;
    let progress = JobProgress {
        title: "File change".into(),
        state: JobState::Completed,
        pause_reason: None,
        phases: Vec::new(),
        active: Vec::new(),
        spawned: 0,
        budget: JobBudget {
            total: 1,
            spent: 0,
            remaining: 1,
        },
        last_event: "finished".into(),
        last_event_at: None,
        question: None,
        observed_terminal_children: 0,
        revision: 1,
    };
    let physical = seed_job_row(&core, session, call, "completed", &progress, None).await;
    seed_admission(&core, session, &physical, call, 1).await;
    let expected = ToolFileChanges {
        version: FILE_CHANGES_VERSION,
        tool_call_doc_id: physical.clone(),
        changes: vec![FileChangeImages {
            root: "/unmapped/runtime/workspace".into(),
            path: "empty.txt".into(),
            before: FileImage::Missing,
            after: FileImage::File {
                content_hash:
                    "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into(),
                data: String::new(),
                permissions: FilePermissions {
                    readonly: false,
                    unix_mode: None,
                },
                links: None,
            },
        }],
    };
    let access = ConfigAccess::local(core.node_arc());
    let mutation = format!(
        r#"mutation($changes: JSON) {{ update_AgentToolCall(docID: "{}", input: {{ file_changes: $changes }}) {{ _docID }} }}"#,
        escape_graphql_string(&physical),
    );
    let variables = serde_json::json!({"changes": expected});
    access
        .transact_write("test.file_review.images", |txn| {
            let (mutation, variables) = (&mutation, &variables);
            Box::pin(async move { txn.execute_with_variables(mutation, variables).await })
        })
        .await
        .unwrap();
    let result = crate::commands::tool_file_changes(
        &core,
        OPERATOR.into(),
        session.into(),
        request.clone(),
        physical.clone(),
    )
    .await
    .unwrap();
    assert_eq!(result, Some(expected));
    let other = seed_job_row(&core, session, "foreign-call", "completed", &progress, None).await;
    seed_admission(&core, session, &other, "foreign-call", 2).await;
    assert!(crate::commands::tool_file_changes(
        &core,
        OPERATOR.into(),
        session.into(),
        "req-foreign-call".into(),
        physical.clone()
    )
    .await
    .is_err());
    assert!(crate::commands::tool_file_changes(
        &core,
        OPERATOR.into(),
        session.into(),
        request.clone(),
        call.into()
    )
    .await
    .is_err());
    for (node, other_session) in [("foreign-node", session), (OPERATOR, "foreign-session")] {
        assert!(!matches!(
            crate::commands::tool_file_changes(
                &core,
                node.into(),
                other_session.into(),
                request.clone(),
                physical.clone()
            )
            .await,
            Ok(Some(_))
        ));
    }
    core.shutdown().await.unwrap();
}
