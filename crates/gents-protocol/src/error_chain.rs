use std::error::Error;
use std::fmt;

/// Renders an error and every `source()` joined by `": "`, byte-identical to
/// anyhow's `{:#}`. Typed errors format only their own message under `{:#}`,
/// so callers that showed a chain (logs, model-facing text, user reports)
/// format through this instead.
pub struct ErrorChain<'a>(pub &'a (dyn Error + 'a));

impl fmt::Display for ErrorChain<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)?;
        let mut source = self.0.source();
        while let Some(cause) = source {
            write!(formatter, ": {cause}")?;
            source = cause.source();
        }
        Ok(())
    }
}

/// What an operation owner can pass through: a value that lends the error it
/// carries, as an application error or a boxed error does.
pub trait PassedSource: fmt::Debug + fmt::Display + Send + Sync + 'static {
    fn top(&self) -> &(dyn Error + Send + Sync + 'static);
}

impl<T> PassedSource for T
where
    T: AsRef<dyn Error + Send + Sync + 'static> + fmt::Debug + fmt::Display + Send + Sync + 'static,
{
    fn top(&self) -> &(dyn Error + Send + Sync + 'static) {
        self.as_ref()
    }
}

/// Another owner's failure an operation owner passes through. An application
/// error converted into `Box<dyn Error>` keeps its top error's message but
/// forwards `source()` past it, so a classifier walking the chain never meets
/// that error: a store conflict or refusal on top reads as unclassified (Lean
/// `AccessClassification.hidden_is_unclassified`). This holder returns the top
/// error from `source()`, so the chain shows its message twice, the trade
/// `AccessClassification.exposed` records for every sourced holder.
#[derive(Debug)]
pub struct PassedThrough(Box<dyn PassedSource>);

impl PassedThrough {
    pub fn new<E>(error: E) -> Self
    where
        E: PassedSource,
    {
        Self(Box::new(error))
    }

    pub fn boxed<E>(error: E) -> Box<dyn Error + Send + Sync>
    where
        E: PassedSource,
    {
        Box::new(Self::new(error))
    }

    /// The passed-through error itself, the first error on its chain.
    pub fn get(&self) -> &(dyn Error + Send + Sync + 'static) {
        self.0.top()
    }
}

impl fmt::Display for PassedThrough {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

impl Error for PassedThrough {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.0.top())
    }
}

/// Finds the first error of type `T` in `error`'s `source()` chain, starting
/// with `error` itself. A `#[error(transparent)]` wrapper forwards `source()`
/// past the error it wraps, so a type found this way must be wrapped by a
/// variant with its own message and a `#[source]` field.
pub fn find_source<'a, T>(error: &'a (dyn Error + 'static)) -> Option<&'a T>
where
    T: Error + 'static,
{
    find_map_source(error, |cause| cause.downcast_ref::<T>())
}

/// Returns the first answer `classify` gives for `error` or any error in its
/// `source()` chain. Boundaries that receive an application (`anyhow`) or
/// boxed error use it to reach each owner enum and ask that enum, whose
/// classifier matches its own variants, including transparent wrappers that
/// `source()` skips.
pub fn find_map_source<'a, T>(
    error: &'a (dyn Error + 'static),
    classify: impl FnMut(&'a (dyn Error + 'static)) -> Option<T>,
) -> Option<T> {
    std::iter::successors(Some(error), |&cause| cause.source()).find_map(classify)
}

#[cfg(test)]
mod tests {
    use super::{find_source, ErrorChain};

    #[derive(Debug, thiserror::Error)]
    #[error("outer {id}")]
    struct Outer {
        id: u32,
        #[source]
        source: std::io::Error,
    }

    #[test]
    fn renders_like_anyhow_alternate() {
        let error = Outer {
            id: 7,
            source: std::io::Error::other("inner"),
        };
        assert_eq!(ErrorChain(&error).to_string(), "outer 7: inner");
        assert_eq!(format!("{error:#}"), "outer 7");
        assert_eq!(
            find_source::<std::io::Error>(&error).map(ToString::to_string),
            Some("inner".to_string())
        );
        assert!(find_source::<std::fmt::Error>(&error).is_none());
    }
}
