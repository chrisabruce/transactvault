//! Argon2id password hashing. Follows the defaults from the `argon2` crate,
//! which map to the OWASP-recommended parameters.

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;

/// Hash a plaintext password, returning a PHC-encoded string ready for storage.
pub fn hash_password(password: &str) -> anyhow::Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("hashing password: {e}"))?
        .to_string();
    Ok(hash)
}

/// Constant-time password verification against a PHC hash.
pub fn verify_password(password: &str, hash: &str) -> anyhow::Result<bool> {
    let parsed = PasswordHash::new(hash).map_err(|e| anyhow::anyhow!("parsing hash: {e}"))?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}
