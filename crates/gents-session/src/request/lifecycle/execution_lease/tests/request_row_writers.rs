//! The conflict premises of `tla/BackgroundAdmissionContention`'s
//! request-row writers: a lease renewal and a background admission write
//! different fields of one `AgentRequest` document, and DefraDB aborts either
//! one when the other commits first.

use super::*;
