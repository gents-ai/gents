use std::any::{Any, TypeId};
use std::sync::Arc;

use rapidhash::RapidHashMap;

use crate::tool_call_lifecycle::runtime::{
    current_tool_runtime_context, debug_assert_same_runtime, CurrentToolRuntimeContext,
};
use crate::tool_effects::ToolEffectDispatcher;

/// The call a tool is serving. The loop, the hook and the plugin host build it
/// per call and pass it by reference.
///
/// While the task-local statics it replaces still exist (#3765), their owners
/// keep setting them, every explicit read through [`Self::runtime`] and
/// [`Self::effects`] is compared with the ambient value in debug builds, and
/// [`Self::ambient`] stands in for a caller that has no envelope yet.
#[derive(Clone, Default)]
pub struct ToolCallEnvelope {
    runtime: Slot<CurrentToolRuntimeContext>,
    effects: Slot<Arc<dyn ToolEffectDispatcher>>,
    extensions: ToolCallExtensions,
}

/// `Ambient` reads the task-local value at each read, so a placeholder built
/// before a scope is entered still sees that scope.
#[derive(Clone)]
enum Slot<T> {
    Ambient,
    Explicit(Option<T>),
}

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Self::Explicit(None)
    }
}

impl ToolCallEnvelope {
    pub fn new(runtime: Option<CurrentToolRuntimeContext>) -> Self {
        Self::default().with_runtime(runtime)
    }

    /// The placeholder for a caller that has no envelope yet: every value is
    /// the task-local one at the time it is read. Exists only until those
    /// statics are deleted (#3765).
    pub fn ambient() -> Self {
        Self {
            runtime: Slot::Ambient,
            effects: Slot::Ambient,
            extensions: ToolCallExtensions::ambient(),
        }
    }

    /// The request execution scope the call runs under; `None` outside one.
    #[track_caller]
    pub fn runtime(&self) -> Option<CurrentToolRuntimeContext> {
        match &self.runtime {
            Slot::Ambient => current_tool_runtime_context(),
            Slot::Explicit(runtime) => {
                debug_assert_same_runtime(
                    runtime.as_ref(),
                    current_tool_runtime_context().as_ref(),
                );
                runtime.clone()
            }
        }
    }

    /// The enclosing context a constructor derives a new one from. Unlike
    /// [`Self::runtime`] it is not compared with the ambient value, because a
    /// context built for a spawned task deliberately encloses nothing.
    pub fn runtime_unchecked(&self) -> Option<CurrentToolRuntimeContext> {
        match &self.runtime {
            Slot::Ambient => current_tool_runtime_context(),
            Slot::Explicit(runtime) => runtime.clone(),
        }
    }

    /// The dispatcher for this call's child tool calls, if it may compose.
    #[track_caller]
    pub fn effects(&self) -> Option<Arc<dyn ToolEffectDispatcher>> {
        match &self.effects {
            Slot::Ambient => crate::tool_effects::current(),
            Slot::Explicit(effects) => {
                debug_assert_same_effects(
                    effects.as_ref(),
                    crate::tool_effects::current().as_ref(),
                );
                effects.clone()
            }
        }
    }

    /// The dispatcher without comparing it with the ambient one; see
    /// [`Self::runtime_unchecked`].
    pub fn effects_unchecked(&self) -> Option<Arc<dyn ToolEffectDispatcher>> {
        match &self.effects {
            Slot::Ambient => crate::tool_effects::current(),
            Slot::Explicit(effects) => effects.clone(),
        }
    }

    pub fn extensions(&self) -> &ToolCallExtensions {
        &self.extensions
    }

    pub fn with_runtime(mut self, runtime: Option<CurrentToolRuntimeContext>) -> Self {
        self.runtime = Slot::Explicit(runtime);
        self
    }

    pub fn with_effects(mut self, effects: Option<Arc<dyn ToolEffectDispatcher>>) -> Self {
        self.effects = Slot::Explicit(effects);
        self
    }

    pub fn with_extension<T>(mut self, value: T) -> Self
    where
        T: Any + Send + Sync,
    {
        self.extensions = self.extensions.with(value);
        self
    }

    pub fn with_extensions(mut self, extensions: ToolCallExtensions) -> Self {
        self.extensions = extensions;
        self
    }

    /// Debug builds only: panics when an explicit runtime context differs from
    /// the ambient one.
    #[track_caller]
    pub fn debug_assert_ambient_runtime(&self) {
        if let Slot::Explicit(runtime) = &self.runtime {
            debug_assert_same_runtime(runtime.as_ref(), current_tool_runtime_context().as_ref());
        }
    }

    /// Debug builds only: panics when an explicit runtime context or
    /// dispatcher differs from the ambient one. Placeholder values are the
    /// ambient ones and are not compared.
    #[track_caller]
    pub fn debug_assert_ambient(&self) {
        if !cfg!(debug_assertions) {
            return;
        }
        if let Slot::Explicit(runtime) = &self.runtime {
            debug_assert_same_runtime(runtime.as_ref(), current_tool_runtime_context().as_ref());
        }
        if let Slot::Explicit(effects) = &self.effects {
            debug_assert_same_effects(effects.as_ref(), crate::tool_effects::current().as_ref());
        }
    }
}

#[track_caller]
fn debug_assert_same_effects(
    explicit: Option<&Arc<dyn ToolEffectDispatcher>>,
    ambient: Option<&Arc<dyn ToolEffectDispatcher>>,
) {
    if !cfg!(debug_assertions) {
        return;
    }
    let address = |effects: Option<&Arc<dyn ToolEffectDispatcher>>| {
        effects.map(|effects| Arc::as_ptr(effects).cast::<()>())
    };
    assert!(
        address(explicit) == address(ambient),
        "explicit tool effect dispatcher differs from the ambient one: explicit present {}, ambient present {}",
        explicit.is_some(),
        ambient.is_some()
    );
}

type ExtensionMap = RapidHashMap<TypeId, Arc<dyn Any + Send + Sync>>;

/// Values of the layers above the loop, keyed by type. The loop carries them
/// without reading them; each owner crate reads its own through a typed
/// accessor, which gives its default when the value is absent.
#[derive(Clone, Default)]
pub struct ToolCallExtensions {
    values: Option<Arc<ExtensionMap>>,
    ambient: bool,
}

impl ToolCallExtensions {
    fn ambient() -> Self {
        Self {
            values: None,
            ambient: true,
        }
    }

    /// Whether these are the placeholder's: their owners then read the
    /// task-local values (#3765).
    pub fn is_ambient(&self) -> bool {
        self.ambient
    }

    pub fn get<T>(&self) -> Option<&T>
    where
        T: Any + Send + Sync,
    {
        self.values
            .as_ref()?
            .get(&TypeId::of::<T>())
            .and_then(|value| (**value).downcast_ref::<T>())
    }

    /// A copy with `value` set. Setting a value on the placeholder's
    /// extensions makes them explicit, holding only that value.
    pub fn with<T>(&self, value: T) -> Self
    where
        T: Any + Send + Sync,
    {
        let mut values = if self.ambient {
            ExtensionMap::default()
        } else {
            self.values.as_deref().cloned().unwrap_or_default()
        };
        values.insert(TypeId::of::<T>(), Arc::new(value));
        Self {
            values: Some(Arc::new(values)),
            ambient: false,
        }
    }
}
