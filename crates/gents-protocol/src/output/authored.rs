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
