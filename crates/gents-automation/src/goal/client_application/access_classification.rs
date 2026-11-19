use super::GoalApplicationError;
use gents_store::config_client::access_classification_fixture::{
    classify_erased, AccessClassification, AccessFailure, Erasure,
};

#[test]
fn owner_variants_preserve_access_classification() {
    gents_lean_vocab::families::access_classification_automation_wrapped::replay::<
        String,
        AccessClassification,
        Erasure,
        AccessFailure,
    >(|input| match input.wrapper.as_str() {
        "goal_application_error_access" => classify_erased(
            input.erasure,
            GoalApplicationError::Access(input.failure.error()),
        ),
        "goal_application_error_view" => classify_erased(
            input.erasure,
            GoalApplicationError::View(gents_store::views::ViewError::Execute(
                input.failure.error(),
            )),
        ),
        other => panic!("unhandled modeled error wrapper {other}"),
    });
}
