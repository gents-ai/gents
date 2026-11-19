use std::any::{Any, TypeId};
use std::sync::Arc;

use rapidhash::RapidHashMap;

use crate::tool_call_lifecycle::runtime::CurrentToolRuntimeContext;
use crate::tool_effects::ToolEffectDispatcher;

/// The call a tool is serving. The loop, the hook and the plugin host build it
/// per call and pass it by reference; a tool learns about the call only from
/// it.
#[derive(Clone, Default)]
pub struct ToolCallEnvelope {
    runtime: Option<CurrentToolRuntimeContext>,
    effects: Option<Arc<dyn ToolEffectDispatcher>>,
    extensions: ToolCallExtensions,
}

impl ToolCallEnvelope {
    pub fn new(runtime: Option<CurrentToolRuntimeContext>) -> Self {
        Self::default().with_runtime(runtime)
    }

    /// The request execution scope the call runs under; `None` outside one.
    pub fn runtime(&self) -> Option<CurrentToolRuntimeContext> {
        self.runtime.clone()
    }

    /// The dispatcher for this call's child tool calls, if it may compose.
    pub fn effects(&self) -> Option<Arc<dyn ToolEffectDispatcher>> {
        self.effects.clone()
    }

    pub fn extensions(&self) -> &ToolCallExtensions {
        &self.extensions
    }

    pub fn with_runtime(mut self, runtime: Option<CurrentToolRuntimeContext>) -> Self {
        self.runtime = runtime;
        self
    }

    pub fn with_effects(mut self, effects: Option<Arc<dyn ToolEffectDispatcher>>) -> Self {
        self.effects = effects;
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
}

type ExtensionMap = RapidHashMap<TypeId, Arc<dyn Any + Send + Sync>>;

/// Values of the layers above the loop, keyed by type. The loop carries them
/// without reading them; each owner crate reads its own through a typed
/// accessor, which gives its default when the value is absent.
#[derive(Clone, Default)]
pub struct ToolCallExtensions {
    values: Option<Arc<ExtensionMap>>,
}

impl ToolCallExtensions {
    pub fn get<T>(&self) -> Option<&T>
    where
        T: Any + Send + Sync,
    {
        self.values
            .as_ref()?
            .get(&TypeId::of::<T>())
            .and_then(|value| (**value).downcast_ref::<T>())
    }

    /// A copy with `value` set.
    pub fn with<T>(&self, value: T) -> Self
    where
        T: Any + Send + Sync,
    {
        let mut values = self.values.as_deref().cloned().unwrap_or_default();
        values.insert(TypeId::of::<T>(), Arc::new(value));
        Self {
            values: Some(Arc::new(values)),
        }
    }
}
