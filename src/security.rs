//! Security primitives shared across controllers:
//!
//! - **Proof-of-work** challenge generator + verifier (HMAC-bound to the
//!   server's JWT secret so attackers can't fake or replay challenges).
//! - **In-memory IP rate limiter** with token-bucket semantics.
//! - **Disposable-email blacklist** so we don't keep handing throwaway
//!   inboxes a free welcome.
//! - **Client IP extractor** that walks the standard reverse-proxy headers.
//!
//! Everything in this module is intentionally side-effect free apart from
//! the rate-limiter mutations, so it's easy to test in isolation.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::http::HeaderMap;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

// ---------------------------------------------------------------------------
// Proof of Work
// ---------------------------------------------------------------------------

/// One challenge issued to a single signup attempt.
///
/// Wire format: `b64(timestamp:8 || nonce:16 || hmac:32)`. `timestamp` is the
/// Unix epoch seconds at issuance. `nonce` is 16 random bytes that make each
/// challenge unique. `hmac` is keyed with the server's JWT secret so we can
/// verify the challenge wasn't crafted client-side.
pub struct PowChallenge {
    pub challenge: String,
    pub difficulty: u32,
}

/// Maximum age of a PoW challenge before we reject it. Five minutes is more
/// than long enough for a slow phone to solve a difficulty-18 puzzle and
/// short enough to limit replay.
const POW_MAX_AGE_SECS: u64 = 300;

/// Generate a fresh, signed PoW challenge.
pub fn issue_challenge(jwt_secret: &str, difficulty: u32) -> PowChallenge {
    use rand::RngCore;

    let now = chrono::Utc::now().timestamp() as u64;
    let mut nonce = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut nonce);

    let mut payload = Vec::with_capacity(8 + 16);
    payload.extend_from_slice(&now.to_be_bytes());
    payload.extend_from_slice(&nonce);

    let mut mac =
        HmacSha256::new_from_slice(jwt_secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(&payload);
    let tag = mac.finalize().into_bytes();

    let mut full = payload;
    full.extend_from_slice(&tag);

    PowChallenge {
        challenge: hex::encode(&full),
        difficulty,
    }
}

/// Verify a (challenge, nonce) pair the client submitted.
///
/// Steps:
/// 1. Parse the challenge: timestamp, nonce, HMAC.
/// 2. Recompute the HMAC with the server secret — must match.
/// 3. Reject if older than [`POW_MAX_AGE_SECS`].
/// 4. Compute `sha256(challenge || ":" || solution)` and require at least
///    `difficulty` leading zero bits.
pub fn verify_challenge(
    jwt_secret: &str,
    challenge_hex: &str,
    solution: &str,
    difficulty: u32,
) -> bool {
    if difficulty == 0 {
        return true; // disabled in tests
    }

    let bytes = match hex::decode(challenge_hex) {
        Ok(b) if b.len() == 8 + 16 + 32 => b,
        _ => return false,
    };
    let (payload, tag) = bytes.split_at(24);

    let mut mac = match HmacSha256::new_from_slice(jwt_secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(payload);
    if mac.verify_slice(tag).is_err() {
        return false;
    }

    let issued = u64::from_be_bytes(payload[..8].try_into().unwrap_or([0; 8]));
    let now = chrono::Utc::now().timestamp() as u64;
    if now.saturating_sub(issued) > POW_MAX_AGE_SECS {
        return false;
    }

    let mut hasher = Sha256::new();
    hasher.update(challenge_hex.as_bytes());
    hasher.update(b":");
    hasher.update(solution.as_bytes());
    let digest = hasher.finalize();

    leading_zero_bits(&digest) >= difficulty
}

fn leading_zero_bits(digest: &[u8]) -> u32 {
    let mut count = 0u32;
    for byte in digest {
        if *byte == 0 {
            count += 8;
        } else {
            count += byte.leading_zeros();
            break;
        }
    }
    count
}

// ---------------------------------------------------------------------------
// IP rate limiter (token bucket, in-memory, sweep-on-access)
// ---------------------------------------------------------------------------

/// One bucket per (route, IP) key. Each request costs one token; tokens
/// regenerate at `tokens_per_window / window` per second.
#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last_refill: Instant,
}

#[derive(Clone)]
pub struct RateLimiter {
    inner: Arc<Mutex<HashMap<String, Bucket>>>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Try to spend one token for `key`. Returns `true` if allowed.
    ///
    /// `capacity` is the burst size; `refill_per_sec` is the sustained rate.
    /// The very first call from a key starts the bucket full so a single
    /// request never gets denied.
    pub fn allow(&self, key: &str, capacity: f64, refill_per_sec: f64) -> bool {
        let now = Instant::now();
        let mut map = self.inner.lock().expect("rate-limit mutex poisoned");

        // Cheap bookkeeping: if the map gets large, drop entries we haven't
        // touched in an hour. Keeps memory bounded under sustained traffic.
        if map.len() > 1024 {
            map.retain(|_, b| {
                now.saturating_duration_since(b.last_refill) < Duration::from_secs(3600)
            });
        }

        let bucket = map.entry(key.to_string()).or_insert(Bucket {
            tokens: capacity,
            last_refill: now,
        });

        let elapsed = now
            .saturating_duration_since(bucket.last_refill)
            .as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * refill_per_sec).min(capacity);
        bucket.last_refill = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

/// Convenience: bucket sized for `n` requests per hour.
pub fn allow_per_hour(rl: &RateLimiter, key: &str, n: u32) -> bool {
    rl.allow(key, n as f64, n as f64 / 3600.0)
}

/// Convenience: bucket sized for `n` requests per quarter-hour.
pub fn allow_per_quarter_hour(rl: &RateLimiter, key: &str, n: u32) -> bool {
    rl.allow(key, n as f64, n as f64 / 900.0)
}

// ---------------------------------------------------------------------------
// Client-IP extraction
// ---------------------------------------------------------------------------

/// Extract the best client-IP we can given the typical reverse-proxy chain
/// (Traefik / Nginx / Cloudflare). Order: `cf-connecting-ip`, the leftmost
/// entry of `x-forwarded-for`, `x-real-ip`, then the TCP peer.
pub fn client_ip(headers: &HeaderMap, peer: Option<&SocketAddr>) -> String {
    if let Some(v) = headers
        .get("cf-connecting-ip")
        .and_then(|v| v.to_str().ok())
        && !v.is_empty()
    {
        return v.trim().to_string();
    }
    if let Some(v) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok())
        && let Some(first) = v.split(',').next()
    {
        let s = first.trim();
        if !s.is_empty() {
            return s.to_string();
        }
    }
    if let Some(v) = headers.get("x-real-ip").and_then(|v| v.to_str().ok())
        && !v.is_empty()
    {
        return v.trim().to_string();
    }
    peer.map(|p| p.ip().to_string())
        .unwrap_or_else(|| "unknown".into())
}

pub fn user_agent(headers: &HeaderMap) -> Option<String> {
    headers
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

// ---------------------------------------------------------------------------
// Disposable-email blacklist
// ---------------------------------------------------------------------------

/// Tiny curated list of well-known throwaway-email providers. Not a complete
/// adversarial list — that's a service in itself — but blocks the obvious
/// abuse vectors. Use lower-case domain names.
const DISPOSABLE_DOMAINS: &[&str] = &[
    "10minutemail.com",
    "10minutemail.net",
    "20minutemail.com",
    "anonymouse.org",
    "boun.cr",
    "deadaddress.com",
    "discardmail.com",
    "dispostable.com",
    "fakeinbox.com",
    "getnada.com",
    "guerrillamail.com",
    "guerrillamail.de",
    "guerrillamail.net",
    "guerrillamailblock.com",
    "harakirimail.com",
    "incognitomail.com",
    "inbox.lv",
    "jetable.org",
    "mailcatch.com",
    "maildrop.cc",
    "mailinator.com",
    "mailinator.net",
    "mailnesia.com",
    "mintemail.com",
    "mvrht.com",
    "mytrashmail.com",
    "spamavert.com",
    "spamgourmet.com",
    "sharklasers.com",
    "tempinbox.com",
    "tempmail.com",
    "temp-mail.org",
    "thrott.com",
    "throwawaymail.com",
    "trashmail.com",
    "trashmail.net",
    "yopmail.com",
    "zetmail.com",
];

/// Returns `true` if the email's domain is in the disposable-mail blacklist.
pub fn is_disposable_email(email: &str) -> bool {
    let domain = match email.rsplit('@').next() {
        Some(d) => d.to_ascii_lowercase(),
        None => return false,
    };
    DISPOSABLE_DOMAINS.iter().any(|&d| d == domain)
}
