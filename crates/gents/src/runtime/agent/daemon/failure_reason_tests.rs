use super::terminal::request_failure_reason;

#[test]
fn request_failure_reason_keeps_the_cause_under_its_context() {
    let cause =
        anyhow::anyhow!("An OpenAI-generated ID is required when using OpenAI reasoning items")
            .context("projecting OpenAI Responses request");
    let error = cause.context("projecting compactable provider messages");
    assert_eq!(
        error.to_string(),
        "projecting compactable provider messages"
    );
    assert_eq!(
        request_failure_reason(&error),
        "projecting compactable provider messages: projecting OpenAI Responses request: \
         An OpenAI-generated ID is required when using OpenAI reasoning items"
    );
}
