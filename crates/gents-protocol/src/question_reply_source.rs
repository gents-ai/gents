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

#[cfg(test)]
mod conformance {
    #[test]
    fn generated_question_reply_source_cases_preserve_attribution() {
        gents_lean_vocab::families::question_reply_source::replay(|i| {
            gents_lean_vocab::families::question_reply_source::QuestionOutput {
                source: super::question_reply_source(
                    i.person,
                    i.request_source.as_deref(),
                    i.evidence_source.as_deref(),
                    i.observed_question,
                )
                .map(str::to_owned),
            }
        });
    }
}
