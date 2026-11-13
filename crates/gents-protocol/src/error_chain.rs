use std::error::Error;
use std::fmt;

/// Renders an error and every `source()` joined by `": "`, as anyhow's `{:#}`
/// does, except that a source whose message repeats the message just printed
/// is printed once: an owner variant that holds an error as its `#[source]`
/// keeps that error's text as its own message
/// (`AccessClassification.Chain.exposed`), and printing it twice in a row says
/// nothing more. Typed errors format only their own message under `{:#}`, so
/// callers that showed a chain (logs, model-facing text, user reports) format
/// through this instead.
pub struct ErrorChain<'a>(pub &'a (dyn Error + 'a));

impl fmt::Display for ErrorChain<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut previous = self.0.to_string();
        formatter.write_str(&previous)?;
        let mut source = self.0.source();
        while let Some(cause) = source {
            let message = cause.to_string();
            if message != previous {
                write!(formatter, ": {message}")?;
                previous = message;
            }
            source = cause.source();
        }
        Ok(())
    }
}

/// Finds the first error of type `T` in `error`'s `source()` chain, starting
/// with `error` itself, held directly or through a box ([`as_owner`]). A
/// `#[error(transparent)]` wrapper forwards `source()` past the error it wraps,
/// so a type found this way must be wrapped by a variant with its own message
/// and a `#[source]` field.
pub fn find_source<'a, T>(error: &'a (dyn Error + 'static)) -> Option<&'a T>
where
    T: Error + 'static,
{
    find_map_source(error, as_owner::<T>)
}

/// `cause` as the owner `T`, whether it is that owner or a box of it. A
/// `#[source] Box<T>` field puts the box on the chain, which is a type of its
/// own and answers `source()` as the `T` in it does, so a downcast to `T`
/// alone never finds it (`AccessClassification.Chain.found_eq`). Classifiers
/// that search a chain for an owner ask through this.
pub fn as_owner<'a, T>(cause: &'a (dyn Error + 'static)) -> Option<&'a T>
where
    T: Error + 'static,
{
    exactly::<T>(cause).or_else(|| exactly::<Box<T>>(cause).map(|boxed| &**boxed))
}

fn exactly<'a, T>(cause: &'a (dyn Error + 'static)) -> Option<&'a T>
where
    T: Error + 'static,
{
    cause.downcast_ref::<T>()
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

    #[derive(Debug, thiserror::Error)]
    enum Holder {
        #[error("{0}")]
        Held(#[source] std::io::Error),
        #[error("{0}")]
        Boxed(#[source] Box<std::io::Error>),
        #[error("holder")]
        Repeating(#[source] Repeated),
    }

    #[derive(Debug, thiserror::Error)]
    #[error("holder")]
    struct Repeated(#[source] std::io::Error);

    #[derive(Debug, thiserror::Error)]
    #[error("reading")]
    struct Reading(#[source] Holder);

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

    /// A sourced holder that keeps its source's text prints it once.
    #[test]
    fn a_source_repeating_the_message_before_it_is_printed_once() {
        let held = Holder::Held(std::io::Error::other("denied"));
        assert_eq!(ErrorChain(&held).to_string(), "denied");
        let context = Reading(Holder::Held(std::io::Error::other("denied")));
        assert_eq!(ErrorChain(&context).to_string(), "reading: denied");
        let repeating = Holder::Repeating(Repeated(std::io::Error::other("denied")));
        assert_eq!(ErrorChain(&repeating).to_string(), "holder: denied");
    }

    #[test]
    fn finds_an_owner_held_through_a_box() {
        let erased: Box<dyn std::error::Error + Send + Sync> = Box::new(Reading(Holder::Boxed(
            Box::new(std::io::Error::other("inner")),
        )));
        assert_eq!(
            find_source::<std::io::Error>(erased.as_ref()).map(ToString::to_string),
            Some("inner".to_string())
        );
        assert_eq!(ErrorChain(erased.as_ref()).to_string(), "reading: inner");
    }
}
