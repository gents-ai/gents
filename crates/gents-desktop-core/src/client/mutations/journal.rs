//! Client-key journals: the Goal command receipt and the Agent publication
//! record. Both are rows of the `PendingChatSubmission` collection, whose name
//! is persisted schema. A chat send keeps no journal (Lean
//! `ClientOptimisticSend`).

pub(crate) mod goal_delivery;
pub(crate) mod records;
