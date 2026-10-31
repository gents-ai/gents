use super::admission_fixture::{
    published_admission_with_owner, PublishedAdmission, PublishedAdmissionOptions,
};
use crate::session::{count_request_output_scans, count_source_output_queries};

/// Appending tool output and delivering its result read the tool's own source
/// through the `(request_doc_id, source)` index. A whole-request read filtered
/// by session scope lets the planner choose the `node_did` index, so each
/// append and delivery scanned every segment the node had written.
#[tokio::test]
async fn tool_append_and_delivery_read_only_their_source() {
    let (fixture, owner) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "tool-source-reads".to_owned(),
        ..Default::default()
    })
    .await
    .unwrap();
    let PublishedAdmission {
        node,
        path,
        mut tool,
        ..
    } = fixture;
    let binding = tool.tool_output_binding().unwrap();

    let ((appended, scans), queries) = count_source_output_queries(count_request_output_scans(
        super::delivery::append_tool_output(&binding, "partial output"),
    ))
    .await;
    appended.unwrap();
    assert_eq!(scans, 0, "append must not scan the request's output");
    assert_eq!(queries, 1, "append reads only the tool's source");

    let ((completed, scans), queries) = count_source_output_queries(count_request_output_scans(
        tool.complete("partial output, done"),
    ))
    .await;
    completed.unwrap();
    assert_eq!(scans, 0, "delivery must not scan the request's output");
    // One read reconstructs the accepted tool-call header; the other is the
    // tool's own output source.
    assert_eq!(queries, 2, "delivery reads only the tool's source");

    drop(owner);
    node.shutdown().await;
    let _ = std::fs::remove_dir_all(path);
}
