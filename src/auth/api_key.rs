//! Per-caller API-key resolution + token-bucket rate limiting for the data
//! plane.
//!
//! Built from `[api_keys.<name>]` config (bounded, fixed key set — one bucket
//! per caller, Power-of-10 rule 3) and rebuilt wholesale on hot-reload. A
//! request presents its secret in the `X-Api-Key` header; the limiter resolves
//! it (constant-time) to a caller and spends one token from that caller's
//! independent bucket. Callers are isolated: one exhausting its bucket cannot
//! affect another.
//!
//! When no keys are configured the limiter is empty and admits everything
//! (`Admission::Open`), preserving the pre-key behavior of the data plane.

use crate::config::ApiKeyEntry;
use indexmap::IndexMap;
use std::sync::Mutex;
use std::time::Instant;
use subtle::ConstantTimeEq;

/// Lazy-refill token bucket. Mirrors the WASM capability broker's per-grant
/// limiter: refill is computed on demand from elapsed wall-monotonic time, so
/// there is no background timer. `capacity == refill_per_sec` makes the
/// sustained rate and the burst ceiling the same number (req/s).
struct TokenBucket {
    capacity: f64,
    tokens: f64,
    refill_per_sec: f64,
    last: Instant,
}

impl TokenBucket {
    fn new(rate: u32) -> Self {
        let r = f64::from(rate);
        Self {
            capacity: r,
            tokens: r,
            refill_per_sec: r,
            last: Instant::now(),
        }
    }

    /// Refill from elapsed time, then try to spend one token. On success
    /// returns `Ok(())`; when empty returns `Err(retry_after_secs)` — whole
    /// seconds until the next token is available, floored at 1 (for the HTTP
    /// `Retry-After` header).
    fn try_take(&mut self) -> Result<(), u64> {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            Ok(())
        } else {
            // Seconds until tokens climbs back to 1.0. refill_per_sec is > 0
            // (config rejects rate_per_sec == 0), but guard anyway rather than
            // risk a divide producing a non-finite value.
            let deficit = 1.0 - self.tokens;
            let secs = if self.refill_per_sec > 0.0 {
                deficit / self.refill_per_sec
            } else {
                1.0
            };
            // f64 → u64 `as` saturates (never panics); ceil + max(1.0) keeps
            // Retry-After a sane whole-second value.
            Err(secs.ceil().max(1.0) as u64)
        }
    }
}

/// One configured caller: its name (for logs/audit), the secret it presents,
/// and its optional independent rate limit.
struct Caller {
    name: String,
    secret: String,
    /// `None` → unlimited (identity only, no rate ceiling).
    bucket: Option<Mutex<TokenBucket>>,
}

/// The outcome of resolving + admitting one request against the key set.
#[derive(Debug, PartialEq, Eq)]
pub enum Admission {
    /// No keys are configured — the data plane is open (pre-key behavior).
    Open,
    /// A configured key matched and had budget (or is unlimited).
    Admitted,
    /// Keys are configured but the presented key is unknown or absent →
    /// fail closed (HTTP 401).
    Unauthorized,
    /// The caller's key matched but its bucket is empty (HTTP 429). Carries
    /// the caller name (for the log line) and the `Retry-After` seconds.
    RateLimited {
        caller: String,
        retry_after_secs: u64,
    },
}

/// The data-plane admission gate: a fixed set of callers, each with its own
/// bucket. Cheap to build; rebuilt on config hot-reload.
pub struct RateLimiter {
    callers: Vec<Caller>,
}

/// An empty limiter admits everything (`Admission::Open`). This is the state
/// when no `[api_keys]` are configured.
impl Default for RateLimiter {
    fn default() -> Self {
        Self {
            callers: Vec::new(),
        }
    }
}

impl RateLimiter {
    /// Build from the `[api_keys]` config. Config validation has already
    /// rejected empty secrets, zero rates, and duplicate secrets.
    pub fn from_config(keys: &IndexMap<String, ApiKeyEntry>) -> Self {
        let callers = keys
            .iter()
            .map(|(name, entry)| Caller {
                name: name.clone(),
                secret: entry.key.clone(),
                bucket: entry.rate_per_sec.map(|r| Mutex::new(TokenBucket::new(r))),
            })
            .collect();
        Self { callers }
    }

    /// True when at least one key is configured (the data plane is gated).
    pub fn is_enforcing(&self) -> bool {
        !self.callers.is_empty()
    }

    /// Resolve `presented_key` against the configured set and apply the matched
    /// caller's rate limit. Comparison is constant-time and scans the whole set
    /// regardless of an early match, so resolution timing does not leak which
    /// key matched.
    pub fn admit(&self, presented_key: Option<&str>) -> Admission {
        if self.callers.is_empty() {
            return Admission::Open;
        }
        let Some(presented) = presented_key else {
            return Admission::Unauthorized;
        };
        let mut matched: Option<&Caller> = None;
        for caller in &self.callers {
            let hit: bool = presented.as_bytes().ct_eq(caller.secret.as_bytes()).into();
            if hit {
                matched = Some(caller);
            }
        }
        let Some(caller) = matched else {
            return Admission::Unauthorized;
        };
        let Some(bucket) = &caller.bucket else {
            return Admission::Admitted; // unlimited key
        };
        // Poisoned mutex (a prior panic while held — impossible here, but the
        // type allows it): recover the guard rather than propagate a panic
        // onto the request path.
        let mut guard = match bucket.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        match guard.try_take() {
            Ok(()) => Admission::Admitted,
            Err(retry_after_secs) => Admission::RateLimited {
                caller: caller.name.clone(),
                retry_after_secs,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(entries: &[(&str, &str, Option<u32>)]) -> IndexMap<String, ApiKeyEntry> {
        entries
            .iter()
            .map(|(name, key, rate)| {
                (
                    (*name).to_string(),
                    ApiKeyEntry {
                        key: (*key).to_string(),
                        rate_per_sec: *rate,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn open_when_no_keys_configured() {
        let rl = RateLimiter::from_config(&IndexMap::new());
        assert!(!rl.is_enforcing());
        assert_eq!(rl.admit(None), Admission::Open);
        assert_eq!(rl.admit(Some("anything")), Admission::Open);
    }

    #[test]
    fn unknown_or_absent_key_fails_closed() {
        let rl = RateLimiter::from_config(&cfg(&[("alice", "secret-a", Some(10))]));
        assert!(rl.is_enforcing());
        assert_eq!(rl.admit(None), Admission::Unauthorized);
        assert_eq!(rl.admit(Some("wrong")), Admission::Unauthorized);
        assert_eq!(rl.admit(Some("secret-a")), Admission::Admitted);
    }

    #[test]
    fn unlimited_key_never_rate_limits() {
        let rl = RateLimiter::from_config(&cfg(&[("svc", "k", None)]));
        for _ in 0..1000 {
            assert_eq!(rl.admit(Some("k")), Admission::Admitted);
        }
    }

    #[test]
    fn bucket_exhausts_then_429_with_retry_after() {
        // burst == rate == 3: three immediate takes, then empty.
        let rl = RateLimiter::from_config(&cfg(&[("alice", "a", Some(3))]));
        assert_eq!(rl.admit(Some("a")), Admission::Admitted);
        assert_eq!(rl.admit(Some("a")), Admission::Admitted);
        assert_eq!(rl.admit(Some("a")), Admission::Admitted);
        match rl.admit(Some("a")) {
            Admission::RateLimited {
                caller,
                retry_after_secs,
            } => {
                assert_eq!(caller, "alice");
                assert!(retry_after_secs >= 1, "Retry-After floored at 1s");
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn callers_are_isolated() {
        // alice's bucket exhaustion must not touch bob's.
        let rl = RateLimiter::from_config(&cfg(&[("alice", "a", Some(1)), ("bob", "b", Some(1))]));
        assert_eq!(rl.admit(Some("a")), Admission::Admitted); // alice spends her only token
        assert!(matches!(rl.admit(Some("a")), Admission::RateLimited { .. })); // alice now limited
        assert_eq!(rl.admit(Some("b")), Admission::Admitted); // bob is unaffected
    }
}
