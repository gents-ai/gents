/// One component of a requested path or of a link's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Component {
    Name,
    Parent,
}

/// What one lookup in the held directory reports for a name, without
/// following a link there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Observed {
    Directory,
    NonDirectory,
    Missing,
    LinkRelative,
    LinkBeneathRoot,
    LinkOutsideRoot,
}

/// What the walk does with one component.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Ascend,
    Descend,
    Land,
    LandMissing,
    PendingCreate,
    FollowRelative,
    FollowFromRoot,
    RefuseEscape,
    RefuseNotDirectory,
    RefuseNotFound,
    RefuseDanglingLink,
    RefuseLoop,
}

/// The facts one component is decided on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StepInput {
    pub component: Component,
    /// The root is the only held handle.
    pub at_root: bool,
    /// No component follows.
    pub last: bool,
    /// The operation creates its target.
    pub create: bool,
    /// The component came from a link's text.
    pub via_link: bool,
    /// Another link may still be followed.
    pub hops_left: bool,
    pub observed: Observed,
}

/// Lean `PathNamespace.step`.
pub fn step(input: StepInput) -> Step {
    match input.component {
        Component::Parent if input.at_root => Step::RefuseEscape,
        Component::Parent => Step::Ascend,
        Component::Name => match input.observed {
            Observed::Directory if input.last => Step::Land,
            Observed::Directory => Step::Descend,
            Observed::NonDirectory if input.last => Step::Land,
            Observed::NonDirectory => Step::RefuseNotDirectory,
            Observed::Missing if !input.create => Step::RefuseNotFound,
            Observed::Missing if input.via_link => Step::RefuseDanglingLink,
            Observed::Missing if input.last => Step::LandMissing,
            Observed::Missing => Step::PendingCreate,
            Observed::LinkOutsideRoot => Step::RefuseEscape,
            Observed::LinkRelative if input.hops_left => Step::FollowRelative,
            Observed::LinkBeneathRoot if input.hops_left => Step::FollowFromRoot,
            Observed::LinkRelative | Observed::LinkBeneathRoot => Step::RefuseLoop,
        },
    }
}
