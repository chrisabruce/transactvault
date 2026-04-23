//! Authentication primitives: password hashing, JWT encode/decode, and the
//! Axum extractor used to gate every authenticated route.

pub mod claims;
pub mod middleware;
pub mod password;

pub use claims::{Claims, CurrentUser, Role};
pub use password::{hash_password, verify_password};

use chrono::{Duration, Utc};
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};

use crate::config::Config;

/// Issue a fresh JWT for the given user record ID.
///
/// The `sub` claim stores only the ID portion of the RecordId (`user:abc` →
/// `abc`); the full RecordId is rebuilt by [`Claims::user_id`] on each request.
pub fn issue_token(config: &Config, user_id_key: &str) -> anyhow::Result<String> {
    let now = Utc::now();
    let expiry = now + Duration::hours(config.jwt_expiry_hours);
    let claims = Claims {
        sub: user_id_key.to_string(),
        iat: now.timestamp() as usize,
        exp: expiry.timestamp() as usize,
    };

    let token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(config.jwt_secret.as_bytes()),
    )?;
    Ok(token)
}

/// Verify a JWT and return its decoded claims.
pub fn decode_token(config: &Config, token: &str) -> anyhow::Result<Claims> {
    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(config.jwt_secret.as_bytes()),
        &Validation::default(),
    )?;
    Ok(data.claims)
}
