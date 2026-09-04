//! Simulates the top-level `xr dev` process for `tests/supervisor.rs`'s
//! "an OS-level hard kill of the parent still kills the child" test:
//! binds its own listener, spawns `zero_downtime_fixture` (its path given
//! as argv[1]) as a real handoff replacement - exercising
//! `lifecycle::supervisor::prepare`/`register` exactly the way production
//! does, via the real `handoff::spawn_replacement_and_wait_for_ready` -
//! prints the bound port and the replacement's pid once it's confirmed
//! ready, then blocks forever. The test hard-kills *this* process while
//! the replacement keeps running, then checks whether the OS itself also
//! took the replacement down.

use larust_core::__internal::{handoff, listener};
use std::net::SocketAddr;
use std::time::Duration;

#[tokio::main]
async fn main() {
    let replacement_path = std::env::args()
        .nth(1)
        .expect("usage: supervisor_parent_fixture <replacement-binary-path>");

    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let parent_listener = listener::bind(addr).expect("bind failed");
    let port = parent_listener.local_addr().unwrap().port();

    // `true`: this fixture simulates `xr dev` itself spawning generation 1
    // directly - see `spawn_replacement_and_wait_for_ready`'s own doc
    // comment for why that hop (unlike a later server-to-server one) needs
    // explicit registration.
    let outcome = handoff::spawn_replacement_and_wait_for_ready(
        &parent_listener,
        replacement_path.as_ref(),
        Duration::from_secs(10),
        true,
    )
    .await
    .expect("spawn_replacement_and_wait_for_ready returned an error");

    let child = outcome.expect("replacement should have become ready");
    let child_pid = child.id().expect("child should have a pid");

    // Printed to stderr, not stdout - the replacement's own stdout is
    // `Stdio::inherit()`ed all the way through to *this* process's own
    // stdout (see `handoff::spawn_replacement_and_wait_for_ready`'s doc
    // comment for why), so it and this line would otherwise land in the
    // same stream in a non-deterministic order. Stderr is untouched: the
    // replacement's stderr is fully captured *inside* the handoff call
    // above for the readiness handshake, never inherited out to here.
    // One line, both values together, so the test can't observe a partial
    // read between two separate prints.
    eprintln!("READY port={port} child_pid={child_pid}");
    use std::io::Write;
    std::io::stderr().flush().unwrap();

    // `child` stays alive in scope for the rest of this function (which
    // never returns) rather than being dropped or explicitly waited on -
    // this process is about to be hard-killed with no chance to run any
    // cleanup code at all, exactly simulating a crashed or force-killed
    // `xr dev`; whatever `lifecycle::supervisor::register` already did at
    // spawn time above is the only thing that can save the replacement
    // from becoming an orphan now.
    let _child = child;
    std::future::pending::<()>().await;
}
