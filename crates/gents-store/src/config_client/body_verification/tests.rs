use super::*;
use gents_lean_vocab::families::placement_body;

#[test]
fn body_verification_replays_the_lean_owner() {
    placement_body::replay(|input| {
        let check = match &input.strict_home {
            Some(home) => MergeCheck::Strict { home: home.clone() },
            None => MergeCheck::Unchecked,
        };
        let closure = input
            .closure
            .iter()
            .map(|entry| ClosureDocument {
                present: entry.present,
                registered: entry.registered,
                head: entry.head.clone(),
            })
            .collect::<Vec<_>>();
        let (verdict, heads) = match body_verification(&check, &input.issuer_home, &closure) {
            BodyVerification::NotStrict => ("not_strict", Vec::new()),
            BodyVerification::Waiting => ("waiting", Vec::new()),
            BodyVerification::Verified { heads } => ("verified", heads),
        };
        placement_body::BodyOutput { verdict, heads }
    });
}
