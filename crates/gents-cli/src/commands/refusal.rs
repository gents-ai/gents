use gents_protocol::operations::NextCall;

/// A refused operation as a person at a terminal reads it: what happened,
/// then the command that corrects it.
#[derive(Debug)]
pub(crate) struct Refusal<E> {
    error: E,
    next: NextCall,
}

impl<E> Refusal<E> {
    pub(crate) fn new(error: E, next: NextCall) -> Self {
        Self { error, next }
    }
}

impl<E> std::fmt::Display for Refusal<E>
where
    E: std::fmt::Display,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}\nNext: {}", self.error, crate::next_call::command(self.next, None))
    }
}

impl<E> std::error::Error for Refusal<E>
where
    E: std::error::Error + 'static,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.error.source()
    }
}
