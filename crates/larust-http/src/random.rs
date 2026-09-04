use rand::Rng;
use std::fmt::Write;

/// `byte_len` cryptographically-strong random bytes, hex-encoded - for
/// anywhere a short, unpredictable, URL/filename-safe token is needed
/// (a CSRF token, a generated upload filename) but a full `uuid` dependency
/// would be overkill for. `rand` is already a dependency of this crate for
/// exactly this reason.
pub fn random_hex(byte_len: usize) -> String {
    let mut bytes = vec![0u8; byte_len];
    rand::thread_rng().fill(&mut bytes[..]);

    let mut hex = String::with_capacity(byte_len * 2);
    for byte in bytes {
        // `write!` into a `String` never fails.
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}
