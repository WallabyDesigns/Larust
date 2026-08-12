// One `#[tokio::test]` fn per file: `listeners()...publish()` sets a
// process-wide `OnceLock` exactly once (same constraint
// `larust_orm::connect()` has, and the same "one scenario per test binary"
// workaround this session's other crates already use for it) — every
// scenario that needs a *specific* published registry gets registered
// together, in one process, rather than risking a second `.publish()`
// silently losing to the first.

use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct Greeted {
    name: String,
}

#[derive(Clone)]
struct Farewelled {
    name: String,
}

#[derive(Clone)]
struct NeverListenedTo;

#[tokio::test]
async fn multiple_listeners_run_in_order_and_event_types_dont_cross_fire() {
    let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    let calls_a = Arc::clone(&calls);
    let calls_b = Arc::clone(&calls);
    let calls_farewell = Arc::clone(&calls);

    larust_events::listeners()
        .on::<Greeted, _, _>(move |event: Greeted| {
            let calls = Arc::clone(&calls_a);
            async move {
                calls
                    .lock()
                    .unwrap()
                    .push(format!("first-hello-{}", event.name));
            }
        })
        .on::<Greeted, _, _>(move |event: Greeted| {
            let calls = Arc::clone(&calls_b);
            async move {
                calls
                    .lock()
                    .unwrap()
                    .push(format!("second-hello-{}", event.name));
            }
        })
        .on::<Farewelled, _, _>(move |event: Farewelled| {
            let calls = Arc::clone(&calls_farewell);
            async move {
                calls.lock().unwrap().push(format!("bye-{}", event.name));
            }
        })
        .publish();

    larust_events::dispatch(Greeted {
        name: "Alice".to_string(),
    })
    .await;

    // Dispatching an unrelated event type must not trigger `Greeted`'s
    // listeners.
    larust_events::dispatch(Farewelled {
        name: "Bob".to_string(),
    })
    .await;

    // A type nothing was ever registered for is a safe no-op, not a panic
    // or an error.
    larust_events::dispatch(NeverListenedTo).await;

    let recorded = calls.lock().unwrap().clone();
    assert_eq!(
        recorded,
        vec![
            "first-hello-Alice".to_string(),
            "second-hello-Alice".to_string(),
            "bye-Bob".to_string(),
        ],
        "listeners must run in registration order, only for their own event type"
    );
}
