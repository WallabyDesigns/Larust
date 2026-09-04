//! End-to-end proof that `lifecycle::listener` (`crates/larust-core/src/
//! lifecycle/listener/`) genuinely shares ONE kernel socket between two
//! real, separate OS processes - not something that could be faked with
//! two independent binds to the same port. This test plays the "parent"
//! role directly (using `larust_core::__internal::listener`, the same
//! functions a later stage wires into `Application::serve()`'s own
//! restart-handoff orchestration); only the "child" role needs to be a
//! genuinely separate process (`listener_handoff_fixture.rs`, this
//! crate's own bin target, resolved via `CARGO_BIN_EXE_...`).
//!
//! Both this process's own background `accept()` call *and* the spawned
//! child's `accept()` call are proven to succeed against the exact same
//! originally-bound listener - the thing a naive "two independent binds
//! to the same port" implementation could never do at all (the second
//! bind would just fail outright).

use larust_core::__internal::listener;
use std::io::{BufRead, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

fn reserve_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

fn spawn_fixture(port: u16) -> Child {
    let exe = env!("CARGO_BIN_EXE_listener_handoff_fixture");
    Command::new(exe)
        .env("LISTENER_HANDOFF_PORT", port.to_string())
        .env(listener::INHERIT_LISTENER_ENV, "1")
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn listener_handoff_fixture")
}

fn wait_for_ready_line(child: &mut Child) {
    let stdout = child.stdout.take().expect("child stdout should be piped");
    let mut reader = std::io::BufReader::new(stdout);
    let start = std::time::Instant::now();
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).unwrap_or(0);
        if n > 0 && line.trim() == "READY" {
            return;
        }
        if n == 0 {
            let mut stderr_output = String::new();
            if let Some(mut stderr) = child.stderr.take() {
                let _ = stderr.read_to_string(&mut stderr_output);
            }
            panic!(
                "child process exited before reporting READY (start.elapsed={:?}); stderr:\n{stderr_output}",
                start.elapsed()
            );
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "child process never reported READY"
        );
    }
}

#[test]
fn a_spawned_child_process_accepts_real_connections_on_the_parents_own_listener() {
    let port = reserve_port();
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();

    // This test process plays the "parent" role directly.
    let parent_listener = listener::bind(addr).expect("parent bind failed");

    // Proves the parent side of the shared socket is *also* genuinely
    // live, not just a handle that happens not to have been closed -
    // started before the child even exists, so it's already waiting on
    // the listen queue by the time the first real connection arrives.
    let parent_accept = std::thread::spawn({
        let listener = parent_listener.try_clone().expect("try_clone failed");
        move || listener.accept()
    });

    let mut child = spawn_fixture(port);

    let encoded = listener::prepare_for_handoff(&parent_listener, child.id())
        .expect("prepare_for_handoff failed");
    {
        let mut stdin = child.stdin.take().expect("child stdin should be piped");
        writeln!(stdin, "{encoded}").expect("failed to write encoded listener to child stdin");
    }

    wait_for_ready_line(&mut child);

    // Two one-shot acceptors (the parent's thread above, the child's own
    // single `accept()` call) against two connections - which specific
    // connection lands on which acceptor is an OS scheduling decision,
    // not something this test can or should assume (see `docs/
    // ARCHITECTURE.md`'s "Server-pushed updates" reasoning on this exact
    // point, made about the real restart handoff this mechanism serves).
    // Both connections send the same "ping" payload with a short read
    // timeout: the one that lands on the child gets it echoed back
    // (proving *that* connection was served by the separate process);
    // the one that lands on the parent's bare `accept()` (which never
    // writes anything back) just times out - both outcomes are expected,
    // exactly one of each, in either order.
    let mut echoed_count = 0;
    for _ in 0..2 {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect failed");
        stream
            .set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        stream.write_all(b"ping").unwrap();
        let mut response = [0u8; 4];
        if stream.read_exact(&mut response).is_ok() {
            assert_eq!(&response, b"ping");
            echoed_count += 1;
        }
    }
    assert_eq!(
        echoed_count, 1,
        "expected exactly one of the two connections to be served (and echoed) by the child process"
    );

    let parent_result = parent_accept.join().expect("parent accept thread panicked");
    assert!(
        parent_result.is_ok(),
        "the parent's own accept() on the shared listener never succeeded: {parent_result:?}"
    );

    let status = child.wait().expect("failed to wait on child");
    assert!(
        status.success(),
        "listener_handoff_fixture exited non-zero: {status:?}"
    );
}
