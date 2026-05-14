//! Argon2id password hashing. Follows the defaults from the `argon2` crate,
//! which map to the OWASP-recommended parameters.
//!
//! Both `hash_password` and `verify_password` are CPU-bound and take tens
//! to hundreds of milliseconds — well over the "never block the executor"
//! threshold. The public async wrappers offload the work onto
//! [`tokio::task::spawn_blocking`] so a single login can't stall every
//! other request on the same runtime thread.

use argon2::Argon2;
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};

/// Hash a plaintext password, returning a PHC-encoded string ready for
/// storage. Runs the Argon2 cost on a blocking-pool thread.
pub async fn hash_password(password: &str) -> anyhow::Result<String> {
    let password = password.to_owned();
    tokio::task::spawn_blocking(move || hash_password_blocking(&password))
        .await
        .map_err(|e| anyhow::anyhow!("password-hash worker join: {e}"))?
}

/// Constant-time password verification against a PHC hash. Runs the
/// Argon2 cost on a blocking-pool thread.
pub async fn verify_password(password: &str, hash: &str) -> anyhow::Result<bool> {
    let password = password.to_owned();
    let hash = hash.to_owned();
    tokio::task::spawn_blocking(move || verify_password_blocking(&password, &hash))
        .await
        .map_err(|e| anyhow::anyhow!("password-verify worker join: {e}"))?
}

fn hash_password_blocking(password: &str) -> anyhow::Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("hashing password: {e}"))?
        .to_string();
    Ok(hash)
}

fn verify_password_blocking(password: &str, hash: &str) -> anyhow::Result<bool> {
    let parsed = PasswordHash::new(hash).map_err(|e| anyhow::anyhow!("parsing hash: {e}"))?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}
