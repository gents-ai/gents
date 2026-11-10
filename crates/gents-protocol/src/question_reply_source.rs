/// Identify a question reply from the request's recorded source and evidence.
/// Legacy requests have no answer evidence and require an observed question.
pub fn question_reply_source<'a>(
    person: bool,
    request_source: Option<&'a str>,
    evidence_source: Option<&str>,
    observed_question: bool,
) -> Option<&'a str> {
    if !person {
        return None;
    }
    match (request_source, evidence_source) {
        (Some(request), Some(evidence)) if !request.is_empty() && request == evidence => {
            Some(request)
        }
        (Some(request), None) if !request.is_empty() && observed_question => Some(request),
        _ => None,
    }
}
