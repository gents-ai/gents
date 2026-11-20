use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::owed::{
    adopt, continues, entry_step, handling, refresh_due, serves_pass, standing, EntryStep,
    Handling, OwedStanding, Recorded, Refresh, DELEGATION_REFRESH_AFTER, OWED_RETRY,
};
use super::user_write::Verdict;

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ModelRecorded {
    Absent,
    Staged,
    AwaitingAuthority,
    Refused,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ModelStanding {
    Settled,
    Sending,
    HeldBehind,
    AwaitingAuthority,
    Failed,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ModelVerdict {
    IssuedBySigner,
    IssuedByAnother,
    Refused,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum ModelHandling {
    HandIn,
    Leave,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum ModelEntryStep {
    Done,
    AskAgain,
    Report,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum ModelRefresh {
    Adopted,
    Refused,
}

fn standing_of(model: ModelStanding) -> OwedStanding {
    match model {
        ModelStanding::Settled => OwedStanding::Settled,
        ModelStanding::Sending => OwedStanding::Sending,
        ModelStanding::HeldBehind => OwedStanding::HeldBehind,
        ModelStanding::AwaitingAuthority => OwedStanding::AwaitingAuthority,
        ModelStanding::Failed => OwedStanding::Failed,
    }
}

fn verdict_of(model: ModelVerdict) -> Verdict {
    match model {
        ModelVerdict::IssuedBySigner => Verdict::IssuedBySigner,
        ModelVerdict::IssuedByAnother => Verdict::IssuedByAnother,
        ModelVerdict::Refused => Verdict::Refused,
    }
}

#[test]
fn the_standing_of_an_owed_entry_matches_the_lean_model() {
    gents_lean_vocab::families::identity_owed_registration_standing::replay::<
        ModelRecorded,
        ModelStanding,
    >(|input| {
        let recorded = match input.recorded {
            ModelRecorded::Absent => Recorded::Absent,
            ModelRecorded::Staged => Recorded::Staged,
            ModelRecorded::AwaitingAuthority => Recorded::AwaitingAuthority,
            ModelRecorded::Refused => Recorded::Refused,
        };
        match standing(recorded, input.sendable, input.held, input.discarded) {
            OwedStanding::Settled => ModelStanding::Settled,
            OwedStanding::Sending => ModelStanding::Sending,
            OwedStanding::HeldBehind => ModelStanding::HeldBehind,
            OwedStanding::AwaitingAuthority => ModelStanding::AwaitingAuthority,
            OwedStanding::Failed => ModelStanding::Failed,
        }
    });
}

#[test]
fn the_handling_of_an_owed_entry_matches_the_lean_model() {
    gents_lean_vocab::families::identity_owed_registration_handling::replay::<
        ModelHandling,
        ModelStanding,
    >(|input| match handling(standing_of(input.standing)) {
        Handling::HandIn => ModelHandling::HandIn,
        Handling::Leave => ModelHandling::Leave,
    });
}

#[test]
fn serving_a_pass_matches_the_lean_model() {
    gents_lean_vocab::families::identity_owed_registration_serves_pass::replay::<ModelVerdict>(
        |input| serves_pass(input.presented, verdict_of(input.verdict)),
    );
}

#[test]
fn the_clients_step_for_an_entry_matches_the_lean_model() {
    gents_lean_vocab::families::identity_owed_registration_entry_step::replay::<
        ModelEntryStep,
        ModelStanding,
    >(|input| match entry_step(standing_of(input.standing)) {
        EntryStep::Done => ModelEntryStep::Done,
        EntryStep::AskAgain => ModelEntryStep::AskAgain,
        EntryStep::Report => ModelEntryStep::Report,
    });
}

#[test]
fn adopting_a_refreshed_delegation_matches_the_lean_model() {
    gents_lean_vocab::families::identity_owed_registration_adopt::replay::<
        ModelRefresh,
        ModelVerdict,
    >(
        |input| match adopt(input.general, verdict_of(input.verdict)) {
            Refresh::Adopted => ModelRefresh::Adopted,
            Refresh::Refused => ModelRefresh::Refused,
        },
    );
}

#[test]
fn when_a_refresh_is_due_matches_the_lean_model() {
    gents_lean_vocab::families::owed_registration_refresh::replay(|input| {
        refresh_due(Duration::from_secs(input.age))
    });
}

#[test]
fn whether_a_client_asks_again_matches_the_lean_model() {
    gents_lean_vocab::families::owed_registration_proceed::replay::<ModelStanding>(|input| {
        let standings: Vec<_> = input.standings.iter().copied().map(standing_of).collect();
        continues(input.online, &standings)
    });
}

#[test]
fn the_settlement_timing_matches_the_lean_model() {
    gents_lean_vocab::families::owed_registration_timing::replay(|_| {
        gents_lean_vocab::families::owed_registration_timing::Timing {
            retry_seconds: OWED_RETRY.as_secs(),
            refresh_after_seconds: DELEGATION_REFRESH_AFTER.as_secs(),
        }
    });
}

#[test]
fn the_step_after_a_writes_own_grants_matches_the_lean_model() {
    use crate::config_client::owed::{dependent_step, observed_grant, DependentStep, GrantWait};
    gents_lean_vocab::families::identity_owed_registration_dependent_step::replay::<
        DependentStep,
        GrantWait,
    >(|input| dependent_step(input.grant_wait));
    gents_lean_vocab::families::identity_owed_registration_observed_grant::replay::<GrantWait>(
        |input| observed_grant(input.external_provider, input.provider),
    );
}
