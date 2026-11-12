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

    fn mode(value: &str) -> Mode {
        match value {
            "ask" => Mode::Ask,
            "auto_review" => Mode::AutoReview,
            "full_access" => Mode::FullAccess,
            other => panic!("unknown mode {other}"),
        }
    }

    fn mode_name(value: Mode) -> String {
        match value {
            Mode::Ask => "ask",
            Mode::AutoReview => "auto_review",
            Mode::FullAccess => "full_access",
        }
        .into()
    }

    fn review(value: &str) -> Review {
        match value {
            "allow" => Review::Allow,
            "ask" => Review::Ask,
            "deny" => Review::Deny,
            "unavailable" => Review::Unavailable,
            other => panic!("unknown review {other}"),
        }
    }

    fn route_name(value: Route) -> String {
        match value {
            Route::Allow => "allow",
            Route::Ask => "ask",
            Route::Review => "review",
            Route::Deny => "deny",
        }
        .into()
    }

    #[test]
    fn command_grant_lookup_follows_the_lean_model() {
        use gents_lean_vocab::families::workspace_command_grants_lookup::{
            Grant as Wire, LookupInput,
        };
        let grant = |wire: &Wire| Grant {
            scope: Scope {
                workspace: wire.scope.workspace,
                revision: wire.scope.revision,
            },
            action: wire.action,
        };
        gents_lean_vocab::families::workspace_command_grants_lookup::replay(
            |case: &LookupInput| {
                has_command_grant(
                    &case.grants.iter().map(grant).collect::<Vec<_>>(),
                    &grant(&case.expected),
                )
            },
        );
    }

    #[test]
    fn workspace_mode_selection_follows_the_lean_model() {
        use gents_lean_vocab::families::workspace_permissions_selection::{
            Preference as Wire, SelectionInput, WriteResult,
        };
        let preference = |wire: &Wire<String>| Preference {
            revision: wire.revision,
            mode: mode(&wire.mode),
            review_task: wire.review_task,
        };
        let wire = |value: &Preference<u64>| Wire {
            revision: value.revision,
            mode: mode_name(value.mode),
            review_task: value.review_task,
        };
        gents_lean_vocab::families::workspace_permissions_selection::replay::<String>(
            |case: &SelectionInput<String>| {
                let previous = case.previous.as_ref().map(preference);
                match select_mode(
                    previous.as_ref(),
                    case.expected,
                    mode(&case.mode),
                    case.requested_task,
                ) {
                    Ok(selection) => WriteResult {
                        accepted: true,
                        changed: selection.changed,
                        next: Some(wire(&selection.next)),
                    },
                    Err(_) => WriteResult {
                        accepted: false,
                        changed: false,
                        next: previous.as_ref().map(wire),
                    },
                }
            },
        );
    }

    #[test]
    fn workspace_routing_follows_the_lean_model() {
        use gents_lean_vocab::families::workspace_permissions_routing::{
            Grant as GrantWire, RoutingInput, RoutingOutput,
        };
        let grant = |wire: &GrantWire| Grant {
            scope: Scope {
                workspace: wire.scope.workspace,
                revision: wire.scope.revision,
            },
            action: wire.action,
        };
        gents_lean_vocab::families::workspace_permissions_routing::replay::<String, String, String>(
            |case: &RoutingInput<String, String>| {
                let input = Input {
                    current: Scope {
                        workspace: case.input.current.workspace,
                        revision: case.input.current.revision,
                    },
                    configured_workspace: case.input.configured_workspace,
                    mode: mode(&case.input.mode),
                    action: case.input.action,
                    ceiling_allows: case.input.ceiling_allows,
                    crosses_boundary: case.input.crosses_boundary,
                    grant: case.input.grant.as_ref().map(grant),
                };
                RoutingOutput {
                    valid_grant: valid_grant(&input),
                    route: route_name(route(&input)),
                    reviewed_route: route_name(reviewed_route(
                        &input,
                        review(&case.review),
                        &grant(&case.reviewed_grant),
                    )),
                }
            },
        );
    }
}
