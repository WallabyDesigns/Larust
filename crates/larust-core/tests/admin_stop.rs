//! End-to-end proof of the admin channel's `STOP` command
//! (`lifecycle::admin::STOP_COMMAND`/`AdminOutcome::Stop`): a real running
//! app, asked to `STOP`, drains and exits gracefully with **no**
//! replacement ever spawned — distinct from `RESTART`, which always hands
//! off to a new process. `STOP` exists specifically for a caller (`xr
//! dev`, once it's handed off past the first generation) that no longer
//! holds a `Child` handle to whatever's currently serving and needs a
//! reliable way to reach "whoever owns this admin-channel address right
//! now" for a clean teardown.

use larust_core::__internal::admin;
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn reserve_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

fn wait_until_listening(addr: &str, timeout: Duration) {
    let start = Instant::now();
    loop {
        if TcpStream::connect(addr).is_ok() {
            return;
        }
        assert!(
            start.elapsed() < timeout,
            "app never started listening on {addr}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn spawn_fixture(app_dir: &std::path::Path, port: u16, app_name: &str) -> ChildGuard {
    let exe = env!("CARGO_BIN_EXE_zero_downtime_fixture");
    let child = Command::new(exe)
        .env("APP_PORT", port.to_string())
        .env("APP_NAME", app_name)
        .current_dir(app_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("failed to spawn zero_downtime_fixture");
    ChildGuard(child)
}

#[cfg(unix)]
fn send_stop_command(address: &str) -> std::io::Result<String> {
    use std::io::{BufRead, Write};
    use std::os::unix::net::UnixStream;

    let path = std::env::temp_dir().join(format!("{address}.sock"));
    let mut stream = UnixStream::connect(&path)?;
    stream.set_read_timeout(Some(Duration::from_secs(20)))?;
    stream.write_all(admin::STOP_COMMAND.as_bytes())?;
    stream.write_all(b"\n")?;
    let mut response = String::new();
    std::io::BufReader::new(stream).read_line(&mut response)?;
    Ok(response.trim().to_string())
}

#[cfg(windows)]
fn send_stop_command(address: &str) -> std::io::Result<String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::windows::named_pipe::ClientOptions;

    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async move {
        let name = format!(r"\\.\pipe\{address}");
        let mut last_error = None;
        let mut client = None;
        for _ in 0..50 {
            match ClientOptions::new().open(&name) {
                Ok(c) => {
                    client = Some(c);
                    break;
                }
                Err(source) => {
                    last_error = Some(source);
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
        let client = client.ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("couldn't connect to {name}: {last_error:?}"),
            )
        })?;
        let (reader, mut writer) = tokio::io::split(client);
        writer.write_all(admin::STOP_COMMAND.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        let mut reader = BufReader::new(reader);
        let mut response = String::new();
        reader.read_line(&mut response).await?;
        Ok(response.trim().to_string())
    })
}

#[test]
fn a_stop_command_drains_and_exits_with_no_replacement_spawned() {
    let port = reserve_port();
    let app_name = format!("admin_stop_test_{port}");
    let address = admin::channel_address(&app_name);
    let addr = format!("127.0.0.1:{port}");

    let app_dir = tempfile::tempdir().unwrap();

    let mut child = spawn_fixture(app_dir.path(), port, &app_name);
    wait_until_listening(&addr, Duration::from_secs(10));

    let response = send_stop_command(&address).expect("failed to send the stop command");
    assert_eq!(
        response,
        admin::ACK_HANDOFF_STARTED,
        "the app should have acknowledged the stop command"
    );

    let status = child
        .0
        .wait()
        .expect("failed to wait on the fixture process");
    assert!(
        status.success(),
        "process should have exited cleanly after a stop command, got {status:?}"
    );

    // No replacement was ever spawned -- the port must now be
    // unreachable, not served by some other process.
    let start = Instant::now();
    loop {
        if TcpStream::connect(&addr).is_err() {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "something is still listening on {addr} after a stop command -- \
             a replacement must have been spawned, which STOP should never do"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}
