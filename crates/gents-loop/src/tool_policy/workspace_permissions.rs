use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(rename = "WorkspacePermissionMode"))]
pub enum Mode {
    #[default]
    Ask,
    AutoReview,
    FullAccess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Route {
    Allow,
    Ask,
    Review,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope<W> {
    pub workspace: W,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant<W, A> {
    pub scope: Scope<W>,
    pub action: A,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ApprovalKind {
    Filesystem,
    Command,
    FileMutation,
}

pub fn can_remember_command(kind: ApprovalKind, supported: bool) -> bool {
    kind == ApprovalKind::Command && supported
}

pub fn has_command_grant<W, A>(grants: &[Grant<W, A>], expected: &Grant<W, A>) -> bool
where
    W: PartialEq,
    A: PartialEq,
{
    grants.contains(expected)
}

pub fn revoke_command_grants<T>(
    previous: &Preference<T>,
    expected: u64,
) -> Result<Selection<T>, SelectionError>
where
    T: Clone,
{
    if previous.revision != expected {
        return Err(SelectionError::Stale);
    }
    let mut next = previous.clone();
    next.revision = next
        .revision
        .checked_add(1)
        .ok_or(SelectionError::RevisionExhausted)?;
    Ok(Selection {
        changed: true,
        next,
    })
}

pub fn answer_allowed(kind: ApprovalKind, remembered: bool) -> bool {
    kind == ApprovalKind::Filesystem || !remembered
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Input<W, A> {
    pub current: Scope<W>,
    pub configured_workspace: W,
    pub mode: Mode,
    pub action: A,
    pub ceiling_allows: bool,
    pub crosses_boundary: bool,
    pub grant: Option<Grant<W, A>>,
}

pub fn valid_grant<W, A>(input: &Input<W, A>) -> bool
where
    W: PartialEq,
    A: PartialEq,
{
    input
        .grant
        .as_ref()
        .is_some_and(|grant| grant.scope == input.current && grant.action == input.action)
}

pub fn resolve<'a, E, K, F>(
    entries: &'a [E],
    session: Option<&K>,
    default: &K,
    key: F,
) -> Option<&'a E>
where
    K: PartialEq,
    F: for<'b> Fn(&'b E) -> &'b K,
{
    let find = |wanted: &K| entries.iter().find(|entry| key(entry) == wanted);
    session.and_then(find).or_else(|| find(default))
}

pub fn approval_scope_matches<W>(available: &[Scope<W>], expected: &Scope<W>) -> bool
where
    W: PartialEq,
{
    available
        .iter()
        .find(|current| current.workspace == expected.workspace)
        == Some(expected)
}

pub fn consume_approval<W, A>(
    available: &[Scope<W>],
    expected: &Grant<W, A>,
    received: &Grant<W, A>,
    allowed: bool,
) -> Option<bool>
where
    W: PartialEq,
    A: PartialEq,
{
    (approval_scope_matches(available, &expected.scope) && expected == received).then_some(allowed)
}

pub fn route<W, A>(input: &Input<W, A>) -> Route
where
    W: PartialEq,
    A: PartialEq,
{
    if !input.ceiling_allows {
        return Route::Deny;
    }
    let mode = if input.configured_workspace == input.current.workspace {
        input.mode
    } else {
        Mode::Ask
    };
    if mode == Mode::Ask {
        return Route::Ask;
    }
    if !input.crosses_boundary || valid_grant(input) {
        return Route::Allow;
    }
    match mode {
        Mode::Ask => Route::Ask,
        Mode::AutoReview => Route::Review,
        Mode::FullAccess => Route::Allow,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Review {
    Allow,
    Ask,
    Deny,
    Unavailable,
}

pub fn reviewed_route<W, A>(input: &Input<W, A>, review: Review, grant: &Grant<W, A>) -> Route
where
    W: PartialEq,
    A: PartialEq,
{
    let current = route(input);
    if current != Route::Review {
        return current;
    }
    if grant.scope != input.current || grant.action != input.action {
        return Route::Deny;
    }
    match review {
        Review::Allow => Route::Allow,
        Review::Ask | Review::Unavailable => Route::Ask,
        Review::Deny => Route::Deny,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Preference<T> {
    pub revision: u64,
    pub mode: Mode,
    pub review_task: Option<T>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection<T> {
    pub changed: bool,
    pub next: Preference<T>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SelectionError {
    #[error("Workspace permissions changed in another window. Reopen the picker and try again.")]
    Stale,
    #[error("Set up an access reviewer before choosing Approve for me.")]
    ReviewerMissing,
    #[error("Workspace permission revision exhausted.")]
    RevisionExhausted,
}

pub fn select_mode<T>(
    previous: Option<&Preference<T>>,
    expected: u64,
    mode: Mode,
    requested_task: Option<T>,
) -> Result<Selection<T>, SelectionError>
where
    T: Clone + PartialEq,
{
    let revision = previous.map_or(0, |value| value.revision);
    if revision != expected {
        return Err(SelectionError::Stale);
    }
    let review_task =
        requested_task.or_else(|| previous.and_then(|value| value.review_task.clone()));
    if mode == Mode::AutoReview && review_task.is_none() {
        return Err(SelectionError::ReviewerMissing);
    }
    if let Some(previous) = previous {
        if previous.mode == mode && previous.review_task == review_task {
            return Ok(Selection {
                changed: false,
                next: previous.clone(),
            });
        }
    }
    Ok(Selection {
        changed: true,
        next: Preference {
            revision: revision
                .checked_add(1)
                .ok_or(SelectionError::RevisionExhausted)?,
            mode,
            review_task,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Case {
        input: Input<u64, u64>,
        review: Review,
        reviewed_grant: Grant<u64, u64>,
        expected_route: Route,
        expected_reviewed_route: Route,
        expected_valid_grant: bool,
    }

    #[derive(Deserialize)]
    struct Snapshot {
        workspace_permission_cases: Vec<Case>,
    }

    fn wire_mode(value: &str) -> Mode {
        serde_json::from_value(serde_json::Value::String(value.to_owned())).unwrap()
    }

    fn case_preference(mode: &str, revision: u64, review_task: Option<u64>) -> Preference<u64> {
        Preference {
            revision,
            mode: wire_mode(mode),
            review_task,
        }
    }

    #[test]
    fn generated_command_grant_lookups() {
        let cases = &gents_lean_vocab::lean_contract_snapshot().workspace_command_grant_lookups;
        assert_eq!(cases.len(), 24);
        for case in cases {
            let grants: Vec<Grant<u64, u64>> = case
                .grants
                .iter()
                .map(|grant| Grant {
                    scope: Scope {
                        workspace: grant.scope.workspace,
                        revision: grant.scope.revision,
                    },
                    action: grant.action,
                })
                .collect();
            let expected = Grant {
                scope: Scope {
                    workspace: case.expected.scope.workspace,
                    revision: case.expected.scope.revision,
                },
                action: case.expected.action,
            };
            assert_eq!(has_command_grant(&grants, &expected), case.allowed);
        }
    }

    #[test]
    fn generated_workspace_permission_write_cases() {
        let cases = &gents_lean_vocab::lean_contract_snapshot().workspace_permission_write_cases;
        assert!(!cases.is_empty());
        for case in cases {
            let previous = case
                .previous
                .as_ref()
                .map(|value| case_preference(&value.mode, value.revision, value.review_task));
            let next = case
                .next
                .as_ref()
                .map(|value| case_preference(&value.mode, value.revision, value.review_task));
            let result = select_mode(
                previous.as_ref(),
                case.expected,
                wire_mode(&case.mode),
                case.requested_task,
            );
            assert_eq!(result.is_ok(), case.accepted);
            match result {
                Ok(selection) => {
                    assert_eq!(Some(selection.next), next);
                    assert_eq!(selection.changed, case.changed);
                }
                Err(_) => {
                    assert_eq!(previous, next);
                    assert!(!case.changed);
                }
            }
        }
    }

    #[test]
    fn generated_workspace_permission_cases() {
        let snapshot: Snapshot = gents_lean_contract::load_contract_snapshot().unwrap();
        assert!(!snapshot.workspace_permission_cases.is_empty());
        for case in snapshot.workspace_permission_cases {
            assert_eq!(valid_grant(&case.input), case.expected_valid_grant);
            assert_eq!(route(&case.input), case.expected_route);
            assert_eq!(
                reviewed_route(&case.input, case.review, &case.reviewed_grant),
                case.expected_reviewed_route,
            );
        }
    }
}

#[cfg(test)]
mod mode_conformance {
    use super::Mode;

    #[test]
    fn workspace_permission_modes_are_the_lean_modes() {
        gents_lean_vocab::families::tool_policy_workspace_permissions_mode_vocabulary::replay(&[
            Mode::Ask,
            Mode::AutoReview,
            Mode::FullAccess,
        ]);
    }
}
