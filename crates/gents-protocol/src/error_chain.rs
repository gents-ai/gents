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
