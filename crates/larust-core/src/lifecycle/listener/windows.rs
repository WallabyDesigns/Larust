use std::io;
use std::net::TcpListener;
use std::os::windows::io::{AsRawSocket, FromRawSocket};
use windows_sys::Win32::Networking::WinSock::{
    WSADuplicateSocketW, WSAGetLastError, WSASocketW, WSAStartup, AF_INET, INVALID_SOCKET,
    IPPROTO_TCP, SOCK_STREAM, WSADATA, WSAPROTOCOL_INFOW, WSA_FLAG_OVERLAPPED,
};

/// `WSASocketW` (used by `inherit` below) fails with "the application has
/// not called WSAStartup" unless *something* in this process already has —
/// std's own sockets trigger it lazily on first use, but a freshly spawned
/// child that inherits a listener and never otherwise touches `std::net`
/// first has nothing to trigger that lazily. Confirmed by hitting this
/// exact failure empirically while building this module (not something
/// caught by the earlier, simpler `WSADuplicateSocketW`/`WSASocketW`
/// spike, which happened to call `std::net::TcpListener::bind` — and thus
/// trigger std's own lazy `WSAStartup` — before ever reaching the raw
/// calls). Safe to call more than once per process; each call just
/// increments an internal reference count.
fn ensure_wsa_started() {
    unsafe {
        let mut data: WSADATA = std::mem::zeroed();
        // 0x0202 == Winsock version 2.2, the same version `WSADuplicateSocketW`
        // and `WSASocketW` themselves require to be available.
        let ret = WSAStartup(0x0202, &mut data);
        assert_eq!(ret, 0, "WSAStartup failed with code {ret}");
    }
}

/// Duplicates `listener`'s underlying socket into `child_pid`'s own handle
/// table via `WSADuplicateSocketW` — the Winsock-sanctioned mechanism for
/// handing a live socket to a specific other process (used by real
/// production software, e.g. IIS; confirmed working here via a throwaway
/// spike before this module was written, since Windows has no direct
/// analogue of Unix's simple fd-inheritance-across-`fork` and getting it
/// wrong silently — this was verified empirically, not assumed from
/// documentation alone). Unlike the Unix implementation, this genuinely
/// needs the child's real PID, which only exists after `Command::spawn()`
/// returns — see `lifecycle::listener`'s module doc comment for why the
/// encoded result travels over the child's stdin rather than an env var
/// set before spawn.
pub(super) fn prepare_for_handoff(listener: &TcpListener, child_pid: u32) -> io::Result<String> {
    ensure_wsa_started();
    let raw_socket = listener.as_raw_socket() as usize;
    let mut info: WSAPROTOCOL_INFOW = unsafe { std::mem::zeroed() };
    let ret = unsafe { WSADuplicateSocketW(raw_socket, child_pid, &mut info) };
    if ret != 0 {
        return Err(io::Error::from_raw_os_error(
            unsafe { WSAGetLastError() } as i32
        ));
    }
    // SAFETY: `info` is a plain-old-data struct (no pointers/ownership of
    // its own beyond the raw bytes) — reading it as a byte slice for
    // encoding is exactly how `WSADuplicateSocketW` itself expects it to
    // be transmitted to the target process (as opaque bytes, reconstructed
    // via `WSASocketW` on the other side).
    let bytes: &[u8] = unsafe {
        std::slice::from_raw_parts(
            &info as *const _ as *const u8,
            std::mem::size_of::<WSAPROTOCOL_INFOW>(),
        )
    };
    Ok(encode_hex(bytes))
}

/// Reconstructs a `TcpListener` from the hex-encoded `WSAPROTOCOL_INFOW` a
/// parent process wrote to this process's own stdin (see
/// `prepare_for_handoff`).
pub(super) fn inherit(encoded: &str) -> io::Result<TcpListener> {
    ensure_wsa_started();
    let bytes = decode_hex(encoded.trim()).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "invalid inherited socket info")
    })?;
    if bytes.len() != std::mem::size_of::<WSAPROTOCOL_INFOW>() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid inherited socket info length",
        ));
    }
    // SAFETY: `bytes` round-trips the exact `WSAPROTOCOL_INFOW` bytes
    // `prepare_for_handoff` produced in the parent process — read
    // unaligned since the byte buffer decoded from hex text has no
    // particular alignment guarantee.
    let info: WSAPROTOCOL_INFOW =
        unsafe { std::ptr::read_unaligned(bytes.as_ptr() as *const WSAPROTOCOL_INFOW) };
    let socket = unsafe {
        WSASocketW(
            AF_INET as i32,
            SOCK_STREAM,
            IPPROTO_TCP,
            &info,
            0,
            WSA_FLAG_OVERLAPPED,
        )
    };
    if socket == INVALID_SOCKET {
        return Err(io::Error::from_raw_os_error(
            unsafe { WSAGetLastError() } as i32
        ));
    }
    // SAFETY: `WSASocketW` just returned this as a freshly-constructed,
    // valid `SOCKET` handle for the duplicated listener.
    //
    // Returned in ordinary blocking mode — see the matching comment in
    // `unix::inherit` for why setting non-blocking mode isn't this
    // function's job.
    Ok(unsafe { TcpListener::from_raw_socket(socket as u64) })
}

fn encode_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}
