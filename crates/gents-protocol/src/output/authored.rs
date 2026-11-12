use super::MessageRole;

pub fn output_obligation_key(index: u32) -> String {
    format!("output-obligation:{index}")
}

/// Recognizes only the runtime's canonical authored key for this exact request.
pub fn is_output_obligation(role: MessageRole, request_doc_id: Option<&str>, key: &str) -> bool {
    if role != MessageRole::User {
        return false;
    }
    let Some(doc) = request_doc_id.filter(|doc| !doc.is_empty()) else {
        return false;
    };
    let Some(index) = key.strip_prefix(&format!("authored:{doc}:output-obligation:")) else {
        return false;
    };
    index
        .parse::<u32>()
        .is_ok_and(|value| value.to_string() == index)
}

#[cfg(test)]
mod conformance {
    use super::*;

    #[test]
    fn generated_output_obligation_cases_match_runtime_input_classification() {
        gents_lean_vocab::families::runtime_input_output_obligation::replay(|i| {
            let role = if i.user {
                MessageRole::User
            } else {
                MessageRole::Assistant
            };
            gents_lean_vocab::families::runtime_input_output_obligation::ObligationOutput {
                control: is_output_obligation(role, i.request.as_deref(), &i.key),
            }
        });
        assert_eq!(output_obligation_key(12), "output-obligation:12");
    }
}
