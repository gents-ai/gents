use gents_lean_vocab::families;

use super::*;

#[test]
fn binding_currency_follows_the_model() {
    for case in families::application_pairing_binding_cases::cases() {
        let instant = |value: u64| i64::try_from(value).expect("modeled instant fits i64");
        let history: Vec<RevisionFact> = case
            .input
            .history
            .iter()
            .map(|revision| RevisionFact {
                sequence: revision.sequence,
                active: revision.active,
                issued_at: instant(revision.issued_at),
                expires_at: instant(revision.expires_at),
            })
            .collect();
        let binding = OverlayBinding {
            network_id: case.input.binding.network.clone(),
            member_did: case.input.binding.member.clone(),
            sequence: case.input.binding.sequence,
        };
        let current = CurrentAuthorization {
            network_id: case.input.current.network.clone(),
            member_did: case.input.current.member.clone(),
            sequence: case.input.current.sequence,
        };
        case.check(binding_is_current(&history, &binding, &current));
    }
}

#[test]
fn overlay_contribution_follows_the_model() {
    for case in families::application_pairing_contribution_cases::cases::<String>() {
        let facts = OverlayFacts {
            enrollment_fresh: case.input.enrollment_fresh,
            binding_current: case.input.binding_current,
            author_live_agent: case.input.author_live_agent,
            author_holds_grant: case.input.author_holds_grant,
            content_owner_written: case.input.content_owner_written,
            template_valid: case.input.template_valid,
            collections_admitted: case.input.collections_admitted,
        };
        case.check(decide_overlay(facts).err().map(|inactive| {
            inactive
                .model_name()
                .expect("a modeled decision has a model name")
                .to_owned()
        }));
    }
}
