use std::error::Error;
use std::fmt;

/// The messages of `error` and every `source()`, outermost first, with a
/// message that repeats the one just printed left out (Lean
/// `AccessClassification.collapse`). An owner variant that holds an error as
/// its `#[source]` and a passthrough ([`PassedThrough`]) keep that error's text
/// as their own message, so a printer of the whole chain meets it twice in a
/// row, and printing it twice says nothing more. A message repeats the one
/// just printed when it equals it or is that message's first line, as for a
/// report that adds lines below the error it reports (`Next: ...`).
pub fn printed_messages(error: &(dyn Error + '_)) -> Vec<String> {
    let mut printed: Vec<String> = vec![error.to_string()];
    let mut source = error.source();
    while let Some(cause) = source {
        let message = cause.to_string();
        let previous = printed.last().map(String::as_str).unwrap_or_default();
        let repeats = previous == message
            || previous
                .strip_prefix(message.as_str())
                .is_some_and(|rest| rest.starts_with('\n'));
        if !repeats {
            printed.push(message);
        }
        source = cause.source();
    }
    printed
}

/// Renders an error and every `source()` joined by `": "`, as anyhow's `{:#}`
/// does, except that a repeated message is printed once
/// ([`printed_messages`]). Typed errors format only their own message under
/// `{:#}`, so callers that showed a chain (logs, model-facing text, user
/// reports) format through this instead.
pub struct ErrorChain<'a>(pub &'a (dyn Error + 'a));

impl fmt::Display for ErrorChain<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&printed_messages(self.0).join(": "))
    }
}

/// Renders an error as anyhow's `{:?}` lays it out (the message, then a
/// `Caused by:` list of its sources), except that a repeated message is
/// printed once ([`printed_messages`]). The CLI reports a failed command with
/// this; anyhow's own `{:?}` prints a passthrough's text twice (Lean
/// `AccessClassification.joined_passthrough_repeats`).
pub struct ErrorReport<'a>(pub &'a (dyn Error + 'a));

impl fmt::Display for ErrorReport<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let printed = printed_messages(self.0);
        let (message, causes) = printed.split_first().expect("the error's own message");
        formatter.write_str(message)?;
        if causes.is_empty() {
            return Ok(());
        }
        formatter.write_str("\n\nCaused by:")?;
        let numbered = causes.len() > 1;
        for (index, cause) in causes.iter().enumerate() {
            formatter.write_str("\n    ")?;
            if numbered {
                write!(formatter, "{index}: ")?;
            }
            let indent = if numbered { "\n       " } else { "\n    " };
            formatter.write_str(&cause.replace('\n', indent))?;
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
/// `AccessClassification.Chain.hidden_passthrough_is_not_found`). This holder
/// returns the top error from `source()`, so classifiers find it
/// (`AccessClassification.Chain.passthrough_is_found`); it shows that error's
/// message as its own, which [`ErrorChain`] and [`ErrorReport`] print once
/// (`AccessClassification.passthrough_is_printed_once`).
#[derive(Debug)]
pub struct PassedThrough(Box<dyn PassedSource>);

impl PassedThrough {
    pub fn new<E>(error: E) -> Self
    where
        E: PassedSource,
    {
        Self(Box::new(error))
    }

    /// Passes a typed owner error through as itself.
    pub fn error<E>(error: E) -> Self
    where
        E: Error + Send + Sync + 'static,
    {
        Self::new(Box::new(error) as Box<dyn Error + Send + Sync>)
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
    use super::{find_source, ErrorChain, ErrorReport, PassedThrough};

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

    /// A report that adds a line below the error it reports.
    #[derive(Debug, thiserror::Error)]
    #[error("{0}\nNext: retry")]
    struct Reported(#[source] Holder);

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
    fn a_report_lists_each_cause_once_as_anyhow_lays_it_out() {
        let single = Reading(Holder::Held(std::io::Error::other("denied")));
        assert_eq!(
            ErrorReport(&single).to_string(),
            "reading\n\nCaused by:\n    denied"
        );
        let passed = Holder::Repeating(Repeated(std::io::Error::other("denied")));
        let passed = PassedThrough::error(Reading(passed));
        assert_eq!(
            ErrorReport(&passed).to_string(),
            "reading\n\nCaused by:\n    0: holder\n    1: denied"
        );
        let reported = Reported(Holder::Held(std::io::Error::other("denied")));
        assert_eq!(ErrorReport(&reported).to_string(), "denied\nNext: retry");
        let alone = std::io::Error::other("one\ntwo");
        assert_eq!(ErrorReport(&alone).to_string(), "one\ntwo");
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
