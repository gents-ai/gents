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
    if !input.crosses_boundary || valid_grant(input) {
        return Route::Allow;
    }
    let mode = if input.configured_workspace == input.current.workspace {
        input.mode
    } else {
        Mode::Ask
    };
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
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct WriteCase {
        previous: Option<Preference<u64>>,
        expected: u64,
        mode: Mode,
        requested_task: Option<u64>,
        accepted: bool,
        changed: bool,
        next: Option<Preference<u64>>,
    }

    #[derive(Deserialize)]
    struct Snapshot {
        workspace_permission_cases: Vec<Case>,
        workspace_permission_write_cases: Vec<WriteCase>,
    }

    #[test]
    fn generated_workspace_permission_write_cases() {
        let snapshot: Snapshot = gents_lean_contract::load_contract_snapshot().unwrap();
        assert!(!snapshot.workspace_permission_write_cases.is_empty());
        for case in snapshot.workspace_permission_write_cases {
            let result = select_mode(
                case.previous.as_ref(),
                case.expected,
                case.mode,
                case.requested_task,
            );
            assert_eq!(result.is_ok(), case.accepted);
            match result {
                Ok(selection) => {
                    assert_eq!(Some(selection.next), case.next);
                    assert_eq!(selection.changed, case.changed);
                }
                Err(_) => {
                    assert_eq!(case.previous, case.next);
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
