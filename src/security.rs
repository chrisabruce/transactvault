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
///
/// Returns `Err` only if the HMAC backend rejects the secret, which the
/// variable-key `Hmac<Sha256>` construction never does in practice —
/// this signature exists so the call site stays panic-free even under
/// future crate-level changes.
pub fn issue_challenge(jwt_secret: &str, difficulty: u32) -> anyhow::Result<PowChallenge> {
    use rand::RngCore;

    let now = chrono::Utc::now().timestamp() as u64;
    let mut nonce = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut nonce);

    let mut payload = Vec::with_capacity(8 + 16);
    payload.extend_from_slice(&now.to_be_bytes());
    payload.extend_from_slice(&nonce);

    let mut mac = HmacSha256::new_from_slice(jwt_secret.as_bytes())
        .map_err(|e| anyhow::anyhow!("HMAC init: {e}"))?;
    mac.update(&payload);
    let tag = mac.finalize().into_bytes();

    let mut full = payload;
    full.extend_from_slice(&tag);

    Ok(PowChallenge {
        challenge: hex::encode(&full),
        difficulty,
    })
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

    // Payload layout is enforced by the length check above (24 = 8 + 16),
    // so this conversion can't fail — but model that explicitly instead
    // of swallowing the impossible Err arm.
    let Ok(ts_bytes) = <[u8; 8]>::try_from(&payload[..8]) else {
        return false;
    };
    let issued = u64::from_be_bytes(ts_bytes);
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
///
/// `capacity` and `refill_per_sec` are stored alongside the count so that
/// eviction can compare buckets sized by different callers on equal terms
/// (see [`Buckets::evict`]).
#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last_refill: Instant,
    capacity: f64,
    refill_per_sec: f64,
}

impl Bucket {
    /// Fraction of this bucket's burst that would be available at `now`,
    /// in `0.0..=1.0`. `1.0` means fully regenerated.
    ///
    /// This is the ranking key for eviction because it is comparable
    /// across buckets with different capacities: a 4-of-5 bucket (0.8) is
    /// *less* throttled than a 4-of-20 bucket (0.2), even though both have
    /// four tokens left.
    fn fraction_at(&self, now: Instant) -> f64 {
        let elapsed = now
            .saturating_duration_since(self.last_refill)
            .as_secs_f64();
        let tokens = (self.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        if self.capacity > 0.0 {
            tokens / self.capacity
        } else {
            1.0
        }
    }
}

/// The map plus the growth threshold that amortizes sweeping.
#[derive(Debug)]
struct Buckets {
    map: HashMap<String, Bucket>,
    /// Sweep once the map exceeds this many entries, then raise it.
    /// Without it, every request past the soft limit pays for an O(n)
    /// scan while holding the global lock.
    sweep_at: usize,
}

/// Never hold more than this many buckets. 50k entries is far above any
/// legitimate working set and bounds memory at a few megabytes.
const HARD_LIMIT: usize = 50_000;
/// Smallest map size worth sweeping.
const SOFT_LIMIT: usize = 1024;

impl Buckets {
    /// Bound the map without handing anyone a fresh burst.
    ///
    /// # Why not just `clear()`
    ///
    /// Clearing gives *every* key a full bucket, so flooding the map with
    /// throwaway keys used to reset the brute-force defenses on `/login`
    /// and friends — and `POST /forgot` handed an attacker a key it fully
    /// controlled. Eviction now runs in an order that cannot be abused:
    ///
    /// 1. Entries untouched for an hour, and entries that have already
    ///    regenerated to full, are dropped. Both are *lossless*: a bucket
    ///    at capacity grants exactly what an absent key grants.
    /// 2. If that is not enough, the **least-throttled** buckets go first.
    ///
    /// Step 2 is what defeats the flood. Each throwaway key costs its
    /// creator only one token, so it sits near-full and is evicted early;
    /// displacing a genuinely exhausted bucket would mean creating 50,000
    /// buckets that are *more* exhausted still, which costs the attacker
    /// the very requests the limiter exists to deny.
    fn evict(&mut self, now: Instant) {
        self.map.retain(|_, b| {
            now.saturating_duration_since(b.last_refill) < Duration::from_secs(3600)
                && b.fraction_at(now) < 1.0
        });

        if self.map.len() > HARD_LIMIT {
            let mut ranked: Vec<(f64, String)> = self
                .map
                .iter()
                .map(|(k, b)| (b.fraction_at(now), k.clone()))
                .collect();
            // Partition so the most-throttled HARD_LIMIT entries sort first.
            ranked.select_nth_unstable_by(HARD_LIMIT, |a, b| a.0.total_cmp(&b.0));
            tracing::warn!(
                entries = self.map.len(),
                evicting = ranked.len() - HARD_LIMIT,
                "rate-limiter map exceeded its ceiling — evicting least-throttled keys \
                 (possible key-flood abuse)"
            );
            for (_, key) in &ranked[HARD_LIMIT..] {
                self.map.remove(key);
            }
        }

        // Amortize: next sweep only after the map has grown substantially.
        self.sweep_at = self.map.len().saturating_mul(2).max(SOFT_LIMIT);
    }
}

#[derive(Clone)]
pub struct RateLimiter {
    inner: Arc<Mutex<Buckets>>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Buckets {
                map: HashMap::new(),
                sweep_at: SOFT_LIMIT,
            })),
        }
    }

    /// Try to spend one token for `key`. Returns `true` if allowed.
    ///
    /// `capacity` is the burst size; `refill_per_sec` is the sustained rate.
    /// The very first call from a key starts the bucket full so a single
    /// request never gets denied.
    ///
    /// Mutex poisoning is recovered from rather than propagated: the only
    /// data behind the lock is timestamps + counts, none of which become
    /// dangerous after a panic, and crashing every subsequent request to
    /// signal a prior failure would be a far worse outcome.
    pub fn allow(&self, key: &str, capacity: f64, refill_per_sec: f64) -> bool {
        let now = Instant::now();
        let mut buckets = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };

        if buckets.map.len() > buckets.sweep_at {
            buckets.evict(now);
        }

        let bucket = buckets.map.entry(key.to_string()).or_insert(Bucket {
            tokens: capacity,
            last_refill: now,
            capacity,
            refill_per_sec,
        });

        let elapsed = now
            .saturating_duration_since(bucket.last_refill)
            .as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * refill_per_sec).min(capacity);
        bucket.last_refill = now;
        // Keep the sizing current so eviction ranks this bucket correctly
        // even if a call site's limits are retuned between requests.
        bucket.capacity = capacity;
        bucket.refill_per_sec = refill_per_sec;

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

/// Build a limiter key from `prefix` plus a **digest** of attacker-supplied
/// `value`.
///
/// Use this whenever the varying part of a key is unvalidated request input
/// (a submitted email address, say) rather than something bounded like an IP.
/// Hashing does two things: it caps each entry at a fixed size no matter how
/// long the submitted value is, and it keeps raw addresses out of the
/// limiter's memory, which is neither encrypted nor redacted in a core dump.
///
/// 128 bits of digest is far past what collision resistance needs here — a
/// collision merely makes two addresses share a throttle bucket.
pub fn throttle_key(prefix: &str, value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(prefix.len() + 33);
    out.push_str(prefix);
    out.push(':');
    for byte in &digest[..16] {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

// ---------------------------------------------------------------------------
// Client-IP extraction
// ---------------------------------------------------------------------------

/// Resolve the client IP, counting back from the TCP peer through
/// `trusted_hops` reverse proxies.
///
/// This value keys the rate limiter, so treating it as untrusted input
/// is the whole point. The previous implementation preferred
/// `CF-Connecting-IP`, then the **leftmost** `X-Forwarded-For` entry —
/// both fully attacker-supplied, since proxies *append* to XFF and
/// nothing strips inbound copies of either header. Sending a random
/// `CF-Connecting-IP` per request put every attempt in a fresh token
/// bucket, making the login and signup limits decorative.
///
/// Correct model: only the last `trusted_hops` entries of
/// `X-Forwarded-For` were written by infrastructure we control, so we
/// index from the RIGHT. With Traefik in front (`TRUSTED_PROXY_HOPS=1`,
/// the default), a client sending
/// `X-Forwarded-For: 9.9.9.9` arrives as `9.9.9.9, <real-ip>` and we
/// correctly read `<real-ip>`. `CF-Connecting-IP` and `X-Real-IP` are
/// ignored entirely — re-introduce them only behind a proxy that
/// provably overwrites them.
///
/// `trusted_hops == 0` means "no proxy": use the TCP peer and ignore
/// forwarding headers completely.
pub fn client_ip(headers: &HeaderMap, peer: Option<&SocketAddr>, trusted_hops: usize) -> String {
    let peer_ip = || {
        peer.map(|p| p.ip().to_string())
            .unwrap_or_else(|| "unknown".into())
    };
    if trusted_hops == 0 {
        return peer_ip();
    }

    // Flatten the chain across repeated headers, left (client-most) to
    // right (proxy-most). The peer itself is the implicit rightmost hop.
    let forwarded: Vec<&str> = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();

    // `trusted_hops` counts proxies between the client and us. The peer
    // is hop 1, so the client sits `trusted_hops - 1` entries from the
    // right of the XFF list.
    match forwarded.len().checked_sub(trusted_hops) {
        Some(idx) => forwarded
            .get(idx)
            .map(|s| (*s).to_string())
            .unwrap_or_else(peer_ip),
        // Fewer entries than expected — the request didn't traverse the
        // full trusted chain. Fall back to the peer rather than trusting
        // a client-supplied entry.
        None => peer_ip(),
    }
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
