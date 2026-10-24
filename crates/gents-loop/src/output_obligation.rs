use std::future::Future;
use std::pin::Pin;

#[derive(Debug, thiserror::Error)]
pub enum OutputObligationError {
    #[error("output obligation check failed")]
    Check {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnmetOutputObligation {
    pub tool_name: String,
    pub minimum_writes: usize,
    pub completed_writes: usize,
    pub expected_writes: Option<usize>,
    pub expected_count_field: Option<String>,
}

impl UnmetOutputObligation {
    pub fn new(
        tool_name: impl Into<String>,
        minimum_writes: usize,
        completed_writes: usize,
        expected_writes: Option<usize>,
        expected_count_field: Option<String>,
    ) -> Self {
        Self {
            tool_name: tool_name.into(),
            minimum_writes,
            completed_writes,
            expected_writes,
            expected_count_field,
        }
    }
}

pub trait OutputObligationCheck: Send + Sync {
    fn unmet<'a>(
        &'a self,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Vec<UnmetOutputObligation>, OutputObligationError>>
                + Send
                + 'a,
        >,
    >;
}

pub fn continuation_message(obligations: &[UnmetOutputObligation]) -> String {
    let requirements = obligations
        .iter()
        .map(|obligation| {
            if let Some(expected) = obligation.expected_writes {
                format!(
                    "`{}` exactly {expected} total time(s) ({} completed, {} remaining)",
                    obligation.tool_name,
                    obligation.completed_writes,
                    expected.saturating_sub(obligation.completed_writes),
                )
            } else if let Some(field) = &obligation.expected_count_field {
                format!(
                    "`{}` at least once to declare the exact closed-set size in `{field}` ({} completed)",
                    obligation.tool_name, obligation.completed_writes
                )
            } else {
                format!(
                    "`{}` at least {} total time(s) ({} completed)",
                    obligation.tool_name, obligation.minimum_writes, obligation.completed_writes
                )
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "The request cannot complete yet because its configured output obligation is unmet. \
         Complete the required durable write before answering: {requirements}."
    )
}
