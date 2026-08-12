// Separate test binary specifically so `listeners()...publish()` is never
// called anywhere in this process — proves `dispatch()` is a safe no-op
// (not a panic, not a hang) when no registry was ever published at all,
// not just when a published registry has nothing for this event type.

#[derive(Clone)]
struct Unheard;

#[tokio::test]
async fn dispatch_before_any_publish_is_a_safe_no_op() {
    larust_events::dispatch(Unheard).await;
}
