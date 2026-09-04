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
    stream.write_all(command.as_bytes())?;
    stream.write_all(b"\n")?;
    let mut response = String::new();
    BufReader::new(stream).read_line(&mut response)?;
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
    writer.write_all(command.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    let mut reader = BufReader::new(reader);
    let mut response = String::new();
    reader.read_line(&mut response).await?;
    Ok(response.trim().to_string())
}
