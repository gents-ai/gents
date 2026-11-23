use super::*;

#[test]
fn standing_follows_the_executable_model() {
    use gents_lean_vocab::families::provider_usage_refusal_standing as family;
    let at = |seconds: u64| DateTime::<Utc>::UNIX_EPOCH + chrono::Duration::seconds(seconds as i64);
    family::replay(|input: &family::StandingInput| {
        stands(
            at(input.now),
            at(input.queued_at),
            input.resets_at.map(at),
            input.last_completion.map(at),
        )
    });
}
