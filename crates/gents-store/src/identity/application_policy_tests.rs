use super::*;

#[test]
fn admission_and_policy_text_match_lean_owner() {
    use gents_lean_vocab::families::application_policy_admission as family;
    family::replay(|i| {
        let policy: ApplicationPolicy =
            serde_json::from_value(serde_json::to_value(i).unwrap()).unwrap();
        let admitted = policy.admit();
        family::AdmissionOutput {
            refusal: admitted.err().map(|refusal| refusal.kind().to_owned()),
            yaml: admitted.is_ok().then(|| policy.yaml()),
        }
    });
}
