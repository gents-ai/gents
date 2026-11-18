use gents_protocol::schemas;

use crate::common;

/// A store a build before command Tasks registered (fresh on that build, so
/// already with `outcome_readers` and the fire budget columns) gains
/// `Task.command` and `Trigger.last_run` with its rows and field ids kept, and
/// a second pass applies nothing.
#[tokio::test]
async fn command_task_columns_extend_a_store_born_before_them() {
    for (name, sdl, added, create) in [
        (
            schemas::TASK_NAME,
            schemas::TASK,
            "command",
            r#"mutation {create_Task(input: {task_id: "retained", node_did: "owner"}) {_docID}}"#,
        ),
        (
            schemas::TRIGGER_NAME,
            schemas::TRIGGER,
            "last_run",
            r#"mutation {create_Trigger(input: {trigger_id: "retained", node_did: "owner"}) {_docID}}"#,
        ),
    ] {
        let previous = sdl
            .lines()
            .filter(|line| !line.trim_start().starts_with(&format!("{added}:")))
            .collect::<Vec<_>>()
            .join("\n");
        let node = common::fresh_node().await;
        node.add_collection(&previous).await.unwrap();
        let before = node.get_collection(name).unwrap().unwrap();
        let created = node.execute(create).await;
        assert!(!created.has_errors(), "{name}: {created:?}");
        gents_migration::ensure_migrations(&node).await.unwrap();
        let after = node.get_collection(name).unwrap().unwrap();
        assert_eq!(after.collection_id, before.collection_id, "{name}");
        for field in &before.fields {
            let kept = after.fields.iter().find(|next| next.name == field.name);
            assert_eq!(
                kept.map(|kept| &kept.id),
                Some(&field.id),
                "{name}.{}",
                field.name
            );
        }
        let values = node
            .execute(&format!("{{{name} {{_docID {added}}}}}"))
            .await;
        assert!(!values.has_errors(), "{name}: {values:?}");
        assert!(
            values.data.unwrap()[name][0][added].is_null(),
            "{name}.{added}"
        );
        assert_eq!(
            gents_migration::ensure_migrations(&node)
                .await
                .unwrap()
                .steps_applied,
            0,
            "{name}"
        );
        node.shutdown().await.expect("shut down test node");
    }
}
