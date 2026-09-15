//! Shared client-side logic for talking to a running app's admin channel
//! (see `larust_core::__internal::admin` for the server side): connect,
//! send one command line, read the one-line response. Both `xr restart`
//! and `xr dev`'s own build loop need exactly this, differing only in
//! which command string they send (`RESTART` vs `STOP`) - factored out
//! once here rather than duplicated, including the trickiest part on
//! Windows: a named-pipe client can race the server's own pipe-instance
//! recreation between connections and needs to retry `ERROR_PIPE_BUSY`
//! rather than fail on the first attempt (see `docs/GOTCHAS.md`).

use anyhow::Context;
use std::time::Duration;

/// Bounds the whole write-command/read-response round trip on both
/// platforms - a *connected* admin channel with nothing actually
/// answering (a stuck/deadlocked process on the other end, as opposed to
/// no process there at all, which fails fast on connect instead) used to
/// hang here indefinitely, with no timeout at all. Every caller of
/// `send_command` runs this *before* printing anything of its own (`xr
/// dev`'s `stop_any_previous_generation` is the very first thing `run()`
/// does), so this exact hang surfaced as "`xr dev` silently failed, no
/// output at all" - and closing/reopening the terminal never helped,
/// since the stuck process from the earlier session was still sitting
/// there for the next attempt to hang on identically. Generous relative
/// to how fast a healthy app actually responds (near-instant - it's one
/// line over an already-open local pipe/socket), while still bounded.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);

#[cfg(unix)]
pub(crate) fn send_command(address: &str, command: &str) -> anyhow::Result<String> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    let path = std::env::temp_dir().join(format!("{address}.sock"));
    let mut stream = UnixStream::connect(&path).with_context(|| {
        format!(
            "couldn't connect to the admin channel at {path:?} -- is the app running with \
             `GracefulShutdown {{ restart_channel: true, .. }}`?"
        )
    })?;
    stream
        .set_write_timeout(Some(RESPONSE_TIMEOUT))
        .context("failed to set a write timeout on the admin channel connection")?;
    stream
        .set_read_timeout(Some(RESPONSE_TIMEOUT))
        .context("failed to set a read timeout on the admin channel connection")?;
    stream
        .write_all(command.as_bytes())
        .with_context(|| format!("timed out or failed writing to the admin channel at {path:?}"))?;
    stream
        .write_all(b"\n")
        .with_context(|| format!("timed out or failed writing to the admin channel at {path:?}"))?;
    let mut response = String::new();
    BufReader::new(stream)
        .read_line(&mut response)
        .with_context(|| {
            format!(
                "timed out waiting for a response from the admin channel at {path:?} - the \
                 process on the other end may be stuck"
            )
        })?;
    Ok(response.trim().to_string())
}

#[cfg(windows)]
pub(crate) fn send_command(address: &str, command: &str) -> anyhow::Result<String> {
    let runtime = tokio::runtime::Runtime::new().context("failed to start async runtime")?;
    runtime.block_on(send_command_async(address, command))
}

#[cfg(windows)]
async fn send_command_async(address: &str, command: &str) -> anyhow::Result<String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::windows::named_pipe::ClientOptions;

    let name = format!(r"\\.\pipe\{address}");

    // A named pipe client can hit `ERROR_PIPE_BUSY` if it connects in the
    // narrow window between one client disconnecting and the app's admin
    // loop creating the next pipe instance - retried briefly rather than
    // failing on the first attempt, the standard pattern for this API.
    let mut last_error = None;
    let mut client = None;
    for _ in 0..20 {
        match ClientOptions::new().open(&name) {
            Ok(c) => {
                client = Some(c);
                break;
            }
            Err(source) => {
                last_error = Some(source);
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
    let client = client.ok_or_else(|| {
        anyhow::anyhow!(
            "couldn't connect to the admin channel at {name} after retrying -- is the app \
             running with `GracefulShutdown {{ restart_channel: true, .. }}`? last error: \
             {last_error:?}"
        )
    })?;

    let (reader, mut writer) = tokio::io::split(client);
    let mut reader = BufReader::new(reader);
    let mut response = String::new();
    tokio::time::timeout(RESPONSE_TIMEOUT, async {
        writer.write_all(command.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        reader.read_line(&mut response).await?;
        Ok::<(), std::io::Error>(())
    })
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "timed out waiting for a response from the admin channel at {name} - the process \
             on the other end may be stuck"
        )
    })??;
    Ok(response.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real end-to-end proof of the exact scenario `RESPONSE_TIMEOUT`'s
    /// own doc comment describes: a channel that *connects* successfully
    /// but has nothing actually reading/responding on the other end (a
    /// stuck process, not an absent one - which fails fast on connect,
    /// already covered by the "couldn't connect" error path). Before this
    /// fix, `send_command` against a fake server like this one would
    /// simply never return.
    #[cfg(windows)]
    #[test]
    fn a_connected_but_unresponsive_admin_channel_times_out_instead_of_hanging_forever() {
        use std::sync::{Arc, Barrier};
        use std::time::{Duration, Instant};
        use tokio::net::windows::named_pipe::ServerOptions;

        let address = format!("larust-test-stuck-{}", std::process::id());
        let name = format!(r"\\.\pipe\{address}");

        let ready = Arc::new(Barrier::new(2));
        let ready_clone = Arc::clone(&ready);
        let server_name = name.clone();
        // A separate OS thread with its own runtime - `send_command`
        // (below, on the test's own thread) spins up its own runtime via
        // `block_on`, and nesting one tokio runtime inside another panics.
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async move {
                let server = ServerOptions::new().create(&server_name).unwrap();
                ready_clone.wait();
                server.connect().await.unwrap();
                // Deliberately never reads or writes anything - this is
                // the "stuck/deadlocked process" `send_command` must not
                // hang on forever.
                tokio::time::sleep(Duration::from_secs(30)).await;
            });
        });
        ready.wait();

        let start = Instant::now();
        let result = send_command(&address, "STOP");
        let elapsed = start.elapsed();

        let error = result.expect_err("an unresponsive channel should time out, not succeed");
        assert!(
            error.to_string().contains("timed out"),
            "expected a timeout error, got: {error}"
        );
        assert!(
            elapsed < Duration::from_secs(8),
            "should time out around RESPONSE_TIMEOUT (5s), took {elapsed:?} instead"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_connected_but_unresponsive_admin_channel_times_out_instead_of_hanging_forever() {
        use std::os::unix::net::UnixListener;
        use std::time::{Duration, Instant};

        let address = format!("larust-test-stuck-{}", std::process::id());
        let path = std::env::temp_dir().join(format!("{address}.sock"));
        let listener = UnixListener::bind(&path).unwrap();
        std::thread::spawn(move || {
            // Accept and hold the connection open, but deliberately never
            // read or write anything - the "stuck process" scenario.
            let (_stream, _) = listener.accept().unwrap();
            std::thread::sleep(Duration::from_secs(30));
        });

        let start = Instant::now();
        let result = send_command(&address, "STOP");
        let elapsed = start.elapsed();

        let error = result.expect_err("an unresponsive channel should time out, not succeed");
        assert!(
            error.to_string().contains("timed out"),
            "expected a timeout error, got: {error}"
        );
        assert!(
            elapsed < Duration::from_secs(8),
            "should time out around RESPONSE_TIMEOUT (5s), took {elapsed:?} instead"
        );
    }
}
