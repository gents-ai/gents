//! Enrollment fixtures shared by this owner's tests and by the root crate's
//! claim-boundary tests, which cannot reach the private authority loop.

pub use crate::test_support::enrollment::{
    active_enrollment, projection_with_active, projection_with_revoked, spawn_scripted_authority,
    ScriptedProjection, ScriptedProjectionReader,
};
