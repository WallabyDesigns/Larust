use crate::component::WireComponent;
use axum::http::StatusCode;
use larust_core::AppError;
use larust_http::session::Session;
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;

type BoxedFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

type MountFn = Box<
    dyn for<'a> Fn(
            &'a Session,
            &'a HashMap<String, Value>,
        ) -> BoxedFuture<'a, Result<Value, AppError>>
        + Send
        + Sync,
>;
type RenderFn =
    Box<dyn for<'a> Fn(&'a Value) -> BoxedFuture<'a, Result<String, AppError>> + Send + Sync>;
type SetManyFn =
    Box<dyn Fn(Value, &HashMap<String, Value>) -> Result<Value, AppError> + Send + Sync>;
/// The `Option<String>` alongside the new state is `call`'s optional
/// redirect path (see `WireComponent::call`'s own doc comment) - carried
/// out of the type-erasure boundary as a plain tuple element rather than a
/// second closure, since it's produced by the same one dispatch.
type CallFn = Box<
    dyn for<'a> Fn(
            Value,
            &'a Session,
            &'a str,
            &'a Value,
        ) -> BoxedFuture<'a, Result<(Value, Option<String>), AppError>>
        + Send
        + Sync,
>;

/// The type-erasure boundary: given only a component *name* string (read
/// back out of session storage) and no compile-time type, this is how
/// `mount()`/`update()` get back to a concrete, statically-typed `C`.
/// State is carried as `serde_json::Value` between calls - `mount`/`call`
/// round-trip it through `C` (via `serde_json::from_value`/`to_value`) so
/// every operation is still fully type-checked against `C`'s own `Serialize`/
/// `Deserialize` impl, just monomorphized once at `register::<C>()` time
/// instead of at every call site. `mount`/`render`/`call` return boxed
/// futures (`WireComponent`'s own methods are `async`, and a `Box<dyn Fn>`
/// can't return `impl Future` directly) - `set_many` stays synchronous, since
/// merging a props object and round-tripping it through `C` purely as a type
/// check needs no async work at all.
pub(crate) struct ComponentEntry {
    pub(crate) mount: MountFn,
    pub(crate) render: RenderFn,
    pub(crate) set_many: SetManyFn,
    pub(crate) call: CallFn,
}

static REGISTRY: OnceLock<HashMap<&'static str, ComponentEntry>> = OnceLock::new();

/// Starts building the process-wide wire-component registry. Call
/// `.register::<C>()` for each component, then `.publish()` once, typically
/// right before `Application::serve()` - same "build via fluent chain, then
/// publish once" shape as `larust_events::listeners()`/`ListenerRegistry`.
pub fn components() -> LiveRegistry {
    LiveRegistry {
        map: HashMap::new(),
    }
}

#[must_use]
pub struct LiveRegistry {
    map: HashMap<&'static str, ComponentEntry>,
}

impl LiveRegistry {
    /// Registers `C` under its own `WireComponent::NAME`. Panics if the same
    /// name is registered twice in one process - a genuine app-author bug
    /// (two components fighting over one `@wire(...)` name), not a
    /// recoverable runtime condition, so this fails loudly at startup rather
    /// than silently letting the second registration shadow the first.
    pub fn register<C: WireComponent>(mut self) -> Self {
        let entry = ComponentEntry {
            mount: Box::new(|session, props| {
                Box::pin(async move {
                    serde_json::to_value(C::mount(session, props).await)
                        .map_err(|source| AppError::Internal(Box::new(source)))
                })
            }),
            render: Box::new(|state| {
                Box::pin(async move {
                    let component = decode::<C>(state.clone())?;
                    Ok(component.render().await.into_html())
                })
            }),
            set_many: Box::new(|state, props| {
                if props.is_empty() {
                    // Nothing to merge - skip the object round-trip
                    // entirely. This matters for a `wire:click`-only
                    // component with no `wire:model` fields at all: such a
                    // `WireComponent` is naturally written as a unit
                    // struct, which `serde_json::to_value` serializes as
                    // `Value::Null`, not `Value::Object` - `as_object`
                    // would otherwise reject every action dispatch on it,
                    // even though there was never anything to merge.
                    return Ok(state);
                }
                let mut merged = as_object(state)?;
                for (key, value) in props {
                    merged.insert(key.clone(), value.clone());
                }
                let merged = Value::Object(merged);
                // Round-tripped through `C` purely as a type check - an
                // update payload with a prop of the wrong shape (a string
                // where `C` expects a number, say) is rejected as a 422
                // here, rather than silently stored malformed.
                let component = decode::<C>(merged)?;
                serde_json::to_value(component)
                    .map_err(|source| AppError::Internal(Box::new(source)))
            }),
            call: Box::new(|state, session, action, args| {
                Box::pin(async move {
                    let mut component = decode::<C>(state)?;
                    let redirect = component.call(session, action, args).await?;
                    let new_state = serde_json::to_value(component)
                        .map_err(|source| AppError::Internal(Box::new(source)))?;
                    Ok((new_state, redirect))
                })
            }),
        };

        let existing = self.map.insert(C::NAME, entry);
        assert!(
            existing.is_none(),
            "duplicate LiveRegistry::register for component NAME {:?} - each wire component \
             must be registered exactly once",
            C::NAME
        );
        self
    }

    /// Publishes this registry process-wide - `mount()`/`update()` read from
    /// it afterward. Same "second call warns, doesn't panic, first writer
    /// wins" shape as `larust_events::ListenerRegistry::publish`.
    pub fn publish(self) {
        if REGISTRY.set(self.map).is_err() {
            tracing::warn!(
                "wire component registry published more than once in this process; \
                 mount()/update() still use the first registry's components"
            );
        }
    }
}

pub(crate) fn lookup(name: &str) -> Option<&'static ComponentEntry> {
    REGISTRY.get().and_then(|map| map.get(name))
}

fn decode<C: WireComponent>(state: Value) -> Result<C, AppError> {
    serde_json::from_value(state).map_err(|source| AppError::Http {
        status: StatusCode::UNPROCESSABLE_ENTITY,
        message: format!("invalid component state: {source}"),
    })
}

/// Rejected as a 422, not an internal error: a client sending non-empty
/// `props` for a component whose state doesn't serialize to a JSON object
/// (a unit-struct component with no `wire:model` fields at all - see
/// `set_many`'s empty-props fast path above, which is what normal usage
/// hits instead) is a client-shaped mismatch, the same category as any
/// other type-mismatched prop, not an internal invariant violation.
fn as_object(state: Value) -> Result<serde_json::Map<String, Value>, AppError> {
    match state {
        Value::Object(map) => Ok(map),
        other => Err(AppError::Http {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            message: format!(
                "cannot apply props to this component's state - expected a JSON object, found: {other}"
            ),
        }),
    }
}
