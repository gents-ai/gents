//! Client-key journals that remain after the chat send became an optimistic
//! send (Lean `ClientOptimisticSend`): the Goal command receipt and the Agent
//! publication record. A chat send keeps no journal: the frontend keeps the
//! payload until it is confirmed, and the runtime's create is idempotent on
//! the request identity.

pub(crate) mod goal_delivery;
pub(crate) mod journal;
