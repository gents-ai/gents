use serde_json::json;

use crate::cli::*;
use crate::{clear_runtime_state, print_json, resolve_home_dir, runtime_state_path};

#[derive(Debug, thiserror::Error)]
pub(crate) enum ResetError {
    #[error(transparent)]
    HomeState(#[from] crate::home_state::HomeStateError),
    #[error(transparent)]
    RequestHelpers(#[from] crate::request_helpers::RequestHelpersError),
}

pub(crate) async fn reset(args: ResetArgs) -> Result<(), ResetError> {
    let home_dir = resolve_home_dir(args.home.as_deref());
    let runtime_state_path = runtime_state_path(&home_dir);
    let cleared = clear_runtime_state(&home_dir)?;
    let output = json!({
        "status": "reset",
        "home": home_dir,
        "runtime_state_path": runtime_state_path,
        "cleared": cleared,
    });
    print_json(&output)?;
    Ok(())
}
