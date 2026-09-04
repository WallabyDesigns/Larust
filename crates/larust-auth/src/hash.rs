//! Password hashing via `argon2` (RustCrypto's actively-maintained crate,
//! Argon2id by default - the current recommended choice for password
//! storage). A fresh random salt is generated per call, which is what
//! makes two hashes of the same password different and resistant to
//! rainbow-table/precomputation attacks; verification is handled entirely
//! by the crate's own constant-time comparison, not hand-rolled here.

use argon2::password_hash::{rand_core::OsRng, SaltString};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use larust_core::AppError;

/// Hashes a plaintext password for storage (Laravel's `Hash::make()`).
pub fn hash_password(password: &str) -> Result<String, AppError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(internal_error)
}

/// Checks a plaintext password against a stored hash (Laravel's
/// `Hash::check()`). Returns `Ok(false)` for a wrong password - only a
/// malformed/corrupt stored hash is an `Err`, since that's the only case
/// that isn't a normal, expected verification outcome.
pub fn verify_password(hash: &str, password: &str) -> Result<bool, AppError> {
    let parsed_hash = PasswordHash::new(hash).map_err(internal_error)?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed_hash)
        .is_ok())
}

/// `argon2::password_hash::Error` doesn't implement `std::error::Error`
/// (it's a minimal `no_std`-friendly error type), so it can't go directly
/// into `AppError::Internal`'s `Box<dyn Error + Send + Sync>` - wrap its
/// `Display` output in a real `Error` impl instead (matching the pattern
/// `larust_support::redirect::route` already uses for a `Display`-only
/// error).
fn internal_error(source: argon2::password_hash::Error) -> AppError {
    AppError::Internal(Box::new(std::io::Error::other(source.to_string())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_and_verify_round_trip() {
        let hash = hash_password("correct horse battery staple").unwrap();
        assert!(verify_password(&hash, "correct horse battery staple").unwrap());
    }

    #[test]
    fn verify_rejects_wrong_password() {
        let hash = hash_password("correct horse battery staple").unwrap();
        assert!(!verify_password(&hash, "wrong password").unwrap());
    }

    #[test]
    fn hashing_the_same_password_twice_produces_different_hashes() {
        // Each call generates a fresh random salt - identical stored hashes
        // for the same password would leak which users share a password.
        let a = hash_password("correct horse battery staple").unwrap();
        let b = hash_password("correct horse battery staple").unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn verify_rejects_malformed_hash() {
        assert!(verify_password("not a real hash", "anything").is_err());
    }
}
