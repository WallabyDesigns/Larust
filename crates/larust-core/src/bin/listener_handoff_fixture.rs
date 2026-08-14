//! Standalone fixture for `tests/listener_handoff.rs` — proves the
//! cross-platform listener-passing mechanism in `lifecycle::listener`
//! actually works between two real, separate OS processes, independent of
//! the readiness protocol / admin channel later stages of this same
//! feature layer on top. Uses `larust_core::__internal` (see its own doc
//! comment) since a `src/bin/*.rs` binary is a separate crate from this
//! package's library, even though they share a package — it can only
//! reach `pub` items, the same as any other external consumer.

use larust_core::__internal::listener;
use std::io::BufRead;
use std::net::SocketAddr;

fn main() {
    let port: u16 = std::env::var("LISTENER_HANDOFF_PORT")
        .expect("LISTENER_HANDOFF_PORT must be set")
        .parse()
        .expect("LISTENER_HANDOFF_PORT must be a valid port number");
    let addr = SocketAddr::from(([127, 0, 0, 1], port));

    let tcp_listener = if std::env::var_os(listener::INHERIT_LISTENER_ENV).is_some() {
        let mut line = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut line)
            .expect("failed to read inherited listener encoding from stdin");
        println!("[fixture] inheriting listener from parent");
        listener::inherit(&line).expect("failed to reconstruct inherited listener")
    } else {
        println!("[fixture] binding fresh listener");
        listener::bind(addr).expect("failed to bind")
    };

    println!("READY");

    // Accept exactly one connection, echo one line back, then exit —
    // enough for the test to prove *this* process is the one that served
    // the request, distinct from whichever process actually bound the
    // socket originally.
    use std::io::{Read, Write};
    let (mut stream, _) = tcp_listener.accept().expect("accept failed");
    let mut buf = [0u8; 64];
    let n = stream.read(&mut buf).unwrap_or(0);
    let _ = stream.write_all(&buf[..n]);
}
