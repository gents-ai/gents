use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProblemRow {
    pub problem_key: String,
    pub name: String,
    pub carried_from: Option<String>,
    pub created_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptRow {
    pub attempt_key: String,
    pub problem_key: String,
    pub predicted: Vec<String>,
    pub accepted: bool,
    pub executed_model_name: String,
    pub carried_from: Option<String>,
    pub created_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeRow {
    pub outcome_key: String,
    pub attempt_key: String,
    pub fixed: Vec<String>,
    pub missed: Vec<String>,
    pub passed: u64,
    pub total: u64,
    pub created_at: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rows {
    pub problems: Vec<ProblemRow>,
    pub attempts: Vec<AttemptRow>,
    pub outcomes: Vec<OutcomeRow>,
    pub aliases: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Current {
    pub agent_did: String,
    pub executed_model_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeDecision {
    Admitted,
    UnknownKey,
    SameKey,
    AlreadyMerged,
    ClosesCycle,
}

impl MergeDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admitted => "admitted",
            Self::UnknownKey => "unknown_key",
            Self::SameKey => "same_key",
            Self::AlreadyMerged => "already_merged",
            Self::ClosesCycle => "closes_cycle",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    Same,
    OtherModel,
    OtherAgent,
    OtherModelAndAgent,
}

impl Provenance {
    pub fn text(self) -> &'static str {
        match self {
            Self::Same => "same setting",
            Self::OtherModel => "other model",
            Self::OtherAgent => "other agent",
            Self::OtherModelAndAgent => "other model and agent",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Score {
    pub passed: u64,
    pub total: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoldedAttempt {
    pub attempt_key: String,
    pub cited_problem_key: String,
    pub accepted: bool,
    pub predicted: Vec<String>,
    pub fixed: Vec<String>,
    pub missed: Vec<String>,
    pub unresolved: Vec<String>,
    pub outcome_keys: Vec<String>,
    pub latest: Option<Score>,
    pub provenance: Provenance,
    pub created_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoldedProblem {
    pub problem_key: String,
    pub name: Option<String>,
    pub carried_from: Option<String>,
    pub merged: Vec<String>,
    pub attempts: Vec<FoldedAttempt>,
    pub latest: Option<Score>,
    pub last_active: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Experience {
    pub problems: Vec<FoldedProblem>,
    pub unattached: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Index {
    pub lines: Vec<String>,
    pub omitted: u64,
}

pub fn normalize(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

pub fn lands_on(problems: &[ProblemRow], root_cause: &str) -> Option<String> {
    let wanted = normalize(root_cause);
    problems
        .iter()
        .find(|problem| normalize(&problem.name) == wanted)
        .map(|problem| problem.problem_key.clone())
}

fn next(aliases: &[(String, String)], key: &str) -> Option<String> {
    aliases
        .iter()
        .find(|(from, _)| from == key)
        .map(|(_, to)| to.clone())
}

fn cycle_least(seen: &[String], key: &str) -> String {
    let start = seen.iter().position(|entry| entry == key).unwrap_or(0);
    seen[start..]
        .iter()
        .fold(key.to_owned(), |least, entry| least.min(entry.clone()))
}

fn resolve_seen(
    aliases: &[(String, String)],
    fuel: usize,
    seen: &mut Vec<String>,
    key: &str,
) -> String {
    if fuel == 0 {
        return key.to_owned();
    }
    if seen.iter().any(|entry| entry == key) {
        return cycle_least(seen, key);
    }
    match next(aliases, key) {
        Some(target) => {
            seen.push(key.to_owned());
            resolve_seen(aliases, fuel - 1, seen, &target)
        }
        None => key.to_owned(),
    }
}

pub fn resolve(aliases: &[(String, String)], key: &str) -> String {
    resolve_seen(aliases, aliases.len() + 1, &mut Vec::new(), key)
}

pub fn merge_admitted(
    problems: &[String],
    aliases: &[(String, String)],
    from_key: &str,
    to_key: &str,
) -> MergeDecision {
    if !problems.iter().any(|key| key == from_key) || !problems.iter().any(|key| key == to_key) {
        return MergeDecision::UnknownKey;
    }
    if from_key == to_key {
        return MergeDecision::SameKey;
    }
    if next(aliases, from_key).is_some() {
        return MergeDecision::AlreadyMerged;
    }
    if resolve(aliases, to_key) == from_key {
        return MergeDecision::ClosesCycle;
    }
    MergeDecision::Admitted
}

fn provenance(current: &Current, attempt: &AttemptRow) -> Provenance {
    let other_model = attempt.executed_model_name != current.executed_model_name;
    let other_agent = attempt
        .carried_from
        .as_deref()
        .is_some_and(|agent| agent != current.agent_did);
    match (other_model, other_agent) {
        (false, false) => Provenance::Same,
        (true, false) => Provenance::OtherModel,
        (false, true) => Provenance::OtherAgent,
        (true, true) => Provenance::OtherModelAndAgent,
    }
}

fn outcome_cmp(first: &OutcomeRow, second: &OutcomeRow) -> std::cmp::Ordering {
    first
        .created_at
        .cmp(&second.created_at)
        .then_with(|| first.outcome_key.cmp(&second.outcome_key))
}

fn attempt_cmp(first: &AttemptRow, second: &AttemptRow) -> std::cmp::Ordering {
    first
        .created_at
        .cmp(&second.created_at)
        .then_with(|| first.attempt_key.cmp(&second.attempt_key))
}

fn observations(outcomes: &[OutcomeRow], attempt_key: &str) -> Vec<OutcomeRow> {
    let mut observed: Vec<OutcomeRow> = outcomes
        .iter()
        .filter(|outcome| outcome.attempt_key == attempt_key)
        .cloned()
        .collect();
    observed.sort_by(outcome_cmp);
    observed
}

fn erase_dups(items: &[String]) -> Vec<String> {
    let mut unique = Vec::new();
    for item in items {
        if !unique.contains(item) {
            unique.push(item.clone());
        }
    }
    unique
}

fn fold_attempt(current: &Current, outcomes: &[OutcomeRow], attempt: &AttemptRow) -> FoldedAttempt {
    let observed = observations(outcomes, &attempt.attempt_key);
    let fixed = erase_dups(
        &observed
            .iter()
            .flat_map(|outcome| outcome.fixed.iter().cloned())
            .collect::<Vec<_>>(),
    );
    let missed = erase_dups(
        &observed
            .iter()
            .flat_map(|outcome| outcome.missed.iter().cloned())
            .collect::<Vec<_>>(),
    );
    let unresolved = erase_dups(&attempt.predicted)
        .into_iter()
        .filter(|prediction| !fixed.contains(prediction) && !missed.contains(prediction))
        .collect();
    FoldedAttempt {
        attempt_key: attempt.attempt_key.clone(),
        cited_problem_key: attempt.problem_key.clone(),
        accepted: attempt.accepted,
        predicted: attempt.predicted.clone(),
        fixed,
        missed,
        unresolved,
        outcome_keys: observed
            .iter()
            .map(|outcome| outcome.outcome_key.clone())
            .collect(),
        latest: observed.last().map(|outcome| Score {
            passed: outcome.passed,
            total: outcome.total,
        }),
        provenance: provenance(current, attempt),
        created_at: attempt.created_at,
    }
}

fn problem_keys(rows: &Rows) -> Vec<String> {
    let mut keys: Vec<String> = rows
        .problems
        .iter()
        .map(|problem| resolve(&rows.aliases, &problem.problem_key))
        .chain(
            rows.attempts
                .iter()
                .map(|attempt| resolve(&rows.aliases, &attempt.problem_key)),
        )
        .collect();
    let mut unique = Vec::new();
    for key in keys.drain(..) {
        if !unique.contains(&key) {
            unique.push(key);
        }
    }
    unique.sort();
    unique
}

fn fold_problem(current: &Current, rows: &Rows, key: &str) -> FoldedProblem {
    let row = rows
        .problems
        .iter()
        .find(|problem| problem.problem_key == key);
    let mut filed: Vec<AttemptRow> = rows
        .attempts
        .iter()
        .filter(|attempt| resolve(&rows.aliases, &attempt.problem_key) == key)
        .cloned()
        .collect();
    filed.sort_by(attempt_cmp);
    let mut observed: Vec<OutcomeRow> = rows
        .outcomes
        .iter()
        .filter(|outcome| {
            filed
                .iter()
                .any(|attempt| attempt.attempt_key == outcome.attempt_key)
        })
        .cloned()
        .collect();
    observed.sort_by(outcome_cmp);
    let mut merged: Vec<String> = rows
        .problems
        .iter()
        .map(|problem| problem.problem_key.clone())
        .filter(|other| other != key && resolve(&rows.aliases, other) == key)
        .collect();
    merged = erase_dups(&merged);
    merged.sort();
    let last_active = row
        .map(|problem| problem.created_at)
        .into_iter()
        .chain(filed.iter().map(|attempt| attempt.created_at))
        .chain(observed.iter().map(|outcome| outcome.created_at))
        .max()
        .unwrap_or(0);
    FoldedProblem {
        problem_key: key.to_owned(),
        name: row.map(|problem| problem.name.clone()),
        carried_from: row.and_then(|problem| problem.carried_from.clone()),
        merged,
        attempts: filed
            .iter()
            .map(|attempt| fold_attempt(current, &rows.outcomes, attempt))
            .collect(),
        latest: observed.last().map(|outcome| Score {
            passed: outcome.passed,
            total: outcome.total,
        }),
        last_active,
    }
}

pub fn fold(current: &Current, rows: &Rows) -> Experience {
    let mut unattached: Vec<OutcomeRow> = rows
        .outcomes
        .iter()
        .filter(|outcome| {
            !rows
                .attempts
                .iter()
                .any(|attempt| attempt.attempt_key == outcome.attempt_key)
        })
        .cloned()
        .collect();
    unattached.sort_by(outcome_cmp);
    Experience {
        problems: problem_keys(rows)
            .iter()
            .map(|key| fold_problem(current, rows, key))
            .collect(),
        unattached: unattached
            .iter()
            .map(|outcome| outcome.outcome_key.clone())
            .collect(),
    }
}

pub(crate) const NAME_LIMIT: usize = 120;

fn excerpt(text: &str) -> String {
    if text.chars().count() <= NAME_LIMIT {
        text.to_owned()
    } else {
        let mut taken: String = text.chars().take(NAME_LIMIT).collect();
        taken.push_str("...");
        taken
    }
}

pub(crate) fn unresolved_count(problem: &FoldedProblem) -> u64 {
    problem
        .attempts
        .iter()
        .map(|attempt| attempt.unresolved.len() as u64)
        .sum()
}

pub(crate) fn flags(problem: &FoldedProblem) -> Vec<Provenance> {
    [
        Provenance::Same,
        Provenance::OtherModel,
        Provenance::OtherAgent,
        Provenance::OtherModelAndAgent,
    ]
    .into_iter()
    .filter(|relation| {
        problem
            .attempts
            .iter()
            .any(|attempt| attempt.provenance == *relation)
    })
    .collect()
}

fn line(problem: &FoldedProblem) -> String {
    let latest = match problem.latest {
        Some(score) => format!("{}/{}", score.passed, score.total),
        None => "none".to_owned(),
    };
    let origin = match flags(problem) {
        relations if relations.is_empty() => String::new(),
        relations => format!(
            "; from {}",
            relations
                .iter()
                .map(|relation| relation.text())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    let name = problem
        .name
        .as_deref()
        .map(excerpt)
        .unwrap_or_else(|| "(no problem row)".to_owned());
    format!(
        "{} {} [attempts {}; latest {}; unresolved {}{}]",
        problem.problem_key,
        name,
        problem.attempts.len(),
        latest,
        unresolved_count(problem),
        origin
    )
}

fn rank_of(ranks: &[String], problem: &FoldedProblem) -> usize {
    ranks
        .iter()
        .take_while(|key| !(**key == problem.problem_key || problem.merged.contains(key)))
        .count()
}

fn shown_before(
    ranks: &[String],
    first: &FoldedProblem,
    second: &FoldedProblem,
) -> std::cmp::Ordering {
    let first_rank = rank_of(ranks, first);
    let second_rank = rank_of(ranks, second);
    let first_open = unresolved_count(first) > 0;
    let second_open = unresolved_count(second) > 0;
    if first_rank != second_rank {
        first_rank.cmp(&second_rank)
    } else if first_open != second_open {
        second_open.cmp(&first_open)
    } else if first.last_active != second.last_active {
        second.last_active.cmp(&first.last_active)
    } else {
        first.problem_key.cmp(&second.problem_key)
    }
}

fn fit(budget: usize, lines: &[String]) -> Vec<String> {
    let mut shown = Vec::new();
    let mut remaining = budget;
    for line in lines {
        let cost = line.chars().count() + 1;
        if cost > remaining {
            break;
        }
        remaining -= cost;
        shown.push(line.clone());
    }
    shown
}

pub fn recall(experience: &Experience, ranks: &[String], budget: usize) -> Index {
    let mut problems = experience.problems.clone();
    problems.sort_by(|first, second| shown_before(ranks, first, second));
    let lines: Vec<String> = problems.iter().map(line).collect();
    let shown = fit(budget, &lines);
    Index {
        omitted: (experience.problems.len() - shown.len()) as u64,
        lines: shown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_lean_vocab::families;

    #[derive(serde::Deserialize)]
    struct FamilyRows {
        problems: Vec<ProblemRow>,
        attempts: Vec<AttemptRow>,
        outcomes: Vec<OutcomeRow>,
        aliases: Vec<FamilyAlias>,
    }

    #[derive(serde::Deserialize)]
    struct FamilyAlias {
        from_problem_key: String,
        to_problem_key: String,
    }

    fn current(agent_did: &str, executed_model_name: &str) -> Current {
        Current {
            agent_did: agent_did.to_owned(),
            executed_model_name: executed_model_name.to_owned(),
        }
    }

    fn aliases(input: &[FamilyAlias]) -> Vec<(String, String)> {
        input
            .iter()
            .map(|alias| (alias.from_problem_key.clone(), alias.to_problem_key.clone()))
            .collect()
    }

    fn decode<T>(value: impl serde::Serialize) -> T
    where
        T: serde::de::DeserializeOwned,
    {
        let value = serde_json::to_value(value).expect("family input encodes");
        serde_json::from_value(value).expect("family input matches the production type")
    }

    fn generated<T>(value: impl serde::Serialize) -> T
    where
        T: serde::de::DeserializeOwned,
    {
        let value = serde_json::to_value(value).expect("production output encodes");
        serde_json::from_value(value).expect("production output matches the family schema")
    }

    fn rows<T>(input: &T) -> Rows
    where
        T: serde::Serialize,
    {
        let parsed: FamilyRows = decode(input);
        Rows {
            problems: parsed.problems,
            attempts: parsed.attempts,
            outcomes: parsed.outcomes,
            aliases: aliases(&parsed.aliases),
        }
    }

    #[test]
    fn memory_experience_fold_replays_the_lean_family() {
        families::memory_experience_fold::replay::<Provenance>(|input| {
            let experience = fold(
                &current(&input.current.agent_did, &input.current.executed_model_name),
                &rows(&input.rows),
            );
            let folded: families::memory_experience_fold::Experience<Provenance> =
                generated(experience);
            folded
        });
    }

    #[test]
    fn memory_experience_recall_replays_the_lean_family() {
        families::memory_experience_recall::replay(|input| {
            let experience = fold(
                &current(&input.current.agent_did, &input.current.executed_model_name),
                &rows(&input.rows),
            );
            generated::<families::memory_experience_recall::Index>(recall(
                &experience,
                &input.ranks,
                input.budget as usize,
            ))
        });
    }

    #[test]
    fn memory_experience_merge_admission_replays_the_lean_family() {
        families::memory_experience_merge_admission::replay(|input| {
            let problems: Vec<ProblemRow> = decode(&input.problems);
            let merged_aliases: Vec<FamilyAlias> = decode(&input.aliases);
            let merged_aliases = aliases(&merged_aliases);
            let keys: Vec<String> = problems
                .iter()
                .map(|problem| problem.problem_key.clone())
                .collect();
            let decision = merge_admitted(
                &keys,
                &merged_aliases,
                &input.from_problem_key,
                &input.to_problem_key,
            );
            families::memory_experience_merge_admission::MergeOutput {
                decision,
                lands_on: lands_on(&problems, &input.root_cause),
            }
        });
    }
}
