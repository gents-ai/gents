#[path = "views/bootstrap.rs"]
mod bootstrap;
#[path = "views/deployment.rs"]
mod deployment;
#[path = "views/events.rs"]
mod events;
#[path = "views/operations.rs"]
mod operations;
#[path = "views/session.rs"]
mod session;

pub(crate) use bootstrap::*;
pub(crate) use deployment::*;
pub(crate) use events::*;
pub(crate) use operations::*;
pub(crate) use session::*;
