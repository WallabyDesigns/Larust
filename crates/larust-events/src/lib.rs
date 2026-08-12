//! A lightweight, explicit, in-process pub/sub — Laravel's `Event::dispatch`/
//! listeners, deliberately scaled down. No persistence, no queue
//! involvement, no derive macro, no DI-container resolution: listeners are
//! registered explicitly (`listeners().on::<E>(...)`, same "build a
//! registry, then publish it" shape as
//! `larust_http::route`'s named-route registry), and every listener for an
//! event runs synchronously, in registration order, in-process — the same
//! default Laravel itself uses (only a `ShouldQueue` listener defers
//! there). A listener that needs to defer real work should enqueue a
//! `larust_queue::Job` instead of doing it inline.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;

/// Any plain `Clone + Send + Sync + 'static` value can be an event — a
/// blanket impl, no derive, no required methods. `Clone` is required
/// because each of possibly-several listeners for one dispatch gets its
/// own owned copy to move into its independently-`'static` future.
pub trait Event: Clone + Send + Sync + 'static {}
impl<T: Clone + Send + Sync + 'static> Event for T {}

type BoxedFuture = Pin<Box<dyn Future<Output = ()> + Send>>;
type BoxedListener = Box<dyn Fn(Box<dyn Any + Send + Sync>) -> BoxedFuture + Send + Sync>;

static LISTENERS: OnceLock<HashMap<TypeId, Vec<BoxedListener>>> = OnceLock::new();

/// Starts building the process-wide listener registry. Call `.on::<E>(...)`
/// for each listener, then `.publish()` once, typically right before
/// `Application::serve()`.
pub fn listeners() -> ListenerRegistry {
    ListenerRegistry {
        map: HashMap::new(),
    }
}

#[must_use]
pub struct ListenerRegistry {
    map: HashMap<TypeId, Vec<BoxedListener>>,
}

impl ListenerRegistry {
    /// Registers `listener` to run whenever an `E` is dispatched. Multiple
    /// listeners for the same `E` (including across separate `.on::<E>()`
    /// calls) all run, in the order they were registered.
    pub fn on<E, F, Fut>(mut self, listener: F) -> Self
    where
        E: Event,
        F: Fn(E) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let boxed: BoxedListener = Box::new(move |event: Box<dyn Any + Send + Sync>| {
            let event = *event
                .downcast::<E>()
                .expect("dispatch() only ever boxes an E under its own TypeId key");
            Box::pin(listener(event))
        });
        self.map.entry(TypeId::of::<E>()).or_default().push(boxed);
        self
    }

    /// Publishes this registry process-wide — `dispatch()` reads from it
    /// afterward. Same "second call warns, doesn't panic, first writer
    /// wins" shape as `larust_http::route::publish_route_names`: a second
    /// `.publish()` in the same process (e.g. `Application::new()`-style
    /// re-entry in tests) doesn't overwrite the first registry, and every
    /// `dispatch()` afterward keeps using it.
    pub fn publish(self) {
        if LISTENERS.set(self.map).is_err() {
            tracing::warn!(
                "event listener registry published more than once in this process; \
                 dispatch() still uses the first registry's listeners"
            );
        }
    }
}

/// Runs every listener registered for `E`, sequentially, awaiting each in
/// turn before moving to the next. A silent no-op if `listeners()...
/// publish()` was never called, or if nothing is registered for this
/// specific `E` — there's no failure mode here worth an `AppError`; a
/// listener that can fail should log its own error internally rather than
/// short-circuit the others.
pub async fn dispatch<E: Event>(event: E) {
    let Some(map) = LISTENERS.get() else {
        return;
    };
    let Some(list) = map.get(&TypeId::of::<E>()) else {
        return;
    };
    for listener in list {
        listener(Box::new(event.clone())).await;
    }
}
